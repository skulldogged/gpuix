//! `<markdown>` — GitHub-flavoured markdown rendered natively and selectable.
//!
//! ```tsx
//! <markdown source={text} theme={{ accent: '#7c86ff' }} onLinkClick={(e) => {}} />
//! ```
//!
//! Every paragraph, heading, table cell and code line registers into the shared
//! selection registry in document order, so a drag can start in a heading and
//! end inside a fenced code block, and Cmd+C copies the whole span.

use std::rc::Rc;
use std::sync::Arc;

use gpui::SharedString;

use super::{CustomElement, CustomElementFactory, CustomRenderContext};
use crate::markdown::parser::{parse, BlockTree};
use crate::markdown::render::{image_resource, image_sources, render_tree, ImageLoad, MdContext};
use crate::markdown::veil::Veil;
use crate::renderer::emit_event_full;
use crate::theme::Theme;

/// `linkClick` value sent by a code block's Wrap control.
pub const CODE_WRAP_TOGGLE: &str = "gpuix:toggle-code-wrap";
/// `linkClick` prefix for a click on an image, followed by its resolved source.
pub const IMAGE_OPEN: &str = "gpuix:open-image:";

pub struct MarkdownFactory;

impl CustomElementFactory for MarkdownFactory {
    fn element_type(&self) -> &str {
        "markdown"
    }

    fn create(&self, _id: u64) -> Box<dyn CustomElement> {
        Box::new(MarkdownElement::default())
    }
}

#[derive(Default)]
pub struct MarkdownElement {
    source: String,
    theme: Theme,
    /// Parsed tree for the current source. `Rc` so a frame clones a pointer
    /// rather than every block, string and inline run in the document.
    tree: Option<Rc<BlockTree>>,
    parsed_len: Option<usize>,
    parsed_hash: Option<u64>,
    code_wrap: bool,
    /// Set while the source is still arriving; see [`Veil`].
    streaming: bool,
    veil: Option<Veil>,
    /// Folder that relative image paths resolve against.
    image_base: Option<std::path::PathBuf>,
}

impl MarkdownElement {
    fn tree(&mut self) -> Rc<BlockTree> {
        let hash = hash64(&self.source);
        let stale = self.parsed_hash != Some(hash) || self.parsed_len != Some(self.source.len());
        if stale || self.tree.is_none() {
            self.tree = Some(Rc::new(parse(&self.source)));
            self.parsed_hash = Some(hash);
            self.parsed_len = Some(self.source.len());
        }
        self.tree.clone().expect("just parsed")
    }
}

fn hash64(source: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    source.hash(&mut hasher);
    hasher.finish()
}

impl CustomElement for MarkdownElement {
    fn render(
        &mut self,
        ctx: CustomRenderContext,
        window: &mut gpui::Window,
        cx: &mut gpui::Context<crate::renderer::GpuixView>,
    ) -> gpui::AnyElement {
        use gpui::prelude::*;

        let theme = self.theme.clone();
        let tree = self.tree();

        // Link clicks are hit-tested per byte range inside the painted text, so
        // clicking prose emits nothing and clicking the second link emits the
        // second URL.
        let on_link: Option<Arc<dyn Fn(&str)>> = if ctx.events.contains("linkClick") {
            let callback = ctx.event_callback.clone();
            let element_id = ctx.id;
            Some(Arc::new(move |url: &str| {
                let url = url.to_string();
                emit_event_full(&callback, element_id, "linkClick", |p| {
                    p.value = Some(url);
                });
            }))
        } else {
            None
        };

        // The wrap toggle reports through linkClick with a reserved value, so a
        // host needs no new event: it flips `codeWrap` and renders again.
        let on_code_wrap: Option<Arc<dyn Fn()>> = on_link
            .clone()
            .map(|link| Arc::new(move || link(CODE_WRAP_TOGGLE)) as Arc<dyn Fn()>);
        let mut md = MdContext::new(
            ctx.id,
            ctx.selection.clone(),
            ctx.selectable,
            ctx.selection_wash,
            theme.clone(),
            on_link,
            ctx.highlight_set.clone(),
        );
        md.code_wrap = self.code_wrap;
        md.on_code_wrap = on_code_wrap;
        // Images load through GPUI's asset cache, which redraws the view when
        // one finishes; until then the block holds a placeholder.
        let mut sources = Vec::new();
        image_sources(&tree.blocks, &mut sources);
        for src in sources {
            let load = match image_resource(&src, self.image_base.as_deref()) {
                None => ImageLoad::Unsupported,
                Some(resource) => {
                    match window.use_asset::<gpui::ImgResourceLoader>(&resource, cx) {
                        None => ImageLoad::Loading,
                        Some(Ok(image)) => {
                            let size = image.size(0);
                            ImageLoad::Ready(resource, size.width.0 as f32, size.height.0 as f32)
                        }
                        Some(Err(_)) => ImageLoad::Failed,
                    }
                }
            };
            md.images.insert(src, load);
        }
        md.veil = if self.streaming {
            Some(self.veil.take().unwrap_or_default())
        } else {
            None
        };
        let body = render_tree(&tree, &mut md, window);
        if let Some(mut veil) = md.veil.take() {
            veil.finish_frame();
            if std::mem::take(&mut veil.animating) {
                window.request_animation_frame();
            }
            self.veil = Some(veil);
        }

        // Block layout, like the document inside it; see `render::stack`.
        let container = gpui::div()
            .id(SharedString::from(format!("__gpuix_markdown_{}", ctx.id)))
            .w_full()
            .min_w_0()
            .text_color(theme.text)
            .font_family(theme.font_sans.clone())
            .text_size(gpui::px(theme.metrics.md_text_size))
            .line_height(gpui::px(theme.metrics.md_line_height));

        super::custom_surface(container, &ctx)
            .child(body)
            .into_any_element()
    }

    fn set_prop(&mut self, key: &str, value: serde_json::Value) {
        match key {
            "source" => self.source = value.as_str().unwrap_or("").to_string(),
            "theme" => self.theme = Theme::from_prop(Some(&value)),
            "codeWrap" => self.code_wrap = value.as_bool().unwrap_or(false),
            "imageBase" => {
                self.image_base = value
                    .as_str()
                    .filter(|base| !base.is_empty())
                    .map(Into::into)
            }
            "streaming" => {
                self.streaming = value.as_bool().unwrap_or(false);
                if !self.streaming {
                    self.veil = None;
                }
            }
            _ => {}
        }
    }

    fn supported_props(&self) -> &'static [&'static str] {
        &["source", "theme", "codeWrap", "streaming", "imageBase"]
    }

    fn supported_events(&self) -> &'static [&'static str] {
        &["linkClick", "click", "mouseEnter", "mouseLeave", "fileDrop"]
    }

    fn destroy(&mut self) {
        self.tree = None;
    }
}

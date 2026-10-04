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
/// A gap between renders that means the element was out of view. A streaming
/// document on screen renders far more often, with whatever shows its progress.
const AWAY: std::time::Duration = std::time::Duration::from_secs(1);

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
    /// Parsed tree for the current source, dropped when the source changes.
    /// `Rc` so a frame clones a pointer rather than every block, string and
    /// inline run in the document.
    tree: Option<Rc<BlockTree>>,
    code_wrap: bool,
    /// Set while the source is still arriving; see [`Veil`].
    streaming: bool,
    /// Outlives `streaming` until the fades it started have finished.
    veil: Option<Veil>,
    /// Streaming just stopped. The update that stops it often brings the last
    /// block, which fades in like the ones before it.
    ending: bool,
    /// When this element last rendered.
    painted: Option<web_time::Instant>,
    /// Folder that relative image paths resolve against.
    image_base: Option<std::path::PathBuf>,
    /// See [`crate::markdown::render::MdContext::code_highlights`].
    code_highlights: std::collections::HashMap<usize, Arc<crate::syntax::HighlightedDocument>>,
}

impl MarkdownElement {
    fn tree(&mut self) -> Rc<BlockTree> {
        if self.tree.is_none() {
            self.tree = Some(Rc::new(parse(&self.source)));
        }
        self.tree.clone().expect("just parsed")
    }
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
        // New blocks and text fade in while streaming and in the update that
        // ends it; fades still running then finish rather than snap to full. Reduced
        // motion shows everything at once, as GPUI's own animations do.
        // Props only arrive when the element renders, so an end that came
        // while a list had it scrolled out of view shows up late; that update
        // shows at once, like the rest of a finished document.
        let now = web_time::Instant::now();
        let away = self
            .painted
            .replace(now)
            .is_some_and(|at| now.duration_since(at) > AWAY);
        let ending = std::mem::take(&mut self.ending) && !away;
        let live = (self.streaming || ending) && !cx.reduce_motion();
        md.veil = self.veil.take().or_else(|| live.then(Veil::default));
        if let Some(veil) = &mut md.veil {
            veil.live = live;
        }
        md.code_highlights = std::mem::take(&mut self.code_highlights);
        let body = render_tree(&tree, &mut md, window);
        let code_blocks = md.next_code;
        self.code_highlights = std::mem::take(&mut md.code_highlights);
        self.code_highlights
            .retain(|ordinal, _| *ordinal < code_blocks);
        if let Some(mut veil) = md.veil.take() {
            veil.finish_frame();
            let animating = std::mem::take(&mut veil.animating);
            if animating {
                window.request_animation_frame();
            }
            if live || animating {
                self.veil = Some(veil);
            }
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
            "source" => {
                let source = value.as_str().unwrap_or("");
                if source != self.source {
                    self.source = source.to_string();
                    self.tree = None;
                }
            }
            "theme" => self.theme = Theme::from_prop(Some(&value)),
            "codeWrap" => self.code_wrap = value.as_bool().unwrap_or(false),
            "imageBase" => {
                self.image_base = value
                    .as_str()
                    .filter(|base| !base.is_empty())
                    .map(Into::into)
            }
            // The veil stays until `render` sees its fades finish.
            "streaming" => {
                let streaming = value.as_bool().unwrap_or(false);
                self.ending |= self.streaming && !streaming;
                self.streaming = streaming;
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

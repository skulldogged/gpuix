/// Image custom elements for raster images and tintable SVG icons.
///
/// This provides a native `<img>` for GPUIX React apps while keeping the same
/// custom-element prop pipeline (`setCustomProp`/`custom_props`).
///
/// HTTP(S) `src` is a GPUI URI resource. GPUI fetches it through the app
/// `HttpClient` on a background task and paints once decode finishes. A
/// definite `width` and `height` keep the layout box stable during that load.
use super::{CustomElement, CustomElementFactory, CustomRenderContext};
use base64::Engine as _;

pub struct ImgFactory;

pub struct SvgFactory;

impl CustomElementFactory for SvgFactory {
    fn element_type(&self) -> &str {
        "svg"
    }

    fn create(&self, _id: u64) -> Box<dyn CustomElement> {
        Box::new(SvgElement::default())
    }

    fn cacheable(&self) -> bool {
        true
    }
}

impl CustomElementFactory for ImgFactory {
    fn element_type(&self) -> &str {
        "img"
    }

    fn create(&self, _id: u64) -> Box<dyn CustomElement> {
        Box::new(ImgElement::default())
    }
}

#[derive(Debug, Clone)]
enum ImgObjectFit {
    Fill,
    Contain,
    Cover,
    ScaleDown,
    None,
}

impl Default for ImgObjectFit {
    fn default() -> Self {
        Self::Contain
    }
}

impl ImgObjectFit {
    fn from_str(value: &str) -> Self {
        match value {
            "fill" => Self::Fill,
            "cover" => Self::Cover,
            "scaleDown" => Self::ScaleDown,
            "none" => Self::None,
            _ => Self::Contain,
        }
    }

    fn as_gpui(&self) -> gpui::ObjectFit {
        match self {
            Self::Fill => gpui::ObjectFit::Fill,
            Self::Contain => gpui::ObjectFit::Contain,
            Self::Cover => gpui::ObjectFit::Cover,
            Self::ScaleDown => gpui::ObjectFit::ScaleDown,
            Self::None => gpui::ObjectFit::None,
        }
    }
}

#[derive(Debug, Clone, Default)]
enum ImgSource {
    #[default]
    Empty,
    Path(std::path::PathBuf),
    Uri(gpui::SharedUri),
    Data(std::sync::Arc<gpui::Image>),
    Render(std::sync::Arc<gpui::RenderImage>),
    Invalid,
}

#[derive(Debug, Clone, Default)]
pub struct ImgElement {
    source: ImgSource,
    object_fit: ImgObjectFit,
    alt: String,
    /// `animated={false}`: load files and URLs as their first frame.
    still: bool,
    dropped: Option<std::sync::Arc<gpui::RenderImage>>,
}

impl ImgElement {
    fn load_src(&mut self, src: &str) {
        let src = src.trim();
        self.source = if src.is_empty() {
            ImgSource::Empty
        } else if src.starts_with("data:") {
            // TODO: Replace JSON data URLs with binary mutations to keep base64 decoding off paint.
            decode_image_data_url(src)
                .map(|(format, bytes)| {
                    ImgSource::Data(std::sync::Arc::new(gpui::Image::from_bytes(format, bytes)))
                })
                .unwrap_or(ImgSource::Invalid)
        } else if let Some(uri) = http_image_uri(src) {
            ImgSource::Uri(uri)
        } else {
            ImgSource::Path(src.into())
        };
    }
}

fn http_image_uri(src: &str) -> Option<gpui::SharedUri> {
    let scheme_end = src.find("://")?;
    let scheme = &src[..scheme_end];
    if !scheme.eq_ignore_ascii_case("http") && !scheme.eq_ignore_ascii_case("https") {
        return None;
    }
    (src.len() > scheme_end + 3).then(|| gpui::SharedUri::from(src.to_string()))
}

/// Decoded `<img>` files and URLs, bounded in memory.
///
/// GPUI's default is to keep every image an `img()` ever loaded, decoded, until
/// the app exits, which an image-heavy app such as a music library grows
/// without limit. This cache keeps them by last use instead. An image counts
/// as used in a frame when its element lays out, which every visible `<img>`
/// does on every frame: a region holding one is never replayed from a cache.
/// So at the start of a frame, anything the previous frame did not draw can be
/// dropped from the GPU atlas without leaving a stale sprite behind.
pub struct ImageMemory {
    entries: std::collections::HashMap<u64, ImageMemoryEntry>,
    budget: usize,
    used: usize,
    frame: u64,
}

struct ImageMemoryEntry {
    item: gpui::ImageCacheItem,
    bytes: usize,
    last_used: u64,
}

/// The image memory as `<img animated={false}>` sees it: the same entries and
/// budget, with animated files loaded as their first frame.
pub struct StillImages(gpui::Entity<ImageMemory>);

struct GlobalImageMemory {
    memory: gpui::Entity<ImageMemory>,
    still: gpui::Entity<StillImages>,
}

impl gpui::Global for GlobalImageMemory {}

fn image_bytes(image: &gpui::RenderImage) -> usize {
    (0..image.frame_count())
        .map(|frame| {
            let size = image.size(frame);
            size.width.0.max(0) as usize * size.height.0.max(0) as usize * 4
        })
        .sum()
}

impl ImageMemory {
    /// Start a frame: drop images the last frame did not draw while the cache
    /// is over budget, least recently used first. Failed loads are forgotten
    /// once unused, so a remounted image tries again.
    pub fn begin_frame(window: &mut gpui::Window, cx: &mut gpui::App) {
        let Some(memory) = cx
            .try_global::<GlobalImageMemory>()
            .map(|g| g.memory.clone())
        else {
            return;
        };
        let evicted = memory.update(cx, |memory, _| {
            memory.frame += 1;
            let keep_after = memory.frame.saturating_sub(2);
            let mut evicted = Vec::new();
            memory.entries.retain(|_, entry| {
                let failed = matches!(entry.item, gpui::ImageCacheItem::Loaded(Err(_)));
                !(failed && entry.last_used <= keep_after)
            });
            if memory.used <= memory.budget {
                return evicted;
            }
            let mut idle: Vec<(u64, u64)> = memory
                .entries
                .iter()
                .filter(|(_, entry)| entry.bytes > 0 && entry.last_used <= keep_after)
                .map(|(key, entry)| (entry.last_used, *key))
                .collect();
            idle.sort_unstable();
            for (_, key) in idle {
                if memory.used <= memory.budget {
                    break;
                }
                if let Some(mut entry) = memory.entries.remove(&key) {
                    memory.used -= entry.bytes;
                    if let Some(Ok(image)) = entry.item.get() {
                        evicted.push(image);
                    }
                }
            }
            evicted
        });
        for image in evicted {
            cx.drop_image(image, Some(window));
        }
    }
}

impl gpui::ImageCache for ImageMemory {
    fn load(
        &mut self,
        resource: &gpui::Resource,
        window: &mut gpui::Window,
        cx: &mut gpui::App,
    ) -> Option<Result<std::sync::Arc<gpui::RenderImage>, gpui::ImageCacheError>> {
        self.load_resource(resource, false, window, cx)
    }
}

impl gpui::ImageCache for StillImages {
    fn load(
        &mut self,
        resource: &gpui::Resource,
        window: &mut gpui::Window,
        cx: &mut gpui::App,
    ) -> Option<Result<std::sync::Arc<gpui::RenderImage>, gpui::ImageCacheError>> {
        self.0.update(cx, |memory, cx| {
            memory.load_resource(resource, true, window, cx)
        })
    }
}

impl ImageMemory {
    fn load_resource(
        &mut self,
        resource: &gpui::Resource,
        still: bool,
        window: &mut gpui::Window,
        cx: &mut gpui::App,
    ) -> Option<Result<std::sync::Arc<gpui::RenderImage>, gpui::ImageCacheError>> {
        use futures::FutureExt as _;
        use gpui::Asset as _;
        let key = gpui::hash(&(resource, still));
        if let Some(entry) = self.entries.get_mut(&key) {
            entry.last_used = self.frame;
            let result = entry.item.get();
            if entry.bytes == 0 {
                if let Some(Ok(image)) = &result {
                    entry.bytes = image_bytes(image);
                    self.used += entry.bytes;
                }
            }
            return result;
        }

        let load = if still {
            load_still(resource.clone(), cx).boxed()
        } else {
            gpui::AssetLogger::<gpui::ImageAssetLoader>::load(resource.clone(), cx).boxed()
        };
        let task = cx.background_executor().spawn(load).shared();
        self.entries.insert(
            key,
            ImageMemoryEntry {
                item: gpui::ImageCacheItem::Loading(task.clone()),
                bytes: 0,
                last_used: self.frame,
            },
        );
        let view = window.current_view();
        window
            .spawn(cx, async move |cx| {
                _ = task.await;
                cx.on_next_frame(move |_, cx| cx.notify(view));
            })
            .detach();
        None
    }
}

/// Load a file or URL as a single still frame. GPUI's own loader decodes every
/// frame of an animated GIF or WebP up front; a few hundred frames of album art
/// is hundreds of megabytes, and the atlas then uploads each frame as it plays.
fn load_still(
    resource: gpui::Resource,
    cx: &mut gpui::App,
) -> impl std::future::Future<
    Output = Result<std::sync::Arc<gpui::RenderImage>, gpui::ImageCacheError>,
> + Send
       + 'static {
    use futures::{AsyncReadExt as _, FutureExt as _};
    let client = cx.http_client();
    let assets = cx.asset_source().clone();
    let svg_renderer = cx.svg_renderer();
    async move {
        let bytes = match &resource {
            gpui::Resource::Path(path) => std::fs::read(path.as_ref())?,
            gpui::Resource::Uri(uri) => {
                let mut response = client.get(uri.as_ref(), ().into(), true).await?;
                let mut body = Vec::new();
                response.body_mut().read_to_end(&mut body).await?;
                if !response.status().is_success() {
                    return Err(gpui::ImageCacheError::BadStatus {
                        uri: uri.clone(),
                        status: response.status(),
                        body: String::new(),
                    });
                }
                body
            }
            gpui::Resource::Embedded(path) => assets
                .load(path.as_ref())
                .ok()
                .flatten()
                .map(|data| data.to_vec())
                .ok_or_else(|| {
                    gpui::ImageCacheError::Asset(
                        format!("Embedded resource not found: {path}").into(),
                    )
                })?,
        };
        let Ok(format) = image::guess_format(&bytes) else {
            // Not a raster format, so an SVG, which has no frames to skip.
            return Ok(gpui::Image::from_bytes(gpui::ImageFormat::Svg, bytes)
                .to_image_data(svg_renderer)?);
        };
        // A decoder read as a single image yields the first frame.
        let mut pixels = image::load_from_memory_with_format(&bytes, format)?.into_rgba8();
        for pixel in pixels.chunks_exact_mut(4) {
            pixel.swap(0, 2);
        }
        let (width, height) = pixels.dimensions();
        gpui::RenderImage::from_bgra(width, height, pixels.into_raw())
            .map(std::sync::Arc::new)
            .ok_or_else(|| anyhow::anyhow!("decoded image does not match its size").into())
    }
    .inspect(move |result| {
        if let Err(error) = result {
            log::error!("{error}");
        }
    })
}

/// Install the GPUI HTTP client so `<img src="https://…">` can fetch, and the
/// image memory every `<img>` loads through. `budget_mb` bounds the decoded
/// images kept while not on screen.
///
/// Web already gets `fetch` from `gpui_platform::single_threaded_web`. Desktop
/// Application defaults to `NullHttpClient`, which fails every URI load.
pub fn init(cx: &mut gpui::App, budget_mb: Option<f64>) {
    use gpui::AppContext as _;
    let budget = (budget_mb.unwrap_or(256.0).max(16.0) * 1024.0 * 1024.0) as usize;
    let memory = cx.new(|_| ImageMemory {
        entries: Default::default(),
        budget,
        used: 0,
        frame: 0,
    });
    let still = cx.new(|_| StillImages(memory.clone()));
    cx.set_global(GlobalImageMemory { memory, still });
    #[cfg(not(target_family = "wasm"))]
    match reqwest_client::ReqwestClient::user_agent("gpuix") {
        Ok(client) => cx.set_http_client(std::sync::Arc::new(client)),
        Err(error) => log::error!(
            "GPUIX HTTP client failed to start; <img src=\"http…\"> will not load: {error:#}"
        ),
    }
    #[cfg(target_family = "wasm")]
    let _ = cx;
}

#[cfg(test)]
mod tests {
    use super::http_image_uri;

    #[test]
    fn only_http_urls_become_uri_sources() {
        assert!(http_image_uri("https://example.test/a.png").is_some());
        assert!(http_image_uri("HTTP://localhost:9/a.png").is_some());
        assert!(http_image_uri("HTTPS://example.test/a.png").is_some());
        assert!(http_image_uri("/tmp/a.png").is_none());
        assert!(http_image_uri("data:image/png;base64,xx").is_none());
        assert!(http_image_uri("file:///tmp/a.png").is_none());
        assert!(http_image_uri("https://").is_none());
        assert!(http_image_uri("http://").is_none());
    }
}

fn img_fallback(ctx: &CustomRenderContext, alt: &str, message: &str) -> gpui::AnyElement {
    use gpui::prelude::*;

    let mut fallback = super::custom_surface(
        gpui::div()
            .id(gpui::SharedString::from(format!("__gpuix_img_{}", ctx.id)))
            .flex()
            .items_center()
            .justify_center()
            .bg(gpui::rgba(0x1f2230ff))
            .border(gpui::px(1.0))
            .border_color(gpui::rgba(0x5d6481ff))
            .text_color(gpui::rgba(0xa4accdff)),
        ctx,
    );
    fallback =
        crate::accessibility::apply_accessibility(fallback, ctx.props, Some(gpui::Role::Image));
    fallback = crate::accessibility::apply_image_label(fallback, ctx.props, alt);
    fallback
        .child(ctx.chrome_text(message.to_string(), None))
        .into_any_element()
}

impl CustomElement for ImgElement {
    fn render(
        &mut self,
        ctx: CustomRenderContext,
        window: &mut gpui::Window,
        _cx: &mut gpui::Context<crate::renderer::GpuixView>,
    ) -> gpui::AnyElement {
        use gpui::prelude::*;

        if let Some(image) = self.dropped.take() {
            window.drop_image(image).ok();
        }

        let el = match &self.source {
            ImgSource::Path(path) => gpui::img(path.clone()),
            ImgSource::Uri(uri) => gpui::img(uri.clone()),
            ImgSource::Data(image) => gpui::img(image.clone()),
            ImgSource::Render(image) => gpui::img(image.clone()),
            ImgSource::Empty => return img_fallback(&ctx, &self.alt, "img: no src"),
            ImgSource::Invalid => return img_fallback(&ctx, &self.alt, "img: load failed"),
        };
        // Files and URLs load through the bounded image memory; data and
        // pixel sources are owned by this element already.
        let el = match _cx.try_global::<GlobalImageMemory>() {
            Some(memory) if self.still => el.image_cache(&memory.still),
            Some(memory) => el.image_cache(&memory.memory),
            None => el,
        };
        // The id is what makes gpui's `ImgState` persist. Without it `Img` has no
        // `GlobalElementId`, so the animated-GIF frame index and the delayed
        // loading state are rebuilt from scratch on every frame and an animation
        // never advances past frame zero.
        let mut el = el
            .object_fit(self.object_fit.as_gpui())
            .with_fallback(|| {
                gpui::div()
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(gpui::rgba(0x1f2230ff))
                    .border(gpui::px(1.0))
                    .border_color(gpui::rgba(0x5d6481ff))
                    .text_color(gpui::rgba(0xa4accdff))
                    .child("img: load failed")
                    .into_any_element()
            })
            .id(gpui::SharedString::from(format!("__gpuix_img_{}", ctx.id)));

        if let Some(style) = ctx.style {
            el = crate::renderer::apply_interactive_styles(el, style);
            // GPUI fills `aspect_ratio` from the bitmap once it loads. That
            // overrides a definite height and jumps the box. A CSS `<img>` with
            // both width and height keeps that box; `objectFit` paints inside it.
            if let (
                Some(crate::style::DimensionValue::Pixels(width)),
                Some(crate::style::DimensionValue::Pixels(height)),
            ) = (style.width.as_ref(), style.height.as_ref())
            {
                if *width > 0.0 && *height > 0.0 {
                    el = el.aspect_ratio((*width as f32) / (*height as f32));
                }
            }
        }

        let mut el =
            crate::accessibility::apply_accessibility(el, ctx.props, Some(gpui::Role::Image));
        el = crate::accessibility::apply_image_label(el, ctx.props, &self.alt);
        let el = super::wire_standard_events(el, &ctx);
        crate::automation::track_own_bounds(el, ctx.id).into_any_element()
    }

    fn set_prop(&mut self, key: &str, value: serde_json::Value) {
        match key {
            "src" => {
                if value.is_null() && matches!(self.source, ImgSource::Render(_)) {
                    return;
                }
                if let ImgSource::Render(image) = &self.source {
                    self.dropped = Some(image.clone());
                }
                self.load_src(value.as_str().unwrap_or(""));
            }
            "objectFit" => {
                self.object_fit = value
                    .as_str()
                    .map(ImgObjectFit::from_str)
                    .unwrap_or_default()
            }
            "alt" => self.alt = value.as_str().unwrap_or_default().to_string(),
            "animated" => self.still = value.as_bool() == Some(false),
            _ => {}
        }
    }

    fn supported_props(&self) -> &'static [&'static str] {
        &["src", "objectFit", "alt", "animated"]
    }

    fn supported_events(&self) -> &'static [&'static str] {
        &["click", "mouseEnter", "mouseLeave", "fileDrop"]
    }

    fn destroy(&mut self) {}

    fn live_image(&self) -> Option<std::sync::Arc<gpui::RenderImage>> {
        match &self.source {
            ImgSource::Render(image) => Some(image.clone()),
            _ => None,
        }
    }

    fn take_dropped_image(&mut self) -> Option<std::sync::Arc<gpui::RenderImage>> {
        self.dropped.take()
    }

    fn replace_live_image(
        &mut self,
        image: std::sync::Arc<gpui::RenderImage>,
    ) -> Option<std::sync::Arc<gpui::RenderImage>> {
        let previous = self.live_image();
        self.source = ImgSource::Render(image);
        previous
    }
}

/// Byte order of a packed `setImagePixels` buffer. Alpha is straight in both.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PixelFormat {
    Rgba,
    /// GPUI's own `RenderImage` order, so the upload skips the swizzle pass.
    Bgra,
}

impl PixelFormat {
    pub fn parse(format: Option<&str>) -> std::result::Result<Self, String> {
        match format {
            None | Some("rgba") => Ok(Self::Rgba),
            Some("bgra") => Ok(Self::Bgra),
            Some(other) => Err(format!(
                "Unknown pixel format {other:?}, expected \"rgba\" or \"bgra\""
            )),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Rgba => "RGBA",
            Self::Bgra => "BGRA",
        }
    }
}

pub fn render_image_from_pixels(
    width: u32,
    height: u32,
    mut bytes: Vec<u8>,
    format: PixelFormat,
) -> std::result::Result<std::sync::Arc<gpui::RenderImage>, String> {
    let name = format.name();
    let expected = (width as u64)
        .checked_mul(height as u64)
        .and_then(|pixels| pixels.checked_mul(4))
        .and_then(|bytes| usize::try_from(bytes).ok())
        .ok_or_else(|| format!("{name} pixel buffer {width}x{height} is too large"))?;
    if bytes.len() != expected {
        return Err(format!(
            "{name} pixel buffer length {} does not match {width}x{height} ({expected} bytes)",
            bytes.len()
        ));
    }
    if format == PixelFormat::Rgba {
        for pixel in bytes.chunks_exact_mut(4) {
            pixel.swap(0, 2);
        }
    }
    gpui::RenderImage::from_bgra(width, height, bytes)
        .map(std::sync::Arc::new)
        .ok_or_else(|| format!("{name} pixel buffer is not a valid image"))
}

pub fn render_image_from_encoded(
    bytes: Vec<u8>,
    svg_renderer: gpui::SvgRenderer,
) -> std::result::Result<std::sync::Arc<gpui::RenderImage>, String> {
    let format = sniff_image_format(&bytes)
        .ok_or_else(|| "unrecognized image format".to_string())?;
    gpui::Image::from_bytes(format, bytes)
        .to_image_data(svg_renderer)
        .map_err(|error| error.to_string())
}

fn sniff_image_format(bytes: &[u8]) -> Option<gpui::ImageFormat> {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
        Some(gpui::ImageFormat::Png)
    } else if bytes.starts_with(&[0xFF, 0xD8]) {
        Some(gpui::ImageFormat::Jpeg)
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some(gpui::ImageFormat::Gif)
    } else if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        Some(gpui::ImageFormat::Webp)
    } else if bytes.starts_with(b"BM") {
        Some(gpui::ImageFormat::Bmp)
    } else if bytes.starts_with(&[0x49, 0x49, 0x2A, 0x00])
        || bytes.starts_with(&[0x4D, 0x4D, 0x00, 0x2A])
    {
        Some(gpui::ImageFormat::Tiff)
    } else if bytes.starts_with(&[0x00, 0x00, 0x01, 0x00]) {
        Some(gpui::ImageFormat::Ico)
    } else if bytes.starts_with(b"P1")
        || bytes.starts_with(b"P2")
        || bytes.starts_with(b"P3")
        || bytes.starts_with(b"P4")
        || bytes.starts_with(b"P5")
        || bytes.starts_with(b"P6")
    {
        Some(gpui::ImageFormat::Pnm)
    } else if looks_like_svg(bytes) {
        Some(gpui::ImageFormat::Svg)
    } else {
        None
    }
}

fn looks_like_svg(bytes: &[u8]) -> bool {
    let start = std::str::from_utf8(bytes)
        .ok()
        .map(|text| text.trim_start())
        .unwrap_or("");
    start.starts_with("<svg") || start.starts_with("<?xml")
}

#[derive(Debug, Clone, Default)]
pub struct SvgElement {
    src: String,
    bytes: Option<std::sync::Arc<[u8]>>,
    source: String,
}

impl SvgElement {
    fn load_src(&mut self, src: String) {
        self.bytes = svg_bytes(&src).map(std::sync::Arc::from);
        self.src = src;
    }
}

fn svg_bytes(src: &str) -> Option<Vec<u8>> {
    if src.starts_with("data:") {
        let (format, bytes) = decode_image_data_url(src)?;
        return (format == gpui::ImageFormat::Svg).then_some(bytes);
    }
    #[cfg(target_family = "wasm")]
    return None;
    #[cfg(not(target_family = "wasm"))]
    std::fs::read(src).ok()
}

fn decode_image_data_url(src: &str) -> Option<(gpui::ImageFormat, Vec<u8>)> {
    let (metadata, data) = src.strip_prefix("data:")?.split_once(',')?;
    let mut parts = metadata.split(';');
    let mime_type = parts.next()?.to_ascii_lowercase();
    let format = gpui::ImageFormat::from_mime_type(&mime_type)?;
    let is_base64 = parts.any(|part| part.eq_ignore_ascii_case("base64"));
    let bytes = if is_base64 {
        base64::engine::general_purpose::STANDARD
            .decode(data)
            .ok()?
    } else {
        percent_decode(data)
    };
    Some((format, bytes))
}

fn percent_decode(input: &str) -> Vec<u8> {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            if let Ok(value) = u8::from_str_radix(
                std::str::from_utf8(&bytes[index + 1..index + 3]).unwrap_or(""),
                16,
            ) {
                out.push(value);
                index += 3;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    out
}

impl CustomElement for SvgElement {
    fn render(
        &mut self,
        ctx: CustomRenderContext,
        _window: &mut gpui::Window,
        _cx: &mut gpui::Context<crate::renderer::GpuixView>,
    ) -> gpui::AnyElement {
        use gpui::prelude::*;

        let bytes = if self.source.trim().is_empty() {
            self.bytes.as_deref()
        } else {
            Some(self.source.as_bytes())
        };
        let element_id = gpui::SharedString::from(format!("__gpuix_svg_{}", ctx.id));
        let Some(bytes) = bytes else {
            let empty = super::custom_surface(gpui::div().id(element_id), &ctx);
            return empty.into_any_element();
        };

        let tint = ctx
            .style
            .and_then(|style| style.color.as_deref())
            .and_then(crate::color::parse_color_rgba)
            .unwrap_or_else(|| gpui::rgb(0xe2e2e2).into());
        let mut icon = gpui::svg()
            .data(bytes)
            .flex_none()
            .text_color(tint)
            .id(element_id);
        if let Some(style) = ctx.style {
            icon = crate::renderer::apply_interactive_styles(icon, style);
        }
        icon = crate::accessibility::apply_accessibility(icon, ctx.props, None);
        let icon = super::wire_standard_events(icon, &ctx);
        crate::automation::track_own_bounds(icon, ctx.id).into_any_element()
    }

    fn set_prop(&mut self, key: &str, value: serde_json::Value) {
        match key {
            "src" => self.load_src(value.as_str().unwrap_or_default().to_string()),
            "source" => self.source = value.as_str().unwrap_or_default().to_string(),
            _ => {}
        }
    }

    fn supported_props(&self) -> &'static [&'static str] {
        &["src", "source"]
    }

    fn supported_events(&self) -> &'static [&'static str] {
        &["click", "mouseEnter", "mouseLeave", "fileDrop"]
    }

    fn destroy(&mut self) {}
}

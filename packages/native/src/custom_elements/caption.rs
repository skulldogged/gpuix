/// `<gpuix-caption action="drag|minimize|maximize|close">`: app-drawn title bar
/// regions that behave like the system's.
///
/// GPUI's `window_control_area` gives Windows the hit-test codes its own title
/// bar uses, so dragging, double-click to maximize, Snap layouts and the
/// caption buttons work natively there. macOS and Linux have no such codes;
/// there a drag area starts a window move and the buttons call the window
/// actions on click. Children are ordinary elements, such as glyphs.
use super::{CustomElement, CustomElementFactory, CustomRenderContext, custom_surface};

pub struct CaptionFactory;

impl CustomElementFactory for CaptionFactory {
    fn element_type(&self) -> &str {
        "gpuix-caption"
    }

    fn create(&self, _id: u64) -> Box<dyn CustomElement> {
        Box::new(Caption::default())
    }

    // Draws its children and hit regions from props alone.
    fn cacheable(&self) -> bool {
        true
    }
}

#[derive(Default)]
struct Caption {
    action: String,
}

impl CustomElement for Caption {
    fn set_prop(&mut self, key: &str, value: serde_json::Value) {
        if key == "action" {
            self.action = value.as_str().unwrap_or("").into();
        }
    }

    fn supported_props(&self) -> &'static [&'static str] {
        &["action"]
    }

    fn supported_events(&self) -> &'static [&'static str] {
        &[]
    }

    fn destroy(&mut self) {}

    fn render(
        &mut self,
        mut ctx: CustomRenderContext,
        _window: &mut gpui::Window,
        _cx: &mut gpui::Context<crate::renderer::GpuixView>,
    ) -> gpui::AnyElement {
        use gpui::prelude::*;

        let children = std::mem::take(&mut ctx.children);
        let area = match self.action.as_str() {
            "minimize" => gpui::WindowControlArea::Min,
            "maximize" => gpui::WindowControlArea::Max,
            "close" => gpui::WindowControlArea::Close,
            _ => gpui::WindowControlArea::Drag,
        };
        let action = self.action.clone();
        let mut el = custom_surface(
            gpui::div().id(gpui::SharedString::from(format!(
                "__gpuix_caption_{}",
                ctx.id
            ))),
            &ctx,
        )
        .window_control_area(area);
        if area == gpui::WindowControlArea::Drag {
            el = el.on_mouse_down(gpui::MouseButton::Left, |event, window, _| {
                // Windows handles the drag area through hit testing.
                if !cfg!(windows) {
                    if event.click_count == 2 {
                        if cfg!(target_os = "linux") {
                            window.zoom_window();
                        } else {
                            window.titlebar_double_click();
                        }
                    } else {
                        window.start_window_move();
                    }
                }
            });
        } else {
            el = el
                .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(move |_, window, cx| {
                    cx.stop_propagation();
                    match action.as_str() {
                        "minimize" => window.minimize_window(),
                        "maximize" => toggle_zoom(window),
                        "close" => window.remove_window(),
                        _ => {}
                    }
                });
        }
        el.children(children).into_any_element()
    }
}

#[cfg(not(windows))]
fn toggle_zoom(window: &gpui::Window) {
    window.zoom_window();
}

/// GPUI's Windows `zoom_window()` only maximizes, although `Window` documents a
/// toggle. Its own HTMAXBUTTON handler restores with `SW_NORMAL`, so this does
/// the same through the window's HWND on the UI thread, without touching
/// GPUI's bounds or cached state.
#[cfg(windows)]
fn toggle_zoom(window: &gpui::Window) {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows::Win32::{
        Foundation::HWND,
        UI::WindowsAndMessaging::{SW_NORMAL, ShowWindowAsync},
    };
    if window.is_maximized() {
        if let Ok(handle) = HasWindowHandle::window_handle(window) {
            if let RawWindowHandle::Win32(handle) = handle.as_raw() {
                unsafe {
                    let _ = ShowWindowAsync(HWND(handle.hwnd.get() as *mut _), SW_NORMAL);
                }
            }
        }
    } else {
        window.zoom_window();
    }
}

//! Window facts the app reads without querying the window, and bundled fonts.
//!
//! The snapshot is refreshed on every frame from the UI thread, so reading it
//! never blocks on the window or races its teardown: after the window closes,
//! `closed` stays true and the last size and state remain readable.
use std::sync::{
    Mutex,
    atomic::{AtomicBool, Ordering},
};

#[derive(Debug, Clone, Copy)]
#[cfg_attr(
    not(all(target_arch = "wasm32", target_os = "unknown")),
    napi_derive::napi(object)
)]
pub struct WindowState {
    pub closed: bool,
    pub maximized: bool,
    pub fullscreen: bool,
    /// Whether this is the foreground window; native caption glyphs dim when not.
    pub active: bool,
    /// The system appearance, for themes that follow it.
    pub dark: bool,
    pub width: f64,
    pub height: f64,
    /// Device pixels per logical pixel on the window's display, for sizing
    /// images fetched for it.
    pub scale: f64,
}

static STATE: Mutex<WindowState> = Mutex::new(WindowState {
    closed: false,
    maximized: false,
    fullscreen: false,
    active: true,
    dark: true,
    width: 0.0,
    height: 0.0,
    scale: 1.0,
});
static OBSERVING: AtomicBool = AtomicBool::new(false);

/// Load `fonts` and start tracking the window. Runs once per application,
/// before the window opens.
pub fn init(cx: &mut gpui::App, fonts: &[String]) {
    let loaded: Vec<std::borrow::Cow<'static, [u8]>> = fonts
        .iter()
        .filter_map(|path| match std::fs::read(path) {
            Ok(bytes) => Some(bytes.into()),
            Err(error) => {
                log::error!("GPUIX could not read the font {path}: {error}");
                None
            }
        })
        .collect();
    if !loaded.is_empty() {
        if let Err(error) = cx.text_system().add_fonts(loaded) {
            log::error!("GPUIX could not load the bundled fonts: {error:#}");
        }
    }
    STATE.lock().unwrap().closed = false;
    OBSERVING.store(false, Ordering::Relaxed);
    cx.on_window_closed(|_, _| STATE.lock().unwrap().closed = true)
        .detach();
}

/// Refresh the snapshot. Called at the start of every frame.
pub fn update<V: 'static>(window: &mut gpui::Window, cx: &mut gpui::Context<V>) {
    // Appearance and activation changes do not render by themselves; render so
    // the snapshot picks them up.
    if !OBSERVING.swap(true, Ordering::Relaxed) {
        cx.observe_window_appearance(window, |_, _, cx| cx.notify())
            .detach();
        cx.observe_window_activation(window, |_, _, cx| cx.notify())
            .detach();
    }
    let size = window.viewport_size();
    let mut state = STATE.lock().unwrap();
    state.maximized = window.is_maximized();
    state.fullscreen = window.is_fullscreen();
    state.active = window.is_window_active();
    state.dark = matches!(
        window.appearance(),
        gpui::WindowAppearance::Dark | gpui::WindowAppearance::VibrantDark
    );
    state.width = f32::from(size.width) as f64;
    state.height = f32::from(size.height) as f64;
    state.scale = window.scale_factor() as f64;
}

pub fn get() -> WindowState {
    *STATE.lock().unwrap()
}

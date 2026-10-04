//! Shared automation host: paint bounds and a controllable motion clock.
//!
//! Record bounds during **paint**, not prepaint. The frame reset canvas
//! clears the map in paint, and GPUI prepaint runs for the whole tree
//! before any paint. A prepaint recorder would be wiped by the reset.
//!
//! TestGpuixRenderer and GpuixRenderer both use this so locators, screenshots,
//! and clock control do not fork between headless tests and a live window.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpui::{
    canvas, point, px, App, Bounds, InputEvent, IntoElement, KeyDownEvent, KeyUpEvent, Keystroke,
    Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Styled, Window,
};
use web_time::Instant;

#[derive(Clone, Copy, Debug)]
pub struct ElementBounds {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl ElementBounds {
    fn from_gpui(bounds: Bounds<Pixels>) -> Self {
        Self {
            x: f64::from(f32::from(bounds.origin.x)),
            y: f64::from(f32::from(bounds.origin.y)),
            width: f64::from(f32::from(bounds.size.width)),
            height: f64::from(f32::from(bounds.size.height)),
        }
    }
}

thread_local! {
    static BOUNDS: RefCell<BoundsRegistry> = RefCell::new(BoundsRegistry::default());
}

/// Painted boxes, kept per view.
///
/// A cached region (`gpuix-cache`) paints from the previous frame without
/// running its elements' paint callbacks, so nothing would record its boxes
/// again. Each view's boxes are therefore kept from the last time it painted,
/// and a box counts while its view is still on screen: the root, or a region
/// the view that last rendered it still holds.
#[derive(Default)]
struct BoundsRegistry {
    root: Option<gpui::EntityId>,
    by_view: HashMap<gpui::EntityId, HashMap<u64, ElementBounds>>,
    /// The view each element was last painted in.
    owner: HashMap<u64, gpui::EntityId>,
    /// The regions each view built when it last rendered, and back again.
    children: HashMap<gpui::EntityId, Vec<gpui::EntityId>>,
    parent: HashMap<gpui::EntityId, gpui::EntityId>,
}

impl BoundsRegistry {
    fn present(&self, mut view: gpui::EntityId) -> bool {
        loop {
            if Some(view) == self.root {
                return true;
            }
            match self.parent.get(&view) {
                Some(&parent) => view = parent,
                None => return false,
            }
        }
    }

    fn get(&self, id: u64) -> Option<ElementBounds> {
        let view = *self.owner.get(&id)?;
        if !self.present(view) {
            return None;
        }
        self.by_view.get(&view)?.get(&id).copied()
    }
}

/// A view is about to render afresh: forget the boxes it painted and the
/// regions it built, which it records again.
pub fn begin_view(view: gpui::EntityId, root: bool) {
    BOUNDS.with(|cell| {
        let mut registry = cell.borrow_mut();
        if root {
            registry.root = Some(view);
        }
        registry.by_view.entry(view).or_default().clear();
        for child in registry.children.remove(&view).unwrap_or_default() {
            if registry.parent.get(&child) == Some(&view) {
                registry.parent.remove(&child);
            }
        }
    });
}

/// `view` built the cached region `region` this frame.
pub fn region_built(view: gpui::EntityId, region: gpui::EntityId) {
    BOUNDS.with(|cell| {
        let mut registry = cell.borrow_mut();
        registry.children.entry(view).or_default().push(region);
        registry.parent.insert(region, view);
    });
}

/// Drop a region's bookkeeping once its element is gone.
pub fn forget_view(view: gpui::EntityId) {
    BOUNDS.with(|cell| {
        let mut registry = cell.borrow_mut();
        registry.by_view.remove(&view);
        registry.children.remove(&view);
        registry.parent.remove(&view);
    });
}

/// Zero-size canvas. Keep it ahead of the app subtree under the root.
///
/// Everything here is recorded during **paint**, never prepaint: gpui's
/// `List::prepaint` speculatively prepaints a row range and can roll the window
/// back and prepaint a different one, so a prepaint-recorded box can belong to a
/// row that never reached the screen.
pub fn bounds_frame_reset() -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |_, _, window, _| {
            let view = window.current_view();
            BOUNDS.with(|cell| {
                if let Some(bounds) = cell.borrow_mut().by_view.get_mut(&view) {
                    bounds.clear();
                }
            });
        },
    )
    .absolute()
    .w(px(0.0))
    .h(px(0.0))
}

/// Record this element's own painted box, with no extra element in the tree.
///
/// `bounds_tracker` needs a positioned parent and one canvas child, which a leaf
/// such as `gpui::img` cannot have. Wrapping the leaf in a div instead would
/// move the layout box: the wrapper would become the flex item and the image
/// would lose intrinsic sizing and corner clipping.
pub fn track_own_bounds<E: gpui::InteractiveElement>(el: E, id: u64) -> E {
    el.on_painted(move |bounds, window, _| record_bounds(id, bounds, window))
}

/// Record this element's padding box as it paints, as `bounds_tracker` does,
/// without adding an element. Every `<div>` and `<text>` carried a tracker
/// child, which doubled the elements each frame laid out and painted.
/// `border` is the element's border widths: the padding box is the border box
/// inset by them, which is where the tracker's absolute child sat.
pub fn track_padding_bounds<E: gpui::InteractiveElement>(
    el: E,
    id: u64,
    border: gpui::Edges<Pixels>,
    selection_start: Option<bool>,
) -> E {
    el.on_painted(move |bounds, window, _| {
        let bounds = Bounds::from_corners(
            gpui::point(bounds.left() + border.left, bounds.top() + border.top),
            gpui::point(
                bounds.right() - border.right,
                bounds.bottom() - border.bottom,
            ),
        );
        record_bounds(id, bounds, window);
        if let Some(selectable) = selection_start {
            crate::text::record_start_region(bounds, selectable);
        }
    })
}

/// Record `id`'s painted box against the view painting it.
pub fn record_bounds(id: u64, bounds: Bounds<Pixels>, window: &Window) {
    let view = window.current_view();
    BOUNDS.with(|cell| {
        let mut registry = cell.borrow_mut();
        registry.owner.insert(id, view);
        registry
            .by_view
            .entry(view)
            .or_default()
            .insert(id, ElementBounds::from_gpui(bounds));
    });
}

pub fn get_bounds(id: u64) -> Option<ElementBounds> {
    BOUNDS.with(|cell| cell.borrow().get(id))
}

pub fn all_bounds() -> HashMap<u64, ElementBounds> {
    BOUNDS.with(|cell| {
        let registry = cell.borrow();
        registry
            .owner
            .keys()
            .filter_map(|&id| registry.get(id).map(|bounds| (id, bounds)))
            .collect()
    })
}

pub fn bounds_tracker(id: u64, selection_start: Option<bool>) -> impl IntoElement {
    canvas(
        |bounds, _, _| bounds,
        move |bounds, _, window, _| {
            record_bounds(id, bounds, window);
            if let Some(selectable) = selection_start {
                crate::text::record_start_region(bounds, selectable);
            }
        },
    )
    // Inset 0, or Taffy places an absolute child at the content box and the
    // box, click target and selection region all shift by the padding.
    .absolute()
    .top_0()
    .left_0()
    .size_full()
}

enum ClockMode {
    Live,
    Frozen { now: Instant },
}

struct ClockInner {
    origin: Instant,
    mode: ClockMode,
}

#[derive(Clone)]
pub struct AutomationClock {
    inner: Arc<Mutex<ClockInner>>,
}

impl Default for AutomationClock {
    fn default() -> Self {
        Self::new()
    }
}

impl AutomationClock {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(ClockInner {
                origin: Instant::now(),
                mode: ClockMode::Live,
            })),
        }
    }

    pub fn now(&self) -> Instant {
        let inner = self.inner.lock().unwrap();
        match inner.mode {
            ClockMode::Live => Instant::now(),
            ClockMode::Frozen { now } => now,
        }
    }

    #[allow(dead_code)]
    pub fn now_ms(&self) -> f64 {
        let inner = self.inner.lock().unwrap();
        current_instant(&inner)
            .saturating_duration_since(inner.origin)
            .as_secs_f64()
            * 1000.0
    }

    pub fn pause(&self) -> f64 {
        let mut inner = self.inner.lock().unwrap();
        let now = current_instant(&inner);
        inner.mode = ClockMode::Frozen { now };
        now.saturating_duration_since(inner.origin).as_secs_f64() * 1000.0
    }

    pub fn set_ms(&self, now_ms: f64) -> f64 {
        let mut inner = self.inner.lock().unwrap();
        let now = inner.origin + duration_ms(now_ms);
        inner.mode = ClockMode::Frozen { now };
        now_ms
    }

    pub fn fast_forward_ms(&self, delta_ms: f64) -> f64 {
        let mut inner = self.inner.lock().unwrap();
        let now = current_instant(&inner) + duration_ms(delta_ms);
        inner.mode = ClockMode::Frozen { now };
        now.saturating_duration_since(inner.origin).as_secs_f64() * 1000.0
    }

    pub fn resume(&self) -> f64 {
        let mut inner = self.inner.lock().unwrap();
        let elapsed = current_instant(&inner).saturating_duration_since(inner.origin);
        inner.origin = Instant::now() - elapsed;
        inner.mode = ClockMode::Live;
        elapsed.as_secs_f64() * 1000.0
    }
}

fn current_instant(inner: &ClockInner) -> Instant {
    match inner.mode {
        ClockMode::Live => Instant::now(),
        ClockMode::Frozen { now } => now,
    }
}

fn duration_ms(ms: f64) -> Duration {
    Duration::from_secs_f64((ms / 1000.0).max(0.0))
}

pub fn mouse_button(button: u32) -> MouseButton {
    match button {
        1 => MouseButton::Middle,
        2 => MouseButton::Right,
        _ => MouseButton::Left,
    }
}

/// Parse the held modifiers of a simulated mouse event from the same
/// hyphenated syntax `press("cmd-a")` already uses: `"cmd"`, `"cmd-shift"`,
/// `"alt"`. `None` and `""` mean no modifier. Unknown names are ignored, so a
/// typo weakens the gesture instead of failing the whole automation call.
///
/// A string, not an object, because the same value has to cross the napi, the
/// wasm and the stdio boundary, and only wasm makes objects awkward.
pub fn parse_modifiers(modifiers: Option<&str>) -> Modifiers {
    let mut parsed = Modifiers::default();
    let Some(text) = modifiers else {
        return parsed;
    };
    for part in text.split('-') {
        match part.trim().to_ascii_lowercase().as_str() {
            "cmd" | "meta" | "super" | "win" | "platform" => parsed.platform = true,
            "ctrl" | "control" => parsed.control = true,
            "alt" | "option" => parsed.alt = true,
            "shift" => parsed.shift = true,
            "fn" | "function" => parsed.function = true,
            _ => {}
        }
    }
    parsed
}

pub fn dispatch_keystrokes(
    window: &mut Window,
    cx: &mut App,
    keystrokes: &str,
) -> Result<(), String> {
    for keystroke in keystrokes.split(' ') {
        window.dispatch_keystroke(parse_keystroke(keystroke)?, cx);
    }
    Ok(())
}

pub fn dispatch_key_down(
    window: &mut Window,
    cx: &mut App,
    keystroke: &str,
    is_held: bool,
) -> Result<(), String> {
    window.dispatch_event(
        KeyDownEvent {
            keystroke: parse_keystroke(keystroke)?,
            is_held,
            prefer_character_input: false,
        }
        .to_platform_input(),
        cx,
    );
    Ok(())
}

pub fn dispatch_key_up(window: &mut Window, cx: &mut App, keystroke: &str) -> Result<(), String> {
    window.dispatch_event(
        KeyUpEvent {
            keystroke: parse_keystroke(keystroke)?,
        }
        .to_platform_input(),
        cx,
    );
    Ok(())
}

fn parse_keystroke(keystroke: &str) -> Result<Keystroke, String> {
    Keystroke::parse(keystroke).map_err(|error| format!("Invalid keystroke '{keystroke}': {error}"))
}

/// Every automation mouse dispatcher takes modifiers, so a test can drive
/// cmd-wheel zoom, shift-click range selection, or alt-drag duplication.
pub fn dispatch_click(
    window: &mut Window,
    cx: &mut App,
    x: f64,
    y: f64,
    button: u32,
    modifiers: Modifiers,
) {
    let position = point(px(x as f32), px(y as f32));
    let button = mouse_button(button);
    window.dispatch_event(
        MouseDownEvent {
            button,
            position,
            modifiers,
            click_count: 1,
            first_mouse: false,
        }
        .to_platform_input(),
        cx,
    );
    window.dispatch_event(
        MouseUpEvent {
            button,
            position,
            modifiers,
            click_count: 1,
        }
        .to_platform_input(),
        cx,
    );
}

pub fn dispatch_mouse_down(
    window: &mut Window,
    cx: &mut App,
    x: f64,
    y: f64,
    button: u32,
    modifiers: Modifiers,
) {
    window.dispatch_event(
        MouseDownEvent {
            button: mouse_button(button),
            position: point(px(x as f32), px(y as f32)),
            modifiers,
            click_count: 1,
            first_mouse: false,
        }
        .to_platform_input(),
        cx,
    );
}

pub fn dispatch_mouse_up(
    window: &mut Window,
    cx: &mut App,
    x: f64,
    y: f64,
    button: u32,
    modifiers: Modifiers,
) {
    window.dispatch_event(
        MouseUpEvent {
            button: mouse_button(button),
            position: point(px(x as f32), px(y as f32)),
            modifiers,
            click_count: 1,
        }
        .to_platform_input(),
        cx,
    );
}

pub fn dispatch_mouse_move(
    window: &mut Window,
    cx: &mut App,
    x: f64,
    y: f64,
    pressed_button: Option<u32>,
    modifiers: Modifiers,
) {
    window.dispatch_event(
        MouseMoveEvent {
            position: point(px(x as f32), px(y as f32)),
            pressed_button: pressed_button.map(mouse_button),
            modifiers,
        }
        .to_platform_input(),
        cx,
    );
}

pub fn dispatch_scroll_wheel(
    window: &mut Window,
    cx: &mut App,
    x: f64,
    y: f64,
    delta_x: f64,
    delta_y: f64,
    modifiers: Modifiers,
) {
    window.dispatch_event(
        gpui::ScrollWheelEvent {
            position: point(px(x as f32), px(y as f32)),
            delta: gpui::ScrollDelta::Pixels(point(px(delta_x as f32), px(delta_y as f32))),
            modifiers,
            touch_phase: gpui::TouchPhase::Moved,
        }
        .to_platform_input(),
        cx,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frozen_clock_holds_and_fast_forwards() {
        let clock = AutomationClock::new();
        clock.set_ms(0.0);
        assert!((clock.now_ms() - 0.0).abs() < 0.001);
        clock.fast_forward_ms(150.0);
        assert!((clock.now_ms() - 150.0).abs() < 0.001);
        let later = clock.now();
        clock.fast_forward_ms(150.0);
        assert_eq!(
            clock.now().saturating_duration_since(later),
            Duration::from_millis(150)
        );
    }
}

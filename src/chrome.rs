//! Window chrome: two buttons revealed near a window's top-right corner.
//! Red square closes, blue square toggles fullscreen. No status strip;
//! chrome lives on the window itself, Idle mode only.

use crate::canvas::{CanvasWindow, Vec2};

/// Pointer within this distance (screen px) of the top-right corner
/// reveals the buttons.
pub const CHROME_REVEAL_RADIUS: f64 = 56.0;
/// Button square size (screen px) and gap between buttons.
pub const CHROME_BUTTON: f64 = 20.0;
pub const CHROME_GAP: f64 = 4.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ChromeButton {
    Close,
    Fullscreen,
}

#[derive(Debug, Clone, Copy)]
pub struct ChromeLayout {
    /// Top-left of the close square, screen px.
    pub close: Vec2,
    /// Top-left of the fullscreen square, screen px.
    pub fullscreen: Vec2,
    pub size: f64,
}

impl ChromeLayout {
    pub fn hit(&self, pointer: Vec2) -> Option<ChromeButton> {
        if in_square(pointer, self.close, self.size) {
            Some(ChromeButton::Close)
        } else if in_square(pointer, self.fullscreen, self.size) {
            Some(ChromeButton::Fullscreen)
        } else {
            None
        }
    }
}

fn in_square(pointer: Vec2, top_left: Vec2, size: f64) -> bool {
    pointer.x >= top_left.x
        && pointer.x < top_left.x + size
        && pointer.y >= top_left.y
        && pointer.y < top_left.y + size
}

/// Buttons for a window drawn at `origin` (screen px of its top-left) with
/// `size` (screen px), when the pointer is near the top-right corner.
/// Returns None when the pointer is too far: no chrome, window stands alone.
pub fn layout_for(origin: Vec2, size: Vec2, pointer: Vec2) -> Option<ChromeLayout> {
    let corner = Vec2::new(origin.x + size.x, origin.y);
    let dx = (pointer.x - corner.x).abs().max(0.0);
    let dy = (pointer.y - corner.y).abs().max(0.0);
    // Reveal zone: above/right/outside allowed, but the pointer must stay
    // within the radius of the corner; deep inside the window hides chrome.
    let inside_deep =
        pointer.x < corner.x - CHROME_REVEAL_RADIUS && pointer.y > corner.y + CHROME_REVEAL_RADIUS;
    if inside_deep {
        return None;
    }
    if dx > CHROME_REVEAL_RADIUS || dy > CHROME_REVEAL_RADIUS {
        return None;
    }
    let close = Vec2::new(
        corner.x - CHROME_BUTTON - CHROME_GAP,
        corner.y + CHROME_GAP,
    );
    let fullscreen = Vec2::new(
        close.x - CHROME_GAP - CHROME_BUTTON,
        corner.y + CHROME_GAP,
    );
    Some(ChromeLayout {
        close,
        fullscreen,
        size: CHROME_BUTTON,
    })
}

/// Corners of a window in canvas coordinates (for the fullscreen rect).
pub fn fullscreen_rect_for(view: Vec2, to_canvas: &dyn Fn(Vec2) -> Vec2) -> CanvasWindow {
    let top_left = to_canvas(Vec2::new(0.0, 0.0));
    let bottom_right = to_canvas(view);
    CanvasWindow::new(
        u64::MAX,
        top_left.x,
        top_left.y,
        bottom_right.x - top_left.x,
        bottom_right.y - top_left.y,
    )
}

// Hover keyboard shortcuts (Idle mode, no modifiers held): act on the
// window under the pointer. evdev codes, layout-independent position.
pub const KEY_Q: u32 = 16;
pub const KEY_E: u32 = 18;
pub const KEY_R: u32 = 19;
pub const KEY_F: u32 = 33;

/// Modifier evdev codes that cancel hover shortcuts (typing stays typing).
pub const MOD_CODES: [u32; 6] = [42, 54, 29, 97, 56, 100];

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum HoverKey {
    Fullscreen,
    Close,
    Grab,
    Scale,
}

pub fn hover_shortcut(code: u32, modifiers_held: bool) -> Option<HoverKey> {
    if modifiers_held {
        return None;
    }
    match code {
        KEY_F => Some(HoverKey::Fullscreen),
        KEY_Q => Some(HoverKey::Close),
        KEY_E => Some(HoverKey::Grab),
        KEY_R => Some(HoverKey::Scale),
        _ => None,
    }
}

/// Centre-anchored scale (the R key): while R is held, the window centre
/// stays pinned and each axis follows the mouse away from the grab point —
/// move right to widen, down to heighten, both to grow. Minimum size
/// clamps the rect, never the centre.
pub const SCALE_MIN_W: f64 = 120.0;
pub const SCALE_MIN_H: f64 = 80.0;

/// Scale step: centre stays, size tracks the mouse delta from the grab
/// point, clamped to the minimum. Pure: unit-tested below.
pub fn apply_scale(
    center: Vec2,
    size0: Vec2,
    mouse_delta: Vec2,
) -> (Vec2, Vec2) {
    let w = (size0.x + mouse_delta.x).max(SCALE_MIN_W);
    let h = (size0.y + mouse_delta.y).max(SCALE_MIN_H);
    (Vec2::new(center.x - w * 0.5, center.y - h * 0.5), Vec2::new(w, h))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn win() -> (Vec2, Vec2) {
        // Window top-left (800, 544), size 486x318 on screen.
        (Vec2::new(800.0, 544.0), Vec2::new(486.0, 318.0))
    }
    #[test]
    fn hidden_far_from_corner_and_deep_inside() {
        let (origin, size) = win();
        // Corner at (1286, 544). Far away: nothing.
        assert!(layout_for(origin, size, Vec2::new(100.0, 100.0)).is_none());
        // Deep inside the window: nothing.
        assert!(layout_for(origin, size, Vec2::new(900.0, 700.0)).is_none());
    }

    #[test]
    fn revealed_near_corner_with_hittable_buttons() {
        let (origin, size) = win();
        let layout = layout_for(origin, size, Vec2::new(1270.0, 550.0))
            .expect("near corner reveals chrome");
        // Close sits top-right inside, fullscreen left of it.
        assert!(layout.close.x < origin.x + size.x);
        assert!(layout.fullscreen.x < layout.close.x);
        assert_eq!(layout.hit(Vec2::new(layout.close.x + 10.0, layout.close.y + 10.0)), Some(ChromeButton::Close));
        assert_eq!(
            layout.hit(Vec2::new(layout.fullscreen.x + 10.0, layout.fullscreen.y + 10.0)),
            Some(ChromeButton::Fullscreen)
        );
        assert_eq!(layout.hit(Vec2::new(900.0, 700.0)), None);
    }

    #[test]
    fn fullscreen_rect_covers_view() {
        let view = Vec2::new(1280.0, 800.0);
        let rect = fullscreen_rect_for(view, &|s| s);
        assert_eq!(rect.pos, Vec2::new(0.0, 0.0));
        assert_eq!(rect.size, Vec2::new(1280.0, 800.0));
    }

    #[test]
    fn hover_keys_map_without_modifiers_only() {
        assert_eq!(hover_shortcut(KEY_F, false), Some(HoverKey::Fullscreen));
        assert_eq!(hover_shortcut(KEY_Q, false), Some(HoverKey::Close));
        assert_eq!(hover_shortcut(KEY_E, false), Some(HoverKey::Grab));
        assert_eq!(hover_shortcut(KEY_R, false), Some(HoverKey::Scale));
        assert_eq!(hover_shortcut(KEY_F, true), None);
        assert_eq!(hover_shortcut(30, false), None); // A is for typing
        assert!(MOD_CODES.contains(&42) && MOD_CODES.contains(&29));
    }

    #[test]
    fn apply_scale_pins_center_and_clamps() {
        let center = Vec2::new(300.0, 250.0);
        let size0 = Vec2::new(400.0, 300.0);
        // +40/+20 from the grab point: grows, centre pinned.
        let (pos, size) = apply_scale(center, size0, Vec2::new(40.0, 20.0));
        assert_eq!((size.x, size.y), (440.0, 320.0));
        assert_eq!((pos.x, pos.y), (300.0 - 220.0, 250.0 - 160.0));
        // Shrink: centre still pinned.
        let (pos, size) = apply_scale(center, size0, Vec2::new(-100.0, -50.0));
        assert_eq!((size.x, size.y), (300.0, 250.0));
        assert_eq!((pos.x, pos.y), (150.0, 125.0));
        // Clamp: never below the minimum, centre still pinned.
        let (pos, size) = apply_scale(center, size0, Vec2::new(-1000.0, -1000.0));
        assert_eq!((size.x, size.y), (SCALE_MIN_W, SCALE_MIN_H));
        assert_eq!(
            (pos.x, pos.y),
            (300.0 - SCALE_MIN_W * 0.5, 250.0 - SCALE_MIN_H * 0.5)
        );
    }
}

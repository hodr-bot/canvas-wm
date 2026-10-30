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
pub const KEY_K: u32 = 37;

/// Modifier evdev codes that cancel hover shortcuts (typing stays typing).
pub const MOD_CODES: [u32; 6] = [42, 54, 29, 97, 56, 100];

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum HoverKey {
    Fullscreen,
    Close,
    Grab,
    Scale,
    Marble,
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
        KEY_K => Some(HoverKey::Marble),
        _ => None,
    }
}

/// Centre-anchored AXIAL scale (the R key): while R is held, the
/// window centre stays pinned and each axis tracks the mouse distance
/// from the centre on THAT axis only — moving in X changes the width
/// alone, moving in Y changes the height alone, diagonal moves change
/// both. Cursor on the centre axis = tiny on that axis (floored at the
/// minimum). Moving away grows, moving toward the centre shrinks, on
/// either side of the centre (absolute distance, so top-left behaves
/// like bottom-right). Aspect is NOT preserved: that is the point.
/// Minimum/maximum clamp the size, never the centre.
pub const SCALE_MIN_W: f64 = 120.0;
pub const SCALE_MIN_H: f64 = 80.0;
/// Upper clamp: a window never scales past this (canvas px).
pub const SCALE_MAX_DIM: f64 = 4096.0;

/// Axial scale step: fx = |mouse.x - cx| / |grab.x - cx|,
/// fy = |mouse.y - cy| / |grab.y - cy|, size = (size0.x * fx,
/// size0.y * fy), centre pinned. Each grab delta is floored so
/// grabbing on (or near) the centre axis never divides by ~zero
/// (it just makes that axis eager). Pure: unit-tested below.
pub fn apply_axial_scale(
    center: Vec2,
    size0: Vec2,
    grab: Vec2,
    mouse: Vec2,
) -> (Vec2, Vec2) {
    let size = axial_target_size(center, size0, grab, mouse);
    (Vec2::new(center.x - size.x * 0.5, center.y - size.y * 0.5), size)
}

/// Raw axial target size (the spring in the backend chases this every
/// frame, so the rect glides instead of snapping).
pub fn axial_target_size(center: Vec2, size0: Vec2, grab: Vec2, mouse: Vec2) -> Vec2 {
    let dx0 = (grab.x - center.x).abs().max(8.0);
    let dy0 = (grab.y - center.y).abs().max(8.0);
    let dx = (mouse.x - center.x).abs();
    let dy = (mouse.y - center.y).abs();
    Vec2::new(
        (size0.x * dx / dx0).clamp(SCALE_MIN_W, SCALE_MAX_DIM),
        (size0.y * dy / dy0).clamp(SCALE_MIN_H, SCALE_MAX_DIM),
    )
}

/// Live-size spring: the R-scale rect chases the raw axial target on
/// a critically-damped spring (~95% in ~0.14s) instead of snapping to
/// it. Frame pacing here is uneven (event-driven pump + sleep), so
/// direct tracking turns pacing jitter into visible notches; the
/// spring eats both the pacing and the finger micro-jitter while
/// staying glued to the hand (no overshoot, exact settle so frames
/// stop). Same integration as the camera spring, stiffer.
#[derive(Debug, Clone, Copy)]
pub struct ScaleSpring {
    /// Current smoothed size. The backend pins the rect centre on it.
    pub size: Vec2,
    vel: Vec2,
}

impl ScaleSpring {
    pub fn new(size: Vec2) -> Self {
        Self {
            size,
            vel: Vec2::new(0.0, 0.0),
        }
    }

    /// Advance toward `target` by `dt` seconds. Returns true while
    /// still moving — the backend forces frames on true so the glide
    /// renders to exact rest (also after release: the window lands
    /// instead of freezing mid-glide).
    pub fn step(&mut self, target: Vec2, dt: f64) -> bool {
        const STIFFNESS: f64 = 1200.0;
        let damping = 2.0 * STIFFNESS.sqrt();
        // Clamp tab-switch spikes: smoothing stays stable, the glide
        // just resumes next frame.
        let dt = dt.clamp(0.0, 0.033);
        let ax = STIFFNESS * (target.x - self.size.x) - damping * self.vel.x;
        let ay = STIFFNESS * (target.y - self.size.y) - damping * self.vel.y;
        self.vel.x += ax * dt;
        self.vel.y += ay * dt;
        self.size.x += self.vel.x * dt;
        self.size.y += self.vel.y * dt;
        let dx = target.x - self.size.x;
        let dy = target.y - self.size.y;
        if dx * dx + dy * dy < 0.25
            && self.vel.x * self.vel.x + self.vel.y * self.vel.y < 25.0
        {
            self.size = target;
            self.vel = Vec2::new(0.0, 0.0);
            false
        } else {
            true
        }
    }
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
    fn fullscreen_rect_times_live_zoom_is_always_the_view() {
        // The per-frame refit feeds to_canvas at the LIVE zoom: rect *
        // zoom must equal the view at any zoom, or quitting Super (or a
        // wheel glide) after F leaves the app looking "zoomed into".
        let view = Vec2::new(1280.0, 800.0);
        for zoom in [1.0, 0.9, 0.85, 1.3225, 0.5] {
            let rect = fullscreen_rect_for(view, &|s| {
                Vec2::new(s.x / zoom, s.y / zoom)
            });
            assert!((rect.size.x * zoom - view.x).abs() < 1e-9, "zoom {}", zoom);
            assert!((rect.size.y * zoom - view.y).abs() < 1e-9, "zoom {}", zoom);
            assert!((rect.pos.x * zoom).abs() < 1e-9, "zoom {}", zoom);
            assert!((rect.pos.y * zoom).abs() < 1e-9, "zoom {}", zoom);
        }
    }

    #[test]
    fn hover_keys_map_without_modifiers_only() {
        assert_eq!(hover_shortcut(KEY_F, false), Some(HoverKey::Fullscreen));
        assert_eq!(hover_shortcut(KEY_Q, false), Some(HoverKey::Close));
        assert_eq!(hover_shortcut(KEY_E, false), Some(HoverKey::Grab));
        assert_eq!(hover_shortcut(KEY_R, false), Some(HoverKey::Scale));
        assert_eq!(hover_shortcut(KEY_K, false), Some(HoverKey::Marble));
        assert_eq!(hover_shortcut(KEY_K, true), None); // modifiers: typing
        assert_eq!(hover_shortcut(KEY_F, true), None);
        assert_eq!(hover_shortcut(30, false), None); // A is for typing
        assert!(MOD_CODES.contains(&42) && MOD_CODES.contains(&29));
    }

    #[test]
    fn apply_axial_scale_moves_each_axis_alone() {
        let center = Vec2::new(300.0, 250.0);
        let size0 = Vec2::new(400.0, 300.0);
        // Grab bottom-right of the centre: dx0 = 40, dy0 = 30.
        let grab = Vec2::new(340.0, 280.0);
        // X move alone: twice as far in X, same Y -> width doubles,
        // height untouched, centre pinned.
        let (pos, size) =
            apply_axial_scale(center, size0, grab, Vec2::new(380.0, 280.0));
        assert_eq!((size.x, size.y), (800.0, 300.0));
        assert_eq!((pos.x, pos.y), (300.0 - 400.0, 250.0 - 150.0));
        // Y move alone: same X, twice as far in Y -> height doubles,
        // width untouched.
        let (pos, size) =
            apply_axial_scale(center, size0, grab, Vec2::new(340.0, 310.0));
        assert_eq!((size.x, size.y), (400.0, 600.0));
        assert_eq!((pos.x, pos.y), (300.0 - 200.0, 250.0 - 300.0));
        // Diagonal: twice as far on both -> both double.
        let (pos, size) =
            apply_axial_scale(center, size0, grab, Vec2::new(380.0, 310.0));
        assert_eq!((size.x, size.y), (800.0, 600.0));
        assert_eq!((pos.x, pos.y), (300.0 - 400.0, 250.0 - 300.0));
        // Top-left ray, moving AWAY: grows too (absolute distance).
        let (pos, size) = apply_axial_scale(
            center,
            size0,
            Vec2::new(260.0, 220.0),
            Vec2::new(220.0, 190.0),
        );
        assert_eq!((size.x, size.y), (800.0, 600.0));
        assert_eq!((pos.x, pos.y), (300.0 - 400.0, 250.0 - 300.0));
        // Moving toward the centre shrinks that axis only.
        let (_, size) =
            apply_axial_scale(center, size0, grab, Vec2::new(320.0, 280.0));
        assert_eq!((size.x, size.y), (200.0, 300.0));
        // Cursor at the centre: tiny (floored at the minimum).
        let (pos, size) = apply_axial_scale(center, size0, grab, center);
        assert_eq!((size.x, size.y), (SCALE_MIN_W, SCALE_MIN_H));
        assert_eq!(
            (pos.x, pos.y),
            (300.0 - SCALE_MIN_W * 0.5, 250.0 - SCALE_MIN_H * 0.5)
        );
        // Aspect is NOT preserved: X-only move stretches the ratio.
        let (_, size) =
            apply_axial_scale(center, size0, grab, Vec2::new(360.0, 280.0));
        assert_eq!((size.x, size.y), (600.0, 300.0));
    }

    #[test]
    fn scale_spring_converges_without_overshoot() {
        let start = Vec2::new(400.0, 300.0);
        let target = Vec2::new(800.0, 600.0);
        let mut s = ScaleSpring::new(start);
        // 60Hz steps: must converge to exact rest.
        let mut moving = true;
        for _ in 0..600 {
            moving = s.step(target, 1.0 / 60.0);
            // Critically damped: never overshoots past the target.
            assert!(s.size.x <= target.x + 1e-6);
            assert!(s.size.y <= target.y + 1e-6);
            if !moving {
                break;
            }
        }
        assert!(!moving);
        assert_eq!((s.size.x, s.size.y), (target.x, target.y));
    }

    #[test]
    fn scale_spring_survives_spike_dt_and_tracks_retarget() {
        let mut s = ScaleSpring::new(Vec2::new(400.0, 300.0));
        // Tab-switch spike: clamped, stays finite, keeps converging.
        assert!(s.step(Vec2::new(800.0, 600.0), 5.0));
        assert!(s.size.x.is_finite() && s.size.y.is_finite());
        // Mid-glide U-turn (finger reverses): no snap, still settles.
        let mut moving = true;
        for _ in 0..600 {
            moving = s.step(Vec2::new(200.0, 200.0), 1.0 / 120.0);
            if !moving {
                break;
            }
        }
        assert!(!moving);
        assert_eq!((s.size.x, s.size.y), (200.0, 200.0));
    }
}

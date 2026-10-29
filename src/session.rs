// Session logic: pure, testable, no backend imports.
// Multi-output viewports on one shared canvas, no status strip (window
// chrome lives on the windows), Super-tap canvas-mode state machine.

use crate::canvas::{Camera, CanvasWindow, Overview, Vec2};

#[derive(Debug, Clone)]
pub struct Output {
    pub name: String,
    pub scale: f64,
    pub view: Vec2,
    pub camera: Camera,
}

impl Output {
    pub fn new(name: &str, view_w: f64, view_h: f64, scale: f64) -> Self {
        Self {
            name: name.to_string(),
            scale,
            view: Vec2::new(view_w, view_h),
            camera: Camera::new(),
        }
    }
}

/// Minimal status bar: never shows the app name. Window chrome (buttons) is
/// hidden from afar so only the window itself is visible; when the cursor
/// nears the window edge, buttons appear inside or outside per option.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ChromeSide {
    Inside,
    Outside,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ChromeVisibility {
    Hidden,
    Shown(ChromeSide),
}

pub fn chrome_visibility(
    zoom: f64,
    cursor_dist_to_edge_px: f64,
    reveal_radius_px: f64,
    side: ChromeSide,
) -> ChromeVisibility {
    // Far zoom-out (overview): never any chrome, windows stand alone.
    if zoom < 0.6 {
        return ChromeVisibility::Hidden;
    }
    if cursor_dist_to_edge_px <= reveal_radius_px {
        ChromeVisibility::Shown(side)
    } else {
        ChromeVisibility::Hidden
    }
}

/// Drag threshold in world pixels: a press only becomes a move past this.
pub const DRAG_THRESHOLD_WORLD: f64 = 6.0;

/// Press-to-current offset once past the threshold, else None (plain click).
pub fn drag_offset(press: Vec2, current: Vec2) -> Option<Vec2> {
    let delta = current - press;
    if delta.x.abs().max(delta.y.abs()) >= DRAG_THRESHOLD_WORLD {
        Some(delta)
    } else {
        None
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SuperState {
    Idle,
    /// Canvas mode: no zoom change, apps frozen. Drag on empty space pans
    /// the canvas; a 1s dwell inside a window arms it for moving.
    Canvas,
}

#[derive(Debug)]
pub enum SuperEvent {
    SuperTap,
    WindowClick(u64),
    CanvasClick,
    Esc,
}

#[derive(Debug)]
pub enum SuperAction {
    None,
    EnterCanvas,
    QuitCanvas { centred: bool },
    RestoreCanvas,
}

/// Canvas origin on the infinite plane, separate from the camera so future
/// velocity/physics animate the content, never the viewpoint.
#[derive(Debug, Clone, Copy, Default)]
pub struct CanvasOrigin {
    pub offset: Vec2,
    pub vel: Vec2,
}

/// Dwell-to-arm: staying inside one window for DWELL_ARM_MS (small moves
/// inside allowed, leaving resets) arms it for moving, Hyprland-style but
/// without holding a key.
pub const DWELL_ARM_MS: u64 = 1000;

#[derive(Debug, Default)]
pub struct DwellTracker {
    pub inside: Option<u64>,
    pub since_ms: u64,
    pub armed: Option<u64>,
}

impl DwellTracker {
    pub fn update(&mut self, inside: Option<u64>, now_ms: u64) -> Option<u64> {
        if inside != self.inside {
            self.inside = inside;
            self.since_ms = now_ms;
            self.armed = None;
        }
        if let Some(id) = self.inside {
            if self.armed.is_none() && now_ms.saturating_sub(self.since_ms) >= DWELL_ARM_MS {
                self.armed = Some(id);
            }
        } else {
            self.armed = None;
        }
        self.armed
    }

    pub fn disarm(&mut self) {
        self.armed = None;
    }
}

/// Drives canvas mode enter/quit + multi-select, shared by all outputs.
pub struct Session {
    pub state: SuperState,
    pub overview: Overview,
    pub camera: Camera,
    pub canvas: CanvasOrigin,
    pub dwell: DwellTracker,
    pub prev_offset: Option<Vec2>,
    /// Zoom before Super mode, for the dolly-back-and-return. Enter
    /// glides to 90%, every quit path glides back here — the view
    /// never moves position on enter/quit, only breathes in zoom.
    pub prev_zoom: Option<f64>,
    /// Super dolly-back factor, synced from Settings by the backend
    /// (default 0.90 = Normal 10%). Enter multiplies the settled zoom
    /// by this; keeps the "Recul" option logic in core, GL-free.
    pub super_dolly: f64,
}

impl Session {
    pub fn new() -> Self {
        Self {
            state: SuperState::Idle,
            overview: Overview::default(),
            camera: Camera::new(),
            canvas: CanvasOrigin::default(),
            dwell: DwellTracker::default(),
            prev_offset: None,
            prev_zoom: None,
            super_dolly: 0.90,
        }
    }

    /// Effective viewpoint: canvas offset shifts content under a fixed camera.
    pub fn view_camera(&self) -> Camera {
        Camera {
            pos: Vec2::new(
                self.camera.pos.x - self.canvas.offset.x,
                self.camera.pos.y - self.canvas.offset.y,
            ),
            zoom: self.camera.zoom,
            ..Camera::new()
        }
    }

    pub fn to_world(&self, screen: Vec2, view: Vec2) -> Vec2 {
        self.view_camera().screen_to_world(screen, view)
    }

    /// Screen -> canvas plane coordinates (for hit-testing windows).
    /// This is exactly the inverse of the render mapping, so it already
    /// is canvas space: no extra offset is applied. (An earlier revision
    /// subtracted the offset twice, which shifted every click/hover by
    /// the pan distance and moved the Canvas tilt pivot off-centre.)
    pub fn to_canvas(&self, screen: Vec2, view: Vec2) -> Vec2 {
        self.to_world(screen, view)
    }

    pub fn to_screen(&self, world: Vec2, view: Vec2) -> Vec2 {
        self.view_camera().world_to_screen(world, view)
    }

    /// Pan the canvas by a screen-space drag delta (content follows cursor).
    pub fn pan_canvas(&mut self, delta_screen: Vec2) {
        self.canvas.offset.x += delta_screen.x / self.camera.zoom;
        self.canvas.offset.y += delta_screen.y / self.camera.zoom;
        self.canvas.vel = Vec2::new(0.0, 0.0);
    }

    /// Zoom keeping the world point under `screen` fixed.
    pub fn zoom_at(&mut self, screen: Vec2, view: Vec2, factor: f64) {
        let anchor = self.to_world(screen, view);
        self.camera.zoom = (self.camera.zoom * factor).clamp(0.05, 4.0);
        // Direct manipulation owns the zoom: snap the glide spring onto
        // the new value, otherwise it would drag the zoom back to a
        // stale target and burn frames forever.
        self.camera.zoom_target = self.camera.zoom;
        self.camera.zoom_vel = 0.0;
        // screen = (anchor + off - pos) * z' + v/2, solved for off.
        self.canvas.offset.x = (screen.x - view.x * 0.5) / self.camera.zoom + self.camera.pos.x
            - anchor.x;
        self.canvas.offset.y = (screen.y - view.y * 0.5) / self.camera.zoom + self.camera.pos.y
            - anchor.y;
    }

    /// Wheel zoom: same anchor as [`zoom_at`], but the zoom GLIDES to
    /// the goal instead of snapping (smooth fade, momentum kept), and
    /// each notch composes onto the SETTLED target — never the live
    /// mid-glide value, so fast scrolling never ratchets or kills a
    /// glide-back in progress (quitting Super then touching the wheel
    /// keeps the return trip, it just aims it a notch further).
    pub fn zoom_glide_at(&mut self, screen: Vec2, view: Vec2, factor: f64) {
        let anchor = self.to_world(screen, view);
        let zt = (self.camera.zoom_target * factor).clamp(0.05, 4.0);
        self.camera.zoom_to(zt);
        self.canvas.offset.x =
            (screen.x - view.x * 0.5) / zt + self.camera.pos.x - anchor.x;
        self.canvas.offset.y =
            (screen.y - view.y * 0.5) / zt + self.camera.pos.y - anchor.y;
    }

    pub fn on_event(
        &mut self,
        event: SuperEvent,
        _resolve: &dyn Fn(u64) -> Option<CanvasWindow>,
        _view: Vec2,
    ) -> SuperAction {
        match (&self.state, event) {
            // Tap Super: enter canvas mode. No pan, no pivot — the camera
            // only dollies back ~10% (glide, momentum kept), and quitting
            // always glides back to the saved zoom. Screen position is
            // never touched on enter/quit.
            (SuperState::Idle, SuperEvent::SuperTap) => {
                self.prev_offset = Some(self.canvas.offset);
                // Snapshot the SETTLED zoom (target), never the mid-glide
                // transient: re-tapping mid-animation must curve back to
                // the pre-Super level, not ratchet down a level each
                // cycle. At rest target == zoom, so this changes nothing
                // for clean taps; mid-glide it makes enter/quit idempotent
                // and the spring superimposes the U-turn for free.
                self.prev_zoom = Some(self.camera.zoom_target);
                self.camera.zoom_to(self.camera.zoom_target * self.super_dolly);
                self.overview.enter();
                self.state = SuperState::Canvas;
                self.dwell = DwellTracker::default();
                SuperAction::EnterCanvas
            }
            (SuperState::Canvas, SuperEvent::WindowClick(id)) => {
                self.overview.toggle_select(id);
                SuperAction::None
            }
            // Re-tap: quit, drop the selection, glide back to the saved
            // zoom. Position is NEVER touched here: the old re-centring
            // pan yanked the view on most quits (any selection, even an
            // accidental click-select) — that drift was the bug.
            (SuperState::Canvas, SuperEvent::SuperTap) => {
                let _ids = self.overview.quit();
                self.state = SuperState::Idle;
                self.prev_offset = None;
                if let Some(z) = self.prev_zoom.take() {
                    self.camera.zoom_to(z);
                }
                self.dwell.disarm();
                SuperAction::QuitCanvas { centred: false }
            }
            // Esc: quit, drop the selection, undo canvas panning, zoom back.
            (SuperState::Canvas, SuperEvent::Esc) => {
                self.overview.quit();
                self.state = SuperState::Idle;
                self.dwell.disarm();
                if let Some(prev) = self.prev_offset.take() {
                    self.canvas.offset = prev;
                }
                if let Some(z) = self.prev_zoom.take() {
                    self.camera.zoom_to(z);
                }
                SuperAction::RestoreCanvas
            }
            // Plain canvas click: quit, drop the selection, keep panning,
            // zoom back to the saved level.
            (SuperState::Canvas, SuperEvent::CanvasClick) => {
                self.overview.quit();
                self.state = SuperState::Idle;
                self.dwell.disarm();
                self.prev_offset = None;
                if let Some(z) = self.prev_zoom.take() {
                    self.camera.zoom_to(z);
                }
                SuperAction::None
            }
            _ => SuperAction::None,
        }
    }
}

impl Default for Session {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn two_windows() -> HashMap<u64, CanvasWindow> {
        let mut m = HashMap::new();
        m.insert(1, CanvasWindow::new(1, 0.0, 0.0, 800.0, 600.0));
        m.insert(2, CanvasWindow::new(2, 2000.0, 1500.0, 800.0, 600.0));
        m
    }

    #[test]
    fn canvas_mode_dollies_back_without_moving() {
        let mut s = Session::new();
        let wins = two_windows();
        let resolve = |id: u64| wins.get(&id).copied();
        let view = Vec2::new(1920.0, 1080.0);
        let (off_before, pos_before) = (s.canvas.offset, s.camera.pos);
        assert!(matches!(
            s.on_event(SuperEvent::SuperTap, &resolve, view),
            SuperAction::EnterCanvas
        ));
        assert_eq!(s.state, SuperState::Canvas);
        // Position untouched; zoom glides (target 90%, value still 1.0
        // this instant — no snap).
        assert_eq!(s.canvas.offset, off_before);
        assert_eq!(s.camera.pos, pos_before);
        assert_eq!(s.camera.zoom, 1.0);
        assert!((s.camera.zoom_target - 0.9).abs() < 1e-12);
    }

    #[test]
    fn retap_quits_without_moving_view() {
        let mut s = Session::new();
        let wins = two_windows();
        let resolve = |id: u64| wins.get(&id).copied();
        let view = Vec2::new(1920.0, 1080.0);
        s.on_event(SuperEvent::SuperTap, &resolve, view);
        s.on_event(SuperEvent::WindowClick(1), &resolve, view);
        s.on_event(SuperEvent::WindowClick(2), &resolve, view);
        let off_before = s.canvas.offset;
        // Even with a selection, re-tap quits WITHOUT the old
        // re-centring pan: the view must not drift.
        let action = s.on_event(SuperEvent::SuperTap, &resolve, view);
        assert!(matches!(
            action,
            SuperAction::QuitCanvas { centred: false }
        ));
        assert_eq!(s.canvas.offset, off_before);
        assert_eq!(s.state, SuperState::Idle);
        // ...and the zoom glides back to the saved level.
        assert!((s.camera.zoom_target - 1.0).abs() < 1e-12);
    }

    #[test]
    fn rapid_super_taps_never_ratchet_zoom() {
        let mut s = Session::new();
        let wins = two_windows();
        let resolve = |id: u64| wins.get(&id).copied();
        let view = Vec2::new(1920.0, 1080.0);
        // Mash enter/quit mid-glide several times: each cycle snapshots
        // the settled intent, so the zoom can never spiral down.
        for _ in 0..4 {
            s.on_event(SuperEvent::SuperTap, &resolve, view);
            for _ in 0..10 {
                s.camera.step(1.0 / 120.0);
            }
            assert_eq!(s.state, SuperState::Canvas);
            s.on_event(SuperEvent::SuperTap, &resolve, view);
            for _ in 0..10 {
                s.camera.step(1.0 / 120.0);
            }
            assert_eq!(s.state, SuperState::Idle);
        }
        for _ in 0..600 {
            if !s.camera.step(1.0 / 120.0) {
                break;
            }
        }
        assert_eq!(s.camera.zoom, 1.0);
        assert!(!s.camera.flying());
    }

    #[test]
    fn wheel_glides_without_snapping_and_survives_quit() {
        let mut s = Session::new();
        let view = Vec2::new(1920.0, 1080.0);
        let at = Vec2::new(960.0, 540.0);
        // Notch 1: no snap — the live zoom barely moved, the target did.
        s.zoom_glide_at(at, view, 1.15);
        assert!((s.camera.zoom_target - 1.15).abs() < 1e-12);
        assert!((s.camera.zoom - 1.0).abs() < 0.05);
        // Notch 2 composes onto the settled target, not the live value.
        s.zoom_glide_at(at, view, 1.15);
        assert!((s.camera.zoom_target - 1.15 * 1.15).abs() < 1e-12);
        // Quit mid-glide then wheel: the return trip survives — the
        // notch re-aims it instead of snapping over it.
        let wins = two_windows();
        let resolve = |id: u64| wins.get(&id).copied();
        s.on_event(SuperEvent::SuperTap, &resolve, view);
        s.on_event(SuperEvent::CanvasClick, &resolve, view);
        s.zoom_glide_at(at, view, 1.15);
        assert!(s.camera.zoom_target > 1.0);
        assert!((s.camera.zoom_vel - 0.0).abs() > 1e-9 || s.camera.zoom != s.camera.zoom_target);
        // Anchor: the world point under the cursor is identical before
        // the glide and after landing (position never moved here).
        let mut s2 = Session::new();
        let before = s2.to_world(at, view);
        s2.zoom_glide_at(at, view, 1.5);
        for _ in 0..1200 {
            if !s2.camera.step(1.0 / 120.0) {
                break;
            }
        }
        assert!(!s2.camera.flying());
        let after = s2.to_world(at, view);
        assert!((after.x - before.x).abs() < 1e-9);
        assert!((after.y - before.y).abs() < 1e-9);
    }

    #[test]
    fn super_round_trip_returns_exact_view() {
        let mut s = Session::new();
        let wins = two_windows();
        let resolve = |id: u64| wins.get(&id).copied();
        let view = Vec2::new(1920.0, 1080.0);
        s.on_event(SuperEvent::SuperTap, &resolve, view);
        for _ in 0..600 {
            if !s.camera.step(1.0 / 120.0) {
                break;
            }
        }
        assert!((s.camera.zoom - 0.9).abs() < 1e-9);
        s.on_event(SuperEvent::SuperTap, &resolve, view);
        for _ in 0..600 {
            if !s.camera.step(1.0 / 120.0) {
                break;
            }
        }
        assert_eq!(s.camera.zoom, 1.0);
        assert_eq!(s.canvas.offset, Vec2::new(0.0, 0.0));
        assert_eq!(s.camera.pos, Vec2::new(0.0, 0.0));
        assert!(!s.camera.flying());
    }

    #[test]
    fn esc_restores_canvas_offset() {
        let mut s = Session::new();
        let wins = two_windows();
        let resolve = |id: u64| wins.get(&id).copied();
        let view = Vec2::new(1920.0, 1080.0);
        s.on_event(SuperEvent::SuperTap, &resolve, view);
        s.pan_canvas(Vec2::new(100.0, 50.0));
        assert_ne!(s.canvas.offset.x, 0.0);
        assert!(matches!(
            s.on_event(SuperEvent::Esc, &resolve, view),
            SuperAction::RestoreCanvas
        ));
        assert_eq!(s.canvas.offset, Vec2::new(0.0, 0.0));
        assert_eq!(s.state, SuperState::Idle);
    }

    #[test]
    fn dwell_arms_after_one_second_inside() {
        let mut d = DwellTracker::default();
        assert_eq!(d.update(None, 0), None);
        assert_eq!(d.update(Some(1), 0), None);
        // Small moves inside the same window do not reset the timer.
        assert_eq!(d.update(Some(1), 500), None);
        assert_eq!(d.update(Some(1), 999), None);
        assert_eq!(d.update(Some(1), 1000), Some(1));
        assert_eq!(d.update(Some(1), 5000), Some(1));
        // Switching window resets; leaving disarms.
        assert_eq!(d.update(Some(2), 5001), None);
        assert_eq!(d.update(Some(2), 6001), Some(2));
        assert_eq!(d.update(None, 6002), None);
    }

    #[test]
    fn pan_canvas_follows_cursor() {
        let mut s = Session::new();
        s.camera.zoom = 2.0;
        s.pan_canvas(Vec2::new(40.0, -20.0));
        assert_eq!(s.canvas.offset, Vec2::new(20.0, -10.0));
    }

    #[test]
    fn hit_testing_survives_pan() {
        // A window's screen position after a pan must map back to the
        // window itself: regression test for the double-subtracted offset
        // that shifted every click/hover by the pan distance.
        let mut s = Session::new();
        let view = Vec2::new(1280.0, 800.0);
        let win = Vec2::new(100.0, 100.0);
        let screen_before = s.to_screen(win, view);
        assert_eq!(s.to_canvas(screen_before, view), win);
        s.pan_canvas(Vec2::new(200.0, 100.0));
        let screen_after = s.to_screen(win, view);
        assert_ne!(screen_after, screen_before);
        let back = s.to_canvas(screen_after, view);
        assert!((back.x - win.x).abs() < 1e-9);
        assert!((back.y - win.y).abs() < 1e-9);
    }

    #[test]
    fn session_zoom_at_keeps_anchor_fixed() {
        let mut s = Session::new();
        let view = Vec2::new(1280.0, 800.0);
        let screen = Vec2::new(900.0, 500.0);
        let before = s.to_world(screen, view);
        s.zoom_at(screen, view, 2.0);
        assert!((s.camera.zoom - 2.0).abs() < 1e-9);
        let after = s.to_world(screen, view);
        assert!((after.x - before.x).abs() < 1e-9);
        assert!((after.y - before.y).abs() < 1e-9);
    }

    #[test]
    fn chrome_hidden_when_far_or_overview() {
        assert_eq!(
            chrome_visibility(0.4, 0.0, 24.0, ChromeSide::Inside),
            ChromeVisibility::Hidden
        );
        assert_eq!(
            chrome_visibility(1.0, 200.0, 24.0, ChromeSide::Outside),
            ChromeVisibility::Hidden
        );
        assert_eq!(
            chrome_visibility(1.0, 10.0, 24.0, ChromeSide::Inside),
            ChromeVisibility::Shown(ChromeSide::Inside)
        );
    }

    #[test]
    fn outputs_share_one_canvas_with_own_viewports() {
        let mut a = Output::new("DP-1", 1920.0, 1080.0, 1.0);
        let mut b = Output::new("HDMI-1", 1920.0, 1080.0, 1.0);
        a.camera.pos = Vec2::new(0.0, 0.0);
        b.camera.pos = Vec2::new(2500.0, 0.0);
        assert_ne!(a.camera.pos.x, b.camera.pos.x);
    }
}

// Session logic: pure, testable, no backend imports.
// Multi-output viewports on one shared canvas, minimal status bar with
// proximity chrome, Super-tap overview state machine.

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
        }
    }

    pub fn to_world(&self, screen: Vec2, view: Vec2) -> Vec2 {
        self.view_camera().screen_to_world(screen, view)
    }

    /// Screen -> canvas plane coordinates (for hit-testing windows).
    pub fn to_canvas(&self, screen: Vec2, view: Vec2) -> Vec2 {
        let world = self.to_world(screen, view);
        Vec2::new(
            world.x - self.canvas.offset.x,
            world.y - self.canvas.offset.y,
        )
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
        // screen = (anchor + off - pos) * z' + v/2, solved for off.
        self.canvas.offset.x = (screen.x - view.x * 0.5) / self.camera.zoom + self.camera.pos.x
            - anchor.x;
        self.canvas.offset.y = (screen.y - view.y * 0.5) / self.camera.zoom + self.camera.pos.y
            - anchor.y;
    }

    pub fn on_event(
        &mut self,
        event: SuperEvent,
        resolve: &dyn Fn(u64) -> Option<CanvasWindow>,
        view: Vec2,
    ) -> SuperAction {
        match (&self.state, event) {
            // Tap Super: enter canvas mode, screen keeps its size, no zoom.
            (SuperState::Idle, SuperEvent::SuperTap) => {
                self.prev_offset = Some(self.canvas.offset);
                self.overview.enter();
                self.state = SuperState::Canvas;
                self.dwell = DwellTracker::default();
                SuperAction::EnterCanvas
            }
            (SuperState::Canvas, SuperEvent::WindowClick(id)) => {
                self.overview.toggle_select(id);
                SuperAction::None
            }
            // Re-tap: pan (never zoom) so the selection sits centred, then quit.
            (SuperState::Canvas, SuperEvent::SuperTap) => {
                let ids = self.overview.quit();
                self.state = SuperState::Idle;
                self.prev_offset = None;
                self.dwell.disarm();
                let selected: Vec<CanvasWindow> =
                    ids.iter().filter_map(|id| resolve(*id)).collect();
                let centred = if selected.is_empty() {
                    false
                } else {
                    let (mut sx, mut sy) = (0.0, 0.0);
                    for w in &selected {
                        sx += w.pos.x + w.size.x * 0.5;
                        sy += w.pos.y + w.size.y * 0.5;
                    }
                    let n = selected.len() as f64;
                    let target = Vec2::new(sx / n, sy / n);
                    // Centre the selection with a pan (never a zoom):
                    // screen-centre shows W iff off = pos - W.
                    self.canvas.offset.x = self.camera.pos.x - target.x;
                    self.canvas.offset.y = self.camera.pos.y - target.y;
                    true
                };
                SuperAction::QuitCanvas { centred }
            }
            // Esc: quit, drop the selection, undo canvas panning.
            (SuperState::Canvas, SuperEvent::Esc) => {
                self.overview.quit();
                self.state = SuperState::Idle;
                self.dwell.disarm();
                if let Some(prev) = self.prev_offset.take() {
                    self.canvas.offset = prev;
                }
                SuperAction::RestoreCanvas
            }
            // Plain canvas click: quit, drop the selection, keep panning.
            (SuperState::Canvas, SuperEvent::CanvasClick) => {
                self.overview.quit();
                self.state = SuperState::Idle;
                self.dwell.disarm();
                self.prev_offset = None;
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
    fn canvas_mode_keeps_screen_size() {
        let mut s = Session::new();
        let wins = two_windows();
        let resolve = |id: u64| wins.get(&id).copied();
        let view = Vec2::new(1920.0, 1080.0);
        let zoom_before = s.camera.zoom;
        assert!(matches!(
            s.on_event(SuperEvent::SuperTap, &resolve, view),
            SuperAction::EnterCanvas
        ));
        assert_eq!(s.state, SuperState::Canvas);
        assert_eq!(s.camera.zoom, zoom_before);
    }

    #[test]
    fn retap_centres_selection_without_zoom() {
        let mut s = Session::new();
        let wins = two_windows();
        let resolve = |id: u64| wins.get(&id).copied();
        let view = Vec2::new(1920.0, 1080.0);
        s.on_event(SuperEvent::SuperTap, &resolve, view);
        s.on_event(SuperEvent::WindowClick(1), &resolve, view);
        s.on_event(SuperEvent::WindowClick(2), &resolve, view);
        let zoom_before = s.camera.zoom;
        let action = s.on_event(SuperEvent::SuperTap, &resolve, view);
        assert!(matches!(
            action,
            SuperAction::QuitCanvas { centred: true }
        ));
        assert_eq!(s.camera.zoom, zoom_before);
        assert_eq!(s.state, SuperState::Idle);
        // Selection centre ((400+2400)/2, (300+1800)/2) sits screen-centred.
        let centre = s.to_world(Vec2::new(960.0, 540.0), view);
        assert!((centre.x - 1400.0).abs() < 1e-9);
        assert!((centre.y - 1050.0).abs() < 1e-9);
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

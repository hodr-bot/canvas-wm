// Canvas core: infinite 2D plane, free-floating windows, game-feel camera.
// No zones (explicitly rejected). Multi-select keeps N windows in view via
// a temporary fit-view computed from the side they come from.

use std::collections::HashSet;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Vec2 {
    pub x: f64,
    pub y: f64,
}

impl Vec2 {
    pub fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }
}

impl std::ops::Sub for Vec2 {
    type Output = Vec2;

    fn sub(self, rhs: Vec2) -> Vec2 {
        Vec2::new(self.x - rhs.x, self.y - rhs.y)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Camera {
    pub pos: Vec2,
    pub zoom: f64,
}

impl Camera {
    pub fn new() -> Self {
        Self {
            pos: Vec2::new(0.0, 0.0),
            zoom: 1.0,
        }
    }

    /// World -> screen for a viewport of `view` logical pixels.
    pub fn world_to_screen(&self, world: Vec2, view: Vec2) -> Vec2 {
        Vec2::new(
            (world.x - self.pos.x) * self.zoom + view.x * 0.5,
            (world.y - self.pos.y) * self.zoom + view.y * 0.5,
        )
    }

    /// Screen -> world (for picking windows on the canvas).
    pub fn screen_to_world(&self, screen: Vec2, view: Vec2) -> Vec2 {
        Vec2::new(
            (screen.x - view.x * 0.5) / self.zoom + self.pos.x,
            (screen.y - view.y * 0.5) / self.zoom + self.pos.y,
        )
    }

    /// Zoom keeping the world point under `screen` fixed.
    pub fn zoom_at(&mut self, screen: Vec2, view: Vec2, factor: f64) {
        let anchor = self.screen_to_world(screen, view);
        self.zoom = (self.zoom * factor).clamp(0.05, 4.0);
        self.pos.x = anchor.x - (screen.x - view.x * 0.5) / self.zoom;
        self.pos.y = anchor.y - (screen.y - view.y * 0.5) / self.zoom;
    }

    /// Pan by a screen-space delta (drag direction).
    pub fn pan_by(&mut self, delta_screen: Vec2) {
        self.pos.x -= delta_screen.x / self.zoom;
        self.pos.y -= delta_screen.y / self.zoom;
    }
}

impl Default for Camera {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Copy)]
pub struct CanvasWindow {
    pub id: u64,
    pub pos: Vec2,
    pub size: Vec2,
}

impl CanvasWindow {
    pub fn new(id: u64, x: f64, y: f64, w: f64, h: f64) -> Self {
        Self {
            id,
            pos: Vec2::new(x, y),
            size: Vec2::new(w, h),
        }
    }

    pub fn center(&self) -> Vec2 {
        Vec2::new(
            self.pos.x + self.size.x * 0.5,
            self.pos.y + self.size.y * 0.5,
        )
    }

    pub fn contains(&self, world: Vec2) -> bool {
        world.x >= self.pos.x
            && world.x <= self.pos.x + self.size.x
            && world.y >= self.pos.y
            && world.y <= self.pos.y + self.size.y
    }
}

/// Overview state: Super tap zooms out slightly, left-click toggles selection,
/// re-Super quits and keeps the selection in view.
#[derive(Debug, Default)]
pub struct Overview {
    pub active: bool,
    pub selected: HashSet<u64>,
}

impl Overview {
    pub fn enter(&mut self) {
        self.active = true;
    }

    pub fn toggle_select(&mut self, id: u64) {
        if !self.selected.remove(&id) {
            self.selected.insert(id);
        }
    }

    pub fn quit(&mut self) -> Vec<u64> {
        self.active = false;
        let mut ids: Vec<u64> = self.selected.iter().copied().collect();
        ids.sort_unstable();
        ids
    }
}

/// Compute a temporary camera that fits all `windows` into `view` with
/// `padding` px margin, approaching from the current camera side.
/// Returns None when the selection is empty.
pub fn fit_view_for_selection(
    current: &Camera,
    windows: &[&CanvasWindow],
    view: Vec2,
    padding: f64,
) -> Option<Camera> {
    if windows.is_empty() {
        return None;
    }
    let (mut min_x, mut min_y) = (f64::INFINITY, f64::INFINITY);
    let (mut max_x, mut max_y) = (f64::NEG_INFINITY, f64::NEG_INFINITY);
    for w in windows {
        min_x = min_x.min(w.pos.x);
        min_y = min_y.min(w.pos.y);
        max_x = max_x.max(w.pos.x + w.size.x);
        max_y = max_y.max(w.pos.y + w.size.y);
    }
    let bbox_w = (max_x - min_x).max(1.0);
    let bbox_h = (max_y - min_y).max(1.0);
    let zoom = ((view.x - padding * 2.0) / bbox_w)
        .min((view.y - padding * 2.0) / bbox_h)
        .clamp(0.05, 2.0);
    // Keep the approach side: if we are left of the bbox, stay slightly left
    // so the camera travels from where it comes from (Stick Fight feel).
    let cx = (min_x + max_x) * 0.5;
    let cy = (min_y + max_y) * 0.5;
    let bias = 0.02;
    let approach_x = if current.pos.x < min_x {
        cx - bbox_w * bias
    } else if current.pos.x > max_x {
        cx + bbox_w * bias
    } else {
        cx
    };
    let approach_y = if current.pos.y < min_y {
        cy - bbox_h * bias
    } else if current.pos.y > max_y {
        cy + bbox_h * bias
    } else {
        cy
    };
    Some(Camera {
        pos: Vec2::new(approach_x, approach_y),
        zoom,
    })
}

/// Directional focus without wrap: S+D means bottom-right. `dir` does not need
/// to be normalised. Returns the id of the nearest window strictly in that
/// half-plane from `from`, or None.
pub fn focus_in_direction(from: Vec2, dir: Vec2, windows: &[CanvasWindow]) -> Option<u64> {
    let len = (dir.x * dir.x + dir.y * dir.y).sqrt();
    if len <= f64::EPSILON {
        return None;
    }
    let (dx, dy) = (dir.x / len, dir.y / len);
    let mut best: Option<(f64, u64)> = None;
    for w in windows {
        let c = w.center();
        let vx = c.x - from.x;
        let vy = c.y - from.y;
        let along = vx * dx + vy * dy;
        if along <= 0.0 {
            continue; // behind us or on top: no wrap, no self
        }
        let perp = (vx * dy - vy * dx).abs();
        // Score: prefer aligned (small perp) and close (small along).
        let score = perp * 2.0 + along;
        if best.map_or(true, |(s, _)| score < s) {
            best = Some((score, w.id));
        }
    }
    best.map(|(_, id)| id)
}

/// Screen rectangle (physical pixels) of a window through the camera.
/// `view` is the logical output size, `scale` the output scale factor.
pub fn window_screen_rect(
    cam: &Camera,
    win: &CanvasWindow,
    view: Vec2,
    scale: f64,
) -> (f64, f64, f64, f64) {
    let x = ((win.pos.x - cam.pos.x) * cam.zoom + view.x * 0.5) * scale;
    let y = ((win.pos.y - cam.pos.y) * cam.zoom + view.y * 0.5) * scale;
    let w = win.size.x * cam.zoom * scale;
    let h = win.size.y * cam.zoom * scale;
    (x, y, w, h)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn camera_roundtrip() {
        let cam = Camera {
            pos: Vec2::new(100.0, -50.0),
            zoom: 1.5,
        };
        let view = Vec2::new(1920.0, 1080.0);
        let world = Vec2::new(123.0, 456.0);
        let screen = cam.world_to_screen(world, view);
        let back = cam.screen_to_world(screen, view);
        assert!((back.x - world.x).abs() < 1e-6);
        assert!((back.y - world.y).abs() < 1e-6);
    }

    #[test]
    fn fit_view_keeps_selection_in_view() {
        let current = Camera::new();
        let a = CanvasWindow::new(1, 0.0, 0.0, 800.0, 600.0);
        let b = CanvasWindow::new(2, 2000.0, 1500.0, 800.0, 600.0);
        let view = Vec2::new(1920.0, 1080.0);
        let fit = fit_view_for_selection(&current, &[&a, &b], view, 80.0).unwrap();
        for w in [&a, &b] {
            for corner in [
                w.pos,
                Vec2::new(w.pos.x + w.size.x, w.pos.y),
                Vec2::new(w.pos.x, w.pos.y + w.size.y),
                Vec2::new(w.pos.x + w.size.x, w.pos.y + w.size.y),
            ] {
                let s = fit.world_to_screen(corner, view);
                assert!(s.x >= 70.0 && s.x <= view.x - 70.0, "x={} off view", s.x);
                assert!(s.y >= 70.0 && s.y <= view.y - 70.0, "y={} off view", s.y);
            }
        }
    }

    #[test]
    fn diagonal_focus_goes_bottom_right_no_wrap() {
        let from = Vec2::new(0.0, 0.0);
        let wins = vec![
            CanvasWindow::new(1, 500.0, 400.0, 400.0, 300.0),
            CanvasWindow::new(2, -600.0, -500.0, 400.0, 300.0),
        ];
        let hit = focus_in_direction(from, Vec2::new(1.0, 1.0), &wins);
        assert_eq!(hit, Some(1));
        let miss = focus_in_direction(from, Vec2::new(-1.0, -1.0), &[wins[0].clone()]);
        assert_eq!(miss, None);
    }

    #[test]
    fn screen_rect_matches_camera() {
        let cam = Camera::new();
        let view = Vec2::new(1280.0, 800.0);
        let win = CanvasWindow::new(1, 96.0, 96.0, 800.0, 600.0);
        let (x, y, w, h) = window_screen_rect(&cam, &win, view, 1.0);
        assert_eq!((x, y, w, h), (736.0, 496.0, 800.0, 600.0));
        // Zoomed out: everything shrinks toward the view center.
        let far = Camera {
            zoom: 0.5,
            ..Camera::new()
        };
        let (x2, _, w2, _) = window_screen_rect(&far, &win, view, 1.0);
        assert!(w2 < w && x2 > 500.0 && x2 < 736.0);
    }

    #[test]
    fn overview_select_and_quit() {
        let mut ov = Overview::default();
        ov.enter();
        ov.toggle_select(7);
        ov.toggle_select(9);
        ov.toggle_select(7);
        assert_eq!(ov.quit(), vec![9]);
        assert!(!ov.active);
    }
}

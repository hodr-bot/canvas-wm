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

impl Default for Vec2 {
    fn default() -> Self {
        Self::new(0.0, 0.0)
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
    /// Flight velocity (world units/s). Retargeting never zeroes it: a
    /// goal change mid-flight keeps momentum and the spring just pulls
    /// elsewhere — that is what makes redirections feel fluid instead
    /// of snapping.
    pub vel: Vec2,
    /// Where `pos` is flying to. Equals `pos` at rest.
    pub target: Vec2,
    /// Zoom glide velocity (zoom units/s). Same rule as `vel`: a goal
    /// change mid-glide keeps momentum, never snaps.
    pub zoom_vel: f64,
    /// Where `zoom` is gliding to. Equals `zoom` at rest.
    pub zoom_target: f64,
}

impl Camera {
    pub fn new() -> Self {
        Self {
            pos: Vec2::new(0.0, 0.0),
            zoom: 1.0,
            vel: Vec2::new(0.0, 0.0),
            target: Vec2::new(0.0, 0.0),
            zoom_vel: 0.0,
            zoom_target: 1.0,
        }
    }

    /// New goal, momentum kept (see `vel`).
    pub fn retarget(&mut self, target: Vec2) {
        self.target = target;
    }

    /// New zoom goal, glide momentum kept. Clamped to the same range
    /// as manual zoom so springs and fingers never disagree.
    pub fn zoom_to(&mut self, zoom: f64) {
        self.zoom_target = zoom.clamp(0.05, 4.0);
    }

    /// True while flying (position, zoom, or either velocity off rest).
    pub fn flying(&self) -> bool {
        self.pos != self.target
            || self.vel != Vec2::new(0.0, 0.0)
            || self.zoom != self.zoom_target
            || self.zoom_vel != 0.0
    }

    /// Advance the spring by `dt` seconds. Critically damped: fast,
    /// no overshoot. Snaps to exact rest inside epsilon so frames stop
    /// (the repaint budget ends with the motion). Returns true while
    /// still flying — the backend forces frames on true.
    pub fn step(&mut self, dt: f64) -> bool {
        if !self.flying() {
            return false;
        }
        // ~0.45s to cover 95% of the distance; damping is critical
        // (2*sqrt(stiffness)) so it never oscillates past the target.
        const STIFFNESS: f64 = 90.0;
        let damping = 2.0 * STIFFNESS.sqrt();
        // Clamp tab-switch spikes: physics stays stable, flight just
        // resumes next frame.
        let dt = dt.clamp(0.0, 0.05);
        let ax = STIFFNESS * (self.target.x - self.pos.x) - damping * self.vel.x;
        let ay = STIFFNESS * (self.target.y - self.pos.y) - damping * self.vel.y;
        self.vel.x += ax * dt;
        self.vel.y += ay * dt;
        self.pos.x += self.vel.x * dt;
        self.pos.y += self.vel.y * dt;
        let dx = self.target.x - self.pos.x;
        let dy = self.target.y - self.pos.y;
        let mut moving = true;
        if dx * dx + dy * dy < 0.25
            && self.vel.x * self.vel.x + self.vel.y * self.vel.y < 25.0
        {
            self.pos = self.target;
            self.vel = Vec2::new(0.0, 0.0);
            moving = false;
        }
        // Zoom glides on its own critically-damped spring, same feel as
        // position (~0.45s, no overshoot, exact snap so frames stop).
        let dz = self.zoom_target - self.zoom;
        let za = STIFFNESS * dz - damping * self.zoom_vel;
        self.zoom_vel += za * dt;
        self.zoom += self.zoom_vel * dt;
        let dz2 = self.zoom_target - self.zoom;
        if dz2 * dz2 < 1e-6 && self.zoom_vel * self.zoom_vel < 1e-4 {
            self.zoom = self.zoom_target;
            self.zoom_vel = 0.0;
        } else {
            moving = true;
        }
        moving
    }
}

/// How a canvas prop follows the camera. The key rule: windows are
/// always `Rigid` — they never subscribe to camera motion, so they
/// never fly around, at zero cost (no state, no per-frame code).
/// Decor and toys opt in per object with a gain.
#[derive(Debug, Clone, Copy)]
pub enum Follow {
    /// Nailed to the canvas (windows). Identity: no state, no step.
    Rigid,
    /// Stateless depth: 1 = canvas-rigid, 0 = screen-locked, between =
    /// slow drift (distant layers). Pure projection math, no
    /// integration, no settle — costs one multiply at draw time.
    /// Anchors are canvas coords; far roaming drifts decor offscreen
    /// (recycle later — extension point, not built here).
    Parallax(f64),
    /// Inverse inertia: a per-object trail chases the camera on a soft
    /// spring; the render point is anchor + (trail - camera) * gain, so
    /// the box swings against the flight, then settles exactly home.
    /// Different stiffness per box = varied wobble. Settles to sleep
    /// (no per-frame cost at rest).
    Laggy { gain: f64, stiff: f64 },
}

/// A lightweight canvas object: decor, toys, future 3D boxes. Windows
/// are NOT props (they stay rigid by construction, see `Follow`).
#[derive(Debug, Clone, Copy)]
pub struct Prop {
    /// Home on the canvas. Rigid and laggy objects always come back.
    pub anchor: Vec2,
    /// Trail point chasing the camera (Laggy only).
    pub trail: Vec2,
    pub trail_vel: Vec2,
    pub follow: Follow,
    /// Base size in canvas px (depth-scaled at draw).
    pub size: f64,
    /// Flat tint [r, g, b, a].
    pub shade: [f32; 4],
}

impl Prop {
    pub fn rigid(anchor: Vec2, size: f64, shade: [f32; 4]) -> Self {
        Self {
            anchor,
            trail: anchor,
            trail_vel: Vec2::new(0.0, 0.0),
            follow: Follow::Rigid,
            size,
            shade,
        }
    }

    pub fn parallax(anchor: Vec2, k: f64, size: f64, shade: [f32; 4]) -> Self {
        Self {
            anchor,
            trail: anchor,
            trail_vel: Vec2::new(0.0, 0.0),
            follow: Follow::Parallax(k.clamp(0.0, 1.0)),
            size,
            shade,
        }
    }

    pub fn laggy(
        anchor: Vec2,
        gain: f64,
        stiff: f64,
        cam: Vec2,
        size: f64,
        shade: [f32; 4],
    ) -> Self {
        Self {
            anchor,
            trail: cam,
            trail_vel: Vec2::new(0.0, 0.0),
            follow: Follow::Laggy { gain, stiff },
            size,
            shade,
        }
    }

    /// Advance one frame against the current camera position. Returns
    /// true while still moving (drives repaint demand). Rigid and
    /// parallax props never integrate: always false, always free.
    pub fn step(&mut self, dt: f64, cam: Vec2) -> bool {
        let stiff = match self.follow {
            Follow::Laggy { stiff, .. } => stiff,
            _ => return false,
        };
        let damp = 2.0 * stiff.sqrt();
        let dt = dt.clamp(0.0, 0.05);
        let ax = stiff * (cam.x - self.trail.x) - damp * self.trail_vel.x;
        let ay = stiff * (cam.y - self.trail.y) - damp * self.trail_vel.y;
        self.trail_vel.x += ax * dt;
        self.trail_vel.y += ay * dt;
        self.trail.x += self.trail_vel.x * dt;
        self.trail.y += self.trail_vel.y * dt;
        let dx = cam.x - self.trail.x;
        let dy = cam.y - self.trail.y;
        if dx * dx + dy * dy < 0.25
            && self.trail_vel.x * self.trail_vel.x + self.trail_vel.y * self.trail_vel.y < 25.0
        {
            self.trail = cam;
            self.trail_vel = Vec2::new(0.0, 0.0);
            false
        } else {
            true
        }
    }

    /// Canvas-space render point: anchor plus the lag offset
    /// (trail - camera) * gain. At rest the trail sits on the camera,
    /// so laggy objects render exactly on their anchor.
    pub fn point(&self, cam: Vec2) -> Vec2 {
        match self.follow {
            Follow::Laggy { gain, .. } => Vec2::new(
                self.anchor.x + (self.trail.x - cam.x) * gain,
                self.anchor.y + (self.trail.y - cam.y) * gain,
            ),
            _ => self.anchor,
        }
    }

    /// Screen rect (logical px) through `cam` for a `view` viewport.
    /// Parallax factor applies at projection: k = 1 renders like a
    /// window, k = 0 stays screen-locked. Size gets a mild depth cue.
    pub fn screen_rect(&self, cam: &Camera, view: Vec2) -> (f64, f64, f64, f64) {
        let k = match self.follow {
            Follow::Parallax(k) => k,
            _ => 1.0,
        };
        let p = self.point(cam.pos);
        let x = (p.x - cam.pos.x * k) * cam.zoom + view.x * 0.5;
        let y = (p.y - cam.pos.y * k) * cam.zoom + view.y * 0.5;
        let s = self.size * cam.zoom * (0.4 + 0.6 * k);
        (x - s * 0.5, y - s * 0.5, s, s)
    }
}

impl Camera {
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

    /// Ensure-selected: moving a window selects it without risking a
    /// toggle-off when it was already selected.
    pub fn select(&mut self, id: u64) {
        self.selected.insert(id);
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
        ..Camera::new()
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
            ..Camera::new()
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
    fn camera_flight_converges_and_stops() {
        let mut cam = Camera::new();
        assert!(!cam.flying());
        cam.retarget(Vec2::new(640.0, 400.0));
        assert!(cam.flying());
        // 2s at 120Hz: settled long before the end.
        let mut flying = true;
        for _ in 0..240 {
            flying = cam.step(1.0 / 120.0);
            if !flying {
                break;
            }
        }
        assert!(!flying);
        assert_eq!(cam.pos, Vec2::new(640.0, 400.0));
        assert_eq!(cam.vel, Vec2::new(0.0, 0.0));
        assert!(!cam.flying());
    }

    #[test]
    fn camera_retarget_keeps_momentum() {
        let mut cam = Camera::new();
        cam.retarget(Vec2::new(1000.0, 0.0));
        for _ in 0..30 {
            cam.step(1.0 / 120.0);
        }
        assert!(cam.flying());
        let vel = cam.vel;
        assert!(vel.x > 1.0, "should be moving by now");
        // New goal mid-flight: velocity untouched, goal updated.
        cam.retarget(Vec2::new(-500.0, 0.0));
        assert_eq!(cam.vel, vel);
        assert_eq!(cam.target, Vec2::new(-500.0, 0.0));
        // Still converges on the new goal.
        for _ in 0..600 {
            if !cam.step(1.0 / 120.0) {
                break;
            }
        }
        assert!(!cam.flying());
        assert_eq!(cam.pos, Vec2::new(-500.0, 0.0));
    }

    #[test]
    fn zoom_glide_converges_without_overshoot() {
        let mut cam = Camera::new();
        cam.zoom_to(0.9);
        assert!(cam.flying());
        for _ in 0..600 {
            cam.step(1.0 / 120.0);
            assert!(cam.zoom <= 1.0, "zoom overshoot past start");
            if !cam.flying() {
                break;
            }
        }
        assert_eq!(cam.zoom, 0.9);
        assert!(!cam.flying());
        // Mid-glide re-target keeps momentum (no snap back to rest).
        cam.zoom_to(1.0);
        cam.step(1.0 / 120.0);
        cam.zoom_to(0.9);
        assert!(cam.flying());
        for _ in 0..600 {
            if !cam.step(1.0 / 120.0) {
                break;
            }
        }
        assert_eq!(cam.zoom, 0.9);
    }

    #[test]
    fn camera_never_overshoots_critically_damped() {
        let mut cam = Camera::new();
        cam.retarget(Vec2::new(500.0, 0.0));
        for _ in 0..600 {
            cam.step(1.0 / 120.0);
            assert!(cam.pos.x <= 500.5, "overshoot at x={}", cam.pos.x);
            if !cam.flying() {
                break;
            }
        }
    }

    #[test]
    fn rigid_prop_is_identity_and_free() {
        let mut p = Prop::rigid(Vec2::new(100.0, 50.0), 40.0, [1.0, 1.0, 1.0, 1.0]);
        let cam = Vec2::new(999.0, -300.0);
        assert!(!p.step(0.016, cam));
        assert_eq!(p.point(cam), Vec2::new(100.0, 50.0));
        // Renders exactly like a window through the same camera.
        let view = Vec2::new(1280.0, 800.0);
        let c = Camera::new();
        let (x, y, _, _) = p.screen_rect(&c, view);
        let s = c.world_to_screen(Vec2::new(100.0, 50.0), view);
        assert!((x + 20.0 - s.x).abs() < 1e-9);
        assert!((y + 20.0 - s.y).abs() < 1e-9);
    }

    #[test]
    fn parallax_zero_is_screen_locked() {
        let view = Vec2::new(1280.0, 800.0);
        let p = Prop::parallax(Vec2::new(100.0, 50.0), 0.0, 40.0, [1.0, 1.0, 1.0, 1.0]);
        let mut c = Camera::new();
        let before = p.screen_rect(&c, view);
        c.pos = Vec2::new(2000.0, -1500.0);
        let after = p.screen_rect(&c, view);
        assert_eq!(before, after);
    }

    #[test]
    fn laggy_swings_against_flight_then_comes_home() {
        let anchor = Vec2::new(100.0, 50.0);
        let cam0 = Vec2::new(0.0, 0.0);
        let mut p = Prop::laggy(anchor, 1.0, 30.0, cam0, 40.0, [1.0, 1.0, 1.0, 1.0]);
        assert_eq!(p.point(cam0), anchor);
        // Teleport the camera: the trail lags, so the render point
        // offsets against the motion...
        let cam1 = Vec2::new(400.0, 0.0);
        assert!(p.step(1.0 / 120.0, cam1));
        let displaced = p.point(cam1);
        assert!(
            displaced.x < anchor.x,
            "should swing against +x flight, got {}",
            displaced.x
        );
        // ...then the trail catches up and the box lands exactly home.
        for _ in 0..600 {
            if !p.step(1.0 / 120.0, cam1) {
                break;
            }
        }
        assert_eq!(p.point(cam1), anchor);
        assert!(!p.step(1.0 / 120.0, cam1));
    }

    #[test]
    fn laggy_gain_scales_the_swing() {
        let anchor = Vec2::new(0.0, 0.0);
        let cam0 = Vec2::new(0.0, 0.0);
        let cam1 = Vec2::new(400.0, 0.0);
        let mut soft = Prop::laggy(anchor, 0.25, 30.0, cam0, 40.0, [1.0, 1.0, 1.0, 1.0]);
        let mut hard = Prop::laggy(anchor, 1.0, 30.0, cam0, 40.0, [1.0, 1.0, 1.0, 1.0]);
        soft.step(1.0 / 120.0, cam1);
        hard.step(1.0 / 120.0, cam1);
        let ds = (soft.point(cam1).x - anchor.x).abs();
        let dh = (hard.point(cam1).x - anchor.x).abs();
        assert!(dh > ds * 3.0, "gain should scale swing: {} vs {}", dh, ds);
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

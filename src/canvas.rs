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
        // Zoom glides on its own SOFTER spring: a slow fade (~0.9s),
        // not the snappy position feel — the zoom value visibly melts
        // while the tracked anchor (see Session::track_zoom_anchor)
        // keeps the content glued under the cursor every frame.
        const ZOOM_STIFFNESS: f64 = 26.0;
        let zoom_damping = 2.0 * ZOOM_STIFFNESS.sqrt();
        let dz = self.zoom_target - self.zoom;
        let za = ZOOM_STIFFNESS * dz - zoom_damping * self.zoom_vel;
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
    /// Includes the gentle fisheye (see [`fisheye_gain`]): identity at
    /// zoom 1, focal breathing off-centre otherwise. The centre pixel
    /// never moves.
    pub fn world_to_screen(&self, world: Vec2, view: Vec2) -> Vec2 {
        let flat = Self::flat_world_to_screen(world, self.pos, self.zoom, view);
        let gain = fisheye_gain(self.zoom);
        if gain <= 0.0 {
            return flat;
        }
        apply_fisheye_gain(flat, view_centre(view), view_half_diag(view), gain)
    }

    /// Screen -> world (for picking windows on the canvas). Exact
    /// inverse of [`world_to_screen`](Self::world_to_screen), so clicks
    /// land where the warped content is drawn.
    pub fn screen_to_world(&self, screen: Vec2, view: Vec2) -> Vec2 {
        let gain = fisheye_gain(self.zoom);
        let s = if gain <= 0.0 {
            screen
        } else {
            unapply_fisheye(screen, view_centre(view), view_half_diag(view), gain)
        };
        Self::flat_screen_to_world(s, self.pos, self.zoom, view)
    }

    /// Pre-warp a world point so the flat GL mapping draws it exactly
    /// where [`world_to_screen`](Self::world_to_screen) says: the
    /// backend warps canvas quad corners through this before handing
    /// them to `canvas_mvp`. Identity at zoom 1.
    pub fn warp_world(&self, world: Vec2, view: Vec2) -> Vec2 {
        let gain = fisheye_gain(self.zoom);
        if gain <= 0.0 {
            return world;
        }
        let flat = Self::flat_world_to_screen(world, self.pos, self.zoom, view);
        let w = apply_fisheye_gain(flat, view_centre(view), view_half_diag(view), gain);
        Self::flat_screen_to_world(w, self.pos, self.zoom, view)
    }

    fn flat_world_to_screen(world: Vec2, pos: Vec2, zoom: f64, view: Vec2) -> Vec2 {
        Vec2::new(
            (world.x - pos.x) * zoom + view.x * 0.5,
            (world.y - pos.y) * zoom + view.y * 0.5,
        )
    }

    fn flat_screen_to_world(screen: Vec2, pos: Vec2, zoom: f64, view: Vec2) -> Vec2 {
        Vec2::new(
            (screen.x - view.x * 0.5) / zoom + pos.x,
            (screen.y - view.y * 0.5) / zoom + pos.y,
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
    // The gentle fisheye pushes off-centre content outward (up to ~8%
    // at the corners when zoomed), so a flat fit can overflow the
    // padding. Shrink until the WARPED bbox corners honour it.
    let probe = |zx: f64, px: f64, py: f64| {
        let mut c = Camera::new();
        c.pos = Vec2::new(px, py);
        c.zoom = zx;
        [
            Vec2::new(min_x, min_y),
            Vec2::new(max_x, min_y),
            Vec2::new(min_x, max_y),
            Vec2::new(max_x, max_y),
        ]
        .into_iter()
        .map(|corner| c.world_to_screen(corner, view))
        .all(|s| {
            s.x >= padding
                && s.x <= view.x - padding
                && s.y >= padding
                && s.y <= view.y - padding
        })
    };
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
    // Shrink the zoom until the warped corners honour the padding
    // (flat fit is the first probe; each step loses 3%).
    let mut fit_zoom = zoom;
    for _ in 0..12 {
        if probe(fit_zoom, approach_x, approach_y) {
            break;
        }
        fit_zoom = (fit_zoom * 0.97).clamp(0.05, 2.0);
    }
    Some(Camera {
        pos: Vec2::new(approach_x, approach_y),
        zoom: fit_zoom,
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

/// Canvas offset that puts `target` at the view centre given camera
/// position `cam_pos`: camera pos minus target. Pure helper.
pub fn offset_to_centre(cam_pos: Vec2, target: Vec2) -> Vec2 {
    cam_pos - target
}

/// Gentle half-zoom half-fisheye radial gain.
///
/// `dist_px` is the distance from the view centre, `half_diag_px` the
/// half-diagonal of the view (i.e. the centre-to-corner distance).
/// Returns a multiplicative gain: 1 at the centre, rising smoothly to
/// ~1.08 at the corners, capped beyond. The smoothstep profile has
/// zero slope at the centre (no kink, no nausea) and zero slope at
/// the cap (no hard clamp edge). Pure function.
pub fn fisheye_factor(dist_px: f64, half_diag_px: f64) -> f64 {
    if !(dist_px > 0.0) || !(half_diag_px > 0.0) {
        return 1.0;
    }
    let t = (dist_px / half_diag_px).clamp(0.0, 1.0);
    // Smoothstep: t*t*(3-2t), monotonic on [0,1], 0 at 0, 1 at 1.
    let s = t * t * (3.0 - 2.0 * t);
    1.0 + 0.08 * s
}

/// Apply the gentle fisheye to a screen point: radial expansion about
/// `centre` by [`fisheye_factor`]. Identity at the centre, smooth and
/// monotonic outward, capped near 8% at the corners. Pure function.
pub fn apply_fisheye(screen: Vec2, centre: Vec2, half_diag: f64) -> Vec2 {
    apply_fisheye_gain(screen, centre, half_diag, 1.0)
}

/// Gain-scaled fisheye: `gain` 0..=1 scales the (factor − 1) term, so
/// gain 0 is exactly identity and gain 1 is [`apply_fisheye`]. The
/// zoom drives the gain (see [`fisheye_gain`]): half the zoom feel
/// stays real scale, up to half comes from this focal breathing.
/// Pure function.
pub fn apply_fisheye_gain(
    screen: Vec2,
    centre: Vec2,
    half_diag: f64,
    gain: f64,
) -> Vec2 {
    let ox = screen.x - centre.x;
    let oy = screen.y - centre.y;
    if ox == 0.0 && oy == 0.0 {
        return centre;
    }
    if !(half_diag > 0.0) {
        return screen;
    }
    let g = gain.clamp(0.0, 1.0);
    if g <= 0.0 {
        return screen;
    }
    let dist = (ox * ox + oy * oy).sqrt();
    let f = 1.0 + (fisheye_factor(dist, half_diag) - 1.0) * g;
    Vec2::new(centre.x + ox * f, centre.y + oy * f)
}

/// Fisheye gain from the camera zoom: 0 at zoom 1 (flat, back-compat),
/// ramping with |zoom − 1|, capped at 1. Zoomed-in and zoomed-out both
/// breathe; the centre never moves. Pure function.
pub fn fisheye_gain(zoom: f64) -> f64 {
    if !zoom.is_finite() {
        return 0.0;
    }
    ((zoom - 1.0).abs() * 0.75).clamp(0.0, 1.0)
}

/// View centre and half-diagonal (centre-to-corner) in screen px.
pub fn view_centre(view: Vec2) -> Vec2 {
    Vec2::new(view.x * 0.5, view.y * 0.5)
}

/// View centre and half-diagonal (centre-to-corner) in screen px.
pub fn view_half_diag(view: Vec2) -> f64 {
    (view.x * view.x + view.y * view.y).sqrt() * 0.5
}

/// Inverse of [`apply_fisheye_gain`]: the radial map r ↦ r·f(r) is
/// strictly increasing (f rises smoothly from 1), so bisection on the
/// radius converges. Identity at the centre. Pure function.
pub fn unapply_fisheye(
    screen: Vec2,
    centre: Vec2,
    half_diag: f64,
    gain: f64,
) -> Vec2 {
    let ox = screen.x - centre.x;
    let oy = screen.y - centre.y;
    if ox == 0.0 && oy == 0.0 {
        return centre;
    }
    if !(half_diag > 0.0) {
        return screen;
    }
    let g = gain.clamp(0.0, 1.0);
    if g <= 0.0 {
        return screen;
    }
    let target = (ox * ox + oy * oy).sqrt();
    // Forward map is monotonic with f >= 1: the pre-image radius never
    // exceeds the image radius, so [0, target] brackets the root.
    let mut lo = 0.0;
    let mut hi = target;
    for _ in 0..48 {
        let mid = (lo + hi) * 0.5;
        let f = 1.0 + (fisheye_factor(mid, half_diag) - 1.0) * g;
        if mid * f < target {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    let r = (lo + hi) * 0.5;
    let k = if target > 0.0 { r / target } else { 0.0 };
    Vec2::new(centre.x + ox * k, centre.y + oy * k)
}

/// Void dust: faint motes pinned to the canvas plane so camera motion
/// reads on screen even with no window in view (panning the void
/// otherwise shows nothing moving). Deterministic hash grid over
/// canvas cells — infinite, stateless, zero per-frame cost beyond
/// drawing. Two depths (rigid + half parallax) for a hint of depth.
/// Fades out when zoomed far (overview), full at working zoom.
/// `motion` is camera speed in canvas px/s; below 60 the void stays
/// clean (no dust at rest). Sizes are constant screen px per layer
/// (3.2 / 2.4) with small per-dot hash variation, never zoom-scaled.
/// Returns screen-space (x, y, size, alpha) in logical px.
pub fn dust_dots(cam: &Camera, view: Vec2, motion: f64) -> Vec<(f64, f64, f64, f32)> {
    if motion < 60.0 {
        return Vec::new();
    }
    const CELL: f64 = 320.0;
    /// Cap cells per axis so zoomed-out views stay cheap: stride the
    /// grid instead (dots shrink with zoom anyway, sparser is fine).
    const MAX_SPAN: i64 = 24;
    const DOTS_PER_CELL: usize = 2;
    let fade = ((cam.zoom - 0.15) / 0.35).clamp(0.0, 1.0) as f32;
    if fade <= 0.0 {
        return Vec::new();
    }
    let mut out = Vec::new();
    // (layer, parallax k, base alpha, base size): rigid reads 1:1,
    // half-parallax drifts behind for depth.
    for (layer, k, base_a, base_s) in
        [(0i64, 1.0f64, 0.30f32, 3.2f64), (1i64, 0.5, 0.15, 2.4)]
    {
        // Canvas range visible for this layer, padded by one cell.
        let x0 = cam.pos.x * k - view.x / cam.zoom - CELL;
        let x1 = cam.pos.x * k + view.x / cam.zoom + CELL;
        let y0 = cam.pos.y * k - view.y / cam.zoom - CELL;
        let y1 = cam.pos.y * k + view.y / cam.zoom + CELL;
        let (cx0, cx1) = ((x0 / CELL).floor() as i64, (x1 / CELL).floor() as i64);
        let (cy0, cy1) = ((y0 / CELL).floor() as i64, (y1 / CELL).floor() as i64);
        let mut stride = 1i64;
        while (cx1 - cx0) / stride > MAX_SPAN || (cy1 - cy0) / stride > MAX_SPAN {
            stride *= 2;
        }
        for cx in cx0..=cx1 {
            if (cx - cx0) % stride != 0 {
                continue;
            }
            for cy in cy0..=cy1 {
                if (cy - cy0) % stride != 0 {
                    continue;
                }
                for d in 0..DOTS_PER_CELL {
                    let h = dust_hash(cx, cy, layer * 31 + d as i64);
                    let fx = (h as f64 / u64::MAX as f64).fract();
                    let fy = (((h >> 21) & 0x1FFFFF) as f64 / 0x1FFFFF as f64).fract();
                    let fv = (((h >> 42) & 0x3FF) as f64 / 0x3FF as f64) as f32;
                    let ox = (cx as f64 + fx) * CELL;
                    let oy = (cy as f64 + fy) * CELL;
                    let sx = (ox - cam.pos.x * k) * cam.zoom + view.x * 0.5;
                    let sy = (oy - cam.pos.y * k) * cam.zoom + view.y * 0.5;
                    // Ride the same focal breathing as the canvas so the
                    // motes stay glued to the plane they dress.
                    let gain = fisheye_gain(cam.zoom);
                    let (sx, sy) = if gain <= 0.0 || !sx.is_finite() || !sy.is_finite() {
                        (sx, sy)
                    } else {
                        let w = apply_fisheye_gain(
                            Vec2::new(sx, sy),
                            view_centre(view),
                            view_half_diag(view),
                            gain,
                        );
                        (w.x, w.y)
                    };
                    let s = base_s * (0.8 + 0.4 * f64::from(fv));
                    let a = base_a * fade * (0.6 + 0.4 * fv);
                    out.push((sx, sy, s, a));
                }
            }
        }
    }
    out
}

/// Stateless cell hash (splitmix64 over mixed coords): the grid never
/// shifts, so dots sit still in the canvas while the camera moves.
fn dust_hash(cx: i64, cy: i64, salt: i64) -> u64 {
    let mut z = (cx as u64)
        .wrapping_mul(0x9E3779B97F4A7C15)
        .wrapping_add((cy as u64).wrapping_mul(0xBF58476D1CE4E5B9))
        .wrapping_add((salt as u64).wrapping_mul(0x94D049BB133111EB))
        .wrapping_add(0x9E3779B97F4A7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
    z ^ (z >> 31)
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

    #[test]
    fn dust_is_deterministic_and_culled() {
        let cam = Camera::new();
        let view = Vec2::new(1920.0, 1080.0);
        let a = dust_dots(&cam, view, 500.0);
        let b = dust_dots(&cam, view, 500.0);
        assert!(dust_dots(&cam, view, 0.0).is_empty());
        assert_eq!(a.len(), b.len());
        assert!(a.len() > 50, "void should read: {}", a.len());
        assert!(a.len() < 800, "bounded draw cost: {}", a.len());
        for (i, (x, y, s, al)) in a.iter().enumerate() {
            assert_eq!((*x, *y, *s, *al), b[i]);
            // Within the covered canvas range around the view centre
            // (cell quantisation pads up to two cells per side).
            assert!(
                (x - view.x / 2.0).abs() <= view.x + 640.0 + s,
                "x={}",
                x
            );
            assert!(
                (y - view.y / 2.0).abs() <= view.y + 640.0 + s,
                "y={}",
                y
            );
            assert!(*s >= 1.5 && *s <= 5.0, "s={}", s);
            assert!(*al > 0.0 && *al <= 0.32, "a={}", al);
        }
    }

    #[test]
    fn dust_moves_with_camera_and_fades_far() {
        let view = Vec2::new(1920.0, 1080.0);
        let here = dust_dots(&Camera::new(), view, 500.0);
        let mut moved = Camera::new();
        moved.pos = Vec2::new(100.0, 0.0);
        let there = dust_dots(&moved, view, 500.0);
        // Rigid layer shifts exactly 100px left on screen (coverage
        // edges may add/drop cells — match on the overlap, not len).
        let shifted = here
            .iter()
            .filter(|(x, y, _, _)| {
                there.iter().any(|(x2, y2, _, _)| {
                    (x2 - (x - 100.0)).abs() < 1e-6 && (y2 - y).abs() < 1e-6
                })
            })
            .count();
        assert!(shifted > 0, "rigid dots must track the camera 1:1");
        // Far overview: invisible (no clutter, no cost).
        let far = Camera {
            zoom: 0.1,
            ..Camera::new()
        };
        assert!(dust_dots(&far, view, 500.0).is_empty());
    }

    #[test]
    fn offset_to_centre_is_cam_minus_target() {
        let cam = Vec2::new(100.0, -50.0);
        let target = Vec2::new(30.0, 20.0);
        assert_eq!(offset_to_centre(cam, target), Vec2::new(70.0, -70.0));
        assert_eq!(
            offset_to_centre(Vec2::new(0.0, 0.0), Vec2::new(0.0, 0.0)),
            Vec2::new(0.0, 0.0)
        );
    }

    #[test]
    fn fisheye_centre_is_identity() {
        let centre = Vec2::new(960.0, 540.0);
        let half_diag = (960.0f64 * 960.0 + 540.0 * 540.0).sqrt();
        assert_eq!(fisheye_factor(0.0, half_diag), 1.0);
        assert_eq!(apply_fisheye(centre, centre, half_diag), centre);
        // Zero / degenerate inputs stay identity (no NaN, no shift).
        assert_eq!(fisheye_factor(0.0, 0.0), 1.0);
        assert_eq!(fisheye_factor(-5.0, half_diag), 1.0);
        assert_eq!(apply_fisheye(centre, centre, 0.0), centre);
    }

    #[test]
    fn fisheye_corner_capped_near_8_percent() {
        let half_diag = 1100.0;
        let corner = fisheye_factor(half_diag, half_diag);
        assert!(
            (corner - 1.08).abs() < 1e-9,
            "corner gain should be ~1.08, got {}",
            corner
        );
        // Beyond the corner: capped, never exceeds ~1.08.
        assert_eq!(fisheye_factor(half_diag * 2.0, half_diag), corner);
        assert!(fisheye_factor(half_diag * 5.0, half_diag) <= 1.08 + 1e-9);
        // apply_fisheye at the corner matches the capped gain.
        let centre = Vec2::new(0.0, 0.0);
        let screen = Vec2::new(half_diag, 0.0);
        let out = apply_fisheye(screen, centre, half_diag);
        assert!((out.x - half_diag * 1.08).abs() < 1e-6);
        assert!((out.y - 0.0).abs() < 1e-9);
    }

    #[test]
    fn fisheye_monotonic_outward() {
        let half_diag = 1100.0;
        let mut prev = fisheye_factor(0.0, half_diag);
        for i in 1..=20 {
            let d = half_diag * i as f64 / 20.0;
            let f = fisheye_factor(d, half_diag);
            assert!(f >= prev, "gain must not decrease at d={}", d);
            assert!(f >= 1.0 && f <= 1.08 + 1e-9);
            prev = f;
        }
        // apply_fisheye pushes outward smoothly along a ray: mapped
        // distance from centre grows monotonically with input distance.
        let centre = Vec2::new(960.0, 540.0);
        let mut prev_d = 0.0;
        for i in 1..=20 {
            let d = half_diag * i as f64 / 20.0;
            let p = Vec2::new(centre.x + d, centre.y);
            let q = apply_fisheye(p, centre, half_diag);
            let mapped = q.x - centre.x;
            assert!(mapped > prev_d, "mapped dist must grow at d={}", d);
            prev_d = mapped;
            // Same side, no flip: gentle, no nausea.
            assert!(q.x > centre.x);
            assert!((q.y - centre.y).abs() < 1e-9);
        }
    }

    #[test]
    fn fisheye_gain_zero_at_unit_zoom_capped() {
        assert_eq!(fisheye_gain(1.0), 0.0);
        assert_eq!(fisheye_gain(f64::NAN), 0.0);
        assert_eq!(fisheye_gain(f64::INFINITY), 0.0);
        // Ramps symmetrically, capped at 1.
        assert!((fisheye_gain(2.0) - 0.75).abs() < 1e-9);
        assert!((fisheye_gain(0.0) - 0.75).abs() < 1e-9);
        assert_eq!(fisheye_gain(4.0), 1.0);
        assert_eq!(fisheye_gain(-2.0), 1.0);
    }

    #[test]
    fn fisheye_gain_scales_warp() {
        let centre = Vec2::new(960.0, 540.0);
        let half_diag = 1100.0;
        let p = Vec2::new(centre.x + 800.0, centre.y);
        // Gain 0 is exactly identity; gain 1 matches apply_fisheye.
        assert_eq!(apply_fisheye_gain(p, centre, half_diag, 0.0), p);
        assert_eq!(
            apply_fisheye_gain(p, centre, half_diag, 1.0),
            apply_fisheye(p, centre, half_diag)
        );
        // Half gain displaces half as far.
        let full = apply_fisheye_gain(p, centre, half_diag, 1.0).x - p.x;
        let half = apply_fisheye_gain(p, centre, half_diag, 0.5).x - p.x;
        assert!((half * 2.0 - full).abs() < 1e-9);
        // Out-of-range gains clamp.
        assert_eq!(
            apply_fisheye_gain(p, centre, half_diag, 3.0),
            apply_fisheye(p, centre, half_diag)
        );
        assert_eq!(apply_fisheye_gain(p, centre, half_diag, -2.0), p);
    }

    #[test]
    fn fisheye_inverse_roundtrips() {
        let view = Vec2::new(1920.0, 1080.0);
        let centre = view_centre(view);
        let half_diag = view_half_diag(view);
        for gain in [0.25, 0.5, 1.0] {
            for &(dx, dy) in &[
                (0.0, 0.0),
                (100.0, -50.0),
                (900.0, 500.0),
                (-960.0, 540.0),
                (300.0, 300.0),
            ] {
                let p = Vec2::new(centre.x + dx, centre.y + dy);
                let fwd = apply_fisheye_gain(p, centre, half_diag, gain);
                let back = unapply_fisheye(fwd, centre, half_diag, gain);
                assert!(
                    (back.x - p.x).abs() < 1e-6 && (back.y - p.y).abs() < 1e-6,
                    "roundtrip failed at {:?} gain {}: got {:?}",
                    p,
                    gain,
                    back
                );
            }
        }
        // Degenerate inputs stay identity.
        let p = Vec2::new(100.0, 100.0);
        assert_eq!(unapply_fisheye(p, centre, 0.0, 1.0), p);
        assert_eq!(unapply_fisheye(p, centre, half_diag, 0.0), p);
    }

    #[test]
    fn camera_mappings_flat_at_unit_zoom() {
        let mut cam = Camera::new();
        cam.pos = Vec2::new(10.0, -20.0);
        let view = Vec2::new(1920.0, 1080.0);
        for w in [
            Vec2::new(0.0, 0.0),
            Vec2::new(500.0, 300.0),
            Vec2::new(-200.0, 700.0),
        ] {
            let s = cam.world_to_screen(w, view);
            assert_eq!(
                s,
                Vec2::new(
                    (w.x - 10.0) + 960.0,
                    (w.y + 20.0) + 540.0
                )
            );
            // Roundtrip exact, pre-warp identity.
            assert_eq!(cam.screen_to_world(s, view), w);
            assert_eq!(cam.warp_world(w, view), w);
        }
    }

    #[test]
    fn camera_warp_draw_pick_consistent() {
        let mut cam = Camera::new();
        cam.zoom = 2.0;
        let view = Vec2::new(1920.0, 1080.0);
        // Centre never moves, even zoomed.
        let centre_world = cam.screen_to_world(view_centre(view), view);
        assert_eq!(cam.world_to_screen(centre_world, view), view_centre(view));
        for w in [
            Vec2::new(100.0, 50.0),
            Vec2::new(-400.0, 200.0),
            Vec2::new(300.0, -250.0),
        ] {
            // Picking roundtrips: click where drawn lands back on w.
            let s = cam.world_to_screen(w, view);
            let back = cam.screen_to_world(s, view);
            assert!(
                (back.x - w.x).abs() < 1e-6 && (back.y - w.y).abs() < 1e-6,
                "pick roundtrip failed for {:?}: got {:?}",
                w,
                back
            );
            // warp_world is draw-consistent: flat-mapping the warped
            // point lands exactly on the warped screen point.
            let warped = cam.warp_world(w, view);
            let drawn = Vec2::new(
                (warped.x - cam.pos.x) * cam.zoom + view.x * 0.5,
                (warped.y - cam.pos.y) * cam.zoom + view.y * 0.5,
            );
            assert!(
                (drawn.x - s.x).abs() < 1e-9 && (drawn.y - s.y).abs() < 1e-9,
                "draw/pick diverge for {:?}: drawn {:?} vs picked {:?}",
                w,
                drawn,
                s
            );
            // Warp pushes outward, never inward, and stays small.
            let flat = Vec2::new(
                (w.x - cam.pos.x) * cam.zoom + view.x * 0.5,
                (w.y - cam.pos.y) * cam.zoom + view.y * 0.5,
            );
            let df = ((flat.x - 960.0).powi(2) + (flat.y - 540.0).powi(2)).sqrt();
            let ds = ((s.x - 960.0).powi(2) + (s.y - 540.0).powi(2)).sqrt();
            assert!(ds >= df - 1e-9, "warp must not pull inward");
            assert!(
                ds <= df * 1.08 + 1e-6,
                "warp exceeds the 8% corner cap"
            );
        }
    }
}

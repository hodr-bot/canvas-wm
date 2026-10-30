//! Toy 3D cubes (the K key, Canvas mode).
//!
//! Pure physics + orientation: no GL, no Wayland, fully unit-tested.
//! The backend spawns one cube at the cursor per K press, steps the
//! pile every frame, projects the corners through the same perspective
//! MVP as the windows and draws shaded faces back-to-front.
//!
//! Model: a cube lives on the canvas plane (world units = canvas px,
//! `z` = bottom height above the plane). Gravity pulls it down, the
//! plane (or a window top) bounces it, ground friction + rolling
//! resistance calm it. The moving canvas drags resting cubes along
//! with partial grip — pan fast and stop hard and the cube keeps
//! sliding, exactly like a box on a yanked tablecloth. Windows are
//! static hitboxes: low cubes bounce off their sides, high cubes land
//! on top of them. Cubes never collide with each other.

use crate::canvas::Vec2;

/// Hard cap: the oldest cube leaves when a new one spawns past this.
/// 200 cubes ≈ 1400 tiny tris/frame worst case — nothing for a real
/// GPU, fine even for llvmpipe. Anti-spam is the fresh-press guard;
/// this only bounds memory and visual chaos.
pub const MAX_CUBES: usize = 200;
/// How tall windows stand (canvas px): a shallow relief so cubes
/// visibly sit ON windows instead of floating over a flat print.
pub const WALL_H: f64 = 28.0;
/// Gravity, canvas px / s^2.
const GRAV: f64 = 2600.0;
/// Vertical bounce kept on landing.
const BOUNCE: f64 = 0.45;
/// |vz| below this on touchdown = rest instead of bounce.
const REST_VZ: f64 = 130.0;
/// Canvas-drag grip: a resting cube relaxes toward this fraction of the
/// *counter* camera motion (the canvas slides under the box).
const GRIP: f64 = 0.4;
/// How fast the cube couples to the canvas motion (/s).
const DRAG_RATE: f64 = 2.5;
/// Rolling resistance on contact (/s exponential).
const ROLL_FRICTION: f64 = 3.0;
/// Below this planar speed a contacting cube sleeps.
const SLEEP: f64 = 6.0;

/// Wood tones (linear-ish 0..1 RGB): every cube reads as a small
/// wooden box, tint varies per cube so the pile stays readable.
const PALETTE: [[f64; 3]; 6] = [
    [0.72, 0.50, 0.28], // oak
    [0.62, 0.42, 0.24], // walnut
    [0.80, 0.60, 0.36], // pine
    [0.55, 0.36, 0.20], // dark oak
    [0.68, 0.46, 0.26], // chestnut
    [0.75, 0.55, 0.32], // beech
];

/// One toy cube. `z` = bottom height above the canvas plane; `rot`
/// columns are the body axes in world space.
pub struct Cube {
    pub pos: Vec2,
    pub z: f64,
    pub vel: Vec2,
    pub vz: f64,
    pub rot: [[f64; 3]; 3],
    pub ang: [f64; 3],
    pub half: f64,
    pub rgb: [f64; 3],
}

fn xorshift(mut s: u64) -> impl FnMut() -> f64 {
    move || {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        (s as f64) / (u64::MAX as f64)
    }
}

/// Spawn a cube over a canvas point: starts high with a random tumble
/// so the K press reads as a throw, not a placement.
pub fn spawn(x: f64, y: f64, seed: u64) -> Cube {
    let mut rnd = xorshift(seed.wrapping_add(0x9e37_79b9_7f4a_7c15));
    let unit = rnd();
    let yaw = rnd() * std::f64::consts::TAU;
    let (sy, cy) = yaw.sin_cos();
    Cube {
        pos: Vec2::new(x, y),
        z: 420.0 + 260.0 * rnd(),
        vel: Vec2::new((rnd() - 0.5) * 100.0, (rnd() - 0.5) * 100.0),
        vz: 0.0,
        // Yawed about the vertical; the tumble rates below tip it over.
        rot: [[cy, sy, 0.0], [-sy, cy, 0.0], [0.0, 0.0, 1.0]],
        ang: [
            (rnd() - 0.5) * 3.0,
            (rnd() - 0.5) * 3.0,
            (rnd() - 0.5) * 2.5 + 1.2 * if unit > 0.5 { 1.0 } else { -1.0 },
        ],
        half: 22.0 + 10.0 * rnd(),
        rgb: PALETTE[(unit * PALETTE.len() as f64) as usize % PALETTE.len()],
    }
}

/// Push a cube, evicting the oldest past [`MAX_CUBES`].
pub fn add_cube(pile: &mut Vec<Cube>, cube: Cube) {
    pile.push(cube);
    while pile.len() > MAX_CUBES {
        pile.remove(0);
    }
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn orthonormalize(m: &mut [[f64; 3]; 3]) {
    // Gram-Schmidt over the columns, in place.
    for i in 0..3 {
        let mut v = m[i];
        for j in 0..i {
            let d = dot(v, m[j]);
            v = [v[0] - d * m[j][0], v[1] - d * m[j][1], v[2] - d * m[j][2]];
        }
        let n = dot(v, v).sqrt().max(1e-9);
        m[i] = [v[0] / n, v[1] / n, v[2] / n];
    }
}

/// Nearest axis-aligned orientation (columns snapped to dominant axes,
/// right-handed: third column rebuilt from the first two).
fn snapped(rot: [[f64; 3]; 3]) -> [[f64; 3]; 3] {
    let mut out = [[0.0; 3]; 3];
    for (i, col) in rot.iter().enumerate().take(2) {
        let mut best = 0;
        let mut best_v = 0.0f64;
        for (a, v) in col.iter().enumerate() {
            if v.abs() > best_v {
                best_v = v.abs();
                best = a;
            }
        }
        out[i][best] = if col[best] >= 0.0 { 1.0 } else { -1.0 };
    }
    // Third = first x second (keeps the frame right-handed).
    let (a, b) = (out[0], out[1]);
    out[2] = [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ];
    out
}

/// Support height under a canvas point: window tops stand tall, the
/// bare canvas is zero. `walls` are (x, y, w, h) canvas rects. Shared
/// with the backend for blob shadows.
pub(crate) fn ground_height(
    x: f64,
    y: f64,
    walls: &[(f64, f64, f64, f64)],
) -> f64 {
    for (wx, wy, w, h) in walls {
        if x >= *wx && x <= wx + w && y >= *wy && y <= wy + h {
            return WALL_H;
        }
    }
    0.0
}

impl Cube {
    /// Ground height under the footprint: the max of centre + 4 edge
    /// samples, so a cube straddling a window edge lands on the top
    /// instead of clipping halfway through the side.
    fn ground(&self, walls: &[(f64, f64, f64, f64)]) -> f64 {
        let r = self.half * 0.7;
        let mut g = ground_height(self.pos.x, self.pos.y, walls);
        for (dx, dy) in [(-r, -r), (r, -r), (-r, r), (r, r)] {
            let h = ground_height(self.pos.x + dx, self.pos.y + dy, walls);
            if h > g {
                g = h;
            }
        }
        g
    }

    /// Step physics by `dt` seconds. `cam_vel` is the camera world
    /// velocity (canvas px/s) — its motion drags resting cubes.
    /// Returns true while anything still moves (the frame must repaint).
    pub fn step(&mut self, dt: f64, cam_vel: Vec2, walls: &[(f64, f64, f64, f64)]) -> bool {
        let dt = dt.min(0.05);
        let ground = self.ground(walls);

        // --- vertical: gravity, bounce, rest ---
        self.vz -= GRAV * dt;
        self.z += self.vz * dt;
        if self.z <= ground {
            self.z = ground;
            if self.vz < -REST_VZ {
                self.vz = -self.vz * BOUNCE;
                // Vertical impact eats slide + spin: no more ice-skating
                // after the bounce, the cube tips instead of gliding.
                self.vel.x *= 0.78;
                self.vel.y *= 0.78;
                for w in self.ang.iter_mut() {
                    *w *= 0.4;
                }
            } else {
                self.vz = 0.0;
            }
        }
        let contact = self.z <= ground + 0.5 && self.vz == 0.0;

        // --- planar: the sliding canvas drags the cube (partial grip),
        // rolling resistance bleeds motion on contact ---
        let k = (DRAG_RATE * dt).min(1.0);
        self.vel.x += (-cam_vel.x * GRIP - self.vel.x) * k;
        self.vel.y += (-cam_vel.y * GRIP - self.vel.y) * k;
        if contact {
            let f = (-ROLL_FRICTION * dt).exp();
            self.vel.x *= f;
            self.vel.y *= f;
        }
        self.pos.x += self.vel.x * dt;
        self.pos.y += self.vel.y * dt;

        // --- window sides: low cubes bounce off, never tunnel ---
        if self.z < WALL_H - 1.0 {
            let r = self.half * 0.85;
            for (x, y, w, h) in walls {
                let cx = self.pos.x.clamp(*x, x + w);
                let cy = self.pos.y.clamp(*y, y + h);
                let mut nx = self.pos.x - cx;
                let mut ny = self.pos.y - cy;
                let mut d = (nx * nx + ny * ny).sqrt();
                if d < r {
                    if d < 1e-6 {
                        // Centre inside the rect: eject along the
                        // smallest penetration axis.
                        let dxs = [self.pos.x - x, x + w - self.pos.x];
                        let dys = [self.pos.y - y, y + h - self.pos.y];
                        let m = dxs[0].min(dxs[1]).min(dys[0]).min(dys[1]);
                        if m == dxs[0] {
                            nx = -1.0;
                            ny = 0.0;
                        } else if m == dxs[1] {
                            nx = 1.0;
                            ny = 0.0;
                        } else if m == dys[0] {
                            nx = 0.0;
                            ny = -1.0;
                        } else {
                            nx = 0.0;
                            ny = 1.0;
                        }
                        d = 0.0;
                    } else {
                        nx /= d;
                        ny /= d;
                    }
                    self.pos.x += nx * (r - d);
                    self.pos.y += ny * (r - d);
                    let vn = self.vel.x * nx + self.vel.y * ny;
                    if vn < 0.0 {
                        self.vel.x -= 1.55 * vn * nx;
                        self.vel.y -= 1.55 * vn * ny;
                        // Impact spin: tangential speed rolls the cube.
                        let vt = self.vel.x * -ny + self.vel.y * nx;
                        self.ang[2] += vt / self.half * 0.3;
                    }
                }
            }
        }

        // --- spin: free tumble airborne, rolling on contact, snap rest ---
        let speed = (self.vel.x * self.vel.x + self.vel.y * self.vel.y).sqrt();
        if contact && speed > 1.0 {
            // Rolling without slipping: w x r = -v, r = (0,0,-half).
            let rate = speed / self.half;
            let tx = -self.vel.y / speed * rate;
            let ty = self.vel.x / speed * rate;
            let rk = (6.0 * dt).min(1.0);
            self.ang[0] += (tx - self.ang[0]) * rk;
            self.ang[1] += (ty - self.ang[1]) * rk;
            self.ang[2] *= (-1.5 * dt).exp();
        } else if !contact {
            let damp = (-0.4 * dt).exp();
            for w in self.ang.iter_mut() {
                *w *= damp;
            }
        } else {
            for w in self.ang.iter_mut() {
                *w *= (-8.0 * dt).exp();
            }
            // Settle flat: ease toward the nearest axis alignment, slowly
            // enough to read as the box tipping over, never as a twist.
            // Only when the spin is nearly dead, or the snap fights the
            // last roll and the cube pirouettes on the spot.
            let ang_speed2 =
                self.ang[0] * self.ang[0] + self.ang[1] * self.ang[1] + self.ang[2] * self.ang[2];
            if ang_speed2 < 2.25 {
                let target = snapped(self.rot);
                let sk = (3.0 * dt).min(1.0);
                for i in 0..3 {
                    for j in 0..3 {
                        self.rot[i][j] += (target[i][j] - self.rot[i][j]) * sk;
                    }
                }
            }
        }
        // Integrate orientation: R += skew(w) * R * dt.
        let (wx, wy, wz) = (self.ang[0], self.ang[1], self.ang[2]);
        for c in self.rot.iter_mut() {
            // w x c
            *c = [
                c[0] + (wy * c[2] - wz * c[1]) * dt,
                c[1] + (wz * c[0] - wx * c[2]) * dt,
                c[2] + (wx * c[1] - wy * c[0]) * dt,
            ];
        }
        orthonormalize(&mut self.rot);

        // --- sleep ---
        if contact && speed < SLEEP {
            let cam = (cam_vel.x * cam_vel.x + cam_vel.y * cam_vel.y).sqrt();
            if cam < 8.0 {
                self.vel = Vec2::new(0.0, 0.0);
            }
        }

        let ang_speed =
            (self.ang[0] * self.ang[0] + self.ang[1] * self.ang[1] + self.ang[2] * self.ang[2])
                .sqrt();
        !contact || speed > 0.5 || ang_speed > 0.05 || self.vz != 0.0
    }

    /// The 8 corners in world space (x, y canvas; z height).
    pub fn corners(&self) -> [[f64; 3]; 8] {
        let cx = self.pos.x;
        let cy = self.pos.y;
        let cz = self.z + self.half;
        let h = self.half;
        let mut out = [[0.0; 3]; 8];
        for (i, s) in [-1.0, 1.0]
            .iter()
            .flat_map(|sx| [-1.0, 1.0].iter().map(move |sy| (*sx, *sy)))
            .flat_map(|(sx, sy)| [-1.0, 1.0].iter().map(move |sz| (sx, sy, *sz)))
            .enumerate()
        {
            let (sx, sy, sz) = s;
            // corner = centre + sx*col0 + sy*col1 + sz*col2; rot[j] is
            // body axis j in world, so world_x mixes column x-parts.
            out[i] = [
                cx + (self.rot[0][0] * sx + self.rot[1][0] * sy + self.rot[2][0] * sz) * h,
                cy + (self.rot[0][1] * sx + self.rot[1][1] * sy + self.rot[2][1] * sz) * h,
                cz + (self.rot[0][2] * sx + self.rot[1][2] * sy + self.rot[2][2] * sz) * h,
            ];
        }
        out
    }
}

/// Overhead lamp swinging just off-screen, above the top edge, at
/// canvas height. Shadows fall away from it (down-screen); the camera
/// shoves it like a chandelier: pan right and the lamp lags left.
/// Pure state — stepped by the backend, unit-tested here.
pub struct Lamp {
    /// Pendulum angle (rad). 0 = centred above the view.
    pub theta: f64,
    /// Angular velocity (rad/s).
    pub omega: f64,
}

impl Lamp {
    pub fn new() -> Self {
        Lamp {
            theta: 0.0,
            omega: 0.0,
        }
    }

    /// Step by `dt`: gentle self-sway so it lives in idle, pendulum
    /// spring + damping, minus a push from the camera x velocity.
    pub fn step(&mut self, dt: f64, cam_vel_x: f64, t: f64) {
        const DRIVE: f64 = 0.35; // self-sway torque (rad/s^2)
        const SWAY_W: f64 = 0.5; // sway frequency (rad/s, ~12.6 s period)
        const STIFF: f64 = 2.2; // pendulum (g/L)
        const DAMP: f64 = 0.6; // damping (/s)
        const PUSH: f64 = 0.0011; // camera coupling
        let acc = DRIVE * (t * SWAY_W).sin() - STIFF * self.theta.sin()
            - DAMP * self.omega
            - PUSH * cam_vel_x;
        self.omega += acc * dt;
        self.theta += self.omega * dt;
        // Keep the lamp above the screen: clamp the swing.
        if self.theta > 1.2 {
            self.theta = 1.2;
            self.omega = 0.0;
        }
        if self.theta < -1.2 {
            self.theta = -1.2;
            self.omega = 0.0;
        }
    }

    /// World position of the source: above the top screen edge, swung
    /// sideways by the pendulum. `mid` = world view centre, `half_w` /
    /// `half_h` = world half-extents of the view.
    pub fn source(&self, mid: Vec2, half_w: f64, half_h: f64) -> Vec2 {
        Vec2::new(
            mid.x + self.theta.sin() * half_w * 0.9,
            mid.y - half_h * 1.15,
        )
    }

    /// Shadow direction at a world point: from the source through the
    /// point (unit 2D vector, pointing down-screen while the lamp is
    /// above).
    pub fn shadow_dir(&self, at: Vec2, src: Vec2) -> Vec2 {
        let dx = at.x - src.x;
        let dy = at.y - src.y;
        let len = (dx * dx + dy * dy).sqrt().max(1e-6);
        Vec2::new(dx / len, dy / len)
    }
}
/// Cube faces: (body axis, 4 corner indices). Corner order from
/// [`Cube::corners`]: sz fastest, then sy, then sx — i = sz_bit +
/// sy_bit*2 + sx_bit*4. -X face: {0,1,2,3}; +X: {4,5,6,7};
/// -Y: {0,1,4,5}; +Y: {2,3,6,7}; -Z: {0,2,4,6}; +Z: {1,3,5,7}.
/// No face culling is used, so winding is irrelevant — only the corner
/// sets matter.
pub const FACES: [([i32; 3], [usize; 4]); 6] = [
    ([-1, 0, 0], [0, 1, 3, 2]),
    ([1, 0, 0], [4, 5, 7, 6]),
    ([0, -1, 0], [0, 1, 5, 4]),
    ([0, 1, 0], [2, 3, 7, 6]),
    ([0, 0, -1], [0, 2, 6, 4]),
    ([0, 0, 1], [1, 3, 7, 5]),
];

/// World-space outward normal of a face given the cube orientation.
pub fn face_normal(rot: [[f64; 3]; 3], axis: [i32; 3]) -> [f64; 3] {
    let mut n = [0.0; 3];
    for (i, a) in axis.iter().enumerate() {
        if *a != 0 {
            let s = *a as f64;
            n = [
                n[0] + rot[i][0] * s,
                n[1] + rot[i][1] * s,
                n[2] + rot[i][2] * s,
            ];
        }
    }
    n
}

/// Procedural wood grain multiplier for a point (u, v) on a face in
/// body units (-half..half). Rings along u with a slow wobble in v:
/// cheap, stable per cube (seeded by the caller), no texture needed.
pub fn wood_grain(u: f64, v: f64, seed: f64) -> f64 {
    let rings = (u * 0.35 + seed + (v * 0.12).sin() * 1.7).sin();
    let fine = (u * 1.7 - v * 0.6 + seed * 2.3).sin();
    1.0 + rings * 0.055 + fine * 0.025
}

#[cfg(test)]
mod tests {
    use super::*;

    fn still() -> Vec2 {
        Vec2::new(0.0, 0.0)
    }

    #[test]
    fn falls_and_rests_on_canvas() {
        let mut c = spawn(0.0, 0.0, 1);
        c.vel = Vec2::new(0.0, 0.0);
        c.ang = [0.0, 0.0, 0.0];
        let mut moving = true;
        for _ in 0..600 {
            moving = c.step(1.0 / 60.0, still(), &[]);
        }
        assert!(!moving, "cube must sleep, z={} vz={}", c.z, c.vz);
        assert!((c.z - 0.0).abs() < 1e-6);
    }

    #[test]
    fn hard_drop_bounces() {
        let mut c = spawn(0.0, 0.0, 2);
        c.z = 10.0;
        c.vz = -800.0;
        c.step(1.0 / 60.0, still(), &[]);
        assert!(c.vz > 0.0, "must bounce upward, vz={}", c.vz);
    }

    #[test]
    fn lands_on_window_top() {
        let mut c = spawn(50.0, 50.0, 3);
        c.vel = Vec2::new(0.0, 0.0);
        c.ang = [0.0, 0.0, 0.0];
        let walls = [(0.0, 0.0, 200.0, 200.0)];
        for _ in 0..600 {
            c.step(1.0 / 60.0, still(), &walls);
        }
        assert!((c.z - WALL_H).abs() < 1.0, "rests on window, z={}", c.z);
    }

    #[test]
    fn low_cube_bounces_off_window_side() {
        let mut c = spawn(-100.0, 50.0, 4);
        c.z = 0.0;
        c.vz = 0.0;
        c.vel = Vec2::new(400.0, 0.0);
        c.ang = [0.0, 0.0, 0.0];
        let walls = [(0.0, 0.0, 200.0, 200.0)];
        for _ in 0..120 {
            c.step(1.0 / 60.0, still(), &walls);
        }
        assert!(
            c.pos.x < 0.0,
            "must stay left of the wall, x={}",
            c.pos.x
        );
        assert!(c.vel.x < 50.0, "must rebound, vx={}", c.vel.x);
    }

    #[test]
    fn camera_pan_drags_resting_cube() {
        let mut c = spawn(0.0, 0.0, 5);
        c.z = 0.0;
        c.vz = 0.0;
        c.vel = Vec2::new(0.0, 0.0);
        // Canvas slides +x under the cube: it must drift -x.
        for _ in 0..60 {
            c.step(1.0 / 60.0, Vec2::new(300.0, 0.0), &[]);
        }
        assert!(c.vel.x < -20.0, "dragged along, vx={}", c.vel.x);
        // Camera stops: friction must calm it again.
        let mut moving = true;
        for _ in 0..600 {
            moving = c.step(1.0 / 60.0, still(), &[]);
        }
        assert!(!moving, "must sleep after the stop");
    }

    #[test]
    fn rolling_couples_spin_to_slide() {
        let mut c = spawn(0.0, 0.0, 6);
        c.z = 0.0;
        c.vz = 0.0;
        c.vel = Vec2::new(200.0, 0.0);
        c.ang = [0.0, 0.0, 0.0];
        for _ in 0..30 {
            c.step(1.0 / 60.0, still(), &[]);
        }
        // Rolling +x tips about +y: ang[1] > 0.
        assert!(c.ang[1] > 0.5, "must roll, ang={:?}", c.ang);
    }

    #[test]
    fn orientation_stays_orthonormal() {
        let mut c = spawn(0.0, 0.0, 7);
        for _ in 0..600 {
            c.step(1.0 / 60.0, Vec2::new(120.0, -60.0), &[(0.0, 0.0, 300.0, 300.0)]);
        }
        for i in 0..3 {
            let n = dot(c.rot[i], c.rot[i]).sqrt();
            assert!((n - 1.0).abs() < 1e-6, "column {} norm {}", i, n);
        }
        assert!(dot(c.rot[0], c.rot[1]).abs() < 1e-6);
    }

    #[test]
    fn cap_evicts_oldest() {
        let mut pile = Vec::new();
        for i in 0..(MAX_CUBES + 5) {
            add_cube(&mut pile, spawn(i as f64, 0.0, i as u64));
        }
        assert_eq!(pile.len(), MAX_CUBES);
        assert_eq!(pile[0].pos.x, 5.0);
    }

    #[test]
    fn corners_follow_columns_yaw_90() {
        // Regression: corners() used rows instead of columns (rendered
        // R^T while normals used R), so faces culled wrong and cubes
        // looked see-through + spun oddly. Yaw 90 distinguishes them.
        let mut c = spawn(0.0, 0.0, 9);
        c.rot = [[0.0, 1.0, 0.0], [-1.0, 0.0, 0.0], [0.0, 0.0, 1.0]];
        c.half = 10.0;
        c.pos = crate::canvas::Vec2::new(0.0, 0.0);
        c.z = 0.0;
        let cs = c.corners();
        // sx=+1, sy=-1, sz=-1 -> index 4; columns give ([1, 1, -1]) * half.
        assert!((cs[4][0] - 10.0).abs() < 1e-9, "x {:?}", cs[4]);
        assert!((cs[4][1] - 10.0).abs() < 1e-9, "y {:?}", cs[4]);
        assert!((cs[4][2] - 0.0).abs() < 1e-9, "z {:?}", cs[4]);
    }

    #[test]
    fn faces_match_corner_coordinates() {
        let mut c = spawn(0.0, 0.0, 9);
        c.rot = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
        let cs = c.corners();
        let centre = [c.pos.x, c.pos.y, c.z + c.half];
        for (axis, quad) in FACES.iter() {
            let ax = axis.iter().position(|a| *a != 0).unwrap();
            let expect = centre[ax] + axis[ax] as f64 * c.half;
            for qi in quad.iter() {
                assert!(
                    (cs[*qi][ax] - expect).abs() < 1e-9,
                    "face {:?} corner {} coord {}",
                    axis,
                    qi,
                    cs[*qi][ax]
                );
            }
        }
    }

    #[test]
    fn wood_grain_stays_near_one() {
        for i in 0..20 {
            let g = wood_grain(i as f64 * 0.7, i as f64 * 1.3, 2.0);
            assert!((g - 1.0).abs() < 0.12, "grain {} = {}", i, g);
        }
    }

    #[test]
    fn lamp_sways_bounded_without_camera() {
        let mut lamp = Lamp::new();
        for i in 0..1200 {
            lamp.step(1.0 / 60.0, 0.0, i as f64 / 60.0);
        }
        assert!(
            lamp.theta.abs() < 0.6,
            "self-sway stays bounded, theta={}",
            lamp.theta
        );
        // It must actually move, not sit at zero.
        let mut lamp = Lamp::new();
        let mut peak: f64 = 0.0;
        for i in 0..800 {
            lamp.step(1.0 / 60.0, 0.0, i as f64 / 60.0);
            peak = peak.max(lamp.theta.abs());
        }
        assert!(peak > 0.05, "lamp visibly sways, peak={}", peak);
    }

    #[test]
    fn lamp_lags_against_camera_push() {
        let mut lamp = Lamp::new();
        // Camera rushing +x: the chandelier lags -x (negative theta).
        for i in 0..120 {
            lamp.step(1.0 / 60.0, 2000.0, i as f64 / 60.0);
        }
        assert!(lamp.theta < -0.1, "pushed left, theta={}", lamp.theta);
    }

    #[test]
    fn lamp_source_stays_above_screen() {
        let mid = Vec2::new(100.0, 200.0);
        let lamp = Lamp::new();
        let s = lamp.source(mid, 400.0, 300.0);
        assert!(s.y < 200.0 - 300.0, "off-screen above, y={}", s.y);
        assert!((s.x - 100.0).abs() < 1e-9, "centred at rest");
    }

    #[test]
    fn lamp_shadow_falls_down_screen() {
        let lamp = Lamp::new();
        let src = Vec2::new(0.0, -500.0);
        let d = lamp.shadow_dir(Vec2::new(60.0, 100.0), src);
        assert!(d.y > 0.7, "mostly downward, dy={}", d.y);
        let len = (d.x * d.x + d.y * d.y).sqrt();
        assert!((len - 1.0).abs() < 1e-9, "unit vector");
    }

    #[test]
    fn corners_centered_on_cube() {
        let c = spawn(10.0, 20.0, 8);
        let cs = c.corners();
        let mut mean = [0.0; 3];
        for p in cs.iter() {
            for j in 0..3 {
                mean[j] += p[j] / 8.0;
            }
        }
        assert!((mean[0] - 10.0).abs() < 1e-9);
        assert!((mean[1] - 20.0).abs() < 1e-9);
        assert!((mean[2] - (c.z + c.half)).abs() < 1e-9);
    }
}

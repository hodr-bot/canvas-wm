//! Glass marbles (the K key, Canvas mode).
//!
//! A tiny physics engine, nothing else: balls with mass on a table,
//! shaken boxes, solid window blocks. No GL, no Wayland, fully
//! unit-tested. The backend spawns marbles, steps the pile every
//! frame with the canvas shake, and draws glass spheres as
//! screen-space discs (no new shader: the solid program paints
//! layered fans/quads).
//!
//! Model (canvas units = canvas px, `z` = bottom height above the
//! surface):
//! - Each marble is a glass ball: mass ~ r^3 (light — a real marble
//!   weighs grams), gravity pulls it down, the table answers.
//! - The canvas is a slightly lively table: it returns a SMALL hop
//!   (glass taps, it doesn't rebound like a superball) and eats
//!   sideways slide on every touch, plus Coulomb friction and
//!   rolling resistance while in contact — so runaway balls always
//!   calm down instead of rolling to infinity.
//! - Windows are SOLID DEEP BOXES: their top stands a few mm above
//!   the canvas (WIN_TOP), their sides run from far below the canvas
//!   up to that top. Nothing passes under, nothing tunnels through
//!   (substepped integration). A ball rolling into one bonks and
//!   rolls along it; a ball dropped on one rests on top, then rolls
//!   off the edge and taps down.
//! - The box is shaken, not dragged: the canvas acceleration acts as
//!   a pseudo-force on every ball (the table moves under them).
//!   Static friction holds slow balls glued to the box (calm pans
//!   ride along); past the friction cone they break loose, lag,
//!   clack into each other and settle somewhere new — shake it twice
//!   and nothing is where it was, like a real box in the hand.
//! - Every landing and every hard clack spends a whisper of
//!   per-marble randomness sideways, so no two shakes ever match.
//! - The inner motif (cat's eye, swirl, ribbon — fresh every spawn)
//!   advects with the roll so the surface visibly flows.

use crate::canvas::Vec2;

/// Hard cap: the oldest marble leaves when a new one spawns past this.
pub const MAX_MARBLES: usize = 200;
/// Window top above the canvas (canvas px): a few mm. Balls rest up
/// here when dropped on a window, bonk into the side when rolling.
pub const WIN_TOP: f64 = 12.0;
/// Simulation detail by distance to the viewport (canvas px).
#[derive(PartialEq, Eq, Debug, Clone, Copy)]
pub enum Lod {
    /// Within 2x the screen span: every frame.
    Full,
    /// Between 2x and 10x: every 4th frame (4x dt, same motion).
    Reduced,
    /// Past 10x: dropped — rolled too far to matter.
    Cull,
}

/// LOD tier for a marble `dist` canvas px outside the viewport, given
/// the viewport `span` (its larger side, canvas px).
pub fn lod_for(dist: f64, span: f64) -> Lod {
    if dist <= 2.0 * span {
        Lod::Full
    } else if dist <= 10.0 * span {
        Lod::Reduced
    } else {
        Lod::Cull
    }
}
/// Gravity, canvas px / s^2.
const GRAV: f64 = 2600.0;
/// Table restitution: the canvas returns a small glass tap.
const E_FLOOR: f64 = 0.25;
/// |vz| below this on touchdown = rest instead of bounce.
const REST_VZ: f64 = 90.0;
/// Sideways kept through a bounce: the table eats some slide.
const BOUNCE_KEEP: f64 = 0.82;
/// Sideways kick on landing, fraction of impact speed: every bounce
/// scatters a little, never straight up-down.
const KICK_K: f64 = 0.06;
/// Static friction coefficient (glued to the box below MU_S * GRAV of
/// shake); kinetic while sliding. Loose on purpose: a small shove
/// breaks the balls free and they waltz — a yanked box slides from
/// under them instead of dragging them along.
const MU_S: f64 = 0.10;
const MU_K: f64 = 0.05;
/// Rolling resistance on contact (/s exponential): gentle, the waltz
/// lasts seconds and dies soft.
const ROLL: f64 = 0.30;
/// Window-wall restitution: wooden-box bonk.
const E_WALL: f64 = 0.45;
/// Window-wall tangential kept through a bonk.
const WALL_KEEP: f64 = 0.90;
/// Marble-marble restitution: glass clacks, it doesn't bounce far.
const E_MARBLE: f64 = 0.50;
/// Glass density: mass = RHO * r^3 — grams, not tonnes. Only ratios
/// matter (heavy shoves light), but small numbers keep the solver
/// calm.
const RHO: f64 = 0.002;
/// Planar speed below which a calm ball sleeps.
const SLEEP_V: f64 = 8.0;
/// Max travel per substep, fraction of r: fast balls substep so they
/// can never tunnel through a window wall.
const SUBSTEP_FACE: f64 = 0.35;
const SUBSTEP_MAX: usize = 10;
/// Motif drift range (units of r): wraps, so the design flows forever.
const FLOW_MAX: f64 = 0.35;
/// Shake conditioning: raw view velocity is a finite difference over a
/// jittery frame clock (software GL swings 30..100 ms), so its raw
/// derivative explodes on every hitch — a smooth pan would randomly
/// read as a yank. Two exponential smoothers (velocity, then
/// acceleration) plus a deadband: hand tremor and frame noise die,
/// a real shove passes through with its shape intact.
const SHAKE_VEL_TAU: f64 = 0.045;
const SHAKE_ACC_TAU: f64 = 0.055;
/// Filtered shake below this reads as zero (hand tremor, quanta).
/// Low on purpose: even a small intentional move must reach the
/// balls — the double EMA already ate the clock jitter. High
/// enough that a stray 1px event quantum at idle reads zero.
const SHAKE_DEAD: f64 = 100.0;
/// Full strength past this; linear ramp between the two.
const SHAKE_FULL: f64 = 300.0;

/// Glass tints (linear-ish 0..1 RGB): jewel tones, one per marble.
const TINTS: [[f64; 3]; 6] = [
    [0.45, 0.80, 0.85], // aqua
    [0.30, 0.45, 0.90], // cobalt
    [0.35, 0.80, 0.50], // emerald
    [0.90, 0.65, 0.30], // amber
    [0.60, 0.45, 0.90], // violet
    [0.90, 0.35, 0.40], // cherry
];

/// Ribbon glass colours (opaque swirls in clear glass, photo-style).
const INKS: [[f64; 3]; 7] = [
    [0.10, 0.25, 0.85], // cobalt
    [0.95, 0.25, 0.08], // red-orange
    [1.00, 0.85, 0.10], // yellow
    [0.05, 0.65, 0.35], // emerald
    [0.95, 0.96, 0.97], // milk white
    [0.15, 0.05, 0.10], // oxblood
    [0.10, 0.70, 0.75], // teal
];

/// One glass marble. `z` = bottom height above the surface.
pub struct Marble {
    pub pos: Vec2,
    pub vel: Vec2,
    pub z: f64,
    pub vz: f64,
    pub r: f64,
    /// Small mass ~ r^3: heavy shoves light in a clack.
    pub mass: f64,
    pub tint: [f64; 3],
    /// 0 = cat's eye, 1 = swirl, 2 = ribbon.
    pub pattern: u8,
    pub seed: f64,
    /// Inner-motif drift in units of r: advects with the roll, wraps.
    pub flow: Vec2,
    /// Interior orientation (Euler XYZ, rad, unbounded — never wraps,
    /// so the 3D ribbons tumble with the roll without pops).
    pub rot: [f64; 3],
    /// Ribbon glass colours (opaque swirls in clear glass).
    pub ink1: [f64; 3],
    pub ink2: [f64; 3],
    /// Per-marble RNG (xorshift state): bounce kicks and clack
    /// scatter draw here, so no two marbles ever answer alike.
    rng: u64,
}

impl Marble {
    fn rand(&mut self) -> f64 {
        // Advance the per-marble xorshift: every kick and scatter
        // draw mutates the state, so the pile never repeats itself.
        let mut s = self.rng | 1;
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        self.rng = s;
        (s as f64) / (u64::MAX as f64)
    }
}

fn xorshift(mut s: u64) -> impl FnMut() -> f64 {
    move || {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        (s as f64) / (u64::MAX as f64)
    }
}

/// Spawn a marble over a canvas point: a real sky-drop, not a plop
/// — it falls from ~170-300px, taps, and scatters, landing on the
/// cursor point with a fresh inner motif and its own mass.
pub fn spawn(x: f64, y: f64, seed: u64) -> Marble {
    let mut rnd = xorshift(seed.wrapping_add(0x9e37_79b9_7f4a_7c15));
    let unit = rnd();
    let r = 13.0 + 9.0 * rnd();
    Marble {
        pos: Vec2::new(x, y),
        z: 170.0 + 130.0 * rnd(),
        vel: Vec2::new((rnd() - 0.5) * 280.0, (rnd() - 0.5) * 280.0),
        vz: 0.0,
        r,
        mass: RHO * r * r * r,
        tint: TINTS[(unit * TINTS.len() as f64) as usize % TINTS.len()],
        pattern: (rnd() * 3.0) as u8 % 3,
        seed: unit * 100.0,
        flow: Vec2::new(0.0, 0.0),
        rot: [
            rnd() * std::f64::consts::TAU,
            rnd() * std::f64::consts::TAU,
            rnd() * std::f64::consts::TAU,
        ],
        ink1: INKS[(unit * INKS.len() as f64) as usize % INKS.len()],
        ink2: if (unit * 7.0) as usize % 3 == 0 {
            INKS[4] // milk-white accent, a classic
        } else {
            INKS[(unit * INKS.len() as f64 + 3.0) as usize % INKS.len()]
        },
        rng: seed
            .wrapping_mul(0x9E37_79B9_7F4A_7C15)
            .wrapping_add(0x1234_5678_9ABC_DEF0)
            | 1,
    }
}

/// Push a marble, evicting the oldest past [`MAX_MARBLES`].
pub fn add_marble(pile: &mut Vec<Marble>, marble: Marble) {
    pile.push(marble);
    while pile.len() > MAX_MARBLES {
        pile.remove(0);
    }
}

/// Table height at a canvas point: window tops stand WIN_TOP tall,
/// everywhere else is the bare canvas. Windows are felt, never seen.
fn support_height(x: f64, y: f64, walls: &[(f64, f64, f64, f64)]) -> f64 {
    for (wx, wy, w, h) in walls {
        if x >= *wx && x <= wx + w && y >= *wy && y <= wy + h {
            return WIN_TOP;
        }
    }
    0.0
}

fn wrap_flow(v: f64) -> f64 {
    let w = 2.0 * FLOW_MAX;
    let mut u = (v + FLOW_MAX) % w;
    if u < 0.0 {
        u += w;
    }
    u - FLOW_MAX
}

/// Input conditioner for the engine: turns the raw per-frame view
/// velocity (finite difference over a jittery clock) into a clean
/// shake acceleration. Feed it every frame, even at rest — the
/// smoothers need the quiet to settle.
///
/// Pipeline: EMA the velocity (kills frame-time quanta), derive,
/// EMA the acceleration (kills single-frame spikes), deadband the
/// tremor. A real shove survives with its shape; noise reads zero.
pub struct Shake {
    vel: Vec2,
    prev: Vec2,
    acc: Vec2,
    live: bool,
}

impl Shake {
    pub fn new() -> Self {
        Shake {
            vel: Vec2::new(0.0, 0.0),
            prev: Vec2::new(0.0, 0.0),
            acc: Vec2::new(0.0, 0.0),
            live: false,
        }
    }

    pub fn push(&mut self, dt: f64, raw_vel: Vec2) -> Vec2 {
        let dt = dt.clamp(1e-4, 0.1);
        if !self.live {
            self.vel = raw_vel;
            self.prev = raw_vel;
            self.live = true;
            return Vec2::new(0.0, 0.0);
        }
        let av = 1.0 - (-dt / SHAKE_VEL_TAU).exp();
        self.prev = self.vel;
        self.vel.x += (raw_vel.x - self.vel.x) * av;
        self.vel.y += (raw_vel.y - self.vel.y) * av;
        // Instantaneous accel from the smoother's own motion, then a
        // second EMA to kill single-frame spikes.
        let inst = Vec2::new(
            (self.vel.x - self.prev.x) / dt,
            (self.vel.y - self.prev.y) / dt,
        );
        let aa = 1.0 - (-dt / SHAKE_ACC_TAU).exp();
        self.acc.x += (inst.x - self.acc.x) * aa;
        self.acc.y += (inst.y - self.acc.y) * aa;
        deadband(self.acc)
    }
}

fn deadband(a: Vec2) -> Vec2 {
    let m = (a.x * a.x + a.y * a.y).sqrt();
    if m <= SHAKE_DEAD || m == 0.0 {
        return Vec2::new(0.0, 0.0);
    }
    let k = ((m - SHAKE_DEAD) / (SHAKE_FULL - SHAKE_DEAD)).min(1.0);
    Vec2::new(a.x * k, a.y * k)
}

impl Marble {
    /// Step the engine by `dt` seconds. `shake` is the canvas
    /// acceleration (px/s^2): the box yanked under the balls. In this
    /// non-inertial frame every ball feels F = -m * shake; static
    /// friction glues calm balls to the box, the loose ones lag,
    /// slide, and rattle. `walls` are the solid window boxes.
    /// Returns true while anything still moves (repaint needed).
    pub fn step(
        &mut self,
        dt: f64,
        shake: Vec2,
        walls: &[(f64, f64, f64, f64)],
    ) -> bool {
        let dt = dt.min(0.05);

        // --- resize eject FIRST: the rect grew over the ball while it
        // sat low. Shove it back out along the shallowest side before
        // anything else, or the wall pass would read it as arrived.
        if self.z < WIN_TOP - 1.0 {
            for (x, y, w, h) in walls {
                if self.pos.x >= *x
                    && self.pos.x <= x + w
                    && self.pos.y >= *y
                    && self.pos.y <= y + h
                {
                    let dl = self.pos.x - x;
                    let dr = x + w - self.pos.x;
                    let dpt = self.pos.y - y;
                    let db = y + h - self.pos.y;
                    let m = dl.min(dr).min(dpt).min(db);
                    let (nx, ny) = if m == dl {
                        (-1.0, 0.0)
                    } else if m == dr {
                        (1.0, 0.0)
                    } else if m == dpt {
                        (0.0, -1.0)
                    } else {
                        (0.0, 1.0)
                    };
                    self.pos.x += nx * (m + self.r);
                    self.pos.y += ny * (m + self.r);
                    let vn = self.vel.x * nx + self.vel.y * ny;
                    if vn < 0.0 {
                        self.vel.x -= (1.0 + E_WALL) * vn * nx;
                        self.vel.y -= (1.0 + E_WALL) * vn * ny;
                    }
                    self.vel.x += nx * 60.0;
                    self.vel.y += ny * 60.0;
                    break;
                }
            }
        }

        // --- substeps: no tunnelling. A fast ball crosses at most a
        // third of its radius per substep, so window walls (deep
        // boxes) always catch it.
        let travel = (self.vel.x * self.vel.x
            + self.vel.y * self.vel.y
            + self.vz * self.vz)
            .sqrt()
            * dt;
        let n = ((travel / (self.r * SUBSTEP_FACE)).ceil() as usize)
            .clamp(1, SUBSTEP_MAX);
        let h = dt / n as f64;
        for _ in 0..n {
            self.substep(h, shake, walls);
        }

        // --- rolling read: the interior tumbles with the roll
        // (unbounded Euler angles — never wraps, so no pops), plus a
        // slow idle turn so resting glass still feels alive.
        let speed =
            (self.vel.x * self.vel.x + self.vel.y * self.vel.y).sqrt();
        if speed > 0.5 {
            self.flow.x =
                wrap_flow(self.flow.x - self.vel.x * dt / self.r * 0.55);
            self.flow.y =
                wrap_flow(self.flow.y - self.vel.y * dt / self.r * 0.55);
            // Rolling: axis = up x velocity, visual rate (damped vs
            // true rolling so fast slides read instead of strobing).
            let w = speed / self.r * 0.35 * dt;
            self.rot[0] += (self.vel.y / speed) * w;
            self.rot[1] += -(self.vel.x / speed) * w;
        }
        self.rot[2] += 0.25 * dt;

        // --- sleep: slow ball on a calm box stops dead (static
        // friction owns it again) instead of creeping forever.
        let amag = (shake.x * shake.x + shake.y * shake.y).sqrt();
        let support = support_height(self.pos.x, self.pos.y, walls);
        let contact = self.z <= support + 0.5 && self.vz == 0.0;
        if contact && speed < SLEEP_V && amag < MU_S * GRAV {
            self.vel = Vec2::new(0.0, 0.0);
        }

        !contact || speed > 0.5 || self.vz != 0.0
    }

    /// One physics substep: forces, integrate, table, walls.
    fn substep(
        &mut self,
        h: f64,
        shake: Vec2,
        walls: &[(f64, f64, f64, f64)],
    ) {
        // --- forces: gravity + the shaken box (pseudo-force, the
        // table accelerating under the ball).
        self.vz -= GRAV * h;
        self.vel.x -= shake.x * h;
        self.vel.y -= shake.y * h;

        // --- friction, only in contact: static glues slow balls to a
        // calm box, kinetic + rolling bleed the sliders.
        let support = support_height(self.pos.x, self.pos.y, walls);
        let contact = self.z <= support + 0.5 && self.vz <= 0.0;
        if contact {
            let sp =
                (self.vel.x * self.vel.x + self.vel.y * self.vel.y).sqrt();
            let amag = (shake.x * shake.x + shake.y * shake.y).sqrt();
            if sp < SLEEP_V && amag < MU_S * GRAV {
                // Static friction wins: rides the box exactly.
                self.vel = Vec2::new(0.0, 0.0);
            } else if sp > 1e-9 {
                // Coulomb, opposing the slide, then rolling.
                let dv = MU_K * GRAV * h;
                let keep = if sp <= dv { 0.0 } else { (sp - dv) / sp };
                let roll = (-ROLL * h).exp();
                self.vel.x *= keep * roll;
                self.vel.y *= keep * roll;
            }
        }

        // --- integrate.
        self.pos.x += self.vel.x * h;
        self.pos.y += self.vel.y * h;
        self.z += self.vz * h;

        // --- table: the canvas, or a window top under the centre.
        // (Window sides run deep below the canvas, so there is no
        // "under" to slip into — low balls meet the wall pass.)
        let g = support_height(self.pos.x, self.pos.y, walls);
        if self.z <= g {
            self.z = g;
            if self.vz < -REST_VZ {
                // Glass tap: small hop, most of the slide eaten, a
                // whisper of sideways scatter — no two bounces match.
                self.vz = -self.vz * E_FLOOR;
                self.vel.x *= BOUNCE_KEEP;
                self.vel.y *= BOUNCE_KEEP;
                let impact = -self.vz / E_FLOOR;
                let k1 = self.rand() - 0.5;
                let k2 = self.rand() - 0.5;
                self.vel.x += k1 * KICK_K * impact;
                self.vel.y += k2 * KICK_K * impact;
            } else {
                self.vz = 0.0;
            }
        }

        // --- window walls: solid deep boxes. Only when the ball is
        // low (bottom below the top); anything riding at or above the
        // top passes and rests freely.
        if self.z < WIN_TOP - 0.5 {
            for (x, y, w, hgt) in walls {
                let cx = self.pos.x.clamp(*x, x + w);
                let cy = self.pos.y.clamp(*y, y + hgt);
                let inside = self.pos.x >= *x
                    && self.pos.x <= x + w
                    && self.pos.y >= *y
                    && self.pos.y <= y + hgt;
                if inside {
                    // Swallowed while low (spawned into a grown rect):
                    // the resize eject owns this case.
                    continue;
                }
                let mut nx = self.pos.x - cx;
                let mut ny = self.pos.y - cy;
                let d = (nx * nx + ny * ny).sqrt();
                if d < self.r && d > 1e-9 {
                    nx /= d;
                    ny /= d;
                    self.pos.x = cx + nx * self.r;
                    self.pos.y = cy + ny * self.r;
                    let vn = self.vel.x * nx + self.vel.y * ny;
                    if vn < 0.0 {
                        self.vel.x -= (1.0 + E_WALL) * vn * nx;
                        self.vel.y -= (1.0 + E_WALL) * vn * ny;
                        self.vel.x *= WALL_KEEP;
                        self.vel.y *= WALL_KEEP;
                    }
                }
            }
        }
    }
}

/// Ball-on-ball: a real pile, not ghosts. Masses ~ r^3 (heavy shoves
/// light), glassy restitution, positional split so nothing overlaps,
/// a whisper of tangential scatter on hard clacks — shake the box and
/// the layout never repeats. Returns true when anything moved.
pub fn collide_marbles(pile: &mut [Marble], rng: &mut u64) -> bool {
    let mut step_rng = || {
        let mut s = *rng | 1;
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        *rng = s;
        (s as f64) / (u64::MAX as f64)
    };
    let n = pile.len();
    let mut moved = false;
    for i in 0..n {
        for j in (i + 1)..n {
            let (dx, dy, rr) = {
                let a = &pile[i];
                let b = &pile[j];
                (b.pos.x - a.pos.x, b.pos.y - a.pos.y, a.r + b.r)
            };
            let d2 = dx * dx + dy * dy;
            if d2 >= rr * rr || d2 < 1e-12 {
                continue;
            }
            let d = d2.sqrt();
            let nx = dx / d;
            let ny = dy / d;
            let (left, right) = pile.split_at_mut(j);
            let a = &mut left[i];
            let b = &mut right[0];
            // Positional split, shared by inverse mass (light yields).
            let overlap = rr - d;
            let total = a.mass + b.mass;
            a.pos.x -= nx * overlap * (b.mass / total);
            a.pos.y -= ny * overlap * (b.mass / total);
            b.pos.x += nx * overlap * (a.mass / total);
            b.pos.y += ny * overlap * (a.mass / total);
            // Impulse along the normal, mass-weighted glassy clack.
            let vn = (b.vel.x - a.vel.x) * nx + (b.vel.y - a.vel.y) * ny;
            if vn < 0.0 {
                let jimp =
                    -(1.0 + E_MARBLE) * vn / (1.0 / a.mass + 1.0 / b.mass);
                a.vel.x -= jimp * nx / a.mass;
                a.vel.y -= jimp * ny / a.mass;
                b.vel.x += jimp * nx / b.mass;
                b.vel.y += jimp * ny / b.mass;
                if vn < -50.0 {
                    // Hard clack: a whisper of random sideways so
                    // chains never mirror.
                    let t = (step_rng() - 0.5) * 0.12 * -vn;
                    a.vel.x += -ny * t;
                    a.vel.y += nx * t;
                    b.vel.x += ny * t;
                    b.vel.y += -nx * t;
                }
            }
            moved = true;
        }
    }
    moved
}

/// Interior ribbons: opaque colour-glass threads inside the clear
/// ball, unit-ball coords. Returns one point list per ribbon;
/// each point is (x, y, z, half-width). Real marbles are built this
/// way — a gather of clear glass over coloured threads — so the
/// swirls below read like the photos, not decals.
///
/// 0 = cat's eye (spindle + equatorial ring), 1 = double-helix
/// swirl, 2 = wave band + accent line.
pub fn ribbons(pattern: u8) -> Vec<Vec<(f64, f64, f64, f64)>> {
    let tau = std::f64::consts::TAU;
    match pattern % 3 {
        0 => {
            // Spindle: fat in the middle, pointed at the poles.
            let mut spindle = Vec::with_capacity(30);
            for i in 0..30 {
                let t = i as f64 / 29.0;
                spindle.push((
                    0.0,
                    (t - 0.5) * 1.30,
                    0.0,
                    0.055 + 0.075 * (std::f64::consts::PI * t).sin(),
                ));
            }
            // Thin equatorial ring, slight wave.
            let mut ring = Vec::with_capacity(36);
            for i in 0..36 {
                let a = i as f64 / 36.0 * tau;
                ring.push((
                    0.52 * a.cos(),
                    0.06 * (2.0 * a).sin(),
                    0.52 * a.sin(),
                    0.050,
                ));
            }
            vec![spindle, ring]
        }
        1 => {
            // Two intertwined helices, 1.5 turns pole to pole.
            let mut out = Vec::with_capacity(2);
            for phase in [0.0, std::f64::consts::PI] {
                let mut helix = Vec::with_capacity(36);
                for i in 0..36 {
                    let t = i as f64 / 35.0;
                    let a = t * 1.5 * tau + phase;
                    helix.push((
                        0.46 * a.cos(),
                        (t - 0.5) * 1.10,
                        0.46 * a.sin(),
                        0.130,
                    ));
                }
                out.push(helix);
            }
            out
        }
        _ => {
            // Wave band around the belly + a thin accent line.
            let mut band = Vec::with_capacity(40);
            for i in 0..40 {
                let a = i as f64 / 40.0 * tau;
                band.push((
                    0.58 * a.cos(),
                    0.53 * a.sin(),
                    0.22 * (2.0 * a + 1.0).sin(),
                    0.125,
                ));
            }
            let mut accent = Vec::with_capacity(40);
            for i in 0..40 {
                let a = (i as f64 / 40.0) * tau + 0.35;
                accent.push((
                    0.46 * a.cos(),
                    0.42 * a.sin(),
                    0.18 * (2.0 * a + 1.0).sin(),
                    0.050,
                ));
            }
            vec![band, accent]
        }
    }
}

/// Rotate a unit-ball point by Euler angles (XYZ order).
pub fn orient(
    px: f64,
    py: f64,
    pz: f64,
    rot: [f64; 3],
) -> (f64, f64, f64) {
    let (sx, cx) = rot[0].sin_cos();
    let (sy, cy) = rot[1].sin_cos();
    let (sz, cz) = rot[2].sin_cos();
    let y1 = py * cx - pz * sx;
    let z1 = py * sx + pz * cx;
    let x2 = px * cy + z1 * sy;
    let z2 = -px * sy + z1 * cy;
    (x2 * cz - y1 * sz, x2 * sz + y1 * cz, z2)
}

/// Apparent (x, y) of an interior point seen through the ball
/// (unit coords): the sphere magnifies toward the centre; past
/// ~0.9R the fresnel edge swallows the point. Returns the warped
/// position plus an edge fade (1 = clear, 0 = gone at 0.97R).
pub fn lens(x: f64, y: f64) -> (f64, f64, f64) {
    let r2 = (x * x + y * y).min(1.0);
    let s = 1.0 + 0.38 * (1.0 - r2);
    let (mut ax, mut ay) = (x * s, y * s);
    let ar = (ax * ax + ay * ay).sqrt();
    let fade = ((0.97 - ar) / 0.12).clamp(0.0, 1.0);
    if ar > 0.96 {
        let k = 0.96 / ar;
        ax *= k;
        ay *= k;
    }
    (ax, ay, fade)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn walls() -> Vec<(f64, f64, f64, f64)> {
        vec![(100.0, 100.0, 200.0, 120.0)]
    }

    fn settle(m: &mut Marble, walls: &[(f64, f64, f64, f64)], secs: f64) {
        let n = (secs / 0.016) as usize;
        let z = Vec2::new(0.0, 0.0);
        for _ in 0..n {
            m.step(0.016, z, walls);
        }
    }

    #[test]
    fn shake_kills_idle_noise_but_passes_a_gentle_pan() {
        // Idle box read through a jittery clock: the input here is
        // already-smooth simulated velocity (camera.vel), so idle is
        // a ~px/s drift, not event quanta. The conditioner must
        // report exactly zero — drift never reaches the balls.
        let mut s = Shake::new();
        let mut seed = 0x1234_5678_9ABC_DEF1u64;
        let mut rnd = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed >> 11) as f64 / (u64::MAX >> 11) as f64
        };
        let mut peak: f64 = 0.0;
        let mut t = 0.0;
        for _ in 0..200 {
            let dt = 0.03 + 0.07 * rnd();
            t += dt;
            let v = 3.0 * (6.2831 * 0.5 * t).sin();
            let a = s.push(dt, Vec2::new(v, 0.0));
            peak = peak.max((a.x * a.x + a.y * a.y).sqrt());
        }
        assert!(peak == 0.0, "idle drift leaked through, peak={peak}");
        // A gentle pan (~600 px/s with hand wobble, jittery clock):
        // Michel's spec says a small move must waltz the balls, so
        // this must arrive at yank strength, not die in the filter.
        let mut s = Shake::new();
        let mut peak: f64 = 0.0;
        let mut t = 0.0;
        for _ in 0..200 {
            let dt = 0.03 + 0.07 * rnd();
            t += dt;
            let v = 600.0
                * (1.0
                    + 0.08 * (6.2831 * 1.3 * t).sin()
                    + 0.03 * (rnd() - 0.5) * 2.0);
            let a = s.push(dt, Vec2::new(v, 0.0));
            peak = peak.max((a.x * a.x + a.y * a.y).sqrt());
        }
        assert!(peak > MU_S * GRAV, "gentle pan died in the filter, peak={peak}");
        // Dead still reads zero.
        let mut s = Shake::new();
        for _ in 0..30 {
            let a = s.push(0.016, Vec2::new(12.0, -7.0));
            assert!((a.x * a.x + a.y * a.y).sqrt() == 0.0);
        }
    }

    #[test]
    fn ribbons_stay_inside_the_ball() {
        for p in 0..3 {
            for rib in ribbons(p) {
                assert!(!rib.is_empty());
                for (x, y, z, w) in rib {
                    let r = (x * x + y * y + z * z).sqrt() + w;
                    assert!(r <= 0.85, "ribbon escapes the ball, r={r}");
                }
            }
        }
    }

    #[test]
    fn lens_keeps_points_in_view() {
        let (ax, ay, fade) = lens(0.0, 0.0);
        assert!(ax.abs() < 1e-12 && ay.abs() < 1e-12 && fade == 1.0);
        let (ax, ay, fade) = lens(0.5, 0.0);
        assert!((ax - 0.6425).abs() < 1e-9 && fade == 1.0);
        let (ax, ay, fade) = lens(0.9, 0.0);
        assert!((ax * ax + ay * ay).sqrt() <= 0.96 + 1e-9);
        assert!(fade < 0.1, "rim point must fade, fade={fade}");
        for i in 0..200 {
            let a = i as f64 * 0.37;
            let (x, y, _) =
                orient(a.cos() * 0.7, a.sin() * 0.7, 0.3, [0.7, 0.2, a]);
            let (ax, ay, _) = lens(x, y);
            assert!((ax * ax + ay * ay).sqrt() <= 0.96 + 1e-9);
        }
    }

    #[test]
    fn orient_identity_and_turn() {
        let (x, y, z) = orient(0.3, -0.4, 0.5, [0.0, 0.0, 0.0]);
        assert!((x - 0.3).abs() + (y + 0.4).abs() + (z - 0.5).abs() < 1e-12);
        let (x, y, z) =
            orient(0.3, -0.4, 0.5, [std::f64::consts::PI, 0.0, 0.0]);
        assert!((x - 0.3).abs() + (y - 0.4).abs() + (z + 0.5).abs() < 1e-9);
    }

    #[test]
    fn lod_tiers_by_distance() {
        assert_eq!(lod_for(10.0, 100.0), Lod::Full);
        assert_eq!(lod_for(200.0, 100.0), Lod::Full);
        assert_eq!(lod_for(201.0, 100.0), Lod::Reduced);
        assert_eq!(lod_for(1000.0, 100.0), Lod::Reduced);
        assert_eq!(lod_for(1001.0, 100.0), Lod::Cull);
    }

    #[test]
    fn spawn_deals_fresh_motifs() {
        let a = spawn(10.0, 10.0, 11);
        let b = spawn(10.0, 10.0, 12);
        assert_ne!((a.pattern, a.seed), (b.pattern, b.seed));
        // Small masses ~ r^3: grams, not tonnes.
        assert!(a.mass > 0.0 && a.mass < 100.0);
        let expect = RHO * a.r * a.r * a.r;
        assert!((a.mass - expect).abs() < 1e-9);
    }

    #[test]
    fn spawn_drops_from_the_sky() {
        let m = spawn(60.0, 160.0, 5);
        assert!(m.z >= 170.0, "drop height, z={}", m.z);
        assert!(m.z <= 300.0, "not orbit, z={}", m.z);
    }

    #[test]
    fn falls_and_rests_on_canvas() {
        let mut m = spawn(50.0, 50.0, 1);
        settle(&mut m, &[], 3.0);
        assert_eq!(m.z, 0.0, "marble must land on the canvas");
        let speed = (m.vel.x * m.vel.x + m.vel.y * m.vel.y).sqrt();
        assert!(speed < 1.0, "must sleep, speed={speed}");
    }

    #[test]
    fn floor_taps_and_damps_slide() {
        // Drop high with sideways speed: the tap must be small and
        // the slide eaten, so nothing rolls to infinity.
        let mut m = spawn(50.0, 50.0, 2);
        m.z = 400.0;
        m.vz = 0.0;
        m.vel = Vec2::new(400.0, 0.0);
        let z = Vec2::new(0.0, 0.0);
        let mut top = 0.0f64;
        let mut touched = false;
        let mut slide_after = 400.0f64;
        for _ in 0..200 {
            m.step(0.016, z, &[]);
            if m.z == 0.0 {
                touched = true;
            }
            if touched {
                top = top.max(m.z);
                let s = (m.vel.x * m.vel.x + m.vel.y * m.vel.y).sqrt();
                slide_after = slide_after.min(s);
                if m.vz == 0.0 {
                    break;
                }
            }
        }
        assert!(touched, "must touch down");
        // e^2 * 400 ~= 25px: a tap, not a rebound. Generous bound.
        assert!(top < 60.0, "glass must barely bounce, top={top}");
        assert!(
            slide_after < 400.0,
            "table must eat slide, min={slide_after}"
        );
    }

    #[test]
    fn tremor_sleeps_but_a_small_shove_waltzes() {
        // Tremor-scale shake never wakes a settled ball; a small but
        // real shove breaks it loose and it waltzes (then settles —
        // see yank test for the long glide).
        let z = Vec2::new(0.0, 0.0);
        let mut m = spawn(50.0, 50.0, 3);
        m.z = 0.0;
        m.vz = 0.0;
        m.vel = Vec2::new(0.0, 0.0);
        settle(&mut m, &[], 1.0);
        for _ in 0..62 {
            m.step(0.016, Vec2::new(100.0, 0.0), &[]);
        }
        let d = ((m.pos.x - 50.0).powi(2) + (m.pos.y - 50.0).powi(2)).sqrt();
        assert!(d < 1.0, "tremor must not wake the ball, d={d}");
        for _ in 0..62 {
            m.step(0.016, Vec2::new(600.0, 0.0), &[]);
        }
        let d = ((m.pos.x - 50.0).powi(2) + (m.pos.y - 50.0).powi(2)).sqrt();
        assert!(d > 5.0, "a small shove must waltz the ball, d={d}");
    }

    #[test]
    fn yank_breaks_loose_and_never_returns() {
        // The box contract: yank it out and back and nothing is where
        // it was — like a real box in the hand.
        let z = Vec2::new(0.0, 0.0);
        let mut m = spawn(50.0, 50.0, 4);
        m.z = 0.0;
        m.vz = 0.0;
        m.vel = Vec2::new(0.0, 0.0);
        settle(&mut m, &[], 1.0);
        for _ in 0..15 {
            m.step(0.016, Vec2::new(20000.0, 0.0), &[]);
        }
        for _ in 0..15 {
            m.step(0.016, Vec2::new(-20000.0, 0.0), &[]);
        }
        settle(&mut m, &[], 2.5);
        let d = ((m.pos.x - 50.0).powi(2) + (m.pos.y - 50.0).powi(2)).sqrt();
        assert!(d > 15.0, "a yank must move the ball, d={d}");
        // Same yank again from the new spot: a different answer —
        // the box never repeats itself.
        let (px, py) = (m.pos.x, m.pos.y);
        for _ in 0..15 {
            m.step(0.016, Vec2::new(20000.0, 0.0), &[]);
        }
        for _ in 0..15 {
            m.step(0.016, Vec2::new(-20000.0, 0.0), &[]);
        }
        settle(&mut m, &[], 2.5);
        let d = ((m.pos.x - px).powi(2) + (m.pos.y - py).powi(2)).sqrt();
        assert!(d > 1.0, "no two shakes alike, d={d}");
    }

    #[test]
    fn window_is_a_solid_wall() {
        // Rolling into the side bonks back — never inside, never under
        // (the box runs deep below the canvas: no "under" exists).
        let z = Vec2::new(0.0, 0.0);
        let mut m = spawn(60.0, 160.0, 5);
        m.z = 0.0;
        m.vz = 0.0;
        m.vel = Vec2::new(600.0, 0.0);
        let mut bad = false;
        for _ in 0..120 {
            m.step(0.016, z, &walls());
            let (x, y, w, h) = walls()[0];
            let inside = m.pos.x > x
                && m.pos.x < x + w
                && m.pos.y > y
                && m.pos.y < y + h;
            if inside && m.z < WIN_TOP - 1.0 {
                bad = true;
            }
            assert!(m.z >= 0.0, "never below the canvas, z={}", m.z);
        }
        assert!(!bad, "ball must never be inside the solid box");
        assert!(
            m.pos.x + m.r <= 100.0 + 1.0,
            "bonk must send it back out, x={}",
            m.pos.x
        );
    }

    #[test]
    fn dropped_on_window_rests_on_top() {
        let mut m = spawn(200.0, 160.0, 6);
        settle(&mut m, &walls(), 3.0);
        assert_eq!(m.z, WIN_TOP, "must rest on the window top, z={}", m.z);
    }

    #[test]
    fn nothing_tunnels_through() {
        // Full speed at the wall: substeps must still catch it.
        let z = Vec2::new(0.0, 0.0);
        let mut m = spawn(40.0, 160.0, 7);
        m.z = 0.0;
        m.vz = 0.0;
        m.vel = Vec2::new(3000.0, 0.0);
        for _ in 0..60 {
            m.step(0.016, z, &walls());
            let (x, y, w, h) = walls()[0];
            let inside = m.pos.x > x
                && m.pos.x < x + w
                && m.pos.y > y
                && m.pos.y < y + h;
            assert!(
                !(inside && m.z < WIN_TOP - 1.0),
                "tunneled! x={} z={}",
                m.pos.x,
                m.z
            );
            assert!(m.z >= 0.0, "never below the canvas");
        }
    }

    #[test]
    fn heavy_shoves_light() {
        // Mass ~ r^3: the heavy ball barely slows, the light one flies.
        let z = Vec2::new(0.0, 0.0);
        let mut pile = vec![spawn(50.0, 50.0, 8), spawn(50.0, 50.0, 9)];
        pile[0].r = 22.0;
        pile[0].mass = RHO * 22.0 * 22.0 * 22.0;
        pile[1].r = 13.0;
        pile[1].mass = RHO * 13.0 * 13.0 * 13.0;
        pile[0].pos = Vec2::new(50.0, 50.0);
        pile[1].pos = Vec2::new(50.0 + 22.0 + 13.0 - 1.0, 50.0);
        for m in pile.iter_mut() {
            m.z = 0.0;
            m.vz = 0.0;
        }
        pile[0].vel = Vec2::new(400.0, 0.0);
        pile[1].vel = Vec2::new(0.0, 0.0);
        let mut rng = 0x1234_5678_9ABC_DEF1;
        for _ in 0..8 {
            for m in pile.iter_mut() {
                m.step(0.016, z, &[]);
            }
            collide_marbles(&mut pile, &mut rng);
        }
        let (a, b) = (&pile[0], &pile[1]);
        let sa = (a.vel.x * a.vel.x + a.vel.y * a.vel.y).sqrt();
        let sb = (b.vel.x * b.vel.x + b.vel.y * b.vel.y).sqrt();
        assert!(sb > sa, "light must leave faster: {sb} vs {sa}");
        assert!(sb > 100.0, "momentum must trade, sb={sb}");
    }

    #[test]
    fn pile_settles_without_overlap() {
        let z = Vec2::new(0.0, 0.0);
        let mut pile: Vec<Marble> = (0..5)
            .map(|i| {
                let mut m = spawn(50.0, 50.0, 20 + i);
                m.z = 0.0;
                m.vz = 0.0;
                m
            })
            .collect();
        let mut rng = 0xBEEF_CAFE_1234_5678;
        for _ in 0..200 {
            for m in pile.iter_mut() {
                m.step(0.016, z, &[]);
            }
            collide_marbles(&mut pile, &mut rng);
        }
        for i in 0..pile.len() {
            assert!(pile[i].z >= 0.0);
            for j in (i + 1)..pile.len() {
                let d = ((pile[j].pos.x - pile[i].pos.x).powi(2)
                    + (pile[j].pos.y - pile[i].pos.y).powi(2))
                .sqrt();
                assert!(
                    d >= pile[i].r + pile[j].r - 1.5,
                    "pile must part, d={d}"
                );
            }
        }
    }

    #[test]
    fn resize_ejects_swallowed_marble() {
        // The rect grew over a low ball: shoved out, never trapped.
        let z = Vec2::new(0.0, 0.0);
        let mut m = spawn(200.0, 150.0, 8);
        m.z = 0.0;
        m.vz = 0.0;
        m.vel = Vec2::new(0.0, 0.0);
        m.step(0.016, z, &walls());
        let (x, y, w, h) = walls()[0];
        let inside = m.pos.x >= x
            && m.pos.x <= x + w
            && m.pos.y >= y
            && m.pos.y <= y + h;
        assert!(!inside, "grown rect must eject, pos={:?}", m.pos);
    }

    #[test]
    fn motif_flow_wraps_forever() {
        let mut m = spawn(10.0, 10.0, 13);
        m.z = 0.0;
        m.vz = 0.0;
        m.vel = Vec2::new(500.0, 300.0);
        let z = Vec2::new(0.0, 0.0);
        for _ in 0..600 {
            m.step(0.05, z, &[]);
            assert!(m.flow.x.abs() <= FLOW_MAX + 1e-9);
            assert!(m.flow.y.abs() <= FLOW_MAX + 1e-9);
        }
    }
}

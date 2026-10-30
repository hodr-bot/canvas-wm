//! Glass marbles (the K key, Canvas mode).
//!
//! Pure physics + look: no GL, no Wayland, fully unit-tested. The
//! backend spawns one marble at the cursor per K press, steps the
//! pile every frame, and draws glass spheres as screen-space discs
//! (no new shader: the solid program paints layered fans/quads).
//!
//! Model: a marble lives on the canvas plane (world units = canvas
//! px, `z` = bottom height above the surface). Gravity pulls it down,
//! the plane (or a window pad) bounces it a LITTLE (glass toys tap,
//! they don't rebound like superballs); every landing scatters a
//! whisper sideways, so no two bounces match. Marbles collide with
//! EACH OTHER (glassy clacks that wake both) — a shaken pile never
//! repeats its layout. The canvas is the box bottom: stuck marbles
//! ride gentle pans exactly (static friction, navigation stays
//! calm), but a hard yank breaks them loose — the box accelerates
//! under the marbles and they lag, slide, clack, and settle
//! somewhere new. Ground friction + rolling resistance calm them;
//! slow and settled re-sticks them to the box. The inner motif
//! (cat's eye, swirl, ribbon — fresh every spawn) advects with the
//! roll so the surface visibly flows.
//!
//! Windows are static hitboxes with NO visible relief: each is a low
//! invisible pad (a few mm). Slow marbles lean on the lip and fall
//! back; fast ones hop a little and climb on top. Spawns fall from
//! the sky (~170-300px) and tap onto the cursor point.

use crate::canvas::Vec2;

/// Hard cap: the oldest marble leaves when a new one spawns past this.
pub const MAX_MARBLES: usize = 200;
/// How tall windows stand (canvas px): a shallow invisible pad so
/// marbles slow on the lip, hop a touch, and sit ON windows with
/// enough momentum instead of floating over a flat print.
pub const PAD_H: f64 = 8.0;
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
/// Vertical bounce kept on landing: glass gives a small hop, not a
/// superball rebound.
const REST_BOUNCE: f64 = 0.22;
/// |vz| below this on touchdown = rest instead of bounce.
const REST_VZ: f64 = 90.0;
/// How fast a marble couples to the canvas motion (/s): snappy, so
/// marbles start rolling the moment the canvas moves.
const DRAG_RATE: f64 = 3.2;
/// Base rolling resistance on contact (/s exponential); each marble
/// adds its own extra so the pile never moves in lockstep.
const ROLL_BASE: f64 = 2.2;
/// Break-loose acceleration (px/s^2): gentle pans ride along
/// (navigation stays calm), a real yank rattles the box.
const BREAK_A: f64 = 1400.0;
/// Re-stick below this canvas acceleration (px/s^2).
const STICK_A: f64 = 500.0;
/// Inertial coupling: the box accelerates under the marble, the
/// marble lags (fraction of the canvas accel felt, 0..1).
const INERT: f64 = 0.85;
/// Floor friction for a loose marble (/s exponential): weaker than
/// the stuck glue, so a yanked marble slides before it settles.
const DRAG_LOOSE: f64 = 1.1;
/// Marble-marble restitution: glass clacks, it doesn't bounce far.
const REST_MARBLE: f64 = 0.45;
/// Sideways kick on landing, fraction of impact speed: every bounce
/// scatters a little, never straight up-down.
const KICK_K: f64 = 0.10;
/// Below this planar speed a contacting marble sleeps.
const SLEEP: f64 = 6.0;
/// Minimum planar speed (px/s) to mount a window lip.
const CLIMB_V: f64 = 130.0;
/// Hop kick on mounting = min(speed * HOP_K, HOP_MAX).
const HOP_K: f64 = 0.35;
const HOP_MAX: f64 = 260.0;
/// Motif drift range (units of r): wraps, so the design flows forever.
const FLOW_MAX: f64 = 0.35;

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
    /// Canvas coupling 0.65..1.25: every marble answers a shove
    /// with its own eagerness.
    grip: f64,
    /// Rolling resistance 2.2..3.8: every marble stops on its own.
    roll_f: f64,
    /// Jitter accel when shaken 20..70 px/s^2, fixed per-marble
    /// direction: a yanked canvas scatters, never translates.
    jig: f64,
    jig_ang: f64,
    /// Stuck = glued to the box (static friction): rides gentle
    /// pans exactly. A hard yank breaks it loose; landing slow
    /// re-sticks it. This stick-slip is what makes a shaken box
    /// never return to the same layout.
    stuck: bool,
    /// Per-marble RNG (xorshift state): bounce kicks and shaken
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
/// cursor point with a fresh inner motif and its own character.
pub fn spawn(x: f64, y: f64, seed: u64) -> Marble {
    let mut rnd = xorshift(seed.wrapping_add(0x9e37_79b9_7f4a_7c15));
    let unit = rnd();
    Marble {
        pos: Vec2::new(x, y),
        z: 170.0 + 130.0 * rnd(),
        vel: Vec2::new((rnd() - 0.5) * 280.0, (rnd() - 0.5) * 280.0),
        vz: 0.0,
        r: 13.0 + 9.0 * rnd(),
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
        grip: 0.65 + 0.6 * rnd(),
        roll_f: ROLL_BASE + 1.6 * rnd(),
        jig: 20.0 + 50.0 * rnd(),
        jig_ang: rnd() * std::f64::consts::TAU,
        // Falling: not stuck until it lands slow.
        stuck: false,
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

/// Surface height at a canvas point: window pads stand PAD_H tall,
/// everywhere else is the bare plane. The pads are NOT drawn — they
/// are felt (lip, hop, climb), never seen. Shared with the backend
/// for blob shadows.
pub(crate) fn ground_height(
    x: f64,
    y: f64,
    walls: &[(f64, f64, f64, f64)],
) -> f64 {
    for (wx, wy, w, h) in walls {
        if x >= *wx && x <= wx + w && y >= *wy && y <= wy + h {
            return PAD_H;
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

impl Marble {
    /// Ground height under the footprint: the max of centre + 4 edge
    /// samples, so a marble straddling a pad lip rides on the top
    /// instead of clipping halfway through the side.
    fn ground(&self, walls: &[(f64, f64, f64, f64)]) -> f64 {
        Self::ground_at(self.pos, walls, self.r)
    }

    /// Footprint ground at an arbitrary point (same sampling as
    /// ground()): centre + 4 rim samples.
    fn ground_at(
        p: Vec2,
        walls: &[(f64, f64, f64, f64)],
        r: f64,
    ) -> f64 {
        let rr = r * 0.6;
        let mut g = ground_height(p.x, p.y, walls);
        for (dx, dy) in [(-rr, -rr), (rr, -rr), (-rr, rr), (rr, rr)] {
            let h = ground_height(p.x + dx, p.y + dy, walls);
            if h > g {
                g = h;
            }
        }
        g
    }

    /// Step physics by `dt` seconds. `cam_vel` is the canvas-slide
    /// velocity (canvas px/s); `accel` is its acceleration — the
    /// yank that breaks static friction and rattles the box.
    /// Returns true while anything still moves (the frame must repaint).
    pub fn step(
        &mut self,
        dt: f64,
        cam_vel: Vec2,
        accel: Vec2,
        walls: &[(f64, f64, f64, f64)],
    ) -> bool {
        let dt = dt.min(0.05);

        // --- resize eject FIRST: the rect grew over the marble (centre
        // inside while rolling below the lip). Shove it back out along
        // the shallowest side before the ground snap, or it would pop
        // on top instead. Clears the whole footprint (1.6r) so the rim
        // doesn't catch the pad on the way out.
        if self.z < PAD_H - 1.0 {
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
                    self.pos.x += nx * (m + self.r * 1.6);
                    self.pos.y += ny * (m + self.r * 1.6);
                    let vn = self.vel.x * nx + self.vel.y * ny;
                    if vn < 0.0 {
                        self.vel.x -= 1.25 * vn * nx;
                        self.vel.y -= 1.25 * vn * ny;
                    }
                    self.vel.x += nx * 60.0;
                    self.vel.y += ny * 60.0;
                    break;
                }
            }
        }
        let ground = self.ground(walls);

        // --- vertical: gravity, small glass hop, rest ---
        self.vz -= GRAV * dt;
        self.z += self.vz * dt;
        if self.z <= ground {
            self.z = ground;
            if self.vz < -REST_VZ {
                self.vz = -self.vz * REST_BOUNCE;
                // The hop eats most of the slide: glass taps, then grips.
                self.vel.x *= 0.85;
                self.vel.y *= 0.85;
                // Sideways scatter: every landing kicks a little,
                // proportional to the impact, in a random direction
                // — no two bounces ever match.
                let impact = -self.vz / REST_BOUNCE;
                let k1 = self.rand() - 0.5;
                let k2 = self.rand() - 0.5;
                self.vel.x += k1 * KICK_K * impact;
                self.vel.y += k2 * KICK_K * impact;
                // Taps wake it: a landing marble is live, not stuck.
                self.stuck = false;
            } else {
                self.vz = 0.0;
            }
        }
        let contact = self.z <= ground + 0.5 && self.vz == 0.0;

        // --- planar: the sliding canvas drags the marble (own grip),
        // a fixed per-marble jitter direction scatters shakes, rolling
        // resistance (own rate) bleeds motion on contact ---
        // --- canvas coupling: stick-slip. Stuck marbles ride gentle
        // pans exactly (static friction — navigation stays calm). A
        // hard yank breaks them loose: the box accelerates under the
        // marble and the marble lags (inertia), then floor friction
        // + collisions + scatter decide where it ends up — never
        // where it started. Airborne marbles always feel the yank.
        let amag = (accel.x * accel.x + accel.y * accel.y).sqrt();
        let airborne = self.z > ground + 0.5;
        if self.stuck && (airborne || amag > BREAK_A) {
            self.stuck = false;
        }
        if !self.stuck {
            self.vel.x -= accel.x * INERT * dt;
            self.vel.y -= accel.y * INERT * dt;
            // Shaken scatter: random kicks ∝ yank strength, drawn
            // from the per-marble RNG — the pile never repeats.
            if amag > 300.0 {
                let s = (amag / 4000.0).min(1.0);
                let a1 = self.rand() * std::f64::consts::TAU;
                let a2 = self.rand() * std::f64::consts::TAU;
                let kick = 900.0 * s * dt;
                self.vel.x += a1.cos() * kick * (0.5 + self.rand());
                self.vel.y += a2.sin() * kick * (0.5 + self.rand());
            }
        }
        // --- planar drag: the stuck glue is snappy (rides the view),
        // the loose floor friction is weaker (slides before settling).
        let rate = if self.stuck { DRAG_RATE } else { DRAG_LOOSE };
        let k = (rate * dt).min(1.0);
        self.vel.x += (-cam_vel.x * self.grip - self.vel.x) * k;
        self.vel.y += (-cam_vel.y * self.grip - self.vel.y) * k;
        let cam_speed =
            (cam_vel.x * cam_vel.x + cam_vel.y * cam_vel.y).sqrt();
        if cam_speed > 30.0 {
            let push = (self.jig * (cam_speed / 800.0).min(1.0) * dt).min(12.0);
            self.vel.x += self.jig_ang.cos() * push;
            self.vel.y += self.jig_ang.sin() * push;
        }
        if contact {
            let f = (-self.roll_f * dt).exp();
            self.vel.x *= f;
            self.vel.y *= f;
        }
        let prev = self.pos;
        self.pos.x += self.vel.x * dt;
        self.pos.y += self.vel.y * dt;

        // --- window lips: slow marbles lean and fall back, fast ones
        // mount with a small hop. Pads are low, so anything already
        // above the lip passes freely; only the rising step is gated.
        // The gate reads the FOOTPRINT ground (same as the physics
        // ground): the lean zone never vetoes a fast climber before
        // its centre crosses.
        let g_prev = Self::ground_at(prev, walls, self.r);
        let g_new = Self::ground_at(self.pos, walls, self.r);
        if g_new > g_prev + 1.0 && self.z < PAD_H - 1.0 {
            let speed =
                (self.vel.x * self.vel.x + self.vel.y * self.vel.y).sqrt();
            if speed < CLIMB_V {
                // Not enough momentum: stay down, bonk softly.
                self.pos = prev;
                self.vel.x *= -0.25;
                self.vel.y *= -0.25;
            } else {
                // Over the lip with momentum: stand on the pad and
                // take a small hop, like a marble hopping a doorsill.
                self.z = self.z.max(g_new);
                self.vz =
                    self.vz.max((speed * HOP_K).min(HOP_MAX));
            }
        }
        // Never overlap a lip while fully below it: lean on it.
        // (Footprint already on the pad = climbing: let it through,
        // the ground snap lifts it.)
        if self.z < PAD_H - 1.0 && self.ground(walls) <= 0.0 {
            for (x, y, w, h) in walls {
                let cx = self.pos.x.clamp(*x, x + w);
                let cy = self.pos.y.clamp(*y, y + h);
                let inside = self.pos.x >= *x
                    && self.pos.x <= x + w
                    && self.pos.y >= *y
                    && self.pos.y <= y + h;
                if inside {
                    // Riding on top, or a footprint straddling the lip
                    // mid-mount: the ground snap lifts it. (Swallowed
                    // marbles never reach here — the resize eject above
                    // owns them.)
                    continue;
                }
                let mut nx = self.pos.x - cx;
                let mut ny = self.pos.y - cy;
                let d = (nx * nx + ny * ny).sqrt();
                if d < self.r && d > 1e-6 {
                    nx /= d;
                    ny /= d;
                    let vn = self.vel.x * nx + self.vel.y * ny;
                    // Fast head-on approach: hands off — the mount gate
                    // above owns this marble (it mounts or bonks). The
                    // lean only holds slow marbles leaning on the lip.
                    if vn < -CLIMB_V {
                        continue;
                    }
                    self.pos.x = cx + nx * self.r;
                    self.pos.y = cy + ny * self.r;
                    if vn < 0.0 {
                        self.vel.x -= 1.25 * vn * nx;
                        self.vel.y -= 1.25 * vn * ny;
                    }
                }
            }
        }

        let speed =
            (self.vel.x * self.vel.x + self.vel.y * self.vel.y).sqrt();
        // --- rolling read: the interior tumbles with the roll (unbounded
        // Euler angles — never wraps, so no pops), plus a slow idle
        // turn so resting glass still feels alive ---
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

        // --- sleep: slow and calm canvas = re-stick to the box.
        // A loose marble that settles slow re-glues (static
        // friction); the strong glue then owns it again.
        if contact && speed < SLEEP {
            let cam = (cam_vel.x * cam_vel.x + cam_vel.y * cam_vel.y).sqrt();
            if cam < 8.0 && amag < STICK_A {
                self.vel = Vec2::new(0.0, 0.0);
                self.stuck = true;
            } else if cam < 8.0 {
                self.vel = Vec2::new(0.0, 0.0);
            }
        }

        !contact || speed > 0.5 || self.vz != 0.0
    }
}

/// Marble-marble collisions: the pile is a real box of glass, not
/// ghosts. Equal masses, glassy restitution, positional split so
/// nothing overlaps. Hard clacks wake both marbles (loose) and add
/// a whisper of random tangential scatter — shake the box and the
/// layout never repeats. Returns true when anything moved.
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
            // Positional split (share by radius so big/small part
            // cleanly), with a hair of slop left to settle on.
            let overlap = rr - d;
            let (left, right) = pile.split_at_mut(j);
            let a = &mut left[i];
            let b = &mut right[0];
            let share = overlap / rr;
            a.pos.x -= nx * share * a.r;
            a.pos.y -= ny * share * a.r;
            b.pos.x += nx * share * b.r;
            b.pos.y += ny * share * b.r;
            // Impulse along the normal, glassy clack.
            let vn = (b.vel.x - a.vel.x) * nx + (b.vel.y - a.vel.y) * ny;
            if vn < 0.0 {
                let jimp = -(1.0 + REST_MARBLE) * vn / 2.0;
                a.vel.x -= jimp * nx;
                a.vel.y -= jimp * ny;
                b.vel.x += jimp * nx;
                b.vel.y += jimp * ny;
                if vn < -50.0 {
                    // Hard clack: both wake, plus a whisper of
                    // random sideways so chains never mirror.
                    a.stuck = false;
                    b.stuck = false;
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
        for _ in 0..n {
            m.step(0.016, Vec2::new(0.0, 0.0), Vec2::new(0.0, 0.0), walls);
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
        // Full sweep: nothing ever leaves the disc.
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
        // Half turn about X flips y and z.
        let (x, y, z) =
            orient(0.3, -0.4, 0.5, [std::f64::consts::PI, 0.0, 0.0]);
        assert!((x - 0.3).abs() + (y - 0.4).abs() + (z + 0.5).abs() < 1e-9);
    }

    #[test]
    fn falls_and_rests_on_canvas() {
        let mut m = spawn(50.0, 50.0, 1);
        settle(&mut m, &[], 3.0);
        assert_eq!(m.z, 0.0, "marble must land on the canvas");
        let speed = (m.vel.x * m.vel.x + m.vel.y * m.vel.y).sqrt();
        assert!(speed < 1.0, "must sleep, speed={}", speed);
    }

    #[test]
    fn bounce_is_small_glass_tap() {
        let mut m = spawn(50.0, 50.0, 2);
        m.z = 400.0;
        m.vz = 0.0;
        m.vel = Vec2::new(0.0, 0.0);
        let mut top = 0.0f64;
        let mut touched = false;
        for _ in 0..200 {
            m.step(0.016, Vec2::new(0.0, 0.0), Vec2::new(0.0, 0.0), &[]);
            if m.z == 0.0 {
                touched = true;
            }
            if touched {
                top = top.max(m.z);
            }
            if touched && m.vz == 0.0 {
                break;
            }
        }
        // e^2 * 400 ~= 19px: a tap, not a rebound. Generous bound.
        assert!(top < 60.0, "glass must barely bounce, top={}", top);
    }

    #[test]
    fn rests_on_window_pad() {
        let mut m = spawn(200.0, 160.0, 3);
        settle(&mut m, &walls(), 3.0);
        assert_eq!(m.z, PAD_H, "marble must rest on the pad, z={}", m.z);
    }

    #[test]
    fn slow_marble_leans_on_lip() {
        let mut m = spawn(60.0, 160.0, 4);
        m.z = 0.0;
        m.vel = Vec2::new(60.0, 0.0); // below CLIMB_V
        for _ in 0..90 {
            m.step(0.016, Vec2::new(0.0, 0.0), Vec2::new(0.0, 0.0), &walls());
        }
        // Still off the pad: leaning, never mounted.
        assert!(
            m.pos.x + m.r * 0.5 < 100.0 || m.z < PAD_H,
            "slow marble must not climb, x={} z={}",
            m.pos.x,
            m.z
        );
        assert!(m.z < PAD_H, "slow marble stays down, z={}", m.z);
    }

    #[test]
    fn fast_marble_mounts_with_hop() {
        let mut m = spawn(60.0, 160.0, 5);
        m.z = 0.0;
        m.vel = Vec2::new(500.0, 0.0);
        let mut top = 0.0f64;
        for _ in 0..120 {
            m.step(0.016, Vec2::new(0.0, 0.0), Vec2::new(0.0, 0.0), &walls());
            top = top.max(m.z);
        }
        assert!(
            top > PAD_H * 0.5,
            "fast marble must hop the lip, top={}",
            top
        );
    }

    #[test]
    fn same_shove_different_answers() {
        let mut a = spawn(50.0, 50.0, 10);
        let mut b = spawn(50.0, 50.0, 11);
        a.z = 0.0;
        b.z = 0.0;
        a.vel = Vec2::new(0.0, 0.0);
        b.vel = Vec2::new(0.0, 0.0);
        let shove = Vec2::new(-600.0, 100.0);
        for _ in 0..30 {
            a.step(0.016, shove, Vec2::new(0.0, 0.0), &[]);
            b.step(0.016, shove, Vec2::new(0.0, 0.0), &[]);
        }
        let dx = (a.vel.x - b.vel.x).abs() + (a.vel.y - b.vel.y).abs();
        assert!(
            dx > 5.0,
            "grip/jitter must split twins apart, d={}",
            dx
        );
    }

    #[test]
    fn spawn_deals_fresh_motifs() {
        let mut seen = [false; 3];
        for i in 0..24 {
            seen[spawn(0.0, 0.0, 100 + i).pattern as usize] = true;
        }
        assert!(
            seen.iter().all(|s| *s),
            "spawns must deal every motif, seen={:?}",
            seen
        );
    }

    #[test]
    fn motif_flow_wraps_forever() {
        let mut m = spawn(50.0, 50.0, 7);
        m.z = 0.0;
        m.vel = Vec2::new(800.0, 0.0);
        for _ in 0..600 {
            m.step(0.016, Vec2::new(0.0, 0.0), Vec2::new(0.0, 0.0), &[]);
        }
        assert!(
            m.flow.x.abs() <= FLOW_MAX + 1e-9,
            "flow must stay wrapped, fx={}",
            m.flow.x
        );
    }


    #[test]
    fn lod_tiers_by_distance() {
        assert_eq!(lod_for(0.0, 100.0), Lod::Full);
        assert_eq!(lod_for(200.0, 100.0), Lod::Full);
        assert_eq!(lod_for(201.0, 100.0), Lod::Reduced);
        assert_eq!(lod_for(1000.0, 100.0), Lod::Reduced);
        assert_eq!(lod_for(1001.0, 100.0), Lod::Cull);
    }

    #[test]
    fn resize_ejects_swallowed_marble() {
        // Grown over by a resize: centre inside, rolling below the lip.
        let mut m = spawn(200.0, 150.0, 7);
        m.z = 0.0;
        m.vel = Vec2::new(0.0, 0.0);
        m.step(0.016, Vec2::new(0.0, 0.0), Vec2::new(0.0, 0.0), &walls());
        let (x, y, w, h) = walls()[0];
        let inside = m.pos.x >= x
            && m.pos.x <= x + w
            && m.pos.y >= y
            && m.pos.y <= y + h;
        assert!(!inside, "resize must push the marble out, pos={:?}", m.pos);
    }

    #[test]
    fn spawn_drops_from_the_sky() {
        // K must feel like dropping a marble into the box: height,
        // sideways scatter, live (not stuck) until it lands slow.
        let m = spawn(60.0, 160.0, 42);
        assert!(
            (170.0..=300.0).contains(&m.z),
            "spawn must fall from height, z={}",
            m.z
        );
        assert!(!m.stuck, "a falling marble is not stuck");
    }

    #[test]
    fn bounce_kicks_sideways() {
        // Dropped dead straight, it must not land in its own
        // footprint: every bounce scatters a little.
        let mut m = spawn(50.0, 50.0, 7);
        m.pos = Vec2::new(50.0, 50.0);
        m.vel = Vec2::new(0.0, 0.0);
        settle(&mut m, &[], 2.5);
        let d = ((m.pos.x - 50.0).powi(2) + (m.pos.y - 50.0).powi(2)).sqrt();
        assert!(d > 3.0, "bounce must scatter the landing, d={}", d);
    }

    #[test]
    fn gentle_pan_rides_but_yank_scatters() {
        // The box contract: carry it gently and the layout rides
        // along; yank it out and back and nothing is where it was.
        let z = Vec2::new(0.0, 0.0);
        let mut m = spawn(50.0, 50.0, 7);
        m.z = 0.0;
        m.vz = 0.0;
        m.vel = Vec2::new(0.0, 0.0);
        settle(&mut m, &[], 2.0);
        assert!(m.stuck, "settled marble must be stuck, stuck={}", m.stuck);
        // Gentle carry out (100px/s) and back: rides the box.
        for _ in 0..62 {
            m.step(0.016, Vec2::new(100.0, 0.0), z, &[]);
        }
        for _ in 0..62 {
            m.step(0.016, Vec2::new(-100.0, 0.0), z, &[]);
        }
        settle(&mut m, &[], 1.0);
        let d = ((m.pos.x - 50.0).powi(2) + (m.pos.y - 50.0).powi(2)).sqrt();
        assert!(d < 3.0, "gentle carry must ride along, d={}", d);
        // Yank out and back: acceleration spikes break it loose.
        for _ in 0..15 {
            m.step(0.016, Vec2::new(1500.0, 0.0), Vec2::new(30000.0, 0.0), &[]);
        }
        for _ in 0..15 {
            m.step(0.016, Vec2::new(-1500.0, 0.0), Vec2::new(-30000.0, 0.0), &[]);
        }
        settle(&mut m, &[], 2.0);
        let d = ((m.pos.x - 50.0).powi(2) + (m.pos.y - 50.0).powi(2)).sqrt();
        assert!(d > 15.0, "a yank must move the marble, d={}", d);
        // Same yank again from the new layout: different answer —
        // the box never repeats itself.
        let (px, py) = (m.pos.x, m.pos.y);
        for _ in 0..15 {
            m.step(0.016, Vec2::new(1500.0, 0.0), Vec2::new(30000.0, 0.0), &[]);
        }
        for _ in 0..15 {
            m.step(0.016, Vec2::new(-1500.0, 0.0), Vec2::new(-30000.0, 0.0), &[]);
        }
        settle(&mut m, &[], 2.0);
        let d = ((m.pos.x - px).powi(2) + (m.pos.y - py).powi(2)).sqrt();
        assert!(d > 1.0, "no two shakes alike, d={}", d);
    }

    #[test]
    fn marbles_clack_and_wake() {
        // Overlapping pile, one rolling in: they must part, trade
        // momentum, and both wake up.
        let mut pile = vec![spawn(50.0, 50.0, 7), spawn(70.0, 50.0, 8)];
        for m in pile.iter_mut() {
            m.z = 0.0;
            m.vz = 0.0;
            m.stuck = true;
        }
        pile[0].vel = Vec2::new(400.0, 0.0);
        pile[1].vel = Vec2::new(0.0, 0.0);
        let mut rng = 0x1234_5678_9ABC_DEF1;
        let mut moved = false;
        let mut kick = 0.0f64;
        for n in 0..60 {
            for m in pile.iter_mut() {
                m.step(0.016, Vec2::new(0.0, 0.0), Vec2::new(0.0, 0.0), &[]);
            }
            moved |= collide_marbles(&mut pile, &mut rng);
            if n == 8 {
                // Early: the clack must already trade momentum.
                kick = pile[1].vel.x;
            }
        }
        let (a, b) = (&pile[0], &pile[1]);
        let d = ((b.pos.x - a.pos.x).powi(2) + (b.pos.y - a.pos.y).powi(2)).sqrt();
        assert!(d >= a.r + b.r - 1.0, "clack must part them, d={}", d);
        assert!(kick > 50.0, "momentum must trade, kick={}", kick);
        assert!(moved, "collision pass must report motion");
    }

    #[test]
    fn rider_stays_on_pad() {
        // Already riding on top: the rect growth must not eject it.
        let mut m = spawn(200.0, 150.0, 8);
        m.z = PAD_H;
        m.vz = 0.0;
        m.vel = Vec2::new(0.0, 0.0);
        for _ in 0..30 {
            m.step(0.016, Vec2::new(0.0, 0.0), Vec2::new(0.0, 0.0), &walls());
        }
        let (x, y, w, h) = walls()[0];
        let inside = m.pos.x >= x
            && m.pos.x <= x + w
            && m.pos.y >= y
            && m.pos.y <= y + h;
        assert!(inside, "rider must stay on the pad, pos={:?}", m.pos);
        assert!((m.z - PAD_H).abs() < 1e-9, "rider keeps pad height");
    }
}

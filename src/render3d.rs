//! Minimal solid-colour quad renderer: the seed of the 3D engine.
//! Window chrome (close/fullscreen squares) draws through this today;
//! textured perspective planes will reuse the same program cache later.
//!
//! The program is compiled once per GL context and driven with a single
//! MVP matrix. The only matrix shipped today is an orthographic one in
//! screen pixels (x right, y down); perspective lands here next.

#[cfg(feature = "compositor")]
use std::ffi::CString;

#[cfg(feature = "compositor")]
use smithay::backend::renderer::gles2::ffi;

/// Column-major 4x4 orthographic matrix mapping screen pixels
/// (x right, y down, origin top-left) to GL clip space.
pub fn ortho(view_w: f32, view_h: f32) -> [f32; 16] {
    let sx = 2.0 / view_w;
    let sy = -2.0 / view_h;
    [
        sx, 0.0, 0.0, 0.0, //
        0.0, sy, 0.0, 0.0, //
        0.0, 0.0, -1.0, 0.0, //
        -1.0, 1.0, 0.0, 1.0,
    ]
}

/// Column-major perspective (fov in radians, y down like ortho —
/// the caller flips geometry instead). Standard frustum projection.
pub fn perspective(fov_y: f32, aspect: f32, near: f32, far: f32) -> [f32; 16] {
    let f = 1.0 / (fov_y * 0.5).tan();
    let nf = 1.0 / (near - far);
    [
        f / aspect, 0.0, 0.0, 0.0, //
        0.0, f, 0.0, 0.0, //
        0.0, 0.0, (far + near) * nf, -1.0, //
        0.0, 0.0, 2.0 * far * near * nf, 0.0,
    ]
}

/// Column-major translation.
pub fn translate(tx: f32, ty: f32, tz: f32) -> [f32; 16] {
    [
        1.0, 0.0, 0.0, 0.0, //
        0.0, 1.0, 0.0, 0.0, //
        0.0, 0.0, 1.0, 0.0, //
        tx, ty, tz, 1.0,
    ]
}

/// Column-major rotation about the X axis (radians).
pub fn rot_x(a: f32) -> [f32; 16] {
    let (s, c) = a.sin_cos();
    [
        1.0, 0.0, 0.0, 0.0, //
        0.0, c, s, 0.0, //
        0.0, -s, c, 0.0, //
        0.0, 0.0, 0.0, 1.0,
    ]
}

/// Column-major rotation about the Y axis (radians).
pub fn rot_y(a: f32) -> [f32; 16] {
    let (s, c) = a.sin_cos();
    [
        c, 0.0, -s, 0.0, //
        0.0, 1.0, 0.0, 0.0, //
        s, 0.0, c, 0.0, //
        0.0, 0.0, 0.0, 1.0,
    ]
}

/// Column-major product `a * b` (b applies first).
pub fn mul(a: &[f32; 16], b: &[f32; 16]) -> [f32; 16] {
    let mut c = [0.0f32; 16];
    for col in 0..4 {
        for row in 0..4 {
            let mut sum = 0.0;
            for k in 0..4 {
                sum += a[k * 4 + row] * b[col * 4 + k];
            }
            c[col * 4 + row] = sum;
        }
    }
    c
}

/// MVP for a canvas plane viewed by a perspective camera.
///
/// `view` is the screen size in px, `center` the canvas point under the
/// screen middle, `zoom` the camera zoom. `tilt` is (y, x) radians applied
/// about the view center — (0, 0) reproduces the flat orthographic mapping
/// (same pixels, perspective division included). Geometry Y is negated by
/// the caller (canvas y grows down, clip y grows up).
pub fn canvas_mvp(
    view: (f32, f32),
    center: (f32, f32),
    zoom: f32,
    tilt: (f32, f32),
) -> [f32; 16] {
    let fov = 60.0f32.to_radians();
    let f = 1.0 / (fov * 0.5).tan();
    // Camera distance: at zero tilt the z=0 plane maps 1:1 with zoom.
    let dist = f * view.1 / (2.0 * zoom);
    let p = perspective(fov, view.0 / view.1, 1.0, 100000.0);
    let to_origin = translate(-center.0, center.1, 0.0);
    let tilt_m = mul(&rot_y(tilt.0), &rot_x(tilt.1));
    let back = translate(0.0, 0.0, -dist);
    mul(&p, &mul(&back, &mul(&tilt_m, &to_origin)))
}

#[cfg(feature = "compositor")]
const VERT_SRC: &str = "#version 100
attribute vec2 position;
uniform mat4 mvp;
void main() { gl_Position = mvp * vec4(position, 0.0, 1.0); }
";

#[cfg(feature = "compositor")]
const FRAG_SRC: &str = "#version 100
precision mediump float;
uniform vec4 color;
void main() { gl_FragColor = color; }
";

#[cfg(feature = "compositor")]
const VERT_TEX_SRC: &str = "#version 100
attribute vec2 position;
attribute vec2 uv;
uniform mat4 mvp;
varying vec2 v_uv;
void main() { v_uv = uv; gl_Position = mvp * vec4(position, 0.0, 1.0); }
";

#[cfg(feature = "compositor")]
const FRAG_TEX_SRC: &str = "#version 100
precision mediump float;
uniform sampler2D tex;
uniform float opaque;
varying vec2 v_uv;
void main() { vec4 t = texture2D(tex, v_uv); gl_FragColor = vec4(t.rgb, mix(t.a, 1.0, opaque)); }
";

/// Compile the solid-colour program. Returns the program id plus the
/// `mvp` / `color` uniform and `position` attribute locations.
#[cfg(feature = "compositor")]
pub fn compile_solid(gl: &ffi::Gles2) -> Result<(u32, i32, i32, u32), String> {
    let c = |s: &str| CString::new(s).unwrap();
    unsafe {
        let vert = compile(gl, ffi::VERTEX_SHADER, VERT_SRC)?;
        let frag = compile(gl, ffi::FRAGMENT_SHADER, FRAG_SRC)?;
        let program = gl.CreateProgram();
        gl.AttachShader(program, vert);
        gl.AttachShader(program, frag);
        gl.LinkProgram(program);
        gl.DetachShader(program, vert);
        gl.DetachShader(program, frag);
        gl.DeleteShader(vert);
        gl.DeleteShader(frag);
        let mut status = ffi::FALSE as i32;
        gl.GetProgramiv(program, ffi::LINK_STATUS, &mut status as *mut _);
        if status == ffi::FALSE as i32 {
            gl.DeleteProgram(program);
            return Err("solid program link failed".into());
        }
        let mvp = gl.GetUniformLocation(program, c("mvp").as_ptr() as *const _);
        let color = gl.GetUniformLocation(program, c("color").as_ptr() as *const _);
        let pos = gl.GetAttribLocation(program, c("position").as_ptr() as *const _) as u32;
        Ok((program, mvp, color, pos))
    }
}

#[cfg(feature = "compositor")]
unsafe fn compile(gl: &ffi::Gles2, variant: u32, src: &'static str) -> Result<u32, String> {
    let shader = gl.CreateShader(variant);
    gl.ShaderSource(
        shader,
        1,
        &src.as_ptr() as *const *const u8 as *const *const ffi::types::GLchar,
        &(src.len() as i32) as *const _,
    );
    gl.CompileShader(shader);
    let mut status = ffi::FALSE as i32;
    gl.GetShaderiv(shader, ffi::COMPILE_STATUS, &mut status as *mut _);
    if status == ffi::FALSE as i32 {
        gl.DeleteShader(shader);
        return Err(format!("solid shader compile failed: {:?}", variant));
    }
    Ok(shader)
}

/// Draw one solid rect (x, y, w, h in MVP units) with an RGBA colour.
/// Leaves the GL state as Smithay's frame engine expects it: its program
/// rebound by its own draws, our attribs disabled, scissor restored.
#[cfg(feature = "compositor")]
pub unsafe fn draw_solid(
    gl: &ffi::Gles2,
    prog: (u32, i32, i32, u32),
    mvp: &[f32; 16],
    color: [f32; 4],
    rect: (f32, f32, f32, f32),
) {
    let (program, u_mvp, u_color, a_pos) = prog;
    // Smithay frames draw with the scissor test on; chrome must not be
    // clipped by whatever rect the frame engine left behind.
    gl.Disable(ffi::SCISSOR_TEST);
    gl.UseProgram(program);
    gl.UniformMatrix4fv(u_mvp, 1, ffi::FALSE as u8, mvp.as_ptr());
    gl.Uniform4f(u_color, color[0], color[1], color[2], color[3]);
    let (x, y, w, h) = rect;
    let verts: [f32; 8] = [x, y, x + w, y, x, y + h, x + w, y + h];
    gl.EnableVertexAttribArray(a_pos);
    gl.VertexAttribPointer(a_pos, 2, ffi::FLOAT, ffi::FALSE as u8, 0, verts.as_ptr() as *const _);
    gl.DrawArrays(ffi::TRIANGLE_STRIP, 0, 4);
    gl.DisableVertexAttribArray(a_pos);
    gl.UseProgram(0);
    gl.Enable(ffi::SCISSOR_TEST);
}

/// Compile the textured program: (program, u_mvp, u_tex, u_opaque, a_pos, a_uv).
#[cfg(feature = "compositor")]
pub fn compile_textured(gl: &ffi::Gles2) -> Result<(u32, i32, i32, i32, u32, u32), String> {
    let c = |s: &str| CString::new(s).unwrap();
    unsafe {
        let vert = compile(gl, ffi::VERTEX_SHADER, VERT_TEX_SRC)?;
        let frag = compile(gl, ffi::FRAGMENT_SHADER, FRAG_TEX_SRC)?;
        let program = gl.CreateProgram();
        gl.AttachShader(program, vert);
        gl.AttachShader(program, frag);
        gl.LinkProgram(program);
        gl.DetachShader(program, vert);
        gl.DetachShader(program, frag);
        gl.DeleteShader(vert);
        gl.DeleteShader(frag);
        let mut status = ffi::FALSE as i32;
        gl.GetProgramiv(program, ffi::LINK_STATUS, &mut status as *mut _);
        if status == ffi::FALSE as i32 {
            gl.DeleteProgram(program);
            return Err("textured program link failed".into());
        }
        let mvp = gl.GetUniformLocation(program, c("mvp").as_ptr() as *const _);
        let tex = gl.GetUniformLocation(program, c("tex").as_ptr() as *const _);
        let opaque = gl.GetUniformLocation(program, c("opaque").as_ptr() as *const _);
        let pos = gl.GetAttribLocation(program, c("position").as_ptr() as *const _) as u32;
        let uv = gl.GetAttribLocation(program, c("uv").as_ptr() as *const _) as u32;
        Ok((program, mvp, tex, opaque, pos, uv))
    }
}

/// Draw one textured quad: `verts` are (x, y, u, v) per corner, triangle
/// strip order TL, TR, BL, BR. Y in MVP units (caller negates canvas y).
/// Same blend contract as Smithay's own draws (premultiplied textures);
/// `opaque` forces alpha to 1 for X-format buffers whose X byte is not
/// real alpha (see `shm_format_is_opaque`).
#[cfg(feature = "compositor")]
pub unsafe fn draw_textured(
    gl: &ffi::Gles2,
    prog: (u32, i32, i32, i32, u32, u32),
    mvp: &[f32; 16],
    tex_id: u32,
    verts: &[f32; 16],
    opaque: bool,
) {
    let (program, u_mvp, u_tex, u_opaque, a_pos, a_uv) = prog;
    gl.Disable(ffi::SCISSOR_TEST);
    gl.UseProgram(program);
    gl.UniformMatrix4fv(u_mvp, 1, ffi::FALSE as u8, mvp.as_ptr());
    gl.ActiveTexture(ffi::TEXTURE0);
    gl.BindTexture(ffi::TEXTURE_2D, tex_id);
    gl.Uniform1i(u_tex, 0);
    gl.Uniform1f(u_opaque, if opaque { 1.0 } else { 0.0 });
    // Fresh shm imports default to mipmapped minification (incomplete
    // texture = black); Smithay's own draws set LINEAR every time too.
    gl.TexParameteri(ffi::TEXTURE_2D, ffi::TEXTURE_MIN_FILTER, ffi::LINEAR as i32);
    gl.TexParameteri(ffi::TEXTURE_2D, ffi::TEXTURE_MAG_FILTER, ffi::LINEAR as i32);
    gl.BlendFunc(ffi::ONE, ffi::ONE_MINUS_SRC_ALPHA);
    gl.EnableVertexAttribArray(a_pos);
    gl.EnableVertexAttribArray(a_uv);
    gl.VertexAttribPointer(
        a_pos,
        2,
        ffi::FLOAT,
        ffi::FALSE as u8,
        (4 * std::mem::size_of::<f32>()) as i32,
        verts.as_ptr() as *const _,
    );
    gl.VertexAttribPointer(
        a_uv,
        2,
        ffi::FLOAT,
        ffi::FALSE as u8,
        (4 * std::mem::size_of::<f32>()) as i32,
        verts.as_ptr().add(2) as *const _,
    );
    gl.DrawArrays(ffi::TRIANGLE_STRIP, 0, 4);
    gl.DisableVertexAttribArray(a_uv);
    gl.DisableVertexAttribArray(a_pos);
    gl.BindTexture(ffi::TEXTURE_2D, 0);
    gl.UseProgram(0);
    gl.Enable(ffi::SCISSOR_TEST);
}

/// True when an shm buffer format carries no real alpha: the X byte is
/// whatever the X server left behind (usually 0x00), so sampling it as
/// alpha turns the window transparent wherever nothing opaque was drawn
/// (xterm text floats over the canvas; fresh selections vanish). The
/// textured draw forces alpha to 1 for these; ARGB keeps its alpha.
#[cfg(feature = "compositor")]
pub fn shm_format_is_opaque(
    format: smithay::reexports::wayland_server::protocol::wl_shm::Format,
) -> bool {
    use smithay::reexports::wayland_server::protocol::wl_shm::Format::*;
    matches!(
        format,
        Xrgb8888
            | Xbgr8888
            | Xrgb4444
            | Xbgr4444
            | Xrgb1555
            | Xbgr1555
            | Rgb565
            | Bgr565
            | C8
    )
}

/// Ambient wallpaper: a fullscreen procedural program. Dark slate base,
/// two octaves short of busy: one slow drift plus one self-warped
/// detail layer, vignette shadows toward the edges, darker when zoomed
/// out (matches `background_color`). Amplitude is deliberately tiny:
/// presence, not wallpaper-as-content.
#[cfg(feature = "compositor")]
const VERT_BG_SRC: &str = "#version 100
attribute vec2 position;
varying vec2 v_uv;
void main() { v_uv = position * 0.5 + 0.5; gl_Position = vec4(position, 0.0, 1.0); }
";

#[cfg(feature = "compositor")]
const FRAG_BG_SRC: &str = "#version 100
// highp: the noise hash needs it. mediump mantissas (~10 bit) quantise
// the hash products to whole integers, whose fract is 0 everywhere —
// the failure mode is a flat black field. If a GPU lacks highp in the
// fragment stage, compile fails and the backend falls back to flat
// clear (see bg_failed), never to a broken frame.
precision highp float;
uniform vec2 res;
uniform float t;
uniform float zoom;
uniform float dim;
uniform float halo;
uniform float depth_on;
uniform float bgon;
uniform float bg_pattern;
uniform float bg_scale;
uniform float bg_grain;
uniform float bg_vig;
uniform vec3 bg_tint;
uniform float w_amp;
uniform float w_shape;
uniform float w_oct;
uniform float w_freq;
uniform float w_dir;
uniform float w_crisp;
uniform vec3 w_col;
uniform float wt;
uniform float glow;
uniform vec3 gcol_a;
uniform vec3 gcol_b;
uniform float g_int;
uniform float g_width;
uniform float gt;
varying vec2 v_uv;
// Precision-safe hash (no sin/fract-of-huge: mediump overflows the
// classic sin(dot)*43758 past ~65504 and returns black). Hoskins
// integer-style hash stays in small range on any precision.
float hash(vec2 p) {
    vec3 p3 = fract(vec3(p.xyx) * 0.1031);
    p3 += dot(p3, p3.yzx + 33.33);
    return fract((p3.x + p3.y) * p3.z);
}
float vnoise(vec2 p) {
    vec2 i = floor(p);
    vec2 f = fract(p);
    vec2 u = f * f * (3.0 - 2.0 * f);
    return mix(mix(hash(i), hash(i + vec2(1.0, 0.0)), u.x),
               mix(hash(i + vec2(0.0, 1.0)), hash(i + vec2(1.0, 1.0)), u.x), u.y);
}
float fbm(vec2 p) { return vnoise(p) * 0.55 + vnoise(p * 2.13) * 0.28 + vnoise(p * 4.41) * 0.17; }
void main() {
    float aspect = res.x / res.y;
    vec2 p = vec2(v_uv.x * aspect, v_uv.y);
    vec2 q = p * bg_scale;
    float slow = t * 0.03;
    // Motif: 0 Calme (flat + faint gradient), 1 Dérive (slow warp),
    // 2 Nébuleuse (two-scale clouds). Branching on a uniform is fine.
    float drift_n;
    if (bg_pattern < 0.5) {
        drift_n = (0.5 - v_uv.y) * 0.18 + (fbm(q * 0.8 + slow * 0.2) - 0.5) * 0.30;
    } else if (bg_pattern < 1.5) {
        float warp = fbm(q * 1.1 - slow * 0.3);
        drift_n = fbm(q * 1.6 + vec2(slow * 0.7, slow * 0.4) + warp * 0.6) - 0.5;
    } else {
        float neb = fbm(q * 0.9 + vec2(-slow * 0.4, slow * 0.2));
        float neb2 = fbm(q * 2.3 + vec2(slow * 0.5, -slow * 0.3) + neb);
        drift_n = (neb * 0.6 + neb2 * 0.4 - 0.5) * 1.6;
    }
    float depth = mix(1.0, clamp(0.35 + zoom * 0.65, 0.35, 1.0), depth_on);
    vec3 base = bg_tint * depth;
    vec3 drift_col = bg_tint * 1.2 + vec3(0.008, 0.010, 0.016);
    vec3 col = base + drift_col * drift_n * 0.9 * halo * bgon;
    // Procedural wave field: shaped octaves travelling along w_dir,
    // crest-sharpened, brighter where it crests, breathing with wt.
    if (w_amp > 0.001) {
        vec2 wdir = vec2(cos(w_dir), sin(w_dir));
        float s = dot(p, wdir) * w_freq;
        float acc = 0.0;
        float amp = 1.0;
        float fr = 1.0;
        float norm = 0.0;
        for (int i = 0; i < 4; i++) {
            if (float(i) < w_oct) {
                float ph = s * fr + wt * (0.6 + 0.35 * float(i));
                float o;
                if (w_shape < 0.5) {
                    o = sin(ph);
                } else if (w_shape < 1.5) {
                    o = asin(sin(ph)) * 0.6366;
                } else {
                    o = vnoise(vec2(ph * 0.5, wt * 0.25 + float(i) * 7.3)) * 2.0 - 1.0;
                }
                acc += o * amp;
                norm += amp;
                amp *= 0.55;
                fr *= 2.03;
            }
        }
        float w = acc / max(norm, 0.001);
        w = sign(w) * pow(abs(w), w_crisp);
        vec2 c2 = v_uv - 0.5;
        float wveil = 1.0 - smoothstep(0.0, 0.85, length(c2));
        col += w_col * (w * 0.5 + 0.5) * w_amp * mix(0.35, 1.0, wveil) * bgon;
    }
    // Film grain: per-pixel hash, strength from the menu.
    col += (hash(v_uv * res) - 0.5) * bg_grain * bgon;
    vec2 c = v_uv - 0.5;
    float vig = 1.0 - smoothstep(0.25, 0.95, length(c * vec2(1.0, 0.85)));
    col *= mix(1.0, mix(0.45, 1.0, vig), bg_vig);
    // Super edge glow: aurora curtains, not flames — slow fbm flow
    // hugging the borders, strongest at the rim, breathing with time.
    // Uniform-gated (glow fades 0..1 with Canvas mode); zero cost
    // concern when off: the band term is exactly 0 outside its width.
    vec2 e = min(v_uv, 1.0 - v_uv);
    float rim = min(e.x * aspect, e.y);
    float band = 1.0 - smoothstep(0.0, g_width, rim);
    float flow = gt * 0.05;
    float curtain = fbm(vec2((v_uv.x + v_uv.y) * 2.0 + flow, flow * 0.6));
    float curtain2 = fbm(vec2((v_uv.x - v_uv.y) * 3.0 - flow * 0.7, flow * 0.4 + 7.3));
    float veil = band * (0.30 + 0.70 * curtain * curtain2 * 2.2);
    vec3 gcol = mix(gcol_a, gcol_b, curtain);
    col += gcol * veil * g_int * glow * 0.35;
    gl_FragColor = vec4(col * dim, 1.0);
}
";

/// Compile the background program: uniform locations for the wallpaper
/// (res, t, zoom, dim, halo, depth_on, bgon, pattern, scale, grain,
/// vignette, tint), the procedural wave field (amplitude, shape,
/// octaves, frequency, direction, sharpness, colour, own clock) and
/// the Super edge glow (glow fade, palette, intensity, width, clock).
#[cfg(feature = "compositor")]
#[derive(Debug, Clone, Copy)]
pub struct BgProg {
    pub program: u32,
    pub u_res: i32,
    pub u_t: i32,
    pub u_zoom: i32,
    pub u_dim: i32,
    pub u_halo: i32,
    pub u_depth_on: i32,
    pub u_bgon: i32,
    pub u_bg_pattern: i32,
    pub u_bg_scale: i32,
    pub u_bg_grain: i32,
    pub u_bg_vig: i32,
    pub u_bg_tint: i32,
    pub u_w_amp: i32,
    pub u_w_shape: i32,
    pub u_w_oct: i32,
    pub u_w_freq: i32,
    pub u_w_dir: i32,
    pub u_w_crisp: i32,
    pub u_w_col: i32,
    pub u_wt: i32,
    pub u_glow: i32,
    pub u_gcol_a: i32,
    pub u_gcol_b: i32,
    pub u_g_int: i32,
    pub u_g_width: i32,
    pub u_gt: i32,
    pub a_pos: u32,
}

#[cfg(feature = "compositor")]
pub fn compile_bg(gl: &ffi::Gles2) -> Result<BgProg, String> {
    let c = |s: &str| CString::new(s).unwrap();
    unsafe {
        let vert = compile(gl, ffi::VERTEX_SHADER, VERT_BG_SRC)?;
        let frag = compile(gl, ffi::FRAGMENT_SHADER, FRAG_BG_SRC)?;
        let program = gl.CreateProgram();
        gl.AttachShader(program, vert);
        gl.AttachShader(program, frag);
        gl.LinkProgram(program);
        gl.DetachShader(program, vert);
        gl.DetachShader(program, frag);
        gl.DeleteShader(vert);
        gl.DeleteShader(frag);
        let mut status = ffi::FALSE as i32;
        gl.GetProgramiv(program, ffi::LINK_STATUS, &mut status as *mut _);
        if status == ffi::FALSE as i32 {
            gl.DeleteProgram(program);
            return Err("background program link failed".into());
        }
        let u = |name: &str| gl.GetUniformLocation(program, c(name).as_ptr() as *const _);
        let pos = gl.GetAttribLocation(program, c("position").as_ptr() as *const _) as u32;
        Ok(BgProg {
            program,
            u_res: u("res"),
            u_t: u("t"),
            u_zoom: u("zoom"),
            u_dim: u("dim"),
            u_halo: u("halo"),
            u_depth_on: u("depth_on"),
            u_bgon: u("bgon"),
            u_bg_pattern: u("bg_pattern"),
            u_bg_scale: u("bg_scale"),
            u_bg_grain: u("bg_grain"),
            u_bg_vig: u("bg_vig"),
            u_bg_tint: u("bg_tint"),
            u_w_amp: u("w_amp"),
            u_w_shape: u("w_shape"),
            u_w_oct: u("w_oct"),
            u_w_freq: u("w_freq"),
            u_w_dir: u("w_dir"),
            u_w_crisp: u("w_crisp"),
            u_w_col: u("w_col"),
            u_wt: u("wt"),
            u_glow: u("glow"),
            u_gcol_a: u("gcol_a"),
            u_gcol_b: u("gcol_b"),
            u_g_int: u("g_int"),
            u_g_width: u("g_width"),
            u_gt: u("gt"),
            a_pos: pos,
        })
    }
}

/// Draw the ambient background over the whole viewport (clip-space quad,
/// opaque). `t` is wallpaper seconds * speed (frozen in eco), `halo`
/// the drift brightness, `depth_on` the zoom relief, `bgon` the
/// wallpaper master (0 = flat slate, glow still works), then the motif
/// block (pattern, scale, grain, vignette, tint), the wave block
/// (amplitude gating the field, shape, octaves, frequency, direction,
/// sharpness, colour, own clock `wt`), then the Super glow block.
#[cfg(feature = "compositor")]
pub unsafe fn draw_bg(
    gl: &ffi::Gles2,
    prog: BgProg,
    w_px: f32,
    h_px: f32,
    t: f32,
    zoom: f32,
    dim: f32,
    halo: f32,
    depth_on: f32,
    bgon: f32,
    bg_pattern: f32,
    bg_scale: f32,
    bg_grain: f32,
    bg_vig: f32,
    bg_tint: [f32; 3],
    w_amp: f32,
    w_shape: f32,
    w_oct: f32,
    w_freq: f32,
    w_dir: f32,
    w_crisp: f32,
    w_col: [f32; 3],
    wt: f32,
    glow: f32,
    gcol_a: [f32; 3],
    gcol_b: [f32; 3],
    g_int: f32,
    g_width: f32,
    gt: f32,
) {
    let p = prog;
    gl.Disable(ffi::SCISSOR_TEST);
    gl.UseProgram(p.program);
    gl.Uniform2f(p.u_res, w_px, h_px);
    gl.Uniform1f(p.u_t, t);
    gl.Uniform1f(p.u_zoom, zoom);
    gl.Uniform1f(p.u_dim, dim);
    gl.Uniform1f(p.u_halo, halo);
    gl.Uniform1f(p.u_depth_on, depth_on);
    gl.Uniform1f(p.u_bgon, bgon);
    gl.Uniform1f(p.u_bg_pattern, bg_pattern);
    gl.Uniform1f(p.u_bg_scale, bg_scale);
    gl.Uniform1f(p.u_bg_grain, bg_grain);
    gl.Uniform1f(p.u_bg_vig, bg_vig);
    gl.Uniform3f(p.u_bg_tint, bg_tint[0], bg_tint[1], bg_tint[2]);
    gl.Uniform1f(p.u_w_amp, w_amp);
    gl.Uniform1f(p.u_w_shape, w_shape);
    gl.Uniform1f(p.u_w_oct, w_oct);
    gl.Uniform1f(p.u_w_freq, w_freq);
    gl.Uniform1f(p.u_w_dir, w_dir);
    gl.Uniform1f(p.u_w_crisp, w_crisp);
    gl.Uniform3f(p.u_w_col, w_col[0], w_col[1], w_col[2]);
    gl.Uniform1f(p.u_wt, wt);
    gl.Uniform1f(p.u_glow, glow);
    gl.Uniform3f(p.u_gcol_a, gcol_a[0], gcol_a[1], gcol_a[2]);
    gl.Uniform3f(p.u_gcol_b, gcol_b[0], gcol_b[1], gcol_b[2]);
    gl.Uniform1f(p.u_g_int, g_int);
    gl.Uniform1f(p.u_g_width, g_width);
    gl.Uniform1f(p.u_gt, gt);
    // Fullscreen triangle strip in clip space.
    let verts: [f32; 8] = [-1.0, -1.0, 1.0, -1.0, -1.0, 1.0, 1.0, 1.0];
    gl.EnableVertexAttribArray(p.a_pos);
    gl.VertexAttribPointer(p.a_pos, 2, ffi::FLOAT, ffi::FALSE as u8, 0, verts.as_ptr() as *const _);
    gl.DrawArrays(ffi::TRIANGLE_STRIP, 0, 4);
    gl.DisableVertexAttribArray(p.a_pos);
    gl.UseProgram(0);
    gl.Enable(ffi::SCISSOR_TEST);
}

/// Glyph atlas: 16x8 cells of 8x8 px (128x64 RGBA, white premultiplied).
/// One upload, then every menu char is a UV sub-rect through the normal
/// textured program.
#[cfg(feature = "compositor")]
pub const ATLAS_W: u32 = 128;
#[cfg(feature = "compositor")]
pub const ATLAS_H: u32 = 64;

/// Build atlas bytes (row-major RGBA) from the vendored 8x8 font.
#[cfg(feature = "compositor")]
pub fn build_glyph_atlas() -> Vec<u8> {
    let mut px = vec![0u8; (ATLAS_W * ATLAS_H * 4) as usize];
    for byte in 0..128u8 {
        let rows = crate::font8x8::glyph(byte);
        let col = (byte % 16) as u32;
        let row = (byte / 16) as u32;
        for (ry, bits) in rows.iter().enumerate() {
            for rx in 0..8u32 {
                if bits & (1 << rx) != 0 {
                    let x = col * 8 + rx;
                    let y = row * 8 + ry as u32;
                    let o = ((y * ATLAS_W + x) * 4) as usize;
                    px[o] = 255;
                    px[o + 1] = 255;
                    px[o + 2] = 255;
                    px[o + 3] = 255;
                }
            }
        }
    }
    px
}

/// UV sub-rect (u0, v0, u1, v1) of one atlas cell. V points down the
/// uploaded rows; the backend flips to upright UVs when drawing.
#[cfg(feature = "compositor")]
pub fn glyph_uv(byte: u8) -> (f32, f32, f32, f32) {
    let b = (byte & 0x7F) as f32;
    let col = b % 16.0;
    let row = (b / 16.0).floor();
    (
        col * 8.0 / ATLAS_W as f32,
        row * 8.0 / ATLAS_H as f32,
        (col * 8.0 + 8.0) / ATLAS_W as f32,
        (row * 8.0 + 8.0) / ATLAS_H as f32,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(m: &[f32; 16], x: f32, y: f32) -> (f32, f32) {
        // Column-major multiply of (x, y, 0, 1).
        let clip_x = m[0] * x + m[4] * y + m[12];
        let clip_y = m[1] * x + m[5] * y + m[13];
        (clip_x, clip_y)
    }

    #[test]
    fn ortho_maps_corners_to_clip() {
        let m = ortho(1280.0, 800.0);
        let (x0, y0) = apply(&m, 0.0, 0.0);
        let (x1, y1) = apply(&m, 1280.0, 800.0);
        let (xc, yc) = apply(&m, 640.0, 400.0);
        assert!((x0 + 1.0).abs() < 1e-5 && (y0 - 1.0).abs() < 1e-5);
        assert!((x1 - 1.0).abs() < 1e-5 && (y1 + 1.0).abs() < 1e-5);
        assert!(xc.abs() < 1e-5 && yc.abs() < 1e-5);
    }

    #[test]
    fn perspective_near_plane_centre() {
        // fov 90, square aspect: x=1,z=-1 on the near plane -> right edge.
        let m = perspective(std::f32::consts::FRAC_PI_2, 1.0, 1.0, 100.0);
        let clip_x = m[0] * 1.0 + m[8] * -1.0;
        let w = m[11] * -1.0;
        assert!(((clip_x / w) - 1.0).abs() < 1e-5);
    }

    fn project(m: &[f32; 16], x: f32, y: f32, w: f32, h: f32) -> (f32, f32) {
        // Canvas (x, y) with y down; geometry negated like the backend.
        let (gx, gy) = (x, -y);
        let cx = m[0] * gx + m[4] * gy + m[12];
        let cy = m[1] * gx + m[5] * gy + m[13];
        let cw = m[3] * gx + m[7] * gy + m[15];
        ((cx / cw * 0.5 + 0.5) * w, (0.5 - cy / cw * 0.5) * h)
    }

    #[test]
    fn canvas_mvp_flat_matches_screen_pixels() {
        // Zero tilt through the perspective camera lands on the same
        // pixels as the flat orthographic mapping.
        let m = canvas_mvp((1280.0, 800.0), (640.0, 400.0), 1.0, (0.0, 0.0));
        for (x, y) in [(0.0, 0.0), (1280.0, 800.0), (640.0, 400.0), (100.0, 700.0)] {
            let (px, py) = project(&m, x, y, 1280.0, 800.0);
            assert!((px - x).abs() < 0.5, "x {} -> {}", x, px);
            assert!((py - y).abs() < 0.5, "y {} -> {}", y, py);
        }
    }

    #[test]
    fn canvas_mvp_tilt_pivots_about_center() {
        // A tilt keeps the view center fixed and visibly moves corners.
        let flat = canvas_mvp((1280.0, 800.0), (640.0, 400.0), 1.0, (0.0, 0.0));
        let tilt = canvas_mvp((1280.0, 800.0), (640.0, 400.0), 1.0, (0.14, 0.07));
        let (cx, cy) = project(&tilt, 640.0, 400.0, 1280.0, 800.0);
        assert!((cx - 640.0).abs() < 0.5 && (cy - 400.0).abs() < 0.5);
        let (fx, fy) = project(&flat, 0.0, 0.0, 1280.0, 800.0);
        let (tx, ty) = project(&tilt, 0.0, 0.0, 1280.0, 800.0);
        assert!((tx - fx).abs() + (ty - fy).abs() > 1.0);
    }

    #[cfg(feature = "compositor")]
    #[test]
    fn x_formats_are_opaque_argb_is_not() {
        use smithay::reexports::wayland_server::protocol::wl_shm::Format::*;
        assert!(shm_format_is_opaque(Xrgb8888));
        assert!(shm_format_is_opaque(Xbgr8888));
        assert!(shm_format_is_opaque(Rgb565));
        assert!(!shm_format_is_opaque(Argb8888));
        assert!(!shm_format_is_opaque(Abgr8888));
    }

    #[cfg(feature = "compositor")]
    #[test]
    fn atlas_holds_lit_a_and_blank_space() {
        let px = build_glyph_atlas();
        assert_eq!(px.len(), (ATLAS_W * ATLAS_H * 4) as usize);
        // 'A' cell (col 1, row 4): first row 0x0C has bits 2,3 lit.
        let cell = |bx: u32, by: u32| ((by * ATLAS_W + bx) * 4) as usize;
        let ax = 1 * 8;
        let ay = 4 * 8;
        assert_eq!(px[cell(ax + 2, ay)], 255);
        assert_eq!(px[cell(ax + 2, ay) + 3], 255);
        assert_eq!(px[cell(ax + 0, ay) + 3], 0);
        // Space cell (col 0, row 2) is fully blank: every texel.
        let sx = 0;
        let sy = 2 * 8;
        let mut lit = Vec::new();
        for yy in 0..8u32 {
            for xx in 0..8u32 {
                let o = cell(sx + xx, sy + yy);
                if px[o] != 0 || px[o + 3] != 0 {
                    lit.push((xx, yy, px[o], px[o + 3]));
                }
            }
        }
        assert!(lit.is_empty(), "space cell not blank: {:?}", lit);
        // UVs stay inside the atlas.
        let (u0, v0, u1, v1) = glyph_uv(b'A');
        assert!(0.0 <= u0 && u0 < u1 && u1 <= 1.0);
        assert!(0.0 <= v0 && v0 < v1 && v1 <= 1.0);
    }
}

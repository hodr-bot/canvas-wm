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
    float slow = t * 0.03;
    float warp = fbm(p * 1.1 - slow * 0.3);
    float n = fbm(p * 1.6 + vec2(slow * 0.7, slow * 0.4) + warp * 0.6);
    float depth = clamp(0.35 + zoom * 0.65, 0.35, 1.0);
    vec3 base = vec3(0.060, 0.070, 0.100) * depth;
    vec3 glow = vec3(0.070, 0.090, 0.140) * (n - 0.5) * 0.9;
    vec2 c = v_uv - 0.5;
    float vig = 1.0 - smoothstep(0.25, 0.95, length(c * vec2(1.0, 0.85)));
    vec3 col = (base + glow) * mix(0.45, 1.0, vig);
    gl_FragColor = vec4(col * dim, 1.0);
}
";

/// Compile the background program: (program, u_res, u_t, u_zoom, u_dim, a_pos).
#[cfg(feature = "compositor")]
pub fn compile_bg(gl: &ffi::Gles2) -> Result<(u32, i32, i32, i32, i32, u32), String> {
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
        let res = gl.GetUniformLocation(program, c("res").as_ptr() as *const _);
        let t = gl.GetUniformLocation(program, c("t").as_ptr() as *const _);
        let zoom = gl.GetUniformLocation(program, c("zoom").as_ptr() as *const _);
        let dim = gl.GetUniformLocation(program, c("dim").as_ptr() as *const _);
        let pos = gl.GetAttribLocation(program, c("position").as_ptr() as *const _) as u32;
        Ok((program, res, t, zoom, dim, pos))
    }
}

/// Draw the ambient background over the whole viewport (clip-space quad,
/// opaque). `t` is seconds * speed (frozen in eco), `zoom` the camera
/// zoom for depth, `dim` the eco dimmer.
#[cfg(feature = "compositor")]
pub unsafe fn draw_bg(
    gl: &ffi::Gles2,
    prog: (u32, i32, i32, i32, i32, u32),
    w_px: f32,
    h_px: f32,
    t: f32,
    zoom: f32,
    dim: f32,
) {
    let (program, u_res, u_t, u_zoom, u_dim, a_pos) = prog;
    gl.Disable(ffi::SCISSOR_TEST);
    gl.UseProgram(program);
    gl.Uniform2f(u_res, w_px, h_px);
    gl.Uniform1f(u_t, t);
    gl.Uniform1f(u_zoom, zoom);
    gl.Uniform1f(u_dim, dim);
    // Fullscreen triangle strip in clip space.
    let verts: [f32; 8] = [-1.0, -1.0, 1.0, -1.0, -1.0, 1.0, 1.0, 1.0];
    gl.EnableVertexAttribArray(a_pos);
    gl.VertexAttribPointer(a_pos, 2, ffi::FLOAT, ffi::FALSE as u8, 0, verts.as_ptr() as *const _);
    gl.DrawArrays(ffi::TRIANGLE_STRIP, 0, 4);
    gl.DisableVertexAttribArray(a_pos);
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
        // Space cell (col 0, row 2) is fully blank.
        let sx = 0;
        let sy = 2 * 8;
        assert!(px[cell(sx, sy)..cell(sx, sy) + 4].iter().all(|&v| v == 0));
        // UVs stay inside the atlas.
        let (u0, v0, u1, v1) = glyph_uv(b'A');
        assert!(0.0 <= u0 && u0 < u1 && u1 <= 1.0);
        assert!(0.0 <= v0 && v0 < v1 && v1 <= 1.0);
    }
}

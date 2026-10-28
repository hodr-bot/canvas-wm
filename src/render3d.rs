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

/// Column-major perspective matrix (fov in radians, y down like ortho —
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
}

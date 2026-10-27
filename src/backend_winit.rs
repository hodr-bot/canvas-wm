//! Winit dev backend (feature `compositor` only).
//!
//! Opens a window titled `canvas-wm`, renders the canvas background color
//! derived from the camera, and pumps Smithay input events. Keysym mapping
//! (Super-tap) and session wiring land next; this proves the window,
//! renderer, and event-pump path end to end.

use std::error::Error;
use std::time::Duration;

use smithay::backend::{
    input::{
        ButtonState, Event, InputBackend, InputEvent, KeyState, KeyboardKeyEvent,
        MouseButton, PointerButtonEvent, PointerMotionAbsoluteEvent,
    },
    renderer::{Frame, ImportShm, Texture, Transform},
    winit::{self, WinitEvent, WinitInputError},
};
use smithay::utils::{Buffer as BufferCoord, Physical, Rectangle};
use smithay::wayland::compositor::{with_states, BufferAssignment, SurfaceAttributes};
use smithay::wayland::shm::with_buffer_contents;

use crate::canvas::{window_screen_rect, Camera};
use crate::keys::{is_super, KeyMapper, TapTracker, KEY_Escape};
use crate::server::{Server, ServerState};
use crate::session::{Session, SuperEvent};

/// Background color of the canvas, derived from the camera.
/// Dark slate that deepens as you zoom out, so overview reads as a void.
pub fn background_color(camera: &Camera) -> [f32; 4] {
    let t = ((camera.zoom - 0.05) / 1.95).clamp(0.0, 1.0) as f32;
    let base = 0.04 + 0.05 * t;
    [base * 0.9, base, base * 1.3, 1.0]
}

/// Open the dev window and run until closed or `max_frames` is reached.
/// `max_frames` exists for headless smoke tests (e.g. under Xvfb).
pub fn run(max_frames: Option<u64>) -> Result<(), Box<dyn Error>> {
    let (mut graphics, mut input) = winit::init(None)?;
    graphics.window().set_title("canvas-wm");

    let mut mapper = KeyMapper::new().ok_or("xkb keymap unavailable (missing xkb-data?)")?;
    let mut session = Session::new();
    let mut tap = TapTracker::default();
    // Client windows land here as xdg-toplevels connect.
    let mut server_state = ServerState::default();
    let (mut server, socket) = Server::new().map_err(|e| format!("wayland bind: {}", e))?;
    log::info!("wayland socket ready on {:?}", socket);
    // Logical size of the winit window; replaced by real output sizes on udev.
    let view = crate::canvas::Vec2::new(1280.0, 800.0);
    let mut pointer = crate::canvas::Vec2::new(640.0, 400.0);
    let mut frames: u64 = 0;

    loop {
        server.pump(&mut server_state);
        let mut need_render = false;
        match input.dispatch_new_events(|event| match event {
            InputEvent::Keyboard { event } => {
                let pressed = matches!(event.state(), KeyState::Pressed);
                let sym = mapper.keysym(event.key_code(), pressed);
                if sym == KEY_Escape && pressed {
                    session.on_event(
                        SuperEvent::Esc,
                        &|wid| server_state.windows.get(&wid).map(|c| c.canvas),
                        view,
                    );
                    need_render = true;
                } else if is_super(sym) {
                    if pressed {
                        tap.note_super_press();
                    } else if tap.note_super_release() {
                        session.on_event(
                            SuperEvent::SuperTap,
                            &|wid| server_state.windows.get(&wid).map(|c| c.canvas),
                            view,
                        );
                        need_render = true;
                    }
                } else if pressed {
                    tap.note_other_press();
                }
            }
            InputEvent::PointerMotionAbsolute { event } => {
                pointer = crate::canvas::Vec2::new(event.x(), event.y());
            }
            InputEvent::PointerButton { event } => {
                if event.state() == ButtonState::Pressed
                    && event.button() == MouseButton::Left
                    && session.state == crate::session::SuperState::Overview
                {
                    let world = session.camera.screen_to_world(pointer, view);
                    let hit = server_state
                        .windows
                        .iter()
                        .find(|(_, c)| c.canvas.contains(world))
                        .map(|(id, _)| *id);
                    match hit {
                        Some(id) => session.on_event(
                            SuperEvent::WindowClick(id),
                            &|wid| server_state.windows.get(&wid).map(|c| c.canvas),
                            view,
                        ),
                        None => session.on_event(
                            SuperEvent::CanvasClick,
                            &|wid| server_state.windows.get(&wid).map(|c| c.canvas),
                            view,
                        ),
                    };
                    need_render = true;
                }
            }
            InputEvent::PointerAxis { event } => {
                log::debug!("axis time={}", event.time());
            }
            InputEvent::Special(evt) => match evt {
                WinitEvent::Refresh => need_render = true,
                WinitEvent::Resized { .. } => need_render = true,
                WinitEvent::Focus(focused) => {
                    log::info!("focus {}", focused);
                }
            },
            _ => {}
        }) {
            Ok(()) => {}
            Err(WinitInputError::WindowClosed) => {
                log::info!("window closed after {} frames", frames);
                break;
            }
        }

        if need_render || frames == 0 || max_frames.is_some() {
            let scale = graphics.window_size().scale_factor;
            let color = background_color(&session.camera);
            graphics.render(|renderer, frame| {
                if let Err(err) = frame.clear(color) {
                    log::error!("clear failed: {}", err);
                }
                for (id, cw) in server_state.windows.iter_mut() {
                    // Import freshly committed shm buffers.
                    if let Some(surface) = cw.surface.as_ref() {
                        let assignment = with_states(surface, |states| {
                            states
                                .cached_state
                                .current::<SurfaceAttributes>()
                                .buffer
                                .take()
                        });
                        if let Ok(Some(BufferAssignment::NewBuffer { buffer, .. })) = assignment {
                            let dims = with_buffer_contents(&buffer, |_, data| {
                                (data.width, data.height)
                            });
                            if let Ok((w, h)) = dims {
                                let damage = Rectangle::<i32, BufferCoord>::from_loc_and_size(
                                    (0, 0),
                                    (w, h),
                                );
                                match renderer.import_shm_buffer(&buffer, None, &[damage]) {
                                    Ok(tex) => {
                                        log::info!(
                                            "imported {}x{} shm buffer for window {}",
                                            w,
                                            h,
                                            id
                                        );
                                        cw.texture = Some(tex)
                                    }
                                    Err(err) => log::warn!("shm import failed: {}", err),
                                }
                            }
                        }
                    }
                    // Draw through the canvas camera.
                    if let Some(tex) = cw.texture.as_ref() {
                        let (x, y, w, h) =
                            window_screen_rect(&session.camera, &cw.canvas, view, scale);
                        let dest =
                            Rectangle::<f64, Physical>::from_loc_and_size((x, y), (w, h));
                        let src = Rectangle::<i32, BufferCoord>::from_loc_and_size(
                            (0, 0),
                            tex.size(),
                        );
                        if let Err(err) = frame.render_texture_from_to(
                            tex,
                            src,
                            dest,
                            Transform::Normal,
                            1.0,
                        ) {
                            log::warn!("draw failed: {}", err);
                        }
                    }
                }
            })?;
            frames += 1;
            if let Some(max) = max_frames {
                if frames >= max {
                    log::info!("frame budget reached ({}), exiting", frames);
                    break;
                }
            }
        }

        std::thread::sleep(Duration::from_millis(8));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn background_deepens_when_zoomed_out() {
        let near = Camera {
            zoom: 1.5,
            ..Camera::new()
        };
        let far = Camera {
            zoom: 0.1,
            ..Camera::new()
        };
        let c_near = background_color(&near);
        let c_far = background_color(&far);
        assert!(c_far[0] < c_near[0]);
        assert!(c_far[1] < c_near[1]);
        for c in [c_near, c_far] {
            assert_eq!(c[3], 1.0);
            assert!(c[0] >= 0.0 && c[1] <= 0.2);
        }
    }
}

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
        Axis, AxisSource as InputAxisSource, ButtonState, Event, InputBackend, InputEvent,
        KeyState, KeyboardKeyEvent, MouseButton, PointerAxisEvent, PointerButtonEvent,
        PointerMotionAbsoluteEvent,
    },
    renderer::{Frame, ImportShm, Texture, Transform},
    winit::{self, WinitEvent, WinitInputError},
};
use smithay::reexports::wayland_server::protocol::wl_pointer::{Axis as WlAxis, AxisSource};
use smithay::utils::{Buffer as BufferCoord, Physical, Rectangle};
use smithay::wayland::compositor::{with_states, BufferAssignment, SurfaceAttributes};
use smithay::wayland::seat::AxisFrame;
use smithay::wayland::shm::with_buffer_contents;

use crate::canvas::{window_screen_rect, Camera};
use crate::keys::{is_super, KeyMapper, TapTracker, KEY_Escape};
use crate::server::{Server, ServerState};
use crate::session::{Session, SuperEvent};
use crate::{bar, bar_client::BarClient};
use crate::bar::BAR_HEIGHT;

/// winit mouse buttons to evdev codes clients expect.
fn button_code(button: MouseButton) -> u32 {
    match button {
        MouseButton::Left => 0x110,
        MouseButton::Right => 0x111,
        MouseButton::Middle => 0x112,
        MouseButton::Other(code) => 0x110 + code as u32,
    }
}

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
    // Window ids the seat already knows about; a change re-asserts focus so
    // late-bound clients receive keyboard Enter on the newest window.
    let mut known_ids: std::collections::HashSet<u64> = std::collections::HashSet::new();
    // Focused canvas window (chip highlight) and last bar state sent.
    let mut focused_id: Option<u64> = None;
    let mut last_bar: (usize, Option<usize>) = (usize::MAX, None);

    // The internal bar maps first, before any external client can connect.
    // Tag window 0 as the screen-space overlay once the server sees it.
    let mut bar = BarClient::connect(&socket, view.x as i32)
        .map_err(|e| format!("bar client: {}", e))?;
    for _ in 0..500 {
        bar.progress();
        server.pump(&mut server_state);
        bar.pump();
        if server_state.windows.contains_key(&0) {
            server_state.set_overlay(0);
            break;
        }
    }
    if !server_state
        .windows
        .get(&0)
        .map(|c| c.overlay)
        .unwrap_or(false)
    {
        log::warn!("bar did not map; continuing without overlay");
    }

    loop {
        server.pump(&mut server_state);
        // Newest canvas window steals keyboard focus, like any floating WM.
        let ids: std::collections::HashSet<u64> = server_state.windows.keys().copied().collect();
        if ids != known_ids {
            known_ids = ids;
            let newest = server_state.canvas_ids().iter().copied().max();
            let surface = newest
                .and_then(|id| server_state.windows.get(&id))
                .and_then(|c| c.surface.clone());
            // Bounce through None so Enter is re-sent even when the
            // surface is unchanged (clients that bound late missed it).
            server.focus_keyboard(None);
            server.focus_keyboard(surface.as_ref());
            focused_id = newest;
        }
        let mut need_render = false;
        match input.dispatch_new_events(|event| match event {
            InputEvent::Keyboard { event } => {
                let pressed = matches!(event.state(), KeyState::Pressed);
                let (sym, evdev) = mapper.resolve(event.key_code(), pressed);
                log::debug!(
                    "key scancode={} sym={} evdev={} pressed={}",
                    event.key_code(),
                    sym,
                    evdev,
                    pressed
                );
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
                } else {
                    if pressed {
                        tap.note_other_press();
                    }
                    server.input_key(evdev, pressed);
                }
            }
            InputEvent::PointerMotionAbsolute { event } => {
                let scale = graphics.window_size().scale_factor;
                let logical = crate::canvas::Vec2::new(event.x() / scale, event.y() / scale);
                pointer = logical;
                // The bar captures the pointer: no client focus above it.
                if bar::in_bar(logical) {
                    server.input_motion((logical.x, logical.y), None);
                    return;
                }
                let world = session.camera.screen_to_world(logical, view);
                log::debug!(
                    "motion physical=({},{}) scale={} logical=({},{}) world=({},{})",
                    event.x(),
                    event.y(),
                    scale,
                    logical.x,
                    logical.y,
                    world.x,
                    world.y
                );
                let hit = server_state
                    .windows
                    .iter()
                    .find(|(_, c)| !c.overlay && c.canvas.contains(world));
                // Seat offset = surface origin in logical space; Smithay
                // reports enter/motion coords as pointer minus this origin.
                let focus = hit.and_then(|(_, c)| {
                    c.surface.clone().map(|s| {
                        let (ox, oy, _, _) =
                            window_screen_rect(&session.camera, &c.canvas, view, scale);
                        (s, ((ox / scale) as i32, (oy / scale) as i32))
                    })
                });
                server.input_motion((logical.x, logical.y), focus);
            }
            InputEvent::PointerButton { event } => {
                let pressed = event.state() == ButtonState::Pressed;
                // Bar clicks focus a chip's window and never reach clients.
                if pressed && bar::in_bar(pointer) {
                    let canvas = server_state.canvas_ids();
                    if let Some(i) = bar::chip_at(pointer, canvas.len(), view.x) {
                        if let Some(id) = canvas.get(i).copied() {
                            if let Some(c) = server_state.windows.get(&id) {
                                server.focus_keyboard(c.surface.as_ref());
                            }
                            focused_id = Some(id);
                            need_render = true;
                        }
                    }
                } else if pressed
                    && event.button() == MouseButton::Left
                    && session.state == crate::session::SuperState::Overview
                {
                    let world = session.camera.screen_to_world(pointer, view);
                    let hit = server_state
                        .windows
                        .iter()
                        .find(|(_, c)| !c.overlay && c.canvas.contains(world))
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
                } else {
                    // Click focuses the window under the cursor, then the
                    // client receives the button.
                    if pressed {
                        let world = session.camera.screen_to_world(pointer, view);
                        if let Some((id, c)) = server_state
                            .windows
                            .iter()
                            .find(|(_, c)| !c.overlay && c.canvas.contains(world))
                        {
                            let id = *id;
                            server.focus_keyboard(c.surface.as_ref());
                            focused_id = Some(id);
                            known_ids.insert(id);
                        }
                    }
                    server.input_button(button_code(event.button()), pressed);
                }
            }
            InputEvent::PointerAxis { event } => {
                let mut frame =
                    AxisFrame::new(event.time()).source(match event.source() {
                        InputAxisSource::Wheel => AxisSource::Wheel,
                        InputAxisSource::WheelTilt => AxisSource::WheelTilt,
                        InputAxisSource::Finger => AxisSource::Finger,
                        InputAxisSource::Continuous => AxisSource::Continuous,
                    });
                if let Some(value) = event.amount(Axis::Horizontal) {
                    frame = frame.value(WlAxis::HorizontalScroll, value);
                }
                if let Some(value) = event.amount(Axis::Vertical) {
                    frame = frame.value(WlAxis::VerticalScroll, value);
                }
                if let Some(steps) = event.amount_discrete(Axis::Horizontal) {
                    frame = frame.discrete(WlAxis::HorizontalScroll, steps as i32);
                }
                if let Some(steps) = event.amount_discrete(Axis::Vertical) {
                    frame = frame.discrete(WlAxis::VerticalScroll, steps as i32);
                }
                server.input_axis(frame);
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

        // Bar follows the canvas window set; redraw on change.
        bar.pump();
        let canvas = server_state.canvas_ids();
        let focused_chip = focused_id.and_then(|id| canvas.iter().position(|c| *c == id));
        if (canvas.len(), focused_chip) != last_bar {
            last_bar = (canvas.len(), focused_chip);
            log::debug!(
                "bar update n={} focused_chip={:?}",
                canvas.len(),
                focused_chip
            );
            bar.redraw(canvas.len(), focused_chip);
            need_render = true;
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
                    // Draw through the canvas camera, or screen-fixed for
                    // the overlay bar.
                    if let Some(tex) = cw.texture.as_ref() {
                        let dest = if cw.overlay {
                            Rectangle::<f64, Physical>::from_loc_and_size(
                                (0.0, 0.0),
                                (view.x * scale, BAR_HEIGHT * scale),
                            )
                        } else {
                            let (x, y, w, h) =
                                window_screen_rect(&session.camera, &cw.canvas, view, scale);
                            Rectangle::<f64, Physical>::from_loc_and_size((x, y), (w, h))
                        };
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

//! Winit dev backend (feature `compositor` only).
//!
//! Opens a window titled `canvas-wm`, renders the canvas background color
//! derived from the camera, and pumps Smithay input events. Keysym mapping
//! (Super-tap) and session wiring land next; this proves the window,
//! renderer, and event-pump path end to end.

use std::cell::RefCell;
use std::error::Error;
use std::os::unix::net::UnixStream;
use std::rc::Rc;
use std::time::Duration;

use smithay::backend::{
    input::{
        Axis, AxisSource as InputAxisSource, ButtonState, Event, InputBackend, InputEvent,
        KeyState, KeyboardKeyEvent, MouseButton, PointerAxisEvent, PointerButtonEvent,
        PointerMotionAbsoluteEvent,
    },
    renderer::{buffer_type, BufferType, Frame, ImportShm, Texture, Transform},
    winit::{self, WinitEvent, WinitInputError},
};
use smithay::reexports::calloop::EventLoop;
use smithay::reexports::wayland_server::protocol::wl_pointer::{Axis as WlAxis, AxisSource};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Buffer as BufferCoord, Physical, Rectangle};
use smithay::wayland::compositor::{with_states, BufferAssignment, SurfaceAttributes};
use smithay::wayland::seat::AxisFrame;
use smithay::wayland::shm::with_buffer_contents;
use smithay::xwayland::{XWayland, XWaylandEvent};

use crate::canvas::{window_screen_rect, Camera};
use crate::keys::{is_super, KeyMapper, TapTracker, KEY_Escape};
use crate::server::{Server, ServerState};
use crate::session::{drag_offset, Session, SuperEvent};
use crate::xwm::Xwm;

/// X display sockets before XWayland starts, to spot its display number.
fn x11_sockets() -> Vec<String> {
    std::fs::read_dir("/tmp/.X11-unix")
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default()
}

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
    // Focused canvas window.
    let mut focused_id: Option<u64> = None;
    // Live canvas gestures: left-drag on empty canvas space pans the canvas
    // (Canvas mode only); a dwell-armed window drag moves that window.
    // Press canvas coords + last canvas coords + whether past threshold.
    let mut grab: Option<(u64, crate::canvas::Vec2, crate::canvas::Vec2, bool)> = None;
    let mut panning: Option<crate::canvas::Vec2> = None;
    // Hover chrome: last frame's revealed buttons (window id + layout).
    let mut last_chrome: Option<(u64, crate::chrome::ChromeLayout)> = None;
    // Hover keyboard shortcuts (Canvas mode, or Super held in Idle):
    // held-modifier set, F/Q repeat guards, E grab-move (window id +
    // last canvas pointer), R resize mode. The modifier set (not a
    // counter) stays correct across key repeats: repeats re-insert an
    // already-present code instead of drifting a count upward forever.
    let mut held_mods: std::collections::HashSet<u32> =
        std::collections::HashSet::new();
    let mut super_held = false;
    let mut f_held = false;
    let mut q_held = false;
    let mut e_grab: Option<(u64, crate::canvas::Vec2)> = None;
    let mut r_held = false;
    let mut r_resize: Option<(u64, crate::chrome::Edge, crate::canvas::Vec2)> = None;
    let mut r_swallow_left = false;
    let mut r_swallow_right = false;
    // Solid-colour GL program for window chrome (seed of the 3D engine).
    let mut solid_prog: Option<(u32, i32, i32, u32)> = None;
    // Textured GL program: windows as perspective planes in 3D.
    let mut tex_prog: Option<(u32, i32, i32, i32, u32, u32)> = None;
    // Canvas-mode press point: released without moving = plain canvas click.
    let mut pan_start: Option<crate::canvas::Vec2> = None;

    // calloop loop just for the XWayland source; polled every frame.
    let mut event_loop: EventLoop<()> =
        EventLoop::try_new().map_err(|e| format!("calloop: {}", e))?;
    let x11_ready: Rc<RefCell<Option<UnixStream>>> = Rc::new(RefCell::new(None));
    let (xwayland, xwayland_source) = XWayland::new(event_loop.handle(), server.display(), None);
    {
        let x11_ready = Rc::clone(&x11_ready);
        event_loop
            .handle()
            .insert_source(xwayland_source, move |event, &mut (), _| {
                if let XWaylandEvent::Ready { connection, .. } = event {
                    *x11_ready.borrow_mut() = Some(connection);
                }
            })
            .map_err(|e| format!("xwayland source: {}", e))?;
    }
    let x11_before = x11_sockets();
    xwayland
        .start()
        .map_err(|e| format!("Xwayland spawn: {}", e))?;
    let mut xwm: Option<Xwm> = None;

    // Unmatched X surface links wait here; the surface may be created
    // server-side after the client message arrives.
    let mut pending_x: Vec<(u32, u32)> = Vec::new();
    let frame_clock = std::time::Instant::now();

    loop {
        if let Err(e) = event_loop.dispatch(Some(Duration::from_millis(0)), &mut ()) {
            log::warn!("calloop dispatch: {}", e);
        }
        // XWayland ready => become its window manager (once).
        if xwm.is_none() {
            if let Some(stream) = x11_ready.borrow_mut().take() {
                match Xwm::new(stream) {
                    Ok(wm) => {
                        for sock in x11_sockets() {
                            if !x11_before.contains(&sock) {
                                if let Some(num) = sock.strip_prefix('X') {
                                    let display = format!(":{}", num);
                                    std::env::set_var("DISPLAY", &display);
                                    log::info!("XWayland ready on DISPLAY={}", display);
                                }
                            }
                        }
                        xwm = Some(wm);
                    }
                    Err(e) => log::warn!("Xwm failed: {}", e),
                }
            }
        }
        if let Some(wm) = xwm.as_mut() {
            wm.poll();
        }
        server.pump(&mut server_state);
        // Link X windows to their Wayland surfaces (zero-copy). Fresh links
        // from the WM plus retries; surfaces may lag the client message.
        if let Some(wm) = xwm.as_mut() {
            pending_x.extend(wm.surface_links.drain(..));
            let mut still_pending = Vec::new();
            for (xid, sid) in pending_x.drain(..) {
                match server_state.surface_by_id(sid) {
                    Some(surface) => {
                        if let Some(id) = server_state
                            .windows
                            .iter()
                            .find(|(_, c)| c.x11 == Some(xid))
                            .map(|(id, _)| *id)
                        {
                            server_state.windows.get_mut(&id).unwrap().surface = Some(surface);
                        } else {
                            let id = server_state.add_x11_window(surface, xid);
                            log::info!("linked X window {} -> canvas {}", xid, id);
                        }
                    }
                    None => still_pending.push((xid, sid)),
                }
            }
            pending_x = still_pending;
            // Drop canvas windows whose X window died.
            for (xid, sid) in wm.dead.drain(..) {
                if let Some(id) = server_state
                    .windows
                    .iter()
                    .find(|(_, c)| c.x11 == Some(xid))
                    .map(|(id, _)| *id)
                {
                    server_state.windows.remove(&id);
                }
                if let Some(sid) = sid {
                    server_state.forget_surface(sid);
                }
            }
        }
        // Mirror canvas raises into X stacking order: X button delivery
        // targets the X-topmost window at the pointer, so a canvas raise
        // without its X mirror sends clicks to the wrong client wherever
        // X windows overlap in root space.
        for xid in server_state.take_restacks() {
            if let Some(wm) = xwm.as_ref() {
                wm.restack_above(xid);
            }
        }
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
            // X-backed focus also moves the X input focus.
            if let Some(id) = newest {
                let xid = server_state
                    .windows
                    .get(&id)
                    .and_then(|c| c.x11);
                if let (Some(wm), Some(xid)) = (xwm.as_ref(), xid) {
                    wm.focus(xid);
                    server.focus_keyboard(None);
                }
            }
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
                    let action = session.on_event(
                        SuperEvent::Esc,
                        &|wid| server_state.windows.get(&wid).map(|c| c.canvas),
                        view,
                    );
                    log::info!(
                        "super {:?} state={:?} zoom={:.2} selected={}",
                        action,
                        session.state,
                        session.camera.zoom,
                        session.overview.selected.len()
                    );
                    need_render = true;
                } else if is_super(sym) {
                    if pressed {
                        super_held = true;
                        tap.note_super_press();
                    } else {
                        super_held = false;
                        if tap.note_super_release() {
                            let action = session.on_event(
                                SuperEvent::SuperTap,
                                &|wid| server_state.windows.get(&wid).map(|c| c.canvas),
                                view,
                            );
                            log::info!(
                                "super {:?} state={:?} zoom={:.2} selected={}",
                                action,
                                session.state,
                                session.camera.zoom,
                                session.overview.selected.len()
                            );
                            need_render = true;
                        }
                    }
                } else {
                    if pressed {
                        tap.note_other_press();
                    }
                    // Modifier tracking as a set: key repeats re-insert the
                    // same code (idempotent) instead of drifting a counter
                    // upward and killing every later bare shortcut.
                    if crate::chrome::MOD_CODES.contains(&evdev) {
                        if pressed {
                            held_mods.insert(evdev);
                        } else {
                            held_mods.remove(&evdev);
                        }
                    }
                    // Hover shortcuts (Canvas mode, or Super held in Idle
                    // so hold-Super+key works like a normal WM): F
                    // fullscreen, Q close, E grab-move, R resize mode.
                    // Consumed — never reaches the app.
                    // Idle sends every key to the app untouched, so normal
                    // typing never triggers an action.
                    let mut consumed = false;
                    if session.state == crate::session::SuperState::Canvas || super_held {
                        if let Some(action) =
                            crate::chrome::hover_shortcut(evdev, !held_mods.is_empty())
                        {
                            let cworld = session.to_canvas(pointer, view);
                            let hover = server_state.topmost_at(cworld);
                            match action {
                                crate::chrome::HoverKey::Fullscreen => {
                                    if pressed && !f_held {
                                        if let Some(id) = hover {
                                            let rect = crate::chrome::fullscreen_rect_for(
                                                view,
                                                &|s| session.to_canvas(s, view),
                                            );
                                            let on =
                                                server_state.toggle_fullscreen(id, &rect);
                                            if let Some(xid) = server_state
                                                .windows
                                                .get(&id)
                                                .and_then(|c| c.x11)
                                            {
                                                let r = server_state.windows[&id].canvas;
                                                if let Some(wm) = xwm.as_ref() {
                                                    wm.configure(
                                                        xid,
                                                        r.size.x as u32,
                                                        r.size.y as u32,
                                                    );
                                                }
                                            }
                                            log::info!(
                                                "hover key fullscreen window {} on={}",
                                                id,
                                                on
                                            );
                                            need_render = true;
                                        }
                                    }
                                    f_held = pressed;
                                    consumed = true;
                                }
                                crate::chrome::HoverKey::Close => {
                                    if pressed && !q_held {
                                        if let Some(id) = hover {
                                            log::info!("hover key close window {}", id);
                                            if let Some(xid) = server_state.close_window(id) {
                                                if let Some(wm) = xwm.as_ref() {
                                                    wm.kill(xid);
                                                }
                                            }
                                            if focused_id == Some(id) {
                                                focused_id = None;
                                            }
                                            need_render = true;
                                        }
                                    }
                                    q_held = pressed;
                                    consumed = true;
                                }
                                crate::chrome::HoverKey::Grab => {
                                    if pressed {
                                        e_grab = hover.map(|id| (id, cworld));
                                        if e_grab.is_some() {
                                            log::info!("hover key grab window {:?}", hover);
                                        }
                                    } else {
                                        e_grab = None;
                                    }
                                    consumed = true;
                                }
                                crate::chrome::HoverKey::Resize => {
                                    r_held = pressed;
                                    if !pressed {
                                        r_resize = None;
                                    }
                                    consumed = true;
                                }
                            }
                        }
                    }
                    if consumed {
                        return;
                    }
                    // Canvas mode is command mode: presses never reach the
                    // apps (Super/Esc/hover-shortcuts act there instead).
                    // Releases still go through so a key held across the
                    // toggle never sticks down in the app.
                    if session.state == crate::session::SuperState::Canvas {
                        if !pressed {
                            let xid = focused_id
                                .and_then(|id| server_state.windows.get(&id))
                                .and_then(|c| c.x11);
                            match (xwm.as_ref(), xid) {
                                (Some(wm), Some(xid)) => {
                                    wm.xtest_key(xid, evdev, false)
                                }
                                _ => server.input_key(evdev, false),
                            }
                        }
                        return;
                    }
                    // X-backed focus eats keys via XTEST, not the wl seat.
                    let xid = focused_id
                        .and_then(|id| server_state.windows.get(&id))
                        .and_then(|c| c.x11);
                    match (xwm.as_ref(), xid) {
                        (Some(wm), Some(xid)) => wm.xtest_key(xid, evdev, pressed),
                        _ => server.input_key(evdev, pressed),
                    }
                }
            }
            InputEvent::PointerMotionAbsolute { event } => {
                let scale = graphics.window_size().scale_factor;
                let logical = crate::canvas::Vec2::new(event.x() / scale, event.y() / scale);
                pointer = logical;
                // Live gestures before picking, in canvas-plane coordinates.
                // Canvas pan: content follows the cursor, velocity-ready.
                if let Some(last) = panning {
                    let dx = pointer.x - last.x;
                    let dy = pointer.y - last.y;
                    session.pan_canvas(crate::canvas::Vec2::new(dx, dy));
                    panning = Some(pointer);
                    need_render = true;
                }
                if let Some((id, press_world, last_world, moved)) = grab {
                    let world_now = session.to_canvas(pointer, view);
                    // Past the threshold, every motion moves the window by its
                    // delta — including the crossing event itself, so even a
                    // single-jump drag (press, one motion, release) moves.
                    if moved || drag_offset(press_world, world_now).is_some() {
                        let dx = world_now.x - last_world.x;
                        let dy = world_now.y - last_world.y;
                        match server_state.windows.get_mut(&id) {
                            Some(cw) => {
                                cw.canvas.pos.x += dx;
                                cw.canvas.pos.y += dy;
                                need_render = true;
                            }
                            None => grab = None,
                        }
                        if grab.is_some() {
                            grab = Some((id, press_world, world_now, true));
                        }
                    }
                }
                // E-grab: while E is held, the window follows the pointer —
                // no button needed, release drops it.
                if let Some((id, last)) = e_grab {
                    let now = session.to_canvas(pointer, view);
                    match server_state.windows.get_mut(&id) {
                        Some(cw) => {
                            cw.canvas.pos.x += now.x - last.x;
                            cw.canvas.pos.y += now.y - last.y;
                            need_render = true;
                            e_grab = Some((id, now));
                        }
                        None => e_grab = None,
                    }
                }
                // R-resize: while R is held, a border drag grows the window.
                if let Some((id, edge, last)) = r_resize {
                    let now = session.to_canvas(pointer, view);
                    match server_state.windows.get_mut(&id) {
                        Some(cw) => {
                            let delta = crate::canvas::Vec2::new(
                                now.x - last.x,
                                now.y - last.y,
                            );
                            cw.canvas =
                                crate::chrome::apply_resize(&cw.canvas, edge, delta);
                            log::debug!(
                                "resize window {} {:?} now {}x{}",
                                id,
                                edge,
                                cw.canvas.size.x,
                                cw.canvas.size.y
                            );
                            need_render = true;
                            r_resize = Some((id, edge, now));
                        }
                        None => r_resize = None,
                    }
                }
                let world = session.to_world(logical, view);
                // Hover chrome reveals on proximity: repaint on motion so
                // buttons appear/disappear with the pointer (Idle only;
                // Canvas mode freezes the apps and their chrome).
                if session.state == crate::session::SuperState::Idle {
                    need_render = true;
                }
                // Hit-test in canvas-plane coordinates (windows live there).
                let cworld = session.to_canvas(logical, view);
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
                let hit_id = server_state.topmost_at(cworld);
                let hit = hit_id.and_then(|id| {
                    server_state.windows.get(&id).map(|c| (id, c))
                });
                // Hyprland-style focus-follows-mouse: the window under the
                // cursor takes keyboard focus (both modes, on change only,
                // never mid-drag). Clicking still raises it to the front.
                // Seat offset = surface origin in logical space; Smithay
                // reports enter/motion coords as pointer minus this origin.
                // X-backed windows additionally get XTEST motion in local px.
                let mut x_motion: Option<(u32, i32, i32)> = None;
                let focus = hit.and_then(|(_, c)| {
                    c.surface.clone().map(|s| {
                        let local = cworld - c.canvas.pos;
                        if let Some(xid) = c.x11 {
                            x_motion = Some((xid, local.x as i32, local.y as i32));
                        }
                        let (ox, oy, _, _) =
                            window_screen_rect(&session.view_camera(), &c.canvas, view, scale);
                        (s, ((ox / scale) as i32, (oy / scale) as i32))
                    })
                });
                server.input_motion((logical.x, logical.y), focus);
                if let (Some(wm), Some((xid, lx, ly))) = (xwm.as_ref(), x_motion) {
                    wm.xtest_motion(xid, lx, ly);
                }
                // Focus follows the pointer (keeps last focus over empty
                // canvas; never steals mid-drag).
                if grab.is_none()
                    && panning.is_none()
                    && e_grab.is_none()
                    && r_resize.is_none()
                {
                    if hit_id != focused_id {
                        if let Some(id) = hit_id {
                            if let Some(c) = server_state.windows.get(&id) {
                                server.focus_keyboard(c.surface.as_ref());
                                if let (Some(wm), Some(xid)) =
                                    (xwm.as_ref(), c.x11)
                                {
                                    wm.focus(xid);
                                    server.focus_keyboard(None);
                                }
                            }
                            focused_id = Some(id);
                            known_ids.insert(id);
                        }
                    }
                }
            }
            InputEvent::PointerButton { event } => {
                let pressed = event.state() == ButtonState::Pressed;
                let left = event.button() == MouseButton::Left;
                // Canvas-mode press starts here; Idle falls through to focus.
                if session.state == crate::session::SuperState::Canvas {
                    // Canvas mode: apps are frozen, nothing reaches clients.
                    // Armed window + press = move it (Hyprland-style, dwell
                    // replaces the held key); empty press starts a canvas pan.
                    if pressed && left {
                        let cw = session.to_canvas(pointer, view);
                        let hit = server_state.topmost_at(cw);
                        match hit {
                            Some(id) if session.dwell.armed == Some(id) => {
                                grab = Some((id, cw, cw, true));
                                server_state.raise(id);
                                log::info!("canvas armed move window {}", id);
                            }
                            Some(id) => {
                                let action = session.on_event(
                                    SuperEvent::WindowClick(id),
                                    &|wid| server_state.windows.get(&wid).map(|c| c.canvas),
                                    view,
                                );
                                log::info!(
                                    "super {:?} click window {} state={:?} zoom={:.2} selected={}",
                                    action,
                                    id,
                                    session.state,
                                    session.camera.zoom,
                                    session.overview.selected.len()
                                );
                            }
                            None => {
                                pan_start = Some(pointer);
                                panning = Some(pointer);
                            }
                        }
                    } else if !pressed && left {
                        grab = None;
                        panning = None;
                        // Press + release without moving = plain canvas click.
                        if let Some(start) = pan_start.take() {
                            if drag_offset(start, pointer).is_none() {
                                let action = session.on_event(
                                    SuperEvent::CanvasClick,
                                    &|wid| server_state.windows.get(&wid).map(|c| c.canvas),
                                    view,
                                );
                                log::info!(
                                    "super {:?} click canvas state={:?} zoom={:.2} selected={}",
                                    action,
                                    session.state,
                                    session.camera.zoom,
                                    session.overview.selected.len()
                                );
                            }
                        }
                    }
                    need_render = true;
                } else {
                    // Idle: click focuses the window, then the client
                    // receives the button. Plain left NEVER moves windows.
                    if pressed {
                        // Window chrome first: a click on a revealed button
                        // acts and never reaches the app.
                        if let Some((chrome_id, layout)) = last_chrome {
                            if let Some(btn) = layout.hit(pointer) {
                                match btn {
                                    crate::chrome::ChromeButton::Close => {
                                        log::info!("chrome close window {}", chrome_id);
                                        if let Some(xid) = server_state.close_window(chrome_id) {
                                            if let Some(wm) = xwm.as_ref() {
                                                wm.kill(xid);
                                            }
                                        }
                                        if focused_id == Some(chrome_id) {
                                            focused_id = None;
                                        }
                                    }
                                    crate::chrome::ChromeButton::Fullscreen => {
                                        let rect = crate::chrome::fullscreen_rect_for(view, &|s| {
                                            session.to_canvas(s, view)
                                        });
                                        let on =
                                            server_state.toggle_fullscreen(chrome_id, &rect);
                                        // True fullscreen: tell the X client its
                                        // new size so the buffer follows.
                                        if let Some(xid) = server_state
                                            .windows
                                            .get(&chrome_id)
                                            .and_then(|c| c.x11)
                                        {
                                            let r =
                                                server_state.windows[&chrome_id].canvas;
                                            if let Some(wm) = xwm.as_ref() {
                                                wm.configure(
                                                    xid,
                                                    r.size.x as u32,
                                                    r.size.y as u32,
                                                );
                                            }
                                        }
                                        log::info!(
                                            "chrome fullscreen window {} on={}",
                                            chrome_id,
                                            on
                                        );
                                    }
                                }
                                need_render = true;
                                last_chrome = None;
                                return;
                            }
                        }
                        let cworld = session.to_canvas(pointer, view);
                        // R resize mode: left/right press on a border starts
                        // a resize drag; every R-held press is swallowed so
                        // the app never sees half of it.
                        if r_held
                            && (event.button() == MouseButton::Left
                                || event.button() == MouseButton::Right)
                        {
                            if let Some(top) = server_state.topmost_at(cworld) {
                                let id = top;
                                if let Some(c) = server_state.windows.get(&id) {
                                    if let Some(edge) = crate::chrome::resize_edge(
                                        &c.canvas,
                                        cworld,
                                        crate::chrome::RESIZE_GRAB,
                                    ) {
                                        r_resize = Some((id, edge, cworld));
                                        log::info!(
                                            "resize start window {} {:?}",
                                            id,
                                            edge
                                        );
                                    }
                                }
                            }
                            match event.button() {
                                MouseButton::Left => r_swallow_left = true,
                                MouseButton::Right => r_swallow_right = true,
                                _ => {}
                            }
                            need_render = true;
                            return;
                        }
                        if let Some(id) = server_state.topmost_at(cworld) {
                            if let Some(c) = server_state.windows.get(&id) {
                                server.focus_keyboard(c.surface.as_ref());
                            }
                            server_state.raise(id);
                            focused_id = Some(id);
                            known_ids.insert(id);
                            // Click diagnostics: which window the pick resolved
                            // to, in canvas-local px and its X id (if any).
                            if let Some(c) = server_state.windows.get(&id) {
                                let local = cworld - c.canvas.pos;
                                log::info!(
                                    "click window {} local=({},{}) x11={:?}",
                                    id,
                                    local.x as i32,
                                    local.y as i32,
                                    c.x11
                                );
                            }
                        }
                    }
                    let code = button_code(event.button());
                    // A release closes an R-swallowed press: never forwarded,
                    // so the app never sees half a drag.
                    if !pressed {
                        let swallowed = match event.button() {
                            MouseButton::Left => std::mem::replace(&mut r_swallow_left, false),
                            MouseButton::Right => {
                                std::mem::replace(&mut r_swallow_right, false)
                            }
                            _ => false,
                        };
                        if swallowed {
                            r_resize = None;
                            return;
                        }
                    }
                    server.input_button(code, pressed);
                    // X-backed focus: mirror press/release via XTEST.
                    let xid = focused_id
                        .and_then(|id| server_state.windows.get(&id))
                        .and_then(|c| c.x11);
                    if let (Some(wm), Some(xid)) = (xwm.as_ref(), xid) {
                        if pressed {
                            // Restack BEFORE delivery: X button routing
                            // targets the X-topmost window, so the focused
                            // window must already be top when the press
                            // lands (the per-frame drain only covers
                            // raises from other paths).
                            wm.restack_above(xid);
                            wm.focus(xid);
                            server.focus_keyboard(None);
                        }
                        wm.xtest_button(xid, code, pressed);
                    }
                }
            }
            InputEvent::PointerAxis { event } => {
                // Wheel over empty canvas zooms at the pointer; over a window
                // it scrolls the client instead.
                let vertical = event
                    .amount(Axis::Vertical)
                    .or_else(|| event.amount_discrete(Axis::Vertical).map(|s| s as f64))
                    .unwrap_or(0.0);
                if vertical != 0.0 {
                    let cworld = session.to_canvas(pointer, view);
                    let over_window = server_state
                        .windows
                        .iter()
                        .any(|(_, c)| c.canvas.contains(cworld));
                    if !over_window {
                        let factor = if vertical > 0.0 { 1.15 } else { 1.0 / 1.15 };
                        session.zoom_at(pointer, view, factor);
                        log::info!("wheel zoom={:.2}", session.camera.zoom);
                        need_render = true;
                        return;
                    }
                }
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

        // Dwell-to-arm runs every frame (a still pointer sends no motion
        // events): 1s inside one window arms it for moving, leaving resets.
        if session.state == crate::session::SuperState::Canvas {
            let now_ms = frame_clock.elapsed().as_millis() as u64;
            let cw = session.to_canvas(pointer, view);
            let inside = server_state.topmost_at(cw);
            if session.dwell.update(inside, now_ms).is_some() {
                log::debug!("dwell armed {:?}", session.dwell.armed);
            }
        }

        // (Bar removed: no status strip; window chrome lives on windows.)

        // Newly committed client buffers (native or XWayland) need a
        // frame even with no input: otherwise live apps (xterm text,
        // clocks) stay stuck on their first imported buffer. Damage-only
        // commits (no new attach) count too: X clients repaint into the
        // same shm buffer and only send damage.
        for cw in server_state.windows.values() {
            if let Some(surface) = cw.surface.as_ref() {
                let pending = with_states(surface, |states| {
                    let current = states.cached_state.current::<SurfaceAttributes>();
                    current.buffer.is_some() || !current.damage.is_empty()
                });
                if matches!(pending, Ok(true)) {
                    need_render = true;
                    break;
                }
            }
        }

        if need_render || frames == 0 || max_frames.is_some() {
            let scale = graphics.window_size().scale_factor;
            let color = background_color(&session.camera);
            graphics.render(|renderer, frame| {
                if let Err(err) = frame.clear(color) {
                    log::error!("clear failed: {}", err);
                }
                // Painter's algorithm: lowest z first, so the visual top
                // always matches the pick top (topmost_at). HashMap
                // iteration is random; unsorted, an occluded window can
                // paint over the focused one while clicks resolve to the
                // focused one (click lands on an invisible window).
                for id in server_state.paint_order() {
                    let cw = match server_state.windows.get_mut(&id) {
                        Some(cw) => cw,
                        None => continue,
                    };
                    // View-locked layer panels stick to the viewport:
                    // recompute the canvas rect from the anchor every
                    // frame so camera moves never drag panels along.
                    if let Some(lock) = cw.view_lock {
                        let org = session.to_world(
                            crate::canvas::Vec2::new(0.0, 0.0),
                            view,
                        );
                        let (w, h) = (cw.canvas.size.x, cw.canvas.size.y);
                        let x = if lock.anchor & crate::layer::ANCHOR_RIGHT != 0 {
                            org.x + view.x - w - lock.margins[1] as f64
                        } else {
                            org.x + lock.margins[3] as f64
                        };
                        let y = if lock.anchor & crate::layer::ANCHOR_BOTTOM != 0 {
                            org.y + view.y - h - lock.margins[2] as f64
                        } else {
                            org.y + lock.margins[0] as f64
                        };
                        cw.canvas.pos.x = x;
                        cw.canvas.pos.y = y;
                    }
                    // Import freshly committed shm buffers. New attaches
                    // upload the new buffer; damage-only commits repaint
                    // into the kept buffer, so re-upload that.
                    if let Some(surface) = cw.surface.as_ref() {
                        let (assignment, damaged) = with_states(surface, |states| {
                            let mut current =
                                states.cached_state.current::<SurfaceAttributes>();
                            let assignment = current.buffer.take();
                            let damaged = !current.damage.is_empty();
                            current.damage.clear();
                            (assignment, damaged)
                        })
                        .unwrap_or((None, false));
                        let buffer = match assignment {
                            Some(BufferAssignment::NewBuffer { buffer, .. }) => Some(buffer),
                            _ => None,
                        };
                        let buffer = buffer.or_else(|| {
                            if damaged {
                                cw.last_buffer.clone()
                            } else {
                                None
                            }
                        });
                        if let Some(buffer) = buffer.as_ref() {
                            cw.last_buffer = Some(buffer.clone());
                            let dims = with_buffer_contents(buffer, |_, data| {
                                (data.width, data.height)
                            });
                            if let Ok((w, h)) = dims {
                                let damage = Rectangle::<i32, BufferCoord>::from_loc_and_size(
                                    (0, 0),
                                    (w, h),
                                );
                                match renderer.import_shm_buffer(&buffer, None, &[damage]) {
                                    Ok(tex) => {
                                        // Follow the buffer only when the CLIENT
                                        // resizes (first map or configure):
                                        // manual moves/resizes never snap back.
                                        // Fullscreen windows keep the exact view
                                        // rect (true fullscreen); the client
                                        // buffer follows via configure.
                                        let buf = (w as u32, h as u32);
                                        if cw.fullscreen_prev.is_none()
                                            && cw.buf_size != Some(buf)
                                        {
                                            cw.buf_size = Some(buf);
                                            cw.canvas.size.x = w as f64;
                                            cw.canvas.size.y = h as f64;
                                            log::info!(
                                                "window {} sized to {}x{}",
                                                id,
                                                w,
                                                h
                                            );
                                        }
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
                    // Windows render as textured perspective planes (3D).
                    // shm buffers go through our own program (tilt in Canvas
                    // mode, flat 1:1 in Idle); anything else keeps Smithay's
                    // flat path so exotic buffers never break the frame.
                    if let Some(tex) = cw.texture.as_ref() {
                        let is_shm = matches!(
                            cw.last_buffer.as_ref().and_then(buffer_type),
                            Some(BufferType::Shm)
                        );
                        if tex_prog.is_none() {
                            match renderer.with_context(|_, gl| {
                                crate::render3d::compile_textured(gl)
                            }) {
                                Ok(Ok(p)) => {
                                    tex_prog = Some(p);
                                    log::info!("textured GL program ready");
                                }
                                Ok(Err(e)) => log::warn!("textured GL program: {}", e),
                                Err(e) => log::warn!("textured GL context: {:?}", e),
                            }
                        }
                        if is_shm {
                            if let Some(prog) = tex_prog {
                                let canvas_mode = session.state
                                    == crate::session::SuperState::Canvas;
                                let tilt = if canvas_mode { (0.14, 0.07) } else { (0.0, 0.0) };
                                // Canvas-plane quad, y negated for clip space, with
                                // upright UVs (buffer row 0 = top). The MVP
                                // maps canvas -> screen; physical px = logical
                                // times the output scale.
                                let sc = scale as f32;
                                let (qx, qy) = (
                                    cw.canvas.pos.x as f32 * sc,
                                    -(cw.canvas.pos.y as f32) * sc,
                                );
                                let (qw, qh) = (
                                    cw.canvas.size.x as f32 * sc,
                                    -(cw.canvas.size.y as f32) * sc,
                                );
                                let vw = view.x as f32 * sc;
                                let vh = view.y as f32 * sc;
                                let mid = session.to_world(
                                    crate::canvas::Vec2::new(view.x / 2.0, view.y / 2.0),
                                    view,
                                );
                                let mvp = crate::render3d::canvas_mvp(
                                    (vw, vh),
                                    (mid.x as f32 * sc, mid.y as f32 * sc),
                                    session.view_camera().zoom as f32,
                                    tilt,
                                );
                                let verts: [f32; 16] = [
                                    qx, qy, 0.0, 0.0, //
                                    qx + qw, qy, 1.0, 0.0, //
                                    qx, qy + qh, 0.0, 1.0, //
                                    qx + qw, qy + qh, 1.0, 1.0,
                                ];
                                let tid = tex.tex_id();
                                // X-format buffers carry no real alpha (the X
                                // byte is server droppings); without forcing
                                // opaque, the window renders transparent
                                // wherever nothing opaque was drawn.
                                let opaque = cw
                                    .last_buffer
                                    .as_ref()
                                    .map(|buffer| {
                                        with_buffer_contents(buffer, |_, data| {
                                            crate::render3d::shm_format_is_opaque(
                                                data.format,
                                            )
                                        })
                                        .unwrap_or(false)
                                    })
                                    .unwrap_or(false);
                                let _ = renderer.with_context(|_, gl| unsafe {
                                    crate::render3d::draw_textured(
                                        gl, prog, &mvp, tid, &verts, opaque,
                                    );
                                });
                                continue;
                            }
                        }
                        let (x, y, w, h) =
                            window_screen_rect(&session.view_camera(), &cw.canvas, view, scale);
                        let dest = Rectangle::<f64, Physical>::from_loc_and_size((x, y), (w, h));
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
                // Window chrome (Idle only): probe the top hit window; when
                // the pointer lingers near its top-right corner, two solid
                // squares draw there — red close, blue fullscreen.
                let mut chrome_now = None;
                if session.state == crate::session::SuperState::Idle {
                    let cworld = session.to_canvas(pointer, view);
                    if let Some(id) = server_state.topmost_at(cworld) {
                        if let Some(cw) = server_state.windows.get(&id) {
                            let origin = session.to_screen(cw.canvas.pos, view);
                            let zoom = session.view_camera().zoom;
                            let size = crate::canvas::Vec2::new(
                                cw.canvas.size.x * zoom,
                                cw.canvas.size.y * zoom,
                            );
                            if let Some(layout) =
                                crate::chrome::layout_for(origin, size, pointer)
                            {
                                chrome_now = Some((id, layout));
                            }
                        }
                    }
                }
                if let Some((_, layout)) = chrome_now {
                    if solid_prog.is_none() {
                        match renderer.with_context(|_, gl| {
                            crate::render3d::compile_solid(gl)
                        }) {
                            Ok(Ok(p)) => {
                                solid_prog = Some(p);
                                log::info!("chrome GL program ready");
                            }
                            Ok(Err(e)) => log::warn!("chrome GL program: {}", e),
                            Err(e) => log::warn!("chrome GL context: {:?}", e),
                        }
                    }
                    if let Some(prog) = solid_prog {
                        let mvp =
                            crate::render3d::ortho(view.x as f32, view.y as f32);
                        let sc = scale as f32;
                        let b = crate::chrome::CHROME_BUTTON as f32 * sc;
                        let _ = renderer.with_context(|_, gl| unsafe {
                            crate::render3d::draw_solid(
                                gl,
                                prog,
                                &mvp,
                                [0.85, 0.2, 0.2, 1.0],
                                (
                                    layout.close.x as f32 * sc,
                                    layout.close.y as f32 * sc,
                                    b,
                                    b,
                                ),
                            );
                            crate::render3d::draw_solid(
                                gl,
                                prog,
                                &mvp,
                                [0.2, 0.45, 0.9, 1.0],
                                (
                                    layout.fullscreen.x as f32 * sc,
                                    layout.fullscreen.y as f32 * sc,
                                    b,
                                    b,
                                ),
                            );
                        });
                    }
                }
                last_chrome = chrome_now;
            })?;
            // Frame callbacks: tell clients this frame was presented, so
            // paced clients (Xwayland damage throttling, animations) keep
            // committing instead of stalling after their first buffer.
            // Output enter goes out first sight per surface: Qt waits for
            // `wl_surface.enter` before its first frame.
            let now_ms = frame_clock.elapsed().as_millis() as u32;
            let surfaces: Vec<WlSurface> = server_state
                .windows
                .values()
                .filter_map(|cw| cw.surface.clone())
                .collect();
            for surface in &surfaces {
                if server_state.claim_output_enter(surface) {
                    server.enter_output(surface);
                }
            }
            for cw in server_state.windows.values() {
                if let Some(surface) = cw.surface.as_ref() {
                    let pending = with_states(surface, |states| {
                        std::mem::take(
                            &mut states
                                .cached_state
                                .current::<SurfaceAttributes>()
                                .frame_callbacks,
                        )
                    });
                    if let Ok(callbacks) = pending {
                        for callback in callbacks {
                            callback.done(now_ms);
                        }
                    }
                }
            }
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

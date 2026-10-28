//! Wayland frontend skeleton (feature `compositor` only).
//!
//! Display + socket + core globals (compositor, shm, xdg_shell). Every new
//! xdg-toplevel becomes a CanvasWindow with cascade placement on the shared
//! canvas. Client buffer rendering and seat input land next.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::OsString;
use std::io;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use smithay::backend::input::KeyState;
use smithay::backend::renderer::gles2::Gles2Texture;
use smithay::reexports::wayland_protocols::unstable::xdg_shell::v6::server::zxdg_shell_v6::ZxdgShellV6;
use smithay::reexports::wayland_protocols::xdg_shell::server::xdg_wm_base::XdgWmBase;
use smithay::reexports::wayland_server::protocol::wl_buffer::WlBuffer;
use smithay::reexports::wayland_server::protocol::wl_compositor::WlCompositor;
use smithay::reexports::wayland_server::protocol::wl_shm;
use smithay::reexports::wayland_server::protocol::wl_subcompositor::WlSubcompositor;
use smithay::reexports::wayland_server::protocol::wl_output::WlOutput;
use smithay::reexports::wayland_server::protocol::wl_pointer::ButtonState as WlButtonState;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{Display, Global};
use smithay::wayland::output::{Mode, Output, PhysicalProperties};
use slog::Drain as _;
use smithay::utils::{Logical, Point};
use smithay::wayland::compositor::compositor_init;
use smithay::wayland::seat::{AxisFrame, KeyboardHandle, PointerHandle, Seat, XkbConfig};
use smithay::wayland::shell::xdg::{xdg_shell_init, ShellState, XdgRequest};
use smithay::wayland::shm::init_shm_global;
use smithay::wayland::Serial;

use crate::canvas::{CanvasWindow, Vec2};

/// Default size for a fresh client window; the client configures itself after.
pub const DEFAULT_SIZE: (f64, f64) = (800.0, 600.0);

/// Cascade placement on the canvas: window n sits one step down-right from
/// the previous, wrapping every 8 so the pile stays near the origin.
pub fn place_window(id: u64, n: usize) -> CanvasWindow {
    let step = (n % 8) as f64;
    CanvasWindow::new(
        id,
        96.0 + step * 64.0,
        96.0 + step * 48.0,
        DEFAULT_SIZE.0,
        DEFAULT_SIZE.1,
    )
}

#[derive(Default)]
pub struct ServerState {
    pub windows: HashMap<u64, ClientWindow>,
    next_id: u64,
    /// Every live wl_surface by its numeric id, XWayland's included.
    /// XWayland publishes each X window's surface id through the
    /// WL_SURFACE_ID client message; the backend resolves it here and
    /// renders the real surface zero-copy (no GetImage bridge).
    surface_ids: HashMap<u32, WlSurface>,
}

/// A client window: canvas placement plus live surface and GPU texture.
/// The texture is refreshed from newly committed shm buffers each frame.
/// `overlay` marks non-canvas surfaces (the internal bar): drawn fixed in
/// screen space, excluded from picking, focus and seat routing.
/// `x11` marks XWayland-backed windows: the surface is XWayland's own
/// (resolved from its WL_SURFACE_ID), rendered zero-copy like any window.
pub struct ClientWindow {
    pub canvas: CanvasWindow,
    pub surface: Option<WlSurface>,
    pub texture: Option<Gles2Texture>,
    /// Last attached buffer, kept so damage-only commits (repaint into
    /// the same shm buffer, no new attach) can re-upload pixels.
    pub last_buffer: Option<WlBuffer>,
    pub overlay: bool,
    pub x11: Option<u32>,
}

impl ClientWindow {
    fn new(id: u64, surface: Option<WlSurface>, n: usize) -> Self {
        Self {
            canvas: place_window(id, n),
            surface,
            texture: None,
            last_buffer: None,
            overlay: false,
            x11: None,
        }
    }
}

impl ServerState {
    pub fn add_toplevel(&mut self, surface: Option<WlSurface>) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        let win = ClientWindow::new(id, surface, self.windows.len());
        self.windows.insert(id, win);
        id
    }

    /// Tag a window as screen-space overlay (the bar). Only the backend
    /// calls this, right after the internal bar client maps at startup.
    pub fn set_overlay(&mut self, id: u64) {
        if let Some(win) = self.windows.get_mut(&id) {
            win.overlay = true;
        }
    }

    /// Create a canvas window for an XWayland surface (resolved from its
    /// WL_SURFACE_ID) and tag it with its X window id for XTEST routing.
    pub fn add_x11_window(&mut self, surface: WlSurface, xid: u32) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        let mut win = ClientWindow::new(id, Some(surface), self.windows.len());
        win.x11 = Some(xid);
        self.windows.insert(id, win);
        id
    }

    /// Resolve a live surface by its numeric Wayland id (XWayland's
    /// WL_SURFACE_ID, or any surface the backend knows by id, like the bar).
    pub fn surface_by_id(&self, id: u32) -> Option<WlSurface> {
        self.surface_ids.get(&id).cloned()
    }

    /// Drop a surface id when its window dies (called with the sid the Xwm
    /// reported for the destroyed X window).
    pub fn forget_surface(&mut self, id: u32) {
        self.surface_ids.remove(&id);
    }

    /// Windows on the canvas, sorted by id; overlays excluded.
    pub fn canvas_ids(&self) -> Vec<u64> {
        let mut ids: Vec<u64> = self
            .windows
            .iter()
            .filter(|(_, c)| !c.overlay)
            .map(|(id, _)| *id)
            .collect();
        ids.sort_unstable();
        ids
    }
}

/// Stderr drain for Smithay's slog seat logs. Level follows
/// `CANVAS_SEAT_TRACE` (trace when set, debug otherwise); keycodes are
/// numbers only, no keysyms or text ever reach the log.
struct SeatLogDrain;
impl slog::Drain for SeatLogDrain {
    type Ok = ();
    type Err = slog::Never;

    fn log(
        &self,
        record: &slog::Record,
        _values: &slog::OwnedKVList,
    ) -> Result<(), slog::Never> {
        eprintln!("[seat {}] {}", record.level(), record.msg());
        Ok(())
    }
}

fn seat_logger() -> slog::Logger {
    let level = if std::env::var("CANVAS_SEAT_TRACE").is_ok() {
        slog::Level::Trace
    } else {
        slog::Level::Debug
    };
    let drain = slog::LevelFilter::new(SeatLogDrain, level).fuse();
    slog::Logger::root(drain, slog::o!())
}

/// Owns the Wayland display and keeps the globals alive. Pump it from the
/// backend event loop; client windows appear in the passed ServerState.
///
/// Also owns the seat: one keyboard + one pointer. The backend feeds raw
/// input here; the seat routes it to the focused surfaces.
pub struct Server {
    display: Rc<RefCell<Display>>,
    _compositor: Global<WlCompositor>,
    _subcompositor: Global<WlSubcompositor>,
    _shm: Global<wl_shm::WlShm>,
    _shell_state: Arc<Mutex<ShellState>>,
    _xdg: Global<XdgWmBase>,
    _zxdg: Global<ZxdgShellV6>,
    _seat: Seat,
    _seat_global: Global<smithay::reexports::wayland_server::protocol::wl_seat::WlSeat>,
    _output: Output,
    _output_global: Global<WlOutput>,
    keyboard: KeyboardHandle,
    pointer: PointerHandle,
    serial: u32,
    start: Instant,
}

impl Server {
    pub fn new() -> io::Result<(Self, OsString)> {
        let display = Rc::new(RefCell::new(Display::new()));
        let (compositor, subcompositor) = compositor_init(
            &mut display.borrow_mut(),
            |surface, mut dispatch| {
                if let Some(state) = dispatch.get::<ServerState>() {
                    let sid = surface.as_ref().id();
                    log::info!("surface created id={}", sid);
                    state.surface_ids.insert(sid, surface.clone());
                }
            },
            None,
        );
        let shm = init_shm_global(
            &mut display.borrow_mut(),
            vec![wl_shm::Format::Argb8888, wl_shm::Format::Xrgb8888],
            None,
        );
        let (shell_state, xdg, zxdg) = xdg_shell_init(
            &mut display.borrow_mut(),
            |request, mut dispatch| match request {
                XdgRequest::NewToplevel { surface } => {
                    if let Some(state) = dispatch.get::<ServerState>() {
                        let wl_surface = surface.get_surface().cloned();
                        let id = state.add_toplevel(wl_surface);
                        log::info!("new toplevel -> canvas window {}", id);
                        surface.send_configure();
                    }
                }
                _ => {}
            },
            None,
        );
        let socket = display.borrow_mut().add_socket_auto()?;
        let (mut seat, seat_global) =
            Seat::new(&mut display.borrow_mut(), "seat-0".into(), Some(seat_logger()));
        let keyboard = seat
            .add_keyboard(XkbConfig::default(), 200, 25, |_, _| {})
            .map_err(|err| io::Error::new(io::ErrorKind::Other, format!("seat keyboard: {:?}", err)))?;
        let pointer = seat.add_pointer(|_| {});
        // One output viewport on the shared canvas. XWayland sizes its
        // screen from wl_output and stalls without one; real clients use
        // it for DPI/positioning. Extra viewports later reuse this.
        let (output, output_global) = Output::new(
            &mut display.borrow_mut(),
            "WL-1".into(),
            PhysicalProperties {
                size: (0, 0).into(),
                subpixel: smithay::reexports::wayland_server::protocol::wl_output::Subpixel::Unknown,
                make: "canvas-wm".into(),
                model: "canvas".into(),
            },
            None,
        );
        let mode = Mode {
            size: (1280, 800).into(),
            refresh: 60000,
        };
        output.add_mode(mode);
        output.set_preferred(mode);
        output.change_current_state(
            Some(mode),
            Some(
                smithay::reexports::wayland_server::protocol::wl_output::Transform::Normal,
            ),
            Some(1),
            Some((0, 0).into()),
        );
        Ok((
            Self {
                display,
                _compositor: compositor,
                _subcompositor: subcompositor,
                _shm: shm,
                _shell_state: shell_state,
                _xdg: xdg,
                _zxdg: zxdg,
                _seat: seat,
                _seat_global: seat_global,
                _output: output,
                _output_global: output_global,
                keyboard,
                pointer,
                serial: 0,
                start: Instant::now(),
            },
            socket,
        ))
    }

    /// Process pending client messages without blocking, then flush replies.
    pub fn pump(&mut self, state: &mut ServerState) {
        let _ = self
            .display
            .borrow_mut()
            .dispatch(Duration::from_millis(0), state);
        self.display.borrow_mut().flush_clients(state);
    }

    /// Shared display handle, for XWayland startup (same-thread use).
    pub fn display(&self) -> Rc<RefCell<Display>> {
        Rc::clone(&self.display)
    }

    fn next_serial(&mut self) -> Serial {
        self.serial = self.serial.wrapping_add(1);
        Serial::from(self.serial)
    }

    fn now_ms(&self) -> u32 {
        self.start.elapsed().as_millis() as u32
    }

    /// Forward a key to the focused client. `evdev` is the kernel keycode.
    /// Super and Esc never reach here: the backend consumes them as gestures.
    pub fn input_key(&mut self, evdev: u32, pressed: bool) {
        log::debug!("seat input_key evdev={} pressed={}", evdev, pressed);
        let state = if pressed {
            KeyState::Pressed
        } else {
            KeyState::Released
        };
        let serial = self.next_serial();
        let time = self.now_ms();
        self.keyboard.input(evdev, state, serial, time, |_, _| true);
    }

    /// Move keyboard focus. Call with None then the new surface when the
    /// window set changes, so late-bound clients also receive Enter.
    pub fn focus_keyboard(&mut self, surface: Option<&WlSurface>) {
        let serial = self.next_serial();
        self.keyboard.set_focus(surface, serial);
    }

    /// Forward pointer motion. `logical` is the output-space position,
    /// `focus` the surface under the cursor with the surface ORIGIN in
    /// logical space (Smithay subtracts it to report surface-local coords).
    pub fn input_motion(
        &mut self,
        logical: (f64, f64),
        focus: Option<(WlSurface, (i32, i32))>,
    ) {
        log::debug!(
            "seat input_motion logical=({},{}) focus={}",
            logical.0,
            logical.1,
            focus.is_some()
        );
        let serial = self.next_serial();
        let time = self.now_ms();
        let location: Point<f64, Logical> = logical.into();
        let focus = focus.map(|(surface, (x, y))| {
            let local: Point<i32, Logical> = (x, y).into();
            (surface, local)
        });
        self.pointer.motion(location, focus, serial, time);
    }

    /// Forward a pointer button. `code` is the evdev button code.
    pub fn input_button(&mut self, code: u32, pressed: bool) {
        let state = if pressed {
            WlButtonState::Pressed
        } else {
            WlButtonState::Released
        };
        let serial = self.next_serial();
        let time = self.now_ms();
        self.pointer.button(code, state, serial, time);
    }

    /// Forward a scroll frame to the focused client.
    pub fn input_axis(&mut self, frame: AxisFrame) {
        self.pointer.axis(frame);
    }
}

/// Canvas position of the nth window, for tests and future tiling helpers.
pub fn cascade_origin(n: usize) -> Vec2 {
    let step = (n % 8) as f64;
    Vec2::new(96.0 + step * 64.0, 96.0 + step * 48.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cascade_steps_down_right_and_wraps() {
        let a = cascade_origin(0);
        let b = cascade_origin(1);
        assert!(b.x > a.x && b.y > a.y);
        assert_eq!(cascade_origin(8), cascade_origin(0));
    }

    #[test]
    fn toplevels_get_unique_ids_and_windows() {
        let mut s = ServerState::default();
        let first = s.add_toplevel(None);
        let second = s.add_toplevel(None);
        assert_ne!(first, second);
        assert_eq!(s.windows.len(), 2);
        let wa = &s.windows[&first].canvas;
        let wb = &s.windows[&second].canvas;
        assert!(wb.pos.x > wa.pos.x && wb.pos.y > wa.pos.y);
    }
}

/// In-process seat proof: a real client connects over the socket, maps a
/// toplevel, then the server focuses it and sends key + pointer events.
/// No X, no winit, no xdotool: pump and dispatch interleave on one thread.
#[cfg(test)]
mod seat_tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    use wayland_client::protocol::wl_compositor::WlCompositor as ClientCompositor;
    use wayland_client::protocol::wl_keyboard::Event as ClientKeyEvent;
    use wayland_client::protocol::wl_pointer::Event as ClientPointerEvent;
    use wayland_client::protocol::wl_seat::WlSeat as ClientSeat;
    use wayland_client::{Display as ClientDisplay, GlobalManager};
    use wayland_protocols::xdg_shell::client::{xdg_surface, xdg_wm_base};

    fn pump_both(
        server: &mut Server,
        state: &mut ServerState,
        display: &ClientDisplay,
        queue: &mut wayland_client::EventQueue,
        rounds: usize,
    ) {
        use std::io::ErrorKind;
        for _ in 0..rounds {
            if let Err(e) = display.flush() {
                assert_eq!(e.kind(), ErrorKind::WouldBlock, "client flush: {:?}", e);
            }
            server.pump(state);
            if let Some(guard) = queue.prepare_read() {
                if let Err(e) = guard.read_events() {
                    assert_eq!(e.kind(), ErrorKind::WouldBlock, "client read: {:?}", e);
                }
            }
            queue
                .dispatch_pending(&mut (), |_, _, _| {})
                .expect("client dispatch");
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }

    #[test]
    fn seat_forwards_key_and_pointer_to_focused_window() {
        std::env::set_var("XDG_RUNTIME_DIR", std::env::temp_dir());
        let (mut server, socket) = Server::new().expect("server");
        let socket_name = socket.to_str().expect("socket name").to_owned();
        let mut state = ServerState::default();

        let display = ClientDisplay::connect_to_name(socket_name).expect("client connect");
        let mut queue = display.create_event_queue();
        let attached = (*display).clone().attach(queue.token());
        let globals = GlobalManager::new(&attached);
        pump_both(&mut server, &mut state, &display, &mut queue, 30);

        let compositor = globals
            .instantiate_exact::<ClientCompositor>(4)
            .expect("wl_compositor");
        let seat = globals.instantiate_exact::<ClientSeat>(5).expect("wl_seat");
        let events: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
        let kb_events = events.clone();
        seat.get_keyboard().quick_assign(move |_, event, _| match event {
            ClientKeyEvent::Enter { .. } => kb_events.borrow_mut().push("kb-enter".into()),
            ClientKeyEvent::Key { key, .. } => {
                kb_events.borrow_mut().push(format!("key {}", key))
            }
            _ => {}
        });
        let pt_events = events.clone();
        seat.get_pointer().quick_assign(move |_, event, _| match event {
            ClientPointerEvent::Enter { .. } => pt_events.borrow_mut().push("pt-enter".into()),
            ClientPointerEvent::Button { button, .. } => {
                pt_events.borrow_mut().push(format!("button {}", button))
            }
            _ => {}
        });
        pump_both(&mut server, &mut state, &display, &mut queue, 20);

        let wm_base = globals
            .instantiate_exact::<xdg_wm_base::XdgWmBase>(1)
            .expect("xdg_wm_base");
        wm_base.quick_assign(|wm_base, event, _| match event {
            xdg_wm_base::Event::Ping { serial } => wm_base.pong(serial),
            _ => {}
        });
        let surface = compositor.create_surface();
        let xdg_surface = wm_base.get_xdg_surface(&surface);
        xdg_surface.quick_assign(|xdg_surface, event, mut data| match event {
            xdg_surface::Event::Configure { serial } => {
                xdg_surface.ack_configure(serial);
                if let Some(done) = data.get::<bool>() {
                    *done = true;
                }
            }
            _ => {}
        });
        let _toplevel = xdg_surface.get_toplevel();
        surface.commit();
        pump_both(&mut server, &mut state, &display, &mut queue, 40);
        assert_eq!(state.windows.len(), 1, "client toplevel becomes a window");

        let id = *state.windows.keys().next().unwrap();
        let surf = state.windows[&id].surface.clone().expect("surface");
        server.focus_keyboard(None);
        server.focus_keyboard(Some(&surf));
        server.input_key(16, true);
        server.input_key(16, false);
        let local = (10, 10);
        server.input_motion((0.0, 0.0), Some((surf, local)));
        server.input_button(0x110, true);
        server.input_button(0x110, false);
        pump_both(&mut server, &mut state, &display, &mut queue, 40);

        let got = events.borrow().join("\n");
        assert!(got.contains("kb-enter"), "keyboard enter:\n{}", got);
        assert!(got.contains("key 16"), "key event:\n{}", got);
        assert!(got.contains("pt-enter"), "pointer enter:\n{}", got);
        assert!(got.contains("button 272"), "button event:\n{}", got);
    }
}

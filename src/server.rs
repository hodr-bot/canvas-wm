//! Wayland frontend skeleton (feature `compositor` only).
//!
//! Display + socket + core globals (compositor, shm, xdg_shell). Every new
//! xdg-toplevel becomes a CanvasWindow with cascade placement on the shared
//! canvas. Client buffer rendering and seat input land next.

use std::collections::HashMap;
use std::ffi::OsString;
use std::io;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use smithay::backend::renderer::gles2::Gles2Texture;
use smithay::reexports::wayland_protocols::unstable::xdg_shell::v6::server::zxdg_shell_v6::ZxdgShellV6;
use smithay::reexports::wayland_protocols::xdg_shell::server::xdg_wm_base::XdgWmBase;
use smithay::reexports::wayland_server::protocol::wl_compositor::WlCompositor;
use smithay::reexports::wayland_server::protocol::wl_shm;
use smithay::reexports::wayland_server::protocol::wl_subcompositor::WlSubcompositor;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{Display, Global};
use smithay::wayland::compositor::compositor_init;
use smithay::wayland::shell::xdg::{xdg_shell_init, ShellState, XdgRequest};
use smithay::wayland::shm::init_shm_global;

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
}

/// A client window: canvas placement plus live surface and GPU texture.
/// The texture is refreshed from newly committed shm buffers each frame.
pub struct ClientWindow {
    pub canvas: CanvasWindow,
    pub surface: Option<WlSurface>,
    pub texture: Option<Gles2Texture>,
}

impl ClientWindow {
    fn new(id: u64, surface: Option<WlSurface>, n: usize) -> Self {
        Self {
            canvas: place_window(id, n),
            surface,
            texture: None,
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
}

/// Owns the Wayland display and keeps the globals alive. Pump it from the
/// backend event loop; client windows appear in the passed ServerState.
pub struct Server {
    display: Display,
    _compositor: Global<WlCompositor>,
    _subcompositor: Global<WlSubcompositor>,
    _shm: Global<wl_shm::WlShm>,
    _shell_state: Arc<Mutex<ShellState>>,
    _xdg: Global<XdgWmBase>,
    _zxdg: Global<ZxdgShellV6>,
}

impl Server {
    pub fn new() -> io::Result<(Self, OsString)> {
        let mut display = Display::new();
        let (compositor, subcompositor) =
            compositor_init(&mut display, |_surface, _dispatch| {}, None);
        let shm = init_shm_global(
            &mut display,
            vec![wl_shm::Format::Argb8888, wl_shm::Format::Xrgb8888],
            None,
        );
        let (shell_state, xdg, zxdg) = xdg_shell_init(
            &mut display,
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
        let socket = display.add_socket_auto()?;
        Ok((
            Self {
                display,
                _compositor: compositor,
                _subcompositor: subcompositor,
                _shm: shm,
                _shell_state: shell_state,
                _xdg: xdg,
                _zxdg: zxdg,
            },
            socket,
        ))
    }

    /// Process pending client messages without blocking, then flush replies.
    pub fn pump(&mut self, state: &mut ServerState) {
        let _ = self
            .display
            .dispatch(Duration::from_millis(0), state);
        self.display.flush_clients(state);
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

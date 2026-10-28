//! Internal bar client: draws the minimal top bar as pixels.
//!
//! Non-blocking by construction: the backend pumps both the server and this
//! client until the bar toplevel maps. The bar maps FIRST at backend startup,
//! before any external client can connect, so the backend tags window 0 as
//! the overlay. It draws background + one chip per window (brighter for the
//! focused one) into a single reused shm buffer, redrawing only on change.

use std::ffi::OsStr;
use std::io::{Seek, SeekFrom, Write};

use wayland_client::protocol::wl_compositor::WlCompositor;
use wayland_client::protocol::wl_shm;
use wayland_client::protocol::wl_shm_pool::WlShmPool;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Display, EventQueue, GlobalManager, Main};
use wayland_protocols::xdg_shell::client::{xdg_surface, xdg_wm_base};

use crate::bar::{self, BAR_HEIGHT};

const APP_ID: &str = "canvas-wm.bar";
const BG: u32 = 0xFF101815;
const CHIP: u32 = 0xFF2E7A4E;
const CHIP_FOCUSED: u32 = 0xFF46B06E;

struct Bound {
    surface: Main<WlSurface>,
    pool: Main<WlShmPool>,
    pool_file: std::fs::File,
    width: i32,
    height: i32,
}

/// Bar client handle. Pump it from the backend loop.
pub struct BarClient {
    display: Display,
    queue: EventQueue,
    globals: GlobalManager,
    bound: Option<Bound>,
    last_drawn: Option<(usize, Option<usize>)>,
    width: i32,
    /// Wayland numeric id of the bar surface, for overlay tagging.
    surface_id: Option<u32>,
}

impl BarClient {
    /// Connect and request the registry. Non-blocking: call `progress`
    /// until it returns true (mapped), pumping the server in between.
    pub fn connect(socket: &OsStr, width: i32) -> Result<Self, String> {
        let display =
            Display::connect_to_name(socket).map_err(|e| format!("bar connect: {:?}", e))?;
        let queue = display.create_event_queue();
        let attached = (*display).clone().attach(queue.token());
        let globals = GlobalManager::new(&attached);
        Ok(Self {
            display,
            queue,
            globals,
            bound: None,
            last_drawn: None,
            width,
            surface_id: None,
        })
    }

    /// Numeric Wayland id of the bar surface (None until mapped).
    pub fn surface_id(&self) -> Option<u32> {
        self.surface_id
    }

    /// Non-blocking pump: flush requests, read what is available, dispatch.
    pub fn pump(&mut self) {
        use std::io::ErrorKind;
        if let Err(e) = self.display.flush() {
            if e.kind() != ErrorKind::WouldBlock {
                log::warn!("bar flush: {:?}", e);
            }
        }
        if let Some(guard) = self.queue.prepare_read() {
            if let Err(e) = guard.read_events() {
                if e.kind() != ErrorKind::WouldBlock {
                    log::warn!("bar read: {:?}", e);
                }
            }
        }
        let _ = self.queue.dispatch_pending(&mut (), |_, _, _| {});
    }

    /// Advance binding/mapping. Returns true once the toplevel is mapped
    /// (committed); the server then sees window 0 on its next pump.
    pub fn progress(&mut self) -> bool {
        self.pump();
        if self.bound.is_some() {
            return true;
        }
        let compositor = match self.globals.instantiate_exact::<WlCompositor>(4) {
            Ok(c) => c,
            Err(_) => return false,
        };
        let shm = match self.globals.instantiate_exact::<wl_shm::WlShm>(1) {
            Ok(s) => s,
            Err(_) => return false,
        };
        let wm_base = match self
            .globals
            .instantiate_exact::<xdg_wm_base::XdgWmBase>(1)
        {
            Ok(w) => w,
            Err(_) => return false,
        };
        wm_base.quick_assign(|wm_base, event, _| match event {
            xdg_wm_base::Event::Ping { serial } => wm_base.pong(serial),
            _ => {}
        });

        let surface = compositor.create_surface();
        let xdg_surface = wm_base.get_xdg_surface(&surface);
        xdg_surface.quick_assign(|xdg_surface, event, _| match event {
            xdg_surface::Event::Configure { serial } => xdg_surface.ack_configure(serial),
            _ => {}
        });
        let toplevel = xdg_surface.get_toplevel();
        toplevel.set_app_id(APP_ID.into());
        toplevel.set_title("canvas-wm bar".into());
        surface.commit();

        let height = BAR_HEIGHT as i32;
        let stride = self.width * 4;
        let pool_size = stride * height;
        let pool_file = match tempfile_in_ram(pool_size as u64) {
            Ok(f) => f,
            Err(e) => {
                log::warn!("bar tmp: {}", e);
                return false;
            }
        };
        let pool = shm.create_pool(pool_file_as_fd(&pool_file), pool_size);
        let buffer = pool.create_buffer(
            0,
            self.width,
            height,
            stride,
            wl_shm::Format::Argb8888,
        );
        surface.attach(Some(&buffer), 0, 0);
        surface.commit();
        self.surface_id = Some(surface.as_ref().id());
        self.bound = Some(Bound {
            surface,
            pool,
            pool_file,
            width: self.width,
            height,
        });
        true
    }

    /// Redraw background + chips when (n, focused) changed. `focused` is the
    /// chip index (position in the backend's sorted canvas id list).
    pub fn redraw(&mut self, n: usize, focused: Option<usize>) {
        if self.last_drawn == Some((n, focused)) {
            return;
        }
        log::debug!("bar redraw n={} focused={:?}", n, focused);
        self.last_drawn = Some((n, focused));
        let bound = match self.bound.as_mut() {
            Some(b) => b,
            None => return,
        };
        let (w, h) = (bound.width, bound.height);
        let mut pixels = vec![BG; (w * h) as usize];
        let chips = bar::chips(w as f64, n);
        let y0 = bar::chip_y() as i32;
        for (i, chip) in chips.iter().enumerate() {
            let color = if Some(i) == focused {
                CHIP_FOCUSED
            } else {
                CHIP
            };
            let x0 = chip.x as i32;
            for y in y0..(y0 + bar::CHIP_H as i32) {
                for x in x0..(x0 + bar::CHIP_W as i32) {
                    if x >= 0 && x < w && y >= 0 && y < h {
                        pixels[(y * w + x) as usize] = color;
                    }
                }
            }
        }
        bound.pool_file.seek(SeekFrom::Start(0)).expect("bar seek");
        for pixel in &pixels {
            bound
                .pool_file
                .write_all(&pixel.to_ne_bytes())
                .expect("bar pixels");
        }
        bound.pool_file.flush().expect("bar flush");
        // Re-attach every redraw: a damage-only commit on an already
        // consumed buffer is not picked up again server-side.
        let buffer = bound.pool.create_buffer(
            0,
            bound.width,
            bound.height,
            bound.width * 4,
            wl_shm::Format::Argb8888,
        );
        bound.surface.attach(Some(&buffer), 0, 0);
        bound.surface.damage(0, 0, bound.width, bound.height);
        bound.surface.commit();
        buffer.destroy();
    }
}

/// Backing file for the shm pool: a RAM file nobody else sees.
fn tempfile_in_ram(size: u64) -> Result<std::fs::File, String> {
    let mut path = std::env::temp_dir();
    path.push(format!("canvas-wm-bar-{}", std::process::id()));
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(&path)
        .map_err(|e| format!("bar tmp: {:?}", e))?;
    file.set_len(size)
        .map_err(|e| format!("bar tmp size: {:?}", e))?;
    std::fs::remove_file(&path).ok();
    Ok(file)
}

#[cfg(unix)]
fn pool_file_as_fd(file: &std::fs::File) -> i32 {
    use std::os::fd::AsRawFd;
    file.as_raw_fd()
}

#[cfg(not(unix))]
fn pool_file_as_fd(_file: &std::fs::File) -> i32 {
    -1
}

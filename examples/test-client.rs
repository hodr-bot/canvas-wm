//! Minimal frameless xdg test client on wayland-client 0.29 (same generation
//! as the compositor): connects, opens one toplevel, acks configure, then
//! acts as a probe for the native-client pipeline: logs whether a keymap
//! arrives, counts frame callbacks, and repaints with alternating colours
//! so screenshots prove live updates (not just the first frame).
//!
//! Run against a live compositor only, never in `cargo test`:
//!   WAYLAND_DISPLAY=... XDG_RUNTIME_DIR=... ./target/debug/examples/test-client [frames=60]

use std::ffi::CString;
use std::io::Write;
use std::os::fd::{AsRawFd, FromRawFd};

use wayland_client::protocol::wl_compositor::WlCompositor;
use wayland_client::protocol::wl_keyboard::Event as KeyEvent;
use wayland_client::protocol::wl_pointer::Event as PointerEvent;
use wayland_client::protocol::wl_seat::WlSeat;
use wayland_client::protocol::wl_shm;
use wayland_client::{Display, GlobalManager, Main};
use wayland_protocols::xdg_shell::client::{xdg_surface, xdg_wm_base};

fn main() {
    let display = Display::connect_to_env().expect("connect to compositor");
    let mut event_queue = display.create_event_queue();
    let attached = (*display).clone().attach(event_queue.token());

    let globals = GlobalManager::new(&attached);
    event_queue
        .sync_roundtrip(&mut (), |_, _, _| unreachable!())
        .expect("roundtrip");
    // Capability probe: GL toolkits need zwp_linux_dmabuf_v1 to present.
    for (id, interface, version) in globals.list() {
        let _ = id;
        if interface == "zwp_linux_dmabuf_v1"
            || interface == "wl_shm"
            || interface == "xdg_wm_base"
        {
            println!("global: {} v{}", interface, version);
        }
    }

    let compositor = globals
        .instantiate_exact::<WlCompositor>(4)
        .expect("wl_compositor");
    let shm = globals
        .instantiate_exact::<wl_shm::WlShm>(1)
        .expect("wl_shm");
    // Bind the seat BEFORE mapping the surface: the compositor moves
    // keyboard focus to each newly mapped window, and only clients that
    // already hold keyboard/pointer objects receive Enter.
    let seat = globals
        .instantiate_exact::<WlSeat>(5)
        .expect("wl_seat");
    let keyboard = seat.get_keyboard();
    keyboard.quick_assign(|_, event, _| match event {
        KeyEvent::Keymap { format, size, .. } => {
            println!("input: keymap format={:?} size={}", format, size)
        }
        KeyEvent::Enter { .. } => println!("input: keyboard enter"),
        KeyEvent::Leave { .. } => println!("input: keyboard leave"),
        KeyEvent::Key { key, state, .. } => {
            println!("input: key code={} state={:?}", key, state)
        }
        _ => {}
    });
    let pointer = seat.get_pointer();
    pointer.quick_assign(|_, event, _| match event {
        PointerEvent::Enter {
            surface_x,
            surface_y,
            ..
        } => println!("input: pointer enter x={} y={}", surface_x, surface_y),
        PointerEvent::Motion {
            surface_x,
            surface_y,
            ..
        } => println!(
            "input: pointer motion x={} y={}",
            surface_x, surface_y
        ),
        PointerEvent::Button { button, state, .. } => {
            println!("input: pointer button code={} state={:?}", button, state)
        }
        _ => {}
    });
    event_queue
        .sync_roundtrip(&mut (), |_, _, _| {})
        .expect("roundtrip");
    let wm_base = globals
        .instantiate_exact::<xdg_wm_base::XdgWmBase>(1)
        .expect("xdg_wm_base");

    wm_base.quick_assign(|wm_base, event, _| match event {
        xdg_wm_base::Event::Ping { serial } => wm_base.pong(serial),
        _ => {}
    });

    let surface = compositor.create_surface();
    let xdg_surface = wm_base.get_xdg_surface(&surface);
    xdg_surface.quick_assign(|xdg_surface, event, mut dispatch_data| match event {
        xdg_surface::Event::Configure { serial } => {
            xdg_surface.ack_configure(serial);
            if let Some(configured) = dispatch_data.get::<bool>() {
                *configured = true;
            }
        }
        _ => {}
    });
    let toplevel = xdg_surface.get_toplevel();
    toplevel.set_title("canvas-wm test".into());
    toplevel.quick_assign(|_, event, _| {
        use wayland_protocols::xdg_shell::client::xdg_toplevel::Event as TlEvent;
        if let TlEvent::Configure { width, height, .. } = event {
            println!("toplevel configure {}x{}", width, height);
        }
    });
    surface.commit();

    // Synchronous roundtrip: flushes our requests and blocks until the
    // compositor processed them. No configure back means no window.
    let mut configured = false;
    event_queue
        .sync_roundtrip(&mut configured, |_, _, _| {})
        .expect("roundtrip");
    if !configured {
        eprintln!("test-client: no configure received, no window created");
        std::process::exit(1);
    }
    // Probe loop: two solid buffers, alternating on every frame callback.
    // First CLI arg = frames to paint (default 60); screenshots taken
    // mid-run must show alternating colours, proving live updates.
    let target_frames: u32 = std::env::args()
        .nth(1)
        .and_then(|arg| arg.parse::<u32>().ok())
        .unwrap_or(60);
    let buf_a = make_buffer(&shm, 0xFFAA8866);
    let buf_b = make_buffer(&shm, 0xFF3F7F8F);
    surface.attach(Some(&buf_a), 0, 0);
    surface.commit();
    let mut frames_done: u32 = 0;
    request_frame(&surface, &mut frames_done);
    // A frame() request only takes effect on commit: push it now, then
    // every Done handler below commits its own successor.
    surface.commit();
    while frames_done < target_frames {
        use std::io::ErrorKind;
        if let Err(e) = display.flush() {
            assert_ne!(e.kind(), ErrorKind::Other, "flush: {:?}", e);
        }
        if let Some(guard) = event_queue.prepare_read() {
            if let Err(e) = guard.read_events() {
                assert_eq!(e.kind(), ErrorKind::WouldBlock, "read: {:?}", e);
            }
        }
        // The frame callback below mutates frames_done through dispatch
        // data and swaps the attached buffer; a second state slot carries
        // the buffers and surface across dispatches.
        let mut state = FrameState {
            surface: surface.clone(),
            buf_a: buf_a.clone(),
            buf_b: buf_b.clone(),
        };
        event_queue
            .dispatch_pending(&mut state, |_, _, _| {})
            .expect("dispatch");
        frames_done = state.frames_seen();
    }
    println!("test-client: {} frames painted, exiting", frames_done);
}

/// Per-dispatch state: the frame callback swaps buffers through it.
struct FrameState {
    surface: Main<wayland_client::protocol::wl_surface::WlSurface>,
    buf_a: Main<wayland_client::protocol::wl_buffer::WlBuffer>,
    buf_b: Main<wayland_client::protocol::wl_buffer::WlBuffer>,
}

impl FrameState {
    fn frames_seen(&self) -> u32 {
        FRAME_COUNT.with(|c| c.get())
    }
}

thread_local! {
    static FRAME_COUNT: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// Ask for the next frame callback; on Done, count, swap colour, commit.
fn request_frame(
    surface: &Main<wayland_client::protocol::wl_surface::WlSurface>,
    _seen: &mut u32,
) {
    let cb = surface.frame();
    cb.quick_assign(|_, event, mut dispatch_data| {
        use wayland_client::protocol::wl_callback::Event as CbEvent;
        if let CbEvent::Done { .. } = event {
            FRAME_COUNT.with(|c| c.set(c.get() + 1));
            if let Some(state) = dispatch_data.get::<FrameState>() {
                let n = FRAME_COUNT.with(|c| c.get());
                println!("frame {} done", n);
                let next = if n % 2 == 0 { &state.buf_a } else { &state.buf_b };
                state.surface.attach(Some(next), 0, 0);
                state.surface.commit();
                request_frame(&state.surface, &mut 0);
                state.surface.commit();
            }
        }
    });
}
/// Build one solid-colour shm buffer (caller attaches and commits it).
fn make_buffer(
    shm: &wl_shm::WlShm,
    argb: u32,
) -> Main<wayland_client::protocol::wl_buffer::WlBuffer> {
    const W: i32 = 320;
    const H: i32 = 240;
    const STRIDE: i32 = W * 4;
    const SIZE: i64 = (STRIDE * H) as i64;
    let name = CString::new("canvas-wm-test").unwrap();
    // SAFETY: memfd_create with a valid C string; fd owned by the File below.
    let fd = unsafe { libc::memfd_create(name.as_ptr(), libc::MFD_CLOEXEC) };
    assert!(fd >= 0, "memfd_create failed");
    // SAFETY: fd is a valid owned file descriptor.
    let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
    assert_eq!(unsafe { libc::ftruncate(fd, SIZE) }, 0, "ftruncate failed");
    let pixel = argb.to_ne_bytes();
    for _ in 0..(W * H) {
        file.write_all(&pixel).expect("pool write");
    }
    file.flush().expect("pool flush");
    let pool = shm.create_pool(file.as_raw_fd(), SIZE as i32);
    pool.create_buffer(0, W, H, STRIDE, wl_shm::Format::Argb8888)
}

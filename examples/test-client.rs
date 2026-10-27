//! Minimal frameless xdg test client on wayland-client 0.29 (same generation
//! as the compositor): connects, opens one toplevel, acks configure, exits.
//! Used for end-to-end proof that a real client becomes a canvas window
//! (`new toplevel -> canvas window N` in compositor logs).
//!
//! Run against a live compositor only, never in `cargo test`:
//!   WAYLAND_DISPLAY=... XDG_RUNTIME_DIR=... ./target/debug/examples/test-client

use wayland_client::protocol::wl_compositor::WlCompositor;
use wayland_client::{Display, GlobalManager};
use wayland_protocols::xdg_shell::client::{xdg_surface, xdg_wm_base};

fn main() {
    let display = Display::connect_to_env().expect("connect to compositor");
    let mut event_queue = display.create_event_queue();
    let attached = (*display).clone().attach(event_queue.token());

    let globals = GlobalManager::new(&attached);
    event_queue
        .sync_roundtrip(&mut (), |_, _, _| unreachable!())
        .expect("roundtrip");

    let compositor = globals
        .instantiate_exact::<WlCompositor>(4)
        .expect("wl_compositor");
    let wm_base = globals
        .instantiate_exact::<xdg_wm_base::XdgWmBase>(1)
        .expect("xdg_wm_base");

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
    toplevel.set_title("canvas-wm test".into());
    surface.commit();

    let mut rounds = 0u32;
    loop {
        event_queue
            .dispatch_pending(&mut (), |_, _, _| {})
            .expect("dispatch");
        std::thread::sleep(std::time::Duration::from_millis(200));
        rounds += 1;
        if rounds > 15 {
            break;
        }
    }
    println!("test-client: toplevel open, exiting");
}

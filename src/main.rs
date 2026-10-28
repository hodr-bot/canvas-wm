mod canvas;
mod session;
mod chrome;
mod render3d;
#[cfg(feature = "compositor")]
mod xdg_v3;
#[cfg(feature = "compositor")]
mod backend_winit;
#[cfg(feature = "compositor")]
mod xwm;
#[cfg(feature = "compositor")]
mod keys;
#[cfg(feature = "compositor")]
mod server;

use std::collections::HashMap;

fn main() {
    env_logger::init();
    #[cfg(feature = "compositor")]
    {
        let args: Vec<String> = std::env::args().collect();
        if args.iter().any(|a| a == "--winit") {
            let frames = args
                .windows(2)
                .find(|w| w[0] == "--frames")
                .and_then(|w| w[1].parse::<u64>().ok());
            log::info!("canvas-wm 0.1.0 starting (winit backend)");
            if let Err(err) = backend_winit::run(frames) {
                log::error!("winit backend failed: {}", err);
                std::process::exit(1);
            }
            return;
        }
    }
    log::info!("canvas-wm 0.1.0 starting (lot 0: canvas core)");
    log::info!("Smithay backend: winit for dev, udev+libinput+XWayland for sessions");
    // Lot 0 entry point: pure canvas logic is exercised by `cargo test`.
    // Full Smithay event loop (winit -> udev, XWayland, multi-output
    // viewports on one shared canvas) lands incrementally behind this stub
    // so `nix build` stays green while the compositor grows.
    let windows: HashMap<u64, canvas::CanvasWindow> = HashMap::new();
    let camera = canvas::Camera::new();
    log::info!(
        "canvas ready: {} windows, camera at ({}, {}) zoom {}",
        windows.len(),
        camera.pos.x,
        camera.pos.y,
        camera.zoom
    );
    println!("canvas-wm: canvas core ok (run `cargo test` for proof)");
}

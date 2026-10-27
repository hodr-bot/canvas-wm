mod canvas;

use std::collections::HashMap;

fn main() {
    env_logger::init();
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

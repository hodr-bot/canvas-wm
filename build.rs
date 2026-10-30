// Build script: generate server-side bindings for the vendored wl_drm
// protocol (compositor feature only). wayland-scanner 0.29 matches the
// wayland-server 0.29 that Smithay 0.3 builds on; the crate is already in
// Cargo.lock as a transitive dependency, so no lock change is needed.
fn main() {
    println!("cargo:rerun-if-changed=protocols/drm.xml");
    if std::env::var("CARGO_FEATURE_COMPOSITOR").is_err() {
        return;
    }
    let out = std::env::var("OUT_DIR").expect("OUT_DIR missing");
    let target = format!("{}/drm_server_api.rs", out);
    wayland_scanner::generate_code("protocols/drm.xml", &target, wayland_scanner::Side::Server);
}

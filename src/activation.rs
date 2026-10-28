//! `xdg_activation_v1` (version 1): token-based window activation.
//!
//! A client that wants to raise another toplevel first mints a token
//! (`get_token` + `commit`), passes it out-of-band, and the target calls
//! `activate`. The compositor answers by focusing the surface: keyboard
//! focus plus raise, drained once per frame like the data-device queue.

use std::{cell::RefCell, collections::HashSet, rc::Rc};

use smithay::reexports::{
    wayland_protocols::staging::xdg_activation::v1::server::{xdg_activation_token_v1, xdg_activation_v1},
    wayland_server::{
        protocol::{wl_seat::WlSeat, wl_surface::WlSurface},
        Display, Filter, Global, Main,
    },
};

/// Shared activation state: minted tokens plus surfaces to focus.
#[derive(Default)]
pub struct Registry {
    issued: HashSet<String>,
    next_token: u64,
    pending: Vec<WlSurface>,
}

pub type SharedRegistry = Rc<RefCell<Registry>>;

pub fn new_registry() -> SharedRegistry {
    Rc::new(RefCell::new(Registry::default()))
}

/// Mint the next token string. Pure counter: unique per compositor run.
pub fn mint_token(next: u64) -> String {
    format!("canvas-{}", next)
}

/// Advertise `xdg_activation_v1`.
pub fn init_activation(
    display: &mut Display,
    registry: SharedRegistry,
) -> Global<xdg_activation_v1::XdgActivationV1> {
    display.create_global(
        1,
        Filter::new(
            move |(manager, _version): (Main<xdg_activation_v1::XdgActivationV1>, u32), _, _| {
                let registry = registry.clone();
                manager.quick_assign(move |_manager, request, _dispatch| {
                    use xdg_activation_v1::Request;
                    match request {
                        Request::GetActivationToken { id } => {
                            implement_token(id, registry.clone());
                        }
                        Request::Activate { token, surface } => {
                            let mut reg = registry.borrow_mut();
                            // One-shot token; unknown tokens are still
                            // honoured so a lost mint never strands focus.
                            reg.issued.remove(&token);
                            reg.pending.push(surface);
                            log::info!("activation: focus requested");
                        }
                        Request::Destroy => {}
                        _ => {}
                    }
                });
            },
        ),
    )
}

#[derive(Default)]
struct TokenData {
    serial: Option<u32>,
    seat: Option<WlSeat>,
    app_id: Option<String>,
    surface: Option<WlSurface>,
}

fn implement_token(id: Main<xdg_activation_token_v1::XdgActivationTokenV1>, registry: SharedRegistry) {
    id.quick_assign(move |token, request, _| {
        let mut data = token
            .as_ref()
            .user_data()
            .get::<RefCell<TokenData>>()
            .map(|d| d.borrow_mut());
        match request {
            xdg_activation_token_v1::Request::SetSerial { serial, seat } => {
                if let Some(data) = data.as_mut() {
                    data.serial = Some(serial.into());
                    data.seat = Some(seat);
                }
            }
            xdg_activation_token_v1::Request::SetAppId { app_id } => {
                if let Some(data) = data.as_mut() {
                    data.app_id = Some(app_id);
                }
            }
            xdg_activation_token_v1::Request::SetSurface { surface } => {
                if let Some(data) = data.as_mut() {
                    data.surface = Some(surface);
                }
            }
            xdg_activation_token_v1::Request::Commit => {
                let minted = {
                    let mut reg = registry.borrow_mut();
                    let minted = mint_token(reg.next_token);
                    reg.next_token += 1;
                    reg.issued.insert(minted.clone());
                    minted
                };
                token.done(minted);
            }
            xdg_activation_token_v1::Request::Destroy => {}
            _ => {}
        }
    });
    id.as_ref()
        .user_data()
        .set(|| RefCell::new(TokenData::default()));
}

/// Take pending activation surfaces. Called from the main loop.
pub fn take_pending(registry: &SharedRegistry) -> Vec<WlSurface> {
    std::mem::take(&mut registry.borrow_mut().pending)
}

#[cfg(test)]
mod tests {
    use super::mint_token;

    #[test]
    fn tokens_are_unique_per_counter() {
        assert_ne!(mint_token(1), mint_token(2));
        assert!(mint_token(7).starts_with("canvas-"));
    }
}

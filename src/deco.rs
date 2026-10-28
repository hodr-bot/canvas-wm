//! `zxdg_decoration_manager_v1` (version 1): decoration negotiation.
//!
//! Canvas draws no server-side chrome of its own (window controls live on
//! the canvas overlay), so every toplevel is told to keep client-side
//! decorations. Advertising the manager matters: GTK and Qt probe for it
//! and fall back to guessing when it is missing.

use std::{cell::RefCell, rc::Rc};

use smithay::reexports::{
    wayland_protocols::unstable::xdg_decoration::v1::server::{
        zxdg_decoration_manager_v1, zxdg_toplevel_decoration_v1,
    },
    wayland_server::{Client, Display, Filter, Global, Main, Resource},
};

use zxdg_toplevel_decoration_v1::{Mode, ZxdgToplevelDecorationV1};

/// (owning client, toplevel id) pairs that already hold a decoration
/// object. Object ids are connection-scoped, so the bare id alone would
/// collide across clients; the client handle disambiguates.
type Decorated = Rc<RefCell<Vec<(Option<Client>, u32)>>>;

fn is_decorated(decorated: &[(Option<Client>, u32)], key: &(Option<Client>, u32)) -> bool {
    decorated.iter().any(|(c, i)| {
        *i == key.1
            && match (c, &key.0) {
                (Some(a), Some(b)) => a.equals(b),
                (None, None) => true,
                _ => false,
            }
    })
}

fn remove_decorated(decorated: &mut Vec<(Option<Client>, u32)>, key: &(Option<Client>, u32)) {
    decorated.retain(|(c, i)| {
        !(*i == key.1
            && match (c, &key.0) {
                (Some(a), Some(b)) => a.equals(b),
                (None, None) => true,
                _ => false,
            })
    });
}

/// Advertise the decoration manager. Clients keep client-side decor.
pub fn init_decoration(
    display: &mut Display,
) -> Global<zxdg_decoration_manager_v1::ZxdgDecorationManagerV1> {
    let decorated: Decorated = Rc::new(RefCell::new(Vec::new()));
    display.create_global(
        1,
        Filter::new(
            move |(manager, _version): (
                Main<zxdg_decoration_manager_v1::ZxdgDecorationManagerV1>,
                u32,
            ),
                  _,
                  _| {
                let decorated = decorated.clone();
                manager.quick_assign(move |_manager, request, _dispatch| {
                    use zxdg_decoration_manager_v1::Request;
                    match request {
                        Request::GetToplevelDecoration { id, toplevel } => {
                            let key = (toplevel.as_ref().client(), toplevel.as_ref().id());
                            if is_decorated(&decorated.borrow(), &key) {
                                // One decoration per toplevel: ignore the
                                // duplicate instead of raising a protocol
                                // error that would kill the client.
                                log::info!("decoration: duplicate request ignored");
                                return;
                            }
                            decorated.borrow_mut().push(key.clone());
                            implement_decoration(id, decorated.clone(), key);
                        }
                        Request::Destroy => {}
                        _ => {}
                    }
                });
            },
        ),
    )
}

fn implement_decoration(
    id: Main<ZxdgToplevelDecorationV1>,
    decorated: Decorated,
    key: (Option<Client>, u32),
) {
    id.quick_assign(|decoration, request, _| {
        // Whatever the client prefers, the answer stays client-side:
        // canvas has no SSD path, and flapping modes would flicker.
        match request {
            zxdg_toplevel_decoration_v1::Request::SetMode { .. } => {
                decoration.configure(Mode::ClientSide);
            }
            zxdg_toplevel_decoration_v1::Request::UnsetMode => {
                decoration.configure(Mode::ClientSide);
            }
            zxdg_toplevel_decoration_v1::Request::Destroy => {}
            _ => {}
        }
    });
    // Free the slot when the decoration dies (client destroy or
    // disconnect): object ids are reused, a stale entry would block a
    // later toplevel from a live client.
    id.assign_destructor(Filter::new(
        move |_: Resource<ZxdgToplevelDecorationV1>, _, _| {
            remove_decorated(&mut decorated.borrow_mut(), &key);
        },
    ));
    id.configure(Mode::ClientSide);
    log::info!("decoration: client-side configured");
}

#[cfg(test)]
mod tests {
    use super::{is_decorated, remove_decorated, Mode};

    #[test]
    fn preferred_mode_is_client_side() {
        assert_eq!(Mode::ClientSide as u32, 1);
    }

    #[test]
    fn decorated_tracks_entries_until_removed() {
        let mut entries: Vec<(Option<super::Client>, u32)> = Vec::new();
        let key = (None, 20u32);
        assert!(!is_decorated(&entries, &key));
        entries.push(key.clone());
        assert!(is_decorated(&entries, &key));
        assert!(!is_decorated(&entries, &(None, 21u32)));
        remove_decorated(&mut entries, &key);
        assert!(!is_decorated(&entries, &key));
    }
}

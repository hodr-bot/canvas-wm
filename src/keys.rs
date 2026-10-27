//! Keyboard mapping: scancode -> keysym via xkbcommon, plus Super-tap tracking.
//!
//! Winit reports platform scancodes (X11 keycodes on X11, evdev codes on
//! Wayland). XKB keycodes are evdev+8, so resolution tries the scancode as-is
//! first and falls back to scancode+8 when that yields NoSymbol. This covers
//! both backends without platform sniffing.

use xkbcommon::xkb::{self, KeyDirection, Keycode, Keysym};

pub use xkb::{KEY_Escape, KEY_Super_L, KEY_Super_R};

/// NoSymbol: lookup missed.
pub const NO_SYMBOL: Keysym = 0;

/// Pure fallback logic, testable with a stub lookup.
pub fn resolve_with_lookup(scancode: u32, mut lookup: impl FnMut(Keycode) -> Keysym) -> Keysym {
    let first = lookup(scancode);
    if first != NO_SYMBOL {
        return first;
    }
    lookup(scancode.wrapping_add(8))
}

pub fn is_super(sym: Keysym) -> bool {
    sym == KEY_Super_L || sym == KEY_Super_R
}

/// Tracks Super press/release to detect a clean tap: press and release of
/// Super with no other key pressed in between.
#[derive(Debug, Default)]
pub struct TapTracker {
    super_down: bool,
    other_pressed: bool,
}

impl TapTracker {
    pub fn note_super_press(&mut self) {
        self.super_down = true;
        self.other_pressed = false;
    }

    pub fn note_other_press(&mut self) {
        if self.super_down {
            self.other_pressed = true;
        }
    }

    /// Returns true when this release completes a clean Super tap.
    pub fn note_super_release(&mut self) -> bool {
        let tap = self.super_down && !self.other_pressed;
        self.super_down = false;
        self.other_pressed = false;
        tap
    }
}

/// Stateful xkbcommon mapper. Fails to construct only when the system keymap
/// database is missing (no xkb-data at runtime).
pub struct KeyMapper {
    _context: xkb::Context,
    _keymap: xkb::Keymap,
    state: xkb::State,
}

impl KeyMapper {
    pub fn new() -> Option<Self> {
        let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
        let keymap = xkb::Keymap::new_from_names(
            &context,
            "",
            "",
            "",
            "",
            None,
            xkb::KEYMAP_COMPILE_NO_FLAGS,
        )?;
        let state = xkb::State::new(&keymap);
        Some(Self {
            _context: context,
            _keymap: keymap,
            state,
        })
    }

    /// Feed a raw winit scancode + edge, get the keysym back.
    pub fn keysym(&mut self, scancode: u32, pressed: bool) -> Keysym {
        // Update both candidate keycodes so shift levels stay coherent.
        // (KeyDirection is not Copy, so the edge is computed per call.)
        let down = |p: bool| {
            if p {
                KeyDirection::Down
            } else {
                KeyDirection::Up
            }
        };
        self.state.update_key(scancode, down(pressed));
        self.state
            .update_key(scancode.wrapping_add(8), down(pressed));
        resolve_with_lookup(scancode, |code| self.state.key_get_one_sym(code))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn clean_super_tap_detected() {
        let mut t = TapTracker::default();
        t.note_super_press();
        assert!(t.note_super_release());
    }

    #[test]
    fn super_with_other_key_is_not_a_tap() {
        let mut t = TapTracker::default();
        t.note_super_press();
        t.note_other_press();
        assert!(!t.note_super_release());
    }

    #[test]
    fn release_without_press_is_not_a_tap() {
        let mut t = TapTracker::default();
        assert!(!t.note_super_release());
    }

    #[test]
    fn fallback_adds_eight_on_nosymbol() {
        // Stub: only the +8 code resolves (Wayland-style raw evdev input).
        let mut table = HashMap::new();
        table.insert(9u32, KEY_Escape);
        let sym = resolve_with_lookup(1, |code| table.get(&code).copied().unwrap_or(NO_SYMBOL));
        assert_eq!(sym, KEY_Escape);
    }

    #[test]
    fn direct_hit_wins_over_fallback() {
        // Stub: raw code already resolves (X11-style input); +8 differs.
        let sym = resolve_with_lookup(133, |code| match code {
            133 => KEY_Super_L,
            141 => KEY_Escape,
            _ => NO_SYMBOL,
        });
        assert_eq!(sym, KEY_Super_L);
    }
}

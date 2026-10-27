//! Keyboard mapping: scancode -> keysym via xkbcommon, plus Super-tap tracking.
//!
//! Winit reports evdev scancodes on Linux (X11 and Wayland backends alike).
//! XKB keycodes are evdev+8, so resolution tries scancode+8 first and falls
//! back to the raw scancode when that yields NoSymbol.

use xkbcommon::xkb::{self, KeyDirection, Keycode, Keysym};

pub use xkb::{KEY_Escape, KEY_Super_L, KEY_Super_R};

/// NoSymbol: lookup missed.
pub const NO_SYMBOL: Keysym = 0;

/// Pure fallback logic, testable with a stub lookup.
/// Returns the keysym plus the xkb keycode that produced it, so callers can
/// derive the evdev code (xkb keycode minus 8) for seat forwarding.
///
/// Winit reports evdev scancodes on Linux (both X11 and Wayland backends),
/// and XKB keycodes are evdev+8, so the +8 code is tried first; the raw
/// scancode is only a fallback for exotic platforms where it already is
/// an XKB keycode.
pub fn resolve_with_lookup(
    scancode: u32,
    mut lookup: impl FnMut(Keycode) -> Keysym,
) -> (Keysym, Keycode) {
    let shifted = lookup(scancode.wrapping_add(8));
    if shifted != NO_SYMBOL {
        return (shifted, scancode.wrapping_add(8));
    }
    (lookup(scancode), scancode)
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

    /// Feed a raw winit scancode + edge, get the keysym and the evdev keycode.
    /// The evdev code is what the seat forwards to clients; it is derived
    /// from whichever xkb keycode actually resolved (xkb is evdev+8).
    pub fn resolve(&mut self, scancode: u32, pressed: bool) -> (Keysym, u32) {
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
        let (sym, code) =
            resolve_with_lookup(scancode, |c| self.state.key_get_one_sym(c));
        (sym, code.wrapping_sub(8))
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
    fn shifted_wins_on_evdev_scancode() {
        // Linux winit gives evdev (16 = KEY_Q); xkb wants evdev+8 (24).
        let mut table = HashMap::new();
        table.insert(16u32, 55); // '7' on xkb keycode 16
        table.insert(24u32, 113); // 'q'
        let (sym, code) =
            resolve_with_lookup(16, |c| table.get(&c).copied().unwrap_or(NO_SYMBOL));
        assert_eq!(sym, 113);
        assert_eq!(code, 24);
    }

    #[test]
    fn raw_fallback_when_shifted_misses() {
        // Stub: only the raw code resolves; +8 is NoSymbol.
        let mut table = HashMap::new();
        table.insert(133u32, KEY_Super_L);
        let (sym, code) = resolve_with_lookup(133, |c| {
            table.get(&c).copied().unwrap_or(NO_SYMBOL)
        });
        assert_eq!(sym, KEY_Super_L);
        assert_eq!(code, 133);
    }
}

//! Minimal top bar: geometry only, no rendering here.
//!
//! The bar is a fixed-height strip at the top of the output. One chip per
//! window, left-stacked. Clicking a chip focuses that window. Rendering and
//! input live in the backend; this module stays pure so it is unit-testable.

use crate::canvas::Vec2;

/// Bar height in logical pixels.
pub const BAR_HEIGHT: f64 = 28.0;
/// Chip size and padding in logical pixels.
pub const CHIP_W: f64 = 40.0;
pub const CHIP_H: f64 = 8.0;
pub const CHIP_X0: f64 = 8.0;
pub const CHIP_GAP: f64 = 6.0;

/// Bottom padding of chips inside the bar.
const CHIP_BOTTOM_PAD: f64 = 6.0;

/// A window chip: horizontal position; y/width come from the constants.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Chip {
    /// Index into the backend's sorted window id list.
    pub index: usize,
    pub x: f64,
}

/// Y of the chip row (top edge) inside the bar.
pub fn chip_y() -> f64 {
    BAR_HEIGHT - CHIP_BOTTOM_PAD - CHIP_H
}

/// Chips for `n` windows, left-stacked, clamped to the visible width.
pub fn chips(view_w: f64, n: usize) -> Vec<Chip> {
    let mut out = Vec::new();
    let mut x = CHIP_X0;
    for index in 0..n {
        if x + CHIP_W > view_w {
            break;
        }
        out.push(Chip { index, x });
        x += CHIP_W + CHIP_GAP;
    }
    out
}

/// True when the logical point sits on the bar strip.
pub fn in_bar(point: Vec2) -> bool {
    point.y >= 0.0 && point.y < BAR_HEIGHT
}

/// Chip index under the logical point, if any.
pub fn chip_at(point: Vec2, n: usize, view_w: f64) -> Option<usize> {
    if !in_bar(point) {
        return None;
    }
    let y = chip_y();
    if point.y < y || point.y >= y + CHIP_H {
        return None;
    }
    chips(view_w, n)
        .into_iter()
        .find(|c| point.x >= c.x && point.x < c.x + CHIP_W)
        .map(|c| c.index)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bar_covers_top_strip_only() {
        assert!(in_bar(Vec2::new(100.0, 0.0)));
        assert!(in_bar(Vec2::new(100.0, 27.9)));
        assert!(!in_bar(Vec2::new(100.0, 28.0)));
        assert!(!in_bar(Vec2::new(100.0, 400.0)));
    }

    #[test]
    fn chips_stack_left_with_gap() {
        let cs = chips(1280.0, 3);
        assert_eq!(cs.len(), 3);
        assert_eq!(cs[0].x, CHIP_X0);
        assert_eq!(cs[1].x, CHIP_X0 + CHIP_W + CHIP_GAP);
        assert_eq!(cs[2].index, 2);
    }

    #[test]
    fn chips_clamp_to_visible_width() {
        let cs = chips(100.0, 10);
        assert!(!cs.is_empty());
        assert!(cs.iter().all(|c| c.x + CHIP_W <= 100.0));
    }

    #[test]
    fn chip_hit_needs_chip_row_and_body() {
        let y = chip_y() + 2.0;
        assert_eq!(chip_at(Vec2::new(CHIP_X0 + 2.0, y), 3, 1280.0), Some(0));
        assert_eq!(
            chip_at(Vec2::new(CHIP_X0 + CHIP_W + CHIP_GAP + 2.0, y), 3, 1280.0),
            Some(1)
        );
        // Gap between chips misses.
        assert_eq!(chip_at(Vec2::new(CHIP_X0 + CHIP_W + 2.0, y), 3, 1280.0), None);
        // Bar above the chip row misses.
        assert_eq!(chip_at(Vec2::new(CHIP_X0 + 2.0, 2.0), 3, 1280.0), None);
        // Below the bar misses.
        assert_eq!(chip_at(Vec2::new(CHIP_X0 + 2.0, 400.0), 3, 1280.0), None);
    }
}

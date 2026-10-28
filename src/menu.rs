//! Data-driven Options menu.
//!
//! Adding an option = one [`MenuItem`] literal in [`items()`] plus the
//! setting it flips in `settings.rs`. No layout, input, or rendering
//! code changes: rows lay out top to bottom, navigation wraps, toggles
//! flip through function pointers, and the backend draws whatever rows
//! exist. Headers and footers are display-only.
//!
//! Opened with the OS-standard Settings shortcuts, positional (evdev)
//! so they hold on any layout: Super + physical comma (macOS Cmd+,),
//! Super + physical I (Windows Win+I).

use crate::settings::Settings;

/// Physical comma key: macOS-style Settings shortcut with Super.
pub const KEY_COMMA: u32 = 51;
/// Physical I key: Windows-style Settings shortcut with Super.
pub const KEY_I: u32 = 23;
/// Menu navigation keys (evdev, layout-independent).
pub const KEY_UP: u32 = 103;
pub const KEY_DOWN: u32 = 108;
pub const KEY_ENTER: u32 = 28;
pub const KEY_ESC: u32 = 1;

/// French accent overlay on a base glyph cell.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Accent {
    Acute,
    Grave,
    Circumflex,
    Umlaut,
    Cedilla,
}

/// One printable cell: ASCII base byte plus an optional accent overlay
/// (drawn as solid pixels above/below the base glyph).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Cell {
    pub byte: u8,
    pub accent: Option<Accent>,
}

/// Map a char to its base cell. French accents compose onto ASCII;
/// anything outside latin-1 printables becomes '?'.
pub fn cell_for(ch: char) -> Cell {
    use Accent::*;
    let (base, accent) = match ch {
        'é' => ('e', Some(Acute)),
        'è' => ('e', Some(Grave)),
        'ê' => ('e', Some(Circumflex)),
        'ë' => ('e', Some(Umlaut)),
        'à' => ('a', Some(Grave)),
        'â' => ('a', Some(Circumflex)),
        'ç' => ('c', Some(Cedilla)),
        'î' => ('i', Some(Circumflex)),
        'ï' => ('i', Some(Umlaut)),
        'ô' => ('o', Some(Circumflex)),
        'û' => ('u', Some(Circumflex)),
        'ù' => ('u', Some(Grave)),
        'É' => ('E', Some(Acute)),
        'È' => ('E', Some(Grave)),
        'Ê' => ('E', Some(Circumflex)),
        'À' => ('A', Some(Grave)),
        'Â' => ('A', Some(Circumflex)),
        'Ç' => ('C', Some(Cedilla)),
        'Î' => ('I', Some(Circumflex)),
        'Ô' => ('O', Some(Circumflex)),
        'Û' => ('U', Some(Circumflex)),
        c if (c as u32) < 128 => (c, None),
        _ => ('?', None),
    };
    Cell {
        byte: base as u8,
        accent,
    }
}

/// Accent overlay pixels in cell coordinates (8x8, y down). Lowercase
/// bodies leave rows 0-1 free; capitals use row 0, so their accents sit
/// one row tighter but stay legible at menu scale.
pub fn accent_pixels(accent: Accent) -> &'static [(i32, i32)] {
    match accent {
        Accent::Acute => &[(4, 1), (5, 0)],
        Accent::Grave => &[(4, 0), (5, 1)],
        Accent::Circumflex => &[(3, 1), (4, 0), (5, 1)],
        Accent::Umlaut => &[(2, 0), (5, 0)],
        Accent::Cedilla => &[(4, 7), (5, 7)],
    }
}

/// A menu option. `Toggle` reads/flips live settings; `Info` is static.
pub struct MenuItem {
    pub id: &'static str,
    pub label: &'static str,
    pub help: &'static str,
    pub kind: MenuKind,
}

/// Toggle behaviour as function pointers so items stay plain data.
pub enum MenuKind {
    Toggle {
        is_on: fn(&Settings) -> bool,
        set: fn(&mut Settings, bool),
    },
    Info,
}

/// A display row: headers/feet are static, items are selectable.
pub enum Row {
    Header(&'static str),
    Item(MenuItem),
    Foot(&'static str),
}

/// The whole menu, in order. Sections group toggles; feet document
/// shortcuts. All labels stay ASCII + composed French accents.
pub fn items() -> Vec<Row> {
    vec![
        Row::Header("Affichage"),
        Row::Item(MenuItem {
            id: "bg_anim",
            label: "Fond d'écran animé",
            help: "Shader d'ambiance lent",
            kind: MenuKind::Toggle {
                is_on: |s: &Settings| s.bg_anim,
                set: |s: &mut Settings, v| s.bg_anim = v,
            },
        }),
        Row::Item(MenuItem {
            id: "bg_slow",
            label: "Mouvement lent",
            help: "Vitesse réduite",
            kind: MenuKind::Toggle {
                is_on: |s: &Settings| s.bg_slow,
                set: |s: &mut Settings, v| s.bg_slow = v,
            },
        }),
        Row::Header("Énergie"),
        Row::Item(MenuItem {
            id: "eco",
            label: "Économie d'énergie",
            help: "Fige le fond, assombrit",
            kind: MenuKind::Toggle {
                is_on: |s: &Settings| s.eco,
                set: |s: &mut Settings, v| s.eco = v,
            },
        }),
        Row::Foot("canvas-wm 0.1.0"),
        Row::Foot("Super+, / Super+I : menu"),
        Row::Foot("Echap : fermer"),
    ]
}

/// Number of selectable items.
pub fn item_count() -> usize {
    items().iter().filter(|r| matches!(r, Row::Item(_))).count()
}

/// Open/close + selection cursor. Navigation wraps around.
#[derive(Debug, Default)]
pub struct MenuState {
    pub open: bool,
    pub selected: usize,
}

impl MenuState {
    pub fn up(&mut self) {
        let n = item_count();
        if n == 0 {
            return;
        }
        self.selected = (self.selected + n - 1) % n;
    }

    pub fn down(&mut self) {
        let n = item_count();
        if n == 0 {
            return;
        }
        self.selected = (self.selected + 1) % n;
    }

    /// The currently selected item, if any.
    pub fn selected_item(&self) -> Option<MenuItem> {
        // Index into the item subsequence, not the row list.
        items()
            .into_iter()
            .filter_map(|r| match r {
                Row::Item(it) => Some(it),
                _ => None,
            })
            .nth(self.selected)
    }

    /// Flip the selected toggle. Returns the item id for logging.
    pub fn activate_at(cursor: usize, settings: &mut Settings) -> Option<&'static str> {
        let item = items()
            .into_iter()
            .filter_map(|r| match r {
                Row::Item(it) => Some(it),
                _ => None,
            })
            .nth(cursor)?;
        match item.kind {
            MenuKind::Toggle { is_on, set } => {
                let on = is_on(settings);
                set(settings, !on);
                Some(item.id)
            }
            MenuKind::Info => Some(item.id),
        }
    }
}

/// Panel layout in canvas coordinates: centred on the view centre
/// (canvas coords, e.g. `to_canvas(view / 2)`), so it follows pans.
pub struct Layout {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    /// Row count including headers and feet.
    pub rows: usize,
}

/// Row height in canvas px (text drawn at 2x scale: 16px cells).
pub const ROW_H: f64 = 30.0;
pub const PANEL_W: f64 = 600.0;
pub const PANEL_PAD: f64 = 24.0;

pub fn layout(_view_w: f64, _view_h: f64, center_x: f64, center_y: f64) -> Layout {
    let rows = items().len();
    let h = rows as f64 * ROW_H + PANEL_PAD * 2.0;
    Layout {
        x: center_x - PANEL_W / 2.0,
        y: center_y - h / 2.0,
        w: PANEL_W,
        h,
        rows,
    }
}

/// Which selectable item (if any) sits at canvas point (px, py).
/// Headers and feet are not selectable; returns the item index.
pub fn item_at(layout: &Layout, px: f64, py: f64) -> Option<usize> {
    if px < layout.x || px > layout.x + layout.w {
        return None;
    }
    let rel = py - layout.y - PANEL_PAD;
    if rel < 0.0 {
        return None;
    }
    let row = (rel / ROW_H) as usize;
    if row >= layout.rows {
        return None;
    }
    // Map the display row to the item subsequence.
    let mut item_idx = 0usize;
    for (i, r) in items().iter().enumerate() {
        match r {
            Row::Item(_) => {
                if i == row {
                    return Some(item_idx);
                }
                item_idx += 1;
            }
            _ => {
                if i == row {
                    return None;
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nav_wraps_both_ways() {
        let n = item_count();
        assert!(n >= 3);
        let mut m = MenuState::default();
        m.up();
        assert_eq!(m.selected, n - 1);
        m.down();
        assert_eq!(m.selected, 0);
    }

    #[test]
    fn activate_flips_toggle_and_reports_id() {
        let mut s = Settings::default();
        assert!(s.bg_anim);
        let id = MenuState::activate_at(0, &mut s);
        assert_eq!(id, Some("bg_anim"));
        assert!(!s.bg_anim);
        // Second item is bg_slow (off by default).
        let id = MenuState::activate_at(1, &mut s);
        assert_eq!(id, Some("bg_slow"));
        assert!(s.bg_slow);
    }

    #[test]
    fn french_accents_compose_onto_ascii() {
        assert_eq!(
            cell_for('é'),
            Cell {
                byte: b'e',
                accent: Some(Accent::Acute)
            }
        );
        assert_eq!(
            cell_for('É'),
            Cell {
                byte: b'E',
                accent: Some(Accent::Acute)
            }
        );
        assert_eq!(
            cell_for('ç'),
            Cell {
                byte: b'c',
                accent: Some(Accent::Cedilla)
            }
        );
        assert_eq!(cell_for('z'), Cell { byte: b'z', accent: None });
        assert_eq!(cell_for('€'), Cell { byte: b'?', accent: None });
    }

    #[test]
    fn hit_test_skips_headers_and_misses_outside() {
        // Rows: 0 header, 1 item0, 2 item1, 3 header, 4 item2, 5-7 feet.
        let l = layout(1280.0, 800.0, 0.0, 0.0);
        let row_y = |r: usize| l.y + PANEL_PAD + r as f64 * ROW_H + 4.0;
        assert_eq!(item_at(&l, l.x + 10.0, row_y(0)), None);
        assert_eq!(item_at(&l, l.x + 10.0, row_y(1)), Some(0));
        assert_eq!(item_at(&l, l.x + 10.0, row_y(4)), Some(2));
        assert_eq!(item_at(&l, l.x + 10.0, row_y(6)), None);
        assert_eq!(item_at(&l, l.x - 5.0, row_y(1)), None);
    }
}

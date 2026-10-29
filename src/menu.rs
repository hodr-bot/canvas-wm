//! Data-driven Options menu.
//!
//! Adding an option = one [`MenuItem`] literal in [`items()`] plus the
//! field it reads in `settings.rs`. No layout, input, or rendering
//! code changes: rows lay out top to bottom, navigation wraps, toggles
//! flip and choices cycle through function pointers, and the backend
//! draws whatever rows exist via [`MenuItem::state_text`]. Headers and
//! footers are display-only.
//!
//! Opened with the OS-standard Settings shortcuts, positional (evdev)
//! so they hold on any layout: Super + physical comma (macOS Cmd+,),
//! Super + physical I (Windows Win+I). Inside: Up/Down moves, Left/Right
//! adjusts, Enter flips/toggles-or-advances, click activates, Esc or
//! outside click closes.

use crate::settings::Settings;

/// Physical comma key: macOS-style Settings shortcut with Super.
pub const KEY_COMMA: u32 = 51;
/// Physical I key: Windows-style Settings shortcut with Super.
pub const KEY_I: u32 = 23;
/// Menu navigation keys (evdev, layout-independent).
pub const KEY_UP: u32 = 103;
pub const KEY_DOWN: u32 = 108;
pub const KEY_LEFT: u32 = 105;
pub const KEY_RIGHT: u32 = 106;
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

/// A menu option. `Toggle` reads/flips a bool; `Choice` cycles one of
/// three labelled levels; `Info` is static.
pub struct MenuItem {
    pub id: &'static str,
    pub label: &'static str,
    pub help: &'static str,
    pub kind: MenuKind,
}

/// Toggle/choice behaviour as function pointers so items stay plain data.
pub enum MenuKind {
    Toggle {
        is_on: fn(&Settings) -> bool,
        set: fn(&mut Settings, bool),
    },
    Choice {
        get: fn(&Settings) -> usize,
        set: fn(&mut Settings, usize),
        options: [&'static str; 3],
    },
    Info,
}

impl MenuItem {
    /// Displayed value suffix, e.g. "Vitesse : Normale".
    /// The backend draws `label + state_text`, no per-kind code there.
    pub fn state_text(&self, settings: &Settings) -> String {
        match &self.kind {
            MenuKind::Toggle { is_on, .. } => {
                if is_on(settings) {
                    " : Activé".to_string()
                } else {
                    " : Désactivé".to_string()
                }
            }
            MenuKind::Choice { get, options, .. } => {
                format!(" : {}", options[get(settings).min(2)])
            }
            MenuKind::Info => String::new(),
        }
    }
}

/// A display row: headers/feet are static, items are selectable.
pub enum Row {
    Header(&'static str),
    Item(MenuItem),
    Foot(&'static str),
}

/// Shorthand for a bool row.
fn tog(
    id: &'static str,
    label: &'static str,
    help: &'static str,
    is_on: fn(&Settings) -> bool,
    set: fn(&mut Settings, bool),
) -> Row {
    Row::Item(MenuItem {
        id,
        label,
        help,
        kind: MenuKind::Toggle { is_on, set },
    })
}

/// Shorthand for a 3-level choice row.
fn choice(
    id: &'static str,
    label: &'static str,
    help: &'static str,
    get: fn(&Settings) -> usize,
    set: fn(&mut Settings, usize),
    options: [&'static str; 3],
) -> Row {
    Row::Item(MenuItem {
        id,
        label,
        help,
        kind: MenuKind::Choice { get, set, options },
    })
}

/// The whole menu, in order. Groups nest parameters: Fond d'écran (the
/// wallpaper), Lueurs Super (the Canvas-mode edge glow), Mode Super
/// (the dolly), Énergie (global freeze). All labels stay ASCII +
/// composed French accents (see the font-coverage test).
pub fn items() -> Vec<Row> {
    vec![
        Row::Header("Fond d'écran"),
        tog(
            "bg_anim",
            "Fond animé",
            "Shader d'ambiance",
            |s: &Settings| s.bg_anim,
            |s: &mut Settings, v| s.bg_anim = v,
        ),
        choice(
            "bg_speed",
            "Vitesse",
            "Dérive du fond",
            |s: &Settings| (s.bg_speed.min(2)) as usize,
            |s: &mut Settings, v| s.bg_speed = (v.min(2)) as u8,
            ["Lente", "Normale", "Vive"],
        ),
        choice(
            "bg_halo",
            "Halo",
            "Nappes de lumière",
            |s: &Settings| (s.bg_halo.min(2)) as usize,
            |s: &mut Settings, v| s.bg_halo = (v.min(2)) as u8,
            ["Doux", "Moyen", "Fort"],
        ),
        tog(
            "bg_depth",
            "Relief au zoom",
            "S'assombrit en reculant",
            |s: &Settings| s.bg_depth,
            |s: &mut Settings, v| s.bg_depth = v,
        ),
        Row::Header("Lueurs Super"),
        tog(
            "glow_on",
            "Lueurs de bord",
            "Aurore en mode Super",
            |s: &Settings| s.glow_on,
            |s: &mut Settings, v| s.glow_on = v,
        ),
        choice(
            "glow_intensity",
            "Intensité",
            "Éclat des lueurs",
            |s: &Settings| (s.glow_intensity.min(2)) as usize,
            |s: &mut Settings, v| s.glow_intensity = (v.min(2)) as u8,
            ["Douces", "Moyennes", "Vives"],
        ),
        choice(
            "glow_flow",
            "Flux",
            "Coulée des lueurs",
            |s: &Settings| (s.glow_flow.min(2)) as usize,
            |s: &mut Settings, v| s.glow_flow = (v.min(2)) as u8,
            ["Lent", "Normal", "Vif"],
        ),
        choice(
            "glow_width",
            "Largeur",
            "Bande lumineuse",
            |s: &Settings| (s.glow_width.min(2)) as usize,
            |s: &mut Settings, v| s.glow_width = (v.min(2)) as u8,
            ["Fine", "Moyenne", "Large"],
        ),
        choice(
            "glow_hue",
            "Teinte",
            "Palette des lueurs",
            |s: &Settings| (s.glow_hue.min(2)) as usize,
            |s: &mut Settings, v| s.glow_hue = (v.min(2)) as u8,
            ["Mer", "Brume", "Mousse"],
        ),
        Row::Header("Mode Super"),
        choice(
            "dolly",
            "Recul",
            "Zoom arrière en Super",
            |s: &Settings| (s.dolly.min(2)) as usize,
            |s: &mut Settings, v| s.dolly = (v.min(2)) as u8,
            ["Léger 5%", "Normal 10%", "Large 15%"],
        ),
        Row::Header("Énergie"),
        tog(
            "eco",
            "Économie d'énergie",
            "Fige tout, assombrit",
            |s: &Settings| s.eco,
            |s: &mut Settings, v| s.eco = v,
        ),
        Row::Foot("canvas-wm 0.1.0"),
        Row::Foot("Haut/Bas : choisir, Gauche/Droite : régler"),
        Row::Foot("Entrée : changer, Echap : fermer"),
    ]
}

/// Number of selectable items.
pub fn item_count() -> usize {
    items().iter().filter(|r| matches!(r, Row::Item(_))).count()
}

/// The selectable items in order (index space for cursor/hit-test).
fn item_list() -> Vec<MenuItem> {
    items()
        .into_iter()
        .filter_map(|r| match r {
            Row::Item(it) => Some(it),
            _ => None,
        })
        .collect()
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
        item_list().into_iter().nth(self.selected)
    }

    /// Nudge the selected row: choices cycle (wraps both ways),
    /// toggles set explicitly (Right = on, Left = off). Returns the
    /// item id for logging.
    pub fn adjust(&mut self, dir: i8, settings: &mut Settings) -> Option<&'static str> {
        let item = item_list().into_iter().nth(self.selected)?;
        match item.kind {
            MenuKind::Toggle { is_on, set } => {
                let want = dir > 0;
                if is_on(settings) != want {
                    set(settings, want);
                }
                Some(item.id)
            }
            MenuKind::Choice { get, set, options } => {
                let n = options.len() as i8;
                let next = (((get(settings) as i8 + dir) % n) + n) % n;
                set(settings, next as usize);
                Some(item.id)
            }
            MenuKind::Info => Some(item.id),
        }
    }

    /// Activate the selected row: toggles flip, choices advance (wrap).
    /// Returns the item id for logging.
    pub fn activate_at(cursor: usize, settings: &mut Settings) -> Option<&'static str> {
        let item = item_list().into_iter().nth(cursor)?;
        match item.kind {
            MenuKind::Toggle { is_on, set } => {
                let on = is_on(settings);
                set(settings, !on);
                Some(item.id)
            }
            MenuKind::Choice { get, set, options } => {
                set(settings, (get(settings) + 1) % options.len());
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
/// Panel width fits the longest footer (42 chars at 16px + padding).
pub const PANEL_W: f64 = 740.0;
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
        assert_eq!(n, 11);
        let mut m = MenuState::default();
        m.up();
        assert_eq!(m.selected, n - 1);
        m.down();
        assert_eq!(m.selected, 0);
    }

    #[test]
    fn activate_flips_toggle_and_advances_choice() {
        let mut s = Settings::default();
        assert!(s.bg_anim);
        let id = MenuState::activate_at(0, &mut s);
        assert_eq!(id, Some("bg_anim"));
        assert!(!s.bg_anim);
        // Item 1 is the bg_speed choice (default Normale = 1).
        let id = MenuState::activate_at(1, &mut s);
        assert_eq!(id, Some("bg_speed"));
        assert_eq!(s.bg_speed, 2);
        let id = MenuState::activate_at(1, &mut s);
        assert_eq!(id, Some("bg_speed"));
        assert_eq!(s.bg_speed, 0);
        // Item 1 now reads " : Lente".
        let mut m = MenuState::default();
        m.selected = 1;
        assert_eq!(
            m.selected_item().unwrap().state_text(&s),
            " : Lente".to_string()
        );
    }

    #[test]
    fn adjust_cycles_choices_and_sets_toggles() {
        let mut s = Settings::default();
        let mut m = MenuState::default();
        m.selected = 8; // glow_hue (default Mer = 0).
        assert_eq!(m.adjust(-1, &mut s), Some("glow_hue"));
        assert_eq!(s.glow_hue, 2); // wraps to Mousse.
        assert_eq!(m.adjust(1, &mut s), Some("glow_hue"));
        assert_eq!(s.glow_hue, 0);
        m.selected = 0; // bg_anim toggle.
        m.adjust(-1, &mut s);
        assert!(!s.bg_anim);
        m.adjust(1, &mut s);
        assert!(s.bg_anim);
    }

    /// Every string the menu can draw must survive the 8x8 font: no
    /// '?' fallbacks anywhere (labels, helps, options, headers, feet,
    /// value suffixes). Accents compose; anything else is a bug.
    #[test]
    fn every_menu_string_survives_the_font() {
        let s = Settings::default();
        let mut strings: Vec<String> = Vec::new();
        for r in items() {
            match r {
                Row::Header(t) | Row::Foot(t) => strings.push(t.to_string()),
                Row::Item(it) => {
                    strings.push(it.label.to_string());
                    strings.push(it.help.to_string());
                    strings.push(it.state_text(&s));
                    if let MenuKind::Choice { options, .. } = it.kind {
                        for o in options {
                            strings.push(format!(" : {}", o));
                        }
                    }
                }
            }
        }
        assert!(!strings.is_empty());
        for text in &strings {
            for ch in text.chars() {
                let cell = cell_for(ch);
                assert!(
                    cell.byte != b'?',
                    "unrenderable char {:?} in {:?}",
                    ch,
                    text
                );
            }
        }
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
        // Rows: 0 header, 1-4 items 0-3, 5 header, 6-10 items 4-8,
        // 11 header, 12 item 9, 13 header, 14 item 10, 15-17 feet.
        let l = layout(1280.0, 800.0, 0.0, 0.0);
        let row_y = |r: usize| l.y + PANEL_PAD + r as f64 * ROW_H + 4.0;
        assert_eq!(item_at(&l, l.x + 10.0, row_y(0)), None);
        assert_eq!(item_at(&l, l.x + 10.0, row_y(1)), Some(0));
        assert_eq!(item_at(&l, l.x + 10.0, row_y(6)), Some(4));
        assert_eq!(item_at(&l, l.x + 10.0, row_y(12)), Some(9));
        assert_eq!(item_at(&l, l.x + 10.0, row_y(14)), Some(10));
        assert_eq!(item_at(&l, l.x + 10.0, row_y(16)), None);
        assert_eq!(item_at(&l, l.x - 5.0, row_y(1)), None);
    }
}

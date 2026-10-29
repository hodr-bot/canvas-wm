//! Data-driven Options menu, tabbed.
//!
//! Adding an option = one row literal in the tab table plus the field
//! it reads in `settings.rs`. No layout, input, or rendering code
//! changes: rows lay out top to bottom under a tab bar, navigation
//! wraps inside the tab, toggles flip, choices cycle and sliders step
//! through function pointers, and the backend draws whatever rows
//! exist via [`MenuItem::state_text`]. Colour channels carry a swatch
//! (the backend paints the live colour next to the row).
//!
//! Opened with the OS-standard Settings shortcuts, positional (evdev)
//! so they hold on any layout: Super + physical comma (macOS Cmd+,),
//! Super + physical I (Windows Win+I). Inside: Up/Down moves,
//! Left/Right adjusts, Enter flips-or-steps, Tab switches tabs, click
//! activates (click the tab bar to switch), Esc or outside click
//! closes.

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
/// Tab key: switches Options tabs.
pub const KEY_TAB: u32 = 15;

/// Option tabs, in bar order.
pub const TABS: [&str; 3] = ["Fond", "Vagues", "Super"];
pub const TAB_FOND: usize = 0;
pub const TAB_VAGUES: usize = 1;
pub const TAB_SUPER: usize = 2;

/// A menu option. `Toggle` reads/flips a bool; `Choice` cycles one of
/// three labelled levels; `Slider` steps a u8 in [min, max]; `Info`
/// is static.
pub struct MenuItem {
    pub id: &'static str,
    pub label: &'static str,
    pub help: &'static str,
    pub kind: MenuKind,
    /// Live colour swatch painted next to the row (RGB channels).
    pub swatch: Option<fn(&Settings) -> [u8; 3]>,
}

/// Toggle/choice/slider behaviour as function pointers so items stay
/// plain data.
pub enum MenuKind {
    Toggle {
        is_on: fn(&Settings) -> bool,
        set: fn(&mut Settings, bool),
    },
    Choice {
        get: fn(&Settings) -> usize,
        set: fn(&mut Settings, usize),
        options: &'static [&'static str],
    },
    Slider {
        get: fn(&Settings) -> u8,
        set: fn(&mut Settings, u8),
        min: u8,
        max: u8,
        step: u8,
        unit: &'static str,
    },
    Info,
}

impl MenuItem {
    /// Displayed value suffix, e.g. " : Normale", " : 128", or
    /// " : 15 #0F121A" for the red channel of an RGB triplet (the
    /// swatch row carries the hex preview of the whole colour).
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
                let last = options.len().saturating_sub(1);
                format!(" : {}", options[get(settings).min(last)])
            }
            MenuKind::Slider { get, unit, .. } => {
                match self.swatch.map(|f| f(settings)) {
                    Some(rgb) => format!(
                        " : {} #{:02X}{:02X}{:02X}",
                        get(settings),
                        rgb[0],
                        rgb[1],
                        rgb[2]
                    ),
                    None => format!(" : {}{}", get(settings), unit),
                }
            }
            MenuKind::Info => String::new(),
        }
    }

    /// Slider fill fraction 0.0-1.0 for the track bar (None if not a slider).
    pub fn slider_fill(&self, settings: &Settings) -> Option<f32> {
        match &self.kind {
            MenuKind::Slider { get, min, max, .. } => {
                let v = get(settings).clamp(*min, *max);
                if max > min {
                    Some((v - min) as f32 / (max - min) as f32)
                } else {
                    Some(0.0)
                }
            }
            _ => None,
        }
    }
}

/// A display row: items are selectable, feet are static.
pub enum Row {
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
        swatch: None,
    })
}

/// Shorthand for a categorical row (motif, waveform): any number of
/// named options, cycled by the Left/Right keys.
fn choice(
    id: &'static str,
    label: &'static str,
    help: &'static str,
    get: fn(&Settings) -> usize,
    set: fn(&mut Settings, usize),
    options: &'static [&'static str],
) -> Row {
    Row::Item(MenuItem {
        id,
        label,
        help,
        kind: MenuKind::Choice { get, set, options },
        swatch: None,
    })
}

/// Shorthand for a slider row (percent, channel, or count knob).
fn slider(
    id: &'static str,
    label: &'static str,
    help: &'static str,
    get: fn(&Settings) -> u8,
    set: fn(&mut Settings, u8),
    min: u8,
    max: u8,
    step: u8,
    unit: &'static str,
) -> Row {
    Row::Item(MenuItem {
        id,
        label,
        help,
        kind: MenuKind::Slider {
            get,
            set,
            min,
            max,
            step,
            unit,
        },
        swatch: None,
    })
}

/// Shorthand for one RGB channel row; the red row carries the live
/// colour swatch for the whole triplet.
fn chan(
    id: &'static str,
    label: &'static str,
    help: &'static str,
    get: fn(&Settings) -> u8,
    set: fn(&mut Settings, u8),
    swatch_on_red: Option<fn(&Settings) -> [u8; 3]>,
    is_red: bool,
) -> Row {
    Row::Item(MenuItem {
        id,
        label,
        help,
        kind: MenuKind::Slider {
            get,
            set,
            min: 0,
            max: 255,
            step: 1,
            unit: "",
        },
        swatch: if is_red { swatch_on_red } else { None },
    })
}

/// Rows of one tab, in order. Labels are plain UTF-8 French; the
/// embedded DejaVu face covers them all (see the font-coverage test).
pub fn tab_items(tab: usize) -> Vec<Row> {
    match tab {
        TAB_FOND => vec![
            tog(
                "bg_anim",
                "Fond animé",
                "Shader d'ambiance",
                |s: &Settings| s.bg_anim,
                |s: &mut Settings, v| s.bg_anim = v,
            ),
            choice(
                "bg_pattern",
                "Motif",
                "Style du fond",
                |s: &Settings| s.bg_pattern.min(7) as usize,
                |s: &mut Settings, v| s.bg_pattern = (v.min(7)) as u8,
                &[
                    "Calme",
                    "Dérive",
                    "Nébuleuse",
                    "Topographie",
                    "Mosaïque",
                    "Plasma",
                    "Kaléidoscope",
                    "Grille",
                ],
            ),
            slider(
                "bg_scale",
                "Échelle",
                "Taille du motif",
                |s: &Settings| s.bg_scale.min(100),
                |s: &mut Settings, v| s.bg_scale = v.min(100),
                0,
                100,
                5,
                "%",
            ),
            slider(
                "bg_drift",
                "Dérive",
                "Vitesse du fond",
                |s: &Settings| s.bg_drift.min(100),
                |s: &mut Settings, v| s.bg_drift = v.min(100),
                0,
                100,
                5,
                "%",
            ),
            slider(
                "bg_grain",
                "Grain",
                "Grain de film",
                |s: &Settings| s.bg_grain.min(100),
                |s: &mut Settings, v| s.bg_grain = v.min(100),
                0,
                100,
                5,
                "%",
            ),
            slider(
                "bg_vig",
                "Vignette",
                "Bords assombris",
                |s: &Settings| s.bg_vig.min(100),
                |s: &mut Settings, v| s.bg_vig = v.min(100),
                0,
                100,
                5,
                "%",
            ),
            tog(
                "bg_depth",
                "Relief au zoom",
                "S'assombrit en reculant",
                |s: &Settings| s.bg_depth,
                |s: &mut Settings, v| s.bg_depth = v,
            ),
            chan(
                "bg_tint_r",
                "Teinte rouge",
                "Fond : canal rouge",
                |s: &Settings| s.bg_tint[0],
                |s: &mut Settings, v| s.bg_tint[0] = v,
                Some(|s: &Settings| s.bg_tint),
                true,
            ),
            chan(
                "bg_tint_g",
                "Teinte vert",
                "Fond : canal vert",
                |s: &Settings| s.bg_tint[1],
                |s: &mut Settings, v| s.bg_tint[1] = v,
                None,
                false,
            ),
            chan(
                "bg_tint_b",
                "Teinte bleu",
                "Fond : canal bleu",
                |s: &Settings| s.bg_tint[2],
                |s: &mut Settings, v| s.bg_tint[2] = v,
                None,
                false,
            ),
            tog(
                "eco",
                "Économie d'énergie",
                "Fige tout, assombrit",
                |s: &Settings| s.eco,
                |s: &mut Settings, v| s.eco = v,
            ),
        ],
        TAB_VAGUES => vec![
            tog(
                "wave_on",
                "Vagues",
                "Champ de vagues",
                |s: &Settings| s.wave_on,
                |s: &mut Settings, v| s.wave_on = v,
            ),
            choice(
                "wave_shape",
                "Forme",
                "Forme d'onde",
                |s: &Settings| s.wave_shape.min(5) as usize,
                |s: &mut Settings, v| s.wave_shape = (v.min(5)) as u8,
                &[
                    "Sinus",
                    "Triangle",
                    "Bruit",
                    "Carrée",
                    "Scie",
                    "Impulsion",
                ],
            ),
            slider(
                "wave_octaves",
                "Octaves",
                "Détail des vagues",
                |s: &Settings| s.wave_octaves() as u8,
                |s: &mut Settings, v| s.wave_octaves = (v.saturating_sub(1)) * 34,
                1,
                4,
                1,
                "",
            ),
            slider(
                "wave_freq",
                "Fréquence",
                "Resserré des vagues",
                |s: &Settings| s.wave_freq.min(100),
                |s: &mut Settings, v| s.wave_freq = v.min(100),
                0,
                100,
                5,
                "%",
            ),
            slider(
                "wave_amp",
                "Amplitude",
                "Hauteur des vagues",
                |s: &Settings| s.wave_amp.min(100),
                |s: &mut Settings, v| s.wave_amp = v.min(100),
                0,
                100,
                5,
                "%",
            ),
            slider(
                "wave_speed",
                "Vitesse",
                "Voyage des vagues",
                |s: &Settings| s.wave_speed.min(100),
                |s: &mut Settings, v| s.wave_speed = v.min(100),
                0,
                100,
                5,
                "%",
            ),
            slider(
                "wave_dir",
                "Direction",
                "Cap des vagues",
                |s: &Settings| s.wave_dir.min(100),
                |s: &mut Settings, v| s.wave_dir = v.min(100),
                0,
                100,
                5,
                "%",
            ),
            slider(
                "wave_crisp",
                "Crêtes",
                "Netteté des crêtes",
                |s: &Settings| s.wave_crisp.min(100),
                |s: &mut Settings, v| s.wave_crisp = v.min(100),
                0,
                100,
                5,
                "%",
            ),
            chan(
                "wave_col_r",
                "Couleur rouge",
                "Vagues : canal rouge",
                |s: &Settings| s.wave_col[0],
                |s: &mut Settings, v| s.wave_col[0] = v,
                Some(|s: &Settings| s.wave_col),
                true,
            ),
            chan(
                "wave_col_g",
                "Couleur vert",
                "Vagues : canal vert",
                |s: &Settings| s.wave_col[1],
                |s: &mut Settings, v| s.wave_col[1] = v,
                None,
                false,
            ),
            chan(
                "wave_col_b",
                "Couleur bleu",
                "Vagues : canal bleu",
                |s: &Settings| s.wave_col[2],
                |s: &mut Settings, v| s.wave_col[2] = v,
                None,
                false,
            ),
            tog(
                "glow_on",
                "Lueurs de bord",
                "Aurore en mode Super",
                |s: &Settings| s.glow_on,
                |s: &mut Settings, v| s.glow_on = v,
            ),
            slider(
                "glow_intensity",
                "Éclat",
                "Éclat des lueurs",
                |s: &Settings| s.glow_intensity.min(100),
                |s: &mut Settings, v| s.glow_intensity = v.min(100),
                0,
                100,
                5,
                "%",
            ),
            slider(
                "glow_flow",
                "Coulée",
                "Vitesse des lueurs",
                |s: &Settings| s.glow_flow.min(100),
                |s: &mut Settings, v| s.glow_flow = v.min(100),
                0,
                100,
                5,
                "%",
            ),
            slider(
                "glow_width",
                "Largeur",
                "Bande lumineuse",
                |s: &Settings| s.glow_width.min(100),
                |s: &mut Settings, v| s.glow_width = v.min(100),
                0,
                100,
                5,
                "%",
            ),
            chan(
                "glow_deep_r",
                "Fond rouge",
                "Lueur profonde : rouge",
                |s: &Settings| s.glow_deep[0],
                |s: &mut Settings, v| s.glow_deep[0] = v,
                Some(|s: &Settings| s.glow_deep),
                true,
            ),
            chan(
                "glow_deep_g",
                "Fond vert",
                "Lueur profonde : vert",
                |s: &Settings| s.glow_deep[1],
                |s: &mut Settings, v| s.glow_deep[1] = v,
                None,
                false,
            ),
            chan(
                "glow_deep_b",
                "Fond bleu",
                "Lueur profonde : bleu",
                |s: &Settings| s.glow_deep[2],
                |s: &mut Settings, v| s.glow_deep[2] = v,
                None,
                false,
            ),
            chan(
                "glow_light_r",
                "Clair rouge",
                "Lueur claire : rouge",
                |s: &Settings| s.glow_light[0],
                |s: &mut Settings, v| s.glow_light[0] = v,
                Some(|s: &Settings| s.glow_light),
                true,
            ),
            chan(
                "glow_light_g",
                "Clair vert",
                "Lueur claire : vert",
                |s: &Settings| s.glow_light[1],
                |s: &mut Settings, v| s.glow_light[1] = v,
                None,
                false,
            ),
            chan(
                "glow_light_b",
                "Clair bleu",
                "Lueur claire : bleu",
                |s: &Settings| s.glow_light[2],
                |s: &mut Settings, v| s.glow_light[2] = v,
                None,
                false,
            ),
        ],
        TAB_SUPER => vec![
            slider(
                "dolly_pct",
                "Recul",
                "Zoom gardé en Super",
                |s: &Settings| s.dolly_pct.clamp(50, 100),
                |s: &mut Settings, v| s.dolly_pct = v.clamp(50, 100),
                50,
                100,
                1,
                "%",
            ),
            slider(
                "super_dim",
                "Pénombre",
                "Fond assombri en Super",
                |s: &Settings| s.super_dim.min(100),
                |s: &mut Settings, v| s.super_dim = v.min(100),
                0,
                100,
                5,
                "%",
            ),
        ],
        _ => vec![],
    }
}

/// Feet shared by all tabs: version plus one single-line key hint
/// (two rows total — the Vagues tab already fills small outputs).
pub fn feet() -> Vec<&'static str> {
    vec![
        "canvas-wm 0.2.0",
        "↑↓ : choisir, ←→ : régler, Tab : onglet, Entrée : changer, Echap : fermer",
    ]
}

/// Number of selectable items in a tab.
pub fn tab_item_count(tab: usize) -> usize {
    tab_items(tab)
        .iter()
        .filter(|r| matches!(r, Row::Item(_)))
        .count()
}

/// The selectable items of a tab in order.
fn tab_item_list(tab: usize) -> Vec<MenuItem> {
    tab_items(tab)
        .into_iter()
        .filter_map(|r| match r {
            Row::Item(it) => Some(it),
            _ => None,
        })
        .collect()
}

/// Open/close + tab + selection cursor. Navigation wraps inside tabs.
#[derive(Debug)]
pub struct MenuState {
    pub open: bool,
    pub tab: usize,
    pub selected: usize,
}

impl Default for MenuState {
    fn default() -> Self {
        Self {
            open: false,
            tab: TAB_FOND,
            selected: 0,
        }
    }
}

impl MenuState {
    /// Switch tab (wraps), resetting the cursor to the first row.
    pub fn set_tab(&mut self, tab: usize) {
        self.tab = tab % TABS.len();
        self.selected = 0;
    }

    pub fn next_tab(&mut self) {
        self.set_tab(self.tab + 1);
    }

    pub fn up(&mut self) {
        let n = tab_item_count(self.tab);
        if n == 0 {
            return;
        }
        self.selected = (self.selected + n - 1) % n;
    }

    pub fn down(&mut self) {
        let n = tab_item_count(self.tab);
        if n == 0 {
            return;
        }
        self.selected = (self.selected + 1) % n;
    }

    /// The currently selected item, if any.
    pub fn selected_item(&self) -> Option<MenuItem> {
        // Index into the tab's item subsequence, not the row list.
        tab_item_list(self.tab).into_iter().nth(self.selected)
    }

    /// Nudge the selected row: choices cycle (wraps both ways),
    /// sliders step and clamp, toggles set explicitly (Right = on,
    /// Left = off). Returns the item id for logging.
    pub fn adjust(&mut self, dir: i8, settings: &mut Settings) -> Option<&'static str> {
        let item = tab_item_list(self.tab).into_iter().nth(self.selected)?;
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
            MenuKind::Slider { get, set, min, max, step, .. } => {
                let v = get(settings).clamp(min, max) as i16;
                let next = (v + dir as i16 * step as i16).clamp(min as i16, max as i16);
                set(settings, next as u8);
                Some(item.id)
            }
            MenuKind::Info => Some(item.id),
        }
    }

    /// Activate the selected row: toggles flip, choices advance
    /// (wrap), sliders step forward (wrap to min past max).
    /// Returns the item id for logging.
    pub fn activate_at(
        tab: usize,
        cursor: usize,
        settings: &mut Settings,
    ) -> Option<&'static str> {
        let item = tab_item_list(tab).into_iter().nth(cursor)?;
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
            MenuKind::Slider { get, set, min, max, step, .. } => {
                let v = get(settings).clamp(min, max);
                let next = if v + step > max { min } else { v + step };
                set(settings, next);
                Some(item.id)
            }
            MenuKind::Info => Some(item.id),
        }
    }
}

/// Panel layout in screen px: centred on the view centre (screen
/// coords), so pans and zooms can neither move nor resize it.
pub struct Layout {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    /// Display rows: 1 tab bar + tab rows + feet.
    pub rows: usize,
    pub tab: usize,
}

/// Row height in screen px: 16px type at TEXT_DY, slider track
/// pinned above the row bottom with daylight between the two.
/// The tallest tab must clear 800px-tall outputs (see the layout
/// test below): keep rows tight, feet short.
pub const ROW_H: f64 = 30.0;
/// Text baseline offset from the row top (16px face).
pub const TEXT_DY: f64 = 6.0;
/// Slider track height and its gap to the row bottom.
pub const TRACK_H: f64 = 3.0;
pub const TRACK_PAD_BOTTOM: f64 = 4.0;
/// Panel width (labels + values + slider tracks + swatches).
pub const PANEL_W: f64 = 800.0;
pub const PANEL_PAD: f64 = 24.0;

pub fn layout(tab: usize, center_x: f64, center_y: f64) -> Layout {
    let rows = 1 + tab_items(tab).len() + feet().len();
    let h = rows as f64 * ROW_H + PANEL_PAD * 2.0;
    Layout {
        x: center_x - PANEL_W / 2.0,
        y: center_y - h / 2.0,
        w: PANEL_W,
        h,
        rows,
        tab,
    }
}

/// Which tab (if any) sits at screen point (px, py) in the tab bar.
pub fn tab_at(layout: &Layout, px: f64, py: f64) -> Option<usize> {
    let top = layout.y + PANEL_PAD;
    if py < top || py > top + ROW_H {
        return None;
    }
    if px < layout.x || px > layout.x + layout.w {
        return None;
    }
    let cell = layout.w / TABS.len() as f64;
    let idx = ((px - layout.x) / cell) as usize;
    if idx < TABS.len() {
        Some(idx)
    } else {
        None
    }
}

/// Which selectable item (if any) sits at screen point (px, py).
/// Row 0 is the tab bar (see [`tab_at`]); feet are not selectable;
/// returns the item index inside the tab.
pub fn item_at(layout: &Layout, px: f64, py: f64) -> Option<usize> {
    if px < layout.x || px > layout.x + layout.w {
        return None;
    }
    let rel = py - layout.y - PANEL_PAD - ROW_H; // skip the tab bar
    if rel < 0.0 {
        return None;
    }
    let row = (rel / ROW_H) as usize;
    let rows = tab_items(layout.tab);
    if row >= rows.len() {
        return None;
    }
    // Map the display row to the tab's item subsequence.
    let mut item_idx = 0usize;
    for (i, r) in rows.iter().enumerate() {
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
    fn tabs_hold_34_options() {
        assert_eq!(TABS.len(), 3);
        assert_eq!(tab_item_count(TAB_FOND), 11);
        // Waves absorb the old Lueurs rows (toggle + 3 sliders + 6
        // channels on top of the 11 wave rows).
        assert_eq!(tab_item_count(TAB_VAGUES), 21);
        assert_eq!(tab_item_count(TAB_SUPER), 2);
        let total: usize = (0..TABS.len()).map(tab_item_count).sum();
        assert_eq!(total, 34);
    }

    #[test]
    fn tab_switch_wraps_and_resets_cursor() {
        let mut m = MenuState::default();
        assert_eq!(m.tab, TAB_FOND);
        m.selected = 5;
        m.next_tab();
        assert_eq!(m.tab, TAB_VAGUES);
        assert_eq!(m.selected, 0);
        m.set_tab(99);
        assert_eq!(m.tab, 99 % TABS.len());
        assert_eq!(m.selected, 0);
    }

    #[test]
    fn nav_wraps_inside_each_tab() {
        for tab in 0..TABS.len() {
            let mut m = MenuState::default();
            m.set_tab(tab);
            let n = tab_item_count(tab);
            assert!(n > 0);
            m.up();
            assert_eq!(m.selected, n - 1);
            m.down();
            assert_eq!(m.selected, 0);
        }
    }

    #[test]
    fn sliders_step_clamp_and_wrap_on_activate() {
        let mut s = Settings::default();
        let mut m = MenuState::default();
        m.set_tab(TAB_FOND);
        // Item 2 is bg_scale (default 50, step 5).
        m.selected = 2;
        assert_eq!(m.adjust(1, &mut s), Some("bg_scale"));
        assert_eq!(s.bg_scale, 55);
        assert_eq!(m.adjust(-1, &mut s), Some("bg_scale"));
        assert_eq!(s.bg_scale, 50);
        // Clamp at the top.
        s.bg_scale = 98;
        m.adjust(1, &mut s);
        assert_eq!(s.bg_scale, 100);
        m.adjust(1, &mut s);
        assert_eq!(s.bg_scale, 100);
        // RGB channel: full 0-255 range, step 1.
        m.selected = 7; // bg_tint_r.
        s.bg_tint[0] = 254;
        m.adjust(1, &mut s);
        assert_eq!(s.bg_tint[0], 255);
        m.adjust(1, &mut s);
        assert_eq!(s.bg_tint[0], 255);
        // Activate wraps past max back to min.
        let id = MenuState::activate_at(TAB_VAGUES, 2, &mut s);
        assert_eq!(id, Some("wave_octaves"));
        assert_eq!(s.wave_octaves(), 3); // was 2 (default 50).
        assert_eq!(s.wave_octaves, 68);
    }

    #[test]
    fn activate_flips_toggle_and_advances_choice() {
        let mut s = Settings::default();
        assert!(s.bg_anim);
        let id = MenuState::activate_at(TAB_FOND, 0, &mut s);
        assert_eq!(id, Some("bg_anim"));
        assert!(!s.bg_anim);
        // Item 1 is the bg_pattern choice (default Dérive = 1).
        let id = MenuState::activate_at(TAB_FOND, 1, &mut s);
        assert_eq!(id, Some("bg_pattern"));
        assert_eq!(s.bg_pattern, 2);
        // Full cycle through the 8 motifs wraps back to Calme = 0.
        for _ in 0..6 {
            let id = MenuState::activate_at(TAB_FOND, 1, &mut s);
            assert_eq!(id, Some("bg_pattern"));
        }
        assert_eq!(s.bg_pattern, 0);
        // Item 1 now reads " : Calme".
        let mut m = MenuState::default();
        m.set_tab(TAB_FOND);
        m.selected = 1;
        assert_eq!(
            m.selected_item().unwrap().state_text(&s),
            " : Calme".to_string()
        );
    }

    #[test]
    fn adjust_cycles_choices_and_steps_sliders() {
        let mut s = Settings::default();
        let mut m = MenuState::default();
        // bg_pattern choice cycles through its 8 motifs.
        m.set_tab(TAB_FOND);
        m.selected = 1;
        assert_eq!(m.adjust(1, &mut s), Some("bg_pattern"));
        assert_eq!(s.bg_pattern, 2);
        for _ in 0..6 {
            assert_eq!(m.adjust(1, &mut s), Some("bg_pattern"));
        }
        assert_eq!(s.bg_pattern, 0);
        // glow sliders step freely 0-100 now (no fixed levels).
        m.set_tab(TAB_VAGUES);
        m.selected = 12; // glow_intensity (default 60, step 5).
        assert_eq!(m.adjust(1, &mut s), Some("glow_intensity"));
        assert_eq!(s.glow_intensity, 65);
        // dolly slider spans 50-100 with step 1.
        m.set_tab(TAB_SUPER);
        m.selected = 0; // dolly_pct (default 90).
        assert_eq!(m.adjust(-1, &mut s), Some("dolly_pct"));
        assert_eq!(s.dolly_pct, 89);
        s.dolly_pct = 50;
        m.adjust(-1, &mut s);
        assert_eq!(s.dolly_pct, 50);
        m.set_tab(TAB_FOND);
        m.selected = 0; // bg_anim toggle.
        m.adjust(-1, &mut s);
        assert!(!s.bg_anim);
        m.adjust(1, &mut s);
        assert!(s.bg_anim);
    }

    #[test]
    fn swatch_and_slider_fill_read_live_values() {
        let s = Settings::default();
        let mut m = MenuState::default();
        m.set_tab(TAB_FOND);
        m.selected = 7; // bg_tint_r carries the swatch.
        let it = m.selected_item().unwrap();
        assert_eq!(it.swatch.unwrap()(&s), [15, 18, 26]);
        // The swatch row carries the hex preview of the triplet.
        assert_eq!(it.state_text(&s), " : 15 #0F121A".to_string());
        // Plain channels show the bare value.
        m.selected = 8; // bg_tint_g: no swatch, no hex.
        assert_eq!(
            m.selected_item().unwrap().state_text(&s),
            " : 18".to_string()
        );
        // A channel row is also a slider: fill tracks the value.
        let fill = it.slider_fill(&s).unwrap();
        assert!((fill - 15.0 / 255.0).abs() < 1e-6);
        // Non-slider rows report no fill.
        m.selected = 0; // bg_anim toggle.
        assert_eq!(m.selected_item().unwrap().slider_fill(&s), None);
    }

    /// Every string the menu can draw must rasterize in the vector
    /// face: no tofu anywhere (tabs, labels, helps, options, feet,
    /// value suffixes). Covered by fontdue, not by cell fallbacks.
    #[test]
    fn every_menu_string_survives_the_font() {
        let s = Settings::default();
        let probe = crate::fontvec::FontCache::new();
        let mut strings: Vec<String> = Vec::new();
        for t in TABS {
            strings.push(t.to_string());
        }
        for tab in 0..TABS.len() {
            for r in tab_items(tab) {
                match r {
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
                    Row::Foot(t) => strings.push(t.to_string()),
                }
            }
        }
        for f in feet() {
            strings.push(f.to_string());
        }
        // Slider extremes must survive too (" : 255", " : 100%").
        strings.push(" : 255".to_string());
        strings.push(" : 100%".to_string());
        assert!(!strings.is_empty());
        for text in &strings {
            for ch in text.chars() {
                if ch.is_whitespace() {
                    continue;
                }
                let (m, px) =
                    probe.font_rasterize(ch, crate::fontvec::MENU_PX);
                assert!(
                    m.width > 0 && px.iter().any(|a| *a > 0),
                    "unrenderable char {:?} in {:?}",
                    ch,
                    text
                );
            }
        }
    }

    #[test]
    fn text_and_slider_track_never_share_a_row_band() {
        // The 16px face at TEXT_DY must end above the slider track
        // pinned to the row bottom: regressions here paint the track
        // straight through the glyphs.
        let text_bottom = TEXT_DY + crate::fontvec::MENU_PX as f64;
        let track_top = ROW_H - TRACK_PAD_BOTTOM - TRACK_H;
        assert!(
            text_bottom < track_top,
            "text ends at {} but the track starts at {}",
            text_bottom,
            track_top
        );
        // The tallest tab (Vagues, 21 rows + tabs + feet) still fits
        // a 800px-tall output with room to breathe.
        let l = layout(TAB_VAGUES, 640.0, 400.0);
        assert!(l.h <= 780.0, "Vagues panel {}px overflows", l.h);
        assert!(l.y >= 10.0, "Vagues panel top {} off-screen", l.y);
        // The single-line key hint fits the panel width.
        let hint = feet()[1];
        let w = crate::fontvec::FontCache::new().measure(hint) as f64;
        assert!(
            w < PANEL_W - PANEL_PAD * 2.0,
            "key hint {}px overflows the panel",
            w
        );
    }

    #[test]
    fn hit_test_skips_tab_bar_and_misses_outside() {
        // Fond tab rows: 0 = tab bar, 1-11 items 0-10, then feet.
        let l = layout(TAB_FOND, 0.0, 0.0);
        let row_y = |r: usize| l.y + PANEL_PAD + r as f64 * ROW_H + 4.0;
        assert_eq!(tab_at(&l, l.x + 10.0, row_y(0)), Some(0));
        assert_eq!(
            tab_at(&l, l.x + l.w * 0.4, row_y(0)),
            Some(1)
        );
        assert_eq!(tab_at(&l, l.x + l.w * 0.8, row_y(0)), Some(2));
        assert_eq!(tab_at(&l, l.x + 10.0, row_y(1)), None);
        assert_eq!(item_at(&l, l.x + 10.0, row_y(0)), None);
        assert_eq!(item_at(&l, l.x + 10.0, row_y(1)), Some(0));
        assert_eq!(item_at(&l, l.x + 10.0, row_y(11)), Some(10));
        assert_eq!(item_at(&l, l.x + 10.0, row_y(12)), None); // foot
        assert_eq!(item_at(&l, l.x - 5.0, row_y(1)), None);
    }
}

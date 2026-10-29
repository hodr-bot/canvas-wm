//! Shared user settings, edited through the Options menu.
//!
//! The menu owns the UI; this struct owns the values. Every frame the
//! backend reads them (background animation, Super edge glow, dolly).
//! Parameters nest in groups (Fond / Lueurs Super / Mode Super): each
//! group is a header in `menu.rs`, each parameter one line of data
//! there. Adding a parameter = a field + default + accessor here, one
//! `MenuItem` literal there. Choices are always 3 labelled levels with
//! a numeric meaning (`CHOICE_VALUES`-style tables per parameter).

/// Live user settings (Options menu).
#[derive(Debug, Clone)]
pub struct Settings {
    // -- Fond d'écran --
    /// Animated ambient wallpaper (procedural shader). Off = flat clear.
    pub bg_anim: bool,
    /// Background drift speed level: 0 Lente, 1 Normale, 2 Vive.
    pub bg_speed: u8,
    /// Halo intensity level: 0 Doux, 1 Moyen, 2 Fort.
    pub bg_halo: u8,
    /// Relief with zoom-out (depth darkening). Off = flat at any zoom.
    pub bg_depth: bool,
    /// Energy saver: freezes all motion and dims slightly.
    pub eco: bool,
    // -- Lueurs Super (edge glow in Canvas mode) --
    /// Master switch for the Super edge glow.
    pub glow_on: bool,
    /// Glow brightness level: 0 Douces, 1 Moyennes, 2 Vives.
    pub glow_intensity: u8,
    /// Flow speed level: 0 Lent, 1 Normal, 2 Vif.
    pub glow_flow: u8,
    /// Band width level: 0 Fines, 1 Moyennes, 2 Larges.
    pub glow_width: u8,
    /// Palette level: 0 Mer, 1 Brume, 2 Mousse.
    pub glow_hue: u8,
    // -- Mode Super --
    /// Dolly-back depth level: 0 Léger (5%), 1 Normal (10%), 2 Large (15%).
    pub dolly: u8,
}

/// Numeric meaning of the 3 speed levels (time multiplier).
pub const BG_SPEED_VALUES: [f32; 3] = [0.35, 1.0, 2.0];
/// Halo brightness multiplier per level.
pub const BG_HALO_VALUES: [f32; 3] = [0.5, 1.0, 1.6];
/// Glow brightness per level.
pub const GLOW_INTENSITY_VALUES: [f32; 3] = [0.35, 0.7, 1.2];
/// Glow flow speed per level (own time multiplier).
pub const GLOW_FLOW_VALUES: [f32; 3] = [0.4, 1.0, 2.2];
/// Glow band width per level (fraction of the half-view).
pub const GLOW_WIDTH_VALUES: [f32; 3] = [0.06, 0.12, 0.20];
/// Dolly-back factor per level (zoom multiplier on Super enter).
pub const DOLLY_VALUES: [f64; 3] = [0.95, 0.90, 0.85];
/// Glow palettes (deep colour, light colour) per hue level:
/// Mer (teal), Brume (violet), Mousse (green). Dark and desaturated,
/// ambience-grade — never neon.
pub const GLOW_HUES: [([f32; 3], [f32; 3]); 3] = [
    ([0.05, 0.35, 0.38], [0.20, 0.75, 0.70]),
    ([0.25, 0.16, 0.42], [0.55, 0.42, 0.85]),
    ([0.12, 0.32, 0.14], [0.35, 0.70, 0.32]),
];

/// Clamp a level to its 3 options (corrupt values can't escape).
fn level(v: u8) -> usize {
    (v.min(2)) as usize
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            bg_anim: true,
            bg_speed: 1,
            bg_halo: 1,
            bg_depth: true,
            eco: false,
            glow_on: true,
            glow_intensity: 1,
            glow_flow: 1,
            glow_width: 1,
            glow_hue: 0,
            dolly: 1,
        }
    }
}

impl Settings {
    /// Background time scale (0 = frozen).
    pub fn bg_speed(&self) -> f32 {
        if !self.bg_anim || self.eco {
            0.0
        } else {
            BG_SPEED_VALUES[level(self.bg_speed)]
        }
    }

    /// Halo brightness multiplier.
    pub fn halo(&self) -> f32 {
        BG_HALO_VALUES[level(self.bg_halo)]
    }

    /// Zoom depth on (1.0) or off (0.0) for the shader.
    pub fn depth_on(&self) -> f32 {
        if self.bg_depth {
            1.0
        } else {
            0.0
        }
    }

    /// How often the background repaints while anything moves: a slow
    /// drift, never a hot loop. None = paint once (or never again).
    /// The Super glow pulses on its own clock, so it holds frames in
    /// Canvas mode even with the wallpaper frozen or off.
    pub fn bg_frame_interval(&self, glow_animating: bool) -> Option<std::time::Duration> {
        if self.bg_speed() > 0.0 || glow_animating {
            Some(std::time::Duration::from_millis(120))
        } else {
            None
        }
    }

    /// Background brightness multiplier (eco dims slightly).
    pub fn bg_dim(&self) -> f32 {
        if self.eco {
            0.8
        } else {
            1.0
        }
    }

    /// Glow brightness (0 when disabled: shader skips the band).
    pub fn glow_intensity(&self) -> f32 {
        if !self.glow_on {
            0.0
        } else {
            GLOW_INTENSITY_VALUES[level(self.glow_intensity)]
        }
    }

    /// Glow flow speed (0 = frozen, shares the eco freeze).
    pub fn glow_flow(&self) -> f32 {
        if self.eco {
            0.0
        } else {
            GLOW_FLOW_VALUES[level(self.glow_flow)]
        }
    }

    /// Glow band width (view fraction).
    pub fn glow_width(&self) -> f32 {
        GLOW_WIDTH_VALUES[level(self.glow_width)]
    }

    /// Glow palette (deep, light).
    pub fn glow_hue(&self) -> ([f32; 3], [f32; 3]) {
        GLOW_HUES[level(self.glow_hue)]
    }

    /// Super dolly-back factor.
    pub fn dolly_factor(&self) -> f64 {
        DOLLY_VALUES[level(self.dolly)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_animate_with_glow_without_eco() {
        let s = Settings::default();
        assert!(s.bg_anim && !s.eco && s.glow_on);
        assert_eq!(s.bg_speed(), 1.0);
        assert!(s.bg_frame_interval(false).is_some());
        assert_eq!(s.dolly_factor(), 0.90);
    }

    #[test]
    fn eco_freezes_and_dims_everything() {
        let mut s = Settings::default();
        s.eco = true;
        assert_eq!(s.bg_speed(), 0.0);
        assert_eq!(s.glow_flow(), 0.0);
        assert!(s.bg_frame_interval(false).is_none());
        assert!(s.bg_dim() < 1.0);
    }

    #[test]
    fn glow_holds_frames_with_wallpaper_off() {
        let mut s = Settings::default();
        s.bg_anim = false;
        // Frozen wallpaper alone: no frames...
        assert!(s.bg_frame_interval(false).is_none());
        // ...but a pulsing glow still needs its cadence.
        assert!(s.bg_frame_interval(true).is_some());
    }

    #[test]
    fn levels_clamp_and_map() {
        let mut s = Settings::default();
        s.bg_speed = 9;
        s.glow_hue = 255;
        assert_eq!(s.bg_speed(), 2.0);
        let (deep, _) = s.glow_hue();
        assert!((deep[0] - 0.12).abs() < 1e-6);
        s.dolly = 0;
        assert_eq!(s.dolly_factor(), 0.95);
    }
}

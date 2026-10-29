//! Shared user settings, edited through the Options menu.
//!
//! The menu owns the UI; this struct owns the values. Every frame the
//! backend reads them (wallpaper, waves, Super edge glow, dolly).
//! Parameters nest in per-tab groups (Fond / Vagues / Super):
//! each tab is a section in `menu.rs`, each parameter one line of data
//! there. Adding a parameter = a field + default + accessor here, one
//! `MenuItem` literal there. Continuous knobs are `u8` percents
//! (0-100, RGB channels 0-255) mapped to shader ranges by accessors;
//! genuinely categorical knobs (motif, waveform) stay `Choice`s with
//! an arbitrary option count.

/// Live user settings (Options menu).
#[derive(Debug, Clone)]
pub struct Settings {
    // -- Fond d'écran (procedural wallpaper) --
    /// Animated ambient wallpaper (procedural shader). Off = flat clear.
    pub bg_anim: bool,
    /// Wallpaper motif: 0 Calme, 1 Dérive, 2 Nébuleuse.
    pub bg_pattern: u8,
    /// Motif scale percent (0 = tight, 100 = wide).
    pub bg_scale: u8,
    /// Drift speed percent (0 = frozen).
    pub bg_drift: u8,
    /// Film grain percent.
    pub bg_grain: u8,
    /// Vignette strength percent.
    pub bg_vig: u8,
    /// Base tint, exact RGB channels.
    pub bg_tint: [u8; 3],
    /// Relief with zoom-out (depth darkening). Off = flat at any zoom.
    pub bg_depth: bool,
    /// Energy saver: freezes all motion and dims slightly.
    pub eco: bool,
    // -- Vagues (procedural wave field over the wallpaper) --
    /// Master switch for the wave field.
    pub wave_on: bool,
    /// Waveform: 0 Sinus, 1 Triangle, 2 Bruit.
    pub wave_shape: u8,
    /// Octave count percent mapped to 1-4.
    pub wave_octaves: u8,
    /// Spatial frequency percent.
    pub wave_freq: u8,
    /// Amplitude percent.
    pub wave_amp: u8,
    /// Travel speed percent (0 = frozen).
    pub wave_speed: u8,
    /// Travel direction percent mapped to a full turn.
    pub wave_dir: u8,
    /// Crest sharpness percent (sine soft .. ridged).
    pub wave_crisp: u8,
    /// Wave colour, exact RGB channels.
    pub wave_col: [u8; 3],
    // -- Lueurs Super (edge glow in Canvas mode) --
    /// Master switch for the Super edge glow.
    pub glow_on: bool,
    /// Glow brightness percent (0 = faint, 100 = blazing).
    pub glow_intensity: u8,
    /// Flow speed percent (0 = frozen).
    pub glow_flow: u8,
    /// Band width percent (0 = hairline, 100 = wide wash).
    pub glow_width: u8,
    /// Deep glow colour, exact RGB channels.
    pub glow_deep: [u8; 3],
    /// Light glow colour, exact RGB channels.
    pub glow_light: [u8; 3],
    // -- Mode Super --
    /// Dolly-back depth percent: zoom kept on Super enter
    /// (50 = half zoom, 100 = no dolly).
    pub dolly_pct: u8,
    /// Wallpaper dimming in Super mode, percent.
    pub super_dim: u8,
}

/// Map a u8 percent to 0.0-1.0.
fn pct(v: u8) -> f32 {
    (v.min(100) as f32) / 100.0
}

/// Clamp a categorical level to its option count (corrupt values
/// can't escape).
fn lvl(v: u8, n: usize) -> usize {
    (v as usize).min(n.saturating_sub(1))
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            bg_anim: true,
            bg_pattern: 1,
            bg_scale: 50,
            bg_drift: 50,
            bg_grain: 20,
            bg_vig: 60,
            bg_tint: [15, 18, 26],
            bg_depth: true,
            eco: false,
            wave_on: true,
            wave_shape: 0,
            wave_octaves: 50,
            wave_freq: 40,
            wave_amp: 35,
            wave_speed: 50,
            wave_dir: 15,
            wave_crisp: 35,
            wave_col: [32, 120, 112],
            glow_on: true,
            glow_intensity: 60,
            glow_flow: 50,
            glow_width: 50,
            glow_deep: [13, 89, 97],
            glow_light: [51, 191, 179],
            dolly_pct: 90,
            super_dim: 25,
        }
    }
}

impl Settings {
    /// Wallpaper time scale (0 = frozen).
    pub fn bg_speed(&self) -> f32 {
        if !self.bg_anim || self.eco {
            0.0
        } else {
            pct(self.bg_drift) * 2.0
        }
    }

    /// Wallpaper motif selector (0 Calme, 1 Dérive, 2 Nébuleuse).
    pub fn bg_pattern(&self) -> usize {
        lvl(self.bg_pattern, 3)
    }

    /// Motif scale (0.5 tight .. 2.0 wide).
    pub fn bg_scale(&self) -> f32 {
        0.5 + pct(self.bg_scale) * 1.5
    }

    /// Film grain amount (0 .. 0.15).
    pub fn bg_grain(&self) -> f32 {
        pct(self.bg_grain) * 0.15
    }

    /// Vignette strength (0 .. 1).
    pub fn bg_vig(&self) -> f32 {
        pct(self.bg_vig)
    }

    /// Base wallpaper tint (linear 0-1).
    pub fn bg_tint(&self) -> [f32; 3] {
        let c = self.bg_tint;
        [c[0] as f32 / 255.0, c[1] as f32 / 255.0, c[2] as f32 / 255.0]
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
    /// Waves and the Super glow pulse on their own clocks, so they
    /// hold frames in Canvas mode even with the wallpaper frozen.
    pub fn bg_frame_interval(&self, glow_animating: bool) -> Option<std::time::Duration> {
        let waves_move = self.wave_on && !self.eco && self.wave_speed > 0;
        if self.bg_speed() > 0.0 || waves_move || glow_animating {
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

    /// Wallpaper dimming while in Super mode (1 = none).
    pub fn super_dim(&self) -> f32 {
        1.0 - pct(self.super_dim) * 0.5
    }

    /// Waveform selector (0 Sinus, 1 Triangle, 2 Bruit).
    pub fn wave_shape(&self) -> usize {
        lvl(self.wave_shape, 3)
    }

    /// Wave octave count (1-4).
    pub fn wave_octaves(&self) -> usize {
        1 + ((self.wave_octaves.min(100) as usize) * 3) / 100
    }

    /// Wave spatial frequency (0.5 .. 5.5).
    pub fn wave_freq(&self) -> f32 {
        0.5 + pct(self.wave_freq) * 5.0
    }

    /// Wave amplitude (0 .. 0.6).
    pub fn wave_amp(&self) -> f32 {
        if !self.wave_on {
            0.0
        } else {
            pct(self.wave_amp) * 0.6
        }
    }

    /// Wave travel speed (0 = frozen, shares the eco freeze).
    pub fn wave_speed(&self) -> f32 {
        if self.eco {
            0.0
        } else {
            pct(self.wave_speed) * 2.0
        }
    }

    /// Wave travel direction, radians over a full turn.
    pub fn wave_dir(&self) -> f32 {
        pct(self.wave_dir) * std::f32::consts::TAU
    }

    /// Crest sharpness (0.5 soft .. 3.5 ridged).
    pub fn wave_crisp(&self) -> f32 {
        0.5 + pct(self.wave_crisp) * 3.0
    }

    /// Wave colour (linear 0-1).
    pub fn wave_col(&self) -> [f32; 3] {
        let c = self.wave_col;
        [c[0] as f32 / 255.0, c[1] as f32 / 255.0, c[2] as f32 / 255.0]
    }

    /// Glow brightness (0 when disabled: shader skips the band).
    pub fn glow_intensity(&self) -> f32 {
        if !self.glow_on {
            0.0
        } else {
            pct(self.glow_intensity) * 1.2
        }
    }

    /// Glow flow speed (0 = frozen, shares the eco freeze).
    pub fn glow_flow(&self) -> f32 {
        if self.eco {
            0.0
        } else {
            pct(self.glow_flow) * 2.2
        }
    }

    /// Glow band width (view fraction).
    pub fn glow_width(&self) -> f32 {
        0.03 + pct(self.glow_width) * 0.19
    }

    /// Glow palette (deep, light) from the exact RGB channels.
    pub fn glow_hue(&self) -> ([f32; 3], [f32; 3]) {
        let d = self.glow_deep;
        let l = self.glow_light;
        (
            [d[0] as f32 / 255.0, d[1] as f32 / 255.0, d[2] as f32 / 255.0],
            [l[0] as f32 / 255.0, l[1] as f32 / 255.0, l[2] as f32 / 255.0],
        )
    }

    /// Super dolly-back factor (zoom kept on enter).
    pub fn dolly_factor(&self) -> f64 {
        (self.dolly_pct.clamp(50, 100) as f64) / 100.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_animate_with_waves_and_glow_without_eco() {
        let s = Settings::default();
        assert!(s.bg_anim && !s.eco && s.glow_on && s.wave_on);
        assert!((s.bg_speed() - 1.0).abs() < 1e-6);
        assert!(s.bg_frame_interval(false).is_some());
        assert_eq!(s.dolly_factor(), 0.90);
        assert_eq!(s.wave_octaves(), 2);
        assert_eq!(s.bg_pattern(), 1);
    }

    #[test]
    fn eco_freezes_and_dims_everything() {
        let mut s = Settings::default();
        s.eco = true;
        assert_eq!(s.bg_speed(), 0.0);
        assert_eq!(s.glow_flow(), 0.0);
        assert_eq!(s.wave_speed(), 0.0);
        assert!(s.bg_frame_interval(false).is_none());
        assert!(s.bg_dim() < 1.0);
    }

    #[test]
    fn glow_holds_frames_with_wallpaper_off() {
        let mut s = Settings::default();
        s.bg_anim = false;
        s.bg_drift = 0;
        s.wave_on = false;
        // Frozen wallpaper alone: no frames...
        assert!(s.bg_frame_interval(false).is_none());
        // ...but a pulsing glow still needs its cadence.
        assert!(s.bg_frame_interval(true).is_some());
    }

    #[test]
    fn waves_hold_frames_with_wallpaper_off() {
        let mut s = Settings::default();
        s.bg_anim = false;
        s.bg_drift = 0;
        s.wave_on = true;
        s.wave_speed = 60;
        assert!(s.bg_frame_interval(false).is_some());
        s.wave_speed = 0;
        assert!(s.bg_frame_interval(false).is_none());
    }

    #[test]
    fn mappings_span_their_ranges() {
        let mut s = Settings::default();
        s.bg_scale = 0;
        assert!((s.bg_scale() - 0.5).abs() < 1e-6);
        s.bg_scale = 100;
        assert!((s.bg_scale() - 2.0).abs() < 1e-6);
        s.wave_octaves = 0;
        assert_eq!(s.wave_octaves(), 1);
        s.wave_octaves = 100;
        assert_eq!(s.wave_octaves(), 4);
        s.wave_dir = 100;
        assert!((s.wave_dir() - std::f32::consts::TAU).abs() < 1e-6);
        s.wave_amp = 0;
        assert_eq!(s.wave_amp(), 0.0);
        // RGB channels map exactly.
        s.glow_deep = [13, 89, 97];
        let (deep, _) = s.glow_hue();
        assert!((deep[0] - 13.0 / 255.0).abs() < 1e-6);
        // Corrupt values can't escape.
        s.bg_drift = 255;
        assert!((s.bg_speed() - 2.0).abs() < 1e-6);
        s.dolly_pct = 9;
        assert_eq!(s.dolly_factor(), 0.50);
        s.dolly_pct = 200;
        assert_eq!(s.dolly_factor(), 1.0);
        s.wave_shape = 77;
        assert_eq!(s.wave_shape(), 2);
    }
}

//! Shared user settings, edited through the Options menu.
//!
//! The menu owns the UI; this struct owns the values. Every frame the
//! backend reads them (background animation, energy saver).

/// Live user settings (Options menu).
#[derive(Debug, Clone)]
pub struct Settings {
    /// Animated ambient wallpaper (procedural shader). Off = flat clear.
    pub bg_anim: bool,
    /// Slow background motion, for the motion-sensitive.
    pub bg_slow: bool,
    /// Energy saver: freezes background motion and dims it slightly.
    pub eco: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            bg_anim: true,
            bg_slow: false,
            eco: false,
        }
    }
}

impl Settings {
    /// Background time scale (0 = frozen).
    pub fn bg_speed(&self) -> f32 {
        if !self.bg_anim || self.eco {
            0.0
        } else if self.bg_slow {
            0.25
        } else {
            1.0
        }
    }

    /// How often the background repaints while animated: a slow drift,
    /// never a hot loop. None = paint once (or never again).
    pub fn bg_frame_interval(&self) -> Option<std::time::Duration> {
        if self.bg_speed() > 0.0 {
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_animate_without_eco() {
        let s = Settings::default();
        assert!(s.bg_anim && !s.eco && !s.bg_slow);
        assert_eq!(s.bg_speed(), 1.0);
        assert!(s.bg_frame_interval().is_some());
    }

    #[test]
    fn eco_freezes_and_dims() {
        let mut s = Settings::default();
        s.eco = true;
        assert_eq!(s.bg_speed(), 0.0);
        assert!(s.bg_frame_interval().is_none());
        assert!(s.bg_dim() < 1.0);
    }

    #[test]
    fn slow_quarters_speed_but_keeps_frames() {
        let mut s = Settings::default();
        s.bg_slow = true;
        assert_eq!(s.bg_speed(), 0.25);
        assert!(s.bg_frame_interval().is_some());
    }
}

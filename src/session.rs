// Session logic: pure, testable, no backend imports.
// Multi-output viewports on one shared canvas, minimal status bar with
// proximity chrome, Super-tap overview state machine.

use crate::canvas::{fit_view_for_selection, Camera, CanvasWindow, Overview, Vec2};

#[derive(Debug, Clone)]
pub struct Output {
    pub name: String,
    pub scale: f64,
    pub view: Vec2,
    pub camera: Camera,
}

impl Output {
    pub fn new(name: &str, view_w: f64, view_h: f64, scale: f64) -> Self {
        Self {
            name: name.to_string(),
            scale,
            view: Vec2::new(view_w, view_h),
            camera: Camera::new(),
        }
    }
}

/// Minimal status bar: never shows the app name. Window chrome (buttons) is
/// hidden from afar so only the window itself is visible; when the cursor
/// nears the window edge, buttons appear inside or outside per option.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ChromeSide {
    Inside,
    Outside,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ChromeVisibility {
    Hidden,
    Shown(ChromeSide),
}

pub fn chrome_visibility(
    zoom: f64,
    cursor_dist_to_edge_px: f64,
    reveal_radius_px: f64,
    side: ChromeSide,
) -> ChromeVisibility {
    // Far zoom-out (overview): never any chrome, windows stand alone.
    if zoom < 0.6 {
        return ChromeVisibility::Hidden;
    }
    if cursor_dist_to_edge_px <= reveal_radius_px {
        ChromeVisibility::Shown(side)
    } else {
        ChromeVisibility::Hidden
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SuperState {
    Idle,
    Overview,
}

#[derive(Debug)]
pub enum SuperEvent {
    SuperTap,
    WindowClick(u64),
    CanvasClick,
    Esc,
}

#[derive(Debug)]
pub enum SuperAction {
    None,
    EnterOverview { prev: Camera },
    QuitWithFit { fit: Option<Camera> },
    Restore { prev: Camera },
}

/// Drives overview enter/quit + multi-select, shared by all outputs.
pub struct Session {
    pub state: SuperState,
    pub overview: Overview,
    pub camera: Camera,
    pub prev_camera: Option<Camera>,
}

impl Session {
    pub fn new() -> Self {
        Self {
            state: SuperState::Idle,
            overview: Overview::default(),
            camera: Camera::new(),
            prev_camera: None,
        }
    }

    pub fn on_event(
        &mut self,
        event: SuperEvent,
        windows: &std::collections::HashMap<u64, CanvasWindow>,
        view: Vec2,
    ) -> SuperAction {
        match (&self.state, event) {
            (SuperState::Idle, SuperEvent::SuperTap) => {
                self.prev_camera = Some(self.camera);
                self.overview.enter();
                self.state = SuperState::Overview;
                // Slight zoom-out overview; full styled animation lands in lot 1.
                self.camera.zoom = (self.camera.zoom * 0.7).clamp(0.05, 2.0);
                SuperAction::EnterOverview {
                    prev: self.prev_camera.unwrap(),
                }
            }
            (SuperState::Overview, SuperEvent::WindowClick(id)) => {
                self.overview.toggle_select(id);
                SuperAction::None
            }
            (SuperState::Overview, SuperEvent::SuperTap) => {
                let ids = self.overview.quit();
                self.state = SuperState::Idle;
                let selected: Vec<&CanvasWindow> =
                    ids.iter().filter_map(|id| windows.get(id)).collect();
                // Empty selection = plain focus-back, no forced fullscreen.
                let fit = fit_view_for_selection(&self.camera, &selected, view, 80.0);
                if let Some(fit) = fit {
                    self.camera = fit;
                } else if let Some(prev) = self.prev_camera.take() {
                    self.camera = prev;
                }
                SuperAction::QuitWithFit { fit }
            }
            (SuperState::Overview, SuperEvent::CanvasClick)
            | (SuperState::Overview, SuperEvent::Esc) => {
                self.overview.quit();
                self.state = SuperState::Idle;
                if let Some(prev) = self.prev_camera.take() {
                    self.camera = prev;
                    SuperAction::Restore { prev }
                } else {
                    SuperAction::None
                }
            }
            _ => SuperAction::None,
        }
    }
}

impl Default for Session {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn two_windows() -> HashMap<u64, CanvasWindow> {
        let mut m = HashMap::new();
        m.insert(1, CanvasWindow::new(1, 0.0, 0.0, 800.0, 600.0));
        m.insert(2, CanvasWindow::new(2, 2000.0, 1500.0, 800.0, 600.0));
        m
    }

    #[test]
    fn super_tap_select_retap_keeps_both_in_view() {
        let mut s = Session::new();
        let wins = two_windows();
        let view = Vec2::new(1920.0, 1080.0);
        assert!(matches!(
            s.on_event(SuperEvent::SuperTap, &wins, view),
            SuperAction::EnterOverview { .. }
        ));
        s.on_event(SuperEvent::WindowClick(1), &wins, view);
        s.on_event(SuperEvent::WindowClick(2), &wins, view);
        let action = s.on_event(SuperEvent::SuperTap, &wins, view);
        let fit = match action {
            SuperAction::QuitWithFit { fit } => fit.expect("both selected => fit"),
            other => panic!("expected fit, got {:?}", other),
        };
        for w in wins.values() {
            let sc = fit.world_to_screen(w.center(), view);
            assert!(sc.x >= 0.0 && sc.x <= view.x, "x={} off view", sc.x);
            assert!(s.state == SuperState::Idle);
        }
    }

    #[test]
    fn esc_restores_previous_camera() {
        let mut s = Session::new();
        let wins = two_windows();
        let view = Vec2::new(1920.0, 1080.0);
        let before = s.camera;
        s.on_event(SuperEvent::SuperTap, &wins, view);
        assert_ne!(s.camera.zoom, before.zoom);
        s.on_event(SuperEvent::Esc, &wins, view);
        assert_eq!(s.camera.zoom, before.zoom);
    }

    #[test]
    fn chrome_hidden_when_far_or_overview() {
        assert_eq!(
            chrome_visibility(0.4, 0.0, 24.0, ChromeSide::Inside),
            ChromeVisibility::Hidden
        );
        assert_eq!(
            chrome_visibility(1.0, 200.0, 24.0, ChromeSide::Outside),
            ChromeVisibility::Hidden
        );
        assert_eq!(
            chrome_visibility(1.0, 10.0, 24.0, ChromeSide::Inside),
            ChromeVisibility::Shown(ChromeSide::Inside)
        );
    }

    #[test]
    fn outputs_share_one_canvas_with_own_viewports() {
        let mut a = Output::new("DP-1", 1920.0, 1080.0, 1.0);
        let mut b = Output::new("HDMI-1", 1920.0, 1080.0, 1.0);
        a.camera.pos = Vec2::new(0.0, 0.0);
        b.camera.pos = Vec2::new(2500.0, 0.0);
        assert_ne!(a.camera.pos.x, b.camera.pos.x);
    }
}

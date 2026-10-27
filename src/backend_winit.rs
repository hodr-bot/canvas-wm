//! Winit dev backend (feature `compositor` only).
//!
//! Opens a window titled `canvas-wm`, renders the canvas background color
//! derived from the camera, and pumps Smithay input events. Keysym mapping
//! (Super-tap) and session wiring land next; this proves the window,
//! renderer, and event-pump path end to end.

use std::error::Error;
use std::time::Duration;

use smithay::backend::{
    input::{
        Event as BackendEvent, InputBackend, InputEvent, KeyState, KeyboardKeyEvent,
        PointerButtonEvent, PointerMotionAbsoluteEvent,
    },
    renderer::Frame,
    winit::{self, WinitEvent, WinitInputError},
};

use crate::canvas::Camera;

/// Background color of the canvas, derived from the camera.
/// Dark slate that deepens as you zoom out, so overview reads as a void.
pub fn background_color(camera: &Camera) -> [f32; 4] {
    let t = ((camera.zoom - 0.05) / 1.95).clamp(0.0, 1.0) as f32;
    let base = 0.04 + 0.05 * t;
    [base * 0.9, base, base * 1.3, 1.0]
}

/// Open the dev window and run until closed or `max_frames` is reached.
/// `max_frames` exists for headless smoke tests (e.g. under Xvfb).
pub fn run(max_frames: Option<u64>) -> Result<(), Box<dyn Error>> {
    let (mut graphics, mut input) = winit::init(None)?;
    graphics.window().set_title("canvas-wm");

    let camera = Camera::new();
    let mut frames: u64 = 0;

    loop {
        let mut need_render = false;
        match input.dispatch_new_events(|event| match event {
            InputEvent::Keyboard { event } => match event.state() {
                KeyState::Pressed => {
                    log::debug!("key pressed code={}", event.key_code());
                }
                KeyState::Released => {
                    log::debug!("key released code={}", event.key_code());
                }
            },
            InputEvent::PointerMotionAbsolute { event } => {
                log::debug!("pointer {:.0},{:.0}", event.x(), event.y());
            }
            InputEvent::PointerButton { event } => {
                log::debug!("button {:?} {:?}", event.button(), event.state());
            }
            InputEvent::PointerAxis { event } => {
                log::debug!("axis time={}", event.time());
            }
            InputEvent::Special(evt) => match evt {
                WinitEvent::Refresh => need_render = true,
                WinitEvent::Resized { .. } => need_render = true,
                WinitEvent::Focus(focused) => {
                    log::info!("focus {}", focused);
                }
            },
            _ => {}
        }) {
            Ok(()) => {}
            Err(WinitInputError::WindowClosed) => {
                log::info!("window closed after {} frames", frames);
                break;
            }
        }

        if need_render || frames == 0 || max_frames.is_some() {
            let color = background_color(&camera);
            graphics.render(|_, frame| {
                if let Err(err) = frame.clear(color) {
                    log::error!("clear failed: {}", err);
                }
            })?;
            frames += 1;
            if let Some(max) = max_frames {
                if frames >= max {
                    log::info!("frame budget reached ({}), exiting", frames);
                    break;
                }
            }
        }

        std::thread::sleep(Duration::from_millis(8));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn background_deepens_when_zoomed_out() {
        let near = Camera {
            zoom: 1.5,
            ..Camera::new()
        };
        let far = Camera {
            zoom: 0.1,
            ..Camera::new()
        };
        let c_near = background_color(&near);
        let c_far = background_color(&far);
        assert!(c_far[0] < c_near[0]);
        assert!(c_far[1] < c_near[1]);
        for c in [c_near, c_far] {
            assert_eq!(c[3], 1.0);
            assert!(c[0] >= 0.0 && c[1] <= 0.2);
        }
    }
}

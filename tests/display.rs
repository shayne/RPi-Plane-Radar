use std::time::{Duration, Instant};

use planeradar::display::{
    DisplayConfig, DisplayHandler, DisplayUpdate, InputEvent, normalize_finger,
    normalize_sdl_event, render_driver_matches, run_display, video_driver_matches,
};
use sdl2::event::Event;
use sdl2::mouse::{MouseButton, MouseState};

#[test]
fn normalized_finger_coordinates_fill_logical_frame() {
    assert_eq!(
        normalize_finger(7, 0.25, 0.75, true),
        InputEvent::Pressed {
            pointer_id: 7,
            x: 120.0,
            y: 360.0,
        }
    );
}

#[test]
fn finger_up_and_motion_preserve_pointer_identity() {
    assert_eq!(
        normalize_sdl_event(&Event::FingerUp {
            timestamp: 0,
            touch_id: 1,
            finger_id: 9,
            x: 0.5,
            y: 0.25,
            dx: 0.0,
            dy: 0.0,
            pressure: 0.0,
        }),
        Some(InputEvent::Released {
            pointer_id: 9,
            x: 240.0,
            y: 120.0,
        })
    );
    assert_eq!(
        normalize_sdl_event(&Event::FingerMotion {
            timestamp: 0,
            touch_id: 1,
            finger_id: 9,
            x: 0.75,
            y: 0.5,
            dx: 0.25,
            dy: 0.25,
            pressure: 1.0,
        }),
        Some(InputEvent::Moved {
            pointer_id: 9,
            x: 360.0,
            y: 240.0,
        })
    );
}

#[test]
fn normalized_finger_coordinates_are_clamped_to_frame_edges() {
    assert_eq!(
        normalize_finger(1, -0.5, 2.0, true),
        InputEvent::Pressed {
            pointer_id: 1,
            x: 0.0,
            y: 480.0,
        }
    );
}

#[test]
fn left_mouse_button_maps_to_pointer_zero() {
    assert_eq!(
        normalize_sdl_event(&Event::MouseButtonDown {
            timestamp: 0,
            window_id: 1,
            which: 0,
            mouse_btn: MouseButton::Left,
            clicks: 1,
            x: 12,
            y: 34,
        }),
        Some(InputEvent::Pressed {
            pointer_id: 0,
            x: 12.0,
            y: 34.0,
        })
    );
    assert_eq!(
        normalize_sdl_event(&Event::MouseMotion {
            timestamp: 0,
            window_id: 1,
            which: 0,
            mousestate: MouseState::from_sdl_state(1),
            x: 13,
            y: 35,
            xrel: 1,
            yrel: 1,
        }),
        Some(InputEvent::Moved {
            pointer_id: 0,
            x: 13.0,
            y: 35.0,
        })
    );
}

#[test]
fn non_left_mouse_buttons_are_ignored() {
    assert_eq!(
        normalize_sdl_event(&Event::MouseButtonDown {
            timestamp: 0,
            window_id: 1,
            which: 0,
            mouse_btn: MouseButton::Right,
            clicks: 1,
            x: 12,
            y: 34,
        }),
        None
    );
}

#[test]
fn synthetic_mouse_events_from_touch_are_ignored() {
    assert_eq!(
        normalize_sdl_event(&Event::MouseButtonDown {
            timestamp: 0,
            window_id: 1,
            which: u32::MAX,
            mouse_btn: MouseButton::Left,
            clicks: 1,
            x: 120,
            y: 240,
        }),
        None
    );
    assert_eq!(
        normalize_sdl_event(&Event::MouseMotion {
            timestamp: 0,
            window_id: 1,
            which: u32::MAX,
            mousestate: MouseState::from_sdl_state(1),
            x: 121,
            y: 241,
            xrel: 1,
            yrel: 1,
        }),
        None
    );
}

#[test]
fn quit_is_normalized() {
    assert_eq!(
        normalize_sdl_event(&Event::Quit { timestamp: 0 }),
        Some(InputEvent::Quit)
    );
}

#[test]
fn sdl_video_driver_name_is_matched_case_insensitively() {
    assert!(video_driver_matches("KMSDRM", "kmsdrm"));
    assert!(!video_driver_matches("dummy", "kmsdrm"));
}

#[test]
fn default_display_requires_the_verified_accelerated_renderer() {
    assert_eq!(DisplayConfig::default().render_driver, "opengles2");
}

#[test]
fn render_driver_name_is_matched_case_insensitively() {
    assert!(render_driver_matches("OpenGLES2", "opengles2"));
    assert!(!render_driver_matches("opengl", "opengles2"));
}

struct ExitImmediately {
    shutdown_calls: usize,
}

impl DisplayHandler for ExitImmediately {
    fn step(&mut self, _events: &[InputEvent], _now: Instant) -> DisplayUpdate {
        DisplayUpdate {
            frame: None,
            exit: true,
        }
    }

    fn shutdown(&mut self) {
        self.shutdown_calls += 1;
    }
}

#[test]
fn display_exit_is_bounded_without_a_touch_read() {
    let started = Instant::now();
    let mut handler = ExitImmediately { shutdown_calls: 0 };
    let config = DisplayConfig {
        width: 16,
        height: 16,
        video_driver: "dummy".to_owned(),
        render_driver: "software".to_owned(),
        fullscreen: false,
    };

    run_display(config, &mut handler).expect("dummy display");
    assert_eq!(handler.shutdown_calls, 1);
    assert!(started.elapsed() < Duration::from_secs(5));
}

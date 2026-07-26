use planeradar::display::{
    InputEvent, normalize_finger, normalize_sdl_event, video_driver_matches,
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

use planeradar::display::InputEvent;
use planeradar::hyperpixel::Ft5x06Tracker;

const DOWN: u8 = 0;
const UP: u8 = 1;
const CONTACT: u8 = 2;

#[test]
fn down_move_and_up_become_pointer_events() {
    let mut tracker = Ft5x06Tracker::default();

    assert_eq!(
        tracker.update(&frame(1, [(DOWN, 3, 120, 340)])),
        vec![InputEvent::Pressed {
            pointer_id: 3,
            x: 120.0,
            y: 340.0,
        }]
    );
    assert_eq!(
        tracker.update(&frame(1, [(CONTACT, 3, 124, 345)])),
        vec![InputEvent::Moved {
            pointer_id: 3,
            x: 124.0,
            y: 345.0,
        }]
    );
    assert_eq!(
        tracker.update(&frame(0, [(UP, 3, 124, 345)])),
        vec![InputEvent::Released {
            pointer_id: 3,
            x: 124.0,
            y: 345.0,
        }]
    );
}

#[test]
fn unchanged_contact_does_not_repeat_motion() {
    let mut tracker = Ft5x06Tracker::default();
    tracker.update(&frame(1, [(DOWN, 0, 200, 210)]));

    assert!(
        tracker
            .update(&frame(1, [(CONTACT, 0, 200, 210)]))
            .is_empty()
    );
}

#[test]
fn missing_lift_frame_still_releases_an_active_contact() {
    let mut tracker = Ft5x06Tracker::default();
    tracker.update(&frame(1, [(DOWN, 1, 50, 60)]));

    assert_eq!(
        tracker.update(&frame(0, [])),
        vec![InputEvent::Released {
            pointer_id: 1,
            x: 50.0,
            y: 60.0,
        }]
    );
}

#[test]
fn two_contacts_are_tracked_independently() {
    let mut tracker = Ft5x06Tracker::default();

    assert_eq!(
        tracker.update(&frame(2, [(DOWN, 0, 10, 20), (DOWN, 1, 470, 460)])),
        vec![
            InputEvent::Pressed {
                pointer_id: 0,
                x: 10.0,
                y: 20.0,
            },
            InputEvent::Pressed {
                pointer_id: 1,
                x: 470.0,
                y: 460.0,
            },
        ]
    );
}

#[test]
fn controller_coordinates_are_clamped_to_the_logical_frame() {
    let mut tracker = Ft5x06Tracker::default();

    assert_eq!(
        tracker.update(&frame(1, [(DOWN, 0, 0xfff, 0xfff)])),
        vec![InputEvent::Pressed {
            pointer_id: 0,
            x: 479.0,
            y: 479.0,
        }]
    );
}

fn frame<const N: usize>(count: u8, contacts: [(u8, u8, u16, u16); N]) -> [u8; 13] {
    let mut bytes = [0_u8; 13];
    bytes[0] = count;
    for (slot, (event, touch_id, x, y)) in contacts.into_iter().enumerate() {
        let offset = 1 + slot * 6;
        bytes[offset] = (event << 6) | ((x >> 8) as u8 & 0x0f);
        bytes[offset + 1] = x as u8;
        bytes[offset + 2] = (touch_id << 4) | ((y >> 8) as u8 & 0x0f);
        bytes[offset + 3] = y as u8;
    }
    bytes
}

use std::time::Duration;

use planeradar::display::InputEvent;
use planeradar::touch::{Gesture, GestureRecognizer};

fn press(pointer_id: i64, x: f32, y: f32) -> InputEvent {
    InputEvent::Pressed { pointer_id, x, y }
}

fn move_to(pointer_id: i64, x: f32, y: f32) -> InputEvent {
    InputEvent::Moved { pointer_id, x, y }
}

fn release(pointer_id: i64, x: f32, y: f32) -> InputEvent {
    InputEvent::Released { pointer_id, x, y }
}

#[test]
fn long_press_fires_once_and_release_is_consumed() {
    let mut recognizer = GestureRecognizer::default();
    assert!(
        recognizer
            .handle(&press(1, 240.0, 240.0), Duration::ZERO)
            .is_empty()
    );
    assert_eq!(
        recognizer.tick(Duration::from_secs(3)),
        vec![Gesture::LongPress]
    );
    assert!(
        recognizer
            .handle(&release(1, 240.0, 240.0), Duration::from_millis(3100))
            .is_empty()
    );
}

#[test]
fn one_hundred_millisecond_press_and_release_emits_tap() {
    let mut recognizer = GestureRecognizer::default();

    assert!(
        recognizer
            .handle(&press(1, 240.0, 240.0), Duration::ZERO)
            .is_empty()
    );
    assert_eq!(
        recognizer.handle(&release(1, 240.0, 240.0), Duration::from_millis(100)),
        vec![Gesture::Tap]
    );
}

#[test]
fn movement_of_exactly_eighteen_pixels_still_emits_tap() {
    let mut recognizer = GestureRecognizer::default();

    assert!(
        recognizer
            .handle(&press(1, 100.0, 100.0), Duration::ZERO)
            .is_empty()
    );
    assert!(
        recognizer
            .handle(&move_to(1, 118.0, 100.0), Duration::from_millis(50))
            .is_empty()
    );
    assert_eq!(
        recognizer.handle(&release(1, 118.0, 100.0), Duration::from_millis(100)),
        vec![Gesture::Tap]
    );
}

#[test]
fn movement_over_eighteen_pixels_cancels_the_gesture() {
    let mut recognizer = GestureRecognizer::default();

    assert!(
        recognizer
            .handle(&press(1, 100.0, 100.0), Duration::ZERO)
            .is_empty()
    );
    assert!(
        recognizer
            .handle(&move_to(1, 118.1, 100.0), Duration::from_millis(50))
            .is_empty()
    );
    assert!(
        recognizer
            .handle(&release(1, 118.1, 100.0), Duration::from_millis(100))
            .is_empty()
    );
}

#[test]
fn duplicate_press_does_not_reset_the_active_pointer() {
    let mut recognizer = GestureRecognizer::default();

    assert!(
        recognizer
            .handle(&press(1, 240.0, 240.0), Duration::ZERO)
            .is_empty()
    );
    assert!(
        recognizer
            .handle(&press(1, 300.0, 300.0), Duration::from_secs(2))
            .is_empty()
    );
    assert_eq!(
        recognizer.tick(Duration::from_secs(3)),
        vec![Gesture::LongPress]
    );
}

#[test]
fn second_pointer_is_ignored_while_a_pointer_is_active() {
    let mut recognizer = GestureRecognizer::default();

    assert!(
        recognizer
            .handle(&press(1, 240.0, 240.0), Duration::ZERO)
            .is_empty()
    );
    assert!(
        recognizer
            .handle(&press(2, 100.0, 100.0), Duration::from_millis(20))
            .is_empty()
    );
    assert!(
        recognizer
            .handle(&release(2, 100.0, 100.0), Duration::from_millis(30))
            .is_empty()
    );
    assert_eq!(
        recognizer.handle(&release(1, 240.0, 240.0), Duration::from_millis(100)),
        vec![Gesture::Tap]
    );
}

#[test]
fn release_without_a_press_is_ignored() {
    let mut recognizer = GestureRecognizer::default();

    assert!(
        recognizer
            .handle(&release(1, 240.0, 240.0), Duration::ZERO)
            .is_empty()
    );
}

#[test]
fn repeated_ticks_after_a_long_press_do_not_emit_again() {
    let mut recognizer = GestureRecognizer::default();

    assert!(
        recognizer
            .handle(&press(1, 240.0, 240.0), Duration::ZERO)
            .is_empty()
    );
    assert_eq!(
        recognizer.tick(Duration::from_secs(3)),
        vec![Gesture::LongPress]
    );
    assert!(recognizer.tick(Duration::from_secs(4)).is_empty());
}

#[test]
fn tap_is_debounced_until_two_hundred_fifty_milliseconds_after_release() {
    let mut recognizer = GestureRecognizer::default();

    assert!(
        recognizer
            .handle(&press(1, 240.0, 240.0), Duration::ZERO)
            .is_empty()
    );
    assert_eq!(
        recognizer.handle(&release(1, 240.0, 240.0), Duration::from_millis(100)),
        vec![Gesture::Tap]
    );
    assert!(
        recognizer
            .handle(&press(1, 240.0, 240.0), Duration::from_millis(349))
            .is_empty()
    );
    assert!(
        recognizer
            .handle(&release(1, 240.0, 240.0), Duration::from_millis(349))
            .is_empty()
    );
    assert!(
        recognizer
            .handle(&press(1, 240.0, 240.0), Duration::from_millis(599))
            .is_empty()
    );
    assert_eq!(
        recognizer.handle(&release(1, 240.0, 240.0), Duration::from_millis(599)),
        vec![Gesture::Tap]
    );
}

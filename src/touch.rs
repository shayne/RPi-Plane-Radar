use std::time::Duration;

use crate::display::InputEvent;

const MAX_TAP_MOVEMENT_PX: f32 = 18.0;
const LONG_PRESS_DURATION: Duration = Duration::from_secs(3);
const TAP_DEBOUNCE_DURATION: Duration = Duration::from_millis(250);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Gesture {
    Tap,
    LongPress,
}

#[derive(Default)]
pub struct GestureRecognizer {
    active: Option<ActivePointer>,
    last_release: Option<Duration>,
}

struct ActivePointer {
    pointer_id: i64,
    x: f32,
    y: f32,
    pressed_at: Duration,
    cancelled: bool,
    long_press_fired: bool,
}

impl GestureRecognizer {
    pub fn handle(&mut self, event: &InputEvent, now: Duration) -> Vec<Gesture> {
        match *event {
            InputEvent::Pressed { pointer_id, x, y } if self.active.is_none() => {
                self.active = Some(ActivePointer {
                    pointer_id,
                    x,
                    y,
                    pressed_at: now,
                    cancelled: false,
                    long_press_fired: false,
                });
            }
            InputEvent::Moved { pointer_id, x, y } => {
                if let Some(active) = self.active.as_mut()
                    && active.pointer_id == pointer_id
                {
                    active.cancelled |= moved_too_far(active, x, y);
                }
            }
            InputEvent::Released { pointer_id, x, y } => {
                let Some(mut active) = self.active.take() else {
                    return Vec::new();
                };
                if active.pointer_id != pointer_id {
                    self.active = Some(active);
                    return Vec::new();
                }

                active.cancelled |= moved_too_far(&active, x, y);
                let debounced = self.last_release.is_some_and(|last_release| {
                    elapsed_since(now, last_release) < TAP_DEBOUNCE_DURATION
                });
                self.last_release = Some(now);
                if !active.cancelled && !active.long_press_fired && !debounced {
                    return vec![Gesture::Tap];
                }
            }
            _ => {}
        }
        Vec::new()
    }

    pub fn tick(&mut self, now: Duration) -> Vec<Gesture> {
        let Some(active) = self.active.as_mut() else {
            return Vec::new();
        };
        if !active.cancelled
            && !active.long_press_fired
            && elapsed_since(now, active.pressed_at) >= LONG_PRESS_DURATION
        {
            active.long_press_fired = true;
            return vec![Gesture::LongPress];
        }
        Vec::new()
    }
}

fn moved_too_far(active: &ActivePointer, x: f32, y: f32) -> bool {
    (x - active.x).hypot(y - active.y) > MAX_TAP_MOVEMENT_PX
}

fn elapsed_since(now: Duration, then: Duration) -> Duration {
    now.checked_sub(then).unwrap_or(Duration::ZERO)
}

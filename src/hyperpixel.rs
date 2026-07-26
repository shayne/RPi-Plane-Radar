use std::collections::{BTreeMap, BTreeSet};

use crate::display::InputEvent;

pub const TOUCH_DEVICE: &str = "/dev/i2c-11";
#[cfg(target_os = "linux")]
const TOUCH_REGISTER: u8 = 0x02;
#[cfg(target_os = "linux")]
const TOUCH_ADDRESS: u16 = 0x15;
const TOUCH_FRAME_LEN: u8 = 13;
const MAX_CONTACTS: usize = 2;
const MAX_COORDINATE: u16 = 479;
const EVENT_DOWN: u8 = 0;
const EVENT_CONTACT: u8 = 2;

#[derive(Default)]
pub struct Ft5x06Tracker {
    active: BTreeMap<i64, (f32, f32)>,
}

impl Ft5x06Tracker {
    pub fn update(&mut self, frame: &[u8; TOUCH_FRAME_LEN as usize]) -> Vec<InputEvent> {
        let count = usize::from(frame[0] & 0x0f).min(MAX_CONTACTS);
        let mut seen = BTreeSet::new();
        let mut events = Vec::new();

        for slot in 0..count {
            let offset = 1 + slot * 6;
            let event = frame[offset] >> 6;
            if !matches!(event, EVENT_DOWN | EVENT_CONTACT) {
                continue;
            }

            let pointer_id = i64::from(frame[offset + 2] >> 4);
            let x = coordinate(frame[offset], frame[offset + 1]);
            let y = coordinate(frame[offset + 2], frame[offset + 3]);
            seen.insert(pointer_id);

            match self.active.insert(pointer_id, (x, y)) {
                None => events.push(InputEvent::Pressed { pointer_id, x, y }),
                Some(previous) if previous != (x, y) => {
                    events.push(InputEvent::Moved { pointer_id, x, y });
                }
                Some(_) => {}
            }
        }

        let released: Vec<_> = self
            .active
            .keys()
            .copied()
            .filter(|pointer_id| !seen.contains(pointer_id))
            .collect();
        for pointer_id in released {
            if let Some((x, y)) = self.active.remove(&pointer_id) {
                events.push(InputEvent::Released { pointer_id, x, y });
            }
        }

        events
    }
}

fn coordinate(high: u8, low: u8) -> f32 {
    let raw = (u16::from(high & 0x0f) << 8) | u16::from(low);
    f32::from(raw.min(MAX_COORDINATE))
}

#[cfg(target_os = "linux")]
pub struct HyperpixelTouch {
    device: i2cdev::linux::LinuxI2CDevice,
    tracker: Ft5x06Tracker,
}

#[cfg(target_os = "linux")]
impl HyperpixelTouch {
    pub fn open(path: &std::path::Path) -> Result<Self, i2cdev::linux::LinuxI2CError> {
        Ok(Self {
            device: i2cdev::linux::LinuxI2CDevice::new(path, TOUCH_ADDRESS)?,
            tracker: Ft5x06Tracker::default(),
        })
    }

    pub fn poll(&mut self) -> Result<Vec<InputEvent>, i2cdev::linux::LinuxI2CError> {
        use i2cdev::core::I2CDevice;

        let block = self
            .device
            .smbus_read_i2c_block_data(TOUCH_REGISTER, TOUCH_FRAME_LEN)?;
        let mut frame = [0_u8; TOUCH_FRAME_LEN as usize];
        frame.copy_from_slice(&block);
        Ok(self.tracker.update(&frame))
    }
}

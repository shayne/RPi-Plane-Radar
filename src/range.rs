use thiserror::Error;

use crate::model::Units;

pub const RANGE_RING3_KM: [f64; 4] = [5.0, 10.0, 15.0, 25.0];
const RING3_TO_OUTER: f64 = 4.0 / 3.0;
const KILOMETRES_TO_MILES: f64 = 0.621_371_192_2;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RangePreset {
    pub ring3_km: f64,
    pub outer_km: f64,
}

#[derive(Debug, Error, PartialEq)]
pub enum RangeError {
    #[error("range index {0} is outside the supported presets")]
    InvalidIndex(u8),
}

pub fn range_preset(index: u8) -> Result<RangePreset, RangeError> {
    let ring3_km = *RANGE_RING3_KM
        .get(usize::from(index))
        .ok_or(RangeError::InvalidIndex(index))?;
    Ok(RangePreset {
        ring3_km,
        outer_km: ring3_km * RING3_TO_OUTER,
    })
}

pub fn next_range_index(index: u8) -> u8 {
    ((usize::from(index) + 1) % RANGE_RING3_KM.len()) as u8
}

pub fn format_range_label(preset: RangePreset, units: Units) -> String {
    match units {
        Units::Kilometres => format!("{}km", preset.ring3_km.round() as i64),
        Units::Miles => format!(
            "{}mi",
            (preset.ring3_km * KILOMETRES_TO_MILES).round() as i64
        ),
    }
}

pub const SIZE: u32 = 480;
pub const CENTER: (f32, f32) = (240.0, 240.0);
pub const GRID_OUTER_RADIUS: f32 = 214.0;
pub const GRID_RING_COUNT: usize = 4;
pub const GRID_STROKE_WIDTH: f32 = 4.0;
pub const CENTER_DOT_RADIUS: f32 = 4.0;

pub const AIRCRAFT_NOSE_LENGTH: f32 = 16.0;
pub const AIRCRAFT_TAIL_LENGTH: f32 = 6.0;
pub const AIRCRAFT_TAIL_HALF_WIDTH: f32 = 8.0;
pub const AIRCRAFT_SAFE_RADIUS: f32 = 188.0;
pub const AIRCRAFT_LABEL_GAP: f32 = 2.0;
pub const TRACK_HORIZON_SECONDS: f64 = 60.0;
pub const TRACK_REFERENCE_OUTER_KM: f64 = 13.3;
pub const TRACK_LENGTH_SCALE: f64 = 1.5 / 5.0;
pub const TRACK_MIN_LENGTH: f32 = 4.0;
pub const TRACK_STROKE_WIDTH: f32 = 4.0;

pub const RIM_RADIUS: f64 = 238.0;
pub const RIM_DOT_RADIUS: f32 = 8.0;
pub const RUNWAY_STROKE_WIDTH: f32 = 4.0;
pub const RUNWAY_LABEL_GAP: f32 = 6.0;

pub const CARDINAL_CAP_HEIGHT: f32 = 28.0;
pub const SCALE_CAP_HEIGHT: f32 = 22.0;
pub const AIRCRAFT_TAG_CAP_HEIGHT: f32 = 26.0;
pub const RUNWAY_LABEL_CAP_HEIGHT: f32 = 28.0;
pub const STALE_CAP_HEIGHT: f32 = 22.0;

pub const BACKGROUND: [u8; 4] = [4, 10, 28, 255];
pub const GRID: [u8; 4] = [16, 100, 32, 255];
pub const LABEL: [u8; 4] = [255, 255, 255, 255];
pub const AIRCRAFT: [u8; 4] = [255, 0, 0, 255];
pub const TRACK: [u8; 4] = [255, 0, 255, 255];
pub const TAG_TYPE: [u8; 4] = [255, 200, 0, 255];
pub const TAG_ALTITUDE: [u8; 4] = [90, 200, 255, 255];
pub const RUNWAY: [u8; 4] = [56, 150, 170, 255];
pub const RUNWAY_LABEL: [u8; 4] = [110, 210, 230, 255];
pub const STALE: [u8; 4] = TAG_TYPE;

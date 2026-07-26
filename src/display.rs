use std::collections::HashMap;
use std::thread;
use std::time::{Duration, Instant};

use sdl2::event::Event;
use sdl2::mouse::MouseButton;
use sdl2::pixels::PixelFormatEnum;
use thiserror::Error;

pub const LOGICAL_WIDTH: u32 = 480;
pub const LOGICAL_HEIGHT: u32 = 480;
const FRAME_RATE: u32 = 30;
const PROBE_DURATION: Duration = Duration::from_secs(30);
const SDL_TOUCH_MOUSE_ID: u32 = u32::MAX;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DisplayConfig {
    pub width: u32,
    pub height: u32,
    pub video_driver: String,
    pub render_driver: String,
    pub fullscreen: bool,
}

impl Default for DisplayConfig {
    fn default() -> Self {
        Self {
            width: LOGICAL_WIDTH,
            height: LOGICAL_HEIGHT,
            video_driver: "kmsdrm".to_owned(),
            render_driver: "opengles2".to_owned(),
            fullscreen: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum InputEvent {
    Pressed { pointer_id: i64, x: f32, y: f32 },
    Moved { pointer_id: i64, x: f32, y: f32 },
    Released { pointer_id: i64, x: f32, y: f32 },
    Quit,
}

pub trait DisplayHandler {
    fn step(&mut self, events: &[InputEvent], now: Instant) -> DisplayUpdate;

    fn shutdown(&mut self) {}
}

pub struct DisplayUpdate {
    pub frame: Option<Vec<u8>>,
    pub exit: bool,
}

#[derive(Default)]
struct PresentationState {
    has_uploaded_frame: bool,
}

impl PresentationState {
    fn record_upload(&mut self) {
        self.has_uploaded_frame = true;
    }

    fn should_present(&self) -> bool {
        self.has_uploaded_frame
    }
}

#[derive(Debug, Error)]
pub enum DisplayError {
    #[error("SDL failure: {0}")]
    Sdl(String),
    #[error("SDL selected video driver {actual:?}, expected {expected:?}")]
    WrongVideoDriver { expected: String, actual: String },
    #[error("SDL selected render driver {actual:?}, expected {expected:?}")]
    WrongRenderDriver { expected: String, actual: String },
    #[error("SDL rejected required hint {name:?}")]
    Hint { name: &'static str },
    #[error("frame has {actual} bytes, expected {expected}")]
    InvalidFrameSize { expected: usize, actual: usize },
    #[error("display dimensions overflow the address space")]
    DimensionsOverflow,
}

pub fn normalize_finger(pointer_id: i64, x: f32, y: f32, pressed: bool) -> InputEvent {
    let (x, y) = normalized_coordinates(x, y);
    if pressed {
        InputEvent::Pressed { pointer_id, x, y }
    } else {
        InputEvent::Released { pointer_id, x, y }
    }
}

pub fn normalize_sdl_event(event: &Event) -> Option<InputEvent> {
    match *event {
        Event::FingerDown {
            finger_id, x, y, ..
        } => Some(normalize_finger(finger_id, x, y, true)),
        Event::FingerUp {
            finger_id, x, y, ..
        } => Some(normalize_finger(finger_id, x, y, false)),
        Event::FingerMotion {
            finger_id, x, y, ..
        } => {
            let (x, y) = normalized_coordinates(x, y);
            Some(InputEvent::Moved {
                pointer_id: finger_id,
                x,
                y,
            })
        }
        Event::MouseButtonDown {
            which,
            mouse_btn: MouseButton::Left,
            x,
            y,
            ..
        } if which != SDL_TOUCH_MOUSE_ID => Some(mouse_event(x, y, true)),
        Event::MouseButtonUp {
            which,
            mouse_btn: MouseButton::Left,
            x,
            y,
            ..
        } if which != SDL_TOUCH_MOUSE_ID => Some(mouse_event(x, y, false)),
        Event::MouseMotion {
            which,
            mousestate,
            x,
            y,
            ..
        } if which != SDL_TOUCH_MOUSE_ID && mousestate.left() => {
            let (x, y) = mouse_coordinates(x, y);
            Some(InputEvent::Moved {
                pointer_id: 0,
                x,
                y,
            })
        }
        Event::Quit { .. } => Some(InputEvent::Quit),
        _ => None,
    }
}

pub fn run_display<H: DisplayHandler>(
    config: DisplayConfig,
    handler: &mut H,
) -> Result<(), DisplayError> {
    sdl2::hint::set("SDL_VIDEODRIVER", &config.video_driver);
    sdl2::hint::set("SDL_TOUCH_MOUSE_EVENTS", "0");
    if !sdl2::hint::set_with_priority(
        "SDL_RENDER_DRIVER",
        &config.render_driver,
        &sdl2::hint::Hint::Override,
    ) {
        return Err(DisplayError::Hint {
            name: "SDL_RENDER_DRIVER",
        });
    }
    let sdl = sdl2::init().map_err(DisplayError::Sdl)?;
    if sdl2::touch::num_touch_devices() <= 0 {
        log::warn!("touch input is unavailable; display and web setup remain active");
    }
    let video = sdl.video().map_err(DisplayError::Sdl)?;
    let actual_driver = video.current_video_driver().to_owned();
    if !video_driver_matches(&actual_driver, &config.video_driver) {
        return Err(DisplayError::WrongVideoDriver {
            expected: config.video_driver,
            actual: actual_driver,
        });
    }

    let mut window_builder = video.window("Plane Radar", config.width, config.height);
    if config.fullscreen {
        window_builder.fullscreen();
    }
    let window = window_builder
        .position_centered()
        .build()
        .map_err(|error| DisplayError::Sdl(error.to_string()))?;
    sdl.mouse().show_cursor(false);

    let mut canvas = window
        .into_canvas()
        .build()
        .map_err(|error| DisplayError::Sdl(error.to_string()))?;
    let actual_renderer = canvas.info().name.to_owned();
    if !render_driver_matches(&actual_renderer, &config.render_driver) {
        return Err(DisplayError::WrongRenderDriver {
            expected: config.render_driver,
            actual: actual_renderer,
        });
    }
    canvas
        .set_logical_size(config.width, config.height)
        .map_err(|error| DisplayError::Sdl(error.to_string()))?;

    let texture_creator = canvas.texture_creator();
    let mut texture = texture_creator
        .create_texture_streaming(PixelFormatEnum::RGBA32, config.width, config.height)
        .map_err(|error| DisplayError::Sdl(error.to_string()))?;
    let mut event_pump = sdl.event_pump().map_err(DisplayError::Sdl)?;
    let expected_len = frame_len(config.width, config.height)?;
    let pitch = usize::try_from(config.width)
        .ok()
        .and_then(|width| width.checked_mul(4))
        .ok_or(DisplayError::DimensionsOverflow)?;
    let frame_period = Duration::from_secs_f64(1.0 / f64::from(FRAME_RATE));
    let mut presentation = PresentationState::default();

    loop {
        let frame_start = Instant::now();
        let events: Vec<_> = event_pump
            .poll_iter()
            .filter_map(|event| normalize_sdl_event(&event))
            .collect();
        let update = handler.step(&events, frame_start);
        if let Some(frame) = update.frame {
            if frame.len() != expected_len {
                return Err(DisplayError::InvalidFrameSize {
                    expected: expected_len,
                    actual: frame.len(),
                });
            }
            texture
                .update(None, &frame, pitch)
                .map_err(|error| DisplayError::Sdl(error.to_string()))?;
            presentation.record_upload();
        }
        if presentation.should_present() {
            // SDL invalidates the renderer backbuffer after every present. Keep
            // the GPU texture, but redraw it so KMS always gets a complete frame.
            canvas.clear();
            canvas
                .copy(&texture, None, None)
                .map_err(DisplayError::Sdl)?;
            canvas.present();
        }
        if update.exit {
            handler.shutdown();
            return Ok(());
        }

        if let Some(remaining) = frame_period.checked_sub(frame_start.elapsed()) {
            thread::sleep(remaining);
        }
    }
}

pub fn video_driver_matches(actual: &str, expected: &str) -> bool {
    actual.eq_ignore_ascii_case(expected)
}

pub fn render_driver_matches(actual: &str, expected: &str) -> bool {
    actual.eq_ignore_ascii_case(expected)
}

pub fn run_probe() -> Result<(), DisplayError> {
    let config = DisplayConfig::default();
    println!("SDL driver: {}", config.video_driver);
    println!("logical mode: {}x{}", config.width, config.height);
    let mut handler = ProbeHandler::new(config.width, config.height)?;
    run_display(config, &mut handler)
}

struct ProbeHandler {
    width: u32,
    height: u32,
    started: Instant,
    touches: HashMap<i64, (f32, f32)>,
    dirty: bool,
}

impl ProbeHandler {
    fn new(width: u32, height: u32) -> Result<Self, DisplayError> {
        frame_len(width, height)?;
        Ok(Self {
            width,
            height,
            started: Instant::now(),
            touches: HashMap::new(),
            dirty: true,
        })
    }

    fn draw(&self) -> Vec<u8> {
        let mut frame = vec![0; frame_len(self.width, self.height).expect("validated dimensions")];
        fill(&mut frame, [7, 20, 38, 255]);

        let center_x = i32::try_from(self.width / 2).unwrap_or(i32::MAX);
        let center_y = i32::try_from(self.height / 2).unwrap_or(i32::MAX);
        let max_x = i32::try_from(self.width.saturating_sub(1)).unwrap_or(i32::MAX);
        let max_y = i32::try_from(self.height.saturating_sub(1)).unwrap_or(i32::MAX);
        rectangle(
            &mut frame,
            self.width,
            center_x - 60,
            0,
            120,
            18,
            [255, 48, 48, 255],
        );
        rectangle(
            &mut frame,
            self.width,
            max_x - 17,
            center_y - 60,
            18,
            120,
            [36, 220, 72, 255],
        );
        rectangle(
            &mut frame,
            self.width,
            center_x - 60,
            max_y - 17,
            120,
            18,
            [54, 110, 255, 255],
        );
        rectangle(
            &mut frame,
            self.width,
            0,
            center_y - 60,
            18,
            120,
            [255, 220, 42, 255],
        );
        draw_top(&mut frame, self.width, center_x - 42, 32);
        rectangle(
            &mut frame,
            self.width,
            center_x - 50,
            center_y - 2,
            101,
            5,
            [255, 255, 255, 255],
        );
        rectangle(
            &mut frame,
            self.width,
            center_x - 2,
            center_y - 50,
            5,
            101,
            [255, 255, 255, 255],
        );
        for &(x, y) in self.touches.values() {
            dot(
                &mut frame,
                self.width,
                x.round() as i32,
                y.round() as i32,
                14,
                [255, 55, 210, 255],
            );
        }
        frame
    }
}

impl DisplayHandler for ProbeHandler {
    fn step(&mut self, events: &[InputEvent], now: Instant) -> DisplayUpdate {
        let mut exit = now.duration_since(self.started) >= PROBE_DURATION;
        for event in events {
            match *event {
                InputEvent::Pressed { pointer_id, x, y } => {
                    println!("pressed pointer={pointer_id} x={x:.1} y={y:.1}");
                    self.touches.insert(pointer_id, (x, y));
                    self.dirty = true;
                }
                InputEvent::Moved { pointer_id, x, y } => {
                    println!("moved pointer={pointer_id} x={x:.1} y={y:.1}");
                    self.touches.insert(pointer_id, (x, y));
                    self.dirty = true;
                }
                InputEvent::Released { pointer_id, x, y } => {
                    println!("released pointer={pointer_id} x={x:.1} y={y:.1}");
                    self.touches.remove(&pointer_id);
                    self.dirty = true;
                }
                InputEvent::Quit => {
                    println!("quit");
                    exit = true;
                }
            }
        }

        let frame = self.dirty.then(|| {
            self.dirty = false;
            self.draw()
        });
        DisplayUpdate { frame, exit }
    }
}

fn normalized_coordinates(x: f32, y: f32) -> (f32, f32) {
    (
        x.clamp(0.0, 1.0) * LOGICAL_WIDTH as f32,
        y.clamp(0.0, 1.0) * LOGICAL_HEIGHT as f32,
    )
}

fn mouse_coordinates(x: i32, y: i32) -> (f32, f32) {
    (
        (x as f32).clamp(0.0, LOGICAL_WIDTH as f32),
        (y as f32).clamp(0.0, LOGICAL_HEIGHT as f32),
    )
}

fn mouse_event(x: i32, y: i32, pressed: bool) -> InputEvent {
    let (x, y) = mouse_coordinates(x, y);
    if pressed {
        InputEvent::Pressed {
            pointer_id: 0,
            x,
            y,
        }
    } else {
        InputEvent::Released {
            pointer_id: 0,
            x,
            y,
        }
    }
}

fn frame_len(width: u32, height: u32) -> Result<usize, DisplayError> {
    usize::try_from(width)
        .ok()
        .and_then(|width| {
            usize::try_from(height)
                .ok()
                .and_then(|height| width.checked_mul(height))
        })
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or(DisplayError::DimensionsOverflow)
}

fn fill(frame: &mut [u8], color: [u8; 4]) {
    for pixel in frame.chunks_exact_mut(4) {
        pixel.copy_from_slice(&color);
    }
}

fn rectangle(
    frame: &mut [u8],
    width: u32,
    x: i32,
    y: i32,
    rectangle_width: i32,
    rectangle_height: i32,
    color: [u8; 4],
) {
    for py in y.max(0)..y.saturating_add(rectangle_height) {
        for px in x.max(0)..x.saturating_add(rectangle_width) {
            set_pixel(frame, width, px, py, color);
        }
    }
}

fn dot(frame: &mut [u8], width: u32, x: i32, y: i32, radius: i32, color: [u8; 4]) {
    for dy in -radius..=radius {
        for dx in -radius..=radius {
            if dx * dx + dy * dy <= radius * radius {
                set_pixel(frame, width, x + dx, y + dy, color);
            }
        }
    }
}

fn set_pixel(frame: &mut [u8], width: u32, x: i32, y: i32, color: [u8; 4]) {
    let Ok(x) = u32::try_from(x) else {
        return;
    };
    let Ok(y) = u32::try_from(y) else {
        return;
    };
    if x >= width {
        return;
    }
    let Some(pixel) = y
        .checked_mul(width)
        .and_then(|offset| offset.checked_add(x))
        .and_then(|offset| usize::try_from(offset).ok())
        .and_then(|offset| offset.checked_mul(4))
    else {
        return;
    };
    let Some(target) = frame.get_mut(pixel..pixel + 4) else {
        return;
    };
    target.copy_from_slice(&color);
}

fn draw_top(frame: &mut [u8], width: u32, x: i32, y: i32) {
    const GLYPHS: [[u8; 7]; 3] = [
        [
            0b11111, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100,
        ],
        [
            0b01110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110,
        ],
        [
            0b11110, 0b10001, 0b10001, 0b11110, 0b10000, 0b10000, 0b10000,
        ],
    ];
    let scale = 4;
    for (glyph_index, glyph) in GLYPHS.iter().enumerate() {
        for (row, bits) in glyph.iter().enumerate() {
            for column in 0..5 {
                if bits & (1 << (4 - column)) != 0 {
                    rectangle(
                        frame,
                        width,
                        x + i32::try_from(glyph_index).unwrap_or(0) * 28 + column * scale,
                        y + i32::try_from(row).unwrap_or(0) * scale,
                        scale,
                        scale,
                        [255, 255, 255, 255],
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::PresentationState;

    #[test]
    fn uploaded_frame_remains_presentable_on_unchanged_ticks() {
        let mut state = PresentationState::default();
        assert!(!state.should_present());

        state.record_upload();
        assert!(state.should_present());
        assert!(state.should_present());
    }
}

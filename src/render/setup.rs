use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use qrcode::QrCode;
use qrcode::types::{Color, EcLevel};
use tiny_skia::Pixmap;
use url::Host;

use crate::display::{DisplayConfig, DisplayHandler, DisplayUpdate, InputEvent, run_display};
use crate::render::text::{HorizontalAnchor, TextRasterizer, TextStyle, VerticalAnchor};
use crate::render::{FontAsset, Frame, RenderError};

pub const CANONICAL_LOCAL_URL: &str = "http://planeradar.local";

const SIZE: u32 = 480;
const WHITE: [u8; 4] = [255, 255, 255, 255];
const INK: [u8; 4] = [4, 10, 28, 255];
const QR_TOP: u32 = 50;
const QR_MAX_PIXELS: u32 = 264;
const QR_QUIET_MODULES: u32 = 4;
const LOCAL_URL_TOP: f32 = 322.0;
const IP_URL_TOP: f32 = 351.0;
const MESSAGE_TOP: f32 = 381.0;
const MESSAGE_LINE_STEP: f32 = 20.0;
const CONTROL_TOP: f32 = 428.0;
const MESSAGE_MAX_WIDTH: f32 = 270.0;
const MAX_URL_BYTES: usize = 128;
const MAX_MESSAGE_CHARACTERS: usize = 512;
const REQUIRED_MESSAGE: &str = "Open this page to set the radar location";
const WAITING_FOR_NETWORK: &str = "WAITING FOR NETWORK";
const REQUIRED_CONTROL: &str = "SETUP REQUIRED";
const CONFIGURED_CONTROL: &str = "TAP TO RETURN";

pub struct SetupRenderer {
    font: FontAsset,
}

impl SetupRenderer {
    pub fn new(font: FontAsset) -> Self {
        Self { font }
    }

    pub fn render(
        &self,
        _local_url: &str,
        ip_url: Option<&str>,
        configured: bool,
        message: &str,
    ) -> Result<Frame, RenderError> {
        let code = QrCode::with_error_correction_level(CANONICAL_LOCAL_URL.as_bytes(), EcLevel::M)?;
        let code_modules =
            u32::try_from(code.width()).map_err(|_| RenderError::DimensionsOverflow)?;
        let total_modules = code_modules
            .checked_add(QR_QUIET_MODULES * 2)
            .ok_or(RenderError::DimensionsOverflow)?;
        let module_pixels = QR_MAX_PIXELS / total_modules;
        if module_pixels == 0 {
            return Err(RenderError::InvalidSettings("QR code is too large"));
        }
        let qr_pixels = total_modules
            .checked_mul(module_pixels)
            .ok_or(RenderError::DimensionsOverflow)?;
        let qr_left = (SIZE - qr_pixels) / 2;

        let mut pixmap = Pixmap::new(SIZE, SIZE).ok_or(RenderError::DimensionsOverflow)?;
        pixmap.fill(tiny_skia::Color::from_rgba8(
            WHITE[0], WHITE[1], WHITE[2], WHITE[3],
        ));
        draw_qr(
            &mut pixmap,
            &code,
            qr_left,
            QR_TOP,
            module_pixels,
            QR_QUIET_MODULES,
        )?;

        let text = TextRasterizer::new(self.font.font());
        draw_centered_line(&text, &mut pixmap, CANONICAL_LOCAL_URL, LOCAL_URL_TOP, 20.0);
        draw_centered_line(
            &text,
            &mut pixmap,
            validated_ip_url(ip_url)
                .as_deref()
                .unwrap_or(WAITING_FOR_NETWORK),
            IP_URL_TOP,
            17.0,
        );

        let message = safe_message(message, configured);
        for (index, line) in wrap_message(&text, &message, MESSAGE_MAX_WIDTH, 14.0, 2)
            .into_iter()
            .enumerate()
        {
            draw_centered_line(
                &text,
                &mut pixmap,
                &line,
                MESSAGE_TOP + MESSAGE_LINE_STEP * index as f32,
                14.0,
            );
        }
        draw_centered_line(
            &text,
            &mut pixmap,
            if configured {
                CONFIGURED_CONTROL
            } else {
                REQUIRED_CONTROL
            },
            CONTROL_TOP,
            12.0,
        );

        Frame::new(SIZE, SIZE, pixmap.take())
    }
}

pub fn fixture_required() -> Result<Frame, RenderError> {
    fixture_renderer()?.render(
        CANONICAL_LOCAL_URL,
        Some("http://10.0.4.74"),
        false,
        REQUIRED_MESSAGE,
    )
}

pub fn fixture_settings() -> Result<Frame, RenderError> {
    fixture_renderer()?.render(
        CANONICAL_LOCAL_URL,
        Some("http://10.0.4.74"),
        true,
        "Settings are available on this page",
    )
}

pub fn write_fixtures(output: &Path) -> Result<(), RenderError> {
    fs::create_dir_all(output)?;
    for (name, frame) in [
        ("setup-required.png", fixture_required()?),
        ("settings.png", fixture_settings()?),
    ] {
        frame.save_png(&output.join(name))?;
    }
    Ok(())
}

pub fn run_setup_demo(
    ip_url: Option<&str>,
    seconds: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    let frame = SetupRenderer::new(FontAsset::embedded()?).render(
        CANONICAL_LOCAL_URL,
        ip_url,
        false,
        REQUIRED_MESSAGE,
    )?;
    let mut handler = SetupDemoHandler {
        frame: Some(frame.pixels().to_vec()),
        started: Instant::now(),
        duration: Duration::from_secs(seconds),
    };
    run_display(DisplayConfig::default(), &mut handler)?;
    Ok(())
}

struct SetupDemoHandler {
    frame: Option<Vec<u8>>,
    started: Instant,
    duration: Duration,
}

impl DisplayHandler for SetupDemoHandler {
    fn step(&mut self, events: &[InputEvent], now: Instant) -> DisplayUpdate {
        let quit = events.iter().any(|event| matches!(event, InputEvent::Quit));
        let expired = now
            .checked_duration_since(self.started)
            .is_some_and(|elapsed| elapsed >= self.duration);
        DisplayUpdate {
            frame: self.frame.take(),
            exit: quit || expired,
        }
    }
}

fn fixture_renderer() -> Result<SetupRenderer, RenderError> {
    Ok(SetupRenderer::new(FontAsset::embedded()?))
}

fn draw_qr(
    pixmap: &mut Pixmap,
    code: &QrCode,
    left: u32,
    top: u32,
    module_pixels: u32,
    quiet_modules: u32,
) -> Result<(), RenderError> {
    let code_width = u32::try_from(code.width()).map_err(|_| RenderError::DimensionsOverflow)?;
    let pixmap_width = pixmap.width();
    let colors = code.to_colors();
    let data = pixmap.data_mut();
    for row in 0..code_width {
        for column in 0..code_width {
            let index = usize::try_from(row * code_width + column)
                .map_err(|_| RenderError::DimensionsOverflow)?;
            if colors[index] != Color::Dark {
                continue;
            }
            let module_left = left + (quiet_modules + column) * module_pixels;
            let module_top = top + (quiet_modules + row) * module_pixels;
            fill_opaque_square(
                data,
                pixmap_width,
                module_left,
                module_top,
                module_pixels,
                INK,
            )?;
        }
    }
    Ok(())
}

fn fill_opaque_square(
    pixels: &mut [u8],
    frame_width: u32,
    left: u32,
    top: u32,
    size: u32,
    color: [u8; 4],
) -> Result<(), RenderError> {
    for y in top..top + size {
        for x in left..left + size {
            let offset = y
                .checked_mul(frame_width)
                .and_then(|offset| offset.checked_add(x))
                .and_then(|offset| usize::try_from(offset).ok())
                .and_then(|offset| offset.checked_mul(4))
                .ok_or(RenderError::DimensionsOverflow)?;
            pixels[offset..offset + 4].copy_from_slice(&color);
        }
    }
    Ok(())
}

fn draw_centered_line(
    text: &TextRasterizer<'_>,
    pixmap: &mut Pixmap,
    value: &str,
    top: f32,
    cap_height: f32,
) {
    text.draw(
        pixmap,
        value,
        SIZE as f32 / 2.0,
        top,
        TextStyle {
            cap_height,
            color: INK,
            horizontal: HorizontalAnchor::Center,
            vertical: VerticalAnchor::Top,
        },
    );
}

fn validated_ip_url(candidate: Option<&str>) -> Option<String> {
    let candidate = candidate?.trim();
    if candidate.is_empty()
        || candidate.len() > MAX_URL_BYTES
        || candidate.chars().any(char::is_control)
    {
        return None;
    }
    let parsed = url::Url::parse(candidate).ok()?;
    if parsed.scheme() != "http"
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
        || parsed.path() != "/"
        || !matches!(parsed.host()?, Host::Ipv4(_) | Host::Ipv6(_))
    {
        return None;
    }
    Some(parsed.as_str().trim_end_matches('/').to_owned())
}

fn safe_message(message: &str, configured: bool) -> String {
    let normalized = normalize_message(message);
    if !configured {
        let lowercase = normalized.to_lowercase();
        if normalized.is_empty()
            || ["tap", "dismiss", "return", "back"]
                .iter()
                .any(|word| lowercase.contains(word))
        {
            return REQUIRED_MESSAGE.to_owned();
        }
    }
    if normalized.is_empty() {
        if configured {
            "Settings are available on this page".to_owned()
        } else {
            REQUIRED_MESSAGE.to_owned()
        }
    } else {
        normalized
    }
}

fn normalize_message(message: &str) -> String {
    let mut normalized = String::new();
    let mut previous_space = true;
    for character in message.chars().take(MAX_MESSAGE_CHARACTERS) {
        let character = if character.is_control() {
            if character.is_whitespace() {
                ' '
            } else {
                '\u{fffd}'
            }
        } else {
            character
        };
        if character.is_whitespace() {
            if !previous_space {
                normalized.push(' ');
            }
            previous_space = true;
        } else {
            normalized.push(character);
            previous_space = false;
        }
    }
    normalized.trim().to_owned()
}

fn wrap_message(
    text: &TextRasterizer<'_>,
    message: &str,
    max_width: f32,
    cap_height: f32,
    max_lines: usize,
) -> Vec<String> {
    if message.is_empty() || max_lines == 0 {
        return Vec::new();
    }
    let mut lines = Vec::new();
    let mut current = String::new();
    let mut words: std::collections::VecDeque<String> =
        message.split_whitespace().map(str::to_owned).collect();
    while let Some(word) = words.pop_front() {
        let candidate = if current.is_empty() {
            word.clone()
        } else {
            format!("{current} {word}")
        };
        if text.measure(&candidate, cap_height).0 <= max_width {
            current = candidate;
            continue;
        }

        if !current.is_empty() {
            if lines.len() + 1 >= max_lines {
                return finish_with_ellipsis(text, lines, current, max_width, cap_height);
            }
            lines.push(std::mem::take(&mut current));
            words.push_front(word);
            continue;
        }

        let (prefix, remainder) = fitting_prefix(text, &word, max_width, cap_height);
        current = prefix;
        if remainder.is_empty() {
            continue;
        }
        if lines.len() + 1 >= max_lines {
            return finish_with_ellipsis(text, lines, current, max_width, cap_height);
        }
        lines.push(std::mem::take(&mut current));
        words.push_front(remainder);
    }
    if !current.is_empty() && lines.len() < max_lines {
        lines.push(current);
    }
    lines
}

fn fitting_prefix(
    text: &TextRasterizer<'_>,
    word: &str,
    max_width: f32,
    cap_height: f32,
) -> (String, String) {
    let mut prefix = String::new();
    let mut remainder = String::new();
    let mut fits = true;
    for character in word.chars() {
        if fits {
            let mut candidate = prefix.clone();
            candidate.push(character);
            if text.measure(&candidate, cap_height).0 <= max_width {
                prefix = candidate;
                continue;
            }
            fits = false;
        }
        remainder.push(character);
    }
    (prefix, remainder)
}

fn finish_with_ellipsis(
    text: &TextRasterizer<'_>,
    mut lines: Vec<String>,
    mut current: String,
    max_width: f32,
    cap_height: f32,
) -> Vec<String> {
    current = current.trim_end().to_owned();
    loop {
        let candidate = format!("{current}…");
        if text.measure(&candidate, cap_height).0 <= max_width {
            lines.push(candidate);
            return lines;
        }
        if current.pop().is_none() {
            lines.push("…".to_owned());
            return lines;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_wrapping_keeps_words_intact_when_each_word_fits() {
        let font = FontAsset::embedded().expect("embedded font");
        let text = TextRasterizer::new(font.font());

        assert_eq!(
            wrap_message(
                &text,
                "Settings are available on this page",
                MESSAGE_MAX_WIDTH,
                14.0,
                2
            ),
            ["Settings are available on", "this page"]
        );
    }
}

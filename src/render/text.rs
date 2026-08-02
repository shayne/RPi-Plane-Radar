use fontdue::Font;
use tiny_skia::Pixmap;

const MAX_TEXT_GLYPHS: usize = 256;

#[derive(Clone, Copy)]
pub enum HorizontalAnchor {
    Left,
    Center,
    Right,
}

#[derive(Clone, Copy)]
pub enum VerticalAnchor {
    Top,
    Middle,
    Bottom,
}

#[derive(Clone, Copy)]
pub struct TextStyle {
    pub cap_height: f32,
    pub color: [u8; 4],
    pub horizontal: HorizontalAnchor,
    pub vertical: VerticalAnchor,
}

pub struct TextRasterizer<'a> {
    font: &'a Font,
}

impl<'a> TextRasterizer<'a> {
    pub fn new(font: &'a Font) -> Self {
        Self { font }
    }

    pub fn measure(&self, text: &str, cap_height: f32) -> (f32, f32) {
        let px = self.pixel_size_for_cap_height(cap_height);
        let width = self.text_width(text, px);
        let height = self
            .font
            .horizontal_line_metrics(px)
            .map_or(cap_height, |metrics| metrics.new_line_size.ceil());
        (width, height)
    }

    pub fn fit_with_ellipsis(&self, text: &str, cap_height: f32, max_width: f32) -> String {
        let sanitized = display_characters(text).collect::<String>();
        if sanitized.is_empty()
            || !cap_height.is_finite()
            || cap_height <= 0.0
            || max_width.is_nan()
            || max_width <= 0.0
        {
            return String::new();
        }
        if self.measure(&sanitized, cap_height).0 <= max_width {
            return sanitized;
        }
        if self.measure("…", cap_height).0 > max_width {
            return String::new();
        }

        let mut prefix = sanitized.chars().collect::<Vec<_>>();
        prefix.truncate(MAX_TEXT_GLYPHS.saturating_sub(1));
        loop {
            while prefix.last() == Some(&'…') {
                prefix.pop();
            }
            let candidate = prefix
                .iter()
                .chain(std::iter::once(&'…'))
                .collect::<String>();
            if self.measure(&candidate, cap_height).0 <= max_width {
                return candidate;
            }
            if prefix.pop().is_none() {
                return String::new();
            }
        }
    }

    pub fn draw(&self, pixmap: &mut Pixmap, text: &str, x: f32, y: f32, style: TextStyle) {
        if text.is_empty() || !style.cap_height.is_finite() || style.cap_height <= 0.0 {
            return;
        }
        let px = self.pixel_size_for_cap_height(style.cap_height);
        let Some(line_metrics) = self.font.horizontal_line_metrics(px) else {
            return;
        };
        let width = self.text_width(text, px);
        let height = line_metrics.new_line_size.ceil();
        let start_x = match style.horizontal {
            HorizontalAnchor::Left => x,
            HorizontalAnchor::Center => x - width / 2.0,
            HorizontalAnchor::Right => x - width,
        };
        let top = match style.vertical {
            VerticalAnchor::Top => y,
            VerticalAnchor::Middle => y - height / 2.0,
            VerticalAnchor::Bottom => y - height,
        };
        let baseline = top + line_metrics.ascent;
        let mut cursor = start_x;
        let mut previous = None;

        for character in display_characters(text) {
            if let Some(previous) = previous {
                cursor += self
                    .font
                    .horizontal_kern(previous, character, px)
                    .unwrap_or(0.0);
            }
            let (metrics, coverage) = self.font.rasterize(character, px);
            let glyph_x = (cursor + metrics.xmin as f32).round() as i32;
            let glyph_y = (baseline - metrics.height as f32 - metrics.ymin as f32).round() as i32;
            blend_glyph(
                pixmap,
                glyph_x,
                glyph_y,
                metrics.width,
                metrics.height,
                &coverage,
                style.color,
            );
            cursor += metrics.advance_width;
            previous = Some(character);
            if cursor > pixmap.width() as f32 + width {
                break;
            }
        }
    }

    fn pixel_size_for_cap_height(&self, target: f32) -> f32 {
        let mut low = 1.0;
        let mut high = (target * 2.0).max(2.0);
        for _ in 0..16 {
            let middle = (low + high) / 2.0;
            let (metrics, _) = self.font.rasterize('H', middle);
            if metrics.height as f32 >= target {
                high = middle;
            } else {
                low = middle;
            }
        }
        high
    }

    fn text_width(&self, text: &str, px: f32) -> f32 {
        let mut width = 0.0;
        let mut previous = None;
        for character in display_characters(text) {
            if let Some(previous) = previous {
                width += self
                    .font
                    .horizontal_kern(previous, character, px)
                    .unwrap_or(0.0);
            }
            width += self.font.metrics(character, px).advance_width;
            previous = Some(character);
        }
        width
    }
}

fn display_characters(text: &str) -> impl Iterator<Item = char> + '_ {
    text.chars().take(MAX_TEXT_GLYPHS).map(|character| {
        if character.is_control() {
            if character.is_whitespace() {
                ' '
            } else {
                '\u{fffd}'
            }
        } else {
            character
        }
    })
}

fn blend_glyph(
    pixmap: &mut Pixmap,
    glyph_x: i32,
    glyph_y: i32,
    width: usize,
    height: usize,
    coverage: &[u8],
    color: [u8; 4],
) {
    let pixmap_width = pixmap.width();
    let pixmap_height = pixmap.height();
    let data = pixmap.data_mut();
    for row in 0..height {
        for column in 0..width {
            let alpha = coverage[row * width + column];
            if alpha == 0 {
                continue;
            }
            let Ok(column) = i32::try_from(column) else {
                continue;
            };
            let Ok(row) = i32::try_from(row) else {
                continue;
            };
            let x = glyph_x.saturating_add(column);
            let y = glyph_y.saturating_add(row);
            let (Ok(x), Ok(y)) = (u32::try_from(x), u32::try_from(y)) else {
                continue;
            };
            if x >= pixmap_width || y >= pixmap_height {
                continue;
            }
            let Some(offset) = y
                .checked_mul(pixmap_width)
                .and_then(|offset| offset.checked_add(x))
                .and_then(|offset| usize::try_from(offset).ok())
                .and_then(|offset| offset.checked_mul(4))
            else {
                continue;
            };
            for channel in 0..3 {
                let background = u16::from(data[offset + channel]);
                let foreground = u16::from(color[channel]);
                let alpha = u16::from(alpha);
                data[offset + channel] =
                    ((foreground * alpha + background * (255 - alpha) + 127) / 255) as u8;
            }
            data[offset + 3] = 255;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{MAX_TEXT_GLYPHS, TextRasterizer};
    use fontdue::{Font, FontSettings};

    fn rasterizer() -> TextRasterizer<'static> {
        let font = Font::from_bytes(
            include_bytes!("../assets/DejaVuSans-Bold.ttf") as &[u8],
            FontSettings::default(),
        )
        .expect("embedded DejaVu font");
        TextRasterizer::new(Box::leak(Box::new(font)))
    }

    #[test]
    fn fit_with_ellipsis_keeps_fitting_text_unchanged() {
        let text = rasterizer();
        let max_width = text.measure("RADAR7", 21.0).0;

        assert_eq!(text.fit_with_ellipsis("RADAR7", 21.0, max_width), "RADAR7");
    }

    #[test]
    fn fit_with_ellipsis_truncates_on_unicode_boundaries_with_one_ellipsis() {
        let text = rasterizer();
        let max_width = text.measure("航班…", 21.0).0;

        let fitted = text.fit_with_ellipsis("航班ABCD", 21.0, max_width);

        assert_eq!(fitted, "航班…");
        assert_eq!(
            fitted.chars().filter(|&character| character == '…').count(),
            1
        );
        assert!(text.measure(&fitted, 21.0).0 <= max_width);
    }

    #[test]
    fn fit_with_ellipsis_returns_empty_when_even_ellipsis_cannot_fit() {
        let text = rasterizer();
        let max_width = text.measure("…", 21.0).0 - 0.1;

        assert_eq!(text.fit_with_ellipsis("RADAR7", 21.0, max_width), "");
    }

    #[test]
    fn fit_with_ellipsis_sanitizes_controls_through_existing_glyph_rules() {
        let text = rasterizer();

        assert_eq!(
            text.fit_with_ellipsis("A\tB\0C", 21.0, f32::INFINITY),
            "A B\u{fffd}C"
        );
    }

    #[test]
    fn fit_with_ellipsis_never_returns_more_than_the_display_glyph_limit() {
        let text = rasterizer();
        let fitted = text.fit_with_ellipsis(&"A".repeat(MAX_TEXT_GLYPHS + 20), 21.0, f32::INFINITY);

        assert_eq!(fitted.chars().count(), MAX_TEXT_GLYPHS);
    }
}

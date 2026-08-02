use std::cmp::Ordering;
use std::time::Duration;

use fontdue::Font;
use tiny_skia::{FillRule, Paint, PathBuilder, Pixmap, Rect, Transform};

use crate::model::{EnvironmentReading, RadarSettings};
use crate::render::text::{HorizontalAnchor, TextRasterizer, TextStyle, VerticalAnchor};
use crate::render::theme;
use crate::weather::{self, FooterContent, FooterItem, FooterTone};

const SEPARATOR: &str = " · ";

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FooterBounds {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}

impl FooterBounds {
    pub fn intersects(self, other: Self) -> bool {
        self.left < other.right
            && self.right > other.left
            && self.top < other.bottom
            && self.bottom > other.top
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct FooterRow {
    pub items: Vec<FooterItem>,
    pub width: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FooterLayout {
    pub bounds: FooterBounds,
    pub rows: Vec<FooterRow>,
    cap_height: f32,
    line_height: f32,
}

pub fn layout_footer(
    font: &Font,
    settings: &RadarSettings,
    content: &FooterContent,
) -> Option<FooterLayout> {
    let mut items = content
        .environment
        .iter()
        .chain(&content.temporal)
        .cloned()
        .collect::<Vec<_>>();
    if items.is_empty() {
        return None;
    }
    items.sort_by_key(|item| tone_order(item.tone));

    let text = TextRasterizer::new(font);
    let cap_height =
        theme::FOOTER_CAP_HEIGHT * f32::from(settings.radar_text_scale_percent) / 100.0;
    let line_height = text.measure("H", cap_height).1;
    let maximum_width = maximum_width();
    let available_width = maximum_width - 2.0 * theme::FOOTER_PADDING_X;

    let all = measured_row(&text, cap_height, items.clone());
    let mut rows = if all.width <= available_width {
        vec![all]
    } else {
        let environment_count = items
            .iter()
            .take_while(|item| tone_order(item.tone) <= tone_order(FooterTone::Humidity))
            .count();
        let preferred = (environment_count > 0 && environment_count < items.len()).then(|| {
            [
                measured_row(&text, cap_height, items[..environment_count].to_vec()),
                measured_row(&text, cap_height, items[environment_count..].to_vec()),
            ]
        });
        if let Some(preferred) =
            preferred.filter(|rows| rows.iter().all(|row| row.width <= available_width))
        {
            preferred.into_iter().collect()
        } else {
            best_two_rows(&text, cap_height, &items, available_width)
                .or_else(|| best_three_rows(&text, cap_height, &items, available_width))
                .or_else(|| {
                    (4..=items.len()).find_map(|row_count| {
                        best_ordered_rows(&text, cap_height, &items, available_width, row_count)
                    })
                })?
        }
    };

    for row in &mut rows {
        fit_row(&text, cap_height, row, available_width)?;
    }
    let rail_width =
        rows.iter().map(|row| row.width).fold(0.0_f32, f32::max) + 2.0 * theme::FOOTER_PADDING_X;
    let rail_height = line_height * rows.len() as f32
        + theme::FOOTER_ROW_GAP * rows.len().saturating_sub(1) as f32
        + 2.0 * theme::FOOTER_PADDING_Y;
    let left = theme::CENTER.0 - rail_width / 2.0;

    Some(FooterLayout {
        bounds: FooterBounds {
            left,
            top: theme::FOOTER_BOTTOM_Y - rail_height,
            right: left + rail_width,
            bottom: theme::FOOTER_BOTTOM_Y,
        },
        rows,
        cap_height,
        line_height,
    })
}

pub fn draw_footer(
    pixmap: &mut Pixmap,
    font: &Font,
    settings: &RadarSettings,
    reading: Option<&EnvironmentReading>,
    monotonic_now: Duration,
    unix_seconds: u64,
) -> Option<FooterBounds> {
    let content = weather::footer_content(&settings.footer, reading, monotonic_now, unix_seconds);
    let layout = layout_footer(font, settings, &content)?;
    fill_rounded_rail(
        pixmap,
        layout.bounds,
        theme::FOOTER_CORNER_RADIUS,
        theme::FOOTER_BORDER,
    );
    let inset = theme::FOOTER_BORDER_WIDTH;
    fill_rounded_rail(
        pixmap,
        FooterBounds {
            left: layout.bounds.left + inset,
            top: layout.bounds.top + inset,
            right: layout.bounds.right - inset,
            bottom: layout.bounds.bottom - inset,
        },
        (theme::FOOTER_CORNER_RADIUS - inset).max(0.0),
        theme::FOOTER_BACKGROUND,
    );

    let text = TextRasterizer::new(font);
    let separator_width = text.measure(SEPARATOR, layout.cap_height).0;
    for (row_index, row) in layout.rows.iter().enumerate() {
        let row_top = layout.bounds.top
            + theme::FOOTER_PADDING_Y
            + row_index as f32 * (layout.line_height + theme::FOOTER_ROW_GAP);
        let mut x = theme::CENTER.0 - row.width / 2.0;
        for (index, item) in row.items.iter().enumerate() {
            if index > 0 {
                text.draw(
                    pixmap,
                    SEPARATOR,
                    x,
                    row_top,
                    TextStyle {
                        cap_height: layout.cap_height,
                        color: theme::FOOTER_BORDER,
                        horizontal: HorizontalAnchor::Left,
                        vertical: VerticalAnchor::Top,
                    },
                );
                x += separator_width;
            }
            text.draw(
                pixmap,
                &item.text,
                x,
                row_top,
                TextStyle {
                    cap_height: layout.cap_height,
                    color: tone_color(item.tone),
                    horizontal: HorizontalAnchor::Left,
                    vertical: VerticalAnchor::Top,
                },
            );
            x += text.measure(&item.text, layout.cap_height).0;
        }
    }
    Some(layout.bounds)
}

fn maximum_width() -> f32 {
    let dy = theme::FOOTER_BOTTOM_Y - theme::CENTER.1;
    let radius = theme::RIM_RADIUS as f32;
    let chord = 2.0 * (radius.powi(2) - dy.powi(2)).max(0.0).sqrt();
    chord - theme::FOOTER_CHORD_INSET
}

fn measured_row(text: &TextRasterizer<'_>, cap_height: f32, items: Vec<FooterItem>) -> FooterRow {
    let item_width = items
        .iter()
        .map(|item| text.measure(&item.text, cap_height).0)
        .sum::<f32>();
    let separators = text.measure(SEPARATOR, cap_height).0 * items.len().saturating_sub(1) as f32;
    FooterRow {
        items,
        width: item_width + separators,
    }
}

fn minimum_row_width(text: &TextRasterizer<'_>, cap_height: f32, row: &FooterRow) -> f32 {
    let items = row
        .items
        .iter()
        .map(|item| {
            if ellipsizable(item.tone) {
                text.measure("…", cap_height).0
            } else {
                text.measure(&item.text, cap_height).0
            }
        })
        .sum::<f32>();
    items + text.measure(SEPARATOR, cap_height).0 * row.items.len().saturating_sub(1) as f32
}

fn best_two_rows(
    text: &TextRasterizer<'_>,
    cap_height: f32,
    items: &[FooterItem],
    available_width: f32,
) -> Option<Vec<FooterRow>> {
    best_ordered_rows(text, cap_height, items, available_width, 2)
}

fn best_three_rows(
    text: &TextRasterizer<'_>,
    cap_height: f32,
    items: &[FooterItem],
    available_width: f32,
) -> Option<Vec<FooterRow>> {
    best_ordered_rows(text, cap_height, items, available_width, 3)
}

fn best_ordered_rows(
    text: &TextRasterizer<'_>,
    cap_height: f32,
    items: &[FooterItem],
    available_width: f32,
    row_count: usize,
) -> Option<Vec<FooterRow>> {
    if row_count == 0 || row_count > items.len() {
        return None;
    }
    let mut best: Option<(f32, Vec<FooterRow>)> = None;
    enumerate_partitions(
        text,
        cap_height,
        items,
        available_width,
        row_count,
        0,
        &mut Vec::with_capacity(row_count),
        &mut best,
    );
    best.map(|(_, rows)| rows)
}

#[allow(clippy::too_many_arguments)]
fn enumerate_partitions(
    text: &TextRasterizer<'_>,
    cap_height: f32,
    items: &[FooterItem],
    available_width: f32,
    rows_remaining: usize,
    start: usize,
    rows: &mut Vec<FooterRow>,
    best: &mut Option<(f32, Vec<FooterRow>)>,
) {
    if rows_remaining == 1 {
        rows.push(measured_row(text, cap_height, items[start..].to_vec()));
        consider_partition(text, cap_height, rows.clone(), available_width, best);
        rows.pop();
        return;
    }

    let last_split = items.len() - rows_remaining + 1;
    for split in start + 1..=last_split {
        rows.push(measured_row(text, cap_height, items[start..split].to_vec()));
        enumerate_partitions(
            text,
            cap_height,
            items,
            available_width,
            rows_remaining - 1,
            split,
            rows,
            best,
        );
        rows.pop();
    }
}

fn consider_partition(
    text: &TextRasterizer<'_>,
    cap_height: f32,
    rows: Vec<FooterRow>,
    available_width: f32,
    best: &mut Option<(f32, Vec<FooterRow>)>,
) {
    if !rows
        .iter()
        .all(|row| minimum_row_width(text, cap_height, row) <= available_width)
    {
        return;
    }
    let score = rows.iter().map(|row| row.width).fold(0.0_f32, f32::max);
    if best
        .as_ref()
        .is_none_or(|(best_score, _)| score.partial_cmp(best_score) == Some(Ordering::Less))
    {
        *best = Some((score, rows));
    }
}

fn fit_row(
    text: &TextRasterizer<'_>,
    cap_height: f32,
    row: &mut FooterRow,
    available_width: f32,
) -> Option<()> {
    if row.width <= available_width {
        return Some(());
    }
    for tone in [FooterTone::Condition, FooterTone::Status] {
        let Some(index) = row.items.iter().position(|item| item.tone == tone) else {
            continue;
        };
        let current_width = text.measure(&row.items[index].text, cap_height).0;
        let allowed = (available_width - (row.width - current_width)).max(0.0);
        let fitted = text.fit_with_ellipsis(&row.items[index].text, cap_height, allowed);
        let fitted = if fitted.is_empty() {
            "…".to_owned()
        } else {
            fitted
        };
        row.items[index].text = fitted;
        row.width = measured_row(text, cap_height, row.items.clone()).width;
        if row.width <= available_width {
            return Some(());
        }
    }
    (row.width <= available_width).then_some(())
}

fn ellipsizable(tone: FooterTone) -> bool {
    matches!(tone, FooterTone::Status | FooterTone::Condition)
}

fn tone_order(tone: FooterTone) -> u8 {
    match tone {
        FooterTone::Status => 0,
        FooterTone::Condition => 1,
        FooterTone::Temperature => 2,
        FooterTone::Humidity => 3,
        FooterTone::Time => 4,
        FooterTone::Date => 5,
    }
}

fn tone_color(tone: FooterTone) -> [u8; 4] {
    match tone {
        FooterTone::Status | FooterTone::Condition => theme::TAG_TYPE,
        FooterTone::Temperature | FooterTone::Humidity => theme::TAG_ALTITUDE,
        FooterTone::Time | FooterTone::Date => theme::LABEL,
    }
}

fn fill_rounded_rail(pixmap: &mut Pixmap, bounds: FooterBounds, radius: f32, color: [u8; 4]) {
    let width = bounds.right - bounds.left;
    let height = bounds.bottom - bounds.top;
    if width <= 0.0 || height <= 0.0 {
        return;
    }
    let radius = radius.min(width / 2.0).min(height / 2.0).max(0.0);
    let paint = paint(color);
    if let Some(horizontal) = Rect::from_xywh(
        bounds.left + radius,
        bounds.top,
        width - 2.0 * radius,
        height,
    ) {
        pixmap.fill_rect(horizontal, &paint, Transform::identity(), None);
    }
    if let Some(vertical) = Rect::from_xywh(
        bounds.left,
        bounds.top + radius,
        width,
        height - 2.0 * radius,
    ) {
        pixmap.fill_rect(vertical, &paint, Transform::identity(), None);
    }
    for (x, y) in [
        (bounds.left + radius, bounds.top + radius),
        (bounds.right - radius, bounds.top + radius),
        (bounds.left + radius, bounds.bottom - radius),
        (bounds.right - radius, bounds.bottom - radius),
    ] {
        let Some(circle) = PathBuilder::from_circle(x, y, radius) else {
            continue;
        };
        pixmap.fill_path(
            &circle,
            &paint,
            FillRule::Winding,
            Transform::identity(),
            None,
        );
    }
}

fn paint(rgba: [u8; 4]) -> Paint<'static> {
    let mut paint = Paint::default();
    paint.set_color_rgba8(rgba[0], rgba[1], rgba[2], rgba[3]);
    paint.force_hq_pipeline = true;
    paint
}

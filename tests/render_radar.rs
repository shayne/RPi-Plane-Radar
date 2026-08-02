mod support;

use std::collections::HashMap;
use std::fs;
use std::sync::Arc;
use std::time::Duration;

use fontdue::{Font, FontSettings};
use planeradar::flight_data::AircraftEnrichment;
use planeradar::geometry::offset_km;
use planeradar::model::{
    Aircraft, Airport, EnvironmentReading, GeoPoint, Location, RadarSettings, RadarSnapshot,
    Runway, Units,
};
use planeradar::range::{format_range_label, range_preset};
use planeradar::render::footer::{FooterLayout, draw_footer, layout_footer};
use planeradar::render::radar::{BackgroundKey, RadarRenderer};
use planeradar::render::text::{HorizontalAnchor, TextRasterizer, TextStyle, VerticalAnchor};
use planeradar::render::theme::{
    AIRCRAFT, AIRCRAFT_LABEL_GAP, AIRCRAFT_NOSE_LENGTH, AIRCRAFT_SAFE_RADIUS,
    AIRCRAFT_TAG_CAP_HEIGHT, AIRCRAFT_TAIL_HALF_WIDTH, AIRCRAFT_TAIL_LENGTH, BACKGROUND,
    CARDINAL_CAP_HEIGHT, CENTER, CENTER_DOT_RADIUS, FOOTER_BACKGROUND, FOOTER_BORDER,
    FOOTER_BORDER_WIDTH, FOOTER_BOTTOM_Y, FOOTER_CAP_HEIGHT, FOOTER_CHORD_INSET,
    FOOTER_CORNER_RADIUS, FOOTER_PADDING_X, FOOTER_PADDING_Y, FOOTER_ROW_GAP, GRID,
    GRID_OUTER_RADIUS, GRID_STROKE_WIDTH, LABEL, RIM_DOT_RADIUS, RIM_RADIUS, RUNWAY,
    RUNWAY_LABEL_CAP_HEIGHT, RUNWAY_LABEL_GAP, RUNWAY_STROKE_WIDTH, SCALE_CAP_HEIGHT, SIZE, STALE,
    STALE_CAP_HEIGHT, TAG_ALTITUDE, TAG_TYPE, TRACK, TRACK_MIN_LENGTH, TRACK_STROKE_WIDTH,
};
use planeradar::render::{FontAsset, Frame, RenderError};
use planeradar::weather::{FooterContent, FooterItem, FooterTone};
use support::FrameAssertions;
use tiny_skia::{IntSize, Pixmap};

const ORIGIN_LATITUDE: f64 = 40.0;
const ORIGIN_LONGITUDE: f64 = -75.0;
const EARTH_RADIUS_KM: f64 = 6_371.008_8;
const EMBEDDED_FONT_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/src/assets/DejaVuSans-Bold.ttf"
);
const EMBEDDED_FONT_LICENSE_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/src/assets/DejaVu-FONT-LICENSE.txt"
);
const EMBEDDED_FONT_LICENSE: &str = include_str!("../src/assets/DejaVu-FONT-LICENSE.txt");

#[test]
fn visual_metrics_use_whole_pixels_without_moving_the_radar() {
    assert_eq!(BACKGROUND, [0, 0, 0, 255]);
    assert_eq!(SIZE, 480);
    assert_eq!(CENTER, (240.0, 240.0));
    assert_eq!(GRID_OUTER_RADIUS, 214.0);
    assert_eq!(AIRCRAFT_SAFE_RADIUS, 188.0);
    assert_eq!(RIM_RADIUS, 238.0);

    let refined = [
        (GRID_STROKE_WIDTH, 3.0),
        (CENTER_DOT_RADIUS, 3.0),
        (AIRCRAFT_NOSE_LENGTH, 13.0),
        (AIRCRAFT_TAIL_LENGTH, 5.0),
        (AIRCRAFT_TAIL_HALF_WIDTH, 6.0),
        (AIRCRAFT_LABEL_GAP, 2.0),
        (TRACK_MIN_LENGTH, 3.0),
        (TRACK_STROKE_WIDTH, 3.0),
        (RIM_DOT_RADIUS, 6.0),
        (RUNWAY_STROKE_WIDTH, 3.0),
        (RUNWAY_LABEL_GAP, 5.0),
        (CARDINAL_CAP_HEIGHT, 22.0),
        (SCALE_CAP_HEIGHT, 18.0),
        (AIRCRAFT_TAG_CAP_HEIGHT, 21.0),
        (RUNWAY_LABEL_CAP_HEIGHT, 22.0),
        (STALE_CAP_HEIGHT, 18.0),
    ];
    for (actual, expected) in refined {
        assert_eq!(actual, expected);
        assert_eq!(actual.fract(), 0.0);
    }

    assert_eq!(FOOTER_CAP_HEIGHT, 18.0);
    assert_eq!(FOOTER_BOTTOM_Y, 420.0);
    assert_eq!(FOOTER_PADDING_X, 12.0);
    assert_eq!(FOOTER_PADDING_Y, 8.0);
    assert_eq!(FOOTER_ROW_GAP, 4.0);
    assert_eq!(FOOTER_CORNER_RADIUS, 12.0);
    assert_eq!(FOOTER_BORDER_WIDTH, 1.0);
    assert_eq!(FOOTER_CHORD_INSET, 16.0);
    assert_eq!(FOOTER_BACKGROUND, [3, 16, 32, 255]);
    assert_eq!(FOOTER_BORDER, GRID);
}

#[test]
fn footer_with_no_selected_items_has_no_layout_bounds_or_pixels() {
    let font = test_font();
    let settings = configured_settings();
    let mut pixmap = Pixmap::new(SIZE, SIZE).expect("footer pixmap");
    let before = pixmap.data().to_vec();

    assert!(layout_footer(&font, &settings, &FooterContent::default()).is_none());
    assert_eq!(
        draw_footer(&mut pixmap, &font, &settings, None, Duration::ZERO, 0,),
        None
    );
    assert_eq!(pixmap.data(), before);
}

#[test]
fn footer_with_one_short_semantic_group_uses_one_centered_row() {
    let content = FooterContent {
        environment: vec![
            footer_item("SCT", FooterTone::Condition),
            footer_item("22°C", FooterTone::Temperature),
            footer_item("RH54%", FooterTone::Humidity),
        ],
        temporal: Vec::new(),
    };

    let layout = layout_footer(&test_font(), &configured_settings(), &content)
        .expect("one-row footer layout");

    assert_eq!(layout.rows.len(), 1);
    assert_eq!(
        flattened_footer(&layout),
        vec![
            ("SCT", FooterTone::Condition),
            ("22°C", FooterTone::Temperature),
            ("RH54%", FooterTone::Humidity),
        ]
    );
    assert!((layout.bounds.left + layout.bounds.right - 2.0 * CENTER.0).abs() < 0.01);
}

#[test]
fn footer_with_all_five_items_uses_two_rows_in_fixed_semantic_order() {
    let layout = layout_footer(&test_font(), &configured_settings(), &full_footer_content())
        .expect("two-row footer layout");

    assert_eq!(layout.rows.len(), 2);
    assert_eq!(layout.rows[0].items.len(), 3);
    assert_eq!(layout.rows[1].items.len(), 2);
    assert_eq!(
        flattened_footer(&layout),
        vec![
            ("SCT", FooterTone::Condition),
            ("22°C", FooterTone::Temperature),
            ("RH54%", FooterTone::Humidity),
            ("11:11Z", FooterTone::Time),
            ("11 JUL", FooterTone::Date),
        ]
    );
}

#[test]
fn footer_preserves_zulu_suffix_and_date_when_it_splits() {
    let layout = layout_footer(&test_font(), &configured_settings(), &full_footer_content())
        .expect("footer layout");
    let items = flattened_footer(&layout);

    assert!(items.contains(&("11:11Z", FooterTone::Time)));
    assert!(items.contains(&("11 JUL", FooterTone::Date)));
}

#[test]
fn footer_at_supported_text_scales_stays_inside_the_safe_round_chord() {
    let maximum_width = {
        let dy = FOOTER_BOTTOM_Y - CENTER.1;
        let radius = RIM_RADIUS as f32;
        2.0 * (radius.powi(2) - dy.powi(2)).max(0.0).sqrt() - FOOTER_CHORD_INSET
    };

    for scale in [80, 130] {
        let mut settings = configured_settings();
        settings.radar_text_scale_percent = scale;
        let layout = layout_footer(&test_font(), &settings, &full_footer_content())
            .expect("scaled footer layout");

        assert!(layout.bounds.right - layout.bounds.left <= maximum_width + 0.01);
        assert!(layout.bounds.left >= CENTER.0 - maximum_width / 2.0 - 0.01);
        assert!(layout.bounds.right <= CENTER.0 + maximum_width / 2.0 + 0.01);
        assert!(
            layout
                .rows
                .iter()
                .all(|row| row.width + 2.0 * FOOTER_PADDING_X <= maximum_width + 0.01),
            "text scale {scale}% overflowed the safe chord"
        );
    }
}

#[test]
fn all_five_footer_items_at_130_percent_use_three_rows_without_losing_values() {
    let mut settings = configured_settings();
    settings.radar_text_scale_percent = 130;
    let layout = layout_footer(&test_font(), &settings, &full_footer_content())
        .expect("three-row footer layout");
    let maximum_width = {
        let dy = FOOTER_BOTTOM_Y - CENTER.1;
        let radius = RIM_RADIUS as f32;
        2.0 * (radius.powi(2) - dy.powi(2)).max(0.0).sqrt() - FOOTER_CHORD_INSET
    };

    assert_eq!(layout.rows.len(), 3);
    assert_eq!(
        flattened_footer(&layout),
        vec![
            ("SCT", FooterTone::Condition),
            ("22°C", FooterTone::Temperature),
            ("RH54%", FooterTone::Humidity),
            ("11:11Z", FooterTone::Time),
            ("11 JUL", FooterTone::Date),
        ]
    );
    assert!(layout.bounds.right - layout.bounds.left <= maximum_width + 0.01);
    assert_eq!(layout.bounds.bottom, FOOTER_BOTTOM_Y);
    assert!(layout.bounds.bottom < 440.0);
}

#[test]
fn footer_ellipsizes_condition_before_selected_numeric_or_temporal_values() {
    let content = FooterContent {
        environment: vec![
            footer_item(&"CONDITION".repeat(128), FooterTone::Condition),
            footer_item("-18°F", FooterTone::Temperature),
            footer_item("RH100%", FooterTone::Humidity),
        ],
        temporal: vec![
            footer_item("11:59PMZ", FooterTone::Time),
            footer_item("31 DEC", FooterTone::Date),
        ],
    };
    let mut settings = configured_settings();
    settings.radar_text_scale_percent = 130;

    let layout = layout_footer(&test_font(), &settings, &content).expect("fitted footer layout");
    let items = flattened_footer(&layout);

    assert_eq!(layout.rows.len(), 4);
    assert!(
        items
            .iter()
            .find(|(_, tone)| *tone == FooterTone::Condition)
            .expect("condition item")
            .0
            .ends_with('…')
    );
    for expected in [
        ("-18°F", FooterTone::Temperature),
        ("RH100%", FooterTone::Humidity),
        ("11:59PMZ", FooterTone::Time),
        ("31 DEC", FooterTone::Date),
    ] {
        assert!(
            items.contains(&expected),
            "selected value {expected:?} was changed or dropped"
        );
    }
}

#[test]
fn footer_bounds_stop_above_the_south_cardinal_label() {
    let layout = layout_footer(&test_font(), &configured_settings(), &full_footer_content())
        .expect("footer layout");

    assert_eq!(layout.bounds.bottom, FOOTER_BOTTOM_Y);
    assert!(layout.bounds.bottom < 440.0);
}

#[test]
fn footer_rail_paints_over_the_static_grid_but_keeps_the_south_label_clear() {
    let settings = footer_settings();
    let snapshot = snapshot_with_footer(
        Vec::new(),
        footer_reading(Duration::ZERO),
        Some(Duration::ZERO),
    );
    let disabled = test_renderer()
        .render(
            &empty_snapshot(Some(Duration::ZERO)),
            &configured_settings(),
            &[],
            Duration::ZERO,
            0,
        )
        .expect("disabled footer render");
    let enabled = test_renderer()
        .render(&snapshot, &settings, &[], Duration::ZERO, 0)
        .expect("enabled footer render");

    assert_eq!(disabled.pixel(CENTER.0 as u32, 417), GRID);
    assert_eq!(enabled.pixel(CENTER.0 as u32, 417), FOOTER_BACKGROUND);
    assert!(enabled.region_is_white(220, 440, 40, 40), "south label");
}

#[test]
fn aircraft_symbol_drawn_after_footer_can_overwrite_the_rail() {
    let settings = footer_settings();
    let snapshot = snapshot_with_footer(
        vec![aircraft_at(0.0, -10.0, 0.0)],
        footer_reading(Duration::ZERO),
        Some(Duration::ZERO),
    );
    let frame = test_renderer()
        .render(&snapshot, &settings, &[], Duration::ZERO, 0)
        .expect("aircraft over footer render");

    assert!(
        frame.color_count(AIRCRAFT, 220, 380, 40, 40) > 0,
        "aircraft symbol inside the rail must remain traffic red"
    );
}

#[test]
fn aircraft_tag_moves_above_footer_when_the_natural_block_intersects() {
    let mut settings = configured_settings();
    settings.footer.show_time = true;
    settings.footer.show_date = true;
    settings.footer.time_zone = planeradar::model::TimeZone::Zulu;
    let plane = aircraft_at(4.0, -9.0, 0.0);
    let snapshot = snapshot_with_footer(
        vec![plane.clone()],
        footer_reading(Duration::ZERO),
        Some(Duration::ZERO),
    );
    let mut footer_pixmap = Pixmap::new(SIZE, SIZE).expect("footer pixmap");
    let footer_bounds = draw_footer(
        &mut footer_pixmap,
        &test_font(),
        &settings,
        snapshot.environment.as_ref(),
        Duration::ZERO,
        0,
    )
    .expect("footer bounds");

    let natural = render_tag(&configured_settings(), plane, AircraftEnrichment::default());
    let avoided = test_renderer()
        .render(&snapshot, &settings, &[], Duration::ZERO, 0)
        .expect("footer avoidance render");
    let (_, natural_top, _, natural_height) = color_bounds(&natural, TAG_TYPE);
    let (_, avoided_top, _, avoided_height) = color_bounds(&avoided, TAG_TYPE);

    assert!(natural_top as f32 + natural_height as f32 > footer_bounds.top);
    assert!((natural_top as f32) < footer_bounds.bottom);
    assert!(
        avoided_top as f32 + avoided_height as f32 <= footer_bounds.top + 1.0,
        "tag type glyphs must move above the footer when that placement fits"
    );
}

fn test_renderer() -> RadarRenderer {
    RadarRenderer::new(FontAsset::embedded().expect("embedded DejaVu font"))
}

fn test_font() -> Font {
    Font::from_bytes(
        include_bytes!("../src/assets/DejaVuSans-Bold.ttf") as &[u8],
        FontSettings::default(),
    )
    .expect("embedded DejaVu font")
}

fn footer_item(text: &str, tone: FooterTone) -> FooterItem {
    FooterItem {
        text: text.to_owned(),
        tone,
    }
}

fn full_footer_content() -> FooterContent {
    FooterContent {
        environment: vec![
            footer_item("22°C", FooterTone::Temperature),
            footer_item("SCT", FooterTone::Condition),
            footer_item("RH54%", FooterTone::Humidity),
        ],
        temporal: vec![
            footer_item("11 JUL", FooterTone::Date),
            footer_item("11:11Z", FooterTone::Time),
        ],
    }
}

fn flattened_footer(layout: &FooterLayout) -> Vec<(&str, FooterTone)> {
    layout
        .rows
        .iter()
        .flat_map(|row| row.items.iter().map(|item| (item.text.as_str(), item.tone)))
        .collect()
}

fn footer_settings() -> RadarSettings {
    let mut settings = configured_settings();
    settings.footer.show_condition = true;
    settings.footer.show_temperature = true;
    settings.footer.show_humidity = true;
    settings.footer.show_time = true;
    settings.footer.show_date = true;
    settings.footer.time_zone = planeradar::model::TimeZone::Zulu;
    settings
}

fn footer_reading(fetched_at: Duration) -> EnvironmentReading {
    EnvironmentReading {
        temperature_celsius: 22.0,
        humidity_percent: 54,
        weather_code: 2,
        utc_offset_seconds: -4 * 60 * 60,
        fetched_at,
    }
}

fn range_glyph_mask(settings: &RadarSettings) -> Frame {
    let font = Font::from_bytes(
        include_bytes!("../src/assets/DejaVuSans-Bold.ttf") as &[u8],
        FontSettings {
            collection_index: 0,
            scale: 40.0,
            load_substitutions: true,
        },
    )
    .expect("embedded DejaVu font");
    let preset = range_preset(settings.range_index).expect("configured range");
    let label = format_range_label(preset, settings.units);
    let anchor_x = CENTER.0 + GRID_OUTER_RADIUS - 12.0;
    let mut mask = Pixmap::new(SIZE, SIZE).expect("range mask");
    TextRasterizer::new(&font).draw(
        &mut mask,
        &label,
        anchor_x,
        CENTER.1,
        TextStyle {
            cap_height: SCALE_CAP_HEIGHT,
            color: LABEL,
            horizontal: HorizontalAnchor::Right,
            vertical: VerticalAnchor::Middle,
        },
    );
    Frame::new(SIZE, SIZE, mask.take()).expect("range mask frame")
}

fn mask_covers(mask: &Frame, x: u32, y: u32) -> bool {
    mask.pixel(x, y)[3] != 0
}

fn mask_within_one_pixel(mask: &Frame, x: u32, y: u32) -> bool {
    let left = x.saturating_sub(1);
    let top = y.saturating_sub(1);
    let right = x.saturating_add(1).min(SIZE - 1);
    let bottom = y.saturating_add(1).min(SIZE - 1);
    (top..=bottom).any(|mask_y| (left..=right).any(|mask_x| mask_covers(mask, mask_x, mask_y)))
}

fn configured_settings() -> RadarSettings {
    RadarSettings {
        location: Some(Location {
            latitude: ORIGIN_LATITUDE,
            longitude: ORIGIN_LONGITUDE,
            label: "Fixture".to_owned(),
        }),
        units: Units::Kilometres,
        show_runways: true,
        range_index: 1,
        ..RadarSettings::default()
    }
}

fn empty_snapshot(fetched_at: Option<Duration>) -> RadarSnapshot {
    RadarSnapshot {
        aircraft: Arc::from([]),
        enrichment: Arc::new(HashMap::new()),
        environment: None,
        fetched_at,
        last_error_at: None,
    }
}

fn east_longitude(kilometres: f64) -> f64 {
    ORIGIN_LONGITUDE
        + kilometres / (EARTH_RADIUS_KM * ORIGIN_LATITUDE.to_radians().cos()) * 180.0
            / std::f64::consts::PI
}

fn aircraft(east_km: f64, speed: f64) -> Aircraft {
    Aircraft {
        hex: "a00001".to_owned(),
        flight_callsign: "EAST123".to_owned(),
        latitude: ORIGIN_LATITUDE,
        longitude: east_longitude(east_km),
        nose_degrees: 90.0,
        track_degrees: 90.0,
        ground_speed_knots: speed,
        callsign: "EAST123".to_owned(),
        aircraft_type: "A320".to_owned(),
        altitude_feet: Some(12_000),
        altitude: "12000".to_owned(),
    }
}

fn aircraft_at(east_km: f64, north_km: f64, speed: f64) -> Aircraft {
    let mut plane = aircraft(east_km, speed);
    plane.latitude = ORIGIN_LATITUDE + north_km / EARTH_RADIUS_KM * 180.0 / std::f64::consts::PI;
    plane
}

fn snapshot_with_footer(
    aircraft: Vec<Aircraft>,
    reading: EnvironmentReading,
    fetched_at: Option<Duration>,
) -> RadarSnapshot {
    RadarSnapshot {
        aircraft: Arc::from(aircraft),
        enrichment: Arc::new(HashMap::new()),
        environment: Some(reading),
        fetched_at,
        last_error_at: None,
    }
}

fn runway_airport() -> Airport {
    Airport {
        ident: "KFIX".to_owned(),
        location: GeoPoint {
            latitude: ORIGIN_LATITUDE,
            longitude: ORIGIN_LONGITUDE,
        },
        runways: vec![Runway {
            low_end: GeoPoint {
                latitude: ORIGIN_LATITUDE,
                longitude: east_longitude(-2.0),
            },
            high_end: GeoPoint {
                latitude: ORIGIN_LATITUDE,
                longitude: east_longitude(2.0),
            },
        }],
    }
}

fn tag_snapshot(plane: Aircraft, enrichment: AircraftEnrichment) -> RadarSnapshot {
    let key = plane.key();
    RadarSnapshot {
        aircraft: Arc::from([plane]),
        enrichment: Arc::new(HashMap::from([(key, enrichment)])),
        environment: None,
        fetched_at: Some(Duration::ZERO),
        last_error_at: None,
    }
}

fn render_tag(settings: &RadarSettings, plane: Aircraft, enrichment: AircraftEnrichment) -> Frame {
    test_renderer()
        .render(
            &tag_snapshot(plane, enrichment),
            settings,
            &[],
            Duration::ZERO,
            0,
        )
        .expect("tag render")
}

fn expected_tag(settings: &RadarSettings, plane: &Aircraft, lines: &[(&str, [u8; 4])]) -> Frame {
    let mut unlabelled = plane.clone();
    unlabelled.callsign.clear();
    unlabelled.aircraft_type.clear();
    unlabelled.altitude.clear();
    let mut base_settings = settings.clone();
    base_settings.show_callsign = false;
    base_settings.show_route = false;
    base_settings.show_expanded_model = false;
    let base = render_tag(&base_settings, unlabelled, AircraftEnrichment::default());
    let mut pixmap = Pixmap::from_vec(
        base.pixels().to_vec(),
        IntSize::from_wh(SIZE, SIZE).expect("radar size"),
    )
    .expect("base frame pixmap");
    let font = Font::from_bytes(
        include_bytes!("../src/assets/DejaVuSans-Bold.ttf") as &[u8],
        FontSettings::default(),
    )
    .expect("embedded DejaVu font");
    let text = TextRasterizer::new(&font);
    let cap_height = AIRCRAFT_TAG_CAP_HEIGHT * f32::from(settings.radar_text_scale_percent) / 100.0;
    let line_height = text.measure("H", cap_height).1;
    let block_width = lines
        .iter()
        .map(|(line, _)| text.measure(line, cap_height).0)
        .fold(0.0_f32, f32::max);
    let block_height = line_height * lines.len() as f32;
    let location = settings.location.as_ref().expect("configured location");
    let preset = range_preset(settings.range_index).expect("configured range");
    let aircraft_offset = offset_km(location, plane.latitude, plane.longitude);
    let pixels_per_kilometre = f64::from(GRID_OUTER_RADIUS) / preset.outer_km;
    let x = CENTER.0 + (aircraft_offset.east * pixels_per_kilometre) as f32;
    let y = CENTER.1 - (aircraft_offset.north * pixels_per_kilometre) as f32;
    let symbol_half = AIRCRAFT_NOSE_LENGTH + AIRCRAFT_TAIL_HALF_WIDTH;
    let on_right = x < CENTER.0;
    let (anchor_x, horizontal) = if on_right {
        (
            (x + symbol_half + AIRCRAFT_LABEL_GAP).min(SIZE as f32 - block_width - 1.0),
            HorizontalAnchor::Left,
        )
    } else {
        (
            (x - symbol_half - AIRCRAFT_LABEL_GAP).max(block_width + 1.0),
            HorizontalAnchor::Right,
        )
    };
    let top = (y - block_height / 2.0).clamp(1.0, SIZE as f32 - block_height - 1.0);
    for (index, (line, color)) in lines.iter().enumerate() {
        text.draw(
            &mut pixmap,
            line,
            anchor_x,
            top + line_height * index as f32,
            TextStyle {
                cap_height,
                color: *color,
                horizontal,
                vertical: VerticalAnchor::Top,
            },
        );
    }
    Frame::new(SIZE, SIZE, pixmap.take()).expect("expected tag frame")
}

fn color_bounds(frame: &Frame, wanted: [u8; 4]) -> (u32, u32, u32, u32) {
    let mut left = SIZE;
    let mut top = SIZE;
    let mut right = 0;
    let mut bottom = 0;
    for y in 0..SIZE {
        for x in 0..SIZE {
            if frame.pixel(x, y) == wanted {
                left = left.min(x);
                top = top.min(y);
                right = right.max(x);
                bottom = bottom.max(y);
            }
        }
    }
    assert!(
        left <= right && top <= bottom,
        "frame must contain requested color"
    );
    (left, top, right - left + 1, bottom - top + 1)
}

fn region_pixels(frame: &Frame, left: u32, top: u32, width: u32, height: u32) -> Vec<u8> {
    let mut pixels = Vec::new();
    for y in top..top + height {
        for x in left..left + width {
            pixels.extend_from_slice(&frame.pixel(x, y));
        }
    }
    pixels
}

#[test]
fn empty_radar_has_exact_size_palette_rings_and_center() {
    let mut renderer = test_renderer();
    let frame = renderer
        .render(
            &empty_snapshot(Some(Duration::ZERO)),
            &configured_settings(),
            &[],
            Duration::ZERO,
            0,
        )
        .expect("render");

    assert_eq!(frame.dimensions(), (SIZE, SIZE));
    assert_eq!(frame.pixel(0, 0), BACKGROUND);
    assert_eq!(CENTER, (240.0, 240.0));
    assert_eq!(GRID_OUTER_RADIUS, 214.0);
    assert_ne!(frame.pixel(240, 240), BACKGROUND);
    for radius in [53_u32, 107, 160, 214] {
        assert!(
            frame.color_count(GRID, 240 + radius - 2, 238, 5, 5) > 0,
            "ring at radius {radius} must cross the east spoke"
        );
    }
}

#[test]
fn cardinal_labels_fit_the_north_south_east_west_bounds() {
    let mut renderer = test_renderer();
    let frame = renderer
        .render(
            &empty_snapshot(Some(Duration::ZERO)),
            &configured_settings(),
            &[],
            Duration::ZERO,
            0,
        )
        .expect("render");

    assert!(frame.region_is_white(220, 0, 40, 40), "north label");
    assert!(frame.region_is_white(220, 440, 40, 40), "south label");
    assert!(frame.region_is_white(0, 220, 40, 40), "west label");
    assert!(frame.region_is_white(440, 220, 40, 40), "east label");
    assert_eq!(frame.dark_square_count(0, 0, 8), 64);
}

#[test]
fn east_aircraft_uses_unrounded_projection_and_draws_tag_and_heading() {
    let snapshot = RadarSnapshot {
        aircraft: Arc::from([aircraft(5.0, 120.0)]),
        enrichment: Arc::new(HashMap::new()),
        environment: None,
        fetched_at: Some(Duration::ZERO),
        last_error_at: None,
    };
    let mut renderer = test_renderer();
    let frame = renderer
        .render(&snapshot, &configured_settings(), &[], Duration::ZERO, 0)
        .expect("render");

    assert!(
        frame.color_count(AIRCRAFT, 310, 225, 36, 30) > 0,
        "east aircraft heading triangle"
    );
    assert!(
        frame.region_is_white(220, 195, 100, 90),
        "callsign tag is drawn toward the center"
    );
}

#[test]
fn enriched_tag_draws_callsign_route_compact_model_and_altitude_in_order() {
    let mut settings = configured_settings();
    settings.show_route = true;
    settings.show_expanded_model = true;
    let mut plane = aircraft(0.0, 0.0);
    plane.callsign = "DAL123".to_owned();
    plane.aircraft_type = "B738".to_owned();
    plane.altitude = "12000 ft".to_owned();
    let enrichment = AircraftEnrichment {
        route: Some("JFK→LAX".to_owned()),
        model: Some("737-800".to_owned()),
    };

    let actual = render_tag(&settings, plane.clone(), enrichment);
    let expected = expected_tag(
        &settings,
        &plane,
        &[
            ("DAL123", LABEL),
            ("JFK→LAX", LABEL),
            ("737-800", TAG_TYPE),
            ("12000 ft", TAG_ALTITUDE),
        ],
    );

    assert_eq!(actual.pixels(), expected.pixels());
}

#[test]
fn hidden_callsign_places_the_known_route_on_the_first_tag_line() {
    let mut settings = configured_settings();
    settings.show_callsign = false;
    settings.show_route = true;
    settings.show_expanded_model = true;
    let mut plane = aircraft(0.0, 0.0);
    plane.callsign = "HIDDEN".to_owned();
    plane.aircraft_type = "B738".to_owned();
    plane.altitude = "12000 ft".to_owned();
    let enrichment = AircraftEnrichment {
        route: Some("JFK→LAX".to_owned()),
        model: Some("737-800".to_owned()),
    };

    let actual = render_tag(&settings, plane.clone(), enrichment);
    let expected = expected_tag(
        &settings,
        &plane,
        &[
            ("JFK→LAX", LABEL),
            ("737-800", TAG_TYPE),
            ("12000 ft", TAG_ALTITUDE),
        ],
    );

    assert_eq!(actual.pixels(), expected.pixels());
}

#[test]
fn expanded_model_miss_falls_back_to_the_current_short_type() {
    let mut settings = configured_settings();
    settings.show_callsign = false;
    settings.show_expanded_model = true;
    let mut plane = aircraft(0.0, 0.0);
    plane.aircraft_type = "B738".to_owned();
    plane.altitude.clear();

    let actual = render_tag(
        &settings,
        plane.clone(),
        AircraftEnrichment {
            route: None,
            model: None,
        },
    );
    let expected = expected_tag(&settings, &plane, &[("B738", TAG_TYPE)]);

    assert_eq!(actual.pixels(), expected.pixels());
}

#[test]
fn route_miss_does_not_reserve_a_blank_tag_line() {
    let mut settings = configured_settings();
    settings.show_callsign = false;
    settings.show_route = true;
    settings.show_expanded_model = true;
    let mut plane = aircraft(0.0, 0.0);
    plane.aircraft_type = "B738".to_owned();
    plane.altitude = "12000 ft".to_owned();

    let actual = render_tag(
        &settings,
        plane.clone(),
        AircraftEnrichment {
            route: None,
            model: Some("737-800".to_owned()),
        },
    );
    let expected = expected_tag(
        &settings,
        &plane,
        &[("737-800", TAG_TYPE), ("12000 ft", TAG_ALTITUDE)],
    );

    assert_eq!(actual.pixels(), expected.pixels());
}

#[test]
fn aircraft_tag_block_uses_measured_sizes_at_80_100_and_130_percent() {
    let mut plane = aircraft(0.0, 0.0);
    plane.callsign.clear();
    plane.aircraft_type = "737-800".to_owned();
    plane.altitude.clear();
    let mut bounds = Vec::new();

    for scale in [80, 100, 130] {
        let mut settings = configured_settings();
        settings.show_callsign = false;
        settings.radar_text_scale_percent = scale;
        let actual = render_tag(&settings, plane.clone(), AircraftEnrichment::default());
        let expected = expected_tag(&settings, &plane, &[("737-800", TAG_TYPE)]);
        assert_eq!(actual.pixels(), expected.pixels(), "text scale {scale}%");
        bounds.push(color_bounds(&actual, TAG_TYPE));
    }

    assert!(bounds[0].2 < bounds[1].2 && bounds[1].2 < bounds[2].2);
    assert!(bounds[0].3 < bounds[1].3 && bounds[1].3 < bounds[2].3);
}

#[test]
fn overlong_expanded_model_is_ellipsized_to_its_actual_side_width() {
    let settings = RadarSettings {
        show_callsign: false,
        show_expanded_model: true,
        ..configured_settings()
    };
    let font = Font::from_bytes(
        include_bytes!("../src/assets/DejaVuSans-Bold.ttf") as &[u8],
        FontSettings::default(),
    )
    .expect("embedded DejaVu font");
    let text = TextRasterizer::new(&font);
    let expected_model = "HHHHHHHHHHHH…";
    let wider_model = "HHHHHHHHHHHHH…";
    let expected_width = text.measure(expected_model, AIRCRAFT_TAG_CAP_HEIGHT).0;
    let wider_width = text.measure(wider_model, AIRCRAFT_TAG_CAP_HEIGHT).0;
    let available_width = (expected_width + wider_width) / 2.0;
    let aircraft_x = available_width
        + 1.0
        + AIRCRAFT_NOSE_LENGTH
        + AIRCRAFT_TAIL_HALF_WIDTH
        + AIRCRAFT_LABEL_GAP;
    assert!((CENTER.0..CENTER.0 + AIRCRAFT_SAFE_RADIUS).contains(&aircraft_x));
    let preset = range_preset(settings.range_index).expect("configured range");
    let east_km = f64::from(aircraft_x - CENTER.0) * preset.outer_km / f64::from(GRID_OUTER_RADIUS);
    let mut plane = aircraft(east_km, 0.0);
    plane.callsign.clear();
    plane.aircraft_type = "B738".to_owned();
    plane.altitude.clear();
    let actual = render_tag(
        &settings,
        plane.clone(),
        AircraftEnrichment {
            route: None,
            model: Some("H".repeat(1_024)),
        },
    );
    let expected = expected_tag(&settings, &plane, &[(expected_model, TAG_TYPE)]);

    assert_eq!(actual.pixels(), expected.pixels());
    let (left, _, width, _) = color_bounds(&actual, TAG_TYPE);
    assert!(left >= 1);
    assert!(left + width < SIZE);
}

#[test]
fn text_scale_changes_all_existing_radar_text_without_scaling_geometry() {
    let airport = runway_airport();
    let mut static_frames = Vec::new();
    let mut tag_frames = Vec::new();
    for scale in [80, 100, 130] {
        let mut settings = configured_settings();
        settings.radar_text_scale_percent = scale;
        static_frames.push(
            test_renderer()
                .render(
                    &empty_snapshot(Some(Duration::ZERO)),
                    &settings,
                    std::slice::from_ref(&airport),
                    Duration::from_secs(30),
                    0,
                )
                .expect("scaled static render"),
        );
        let mut plane = aircraft(5.0, 120.0);
        plane.callsign.clear();
        plane.altitude.clear();
        settings.show_callsign = false;
        tag_frames.push(render_tag(&settings, plane, AircraftEnrichment::default()));
    }

    let cardinal_counts = static_frames
        .iter()
        .map(|frame| frame.color_count(LABEL, 210, 0, 60, 50))
        .collect::<Vec<_>>();
    let runway_label_counts = static_frames
        .iter()
        .map(|frame| frame.color_count(planeradar::render::theme::RUNWAY_LABEL, 0, 0, SIZE, SIZE))
        .collect::<Vec<_>>();
    let stale_counts = static_frames
        .iter()
        .map(|frame| frame.color_count(STALE, 130, 30, 220, 60))
        .collect::<Vec<_>>();
    let tag_counts = tag_frames
        .iter()
        .map(|frame| frame.color_count(TAG_TYPE, 0, 0, SIZE, SIZE))
        .collect::<Vec<_>>();
    for counts in [
        cardinal_counts,
        runway_label_counts,
        stale_counts,
        tag_counts,
    ] {
        assert!(counts[0] < counts[1] && counts[1] < counts[2], "{counts:?}");
    }
    for pair in static_frames.windows(2) {
        assert_ne!(
            region_pixels(&pair[0], 370, 220, 80, 40),
            region_pixels(&pair[1], 370, 220, 80, 40),
            "range text must change with scale"
        );
        assert_eq!(pair[0].pixel(240, 240), pair[1].pixel(240, 240));
        assert_eq!(pair[0].pixel(454, 240), pair[1].pixel(454, 240));
        assert_eq!(pair[0].pixel(230, 240), pair[1].pixel(230, 240));
    }
    for pair in tag_frames.windows(2) {
        assert_eq!(
            pair[0].color_count(AIRCRAFT, 0, 0, SIZE, SIZE),
            pair[1].color_count(AIRCRAFT, 0, 0, SIZE, SIZE)
        );
        assert_eq!(
            pair[0].color_count(TRACK, 0, 0, SIZE, SIZE),
            pair[1].color_count(TRACK, 0, 0, SIZE, SIZE)
        );
    }
}

#[test]
fn transparent_aircraft_tag_preserves_static_pixels_and_draws_text_last() {
    let settings = configured_settings();
    let plane = aircraft(11.6, 0.0);
    let snapshot = RadarSnapshot {
        aircraft: Arc::from([plane.clone()]),
        enrichment: Arc::new(HashMap::new()),
        environment: None,
        fetched_at: Some(Duration::ZERO),
        last_error_at: None,
    };
    let mut renderer = test_renderer();
    let empty = renderer
        .render(
            &empty_snapshot(Some(Duration::ZERO)),
            &settings,
            &[],
            Duration::ZERO,
            0,
        )
        .expect("empty render");
    let traffic = renderer
        .render(&snapshot, &settings, &[], Duration::ZERO, 0)
        .expect("traffic render");

    let location = settings.location.as_ref().expect("configured location");
    let preset = range_preset(settings.range_index).expect("configured range");
    let east = offset_km(location, plane.latitude, plane.longitude).east;
    let aircraft_x = CENTER.0 + (east * f64::from(GRID_OUTER_RADIUS) / preset.outer_km) as f32;
    let tag_anchor_x =
        aircraft_x - AIRCRAFT_NOSE_LENGTH - AIRCRAFT_TAIL_HALF_WIDTH - AIRCRAFT_LABEL_GAP;
    let overlap_width = 24;
    let overlap_left = tag_anchor_x.floor() as u32 - overlap_width;
    let overlap_top = (CENTER.1 - AIRCRAFT_TAG_CAP_HEIGHT / 2.0).floor() as u32;
    let overlap_height = AIRCRAFT_TAG_CAP_HEIGHT.ceil() as u32;
    let empty_grid = empty.color_count(
        GRID,
        overlap_left,
        overlap_top,
        overlap_width,
        overlap_height,
    );
    let traffic_grid = traffic.color_count(
        GRID,
        overlap_left,
        overlap_top,
        overlap_width,
        overlap_height,
    );

    assert!(
        empty_grid > 0,
        "overlap precondition needs range-label pixels"
    );
    assert!(
        traffic_grid > 0,
        "static range pixels must remain visible through transparent glyph gaps"
    );
    assert!(
        traffic_grid < empty_grid,
        "later aircraft glyphs must replace directly overlapped static pixels"
    );
    assert!(
        traffic.color_count(
            TAG_TYPE,
            overlap_left,
            overlap_top,
            overlap_width,
            overlap_height
        ) > 0,
        "aircraft type glyphs must remain visible"
    );
}

#[test]
fn range_label_has_a_one_pixel_shape_only_outline() {
    let settings = configured_settings();
    let frame = test_renderer()
        .render(
            &empty_snapshot(Some(Duration::ZERO)),
            &settings,
            &[],
            Duration::ZERO,
            0,
        )
        .expect("render");
    let mask = range_glyph_mask(&settings);
    let center_y = CENTER.1 as u32;
    let scope_x = 380..448;
    let black_pixels = scope_x
        .clone()
        .filter(|&x| frame.pixel(x, center_y) == BACKGROUND)
        .collect::<Vec<_>>();

    assert!(
        !black_pixels.is_empty(),
        "the black contour must separate the range label from the east scope line"
    );
    assert!(
        black_pixels
            .iter()
            .all(|&x| mask_within_one_pixel(&mask, x, center_y)),
        "the range outline must not extend beyond one pixel of glyph coverage"
    );
    assert!(
        black_pixels
            .iter()
            .any(|&x| !mask_covers(&mask, x, center_y)),
        "the black contour must extend outside the green glyph coverage"
    );
    for x in scope_x {
        if !mask_within_one_pixel(&mask, x, center_y) {
            assert_ne!(
                frame.pixel(x, center_y),
                BACKGROUND,
                "a black backplate appeared outside the one-pixel glyph contour at x={x}"
            );
        }
    }

    let mut opaque_glyph_pixels = 0;
    for y in 220..260 {
        for x in 370..450 {
            if mask.pixel(x, y) == LABEL {
                opaque_glyph_pixels += 1;
                assert_eq!(
                    frame.pixel(x, y),
                    GRID,
                    "the green range fill moved or changed at ({x}, {y})"
                );
            }
        }
    }
    assert!(
        opaque_glyph_pixels > 0,
        "range mask must contain glyph pixels"
    );
}

#[test]
fn whitespace_runway_label_does_not_mask_radar_geometry() {
    let mut empty_ident = runway_airport();
    empty_ident.ident.clear();
    let mut whitespace_ident = empty_ident.clone();
    whitespace_ident.ident = "                        ".to_owned();
    let settings = configured_settings();
    let snapshot = empty_snapshot(Some(Duration::ZERO));

    let empty_frame = test_renderer()
        .render(
            &snapshot,
            &settings,
            std::slice::from_ref(&empty_ident),
            Duration::ZERO,
            0,
        )
        .expect("empty-label render");
    let whitespace_frame = test_renderer()
        .render(
            &snapshot,
            &settings,
            std::slice::from_ref(&whitespace_ident),
            Duration::ZERO,
            0,
        )
        .expect("whitespace-label render");

    assert_eq!(
        whitespace_frame.pixels(),
        empty_frame.pixels(),
        "a label with no glyph coverage must not paint a background plate"
    );
}

#[test]
fn traffic_outside_the_aircraft_safe_ring_is_a_red_dot_on_the_238_pixel_rim() {
    let snapshot = RadarSnapshot {
        aircraft: Arc::from([aircraft(13.0, 0.0)]),
        enrichment: Arc::new(HashMap::new()),
        environment: None,
        fetched_at: Some(Duration::ZERO),
        last_error_at: None,
    };
    let mut renderer = test_renderer();
    let frame = renderer
        .render(&snapshot, &configured_settings(), &[], Duration::ZERO, 0)
        .expect("render");

    assert!(frame.color_count(AIRCRAFT, 468, 230, 12, 20) > 0);
    assert_eq!(
        frame.color_count(AIRCRAFT, 410, 220, 45, 40),
        0,
        "outer traffic must not leave an aircraft triangle near the grid ring"
    );
}

#[test]
fn runway_toggle_removes_lines_and_labels_from_the_cached_background() {
    let airport = runway_airport();
    let mut renderer = test_renderer();
    let enabled = renderer
        .render(
            &empty_snapshot(Some(Duration::ZERO)),
            &configured_settings(),
            std::slice::from_ref(&airport),
            Duration::ZERO,
            0,
        )
        .expect("render with runways");
    let mut disabled_settings = configured_settings();
    disabled_settings.show_runways = false;
    let disabled = renderer
        .render(
            &empty_snapshot(Some(Duration::ZERO)),
            &disabled_settings,
            &[airport],
            Duration::ZERO,
            0,
        )
        .expect("render without runways");

    assert!(enabled.color_count(RUNWAY, 195, 232, 90, 16) > 0);
    assert_eq!(disabled.color_count(RUNWAY, 0, 0, SIZE, SIZE), 0);
    assert_ne!(enabled.pixels(), disabled.pixels());
}

#[test]
fn speed_vector_is_clipped_to_the_214_pixel_grid_ring() {
    let snapshot = RadarSnapshot {
        aircraft: Arc::from([aircraft(11.6, 2_000.0)]),
        enrichment: Arc::new(HashMap::new()),
        environment: None,
        fetched_at: Some(Duration::ZERO),
        last_error_at: None,
    };
    let mut renderer = test_renderer();
    let frame = renderer
        .render(&snapshot, &configured_settings(), &[], Duration::ZERO, 0)
        .expect("render");

    assert!(frame.color_count(TRACK, 440, 236, 16, 9) > 0);
    assert_eq!(
        frame.color_count(TRACK, 455, 220, 25, 40),
        0,
        "track vector must not cross outside the grid disc"
    );
}

#[test]
fn stale_notice_appears_at_thirty_seconds_but_not_before_or_on_clock_underflow() {
    let mut renderer = test_renderer();
    let settings = configured_settings();
    let before = renderer
        .render(
            &empty_snapshot(Some(Duration::from_secs(5))),
            &settings,
            &[],
            Duration::from_secs(34),
            0,
        )
        .expect("fresh render");
    let threshold = renderer
        .render(
            &empty_snapshot(Some(Duration::from_secs(5))),
            &settings,
            &[],
            Duration::from_secs(35),
            0,
        )
        .expect("stale render");
    let underflow = renderer
        .render(
            &empty_snapshot(Some(Duration::from_secs(35))),
            &settings,
            &[],
            Duration::from_secs(5),
            0,
        )
        .expect("clock underflow render");

    assert_eq!(before.color_count(STALE, 130, 38, 220, 50), 0);
    assert!(threshold.color_count(STALE, 130, 38, 220, 50) > 0);
    assert_eq!(underflow.color_count(STALE, 130, 38, 220, 50), 0);
}

#[test]
fn invalid_or_unconfigured_settings_return_errors_without_rendering() {
    let mut renderer = test_renderer();
    let snapshot = empty_snapshot(None);
    let mut cases = vec![RadarSettings::default()];
    let mut bad_schema = configured_settings();
    bad_schema.schema_version = 3;
    cases.push(bad_schema);
    let mut bad_range = configured_settings();
    bad_range.range_index = u8::MAX;
    cases.push(bad_range);
    let mut bad_location = configured_settings();
    bad_location.location.as_mut().expect("location").latitude = f64::NAN;
    cases.push(bad_location);

    for settings in cases {
        assert!(
            renderer
                .render(&snapshot, &settings, &[], Duration::ZERO, 0)
                .is_err()
        );
    }
}

#[test]
fn empty_and_unrenderable_tag_strings_do_not_panic() {
    let mut plane = aircraft(3.0, 0.0);
    plane.callsign.clear();
    plane.aircraft_type = "\0\n\u{10ffff}".to_owned();
    plane.altitude = "🛩".repeat(1_024);
    let snapshot = RadarSnapshot {
        aircraft: Arc::from([plane]),
        enrichment: Arc::new(HashMap::new()),
        environment: None,
        fetched_at: Some(Duration::ZERO),
        last_error_at: None,
    };

    test_renderer()
        .render(&snapshot, &configured_settings(), &[], Duration::ZERO, 0)
        .expect("malformed display strings are safely clipped");
}

#[test]
fn background_key_uses_exact_float_bits_and_every_static_setting() {
    let mut base_settings = configured_settings();
    base_settings.location.as_mut().expect("location").latitude = 0.0;
    let base = BackgroundKey::from_settings(&base_settings).expect("background key");

    let mut changed = base_settings.clone();
    changed.location.as_mut().expect("location").latitude = -0.0;
    let negative_zero = BackgroundKey::from_settings(&changed).expect("negative-zero key");
    assert_ne!(base, negative_zero);

    changed = base_settings.clone();
    changed.location.as_mut().expect("location").longitude = -74.0;
    assert_ne!(
        base,
        BackgroundKey::from_settings(&changed).expect("longitude key")
    );
    changed = base_settings.clone();
    changed.range_index = 2;
    assert_ne!(
        base,
        BackgroundKey::from_settings(&changed).expect("range key")
    );
    changed = base_settings.clone();
    changed.units = Units::Miles;
    assert_ne!(
        base,
        BackgroundKey::from_settings(&changed).expect("units key")
    );
    changed = base_settings.clone();
    changed.show_runways = false;
    assert_ne!(
        base,
        BackgroundKey::from_settings(&changed).expect("runway key")
    );
    changed = base_settings.clone();
    changed.radar_text_scale_percent = 110;
    assert_ne!(
        base,
        BackgroundKey::from_settings(&changed).expect("text-scale key")
    );

    for mutate_dynamic_setting in [
        |settings: &mut RadarSettings| settings.show_callsign = false,
        |settings: &mut RadarSettings| settings.show_route = true,
        |settings: &mut RadarSettings| settings.show_expanded_model = true,
        |settings: &mut RadarSettings| settings.footer.show_condition = true,
        |settings: &mut RadarSettings| settings.minimum_altitude_feet = Some(5_000),
        |settings: &mut RadarSettings| settings.maximum_altitude_feet = Some(25_000),
    ] {
        changed = base_settings.clone();
        mutate_dynamic_setting(&mut changed);
        assert_eq!(
            base,
            BackgroundKey::from_settings(&changed).expect("dynamic setting key")
        );
    }
}

#[test]
fn embedded_font_sidecar_contains_the_complete_upstream_notices() {
    for distinctive_notice in [
        "Bitstream Vera Fonts Copyright",
        "DejaVu changes are in public domain.",
        "Arev Fonts Copyright",
        "Copyright (c) 2006 by Tavmjong Bah. All Rights Reserved.",
        "the words \"Tavmjong Bah\" or the word \"Arev\"",
    ] {
        assert!(
            EMBEDDED_FONT_LICENSE.contains(distinctive_notice),
            "font sidecar must contain {distinctive_notice:?}"
        );
    }

    let font = std::path::Path::new(EMBEDDED_FONT_PATH);
    let license = std::path::Path::new(EMBEDDED_FONT_LICENSE_PATH);
    assert_eq!(font.parent(), license.parent());
    assert!(font.is_file());
    assert!(license.is_file());
}

#[test]
fn frame_rejects_wrong_lengths_overflow_and_zero_dimensions() {
    assert!(matches!(
        Frame::new(2, 2, vec![0; 15]),
        Err(RenderError::InvalidFrameLength { .. })
    ));
    assert!(matches!(
        Frame::new(u32::MAX, u32::MAX, Vec::new()),
        Err(RenderError::DimensionsOverflow)
    ));
    assert!(matches!(
        Frame::new(0, 480, Vec::new()),
        Err(RenderError::InvalidDimensions { .. })
    ));
}

#[test]
fn png_failure_does_not_replace_or_remove_the_destination() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let destination = directory.path().join("radar.png");
    fs::create_dir(&destination).expect("directory destination");
    fs::write(destination.join("keep"), b"safe").expect("sentinel");
    let frame = Frame::new(1, 1, vec![4, 10, 28, 255]).expect("frame");

    assert!(frame.save_png(&destination).is_err());
    assert!(destination.is_dir());
    assert_eq!(
        fs::read(destination.join("keep")).expect("sentinel after failure"),
        b"safe"
    );
}

#[test]
fn empty_fixture_matches_golden() {
    let fixture = planeradar::render::radar::fixture_empty().expect("empty fixture");
    fixture.assert_matches_golden("radar-empty");
}

#[test]
fn traffic_fixture_matches_golden() {
    let fixture = planeradar::render::radar::fixture_traffic().expect("traffic fixture");
    fixture.assert_matches_golden("radar-traffic");
}

#[test]
fn stale_fixture_matches_golden() {
    let fixture = planeradar::render::radar::fixture_stale().expect("stale fixture");
    fixture.assert_matches_golden("radar-stale");
}

#[test]
fn enriched_fixture_matches_golden() {
    let fixture = planeradar::render::radar::fixture_enriched().expect("enriched fixture");
    fixture.assert_matches_golden("radar-enriched");
}

#[test]
fn footer_fixture_matches_golden() {
    let fixture = planeradar::render::radar::fixture_footer().expect("footer fixture");
    fixture.assert_matches_golden("radar-footer");
}

#[test]
fn large_stale_footer_fixture_matches_golden() {
    let fixture = planeradar::render::radar::fixture_footer_large_stale()
        .expect("large stale footer fixture");
    fixture.assert_matches_golden("radar-footer-large-stale");
}

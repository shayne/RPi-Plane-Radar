use std::cmp::Ordering;
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tiny_skia::{FillRule, LineCap, Paint, PathBuilder, Pixmap, Stroke, Transform};

use crate::display::{DisplayConfig, DisplayHandler, DisplayUpdate, InputEvent, run_display};
use crate::geometry::{offset_km, project_to_radar, rim_point};
use crate::model::{
    Aircraft, Airport, GeoPoint, Location, RadarSettings, RadarSnapshot, Runway,
    SETTINGS_SCHEMA_VERSION, Units,
};
use crate::range::{format_range_label, range_preset};
use crate::render::text::{HorizontalAnchor, TextRasterizer, TextStyle, VerticalAnchor};
use crate::render::theme;
use crate::render::{FontAsset, Frame, RenderError};

const STALE_AFTER: Duration = Duration::from_secs(30);
const MAX_AIRCRAFT: usize = 64;
const MAX_AIRPORT_LABELS: usize = 32;
const RANGE_LABEL_OUTLINE_OFFSETS: [(f32, f32); 8] = [
    (-1.0, -1.0),
    (0.0, -1.0),
    (1.0, -1.0),
    (-1.0, 0.0),
    (1.0, 0.0),
    (-1.0, 1.0),
    (0.0, 1.0),
    (1.0, 1.0),
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BackgroundKey {
    latitude_bits: u64,
    longitude_bits: u64,
    range_index: u8,
    units: Units,
    show_runways: bool,
}

impl BackgroundKey {
    pub fn from_settings(settings: &RadarSettings) -> Result<Self, RenderError> {
        validate_settings(settings)?;
        let location = settings
            .location
            .as_ref()
            .ok_or(RenderError::UnconfiguredLocation)?;
        Ok(Self {
            latitude_bits: location.latitude.to_bits(),
            longitude_bits: location.longitude.to_bits(),
            range_index: settings.range_index,
            units: settings.units,
            show_runways: settings.show_runways,
        })
    }
}

pub struct RadarRenderer {
    font: FontAsset,
    background: Option<(BackgroundKey, Vec<u8>)>,
}

impl RadarRenderer {
    pub fn new(font: FontAsset) -> Self {
        Self {
            font,
            background: None,
        }
    }

    pub fn render(
        &mut self,
        snapshot: &RadarSnapshot,
        settings: &RadarSettings,
        airports: &[Airport],
        now: Duration,
    ) -> Result<Frame, RenderError> {
        let key = BackgroundKey::from_settings(settings)?;
        if self
            .background
            .as_ref()
            .is_none_or(|(cached_key, _)| *cached_key != key)
        {
            let background = self.render_background(settings, airports)?;
            self.background = Some((key, background));
        }
        let background = self
            .background
            .as_ref()
            .map(|(_, pixels)| pixels)
            .ok_or(RenderError::InvalidSettings("background cache unavailable"))?;
        let mut pixmap =
            Pixmap::new(theme::SIZE, theme::SIZE).ok_or(RenderError::DimensionsOverflow)?;
        pixmap.data_mut().copy_from_slice(background);

        self.draw_aircraft(&mut pixmap, snapshot, settings)?;
        if snapshot
            .fetched_at
            .and_then(|fetched_at| now.checked_sub(fetched_at))
            .is_some_and(|age| age >= STALE_AFTER)
        {
            TextRasterizer::new(self.font.font()).draw(
                &mut pixmap,
                "DATA STALE",
                theme::CENTER.0,
                44.0,
                TextStyle {
                    cap_height: theme::STALE_CAP_HEIGHT,
                    color: theme::STALE,
                    horizontal: HorizontalAnchor::Center,
                    vertical: VerticalAnchor::Top,
                },
            );
        }

        Frame::new(theme::SIZE, theme::SIZE, pixmap.take())
    }

    fn render_background(
        &self,
        settings: &RadarSettings,
        airports: &[Airport],
    ) -> Result<Vec<u8>, RenderError> {
        let mut pixmap =
            Pixmap::new(theme::SIZE, theme::SIZE).ok_or(RenderError::DimensionsOverflow)?;
        pixmap.fill(color(theme::BACKGROUND));

        for ring in 1..=theme::GRID_RING_COUNT {
            let radius = theme::GRID_OUTER_RADIUS * ring as f32 / theme::GRID_RING_COUNT as f32;
            draw_circle_stroke(
                &mut pixmap,
                theme::CENTER.0,
                theme::CENTER.1,
                radius,
                theme::GRID_STROKE_WIDTH,
                theme::GRID,
            );
        }
        draw_line(
            &mut pixmap,
            theme::CENTER.0,
            theme::CENTER.1 - theme::GRID_OUTER_RADIUS,
            theme::CENTER.0,
            theme::CENTER.1 + theme::GRID_OUTER_RADIUS,
            theme::GRID_STROKE_WIDTH,
            theme::GRID,
        );
        draw_line(
            &mut pixmap,
            theme::CENTER.0 - theme::GRID_OUTER_RADIUS,
            theme::CENTER.1,
            theme::CENTER.0 + theme::GRID_OUTER_RADIUS,
            theme::CENTER.1,
            theme::GRID_STROKE_WIDTH,
            theme::GRID,
        );

        if settings.show_runways {
            self.draw_runways(&mut pixmap, settings, airports)?;
        }

        draw_filled_circle(
            &mut pixmap,
            theme::CENTER.0,
            theme::CENTER.1,
            theme::CENTER_DOT_RADIUS,
            theme::LABEL,
        );
        self.draw_grid_labels(&mut pixmap, settings)?;
        Ok(pixmap.take())
    }

    fn draw_grid_labels(
        &self,
        pixmap: &mut Pixmap,
        settings: &RadarSettings,
    ) -> Result<(), RenderError> {
        let text = TextRasterizer::new(self.font.font());
        let cardinal = |horizontal, vertical| TextStyle {
            cap_height: theme::CARDINAL_CAP_HEIGHT,
            color: theme::LABEL,
            horizontal,
            vertical,
        };
        text.draw(
            pixmap,
            "N",
            theme::CENTER.0,
            0.0,
            cardinal(HorizontalAnchor::Center, VerticalAnchor::Top),
        );
        text.draw(
            pixmap,
            "S",
            theme::CENTER.0,
            theme::SIZE as f32,
            cardinal(HorizontalAnchor::Center, VerticalAnchor::Bottom),
        );
        text.draw(
            pixmap,
            "W",
            0.0,
            theme::CENTER.1,
            cardinal(HorizontalAnchor::Left, VerticalAnchor::Middle),
        );
        text.draw(
            pixmap,
            "E",
            theme::SIZE as f32,
            theme::CENTER.1,
            cardinal(HorizontalAnchor::Right, VerticalAnchor::Middle),
        );

        let preset = range_preset(settings.range_index)?;
        let range_label = format_range_label(preset, settings.units);
        let anchor_x = theme::CENTER.0 + theme::GRID_OUTER_RADIUS - 12.0;
        let style = TextStyle {
            cap_height: theme::SCALE_CAP_HEIGHT,
            color: theme::GRID,
            horizontal: HorizontalAnchor::Right,
            vertical: VerticalAnchor::Middle,
        };
        for (offset_x, offset_y) in RANGE_LABEL_OUTLINE_OFFSETS {
            text.draw(
                pixmap,
                &range_label,
                anchor_x + offset_x,
                theme::CENTER.1 + offset_y,
                TextStyle {
                    color: theme::BACKGROUND,
                    ..style
                },
            );
        }
        text.draw(pixmap, &range_label, anchor_x, theme::CENTER.1, style);
        Ok(())
    }

    fn draw_runways(
        &self,
        pixmap: &mut Pixmap,
        settings: &RadarSettings,
        airports: &[Airport],
    ) -> Result<(), RenderError> {
        let location = settings
            .location
            .as_ref()
            .ok_or(RenderError::UnconfiguredLocation)?;
        let preset = range_preset(settings.range_index)?;
        let fetch_radius_km =
            preset.outer_km * theme::RIM_RADIUS / f64::from(theme::GRID_OUTER_RADIUS);
        let pixels_per_kilometre = f64::from(theme::GRID_OUTER_RADIUS) / preset.outer_km;
        let mut labels = Vec::new();

        for airport in airports {
            if !valid_coordinate(airport.location.latitude, airport.location.longitude) {
                continue;
            }
            let airport_offset = offset_km(
                location,
                airport.location.latitude,
                airport.location.longitude,
            );
            if f64::hypot(airport_offset.east, airport_offset.north) > fetch_radius_km {
                continue;
            }
            let mut drew_runway = false;
            for runway in &airport.runways {
                let Some((x0, y0, x1, y1)) = runway_segment(location, runway, pixels_per_kilometre)
                else {
                    continue;
                };
                let Some((x0, y0, x1, y1)) =
                    clip_segment_to_disc(x0, y0, x1, y1, theme::GRID_OUTER_RADIUS)
                else {
                    continue;
                };
                draw_line(
                    pixmap,
                    x0,
                    y0,
                    x1,
                    y1,
                    theme::RUNWAY_STROKE_WIDTH,
                    theme::RUNWAY,
                );
                drew_runway = true;
            }
            if drew_runway && labels.len() < MAX_AIRPORT_LABELS {
                labels.push((airport, airport_offset.east, airport_offset.north));
            }
        }

        let text = TextRasterizer::new(self.font.font());
        for (airport, east, north) in labels {
            let mut x = f64::from(theme::CENTER.0) + east * pixels_per_kilometre;
            let mut y = f64::from(theme::CENTER.1) - north * pixels_per_kilometre;
            let distance = f64::hypot(
                x - f64::from(theme::CENTER.0),
                y - f64::from(theme::CENTER.1),
            );
            if distance > f64::from(theme::GRID_OUTER_RADIUS) {
                let scale = f64::from(theme::GRID_OUTER_RADIUS) / distance;
                x = f64::from(theme::CENTER.0) + (x - f64::from(theme::CENTER.0)) * scale;
                y = f64::from(theme::CENTER.1) + (y - f64::from(theme::CENTER.1)) * scale;
            }
            let outward_x = x - f64::from(theme::CENTER.0);
            let outward_y = y - f64::from(theme::CENTER.1);
            let outward_length = f64::hypot(outward_x, outward_y);
            let (label_x, label_y) = if outward_length < f64::EPSILON {
                (x, y - f64::from(theme::RUNWAY_LABEL_GAP))
            } else {
                (
                    x + outward_x / outward_length * f64::from(theme::RUNWAY_LABEL_GAP),
                    y + outward_y / outward_length * f64::from(theme::RUNWAY_LABEL_GAP),
                )
            };
            text.draw(
                pixmap,
                &airport.ident,
                label_x as f32,
                label_y as f32,
                TextStyle {
                    cap_height: theme::RUNWAY_LABEL_CAP_HEIGHT,
                    color: theme::RUNWAY_LABEL,
                    horizontal: HorizontalAnchor::Center,
                    vertical: VerticalAnchor::Bottom,
                },
            );
        }
        Ok(())
    }

    fn draw_aircraft(
        &self,
        pixmap: &mut Pixmap,
        snapshot: &RadarSnapshot,
        settings: &RadarSettings,
    ) -> Result<(), RenderError> {
        let location = settings
            .location
            .as_ref()
            .ok_or(RenderError::UnconfiguredLocation)?;
        let preset = range_preset(settings.range_index)?;
        let pixels_per_kilometre = f64::from(theme::GRID_OUTER_RADIUS) / preset.outer_km;
        let mut inside = Vec::new();
        let mut rim = Vec::new();

        for aircraft in snapshot.aircraft.iter().take(MAX_AIRCRAFT) {
            if !valid_coordinate(aircraft.latitude, aircraft.longitude) {
                continue;
            }
            let projection = project_to_radar(
                location,
                aircraft.latitude,
                aircraft.longitude,
                preset.outer_km,
                f64::from(theme::GRID_OUTER_RADIUS),
                f64::from(theme::AIRCRAFT_SAFE_RADIUS),
            )?;
            if projection.inside_ring {
                let x = f64::from(theme::CENTER.0) + projection.offset.east * pixels_per_kilometre;
                let y = f64::from(theme::CENTER.1) - projection.offset.north * pixels_per_kilometre;
                inside.push((
                    aircraft,
                    x as f32,
                    y as f32,
                    f64::hypot(projection.offset.east, projection.offset.north),
                ));
            } else {
                let (x, y) = rim_point(
                    projection.offset.east,
                    projection.offset.north,
                    theme::RIM_RADIUS,
                );
                if (x, y) != (theme::CENTER.0 as i32, theme::CENTER.1 as i32) {
                    rim.push((x as f32, y as f32));
                }
            }
        }
        inside.sort_by(|left, right| right.3.partial_cmp(&left.3).unwrap_or(Ordering::Equal));

        for (x, y) in rim {
            draw_filled_circle(pixmap, x, y, theme::RIM_DOT_RADIUS, theme::AIRCRAFT);
        }
        for (aircraft, x, y, _) in &inside {
            self.draw_vector(pixmap, aircraft, *x, *y);
            draw_heading_triangle(pixmap, aircraft.nose_degrees, *x, *y);
        }
        for (aircraft, x, y, _) in inside {
            self.draw_aircraft_tag(pixmap, aircraft, x, y);
        }
        Ok(())
    }

    fn draw_vector(&self, pixmap: &mut Pixmap, aircraft: &Aircraft, x: f32, y: f32) {
        let heading = finite_heading(aircraft.nose_degrees);
        let track = finite_heading(aircraft.track_degrees);
        let heading_radians = heading.to_radians();
        let start_x = x + heading_radians.sin() as f32 * theme::AIRCRAFT_NOSE_LENGTH;
        let start_y = y - heading_radians.cos() as f32 * theme::AIRCRAFT_NOSE_LENGTH;
        let mut length =
            if aircraft.ground_speed_knots.is_finite() && aircraft.ground_speed_knots > 0.0 {
                aircraft.ground_speed_knots * 1.852 * theme::TRACK_HORIZON_SECONDS / 3_600.0
                    * f64::from(theme::GRID_OUTER_RADIUS)
                    / theme::TRACK_REFERENCE_OUTER_KM
                    * theme::TRACK_LENGTH_SCALE
            } else {
                0.0
            };
        if length <= 0.0 {
            return;
        }
        length = length.max(f64::from(theme::TRACK_MIN_LENGTH));
        let track_radians = track.to_radians();
        let end_x = start_x + track_radians.sin() as f32 * length as f32;
        let end_y = start_y - track_radians.cos() as f32 * length as f32;
        let Some((_, _, clipped_x, clipped_y)) =
            clip_segment_to_disc(start_x, start_y, end_x, end_y, theme::GRID_OUTER_RADIUS)
        else {
            return;
        };
        if f32::hypot(clipped_x - start_x, clipped_y - start_y) < 0.5 {
            return;
        }
        draw_line(
            pixmap,
            start_x,
            start_y,
            clipped_x,
            clipped_y,
            theme::TRACK_STROKE_WIDTH,
            theme::TRACK,
        );
    }

    fn draw_aircraft_tag(&self, pixmap: &mut Pixmap, aircraft: &Aircraft, x: f32, y: f32) {
        let text = TextRasterizer::new(self.font.font());
        let lines = [
            (&aircraft.callsign, theme::LABEL),
            (&aircraft.aircraft_type, theme::TAG_TYPE),
            (&aircraft.altitude, theme::TAG_ALTITUDE),
        ];
        let widths = lines.map(|(line, _)| text.measure(line, theme::AIRCRAFT_TAG_CAP_HEIGHT).0);
        let line_height = text.measure("H", theme::AIRCRAFT_TAG_CAP_HEIGHT).1;
        let block_width = widths.into_iter().fold(0.0_f32, f32::max);
        let block_height = line_height * 3.0;
        let symbol_half = theme::AIRCRAFT_NOSE_LENGTH + theme::AIRCRAFT_TAIL_HALF_WIDTH;
        let on_right = x < theme::CENTER.0;
        let (anchor_x, horizontal) = if on_right {
            (
                (x + symbol_half + theme::AIRCRAFT_LABEL_GAP)
                    .min(theme::SIZE as f32 - block_width - 1.0),
                HorizontalAnchor::Left,
            )
        } else {
            (
                (x - symbol_half - theme::AIRCRAFT_LABEL_GAP).max(block_width + 1.0),
                HorizontalAnchor::Right,
            )
        };
        let top = (y - block_height / 2.0).clamp(1.0, theme::SIZE as f32 - block_height - 1.0);
        for (index, (line, color)) in lines.into_iter().enumerate() {
            text.draw(
                pixmap,
                line,
                anchor_x,
                top + line_height * index as f32,
                TextStyle {
                    cap_height: theme::AIRCRAFT_TAG_CAP_HEIGHT,
                    color,
                    horizontal,
                    vertical: VerticalAnchor::Top,
                },
            );
        }
    }
}

pub fn write_fixtures(output: &Path) -> Result<(), RenderError> {
    fs::create_dir_all(output)?;
    for (name, frame) in [
        ("radar-empty.png", fixture_empty()?),
        ("radar-traffic.png", fixture_traffic()?),
        ("radar-stale.png", fixture_stale()?),
    ] {
        frame.save_png(&output.join(name))?;
    }
    Ok(())
}

pub fn fixture_empty() -> Result<Frame, RenderError> {
    fixture_renderer()?.render(
        &fixture_snapshot(Vec::new(), Some(Duration::ZERO)),
        &fixture_settings(),
        &[],
        Duration::ZERO,
    )
}

pub fn fixture_traffic() -> Result<Frame, RenderError> {
    let aircraft = vec![
        Aircraft {
            hex: "a00001".to_owned(),
            flight_callsign: "RADAR7".to_owned(),
            latitude: fixture_point(5.0, -3.0).latitude,
            longitude: fixture_point(5.0, -3.0).longitude,
            nose_degrees: 45.0,
            track_degrees: 75.0,
            ground_speed_knots: 280.0,
            callsign: "RADAR7".to_owned(),
            aircraft_type: "A320".to_owned(),
            altitude_feet: Some(12_000),
            altitude: "12000".to_owned(),
        },
        Aircraft {
            hex: "a00002".to_owned(),
            flight_callsign: "RIM".to_owned(),
            latitude: fixture_point(15.0, 15.0).latitude,
            longitude: fixture_point(15.0, 15.0).longitude,
            nose_degrees: 270.0,
            track_degrees: 270.0,
            ground_speed_knots: 400.0,
            callsign: "RIM".to_owned(),
            aircraft_type: String::new(),
            altitude_feet: None,
            altitude: String::new(),
        },
    ];
    fixture_renderer()?.render(
        &fixture_snapshot(aircraft, Some(Duration::ZERO)),
        &fixture_settings(),
        &[fixture_airport()],
        Duration::from_secs(5),
    )
}

pub fn fixture_stale() -> Result<Frame, RenderError> {
    fixture_renderer()?.render(
        &fixture_snapshot(Vec::new(), Some(Duration::ZERO)),
        &fixture_settings(),
        &[],
        Duration::from_secs(30),
    )
}

pub fn run_radar_demo(seconds: u64) -> Result<(), Box<dyn std::error::Error>> {
    let frame = fixture_traffic()?;
    let mut handler = RadarDemoHandler {
        frame: Some(frame.pixels().to_vec()),
        started: Instant::now(),
        duration: Duration::from_secs(seconds),
    };
    run_display(DisplayConfig::default(), &mut handler)?;
    Ok(())
}

struct RadarDemoHandler {
    frame: Option<Vec<u8>>,
    started: Instant,
    duration: Duration,
}

impl DisplayHandler for RadarDemoHandler {
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

fn fixture_renderer() -> Result<RadarRenderer, RenderError> {
    Ok(RadarRenderer::new(FontAsset::embedded()?))
}

fn fixture_settings() -> RadarSettings {
    RadarSettings {
        location: Some(Location {
            latitude: 40.0,
            longitude: -75.0,
            label: "Fixture".to_owned(),
        }),
        units: Units::Kilometres,
        show_runways: true,
        range_index: 1,
        ..RadarSettings::default()
    }
}

fn fixture_snapshot(aircraft: Vec<Aircraft>, fetched_at: Option<Duration>) -> RadarSnapshot {
    RadarSnapshot {
        aircraft: Arc::from(aircraft),
        enrichment: Arc::new(HashMap::new()),
        environment: None,
        fetched_at,
        last_error_at: None,
    }
}

fn fixture_east_longitude(kilometres: f64) -> f64 {
    -75.0 + kilometres / (6_371.008_8 * 40.0_f64.to_radians().cos()) * 180.0 / std::f64::consts::PI
}

fn fixture_point(east_km: f64, north_km: f64) -> GeoPoint {
    GeoPoint {
        latitude: 40.0 + north_km / 6_371.008_8 * 180.0 / std::f64::consts::PI,
        longitude: fixture_east_longitude(east_km),
    }
}

fn fixture_airport() -> Airport {
    Airport {
        ident: "KFIX".to_owned(),
        location: fixture_point(-4.0, 4.0),
        runways: vec![Runway {
            low_end: fixture_point(-6.0, 3.0),
            high_end: fixture_point(-2.0, 5.0),
        }],
    }
}

fn validate_settings(settings: &RadarSettings) -> Result<(), RenderError> {
    if settings.schema_version != SETTINGS_SCHEMA_VERSION {
        return Err(RenderError::InvalidSettings("unsupported schema version"));
    }
    let location = settings
        .location
        .as_ref()
        .ok_or(RenderError::UnconfiguredLocation)?;
    if !valid_coordinate(location.latitude, location.longitude) {
        return Err(RenderError::InvalidSettings(
            "location is outside valid bounds",
        ));
    }
    range_preset(settings.range_index)?;
    Ok(())
}

fn valid_coordinate(latitude: f64, longitude: f64) -> bool {
    latitude.is_finite()
        && longitude.is_finite()
        && (-90.0..=90.0).contains(&latitude)
        && (-180.0..=180.0).contains(&longitude)
}

fn runway_segment(
    origin: &Location,
    runway: &Runway,
    pixels_per_kilometre: f64,
) -> Option<(f32, f32, f32, f32)> {
    if !valid_coordinate(runway.low_end.latitude, runway.low_end.longitude)
        || !valid_coordinate(runway.high_end.latitude, runway.high_end.longitude)
    {
        return None;
    }
    let low = offset_km(origin, runway.low_end.latitude, runway.low_end.longitude);
    let high = offset_km(origin, runway.high_end.latitude, runway.high_end.longitude);
    Some((
        (f64::from(theme::CENTER.0) + low.east * pixels_per_kilometre) as f32,
        (f64::from(theme::CENTER.1) - low.north * pixels_per_kilometre) as f32,
        (f64::from(theme::CENTER.0) + high.east * pixels_per_kilometre) as f32,
        (f64::from(theme::CENTER.1) - high.north * pixels_per_kilometre) as f32,
    ))
}

fn clip_segment_to_disc(
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
    radius: f32,
) -> Option<(f32, f32, f32, f32)> {
    let dx = x1 - x0;
    let dy = y1 - y0;
    let fx = x0 - theme::CENTER.0;
    let fy = y0 - theme::CENTER.1;
    let a = dx * dx + dy * dy;
    if !a.is_finite() || a <= f32::EPSILON {
        return (fx * fx + fy * fy <= radius * radius).then_some((x0, y0, x1, y1));
    }
    let b = 2.0 * (fx * dx + fy * dy);
    let c = fx * fx + fy * fy - radius * radius;
    let discriminant = b * b - 4.0 * a * c;
    let start_inside = c <= 0.0;
    let end_dx = x1 - theme::CENTER.0;
    let end_dy = y1 - theme::CENTER.1;
    let end_inside = end_dx * end_dx + end_dy * end_dy <= radius * radius;
    if discriminant < 0.0 {
        return (start_inside && end_inside).then_some((x0, y0, x1, y1));
    }
    let root = discriminant.sqrt();
    let first = (-b - root) / (2.0 * a);
    let second = (-b + root) / (2.0 * a);
    let enter = first.min(second);
    let leave = first.max(second);
    let start_t = if start_inside { 0.0 } else { enter.max(0.0) };
    let end_t = if end_inside { 1.0 } else { leave.min(1.0) };
    if start_t > end_t || end_t < 0.0 || start_t > 1.0 {
        return None;
    }
    Some((
        x0 + dx * start_t,
        y0 + dy * start_t,
        x0 + dx * end_t,
        y0 + dy * end_t,
    ))
}

fn finite_heading(heading: f64) -> f64 {
    if heading.is_finite() {
        heading.rem_euclid(360.0)
    } else {
        0.0
    }
}

fn draw_heading_triangle(pixmap: &mut Pixmap, heading_degrees: f64, x: f32, y: f32) {
    let radians = finite_heading(heading_degrees).to_radians();
    let sin = radians.sin() as f32;
    let cos = radians.cos() as f32;
    let tip_x = x + sin * theme::AIRCRAFT_NOSE_LENGTH;
    let tip_y = y - cos * theme::AIRCRAFT_NOSE_LENGTH;
    let base_x = x - sin * theme::AIRCRAFT_TAIL_LENGTH;
    let base_y = y + cos * theme::AIRCRAFT_TAIL_LENGTH;
    let wing_x = cos * theme::AIRCRAFT_TAIL_HALF_WIDTH;
    let wing_y = sin * theme::AIRCRAFT_TAIL_HALF_WIDTH;
    let mut path = PathBuilder::new();
    path.move_to(tip_x, tip_y);
    path.line_to(base_x + wing_x, base_y + wing_y);
    path.line_to(base_x - wing_x, base_y - wing_y);
    path.close();
    if let Some(path) = path.finish() {
        let paint = paint(theme::AIRCRAFT);
        pixmap.fill_path(
            &path,
            &paint,
            FillRule::Winding,
            Transform::identity(),
            None,
        );
    }
}

fn draw_circle_stroke(pixmap: &mut Pixmap, x: f32, y: f32, radius: f32, width: f32, rgba: [u8; 4]) {
    let Some(path) = PathBuilder::from_circle(x, y, radius) else {
        return;
    };
    let stroke = Stroke {
        width,
        ..Stroke::default()
    };
    let paint = paint(rgba);
    pixmap.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
}

fn draw_filled_circle(pixmap: &mut Pixmap, x: f32, y: f32, radius: f32, rgba: [u8; 4]) {
    let Some(path) = PathBuilder::from_circle(x, y, radius) else {
        return;
    };
    let paint = paint(rgba);
    pixmap.fill_path(
        &path,
        &paint,
        FillRule::Winding,
        Transform::identity(),
        None,
    );
}

fn draw_line(pixmap: &mut Pixmap, x0: f32, y0: f32, x1: f32, y1: f32, width: f32, rgba: [u8; 4]) {
    let mut builder = PathBuilder::new();
    builder.move_to(x0, y0);
    builder.line_to(x1, y1);
    let Some(path) = builder.finish() else {
        return;
    };
    let stroke = Stroke {
        width,
        line_cap: LineCap::Round,
        ..Stroke::default()
    };
    let paint = paint(rgba);
    pixmap.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
}

fn paint(rgba: [u8; 4]) -> Paint<'static> {
    let mut paint = Paint::default();
    paint.set_color_rgba8(rgba[0], rgba[1], rgba[2], rgba[3]);
    paint.force_hq_pipeline = true;
    paint
}

fn color(rgba: [u8; 4]) -> tiny_skia::Color {
    tiny_skia::Color::from_rgba8(rgba[0], rgba[1], rgba[2], rgba[3])
}

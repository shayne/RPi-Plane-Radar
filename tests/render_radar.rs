mod support;

use std::fs;
use std::sync::Arc;
use std::time::Duration;

use planeradar::model::{
    Aircraft, Airport, GeoPoint, Location, RadarSettings, RadarSnapshot, Runway, Units,
};
use planeradar::render::radar::{BackgroundKey, RadarRenderer};
use planeradar::render::theme::{
    AIRCRAFT, BACKGROUND, CENTER, GRID, GRID_OUTER_RADIUS, RUNWAY, SIZE, STALE, TRACK,
};
use planeradar::render::{FontAsset, Frame, RenderError};
use support::FrameAssertions;

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

fn test_renderer() -> RadarRenderer {
    RadarRenderer::new(FontAsset::embedded().expect("embedded DejaVu font"))
}

fn configured_settings() -> RadarSettings {
    RadarSettings {
        schema_version: 1,
        location: Some(Location {
            latitude: ORIGIN_LATITUDE,
            longitude: ORIGIN_LONGITUDE,
            label: "Fixture".to_owned(),
        }),
        units: Units::Kilometres,
        show_runways: true,
        range_index: 1,
    }
}

fn empty_snapshot(fetched_at: Option<Duration>) -> RadarSnapshot {
    RadarSnapshot {
        aircraft: Arc::from([]),
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
        latitude: ORIGIN_LATITUDE,
        longitude: east_longitude(east_km),
        nose_degrees: 90.0,
        track_degrees: 90.0,
        ground_speed_knots: speed,
        callsign: "EAST123".to_owned(),
        aircraft_type: "A320".to_owned(),
        altitude: "12000".to_owned(),
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

#[test]
fn empty_radar_has_exact_size_palette_rings_and_center() {
    let mut renderer = test_renderer();
    let frame = renderer
        .render(
            &empty_snapshot(Some(Duration::ZERO)),
            &configured_settings(),
            &[],
            Duration::ZERO,
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
        fetched_at: Some(Duration::ZERO),
        last_error_at: None,
    };
    let mut renderer = test_renderer();
    let frame = renderer
        .render(&snapshot, &configured_settings(), &[], Duration::ZERO)
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
fn traffic_outside_the_aircraft_safe_ring_is_a_red_dot_on_the_238_pixel_rim() {
    let snapshot = RadarSnapshot {
        aircraft: Arc::from([aircraft(13.0, 0.0)]),
        fetched_at: Some(Duration::ZERO),
        last_error_at: None,
    };
    let mut renderer = test_renderer();
    let frame = renderer
        .render(&snapshot, &configured_settings(), &[], Duration::ZERO)
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
        fetched_at: Some(Duration::ZERO),
        last_error_at: None,
    };
    let mut renderer = test_renderer();
    let frame = renderer
        .render(&snapshot, &configured_settings(), &[], Duration::ZERO)
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
        )
        .expect("fresh render");
    let threshold = renderer
        .render(
            &empty_snapshot(Some(Duration::from_secs(5))),
            &settings,
            &[],
            Duration::from_secs(35),
        )
        .expect("stale render");
    let underflow = renderer
        .render(
            &empty_snapshot(Some(Duration::from_secs(35))),
            &settings,
            &[],
            Duration::from_secs(5),
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
    bad_schema.schema_version = 2;
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
                .render(&snapshot, &settings, &[], Duration::ZERO)
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
        fetched_at: Some(Duration::ZERO),
        last_error_at: None,
    };

    test_renderer()
        .render(&snapshot, &configured_settings(), &[], Duration::ZERO)
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

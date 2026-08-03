use planeradar::flight_data::RouteCandidate;
use planeradar::model::{Aircraft, GeoPoint};

fn point(latitude: f64, longitude: f64) -> GeoPoint {
    GeoPoint {
        latitude,
        longitude,
    }
}

fn aircraft(latitude: f64, longitude: f64) -> Aircraft {
    Aircraft {
        hex: "abc123".to_owned(),
        flight_callsign: "AAL1".to_owned(),
        latitude,
        longitude,
        nose_degrees: 0.0,
        track_degrees: 0.0,
        ground_speed_knots: 0.0,
        callsign: "AAL1".to_owned(),
        aircraft_type: "B738".to_owned(),
        altitude_feet: Some(10_000),
        altitude: "10000 ft".to_owned(),
    }
}

fn candidate(label: &str, points: Vec<GeoPoint>) -> RouteCandidate {
    RouteCandidate::new(label.to_owned(), points).expect("valid route candidate")
}

#[test]
fn accepts_new_york_on_a_jfk_to_lax_candidate() {
    let route = candidate(
        "JFK→LAX",
        vec![point(40.6413, -73.7781), point(33.9416, -118.4085)],
    );
    assert_eq!(
        route.label_for(&aircraft(40.792_283, -73.972_639_1)),
        Some("JFK→LAX")
    );
}

#[test]
fn rejects_new_york_for_iah_to_abq() {
    let route = candidate(
        "IAH→ABQ",
        vec![point(29.9902, -95.3368), point(35.0402, -106.6090)],
    );
    assert_eq!(route.label_for(&aircraft(40.792_283, -73.972_639_1)), None);
}

#[test]
fn midpoint_candidate_is_accepted_near_either_leg() {
    let route = candidate(
        "SFO→HNL→NRT",
        vec![
            point(37.6213, -122.3790),
            point(21.3187, -157.9225),
            point(35.7720, 140.3929),
        ],
    );
    assert_eq!(
        route.label_for(&aircraft(21.35, -157.90)),
        Some("SFO→HNL→NRT")
    );
    assert_eq!(route.label_for(&aircraft(34.0, 145.0)), Some("SFO→HNL→NRT"));
}

#[test]
fn date_line_segment_uses_the_short_wrapped_path() {
    let route = candidate("AAA→BBB", vec![point(10.0, 170.0), point(10.0, -170.0)]);
    assert_eq!(route.label_for(&aircraft(10.0, 179.0)), Some("AAA→BBB"));
}

#[test]
fn point_beyond_an_endpoint_does_not_use_the_infinite_great_circle() {
    let route = candidate("AAA→BBB", vec![point(0.0, 0.0), point(0.0, 2.0)]);
    assert_eq!(route.label_for(&aircraft(0.0, 5.0)), None);
}

#[test]
fn invalid_live_aircraft_coordinates_fail_closed() {
    let route = candidate("AAA→BBB", vec![point(0.0, 0.0), point(0.0, 2.0)]);
    assert_eq!(route.label_for(&aircraft(f64::NAN, 1.0)), None);
    assert_eq!(route.label_for(&aircraft(0.0, 181.0)), None);
}

#[test]
fn invalid_points_and_degenerate_segments_are_rejected() {
    assert!(
        RouteCandidate::new("BAD".to_owned(), vec![point(91.0, 0.0), point(0.0, 0.0)]).is_none()
    );
    assert!(
        RouteCandidate::new(
            "BAD".to_owned(),
            vec![point(0.0, f64::NAN), point(0.0, 1.0)]
        )
        .is_none()
    );
    assert!(RouteCandidate::new("BAD".to_owned(), vec![point(0.0, 0.0)]).is_none());
    assert!(
        RouteCandidate::new(
            "BAD".to_owned(),
            vec![point(0.0, 0.0), point(0.0, 0.000_001)]
        )
        .is_none()
    );
    assert!(
        RouteCandidate::new("BAD".to_owned(), vec![point(0.0, 0.0), point(0.0, 180.0)]).is_none()
    );
}

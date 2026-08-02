use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use planeradar::adsb::{AdsbClient, AdsbError, AltitudeFilter, parse_aircraft};
use planeradar::http::{HttpClient, HttpError, HttpRequest, HttpResponse, UreqHttpClient};
use planeradar::model::{AircraftKey, Location, RadarSettings};

fn fixture() -> serde_json::Value {
    serde_json::from_str(include_str!("fixtures/adsb/aircraft.json")).expect("fixture")
}

fn unbounded() -> AltitudeFilter {
    AltitudeFilter {
        minimum_feet: None,
        maximum_feet: None,
    }
}

#[test]
fn parser_preserves_upstream_field_preference() {
    let aircraft = parse_aircraft(&fixture(), 64, false, unbounded()).expect("parse");
    let first = &aircraft[0];
    assert_eq!(first.hex, "a835af");
    assert_eq!(first.flight_callsign, "UAL123");
    assert_eq!(first.callsign, "UAL123");
    assert_eq!(first.altitude_feet, Some(33_000));
    assert_eq!(
        first.key(),
        AircraftKey {
            hex: "a835af".to_owned(),
            callsign: "UAL123".to_owned(),
        }
    );
    assert_eq!(first.nose_degrees, 91.0);
    assert_eq!(first.track_degrees, 93.0);
    assert_eq!(first.ground_speed_knots, 420.0);
    assert!(aircraft.iter().all(|item| item.altitude != "GND"));
    assert_eq!(aircraft.len(), 64);
}

#[test]
fn parser_uses_heading_fallback_orders() {
    let aircraft = parse_aircraft(&fixture(), usize::MAX, false, unbounded()).expect("parse");
    let named = |callsign: &str| {
        aircraft
            .iter()
            .find(|item| item.callsign == callsign)
            .expect("named aircraft")
    };

    assert_eq!(
        (named("MAG002").nose_degrees, named("MAG002").track_degrees),
        (102.0, 103.0)
    );
    assert_eq!(
        (named("TRK003").nose_degrees, named("TRK003").track_degrees),
        (113.0, 113.0)
    );
    assert_eq!(
        (named("DIR004").nose_degrees, named("DIR004").track_degrees),
        (124.0, 124.0)
    );
    assert_eq!(
        (
            named("ZERO005").nose_degrees,
            named("ZERO005").track_degrees
        ),
        (0.0, 0.0)
    );
    assert_eq!(named("TAS006").track_degrees, 131.0);
    assert_eq!(named("IAS007").track_degrees, 141.0);
}

#[test]
fn parser_uses_speed_fallback_order() {
    let aircraft = parse_aircraft(&fixture(), usize::MAX, false, unbounded()).expect("parse");
    let speed = |callsign: &str| {
        aircraft
            .iter()
            .find(|item| item.callsign == callsign)
            .expect("named aircraft")
            .ground_speed_knots
    };

    assert_eq!(speed("UAL123"), 420.0);
    assert_eq!(speed("TAS006"), 306.0);
    assert_eq!(speed("IAS007"), 307.0);
    assert_eq!(speed("SPD008"), 0.0);
}

#[test]
fn parser_trims_flight_then_falls_back_to_hex() {
    let aircraft = parse_aircraft(&fixture(), usize::MAX, false, unbounded()).expect("parse");
    assert_eq!(aircraft[0].callsign, "UAL123");
    let hex_fallback = aircraft
        .iter()
        .find(|item| item.callsign == "abc009")
        .expect("hex fallback aircraft");
    assert_eq!(hex_fallback.hex, "abc009");
    assert_eq!(hex_fallback.flight_callsign, "");
    assert_eq!(hex_fallback.callsign, "abc009");
    assert_eq!(
        hex_fallback.key(),
        AircraftKey {
            hex: "abc009".to_owned(),
            callsign: String::new(),
        }
    );
}

#[test]
fn parser_prefers_baro_altitude_then_geometric_altitude() {
    let aircraft = parse_aircraft(&fixture(), usize::MAX, false, unbounded()).expect("parse");
    let first = &aircraft[0];
    let geometric = aircraft
        .iter()
        .find(|item| item.callsign == "GEO010")
        .expect("geometric altitude aircraft");

    assert_eq!(first.altitude, "33000 ft");
    assert_eq!(first.altitude_feet, Some(33_000));
    assert_eq!(geometric.altitude, "14501 ft");
    assert_eq!(geometric.altitude_feet, Some(14_501));
}

#[test]
fn parser_rejects_out_of_range_baro_before_using_geometric_altitude() {
    let response = serde_json::json!({
        "ac": [{
            "hex": "abc011",
            "lat": 40.751,
            "lon": -73.991,
            "alt_baro": 3_000_000_000_i64,
            "alt_geom": 9_000.4
        }]
    });

    let aircraft =
        parse_aircraft(&response, usize::MAX, false, unbounded()).expect("parse aircraft");
    assert_eq!(aircraft[0].altitude_feet, Some(9_000));
    assert_eq!(aircraft[0].altitude, "9000 ft");
}

#[test]
fn altitude_filter_is_inclusive_and_uses_the_rounded_display_altitude() {
    let cases = [
        ("minimum below", Some(14_500), None, 1),
        ("minimum equal", Some(14_501), None, 1),
        ("minimum above", Some(14_502), None, 0),
        ("maximum below", None, Some(14_500), 0),
        ("maximum equal", None, Some(14_501), 1),
        ("maximum above", None, Some(14_502), 1),
        ("equal bounds", Some(14_501), Some(14_501), 1),
    ];
    let response = serde_json::json!({
        "ac": [{
            "hex": "abc010",
            "flight": "GEO010 ",
            "lat": 40.751,
            "lon": -73.991,
            "alt_geom": 14500.6
        }]
    });

    for (name, minimum_feet, maximum_feet, expected_len) in cases {
        let parsed = parse_aircraft(
            &response,
            usize::MAX,
            false,
            AltitudeFilter {
                minimum_feet,
                maximum_feet,
            },
        )
        .expect(name);
        assert_eq!(parsed.len(), expected_len, "{name}");
        if let Some(aircraft) = parsed.first() {
            assert_eq!(aircraft.altitude_feet, Some(14_501), "{name}");
            assert_eq!(aircraft.altitude, "14501 ft", "{name}");
        }
    }
}

#[test]
fn altitude_filter_handles_negative_and_unknown_altitudes() {
    let response = serde_json::json!({
        "ac": [
            {"hex": "negative", "lat": 40.0, "lon": -73.0, "alt_baro": -25.4},
            {"hex": "unknown", "lat": 40.1, "lon": -73.1}
        ]
    });

    let unbounded_aircraft =
        parse_aircraft(&response, usize::MAX, false, unbounded()).expect("unbounded");
    assert_eq!(unbounded_aircraft.len(), 2);
    assert_eq!(unbounded_aircraft[0].altitude_feet, Some(-25));
    assert_eq!(unbounded_aircraft[0].altitude, "-25 ft");
    assert_eq!(unbounded_aircraft[1].altitude_feet, None);

    let bounded = parse_aircraft(
        &response,
        usize::MAX,
        false,
        AltitudeFilter {
            minimum_feet: Some(-25),
            maximum_feet: Some(-25),
        },
    )
    .expect("bounded");
    assert_eq!(bounded.len(), 1);
    assert_eq!(bounded[0].hex, "negative");
}

#[test]
fn altitude_filter_runs_before_the_accepted_aircraft_limit() {
    let aircraft = parse_aircraft(
        &fixture(),
        2,
        false,
        AltitudeFilter {
            minimum_feet: Some(12_002),
            maximum_feet: None,
        },
    )
    .expect("parse");

    assert_eq!(aircraft.len(), 2);
    assert_eq!(aircraft[0].callsign, "UAL123");
    assert_eq!(aircraft[1].callsign, "MAG002");
}

#[test]
fn altitude_filter_derives_from_radar_settings() {
    let settings = RadarSettings {
        minimum_altitude_feet: Some(-100),
        maximum_altitude_feet: Some(45_000),
        ..RadarSettings::default()
    };

    assert_eq!(
        AltitudeFilter::from(&settings),
        AltitudeFilter {
            minimum_feet: Some(-100),
            maximum_feet: Some(45_000),
        }
    );
}

#[test]
fn parser_can_include_ground_aircraft() {
    let hidden = parse_aircraft(&fixture(), usize::MAX, false, unbounded()).expect("parse");
    let shown = parse_aircraft(&fixture(), usize::MAX, true, unbounded()).expect("parse");

    assert!(!hidden.iter().any(|item| item.callsign == "GROUND"));
    assert_eq!(
        shown
            .iter()
            .find(|item| item.callsign == "GROUND")
            .expect("ground aircraft")
            .altitude,
        "GND"
    );
}

#[test]
fn parser_skips_missing_coordinates_and_tolerates_malformed_optional_fields() {
    let aircraft = parse_aircraft(&fixture(), usize::MAX, false, unbounded()).expect("parse");

    assert!(!aircraft.iter().any(|item| item.callsign == "NOCOORD"));
    let malformed = aircraft
        .iter()
        .find(|item| item.latitude == 40.7513)
        .expect("record with malformed optional fields");
    assert_eq!(malformed.nose_degrees, 0.0);
    assert_eq!(malformed.track_degrees, 0.0);
    assert_eq!(malformed.ground_speed_knots, 0.0);
    assert_eq!(malformed.callsign, "");
    assert_eq!(malformed.aircraft_type, "");
    assert_eq!(malformed.altitude, "");
}

#[test]
fn parser_accepts_empty_or_missing_aircraft_array() {
    let empty = serde_json::from_str(include_str!("fixtures/adsb/empty.json")).expect("fixture");
    assert!(
        parse_aircraft(&empty, 64, false, unbounded())
            .expect("empty response")
            .is_empty()
    );
    assert!(
        parse_aircraft(
            &serde_json::json!({"now": 1784995200}),
            64,
            false,
            unbounded(),
        )
        .expect("missing ac")
        .is_empty()
    );
}

#[test]
fn parser_rejects_malformed_top_level_schema() {
    let malformed =
        serde_json::from_str(include_str!("fixtures/adsb/malformed.json")).expect("fixture");
    assert!(matches!(
        parse_aircraft(&malformed, 64, false, unbounded()),
        Err(AdsbError::Schema(_))
    ));
    assert!(matches!(
        parse_aircraft(&serde_json::json!([]), 64, false, unbounded()),
        Err(AdsbError::Schema(_))
    ));
}

#[derive(Clone, Debug)]
struct FakeHttpClient {
    state: Arc<FakeHttpState>,
}

#[derive(Debug)]
struct FakeHttpState {
    results: Mutex<VecDeque<Result<HttpResponse, HttpError>>>,
    requests: Mutex<Vec<HttpRequest>>,
}

impl FakeHttpClient {
    fn responding(result: Result<HttpResponse, HttpError>) -> Self {
        Self::responding_in_order([result])
    }

    fn responding_in_order(
        results: impl IntoIterator<Item = Result<HttpResponse, HttpError>>,
    ) -> Self {
        Self {
            state: Arc::new(FakeHttpState {
                results: Mutex::new(results.into_iter().collect()),
                requests: Mutex::new(Vec::new()),
            }),
        }
    }

    fn request(&self) -> HttpRequest {
        self.state
            .requests
            .lock()
            .expect("requests")
            .first()
            .expect("request")
            .clone()
    }
}

impl HttpClient for FakeHttpClient {
    fn execute(&self, request: HttpRequest) -> Result<HttpResponse, HttpError> {
        self.state.requests.lock().expect("requests").push(request);
        self.state
            .results
            .lock()
            .expect("results")
            .pop_front()
            .expect("fake response")
    }
}

fn ok_response(body: &[u8]) -> Result<HttpResponse, HttpError> {
    Ok(HttpResponse {
        status: 200,
        body: body.to_vec(),
    })
}

fn location() -> Location {
    Location {
        latitude: 40.7128,
        longitude: -74.006,
        label: String::new(),
    }
}

#[test]
fn fetch_builds_exact_tls_verified_bounded_request() {
    let http = FakeHttpClient::responding(ok_response(
        include_str!("fixtures/adsb/empty.json").as_bytes(),
    ));
    let request_probe = http.clone();
    let client = AdsbClient::new(http);

    assert!(
        client
            .fetch(&location(), 13.3333, unbounded())
            .expect("fetch")
            .is_empty()
    );
    let request = request_probe.request();
    assert_eq!(
        request.url,
        "https://opendata.adsb.fi/api/v3/lat/40.712800/lon/-74.006000/dist/7.2"
    );
    assert!(request.query.is_empty());
    assert!(request.headers.is_empty());
    assert_eq!(request.connect_timeout, Duration::from_millis(3050));
    assert_eq!(request.read_timeout, Duration::from_secs(10));
    assert_eq!(request.max_response_bytes, 2 * 1024 * 1024);
    assert!(request.verify_tls);
}

#[test]
fn fetch_propagates_timeout_instead_of_returning_a_previous_snapshot() {
    let http = FakeHttpClient::responding_in_order([
        ok_response(include_str!("fixtures/adsb/aircraft.json").as_bytes()),
        Err(HttpError::Timeout),
    ]);
    let client = AdsbClient::new(http);

    assert_eq!(
        client
            .fetch(&location(), 13.3333, unbounded())
            .expect("first fetch")
            .len(),
        64
    );
    assert!(matches!(
        client.fetch(&location(), 13.3333, unbounded()),
        Err(AdsbError::Http(HttpError::Timeout))
    ));
}

#[test]
fn fetch_maps_status_body_and_json_errors() {
    let status = AdsbClient::new(FakeHttpClient::responding(Ok(HttpResponse {
        status: 503,
        body: b"not retained".to_vec(),
    })));
    assert!(matches!(
        status.fetch(&location(), 13.3333, unbounded()),
        Err(AdsbError::Status(503))
    ));

    let body = AdsbClient::new(FakeHttpClient::responding(Err(HttpError::Body)));
    assert!(matches!(
        body.fetch(&location(), 13.3333, unbounded()),
        Err(AdsbError::Http(HttpError::Body))
    ));

    let json = AdsbClient::new(FakeHttpClient::responding(ok_response(b"{")));
    assert!(matches!(
        json.fetch(&location(), 13.3333, unbounded()),
        Err(AdsbError::Json(_))
    ));
}

#[test]
fn production_http_client_rejects_disabled_tls_before_network_io() {
    let error = UreqHttpClient
        .execute(HttpRequest {
            url: "https://example.invalid/never-contacted".to_owned(),
            query: Vec::new(),
            headers: Vec::new(),
            connect_timeout: Duration::from_millis(1),
            read_timeout: Duration::from_millis(1),
            max_response_bytes: 1,
            verify_tls: false,
        })
        .expect_err("disabled TLS must be rejected");

    assert!(matches!(error, HttpError::TlsVerificationRequired));
}

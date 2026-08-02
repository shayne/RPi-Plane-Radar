use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use planeradar::flight_data::{
    EnrichmentCache, EnrichmentNeeds, FlightDataClient, FlightDataError, FlightLookup, LookupValue,
};
use planeradar::http::{HttpClient, HttpError, HttpRequest, HttpResponse};
use planeradar::model::Aircraft;
use planeradar::time::{Clock, Sleeper};

#[derive(Clone, Debug, Default)]
struct FakeClock {
    monotonic: Arc<Mutex<Duration>>,
}

impl Clock for FakeClock {
    fn monotonic(&self) -> Duration {
        *self.monotonic.lock().expect("monotonic clock")
    }

    fn unix_seconds(&self) -> u64 {
        0
    }
}

#[derive(Clone, Debug)]
struct FakeSleeper {
    clock: FakeClock,
    sleeps: Arc<Mutex<Vec<Duration>>>,
}

impl FakeSleeper {
    fn new(clock: FakeClock) -> Self {
        Self {
            clock,
            sleeps: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn sleeps(&self) -> Vec<Duration> {
        self.sleeps.lock().expect("sleep records").clone()
    }
}

impl Sleeper for FakeSleeper {
    fn sleep(&self, duration: Duration) {
        self.sleeps.lock().expect("sleep records").push(duration);
        *self.clock.monotonic.lock().expect("monotonic clock") += duration;
    }
}

#[derive(Clone, Debug)]
struct FakeHttpClient {
    state: Arc<FakeHttpState>,
}

#[derive(Debug)]
struct FakeHttpState {
    clock: FakeClock,
    responses: Mutex<VecDeque<Result<HttpResponse, HttpError>>>,
    requests: Mutex<Vec<(Duration, HttpRequest)>>,
}

impl FakeHttpClient {
    fn responding(
        clock: FakeClock,
        responses: impl IntoIterator<Item = Result<HttpResponse, HttpError>>,
    ) -> Self {
        Self {
            state: Arc::new(FakeHttpState {
                clock,
                responses: Mutex::new(responses.into_iter().collect()),
                requests: Mutex::new(Vec::new()),
            }),
        }
    }

    fn request_count(&self) -> usize {
        self.state.requests.lock().expect("requests").len()
    }

    fn requests(&self) -> Vec<(Duration, HttpRequest)> {
        self.state.requests.lock().expect("requests").clone()
    }
}

impl HttpClient for FakeHttpClient {
    fn execute(&self, request: HttpRequest) -> Result<HttpResponse, HttpError> {
        self.state
            .requests
            .lock()
            .expect("requests")
            .push((self.state.clock.monotonic(), request));
        self.state
            .responses
            .lock()
            .expect("responses")
            .pop_front()
            .expect("fake response")
    }
}

fn ok(body: &[u8]) -> Result<HttpResponse, HttpError> {
    Ok(HttpResponse {
        status: 200,
        body: body.to_vec(),
    })
}

fn status(status: u16, body: &[u8]) -> Result<HttpResponse, HttpError> {
    Ok(HttpResponse {
        status,
        body: body.to_vec(),
    })
}

fn json_response(value: serde_json::Value) -> Result<HttpResponse, HttpError> {
    ok(&serde_json::to_vec(&value).expect("response JSON"))
}

fn aircraft(hex: &str, flight_callsign: &str) -> Aircraft {
    Aircraft {
        hex: hex.to_owned(),
        flight_callsign: flight_callsign.to_owned(),
        latitude: 40.0,
        longitude: -74.0,
        nose_degrees: 0.0,
        track_degrees: 0.0,
        ground_speed_knots: 0.0,
        callsign: flight_callsign.to_owned(),
        aircraft_type: String::new(),
        altitude_feet: Some(10_000),
        altitude: "10000 ft".to_owned(),
    }
}

fn client(
    responses: impl IntoIterator<Item = Result<HttpResponse, HttpError>>,
) -> (
    FlightDataClient<FakeHttpClient, FakeClock, FakeSleeper>,
    FakeHttpClient,
    FakeSleeper,
) {
    let clock = FakeClock::default();
    let http = FakeHttpClient::responding(clock.clone(), responses);
    let probe = http.clone();
    let sleeper = FakeSleeper::new(clock.clone());
    let sleeper_probe = sleeper.clone();
    (
        FlightDataClient::with_provider_base(
            http,
            clock,
            sleeper,
            "https://api.adsbdb.test/v0///".to_owned(),
        ),
        probe,
        sleeper_probe,
    )
}

fn both() -> EnrichmentNeeds {
    EnrichmentNeeds {
        route: true,
        model: true,
    }
}

#[test]
fn combined_fixture_prefers_iata_and_compacts_the_model() {
    let (mut client, _, _) = client([ok(include_bytes!("fixtures/adsbdb/combined.json"))]);

    let lookup = client
        .lookup(&aircraft("a1-b2 c3", "aa 123"), both())
        .expect("combined lookup");

    assert_eq!(
        lookup,
        FlightLookup {
            route: LookupValue::Found("JFK→LAX".to_owned()),
            model: LookupValue::Found("737-800".to_owned()),
        }
    );
}

#[test]
fn route_uses_icao_fallback_and_rejects_a_one_ended_route() {
    let icao_only = serde_json::json!({
        "response": {
            "flightroute": {
                "origin": {"iata_code": " ", "icao_code": "KSEA"},
                "destination": {"iata_code": null, "icao_code": "KORD"}
            }
        }
    });
    let one_ended = serde_json::json!({
        "response": {
            "flightroute": {
                "origin": {"iata_code": "SEA", "icao_code": "KSEA"},
                "destination": {"iata_code": "", "icao_code": ""}
            }
        }
    });
    let (mut client, _, _) = client([json_response(icao_only), json_response(one_ended)]);
    let needs = EnrichmentNeeds {
        route: true,
        model: false,
    };

    assert_eq!(
        client
            .lookup(&aircraft("abc123", "asa1"), needs)
            .expect("ICAO route"),
        FlightLookup {
            route: LookupValue::Found("KSEA→KORD".to_owned()),
            model: LookupValue::NotRequested,
        }
    );
    assert_eq!(
        client
            .lookup(&aircraft("abc123", "asa2"), needs)
            .expect("one-ended route"),
        FlightLookup {
            route: LookupValue::Missing,
            model: LookupValue::NotRequested,
        }
    );
}

#[test]
fn model_normalization_strips_only_approved_leading_manufacturers() {
    let cases = [
        ("Boeing  737-800", "737-800"),
        ("boEIng\t737 MAX 8", "737 MAX 8"),
        ("Airbus\nA320-214", "A320-214"),
        ("Embraer   E175", "E175"),
        ("Bombardier CRJ-900", "CRJ-900"),
        ("De Havilland Canada   DHC-8-400", "DHC-8-400"),
        ("McDonnell Douglas   MD-80", "McDonnell Douglas MD-80"),
    ];
    let responses = cases.iter().map(|(model, _)| {
        json_response(serde_json::json!({
            "response": {"aircraft": {"type": model, "icao_type": "FALLBACK"}}
        }))
    });
    let (mut client, _, _) = client(responses);
    let needs = EnrichmentNeeds {
        route: false,
        model: true,
    };

    for (index, (_, expected)) in cases.iter().enumerate() {
        let lookup = client
            .lookup(&aircraft(&format!("abc{index}"), "unused"), needs)
            .expect("model lookup");
        assert_eq!(lookup.model, LookupValue::Found((*expected).to_owned()));
        assert_eq!(lookup.route, LookupValue::NotRequested);
    }
}

#[test]
fn blank_model_type_falls_back_to_normalized_icao_type() {
    let response = serde_json::json!({
        "response": {"aircraft": {"type": " \t ", "icao_type": "  B738  "}}
    });
    let (mut client, _, _) = client([json_response(response)]);

    let lookup = client
        .lookup(
            &aircraft("abc123", "unused"),
            EnrichmentNeeds {
                route: false,
                model: true,
            },
        )
        .expect("model lookup");

    assert_eq!(lookup.model, LookupValue::Found("B738".to_owned()));
}

#[test]
fn unknown_and_not_found_responses_are_definite_misses() {
    let (mut client, _, _) = client([
        ok(include_bytes!("fixtures/adsbdb/unknown.json")),
        status(404, b"not found"),
    ]);

    assert_eq!(
        client
            .lookup(
                &aircraft("abc123", "aal1"),
                EnrichmentNeeds {
                    route: false,
                    model: true,
                },
            )
            .expect("unknown aircraft"),
        FlightLookup {
            route: LookupValue::NotRequested,
            model: LookupValue::Missing,
        }
    );
    assert_eq!(
        client
            .lookup(
                &aircraft("abc124", "aal2"),
                EnrichmentNeeds {
                    route: true,
                    model: false,
                },
            )
            .expect("404 route"),
        FlightLookup {
            route: LookupValue::Missing,
            model: LookupValue::NotRequested,
        }
    );
}

#[test]
fn malformed_json_wrong_root_types_and_unexpected_statuses_are_errors() {
    let (mut malformed, _, _) = client([ok(br#"{"response": "#)]);
    assert!(matches!(
        malformed.lookup(&aircraft("abc123", "aal1"), both()),
        Err(FlightDataError::Json(_))
    ));

    for wrong_root in [serde_json::json!([]), serde_json::json!({"response": 42})] {
        let (mut client, _, _) = client([json_response(wrong_root)]);
        assert!(matches!(
            client.lookup(&aircraft("abc123", "aal1"), both()),
            Err(FlightDataError::Schema(_))
        ));
    }

    let (mut unavailable, _, _) = client([status(503, b"unavailable")]);
    assert!(matches!(
        unavailable.lookup(&aircraft("abc123", "aal1"), both()),
        Err(FlightDataError::Status(503))
    ));
}

#[test]
fn transport_errors_remain_http_errors() {
    let (mut client, _, _) = client([Err(HttpError::Timeout)]);

    assert!(matches!(
        client.lookup(
            &aircraft("abc123", "unused"),
            EnrichmentNeeds {
                route: false,
                model: true,
            }
        ),
        Err(FlightDataError::Http(HttpError::Timeout))
    ));
}

#[test]
fn non_https_provider_base_is_rejected_before_http_execution() {
    let clock = FakeClock::default();
    let http = FakeHttpClient::responding(clock.clone(), []);
    let probe = http.clone();
    let sleeper = FakeSleeper::new(clock.clone());
    let mut client = FlightDataClient::with_provider_base(
        http,
        clock,
        sleeper,
        "http://api.adsbdb.test/v0".to_owned(),
    );

    assert!(matches!(
        client.lookup(&aircraft("abc123", "aal1"), both()),
        Err(FlightDataError::Schema(_))
    ));
    assert_eq!(probe.request_count(), 0);
}

#[test]
fn provider_base_preserves_path_and_rejects_query_or_fragment_before_http() {
    let model_response = serde_json::json!({
        "response": {"aircraft": {"type": "Boeing 737-800", "icao_type": "B738"}}
    });
    let clock = FakeClock::default();
    let path_http = FakeHttpClient::responding(clock.clone(), [json_response(model_response)]);
    let path_probe = path_http.clone();
    let sleeper = FakeSleeper::new(clock.clone());
    let mut path_client = FlightDataClient::with_provider_base(
        path_http,
        clock,
        sleeper,
        "https://api.adsbdb.test/custom/v0///".to_owned(),
    );
    path_client
        .lookup(
            &aircraft("abc123", "unused"),
            EnrichmentNeeds {
                route: false,
                model: true,
            },
        )
        .expect("base-path lookup");
    assert_eq!(
        path_probe.requests()[0].1.url,
        "https://api.adsbdb.test/custom/v0/aircraft/ABC123"
    );

    for invalid_base in [
        "https://api.adsbdb.test/v0?tenant=one",
        "https://api.adsbdb.test/v0#provider",
    ] {
        let clock = FakeClock::default();
        let http = FakeHttpClient::responding(clock.clone(), []);
        let probe = http.clone();
        let sleeper = FakeSleeper::new(clock.clone());
        let mut client =
            FlightDataClient::with_provider_base(http, clock, sleeper, invalid_base.to_owned());

        assert!(matches!(
            client.lookup(&aircraft("abc123", "aal1"), both()),
            Err(FlightDataError::Schema(_))
        ));
        assert_eq!(probe.request_count(), 0, "base: {invalid_base}");
    }
}

#[test]
fn endpoint_selection_normalizes_identifiers_and_sends_only_required_data() {
    let (mut route_client, route_http, _) =
        client([ok(include_bytes!("fixtures/adsbdb/callsign.json"))]);
    route_client
        .lookup(
            &aircraft("ab-c 123", "aa-l 12!"),
            EnrichmentNeeds {
                route: true,
                model: false,
            },
        )
        .expect("route lookup");
    let route_request = &route_http.requests()[0].1;
    assert_eq!(
        route_request.url,
        "https://api.adsbdb.test/v0/callsign/AAL12"
    );
    assert!(route_request.query.is_empty());

    let model_response = serde_json::json!({
        "response": {"aircraft": {"type": "Airbus A320-214", "icao_type": "A320"}}
    });
    let (mut model_client, model_http, _) = client([json_response(model_response)]);
    model_client
        .lookup(
            &aircraft("ab-c 123", "private callsign"),
            EnrichmentNeeds {
                route: false,
                model: true,
            },
        )
        .expect("model lookup");
    let model_request = &model_http.requests()[0].1;
    assert_eq!(
        model_request.url,
        "https://api.adsbdb.test/v0/aircraft/ABC123"
    );
    assert!(model_request.query.is_empty());

    let (mut combined_client, combined_http, _) =
        client([ok(include_bytes!("fixtures/adsbdb/combined.json"))]);
    combined_client
        .lookup(&aircraft("ab-c 123", "aa-l 12!"), both())
        .expect("combined lookup");
    let combined_request = &combined_http.requests()[0].1;
    assert_eq!(
        combined_request.url,
        "https://api.adsbdb.test/v0/aircraft/ABC123"
    );
    assert_eq!(
        combined_request.query,
        vec![("callsign".to_owned(), "AAL12".to_owned())]
    );

    for request in [route_request, model_request, combined_request] {
        assert!(request.headers.is_empty());
        assert_eq!(request.connect_timeout, Duration::from_secs(2));
        assert_eq!(request.read_timeout, Duration::from_secs(3));
        assert_eq!(request.max_response_bytes, 256 * 1024);
        assert!(request.verify_tls);
    }
}

#[test]
fn missing_identifiers_issue_only_requests_that_can_satisfy_a_field() {
    let model_response = serde_json::json!({
        "response": {"aircraft": {"type": "Boeing 737-800", "icao_type": "B738"}}
    });
    let (mut no_callsign, model_http, _) = client([json_response(model_response)]);
    assert_eq!(
        no_callsign
            .lookup(&aircraft("ab-c123", "---"), both())
            .expect("model-only fallback"),
        FlightLookup {
            route: LookupValue::Missing,
            model: LookupValue::Found("737-800".to_owned()),
        }
    );
    assert_eq!(model_http.request_count(), 1);
    assert!(model_http.requests()[0].1.query.is_empty());

    let (mut no_hex, route_http, _) = client([ok(include_bytes!("fixtures/adsbdb/callsign.json"))]);
    assert_eq!(
        no_hex
            .lookup(&aircraft("---", "aa 12"), both())
            .expect("route-only fallback"),
        FlightLookup {
            route: LookupValue::Found("JFK→LAX".to_owned()),
            model: LookupValue::Missing,
        }
    );
    assert_eq!(route_http.request_count(), 1);
    assert_eq!(
        route_http.requests()[0].1.url,
        "https://api.adsbdb.test/v0/callsign/AA12"
    );

    let (mut neither, neither_http, _) = client([]);
    assert_eq!(
        neither
            .lookup(&aircraft("---", "!!!"), both())
            .expect("no identifiers"),
        FlightLookup {
            route: LookupValue::Missing,
            model: LookupValue::Missing,
        }
    );
    assert_eq!(neither_http.request_count(), 0);

    let (mut disabled, disabled_http, _) = client([]);
    assert_eq!(
        disabled
            .lookup(&aircraft("abc123", "aal1"), EnrichmentNeeds::default())
            .expect("disabled lookup"),
        FlightLookup {
            route: LookupValue::NotRequested,
            model: LookupValue::NotRequested,
        }
    );
    assert_eq!(disabled_http.request_count(), 0);
}

#[test]
fn combined_callsign_fallback_uses_the_spaced_execute_path() {
    let aircraft_only = serde_json::json!({
        "response": {
            "aircraft": {"type": "Boeing 737-800", "icao_type": "B738"}
        }
    });
    let (mut client, http, sleeper) = client([
        json_response(aircraft_only),
        ok(include_bytes!("fixtures/adsbdb/callsign.json")),
    ]);

    let lookup = client
        .lookup(&aircraft("abc123", "aal1"), both())
        .expect("fallback lookup");

    assert_eq!(lookup.route, LookupValue::Found("JFK→LAX".to_owned()));
    assert_eq!(lookup.model, LookupValue::Found("737-800".to_owned()));
    let requests = http.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].0, Duration::ZERO);
    assert!(requests[1].0 >= Duration::from_millis(750));
    assert_eq!(
        requests[1].1.url,
        "https://api.adsbdb.test/v0/callsign/AAL1"
    );
    assert!(requests[1].1.query.is_empty());
    assert_eq!(sleeper.sleeps(), vec![Duration::from_millis(750)]);
}

#[test]
fn combined_unknown_aircraft_still_uses_callsign_fallback_for_route() {
    let (mut client, http, _) = client([
        ok(include_bytes!("fixtures/adsbdb/unknown.json")),
        ok(include_bytes!("fixtures/adsbdb/callsign.json")),
    ]);

    let lookup = client
        .lookup(&aircraft("abc123", "aal1"), both())
        .expect("unknown-aircraft route fallback");

    assert_eq!(lookup.route, LookupValue::Found("JFK→LAX".to_owned()));
    assert_eq!(lookup.model, LookupValue::Missing);
    assert_eq!(http.request_count(), 2);
}

#[test]
fn combined_404_is_a_terminal_miss_without_callsign_fallback() {
    let (mut client, http, _) = client([status(404, b"not found")]);

    let lookup = client
        .lookup(&aircraft("abc123", "aal1"), both())
        .expect("combined 404");

    assert_eq!(lookup.route, LookupValue::Missing);
    assert_eq!(lookup.model, LookupValue::Missing);
    assert_eq!(http.request_count(), 1);
}

fn found(route: &str, model: &str) -> FlightLookup {
    FlightLookup {
        route: LookupValue::Found(route.to_owned()),
        model: LookupValue::Found(model.to_owned()),
    }
}

#[test]
fn cache_keys_routes_by_callsign_and_models_by_hex() {
    let mut cache = EnrichmentCache::new(256);
    let original = aircraft("hex-one", "flight-one");
    cache.record(
        &original,
        both(),
        &found("JFK→LAX", "737-800"),
        Duration::ZERO,
    );

    let same_callsign_new_hex = cache.resolve(
        &aircraft("hex-two", "flight-one"),
        both(),
        Duration::from_secs(1),
    );
    assert_eq!(
        same_callsign_new_hex.enrichment.route,
        Some("JFK→LAX".to_owned())
    );
    assert_eq!(same_callsign_new_hex.enrichment.model, None);
    assert!(!same_callsign_new_hex.pending.route);
    assert!(same_callsign_new_hex.pending.model);

    let same_hex_new_callsign = cache.resolve(
        &aircraft("hex-one", "flight-two"),
        both(),
        Duration::from_secs(2),
    );
    assert_eq!(same_hex_new_callsign.enrichment.route, None);
    assert_eq!(
        same_hex_new_callsign.enrichment.model,
        Some("737-800".to_owned())
    );
    assert!(same_hex_new_callsign.pending.route);
    assert!(!same_hex_new_callsign.pending.model);
}

#[test]
fn successful_entries_expire_at_exactly_six_hours() {
    let mut cache = EnrichmentCache::new(256);
    let plane = aircraft("abc123", "aal1");
    cache.record(&plane, both(), &found("JFK→LAX", "737-800"), Duration::ZERO);

    let before = cache.resolve(&plane, both(), Duration::from_secs(6 * 60 * 60 - 1));
    assert_eq!(before.enrichment.route, Some("JFK→LAX".to_owned()));
    assert_eq!(before.enrichment.model, Some("737-800".to_owned()));
    assert_eq!(before.pending, EnrichmentNeeds::default());

    let boundary = cache.resolve(&plane, both(), Duration::from_secs(6 * 60 * 60));
    assert_eq!(boundary.enrichment.route, None);
    assert_eq!(boundary.enrichment.model, None);
    assert_eq!(boundary.pending, both());
}

#[test]
fn missing_entries_expire_at_exactly_ten_minutes() {
    let mut cache = EnrichmentCache::new(256);
    let plane = aircraft("abc123", "aal1");
    cache.record(
        &plane,
        both(),
        &FlightLookup {
            route: LookupValue::Missing,
            model: LookupValue::Missing,
        },
        Duration::from_secs(30),
    );

    let before = cache.resolve(&plane, both(), Duration::from_secs(30 + 10 * 60 - 1));
    assert_eq!(before.enrichment.route, None);
    assert_eq!(before.enrichment.model, None);
    assert_eq!(before.pending, EnrichmentNeeds::default());

    let boundary = cache.resolve(&plane, both(), Duration::from_secs(30 + 10 * 60));
    assert_eq!(boundary.pending, both());
}

#[test]
fn full_and_partial_cache_hits_request_only_uncached_fields() {
    let mut cache = EnrichmentCache::new(256);
    let plane = aircraft("abc123", "aal1");
    cache.record(
        &plane,
        EnrichmentNeeds {
            route: true,
            model: false,
        },
        &FlightLookup {
            route: LookupValue::Found("JFK→LAX".to_owned()),
            model: LookupValue::NotRequested,
        },
        Duration::ZERO,
    );

    let partial = cache.resolve(&plane, both(), Duration::from_secs(1));
    assert_eq!(partial.enrichment.route, Some("JFK→LAX".to_owned()));
    assert_eq!(
        partial.pending,
        EnrichmentNeeds {
            route: false,
            model: true,
        }
    );

    cache.record(
        &plane,
        partial.pending,
        &FlightLookup {
            route: LookupValue::NotRequested,
            model: LookupValue::Found("737-800".to_owned()),
        },
        Duration::from_secs(1),
    );
    let full = cache.resolve(&plane, both(), Duration::from_secs(2));
    assert_eq!(full.enrichment.route, Some("JFK→LAX".to_owned()));
    assert_eq!(full.enrichment.model, Some("737-800".to_owned()));
    assert_eq!(full.pending, EnrichmentNeeds::default());
}

#[test]
fn not_requested_lookup_values_leave_existing_entries_untouched() {
    let mut cache = EnrichmentCache::new(256);
    let plane = aircraft("abc123", "aal1");
    cache.record(&plane, both(), &found("JFK→LAX", "737-800"), Duration::ZERO);
    cache.record(
        &plane,
        both(),
        &FlightLookup {
            route: LookupValue::NotRequested,
            model: LookupValue::NotRequested,
        },
        Duration::from_secs(1),
    );

    let resolution = cache.resolve(&plane, both(), Duration::from_secs(2));
    assert_eq!(resolution.enrichment.route, Some("JFK→LAX".to_owned()));
    assert_eq!(resolution.enrichment.model, Some("737-800".to_owned()));
    assert_eq!(resolution.pending, EnrichmentNeeds::default());
}

#[test]
fn route_and_model_maps_each_evict_the_least_recently_used_of_256_entries() {
    let mut cache = EnrichmentCache::new(256);
    for index in 0..256 {
        let plane = aircraft(&format!("hex{index}"), &format!("call{index}"));
        cache.record(
            &plane,
            both(),
            &found(&format!("R{index}→D{index}"), &format!("M{index}")),
            Duration::ZERO,
        );
    }

    let first = aircraft("hex0", "call0");
    let touched = cache.resolve(&first, both(), Duration::from_secs(1));
    assert_eq!(touched.pending, EnrichmentNeeds::default());

    let route_newcomer = aircraft("unshared-route-hex", "call256");
    cache.record(
        &route_newcomer,
        EnrichmentNeeds {
            route: true,
            model: false,
        },
        &FlightLookup {
            route: LookupValue::Found("R256→D256".to_owned()),
            model: LookupValue::NotRequested,
        },
        Duration::from_secs(2),
    );

    let second = aircraft("hex1", "call1");
    let after_route_eviction = cache.resolve(&second, both(), Duration::from_secs(3));
    assert!(after_route_eviction.pending.route);
    assert!(!after_route_eviction.pending.model);
    assert_eq!(after_route_eviction.enrichment.model, Some("M1".to_owned()));

    let model_newcomer = aircraft("hex256", "unshared-model-call");
    cache.record(
        &model_newcomer,
        EnrichmentNeeds {
            route: false,
            model: true,
        },
        &FlightLookup {
            route: LookupValue::NotRequested,
            model: LookupValue::Found("M256".to_owned()),
        },
        Duration::from_secs(4),
    );

    let third = aircraft("hex2", "call2");
    let after_model_eviction = cache.resolve(&third, both(), Duration::from_secs(5));
    assert!(!after_model_eviction.pending.route);
    assert!(after_model_eviction.pending.model);
    assert_eq!(
        after_model_eviction.enrichment.route,
        Some("R2→D2".to_owned())
    );

    let retained = cache.resolve(&first, both(), Duration::from_secs(6));
    assert_eq!(retained.pending, EnrichmentNeeds::default());
}

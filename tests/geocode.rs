use std::collections::VecDeque;
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use log::{LevelFilter, Log, Metadata, Record};
use planeradar::geocode::{GeocodeError, Geocoder};
use planeradar::http::{HttpClient, HttpError, HttpRequest, HttpResponse};
use planeradar::time::{Clock, Sleeper};

const START_UNIX: u64 = 1_700_000_000;
const SEVEN_DAYS_SECONDS: u64 = 7 * 24 * 60 * 60;
const USER_AGENT: &str = "RPi-Plane-Radar/0.1 (+https://github.com/shayne/RPi-Plane-Radar)";

#[derive(Clone, Debug)]
struct FakeClock {
    state: Arc<Mutex<FakeTime>>,
}

#[derive(Debug)]
struct FakeTime {
    monotonic: Duration,
    unix_seconds: u64,
}

impl FakeClock {
    fn new(unix_seconds: u64) -> Self {
        Self {
            state: Arc::new(Mutex::new(FakeTime {
                monotonic: Duration::ZERO,
                unix_seconds,
            })),
        }
    }

    fn advance_unix(&self, seconds: u64) {
        self.state.lock().expect("time").unix_seconds += seconds;
    }
}

impl Clock for FakeClock {
    fn monotonic(&self) -> Duration {
        self.state.lock().expect("time").monotonic
    }

    fn unix_seconds(&self) -> u64 {
        self.state.lock().expect("time").unix_seconds
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
        self.sleeps.lock().expect("sleeps").clone()
    }
}

impl Sleeper for FakeSleeper {
    fn sleep(&self, duration: Duration) {
        self.sleeps.lock().expect("sleeps").push(duration);
        let mut time = self.clock.state.lock().expect("time");
        time.monotonic += duration;
        time.unix_seconds += duration.as_secs();
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

fn ok_response(body: &[u8]) -> Result<HttpResponse, HttpError> {
    Ok(HttpResponse {
        status: 200,
        body: body.to_vec(),
    })
}

fn fixture_response() -> Result<HttpResponse, HttpError> {
    ok_response(include_bytes!("fixtures/nominatim/results.json"))
}

fn one_result_response(name: &str) -> Result<HttpResponse, HttpError> {
    ok_response(
        serde_json::to_string(&serde_json::json!([{
            "lat": "40.7128",
            "lon": "-74.0060",
            "display_name": name
        }]))
        .expect("response JSON")
        .as_bytes(),
    )
}

fn geocoder(
    http: FakeHttpClient,
    clock: FakeClock,
    sleeper: FakeSleeper,
    cache_path: PathBuf,
) -> Geocoder<FakeHttpClient, FakeClock, FakeSleeper> {
    Geocoder::new(http, clock, sleeper, cache_path)
}

#[test]
fn parses_valid_results_skips_invalid_records_and_caps_at_five() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let clock = FakeClock::new(START_UNIX);
    let http = FakeHttpClient::responding(clock.clone(), [fixture_response()]);
    let sleeper = FakeSleeper::new(clock.clone());
    let mut geocoder = geocoder(
        http,
        clock,
        sleeper,
        directory.path().join("geocode-cache.json"),
    );

    let results = geocoder.search("New York").expect("search");

    assert_eq!(results.len(), 5);
    assert_eq!(results[0].display_name, "New York, New York, United States");
    assert!(results[0].location.latitude.is_finite());
    assert!(
        results
            .iter()
            .all(|result| (-90.0..=90.0).contains(&result.location.latitude))
    );
    assert!(
        results
            .iter()
            .all(|result| (-180.0..=180.0).contains(&result.location.longitude))
    );
    assert!(
        results
            .iter()
            .all(|result| !result.display_name.contains("Malformed"))
    );
    assert!(
        results
            .iter()
            .all(|result| !result.display_name.contains("Outside"))
    );
    assert!(
        results
            .iter()
            .all(|result| !result.display_name.contains("Seattle"))
    );
}

#[test]
fn missing_or_blank_display_names_are_skipped_and_html_like_names_stay_plain_data() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let clock = FakeClock::new(START_UNIX);
    let response = ok_response(
        br#"[
            {"lat":"34.0522","lon":"-118.2437","display_name":"<b>Los Angeles</b>"},
            {"lat":"47.6062","lon":"-122.3321"},
            {"lat":"51.5072","lon":"-0.1276","display_name":"   "}
        ]"#,
    );
    let http = FakeHttpClient::responding(clock.clone(), [response]);
    let sleeper = FakeSleeper::new(clock.clone());
    let mut geocoder = geocoder(
        http,
        clock,
        sleeper,
        directory.path().join("geocode-cache.json"),
    );

    let results = geocoder.search("Los Angeles").expect("search");

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].display_name, "<b>Los Angeles</b>");
    assert_eq!(results[0].location.label, "<b>Los Angeles</b>");
}

#[test]
fn blank_and_control_character_queries_are_rejected_without_io() {
    for query in ["", "  \t  ", "New\nYork", "Paris\u{0000}France"] {
        let directory = tempfile::tempdir().expect("temporary directory");
        let cache_path = directory.path().join("geocode-cache.json");
        let clock = FakeClock::new(START_UNIX);
        let http = FakeHttpClient::responding(clock.clone(), []);
        let probe = http.clone();
        let sleeper = FakeSleeper::new(clock.clone());
        let mut geocoder = geocoder(http, clock, sleeper, cache_path.clone());

        assert!(
            matches!(geocoder.search(query), Err(GeocodeError::InvalidQuery)),
            "{query:?} must be rejected"
        );
        assert_eq!(probe.request_count(), 0);
        assert!(!cache_path.exists());
    }
}

#[test]
fn request_uses_original_query_exact_parameters_user_agent_and_default_provider() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let clock = FakeClock::new(START_UNIX);
    let http = FakeHttpClient::responding(clock.clone(), [one_result_response("Mexico City")]);
    let probe = http.clone();
    let sleeper = FakeSleeper::new(clock.clone());
    let mut geocoder = geocoder(
        http,
        clock,
        sleeper,
        directory.path().join("geocode-cache.json"),
    );

    geocoder.search("  MÉXICO   City ").expect("search");

    let requests = probe.requests();
    let request = &requests[0].1;
    assert_eq!(request.url, "https://nominatim.openstreetmap.org/search");
    assert_eq!(
        request.query,
        [
            ("q".to_owned(), "  MÉXICO   City ".to_owned()),
            ("format".to_owned(), "jsonv2".to_owned()),
            ("limit".to_owned(), "5".to_owned()),
            ("addressdetails".to_owned(), "0".to_owned()),
        ]
    );
    assert_eq!(
        request.headers,
        [("User-Agent".to_owned(), USER_AGENT.to_owned())]
    );
    assert!(request.verify_tls);
}

#[test]
fn configured_provider_base_replaces_the_default_endpoint() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let clock = FakeClock::new(START_UNIX);
    let http = FakeHttpClient::responding(clock.clone(), [one_result_response("Configured")]);
    let probe = http.clone();
    let sleeper = FakeSleeper::new(clock.clone());
    let mut geocoder = Geocoder::with_provider_base(
        http,
        clock,
        sleeper,
        directory.path().join("geocode-cache.json"),
        "https://provider.example.test/custom-search".to_owned(),
    );

    geocoder.search("Configured").expect("search");

    assert_eq!(
        probe.requests()[0].1.url,
        "https://provider.example.test/custom-search"
    );
}

#[test]
fn normalized_cache_key_collapses_whitespace_lowercases_unicode_and_survives_restart() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let cache_path = directory.path().join("geocode-cache.json");
    let clock = FakeClock::new(START_UNIX);
    let first_http =
        FakeHttpClient::responding(clock.clone(), [one_result_response("Mexico City")]);
    let first_probe = first_http.clone();
    let first_sleeper = FakeSleeper::new(clock.clone());
    let mut first = geocoder(first_http, clock.clone(), first_sleeper, cache_path.clone());

    first.search("  MÉXICO   City ").expect("initial search");
    assert_eq!(first_probe.request_count(), 1);

    let cache_text = fs::read_to_string(&cache_path).expect("cache JSON");
    assert!(cache_text.contains("\"méxico city\""));
    assert!(!cache_text.contains("  MÉXICO   City "));

    let cached_http = FakeHttpClient::responding(clock.clone(), []);
    let cached_probe = cached_http.clone();
    let cached_sleeper = FakeSleeper::new(clock.clone());
    let mut restarted = geocoder(cached_http, clock, cached_sleeper, cache_path);

    let results = restarted.search("méxico city").expect("cached search");

    assert_eq!(results[0].display_name, "Mexico City");
    assert_eq!(cached_probe.request_count(), 0);
}

#[test]
fn successful_cache_entry_expires_after_exactly_seven_days() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let cache_path = directory.path().join("geocode-cache.json");
    let clock = FakeClock::new(START_UNIX);
    let http = FakeHttpClient::responding(
        clock.clone(),
        [
            one_result_response("First"),
            one_result_response("Refreshed"),
        ],
    );
    let probe = http.clone();
    let sleeper = FakeSleeper::new(clock.clone());
    let mut geocoder = geocoder(http, clock.clone(), sleeper, cache_path.clone());

    geocoder.search("New York").expect("initial search");
    let cache: serde_json::Value =
        serde_json::from_slice(&fs::read(cache_path).expect("cache JSON")).expect("valid cache");
    assert_eq!(cache["schema_version"], 1);
    assert_eq!(
        cache["entries"]["new york"]["expires_at_unix"],
        START_UNIX + SEVEN_DAYS_SECONDS
    );
    assert_eq!(
        cache["entries"]["new york"]["results"][0]["display_name"],
        "First"
    );

    clock.advance_unix(SEVEN_DAYS_SECONDS - 1);
    assert_eq!(
        geocoder.search("NEW YORK").expect("unexpired cache")[0].display_name,
        "First"
    );
    assert_eq!(probe.request_count(), 1);

    clock.advance_unix(1);
    assert_eq!(
        geocoder.search("new york").expect("expired refresh")[0].display_name,
        "Refreshed"
    );
    assert_eq!(probe.request_count(), 2);
}

#[test]
fn cache_ttl_starts_after_rate_limit_wait_and_successful_lookup() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let cache_path = directory.path().join("geocode-cache.json");
    let clock = FakeClock::new(START_UNIX);
    let http = FakeHttpClient::responding(
        clock.clone(),
        [
            one_result_response("New York"),
            one_result_response("Boston"),
        ],
    );
    let sleeper = FakeSleeper::new(clock.clone());
    let mut geocoder = geocoder(http, clock, sleeper, cache_path.clone());

    geocoder.search("New York").expect("first miss");
    geocoder.search("Boston").expect("rate-limited miss");

    let cache: serde_json::Value =
        serde_json::from_slice(&fs::read(cache_path).expect("cache JSON")).expect("valid cache");
    assert_eq!(
        cache["entries"]["boston"]["expires_at_unix"],
        START_UNIX + 1 + SEVEN_DAYS_SECONDS
    );
}

#[test]
fn invalid_loaded_cache_records_are_rejected_before_they_can_be_served() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let cache_path = directory.path().join("geocode-cache.json");
    fs::write(
        &cache_path,
        format!(
            concat!(
                "{{\n",
                "  \"schema_version\": 1,\n",
                "  \"entries\": {{\n",
                "    \"new york\": {{\n",
                "      \"results\": [{{\n",
                "        \"display_name\": \"Invalid cached result\",\n",
                "        \"location\": {{\"latitude\": 91.0, \"longitude\": -74.006, \"label\": \"Invalid cached result\"}}\n",
                "      }}],\n",
                "      \"expires_at_unix\": {}\n",
                "    }}\n",
                "  }}\n",
                "}}\n"
            ),
            START_UNIX + SEVEN_DAYS_SECONDS
        ),
    )
    .expect("invalid cache fixture");
    let clock = FakeClock::new(START_UNIX);
    let http = FakeHttpClient::responding(clock.clone(), []);
    let probe = http.clone();
    let sleeper = FakeSleeper::new(clock.clone());
    let mut geocoder = geocoder(http, clock, sleeper, cache_path);

    assert!(matches!(
        geocoder.search("New York"),
        Err(GeocodeError::Cache(_))
    ));
    assert_eq!(probe.request_count(), 0);
}

#[test]
fn distinct_cache_misses_start_at_least_one_point_zero_five_seconds_apart() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let clock = FakeClock::new(START_UNIX);
    let http = FakeHttpClient::responding(
        clock.clone(),
        [
            one_result_response("New York"),
            one_result_response("Boston"),
        ],
    );
    let probe = http.clone();
    let sleeper = FakeSleeper::new(clock.clone());
    let sleeper_probe = sleeper.clone();
    let mut geocoder = geocoder(
        http,
        clock,
        sleeper,
        directory.path().join("geocode-cache.json"),
    );

    geocoder.search("New York").expect("first miss");
    geocoder.search("Boston").expect("second miss");

    let requests = probe.requests();
    assert_eq!(requests.len(), 2);
    assert!(requests[1].0.saturating_sub(requests[0].0) >= Duration::from_millis(1050));
    assert_eq!(sleeper_probe.sleeps(), [Duration::from_millis(1050)]);
}

#[test]
fn timeout_status_malformed_json_and_empty_results_leave_persistent_cache_unchanged() {
    let failures = [
        ("timeout", Err(HttpError::Timeout), Some("http")),
        (
            "status",
            Ok(HttpResponse {
                status: 503,
                body: b"service unavailable".to_vec(),
            }),
            Some("status"),
        ),
        ("malformed JSON", ok_response(b"{"), Some("json")),
        ("empty results", ok_response(b"[]"), None),
    ];

    for (name, failed_response, expected_error) in failures {
        let directory = tempfile::tempdir().expect("temporary directory");
        let cache_path = directory.path().join("geocode-cache.json");
        let clock = FakeClock::new(START_UNIX);
        let http = FakeHttpClient::responding(
            clock.clone(),
            [one_result_response("Seed"), failed_response],
        );
        let sleeper = FakeSleeper::new(clock.clone());
        let mut geocoder = geocoder(http, clock, sleeper, cache_path.clone());

        geocoder.search("seed query").expect("seed cache");
        let before = fs::read(&cache_path).expect("seed cache bytes");
        let outcome = geocoder.search("distinct failure");

        match expected_error {
            Some("http") => assert!(
                matches!(outcome, Err(GeocodeError::Http(HttpError::Timeout))),
                "{name}"
            ),
            Some("status") => assert!(matches!(outcome, Err(GeocodeError::Status(503))), "{name}"),
            Some("json") => assert!(matches!(outcome, Err(GeocodeError::Json(_))), "{name}"),
            None => assert!(outcome.expect(name).is_empty(), "{name}"),
            Some(other) => panic!("unexpected error category {other}"),
        }
        assert_eq!(
            fs::read(&cache_path).expect("cache after failure"),
            before,
            "{name} must not mutate persistent cache"
        );
    }
}

#[test]
fn atomic_cache_persist_failure_preserves_the_destination_directory() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let cache_path = directory.path().join("geocode-cache.json");
    fs::create_dir(&cache_path).expect("directory destination");
    fs::write(cache_path.join("keep"), "do not replace").expect("destination fixture");
    let clock = FakeClock::new(START_UNIX);
    let http = FakeHttpClient::responding(clock.clone(), [one_result_response("New York")]);
    let sleeper = FakeSleeper::new(clock.clone());
    let mut geocoder = geocoder(http, clock, sleeper, cache_path.clone());

    assert!(matches!(
        geocoder.search("New York"),
        Err(GeocodeError::Cache(_))
    ));
    assert!(cache_path.is_dir());
    assert_eq!(
        fs::read(cache_path.join("keep")).expect("destination fixture"),
        b"do not replace"
    );
}

#[derive(Debug)]
struct CaptureLogger {
    messages: Mutex<Vec<String>>,
}

impl Log for CaptureLogger {
    fn enabled(&self, _metadata: &Metadata<'_>) -> bool {
        true
    }

    fn log(&self, record: &Record<'_>) {
        self.messages
            .lock()
            .expect("captured logs")
            .push(record.args().to_string());
    }

    fn flush(&self) {}
}

fn capture_logger() -> &'static CaptureLogger {
    static LOGGER: OnceLock<CaptureLogger> = OnceLock::new();
    let logger = LOGGER.get_or_init(|| CaptureLogger {
        messages: Mutex::new(Vec::new()),
    });
    let _ = log::set_logger(logger);
    log::set_max_level(LevelFilter::Trace);
    logger.messages.lock().expect("captured logs").clear();
    logger
}

#[test]
fn logs_never_contain_raw_or_normalized_query_or_result_text() {
    let logger = capture_logger();
    let directory = tempfile::tempdir().expect("temporary directory");
    let clock = FakeClock::new(START_UNIX);
    let http =
        FakeHttpClient::responding(clock.clone(), [one_result_response("PRIVATE RESULT TEXT")]);
    let sleeper = FakeSleeper::new(clock.clone());
    let mut geocoder = geocoder(
        http,
        clock,
        sleeper,
        directory.path().join("geocode-cache.json"),
    );

    geocoder
        .search("  PRIVATE   Query ")
        .expect("private search");

    let logs = logger.messages.lock().expect("captured logs").join("\n");
    for sensitive in ["  PRIVATE   Query ", "private query", "PRIVATE RESULT TEXT"] {
        assert!(
            !logs.contains(sensitive),
            "logs leaked sensitive geocoding text"
        );
    }
}

use std::collections::VecDeque;
use std::fs;
use std::os::unix::fs::{MetadataExt, symlink};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use planeradar::http::{HttpClient, HttpError, HttpRequest, HttpResponse};
use planeradar::model::Location;
use planeradar::solar::{
    SolarClient, SolarDay, SolarErrorCategory, SolarSchedule, load_cache, save_cache,
};
use serde_json::{Value, json};

const FETCHED_AT: u64 = 1_785_711_600;

#[derive(Clone, Debug)]
struct RecordingHttp {
    state: Arc<Mutex<FakeState>>,
}

#[derive(Debug)]
struct FakeState {
    responses: VecDeque<Result<HttpResponse, HttpError>>,
    requests: Vec<HttpRequest>,
}

impl RecordingHttp {
    fn responding(response: Result<HttpResponse, HttpError>) -> Self {
        Self {
            state: Arc::new(Mutex::new(FakeState {
                responses: VecDeque::from([response]),
                requests: Vec::new(),
            })),
        }
    }

    fn requests(&self) -> Vec<HttpRequest> {
        self.state.lock().expect("fake state").requests.clone()
    }
}

impl HttpClient for RecordingHttp {
    fn execute(&self, request: HttpRequest) -> Result<HttpResponse, HttpError> {
        let mut state = self.state.lock().expect("fake state");
        state.requests.push(request);
        state.responses.pop_front().expect("fake response")
    }
}

fn location() -> Location {
    Location {
        latitude: 40.7769,
        longitude: -73.8740,
        label: "LaGuardia Airport".to_owned(),
    }
}

fn fixture_value() -> Value {
    serde_json::from_slice(include_bytes!("fixtures/open_meteo/solar.json")).expect("solar fixture")
}

fn response(value: &Value) -> Result<HttpResponse, HttpError> {
    Ok(HttpResponse {
        status: 200,
        body: serde_json::to_vec(value).expect("response JSON"),
    })
}

fn raw_response(status: u16, body: &[u8]) -> Result<HttpResponse, HttpError> {
    Ok(HttpResponse {
        status,
        body: body.to_vec(),
    })
}

fn client(
    response: Result<HttpResponse, HttpError>,
) -> (SolarClient<RecordingHttp>, RecordingHttp) {
    let http = RecordingHttp::responding(response);
    let probe = http.clone();
    (
        SolarClient::with_provider_base(
            http,
            "https://api.open-meteo.test/v1/forecast///".to_owned(),
        ),
        probe,
    )
}

fn fetch_value(value: &Value) -> Result<SolarSchedule, planeradar::solar::SolarError> {
    client(response(value)).0.fetch(&location(), FETCHED_AT)
}

fn schedule() -> SolarSchedule {
    fetch_value(&fixture_value()).expect("valid solar fixture")
}

fn error_text(error: &planeradar::solar::SolarError) -> String {
    error.to_string()
}

fn assert_category(
    result: Result<SolarSchedule, planeradar::solar::SolarError>,
    expected: SolarErrorCategory,
) -> planeradar::solar::SolarError {
    let error = result.expect_err("request must fail");
    assert_eq!(error.category(), expected);
    error
}

fn write_json(path: &Path, value: &Value) {
    fs::write(path, serde_json::to_vec_pretty(value).expect("cache JSON")).expect("write cache");
}

#[test]
fn fixture_maps_seventeen_days_and_preserves_the_exact_requested_location_identity() {
    let schedule = schedule();

    assert_eq!(schedule.schema_version, 1);
    assert_eq!(schedule.latitude, 40.7769);
    assert_eq!(schedule.longitude, -73.8740);
    assert_eq!(schedule.time_zone, "America/New_York");
    assert_eq!(schedule.fetched_at_unix, FETCHED_AT);
    assert_eq!(schedule.days.len(), 17);
    assert_eq!(
        schedule.days[0],
        SolarDay {
            date: "2026-08-02".to_owned(),
            sunrise_unix: Some(1_785_664_800),
        }
    );
    assert_eq!(schedule.days[8].date, "2026-08-10");
    assert_eq!(schedule.days[8].sunrise_unix, None);
    assert_eq!(schedule.days[16].date, "2026-08-18");
}

#[test]
fn fetch_emits_only_the_bounded_https_solar_request() {
    let (client, http) = client(response(&fixture_value()));

    client
        .fetch(&location(), FETCHED_AT)
        .expect("solar schedule");

    let requests = http.requests();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_eq!(request.url, "https://api.open-meteo.test/v1/forecast");
    assert_eq!(
        request.query,
        vec![
            ("latitude".to_owned(), "40.7769".to_owned()),
            ("longitude".to_owned(), "-73.8740".to_owned()),
            ("daily".to_owned(), "sunrise".to_owned()),
            ("timezone".to_owned(), "auto".to_owned()),
            ("timeformat".to_owned(), "unixtime".to_owned()),
            ("past_days".to_owned(), "1".to_owned()),
            ("forecast_days".to_owned(), "16".to_owned()),
        ]
    );
    assert!(request.headers.is_empty());
    assert!(request.verify_tls);
    assert_eq!(request.connect_timeout, Duration::from_secs(2));
    assert_eq!(request.read_timeout, Duration::from_secs(4));
    assert_eq!(
        request.connect_timeout + request.read_timeout,
        Duration::from_secs(6)
    );
    assert_eq!(request.max_response_bytes, 64 * 1024);
    assert!(
        request
            .query
            .iter()
            .all(|(name, _)| name != "current" && !name.starts_with("current_"))
    );
}

#[test]
fn invalid_request_coordinates_are_rejected_before_network_access() {
    let invalid = [
        (f64::NAN, -73.8740),
        (f64::INFINITY, -73.8740),
        (-90.000_1, -73.8740),
        (90.000_1, -73.8740),
        (40.7769, f64::NEG_INFINITY),
        (40.7769, -180.000_1),
        (40.7769, 180.000_1),
    ];

    for (latitude, longitude) in invalid {
        let (client, http) = client(response(&fixture_value()));
        let mut location = location();
        location.latitude = latitude;
        location.longitude = longitude;
        let error = client
            .fetch(&location, FETCHED_AT)
            .expect_err("invalid coordinates");
        assert_eq!(error.category(), SolarErrorCategory::InvalidLocation);
        assert!(http.requests().is_empty());
        let display = error_text(&error);
        assert!(!display.contains(&latitude.to_string()));
        assert!(!display.contains(&longitude.to_string()));
    }
}

#[test]
fn invalid_provider_coordinates_are_schema_errors() {
    for (field, value) in [
        ("latitude", json!(null)),
        ("latitude", json!(91.0)),
        ("longitude", json!(-181.0)),
        ("longitude", json!("NaN")),
    ] {
        let mut fixture = fixture_value();
        fixture[field] = value;
        assert_category(fetch_value(&fixture), SolarErrorCategory::Schema);
    }
}

#[test]
fn response_requires_exactly_seventeen_unique_canonical_matching_days() {
    let mut short = fixture_value();
    short["daily"]["time"].as_array_mut().expect("times").pop();
    short["daily"]["sunrise"]
        .as_array_mut()
        .expect("sunrises")
        .pop();
    assert_category(fetch_value(&short), SolarErrorCategory::Schema);

    let mut unequal = fixture_value();
    unequal["daily"]["sunrise"]
        .as_array_mut()
        .expect("sunrises")
        .pop();
    assert_category(fetch_value(&unequal), SolarErrorCategory::Schema);

    let mut duplicate = fixture_value();
    duplicate["daily"]["time"][1] = json!("2026-08-02");
    assert_category(fetch_value(&duplicate), SolarErrorCategory::Schema);

    let mut noncanonical = fixture_value();
    noncanonical["daily"]["time"][1] = json!("2026-8-3");
    assert_category(fetch_value(&noncanonical), SolarErrorCategory::Schema);

    let mut mismatched_sunrise_date = fixture_value();
    mismatched_sunrise_date["daily"]["sunrise"][1] = json!(1_785_664_800);
    assert_category(
        fetch_value(&mismatched_sunrise_date),
        SolarErrorCategory::Schema,
    );
}

#[test]
fn sunrise_values_are_only_checked_i64_integers_or_null() {
    for value in [json!(1785751200.0), json!("1785751200"), json!({})] {
        let mut fixture = fixture_value();
        fixture["daily"]["sunrise"][1] = value;
        assert_category(fetch_value(&fixture), SolarErrorCategory::Schema);
    }

    let mut outside_jiff_range = fixture_value();
    outside_jiff_range["daily"]["sunrise"][1] = json!(i64::MAX);
    assert_category(fetch_value(&outside_jiff_range), SolarErrorCategory::Schema);
}

#[test]
fn time_zone_must_be_bounded_iana_syntax_with_available_system_zoneinfo() {
    for zone in [
        "",
        "/America/New_York",
        "America//New_York",
        "America/../New_York",
        "Etc/GMT 4",
        "A/this_zone_does_not_exist",
    ] {
        let mut fixture = fixture_value();
        fixture["timezone"] = json!(zone);
        let error = assert_category(fetch_value(&fixture), SolarErrorCategory::Schema);
        if !zone.is_empty() {
            assert!(!error_text(&error).contains(zone));
        }
    }

    let long_zone = format!("America/{}", "A".repeat(128));
    let mut fixture = fixture_value();
    fixture["timezone"] = json!(long_zone.clone());
    let error = assert_category(fetch_value(&fixture), SolarErrorCategory::Schema);
    assert!(!error_text(&error).contains(&long_zone));
}

#[test]
fn strict_wire_schema_rejects_unknown_fields_and_wrong_units() {
    let mut top_level = fixture_value();
    top_level["provider_message"] = json!("PRIVATE PROVIDER BODY");
    assert_category(fetch_value(&top_level), SolarErrorCategory::Schema);

    let mut daily = fixture_value();
    daily["daily"]["sunset"] = json!([]);
    assert_category(fetch_value(&daily), SolarErrorCategory::Schema);

    for (field, value) in [("time", "unixtime"), ("sunrise", "iso8601")] {
        let mut units = fixture_value();
        units["daily_units"][field] = json!(value);
        assert_category(fetch_value(&units), SolarErrorCategory::Schema);
    }
}

#[test]
fn transport_status_json_and_schema_failures_have_sanitized_categories() {
    let cases = [
        (
            Err(HttpError::TlsVerificationRequired),
            SolarErrorCategory::Tls,
        ),
        (Err(HttpError::Timeout), SolarErrorCategory::Timeout),
        (Err(HttpError::Transport), SolarErrorCategory::Transport),
        (Err(HttpError::Body), SolarErrorCategory::Body),
        (
            Err(HttpError::BodyTooLarge),
            SolarErrorCategory::BodyTooLarge,
        ),
    ];
    for (response, category) in cases {
        let (client, _) = client(response);
        let error = assert_category(client.fetch(&location(), FETCHED_AT), category);
        assert!(!error_text(&error).contains("40.7769"));
        assert!(!error_text(&error).contains("-73.874"));
    }

    let provider_body = b"PRIVATE PROVIDER BODY America/Invalid_Private_Zone";
    let (status_client, _) = client(raw_response(503, provider_body));
    let status = assert_category(
        status_client.fetch(&location(), FETCHED_AT),
        SolarErrorCategory::Status,
    );
    assert!(!error_text(&status).contains("PRIVATE PROVIDER BODY"));

    let (json_client, _) = client(raw_response(200, b"{PRIVATE PROVIDER BODY"));
    let json = assert_category(
        json_client.fetch(&location(), FETCHED_AT),
        SolarErrorCategory::Json,
    );
    assert!(!error_text(&json).contains("PRIVATE PROVIDER BODY"));

    let mut invalid_schema = fixture_value();
    invalid_schema["daily"] = json!("PRIVATE PROVIDER BODY");
    let schema = assert_category(fetch_value(&invalid_schema), SolarErrorCategory::Schema);
    assert!(!error_text(&schema).contains("PRIVATE PROVIDER BODY"));
}

#[test]
fn cache_reuses_only_exact_coordinates_and_ignores_the_location_label() {
    let temporary = tempfile::tempdir().expect("state directory");
    let path = temporary.path().join("solar-schedule.json");
    let expected = schedule();
    save_cache(&path, &expected).expect("save cache");

    let mut renamed = location();
    renamed.label = "A completely different display label".to_owned();
    assert_eq!(load_cache(&path, &renamed), Some(expected.clone()));

    for (latitude, longitude) in [
        (
            f64::from_bits(location().latitude.to_bits() + 1),
            location().longitude,
        ),
        (
            location().latitude,
            f64::from_bits(location().longitude.to_bits() + 1),
        ),
    ] {
        let mut mismatch = location();
        mismatch.latitude = latitude;
        mismatch.longitude = longitude;
        assert_eq!(load_cache(&path, &mismatch), None);
    }
}

#[test]
fn cache_rejects_wrong_schema_corruption_unknown_fields_and_invalid_records() {
    let temporary = tempfile::tempdir().expect("state directory");
    let path = temporary.path().join("solar-schedule.json");
    let valid = serde_json::to_value(schedule()).expect("schedule value");

    for value in [
        json!({"schema_version": 2}),
        {
            let mut value = valid.clone();
            value["unknown"] = json!(true);
            value
        },
        {
            let mut value = valid.clone();
            value["days"][0]["unknown"] = json!(true);
            value
        },
        {
            let mut value = valid.clone();
            value["days"][1]["date"] = json!("2026-08-02");
            value
        },
        {
            let mut value = valid.clone();
            value["time_zone"] = json!("A/not_available");
            value
        },
    ] {
        write_json(&path, &value);
        assert_eq!(load_cache(&path, &location()), None);
    }

    fs::write(&path, b"{corrupt").expect("corrupt cache");
    assert_eq!(load_cache(&path, &location()), None);
}

#[test]
fn cache_loader_rejects_symlinks_and_nonregular_paths() {
    let temporary = tempfile::tempdir().expect("state directory");
    let real = temporary.path().join("real.json");
    let linked = temporary.path().join("linked.json");
    save_cache(&real, &schedule()).expect("real cache");
    symlink(&real, &linked).expect("cache symlink");
    assert_eq!(load_cache(&linked, &location()), None);

    let directory = temporary.path().join("directory-cache");
    fs::create_dir(&directory).expect("directory cache");
    assert_eq!(load_cache(&directory, &location()), None);
}

#[test]
fn save_atomically_replaces_with_a_synced_complete_file_and_directory_entry() {
    let temporary = tempfile::tempdir().expect("state directory");
    let parent = temporary.path().join("state");
    let path = parent.join("solar-schedule.json");
    let mut first = schedule();
    first.fetched_at_unix = 100;
    save_cache(&path, &first).expect("first save");
    let first_inode = fs::metadata(&path).expect("first metadata").ino();

    let mut replacement = schedule();
    replacement.fetched_at_unix = 200;
    replacement.days[0].sunrise_unix = None;
    save_cache(&path, &replacement).expect("replacement save");

    let metadata = fs::metadata(&path).expect("replacement metadata");
    assert!(metadata.is_file());
    assert_ne!(
        metadata.ino(),
        first_inode,
        "replacement must use rename, not truncate"
    );
    assert_eq!(load_cache(&path, &location()), Some(replacement));
    let entries = fs::read_dir(&parent)
        .expect("state entries")
        .map(|entry| entry.expect("entry").file_name())
        .collect::<Vec<_>>();
    assert_eq!(entries, ["solar-schedule.json"]);
}

#[test]
fn invalid_fetch_preserves_a_good_cache_and_creates_no_temporary_write() {
    let temporary = tempfile::tempdir().expect("state directory");
    let path = temporary.path().join("solar-schedule.json");
    let good = schedule();
    save_cache(&path, &good).expect("good cache");
    let before = fs::read(&path).expect("good bytes");

    let provider_body = b"PRIVATE PROVIDER BODY";
    let (client, _) = client(raw_response(502, provider_body));
    assert_category(
        client.fetch(&location(), FETCHED_AT + 60),
        SolarErrorCategory::Status,
    );

    assert_eq!(fs::read(&path).expect("cache after fetch"), before);
    assert_eq!(load_cache(&path, &location()), Some(good));
    assert_eq!(fs::read_dir(temporary.path()).expect("entries").count(), 1);
}

#[test]
fn invalid_schedule_is_rejected_before_creating_or_replacing_cache_state() {
    let temporary = tempfile::tempdir().expect("state directory");
    let parent = temporary.path().join("not-created");
    let path = parent.join("solar-schedule.json");
    let mut invalid = schedule();
    invalid.time_zone = "PRIVATE/Invalid_Zone".to_owned();

    let error = save_cache(&path, &invalid).expect_err("invalid schedule");
    assert_eq!(error.category(), SolarErrorCategory::Schema);
    assert!(!error_text(&error).contains("PRIVATE/Invalid_Zone"));
    assert!(!parent.exists());

    let good_path = temporary.path().join("good.json");
    let good = schedule();
    save_cache(&good_path, &good).expect("good cache");
    let before = fs::read(&good_path).expect("good bytes");
    assert!(save_cache(&good_path, &invalid).is_err());
    assert_eq!(fs::read(&good_path).expect("preserved bytes"), before);
}

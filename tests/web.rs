use std::collections::HashSet;
use std::io::{self, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use planeradar::geocode::{GeocodeError, GeocodeResult, GeocodeService};
use planeradar::model::{
    AppState, ClockFormat, FooterSettings, Location, RadarSettings, TemperatureUnit, TimeZone,
    Units,
};
use planeradar::web::{HealthSnapshot, HealthSource, SettingsServer, SettingsService, WebError};

const SESSION_COOKIE: &str = "planeradar_session";
const LOCAL_URL: &str = "http://hangar-2.local";
const LOCAL_HOST: &str = "hangar-2.local";

#[derive(Clone)]
struct TestSettings {
    current: Arc<Mutex<RadarSettings>>,
    replacements: Arc<Mutex<Vec<RadarSettings>>>,
    fail_replacements: bool,
}

impl TestSettings {
    fn new(current: RadarSettings) -> Self {
        Self {
            current: Arc::new(Mutex::new(current)),
            replacements: Arc::new(Mutex::new(Vec::new())),
            fail_replacements: false,
        }
    }

    fn failing(current: RadarSettings) -> Self {
        Self {
            current: Arc::new(Mutex::new(current)),
            replacements: Arc::new(Mutex::new(Vec::new())),
            fail_replacements: true,
        }
    }

    fn replacement_count(&self) -> usize {
        self.replacements.lock().unwrap().len()
    }
}

impl SettingsService for TestSettings {
    fn current(&self) -> RadarSettings {
        self.current.lock().unwrap().clone()
    }

    fn replace(&self, candidate: RadarSettings) -> Result<(), WebError> {
        if self.fail_replacements {
            return Err(WebError::Settings);
        }
        self.replacements.lock().unwrap().push(candidate.clone());
        *self.current.lock().unwrap() = candidate;
        Ok(())
    }
}

struct TestGeocoder {
    results: Vec<GeocodeResult>,
    queries: Arc<Mutex<Vec<String>>>,
}

struct FailingGeocoder;

impl GeocodeService for FailingGeocoder {
    fn search(&mut self, _query: &str) -> Result<Vec<GeocodeResult>, GeocodeError> {
        Err(GeocodeError::InvalidQuery)
    }
}

impl GeocodeService for TestGeocoder {
    fn search(&mut self, query: &str) -> Result<Vec<GeocodeResult>, GeocodeError> {
        self.queries.lock().unwrap().push(query.to_owned());
        Ok(self.results.clone())
    }
}

struct TestHealth;

impl HealthSource for TestHealth {
    fn health(&self) -> HealthSnapshot {
        HealthSnapshot {
            configured: true,
            state: AppState::Radar,
            data_stale: false,
            revision: "test-revision",
        }
    }
}

#[derive(Debug)]
struct HttpResponse {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}

struct WireBody<'a> {
    bytes: &'a [u8],
    has_content_length: bool,
}

impl HttpResponse {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(candidate, _)| candidate.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

#[derive(Clone)]
struct Session {
    cookie: String,
    csrf: String,
}

struct TestServer {
    address: SocketAddr,
    settings: TestSettings,
    queries: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl TestServer {
    fn new(initial: RadarSettings, results: Vec<GeocodeResult>) -> Self {
        let queries = Arc::new(Mutex::new(Vec::new()));
        let geocoder: Box<dyn GeocodeService> = Box::new(TestGeocoder {
            results,
            queries: queries.clone(),
        });
        Self::start(TestSettings::new(initial), geocoder, queries)
    }

    fn with_failing_geocoder(initial: RadarSettings) -> Self {
        Self::start(
            TestSettings::new(initial),
            Box::new(FailingGeocoder),
            Arc::new(Mutex::new(Vec::new())),
        )
    }

    fn with_failing_settings(initial: RadarSettings) -> Self {
        let queries = Arc::new(Mutex::new(Vec::new()));
        let geocoder: Box<dyn GeocodeService> = Box::new(TestGeocoder {
            results: Vec::new(),
            queries: queries.clone(),
        });
        Self::start(TestSettings::failing(initial), geocoder, queries)
    }

    fn start(
        settings: TestSettings,
        geocoder: Box<dyn GeocodeService>,
        queries: Arc<Mutex<Vec<String>>>,
    ) -> Self {
        let address = reserve_address();
        let allowed_address = address;
        let allowed_hosts = Arc::new(move || {
            HashSet::from([
                LOCAL_HOST.to_owned(),
                format!("127.0.0.1:{}", allowed_address.port()),
            ])
        });
        let server = Arc::new(
            SettingsServer::bind(
                address,
                Arc::new(settings.clone()),
                Arc::new(Mutex::new(geocoder)),
                Arc::new(TestHealth),
                LOCAL_URL.to_owned(),
                allowed_hosts,
            )
            .unwrap(),
        );
        let stop = Arc::new(AtomicBool::new(false));
        let server_for_thread = server.clone();
        let stop_for_thread = stop.clone();
        let thread = thread::spawn(move || {
            server_for_thread.run(&stop_for_thread).unwrap();
        });

        Self {
            address,
            settings,
            queries,
            stop,
            thread: Some(thread),
        }
    }

    fn allowed_host(&self) -> String {
        format!("127.0.0.1:{}", self.address.port())
    }

    fn current_ip_origin(&self) -> String {
        format!("http://{}", self.allowed_host())
    }

    fn get(&self, path: &str) -> HttpResponse {
        self.request("GET", path, &self.allowed_host(), Vec::new(), &[], true)
    }

    fn session(&self) -> Session {
        let response = self.get("/");
        assert_eq!(response.status, 200);
        let set_cookie = response.header("set-cookie").unwrap();
        let cookie = set_cookie.split(';').next().unwrap().to_owned();
        let csrf = extract_attribute(&response.body, "name=\"csrf_token\" value=\"");
        Session { cookie, csrf }
    }

    fn post_form(
        &self,
        path: &str,
        fields: &[(&str, &str)],
        session: &Session,
        origin: Option<&str>,
        referer: Option<&str>,
    ) -> HttpResponse {
        let mut serializer = url::form_urlencoded::Serializer::new(String::new());
        serializer.append_pair("csrf_token", &session.csrf);
        for (name, value) in fields {
            serializer.append_pair(name, value);
        }
        let body = serializer.finish();
        let mut headers = vec![
            (
                "Content-Type".to_owned(),
                "application/x-www-form-urlencoded".to_owned(),
            ),
            ("Cookie".to_owned(), session.cookie.clone()),
        ];
        if let Some(origin) = origin {
            headers.push(("Origin".to_owned(), origin.to_owned()));
        }
        if let Some(referer) = referer {
            headers.push(("Referer".to_owned(), referer.to_owned()));
        }
        self.request(
            "POST",
            path,
            &self.allowed_host(),
            headers,
            body.as_bytes(),
            true,
        )
    }

    fn request(
        &self,
        method: &str,
        path: &str,
        host: &str,
        headers: Vec<(String, String)>,
        wire_body: &[u8],
        add_content_length: bool,
    ) -> HttpResponse {
        self.request_with_timeout(
            method,
            path,
            host,
            headers,
            WireBody {
                bytes: wire_body,
                has_content_length: add_content_length,
            },
            Duration::from_secs(3),
        )
        .unwrap()
    }

    fn get_with_timeout(&self, path: &str, timeout: Duration) -> io::Result<HttpResponse> {
        self.request_with_timeout(
            "GET",
            path,
            &self.allowed_host(),
            Vec::new(),
            WireBody {
                bytes: &[],
                has_content_length: true,
            },
            timeout,
        )
    }

    fn request_with_timeout(
        &self,
        method: &str,
        path: &str,
        host: &str,
        headers: Vec<(String, String)>,
        wire_body: WireBody<'_>,
        timeout: Duration,
    ) -> io::Result<HttpResponse> {
        let mut request =
            format!("{method} {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n")
                .into_bytes();
        for (name, value) in headers {
            request.extend_from_slice(format!("{name}: {value}\r\n").as_bytes());
        }
        if wire_body.has_content_length {
            request.extend_from_slice(
                format!("Content-Length: {}\r\n", wire_body.bytes.len()).as_bytes(),
            );
        }
        request.extend_from_slice(b"\r\n");
        request.extend_from_slice(wire_body.bytes);

        let mut stream = TcpStream::connect(self.address)?;
        stream.set_read_timeout(Some(timeout))?;
        stream.write_all(&request)?;
        stream.shutdown(Shutdown::Write)?;
        let mut response = Vec::new();
        stream.read_to_end(&mut response)?;
        Ok(parse_response(&response))
    }

    fn begin_incomplete_settings_post(&self, session: &Session) -> TcpStream {
        let full_body = format!(
            "csrf_token={}&latitude=40.7&longitude=-74.0&padding={}",
            session.csrf,
            "x".repeat(2_048)
        );
        let partial_body = &full_body.as_bytes()[..32];
        let request = format!(
            "POST /settings HTTP/1.1\r\nHost: {}\r\nContent-Type: application/x-www-form-urlencoded\r\nCookie: {}\r\nOrigin: {}\r\nContent-Length: {}\r\n\r\n",
            self.allowed_host(),
            session.cookie,
            self.current_ip_origin(),
            full_body.len(),
        );
        let mut stream = TcpStream::connect(self.address).unwrap();
        stream.write_all(request.as_bytes()).unwrap();
        stream.write_all(partial_body).unwrap();
        stream.flush().unwrap();
        stream
    }

    fn stop_is_observed_within(&mut self, timeout: Duration) -> bool {
        self.stop.store(true, Ordering::Release);
        let deadline = Instant::now() + timeout;
        while self
            .thread
            .as_ref()
            .is_some_and(|thread| !thread.is_finished())
            && Instant::now() < deadline
        {
            thread::sleep(Duration::from_millis(5));
        }
        if self
            .thread
            .as_ref()
            .is_some_and(|thread| thread.is_finished())
        {
            self.thread.take().unwrap().join().unwrap();
            true
        } else {
            false
        }
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            thread.join().unwrap();
        }
    }
}

fn reserve_address() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap()
}

fn parse_response(bytes: &[u8]) -> HttpResponse {
    let split = bytes
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .unwrap();
    let head = String::from_utf8(bytes[..split].to_vec()).unwrap();
    let mut lines = head.split("\r\n");
    let status = lines
        .next()
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    let headers = lines
        .map(|line| {
            let (name, value) = line.split_once(':').unwrap();
            (name.to_owned(), value.trim().to_owned())
        })
        .collect();
    let body = String::from_utf8(bytes[split + 4..].to_vec()).unwrap();
    HttpResponse {
        status,
        headers,
        body,
    }
}

fn extract_attribute(body: &str, prefix: &str) -> String {
    let start = body.find(prefix).unwrap() + prefix.len();
    let end = body[start..].find('"').unwrap() + start;
    body[start..end].to_owned()
}

fn configured_settings() -> RadarSettings {
    RadarSettings {
        location: Some(Location {
            latitude: 51.5072,
            longitude: -0.1276,
            label: "Old location".to_owned(),
        }),
        units: Units::Miles,
        show_runways: false,
        range_index: 3,
        ..RadarSettings::default()
    }
}

fn optional_settings_enabled() -> RadarSettings {
    RadarSettings {
        location: Some(Location {
            latitude: 51.5072,
            longitude: -0.1276,
            label: "Old location".to_owned(),
        }),
        units: Units::Miles,
        show_runways: true,
        range_index: 3,
        show_callsign: true,
        show_route: true,
        show_expanded_model: true,
        radar_text_scale_percent: 130,
        minimum_altitude_feet: Some(1_000),
        maximum_altitude_feet: Some(45_000),
        footer: FooterSettings {
            show_condition: true,
            show_temperature: true,
            show_humidity: true,
            show_time: true,
            show_date: true,
            temperature_unit: TemperatureUnit::Fahrenheit,
            time_zone: TimeZone::Zulu,
            clock_format: ClockFormat::Twelve,
        },
        ..RadarSettings::default()
    }
}

fn geocode_result(display_name: &str) -> GeocodeResult {
    GeocodeResult {
        display_name: display_name.to_owned(),
        location: Location {
            latitude: 40.7128,
            longitude: -74.006,
            label: display_name.to_owned(),
        },
    }
}

#[test]
fn page_exposes_local_settings_without_wifi_or_browser_geolocation() {
    let server = TestServer::new(configured_settings(), Vec::new());

    let response = server.get("/");

    assert_eq!(response.status, 200);
    assert_eq!(
        response.header("content-type"),
        Some("text/html; charset=utf-8")
    );
    assert!(
        response
            .body
            .contains("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1, viewport-fit=cover\">")
    );
    for expected in [
        "--surface: oklch(",
        "min-height: 44px",
        ":focus-visible",
        "@media (min-width: 52rem)",
        "grid-template-areas:",
        "align-content: start",
        "prefers-reduced-motion: reduce",
    ] {
        assert!(
            response.body.contains(expected),
            "page omitted CSS contract {expected:?}"
        );
    }
    for forbidden in [
        "<script",
        "https://fonts.",
        "backdrop-filter",
        "background-clip: text",
    ] {
        assert!(
            !response.body.contains(forbidden),
            "page included {forbidden:?}"
        );
    }
    let page = response.body.to_lowercase();
    for expected in [
        LOCAL_URL,
        "search",
        "latitude",
        "longitude",
        "units",
        "runways",
        "range",
        "openstreetmap",
        "submitted search text",
        "sent to openstreetmap",
    ] {
        assert!(page.contains(expected), "page omitted {expected:?}");
    }
    assert!(!page.contains("wi-fi"));
    assert!(!page.contains("wifi"));
    assert!(!page.contains("navigator.geolocation"));
    assert!(!page.contains("getcurrentposition"));
}

#[test]
fn unconfigured_page_prioritizes_setup_and_exposes_semantic_controls() {
    let server = TestServer::new(RadarSettings::default(), Vec::new());

    let response = server.get("/");

    assert_eq!(response.status, 200);
    for expected in [
        "Local control",
        "Setup required",
        "Choose the radar's home location",
        "<main",
        "<fieldset",
        "<legend>Units</legend>",
        "<legend>Range</legend>",
        "<details",
        "Manual coordinates",
        "Apply settings",
    ] {
        assert!(response.body.contains(expected), "missing {expected:?}");
    }
}

#[test]
fn optional_settings_page_exposes_semantic_progressive_disclosure_controls() {
    let response = TestServer::new(RadarSettings::default(), Vec::new()).get("/");

    assert_eq!(response.status, 200);
    for expected in [
        "Aircraft labels",
        "Show callsign",
        "Show origin and destination",
        "Show expanded aircraft model",
        "Footer",
        "Weather condition",
        "Temperature",
        "Humidity",
        "Time",
        "Date",
        "Radar location",
        "Zulu",
        "12-hour",
        "24-hour",
        "Traffic filter",
        "Minimum altitude",
        "Maximum altitude",
        "ADSBDB",
        "Open-Meteo",
    ] {
        assert!(response.body.contains(expected), "missing {expected:?}");
    }
    assert!(!response.body.contains("<script"));
    assert!(response.body.contains(".switch {\n  min-height: 44px;"));
    for section in ["aircraft", "footer", "traffic"] {
        assert!(
            response.body.contains(&format!(
                "<details class=\"option-group\" data-section=\"{section}\""
            )),
            "missing {section:?} disclosure"
        );
        assert!(
            !response
                .body
                .contains(&format!("data-section=\"{section}\" open")),
            "default {section:?} disclosure should start collapsed"
        );
    }

    let configured = TestServer::new(optional_settings_enabled(), Vec::new()).get("/");
    for section in ["aircraft", "footer", "traffic"] {
        assert!(
            configured
                .body
                .contains(&format!("data-section=\"{section}\" open")),
            "non-default {section:?} disclosure should start open"
        );
    }
}

#[test]
fn footer_disclosure_summary_reports_zero_one_and_multiple_selections() {
    let off = TestServer::new(RadarSettings::default(), Vec::new()).get("/");
    assert!(off.body.contains("<summary>Footer — Off</summary>"));

    let mut one = RadarSettings::default();
    one.footer.show_time = true;
    let one = TestServer::new(one, Vec::new()).get("/");
    assert!(one.body.contains("<summary>Footer — Time</summary>"));

    let mut multiple = RadarSettings::default();
    multiple.footer.show_temperature = true;
    multiple.footer.show_time = true;
    multiple.footer.show_date = true;
    let multiple = TestServer::new(multiple, Vec::new()).get("/");
    assert!(
        multiple
            .body
            .contains("<summary>Footer — Temperature, time, date</summary>")
    );
}

#[test]
fn range_choices_use_display_values_in_the_selected_units() {
    let kilometres = TestServer::new(RadarSettings::default(), Vec::new()).get("/");
    for label in ["5 km", "10 km", "15 km", "25 km"] {
        assert!(kilometres.body.contains(label), "missing {label:?}");
    }

    let miles = TestServer::new(configured_settings(), Vec::new()).get("/");
    for label in ["3 mi", "6 mi", "9 mi", "16 mi"] {
        assert!(miles.body.contains(label), "missing {label:?}");
    }
    assert!(miles.body.contains("Radar configured"));
    assert!(miles.body.contains("Old location"));
}

#[test]
fn healthz_is_json_and_does_not_disclose_location_or_search_data() {
    let server = TestServer::new(configured_settings(), Vec::new());

    let response = server.get("/healthz");

    assert_eq!(response.status, 200);
    assert_eq!(
        response.header("content-type"),
        Some("application/json; charset=utf-8")
    );
    let json: serde_json::Value = serde_json::from_str(&response.body).unwrap();
    assert_eq!(
        json,
        serde_json::json!({
            "configured": true,
            "state": "RADAR",
            "data_stale": false,
            "revision": "test-revision"
        })
    );
    for forbidden in ["latitude", "longitude", "query", "search", "Old location"] {
        assert!(!response.body.contains(forbidden));
    }
}

#[test]
fn session_cookie_and_csrf_token_are_random_and_cookie_is_hardened() {
    let server = TestServer::new(configured_settings(), Vec::new());

    let first = server.get("/");
    let second = server.get("/");
    let first_cookie_header = first.header("set-cookie").unwrap();
    let second_cookie_header = second.header("set-cookie").unwrap();
    let first_cookie = first_cookie_header.split(';').next().unwrap();
    let second_cookie = second_cookie_header.split(';').next().unwrap();
    let first_token = extract_attribute(&first.body, "name=\"csrf_token\" value=\"");
    let second_token = extract_attribute(&second.body, "name=\"csrf_token\" value=\"");

    assert_ne!(first_cookie, second_cookie);
    assert_ne!(first_token, second_token);
    assert_eq!(first_cookie.split_once('=').unwrap().0, SESSION_COOKIE);
    assert_eq!(first_cookie.split_once('=').unwrap().1.len(), 64);
    assert_eq!(first_token.len(), 64);
    assert!(first_token.bytes().all(|byte| byte.is_ascii_hexdigit()));
    assert!(first_cookie_header.contains("HttpOnly"));
    assert!(first_cookie_header.contains("SameSite=Strict"));
    assert!(first_cookie_header.contains("Path=/"));
    assert!(!first_cookie_header.contains("Secure"));
    assert_eq!(first.header("cache-control"), Some("no-store"));
}

#[test]
fn host_header_requires_an_exact_allowlist_entry() {
    let server = TestServer::new(configured_settings(), Vec::new());

    assert_eq!(
        server
            .request("GET", "/", LOCAL_HOST, Vec::new(), &[], true)
            .status,
        200
    );
    assert_eq!(
        server
            .request("GET", "/", "planeradar.local", Vec::new(), &[], true)
            .status,
        403
    );
    assert_eq!(
        server
            .request("GET", "/", "attacker.local", Vec::new(), &[], true)
            .status,
        403
    );
    assert_eq!(
        server
            .request(
                "GET",
                "/",
                "hangar-2.local.evil.example",
                Vec::new(),
                &[],
                true,
            )
            .status,
        403
    );
    assert_eq!(
        server
            .request("GET", "/", "user@hangar-2.local", Vec::new(), &[], true,)
            .status,
        403
    );
    assert_eq!(
        server
            .request(
                "GET",
                "/",
                &server.allowed_host(),
                vec![("Host".to_owned(), server.allowed_host())],
                &[],
                true,
            )
            .status,
        403
    );
}

#[test]
fn both_post_endpoints_reject_wrong_token_and_invalid_origin() {
    for path in ["/search", "/settings"] {
        let server = TestServer::new(configured_settings(), Vec::new());
        let session = server.session();
        let mut wrong = session.clone();
        wrong.csrf = "00".repeat(32);
        let fields = if path == "/search" {
            vec![("query", "Boston")]
        } else {
            vec![("latitude", "40.7"), ("longitude", "-74.0")]
        };

        assert_eq!(
            server
                .post_form(
                    path,
                    &fields,
                    &wrong,
                    Some(&server.current_ip_origin()),
                    None,
                )
                .status,
            403,
            "{path} accepted the wrong CSRF token"
        );
        assert_eq!(
            server
                .post_form(path, &fields, &session, Some("http://evil.example"), None)
                .status,
            403,
            "{path} accepted a non-allowlisted Origin"
        );
    }
}

#[test]
fn settings_post_requires_matching_csrf_and_origin() {
    let server = TestServer::new(configured_settings(), Vec::new());
    let session = server.session();

    let response = server.post_form(
        "/settings",
        &[("latitude", "40.7"), ("longitude", "-74.0")],
        &session,
        Some("http://evil.example"),
        None,
    );

    assert_eq!(response.status, 403);
    assert_eq!(server.settings.replacement_count(), 0);
}

#[test]
fn posts_accept_exact_local_name_and_current_ip_origins() {
    let server = TestServer::new(configured_settings(), vec![geocode_result("Boston")]);
    let session = server.session();
    let local_name = server.post_form(
        "/search",
        &[("query", "Boston")],
        &session,
        Some(LOCAL_URL),
        None,
    );
    assert_eq!(local_name.status, 200);

    let current_ip = server.post_form(
        "/settings",
        &[("latitude", "40.7"), ("longitude", "-74.0")],
        &session,
        Some(&server.current_ip_origin()),
        None,
    );
    assert_eq!(current_ip.status, 303);
}

#[test]
fn absent_origin_requires_a_same_host_allowlisted_referer() {
    let server = TestServer::new(configured_settings(), Vec::new());
    let session = server.session();
    let fields = [("latitude", "40.7"), ("longitude", "-74.0")];
    let referer = format!("{}/settings?from=form", server.current_ip_origin());

    assert_eq!(
        server
            .post_form("/settings", &fields, &session, None, Some(&referer))
            .status,
        303
    );
    assert_eq!(
        server
            .post_form("/settings", &fields, &session, None, None)
            .status,
        403
    );
    assert_eq!(
        server
            .post_form(
                "/settings",
                &fields,
                &session,
                None,
                Some("http://hangar-2.local/settings"),
            )
            .status,
        403
    );
}

#[test]
fn duplicate_or_malformed_provenance_headers_are_rejected() {
    let server = TestServer::new(configured_settings(), Vec::new());
    let session = server.session();
    let body = format!("csrf_token={}&latitude=40.7&longitude=-74.0", session.csrf);
    let base_headers = vec![
        (
            "Content-Type".to_owned(),
            "application/x-www-form-urlencoded".to_owned(),
        ),
        ("Cookie".to_owned(), session.cookie.clone()),
    ];
    let mut duplicate = base_headers.clone();
    duplicate.push(("Origin".to_owned(), server.current_ip_origin()));
    duplicate.push(("Origin".to_owned(), LOCAL_URL.to_owned()));
    assert_eq!(
        server
            .request(
                "POST",
                "/settings",
                &server.allowed_host(),
                duplicate,
                body.as_bytes(),
                true,
            )
            .status,
        403
    );

    let mut malformed = base_headers;
    malformed.push(("Origin".to_owned(), "http://user@hangar-2.local".to_owned()));
    assert_eq!(
        server
            .request(
                "POST",
                "/settings",
                &server.allowed_host(),
                malformed,
                body.as_bytes(),
                true,
            )
            .status,
        403
    );
}

#[test]
fn cookie_parser_requires_the_exact_session_name_and_value() {
    let server = TestServer::new(configured_settings(), Vec::new());
    let session = server.session();
    let body = format!("csrf_token={}&latitude=40.7&longitude=-74.0", session.csrf);
    let session_value = session.cookie.split_once('=').unwrap().1;

    for cookie in [
        format!("x{SESSION_COOKIE}={session_value}"),
        format!("{SESSION_COOKIE}={session_value}x"),
    ] {
        let response = server.request(
            "POST",
            "/settings",
            &server.allowed_host(),
            vec![
                (
                    "Content-Type".to_owned(),
                    "application/x-www-form-urlencoded".to_owned(),
                ),
                ("Cookie".to_owned(), cookie),
                ("Origin".to_owned(), server.current_ip_origin()),
            ],
            body.as_bytes(),
            true,
        );
        assert_eq!(response.status, 403);
    }
}

#[test]
fn repeated_gets_evict_old_sessions_but_keep_recent_sessions_valid() {
    let server = TestServer::new(configured_settings(), Vec::new());
    let first = server.session();
    let mut newest = first.clone();
    for _ in 0..300 {
        newest = server.session();
    }
    let fields = [("latitude", "40.7"), ("longitude", "-74.0")];

    assert_eq!(
        server
            .post_form(
                "/settings",
                &fields,
                &first,
                Some(&server.current_ip_origin()),
                None,
            )
            .status,
        403
    );
    assert_eq!(
        server
            .post_form(
                "/settings",
                &fields,
                &newest,
                Some(&server.current_ip_origin()),
                None,
            )
            .status,
        303
    );
}

#[test]
fn invalid_coordinates_preserve_settings_and_never_call_replace() {
    let initial = configured_settings();
    let server = TestServer::new(initial.clone(), Vec::new());
    let session = server.session();

    let response = server.post_form(
        "/settings",
        &[("latitude", "91"), ("longitude", "-74.0")],
        &session,
        Some(&server.current_ip_origin()),
        None,
    );

    assert_eq!(response.status, 400);
    assert_eq!(server.settings.current(), initial);
    assert_eq!(server.settings.replacement_count(), 0);
}

#[test]
fn invalid_units_range_and_runway_values_never_call_replace() {
    for invalid_field in [
        ("units", "nautical"),
        ("range_index", "4"),
        ("range_index", "many"),
        ("show_runways", "sometimes"),
    ] {
        let initial = configured_settings();
        let server = TestServer::new(initial.clone(), Vec::new());
        let session = server.session();
        let fields = [("latitude", "40.7"), ("longitude", "-74.0"), invalid_field];

        let response = server.post_form(
            "/settings",
            &fields,
            &session,
            Some(&server.current_ip_origin()),
            None,
        );

        assert_eq!(response.status, 400, "accepted {invalid_field:?}");
        assert_eq!(server.settings.current(), initial);
        assert_eq!(server.settings.replacement_count(), 0);
    }
}

#[test]
fn search_results_are_selectable_escaped_and_never_persist() {
    let hostile_name = "<script>\"'& place";
    let server = TestServer::new(configured_settings(), vec![geocode_result(hostile_name)]);
    let session = server.session();

    let response = server.post_form(
        "/search",
        &[("query", "New York")],
        &session,
        Some(LOCAL_URL),
        None,
    );

    assert_eq!(response.status, 200);
    assert_eq!(response.header("cache-control"), Some("no-store"));
    assert!(response.body.contains("action=\"/settings\""));
    assert!(response.body.contains("value=\"40.7128\""));
    assert!(response.body.contains("value=\"-74.006\""));
    assert!(
        response
            .body
            .contains("&lt;script&gt;&quot;&#39;&amp; place")
    );
    assert!(!response.body.contains(hostile_name));
    assert!(response.body.contains("name=\"units\" value=\"mi\""));
    assert!(
        response
            .body
            .contains("name=\"show_runways\" value=\"false\"")
    );
    assert!(response.body.contains("name=\"range_index\" value=\"3\""));
    assert_eq!(server.settings.replacement_count(), 0);
    assert_eq!(
        server.queries.lock().unwrap().as_slice(),
        ["New York".to_owned()]
    );
}

#[test]
fn failed_search_keeps_the_manual_settings_page_without_sensitive_error_text() {
    let server = TestServer::with_failing_geocoder(configured_settings());
    let session = server.session();
    let query = "private place search";

    let response = server.post_form(
        "/search",
        &[("query", query)],
        &session,
        Some(&server.current_ip_origin()),
        None,
    );

    assert_eq!(response.status, 200);
    assert_eq!(
        response.header("content-type"),
        Some("text/html; charset=utf-8")
    );
    assert_eq!(response.header("cache-control"), Some("no-store"));
    assert!(
        response
            .body
            .contains("Search unavailable. Enter coordinates manually.")
    );
    assert!(response.body.contains("action=\"/settings\""));
    assert!(
        response
            .body
            .contains(&format!("name=\"csrf_token\" value=\"{}\"", session.csrf))
    );
    assert!(!response.body.contains(query));
    assert!(!response.body.contains("geocode query"));
}

#[test]
fn empty_search_has_a_distinct_manual_fallback_state() {
    let server = TestServer::new(configured_settings(), Vec::new());
    let session = server.session();

    let response = server.post_form(
        "/search",
        &[("query", "no match")],
        &session,
        Some(&server.current_ip_origin()),
        None,
    );

    assert_eq!(response.status, 200);
    assert!(response.body.contains("No matching places found"));
    assert!(response.body.contains("Enter coordinates manually"));
    assert!(!response.body.contains("Search unavailable"));
}

#[test]
fn invalid_settings_return_the_page_with_safe_guidance() {
    let server = TestServer::new(configured_settings(), Vec::new());
    let session = server.session();

    let response = server.post_form(
        "/settings",
        &[("latitude", "91"), ("longitude", "-74.0")],
        &session,
        Some(&server.current_ip_origin()),
        None,
    );

    assert_eq!(response.status, 400);
    assert_eq!(
        response.header("content-type"),
        Some("text/html; charset=utf-8")
    );
    assert!(
        response
            .body
            .contains("Those settings could not be applied")
    );
    assert!(response.body.contains("Old location"));
    assert!(!response.body.contains("latitude must be between"));
}

#[test]
fn failed_settings_write_returns_safe_page() {
    let server = TestServer::with_failing_settings(configured_settings());
    let session = server.session();

    let response = server.post_form(
        "/settings",
        &[("latitude", "40.7"), ("longitude", "-74.0")],
        &session,
        Some(&server.current_ip_origin()),
        None,
    );

    assert_eq!(response.status, 500);
    assert_eq!(
        response.header("content-type"),
        Some("text/html; charset=utf-8")
    );
    assert!(
        response
            .body
            .contains("Plane Radar could not save those settings")
    );
    assert!(response.body.contains("Old location"));
    assert!(!response.body.contains("LAN settings service failed"));
    assert_eq!(server.settings.replacement_count(), 0);
}

#[test]
fn successful_settings_redirect_to_a_fixed_confirmation_page() {
    let server = TestServer::new(RadarSettings::default(), Vec::new());
    let session = server.session();

    let response = server.post_form(
        "/settings",
        &[("latitude", "40.7"), ("longitude", "-74.0")],
        &session,
        Some(&server.current_ip_origin()),
        None,
    );

    assert_eq!(response.status, 303);
    assert_eq!(response.header("location"), Some("/?saved=1"));

    let confirmed = server.get("/?saved=1");
    assert_eq!(confirmed.status, 200);
    assert!(confirmed.body.contains("Radar settings applied"));
    assert!(confirmed.body.contains("role=\"status\""));
}

#[test]
fn incomplete_post_body_does_not_block_another_request_or_stop() {
    let mut server = TestServer::new(configured_settings(), Vec::new());
    let session = server.session();
    let partial_connection = server.begin_incomplete_settings_post(&session);
    thread::sleep(Duration::from_millis(75));

    let started = Instant::now();
    let health = server
        .get_with_timeout("/healthz", Duration::from_millis(300))
        .expect("an incomplete POST must not wedge the handler");
    assert_eq!(health.status, 200);
    assert!(started.elapsed() < Duration::from_millis(300));
    assert!(server.stop_is_observed_within(Duration::from_millis(300)));

    drop(partial_connection);
}

#[test]
fn settings_accept_a_selected_search_result_and_preserve_preferences() {
    let initial = configured_settings();
    let selected = geocode_result("New York & nearby");
    let server = TestServer::new(initial.clone(), vec![selected.clone()]);
    let session = server.session();
    let search = server.post_form(
        "/search",
        &[("query", "New York")],
        &session,
        Some(&server.current_ip_origin()),
        None,
    );
    assert_eq!(search.status, 200);

    let response = server.post_form(
        "/settings",
        &[
            ("latitude", "40.7128"),
            ("longitude", "-74.006"),
            ("label", "New York & nearby"),
        ],
        &session,
        Some(&server.current_ip_origin()),
        None,
    );

    assert_eq!(response.status, 303);
    assert_eq!(response.header("location"), Some("/?saved=1"));
    let stored = server.settings.current();
    assert_eq!(stored.location, Some(selected.location));
    assert_eq!(stored.units, initial.units);
    assert_eq!(stored.show_runways, initial.show_runways);
    assert_eq!(stored.range_index, initial.range_index);
    assert_eq!(server.settings.replacement_count(), 1);
}

#[test]
fn optional_settings_search_result_selection_preserves_every_preference() {
    let initial = optional_settings_enabled();
    let selected = geocode_result("New York & nearby");
    let server = TestServer::new(initial.clone(), vec![selected.clone()]);
    let session = server.session();
    let search = server.post_form(
        "/search",
        &[("query", "New York")],
        &session,
        Some(&server.current_ip_origin()),
        None,
    );
    assert_eq!(search.status, 200);
    assert!(search.body.contains("New York &amp; nearby"));

    let response = server.post_form(
        "/settings",
        &[
            ("latitude", "40.7128"),
            ("longitude", "-74.006"),
            ("label", "New York & nearby"),
        ],
        &session,
        Some(&server.current_ip_origin()),
        None,
    );

    assert_eq!(response.status, 303);
    let mut expected = initial;
    expected.location = Some(selected.location);
    assert_eq!(server.settings.current(), expected);
    assert_eq!(server.settings.replacement_count(), 1);
}

#[test]
fn optional_settings_fully_enabled_form_round_trips_every_field() {
    let server = TestServer::new(RadarSettings::default(), Vec::new());
    let session = server.session();

    let response = server.post_form(
        "/settings",
        &[
            ("latitude", "40.7128"),
            ("longitude", "-74.006"),
            ("label", "New York, NY"),
            ("units", "mi"),
            ("range_index", "3"),
            ("show_runways_present", "true"),
            ("show_runways", "true"),
            ("show_callsign_present", "true"),
            ("show_callsign", "true"),
            ("show_route_present", "true"),
            ("show_route", "true"),
            ("show_expanded_model_present", "true"),
            ("show_expanded_model", "true"),
            ("radar_text_scale_percent", "130"),
            ("footer_show_condition_present", "true"),
            ("footer_show_condition", "true"),
            ("footer_show_temperature_present", "true"),
            ("footer_show_temperature", "true"),
            ("footer_show_humidity_present", "true"),
            ("footer_show_humidity", "true"),
            ("footer_show_time_present", "true"),
            ("footer_show_time", "true"),
            ("footer_show_date_present", "true"),
            ("footer_show_date", "true"),
            ("temperature_unit", "fahrenheit"),
            ("time_zone", "zulu"),
            ("clock_format", "twelve"),
            ("minimum_altitude_feet", "1000"),
            ("maximum_altitude_feet", "45000"),
        ],
        &session,
        Some(&server.current_ip_origin()),
        None,
    );

    assert_eq!(response.status, 303);
    assert_eq!(
        server.settings.current(),
        RadarSettings {
            location: Some(Location {
                latitude: 40.7128,
                longitude: -74.006,
                label: "New York, NY".to_owned(),
            }),
            ..optional_settings_enabled()
        }
    );
    assert_eq!(server.settings.replacement_count(), 1);
}

#[test]
fn optional_settings_presence_sentinels_save_every_unchecked_switch_as_false() {
    let initial = optional_settings_enabled();
    let server = TestServer::new(initial.clone(), Vec::new());
    let session = server.session();

    let response = server.post_form(
        "/settings",
        &[
            ("latitude", "51.5072"),
            ("longitude", "-0.1276"),
            ("show_runways_present", "true"),
            ("show_callsign_present", "true"),
            ("show_route_present", "true"),
            ("show_expanded_model_present", "true"),
            ("footer_show_condition_present", "true"),
            ("footer_show_temperature_present", "true"),
            ("footer_show_humidity_present", "true"),
            ("footer_show_time_present", "true"),
            ("footer_show_date_present", "true"),
        ],
        &session,
        Some(&server.current_ip_origin()),
        None,
    );

    assert_eq!(response.status, 303);
    let stored = server.settings.current();
    assert!(!stored.show_runways);
    assert!(!stored.show_callsign);
    assert!(!stored.show_route);
    assert!(!stored.show_expanded_model);
    assert!(!stored.footer.show_condition);
    assert!(!stored.footer.show_temperature);
    assert!(!stored.footer.show_humidity);
    assert!(!stored.footer.show_time);
    assert!(!stored.footer.show_date);
    assert_eq!(
        stored.radar_text_scale_percent,
        initial.radar_text_scale_percent
    );
    assert_eq!(stored.minimum_altitude_feet, initial.minimum_altitude_feet);
    assert_eq!(stored.maximum_altitude_feet, initial.maximum_altitude_feet);
    assert_eq!(server.settings.replacement_count(), 1);
}

#[test]
fn optional_settings_runway_sentinel_accepts_legacy_explicit_false() {
    let initial = optional_settings_enabled();
    let server = TestServer::new(initial, Vec::new());
    let session = server.session();

    let response = server.post_form(
        "/settings",
        &[
            ("latitude", "51.5072"),
            ("longitude", "-0.1276"),
            ("show_runways_present", "true"),
            ("show_runways", "false"),
        ],
        &session,
        Some(&server.current_ip_origin()),
        None,
    );

    assert_eq!(response.status, 303);
    assert!(!server.settings.current().show_runways);
    assert_eq!(server.settings.replacement_count(), 1);
}

#[test]
fn optional_settings_invalid_duplicate_and_unknown_values_are_atomic() {
    type InvalidOptionalSettingCase<'a> = (&'a str, &'a [(&'a str, &'a str)], Option<&'a str>);

    let cases: &[InvalidOptionalSettingCase<'_>] = &[
        (
            "duplicate checkbox",
            &[
                ("show_route_present", "true"),
                ("show_route", "true"),
                ("show_route", "on"),
            ],
            Some("aircraft"),
        ),
        (
            "unknown checkbox value",
            &[("show_route_present", "true"), ("show_route", "sometimes")],
            Some("aircraft"),
        ),
        (
            "unknown checkbox value without sentinel",
            &[("show_route", "sometimes")],
            Some("aircraft"),
        ),
        (
            "invalid enum",
            &[("temperature_unit", "kelvin")],
            Some("footer"),
        ),
        (
            "invalid scale",
            &[("radar_text_scale_percent", "105")],
            None,
        ),
        (
            "out-of-range altitude",
            &[("minimum_altitude_feet", "-2001")],
            Some("traffic"),
        ),
        ("unknown field", &[("mystery_preference", "true")], None),
    ];

    for (name, extra, section) in cases {
        let initial = configured_settings();
        let server = TestServer::new(initial.clone(), Vec::new());
        let session = server.session();
        let mut fields = vec![("latitude", "40.7"), ("longitude", "-74.0")];
        fields.extend_from_slice(extra);

        let response = server.post_form(
            "/settings",
            &fields,
            &session,
            Some(&server.current_ip_origin()),
            None,
        );

        assert_eq!(response.status, 400, "accepted {name}");
        assert!(
            response.body.contains(
                "Those settings could not be applied. Check the coordinates and try again."
            ),
            "lost generic guidance for {name}"
        );
        if let Some(section) = section {
            assert!(
                response
                    .body
                    .contains(&format!("data-section=\"{section}\" open")),
                "did not open {section} for {name}"
            );
        }
        assert_eq!(
            server.settings.current(),
            initial,
            "changed settings for {name}"
        );
        assert_eq!(
            server.settings.replacement_count(),
            0,
            "replaced for {name}"
        );
    }
}

#[test]
fn optional_settings_crossed_altitude_bounds_show_specific_traffic_error_atomically() {
    let initial = configured_settings();
    let server = TestServer::new(initial.clone(), Vec::new());
    let session = server.session();

    let response = server.post_form(
        "/settings",
        &[
            ("latitude", "40.7"),
            ("longitude", "-74.0"),
            ("minimum_altitude_feet", "45001"),
            ("maximum_altitude_feet", "45000"),
        ],
        &session,
        Some(&server.current_ip_origin()),
        None,
    );

    assert_eq!(response.status, 400);
    assert!(
        response
            .body
            .contains("Minimum altitude cannot exceed maximum altitude.")
    );
    assert!(response.body.contains("data-section=\"traffic\" open"));
    assert_eq!(server.settings.current(), initial);
    assert_eq!(server.settings.replacement_count(), 0);
}

#[test]
fn settings_accept_manual_coordinates_and_replace_exactly_once() {
    let server = TestServer::new(RadarSettings::default(), Vec::new());
    let session = server.session();

    let response = server.post_form(
        "/settings",
        &[
            ("latitude", "-33.8688"),
            ("longitude", "151.2093"),
            ("label", "Home"),
            ("units", "mi"),
            ("show_runways", "false"),
            ("range_index", "2"),
        ],
        &session,
        Some(&server.current_ip_origin()),
        None,
    );

    assert_eq!(response.status, 303);
    assert_eq!(
        server.settings.current(),
        RadarSettings {
            location: Some(Location {
                latitude: -33.8688,
                longitude: 151.2093,
                label: "Home".to_owned(),
            }),
            units: Units::Miles,
            show_runways: false,
            range_index: 2,
            ..RadarSettings::default()
        }
    );
    assert_eq!(server.settings.replacement_count(), 1);
}

#[test]
fn oversized_fixed_and_chunked_form_bodies_return_413() {
    let server = TestServer::new(configured_settings(), Vec::new());
    let oversized = format!("padding={}", "x".repeat(16 * 1024));
    let fixed = server.request(
        "POST",
        "/search",
        &server.allowed_host(),
        vec![(
            "Content-Type".to_owned(),
            "application/x-www-form-urlencoded".to_owned(),
        )],
        oversized.as_bytes(),
        true,
    );
    assert_eq!(fixed.status, 413);

    let chunk = format!("{:x}\r\n{}\r\n0\r\n\r\n", oversized.len(), oversized);
    let chunked = server.request(
        "POST",
        "/search",
        &server.allowed_host(),
        vec![
            (
                "Content-Type".to_owned(),
                "application/x-www-form-urlencoded".to_owned(),
            ),
            ("Transfer-Encoding".to_owned(), "chunked".to_owned()),
        ],
        chunk.as_bytes(),
        false,
    );
    assert_eq!(chunked.status, 413);
}

#[test]
fn form_content_type_is_exact_case_insensitive_and_has_safe_parameters() {
    let server = TestServer::new(configured_settings(), Vec::new());
    let session = server.session();
    let body = format!("csrf_token={}&latitude=40.7&longitude=-74.0", session.csrf);
    let common = |content_type: &str| {
        server.request(
            "POST",
            "/settings",
            &server.allowed_host(),
            vec![
                ("Content-Type".to_owned(), content_type.to_owned()),
                ("Cookie".to_owned(), session.cookie.clone()),
                ("Origin".to_owned(), server.current_ip_origin()),
            ],
            body.as_bytes(),
            true,
        )
    };

    assert_eq!(
        common("Application/X-WWW-Form-Urlencoded; Charset=UTF-8").status,
        303
    );
    assert_eq!(common("application/x-www-form-urlencodedx").status, 415);
    assert_eq!(
        common("application/x-www-form-urlencoded; charset").status,
        415
    );
}

#[test]
fn every_unlisted_route_returns_404() {
    let server = TestServer::new(configured_settings(), Vec::new());

    for (method, path) in [
        ("GET", "/missing"),
        ("GET", "/search"),
        ("GET", "/settings"),
        ("POST", "/healthz"),
        ("PUT", "/"),
        ("GET", "/healthz?verbose=true"),
    ] {
        let response = server.request(method, path, &server.allowed_host(), Vec::new(), &[], true);
        assert_eq!(response.status, 404, "{method} {path} was not rejected");
    }
}

use std::collections::{HashMap, HashSet};
use std::io::{self, Read};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rand::RngCore;
use serde::Serialize;
use subtle::ConstantTimeEq;
use thiserror::Error;
use tiny_http::{Header, Method, Request, Response, Server};
use url::Url;

use crate::geocode::{GeocodeResult, GeocodeService};
use crate::model::{AppState, Location, RadarSettings, Units};
use crate::settings::validate_settings;

const MAX_FORM_BODY_BYTES: usize = 16 * 1024;
const MAX_SESSIONS: usize = 128;
const SESSION_LIFETIME: Duration = Duration::from_secs(60 * 60);
const RECEIVE_TIMEOUT: Duration = Duration::from_millis(50);
const SESSION_COOKIE: &str = "planeradar_session";
const MAX_IN_FLIGHT_REQUESTS: usize = 16;

pub trait SettingsService: Send + Sync {
    fn current(&self) -> RadarSettings;
    fn replace(&self, candidate: RadarSettings) -> Result<(), WebError>;
}

pub trait HealthSource: Send + Sync {
    fn health(&self) -> HealthSnapshot;
}

#[derive(Clone, Debug, Serialize)]
pub struct HealthSnapshot {
    pub configured: bool,
    pub state: AppState,
    pub data_stale: bool,
    pub revision: &'static str,
}

#[derive(Debug, Error)]
pub enum WebError {
    #[error("failed to bind LAN settings server: {0}")]
    Bind(String),
    #[error("LAN settings server I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("failed to serialize LAN settings response: {0}")]
    Json(#[from] serde_json::Error),
    #[error("LAN settings service failed")]
    Settings,
    #[error("runtime settings update notification is unavailable")]
    WorkerUnavailable,
    #[error("LAN settings server state is unavailable")]
    State,
}

pub struct SettingsServer {
    server: Server,
    state: Arc<ServerState>,
}

struct ServerState {
    settings: Arc<dyn SettingsService>,
    geocoder: Arc<Mutex<Box<dyn GeocodeService>>>,
    health: Arc<dyn HealthSource>,
    local_url: String,
    allowed_hosts: Arc<dyn Fn() -> HashSet<String> + Send + Sync>,
    sessions: Mutex<SessionStore>,
    in_flight_requests: AtomicUsize,
}

impl SettingsServer {
    pub fn bind(
        address: SocketAddr,
        settings: Arc<dyn SettingsService>,
        geocoder: Arc<Mutex<Box<dyn GeocodeService>>>,
        health: Arc<dyn HealthSource>,
        local_url: String,
        allowed_hosts: Arc<dyn Fn() -> HashSet<String> + Send + Sync>,
    ) -> Result<Self, WebError> {
        let server = Server::http(address).map_err(|error| WebError::Bind(error.to_string()))?;
        Ok(Self {
            server,
            state: Arc::new(ServerState {
                settings,
                geocoder,
                health,
                local_url,
                allowed_hosts,
                sessions: Mutex::new(SessionStore::default()),
                in_flight_requests: AtomicUsize::new(0),
            }),
        })
    }

    pub fn run(&self, stop: &AtomicBool) -> Result<(), WebError> {
        while !stop.load(Ordering::Acquire) {
            if let Some(request) = self.server.recv_timeout(RECEIVE_TIMEOUT)? {
                if !self.state.try_acquire_request_slot() {
                    let _ =
                        request.respond(Outgoing::text(503, "Service unavailable").into_response());
                    continue;
                }
                let state = self.state.clone();
                std::thread::spawn(move || {
                    let _request_slot = RequestSlot {
                        state: state.clone(),
                    };
                    let _ = state.handle(request);
                });
            }
        }
        Ok(())
    }
}

impl ServerState {
    fn handle(&self, mut request: Request) -> Result<(), WebError> {
        let outgoing = match self.route(&mut request) {
            Ok(response) => response,
            Err(_) => Outgoing::text(500, "Internal server error"),
        };
        request.respond(outgoing.into_response())?;
        Ok(())
    }

    fn route(&self, request: &mut Request) -> Result<Outgoing, WebError> {
        let route = match (request.method(), request.url()) {
            (&Method::Get, "/") => Route::Page,
            (&Method::Get, "/healthz") => Route::Health,
            (&Method::Post, "/search") => Route::Search,
            (&Method::Post, "/settings") => Route::Settings,
            _ => return Ok(Outgoing::text(404, "Not found")),
        };

        let Some(request_host) = self.valid_request_host(request) else {
            return Ok(Outgoing::text(403, "Forbidden"));
        };

        match route {
            Route::Page => self.page(),
            Route::Health => self.health(),
            Route::Search | Route::Settings => self.mutation(request, request_host, route),
        }
    }

    fn page(&self) -> Result<Outgoing, WebError> {
        let (session_id, csrf_token) = self.sessions.lock().map_err(|_| WebError::State)?.create();
        let body = render_page(
            &self.settings.current(),
            &self.local_url,
            &csrf_token,
            &[],
            None,
        );
        Ok(Outgoing::html(200, body)
            .with_header(
                "Set-Cookie",
                format!("{SESSION_COOKIE}={session_id}; HttpOnly; SameSite=Strict; Path=/"),
            )
            .with_header("Cache-Control", "no-store"))
    }

    fn health(&self) -> Result<Outgoing, WebError> {
        let body = serde_json::to_vec(&self.health.health())?;
        Ok(Outgoing::new(200, "application/json; charset=utf-8", body))
    }

    fn mutation(
        &self,
        request: &mut Request,
        request_host: Authority,
        route: Route,
    ) -> Result<Outgoing, WebError> {
        if request
            .body_length()
            .is_some_and(|length| length > MAX_FORM_BODY_BYTES)
        {
            return Ok(Outgoing::text(413, "Payload too large"));
        }
        if !has_form_content_type(request) {
            return Ok(Outgoing::text(415, "Unsupported media type"));
        }

        let mut body = Vec::new();
        request
            .as_reader()
            .take((MAX_FORM_BODY_BYTES + 1) as u64)
            .read_to_end(&mut body)?;
        if body.len() > MAX_FORM_BODY_BYTES {
            return Ok(Outgoing::text(413, "Payload too large"));
        }
        let form = Form::parse(&body);
        let Ok(Some(submitted_csrf)) = form.single("csrf_token") else {
            return Ok(Outgoing::text(403, "Forbidden"));
        };
        let submitted_csrf = submitted_csrf.to_owned();
        let Some(session_id) = exact_session_cookie(request) else {
            return Ok(Outgoing::text(403, "Forbidden"));
        };
        if !self.valid_provenance(request, &request_host)
            || !self
                .sessions
                .lock()
                .map_err(|_| WebError::State)?
                .validate(&session_id, &submitted_csrf)
        {
            return Ok(Outgoing::text(403, "Forbidden"));
        }

        match route {
            Route::Search => self.search(form, &submitted_csrf),
            Route::Settings => self.replace_settings(form),
            Route::Page | Route::Health => unreachable!("mutation routes are POST-only"),
        }
    }

    fn search(&self, form: Form, csrf_token: &str) -> Result<Outgoing, WebError> {
        let Ok(Some(query)) = form.single("query") else {
            return Ok(Outgoing::text(400, "Invalid search"));
        };
        let results = match self
            .geocoder
            .lock()
            .map_err(|_| WebError::State)?
            .search(query)
        {
            Ok(results) => results,
            Err(_) => {
                let body = render_page(
                    &self.settings.current(),
                    &self.local_url,
                    csrf_token,
                    &[],
                    Some("Search unavailable; enter coordinates manually"),
                );
                return Ok(Outgoing::html(200, body).with_header("Cache-Control", "no-store"));
            }
        };
        let body = render_page(
            &self.settings.current(),
            &self.local_url,
            csrf_token,
            &results,
            None,
        );
        Ok(Outgoing::html(200, body).with_header("Cache-Control", "no-store"))
    }

    fn replace_settings(&self, form: Form) -> Result<Outgoing, WebError> {
        let candidate = match candidate_from_form(&self.settings.current(), &form) {
            Ok(candidate) => candidate,
            Err(()) => return Ok(Outgoing::text(400, "Invalid settings")),
        };
        let value = serde_json::to_value(candidate)?;
        let validated = match validate_settings(value) {
            Ok(validated) => validated,
            Err(_) => return Ok(Outgoing::text(400, "Invalid settings")),
        };

        if self.settings.replace(validated).is_err() {
            return Ok(Outgoing::text(500, "Internal server error"));
        }
        Ok(Outgoing::text(303, "").with_header("Location", "/"))
    }

    fn valid_request_host(&self, request: &Request) -> Option<Authority> {
        let host = exactly_one_header(request, "Host")?;
        let authority = Authority::parse_host_header(host)?;
        self.authority_is_allowed(&authority).then_some(authority)
    }

    fn valid_provenance(&self, request: &Request, request_host: &Authority) -> bool {
        let origins = header_values(request, "Origin");
        if origins.len() > 1 {
            return false;
        }
        if let Some(origin) = origins.first() {
            return Authority::parse_origin(origin)
                .is_some_and(|authority| self.authority_is_allowed(&authority));
        }

        let referers = header_values(request, "Referer");
        if referers.len() != 1 {
            return false;
        }
        Authority::parse_referer(referers[0]).is_some_and(|authority| {
            authority == *request_host && self.authority_is_allowed(&authority)
        })
    }

    fn authority_is_allowed(&self, authority: &Authority) -> bool {
        (self.allowed_hosts)()
            .iter()
            .filter_map(|allowed| Authority::parse_host_header(allowed))
            .any(|allowed| allowed == *authority)
    }

    fn try_acquire_request_slot(&self) -> bool {
        let mut active = self.in_flight_requests.load(Ordering::Acquire);
        loop {
            if active >= MAX_IN_FLIGHT_REQUESTS {
                return false;
            }
            match self.in_flight_requests.compare_exchange_weak(
                active,
                active + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return true,
                Err(current) => active = current,
            }
        }
    }
}

struct RequestSlot {
    state: Arc<ServerState>,
}

impl Drop for RequestSlot {
    fn drop(&mut self) {
        self.state.in_flight_requests.fetch_sub(1, Ordering::AcqRel);
    }
}

#[derive(Clone, Copy)]
enum Route {
    Page,
    Health,
    Search,
    Settings,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Authority {
    host: String,
    port: Option<u16>,
}

impl Authority {
    fn parse_host_header(value: &str) -> Option<Self> {
        if value.trim() != value || value.chars().any(char::is_control) {
            return None;
        }
        let url = Url::parse(&format!("http://{value}/")).ok()?;
        if !url.username().is_empty()
            || url.password().is_some()
            || url.path() != "/"
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return None;
        }
        Self::from_url(&url)
    }

    fn parse_origin(value: &str) -> Option<Self> {
        let url = Self::parse_http_url(value)?;
        if url.path() != "/" || url.query().is_some() || url.fragment().is_some() {
            return None;
        }
        Self::from_url(&url)
    }

    fn parse_referer(value: &str) -> Option<Self> {
        let url = Self::parse_http_url(value)?;
        if url.fragment().is_some() {
            return None;
        }
        Self::from_url(&url)
    }

    fn parse_http_url(value: &str) -> Option<Url> {
        if value.trim() != value || value.chars().any(char::is_control) {
            return None;
        }
        let url = Url::parse(value).ok()?;
        if url.scheme() != "http" || !url.username().is_empty() || url.password().is_some() {
            return None;
        }
        Some(url)
    }

    fn from_url(url: &Url) -> Option<Self> {
        Some(Self {
            host: url.host_str()?.to_owned(),
            port: url.port(),
        })
    }
}

#[derive(Default)]
struct SessionStore {
    sessions: HashMap<String, SessionRecord>,
    next_serial: u64,
}

impl SessionStore {
    fn create(&mut self) -> (String, String) {
        self.remove_expired();
        while self.sessions.len() >= MAX_SESSIONS {
            let oldest = self
                .sessions
                .iter()
                .min_by_key(|(_, record)| record.serial)
                .map(|(id, _)| id.clone());
            if let Some(oldest) = oldest {
                self.sessions.remove(&oldest);
            }
        }

        let session_id = loop {
            let candidate = random_token();
            if !self.sessions.contains_key(&candidate) {
                break candidate;
            }
        };
        let csrf_token = random_token();
        let serial = self.next_serial;
        self.next_serial = self.next_serial.wrapping_add(1);
        self.sessions.insert(
            session_id.clone(),
            SessionRecord {
                csrf_token: csrf_token.clone(),
                expires_at: Instant::now() + SESSION_LIFETIME,
                serial,
            },
        );
        (session_id, csrf_token)
    }

    fn validate(&mut self, session_id: &str, submitted_csrf: &str) -> bool {
        self.remove_expired();
        let Some(record) = self.sessions.get(session_id) else {
            return false;
        };
        if record.csrf_token.len() != submitted_csrf.len() {
            return false;
        }
        bool::from(
            record
                .csrf_token
                .as_bytes()
                .ct_eq(submitted_csrf.as_bytes()),
        )
    }

    fn remove_expired(&mut self) {
        let now = Instant::now();
        self.sessions.retain(|_, session| session.expires_at > now);
    }
}

struct SessionRecord {
    csrf_token: String,
    expires_at: Instant,
    serial: u64,
}

fn random_token() -> String {
    let mut bytes = [0_u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    let mut token = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        write!(&mut token, "{byte:02x}").expect("writing to a String cannot fail");
    }
    token
}

struct Form {
    fields: Vec<(String, String)>,
}

impl Form {
    fn parse(body: &[u8]) -> Self {
        Self {
            fields: url::form_urlencoded::parse(body).into_owned().collect(),
        }
    }

    fn single(&self, name: &str) -> Result<Option<&str>, ()> {
        let mut values = self
            .fields
            .iter()
            .filter(|(candidate, _)| candidate == name)
            .map(|(_, value)| value.as_str());
        let first = values.next();
        if values.next().is_some() {
            Err(())
        } else {
            Ok(first)
        }
    }
}

fn candidate_from_form(current: &RadarSettings, form: &Form) -> Result<RadarSettings, ()> {
    let latitude = form
        .single("latitude")?
        .ok_or(())?
        .parse()
        .map_err(|_| ())?;
    let longitude = form
        .single("longitude")?
        .ok_or(())?
        .parse()
        .map_err(|_| ())?;
    let label = form.single("label")?.unwrap_or("").to_owned();
    let mut candidate = current.clone();
    candidate.schema_version = 1;
    candidate.location = Some(Location {
        latitude,
        longitude,
        label,
    });

    if let Some(units) = form.single("units")? {
        candidate.units = match units {
            "km" => Units::Kilometres,
            "mi" => Units::Miles,
            _ => return Err(()),
        };
    }
    if let Some(range_index) = form.single("range_index")? {
        candidate.range_index = range_index.parse().map_err(|_| ())?;
    }
    if form.single("show_runways_present")?.is_some() {
        candidate.show_runways = match form.single("show_runways")? {
            Some("true" | "on") => true,
            None | Some("false") => false,
            Some(_) => return Err(()),
        };
    } else if let Some(show_runways) = form.single("show_runways")? {
        candidate.show_runways = match show_runways {
            "true" | "on" => true,
            "false" => false,
            _ => return Err(()),
        };
    }
    Ok(candidate)
}

fn has_form_content_type(request: &Request) -> bool {
    let Some(value) = exactly_one_header(request, "Content-Type") else {
        return false;
    };
    let mut parts = value.split(';');
    if !parts.next().is_some_and(|media_type| {
        media_type
            .trim()
            .eq_ignore_ascii_case("application/x-www-form-urlencoded")
    }) {
        return false;
    }

    let mut saw_charset = false;
    for parameter in parts {
        let Some((name, value)) = parameter.trim().split_once('=') else {
            return false;
        };
        if !name.trim().eq_ignore_ascii_case("charset") || saw_charset {
            return false;
        }
        let value = value.trim();
        let value = value
            .strip_prefix('"')
            .and_then(|quoted| quoted.strip_suffix('"'))
            .unwrap_or(value);
        if !value.eq_ignore_ascii_case("utf-8") {
            return false;
        }
        saw_charset = true;
    }
    true
}

fn exact_session_cookie(request: &Request) -> Option<String> {
    let header = exactly_one_header(request, "Cookie")?;
    let mut found = None;
    for cookie in header.split(';') {
        let (name, value) = cookie.trim().split_once('=')?;
        if name == SESSION_COOKIE {
            if found.is_some() || value.is_empty() {
                return None;
            }
            found = Some(value.to_owned());
        }
    }
    found
}

fn exactly_one_header<'a>(request: &'a Request, name: &'static str) -> Option<&'a str> {
    let values = header_values(request, name);
    (values.len() == 1).then_some(values[0])
}

fn header_values<'a>(request: &'a Request, name: &'static str) -> Vec<&'a str> {
    request
        .headers()
        .iter()
        .filter(|header| header.field.equiv(name))
        .map(|header| header.value.as_str())
        .collect()
}

fn render_page(
    settings: &RadarSettings,
    local_url: &str,
    csrf_token: &str,
    results: &[GeocodeResult],
    message: Option<&str>,
) -> String {
    let (latitude, longitude, label) = settings
        .location
        .as_ref()
        .map(|location| {
            (
                location.latitude.to_string(),
                location.longitude.to_string(),
                location.label.as_str(),
            )
        })
        .unwrap_or_else(|| (String::new(), String::new(), ""));
    let csrf = escape_html(csrf_token);
    let local_url = escape_html(local_url);
    let label = escape_html(label);
    let kilometres_selected = if settings.units == Units::Kilometres {
        " selected"
    } else {
        ""
    };
    let miles_selected = if settings.units == Units::Miles {
        " selected"
    } else {
        ""
    };
    let runways_checked = if settings.show_runways {
        " checked"
    } else {
        ""
    };
    let message = message
        .map(|message| format!("<p role=\"alert\">{}</p>", escape_html(message)))
        .unwrap_or_default();

    let mut result_forms = String::new();
    for result in results {
        let display_name = escape_html(&result.display_name);
        let result_label = escape_html(&result.location.label);
        let units = match settings.units {
            Units::Kilometres => "km",
            Units::Miles => "mi",
        };
        result_forms.push_str(&format!(
            r#"<form action="/settings" method="post">
<input type="hidden" name="csrf_token" value="{csrf}">
<input type="hidden" name="latitude" value="{latitude}">
<input type="hidden" name="longitude" value="{longitude}">
<input type="hidden" name="label" value="{result_label}">
<input type="hidden" name="units" value="{units}">
<input type="hidden" name="show_runways" value="{show_runways}">
<input type="hidden" name="range_index" value="{range_index}">
<button type="submit">Use {display_name}</button>
</form>"#,
            latitude = result.location.latitude,
            longitude = result.location.longitude,
            show_runways = settings.show_runways,
            range_index = settings.range_index,
        ));
    }

    let range_options = (0_u8..=3)
        .map(|index| {
            let selected = if settings.range_index == index {
                " selected"
            } else {
                ""
            };
            format!(r#"<option value="{index}"{selected}>{index}</option>"#)
        })
        .collect::<String>();

    format!(
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Plane Radar settings</title>
<style>
* {{ box-sizing: border-box; }}
body {{ margin: 0; font-size: 16px; line-height: 1.5; }}
main {{ width: min(100%, 42rem); margin: 0 auto; padding: 1rem; }}
section {{ margin-block: 1.5rem; }}
form {{ display: grid; gap: 0.75rem; }}
label {{ display: grid; gap: 0.25rem; min-width: 0; }}
input, select, button {{ width: 100%; min-height: 2.75rem; font: inherit; }}
p, button {{ overflow-wrap: anywhere; }}
@media (min-width: 40rem) {{ main {{ padding: 2rem; }} }}
</style>
</head>
<body>
<main>
<h1>Plane Radar settings</h1>
<p>Open this page at <a href="{local_url}">{local_url}</a>.</p>
{message}
<section>
<h2>Search for a place</h2>
<form action="/search" method="post">
<input type="hidden" name="csrf_token" value="{csrf}">
<label>Search <input name="query" type="search" required></label>
<button type="submit">Search</button>
</form>
<p>Your submitted search text is sent to OpenStreetMap. Search data © OpenStreetMap contributors.</p>
{result_forms}
</section>
<section>
<h2>Manual location and radar options</h2>
<form action="/settings" method="post">
<input type="hidden" name="csrf_token" value="{csrf}">
<label>Latitude <input name="latitude" value="{latitude}" inputmode="decimal" required></label>
<label>Longitude <input name="longitude" value="{longitude}" inputmode="decimal" required></label>
<label>Place name <input name="label" value="{label}"></label>
<label>Units <select name="units">
<option value="km"{kilometres_selected}>Kilometres</option>
<option value="mi"{miles_selected}>Miles</option>
</select></label>
<input type="hidden" name="show_runways_present" value="true">
<label><input type="checkbox" name="show_runways" value="true"{runways_checked}> Show runways</label>
<label>Range <select name="range_index">{range_options}</select></label>
<button type="submit">Save settings</button>
</form>
</section>
</main>
</body>
</html>"#
    )
}

fn escape_html(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            _ => escaped.push(character),
        }
    }
    escaped
}

struct Outgoing {
    status: u16,
    content_type: &'static str,
    body: Vec<u8>,
    headers: Vec<Header>,
}

impl Outgoing {
    fn new(status: u16, content_type: &'static str, body: Vec<u8>) -> Self {
        Self {
            status,
            content_type,
            body,
            headers: Vec::new(),
        }
    }

    fn html(status: u16, body: String) -> Self {
        Self::new(status, "text/html; charset=utf-8", body.into_bytes())
    }

    fn text(status: u16, body: &str) -> Self {
        Self::new(
            status,
            "text/plain; charset=utf-8",
            body.as_bytes().to_vec(),
        )
    }

    fn with_header(mut self, name: &'static str, value: impl Into<String>) -> Self {
        self.headers.push(
            Header::from_bytes(name.as_bytes(), value.into().as_bytes())
                .expect("response header must contain ASCII"),
        );
        self
    }

    fn into_response(self) -> Response<std::io::Cursor<Vec<u8>>> {
        let mut response = Response::from_data(self.body)
            .with_status_code(self.status)
            .with_header(
                Header::from_bytes("Content-Type", self.content_type)
                    .expect("static response content type must be ASCII"),
            );
        for header in self.headers {
            response = response.with_header(header);
        }
        response
    }
}

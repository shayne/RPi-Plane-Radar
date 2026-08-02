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
use crate::model::{
    AppState, ClockFormat, Location, RadarSettings, SETTINGS_SCHEMA_VERSION, TemperatureUnit,
    TimeZone, Units,
};
use crate::range::{format_range_label, range_preset};
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
            (&Method::Get, "/") => Route::Page { saved: false },
            (&Method::Get, "/?saved=1") => Route::Page { saved: true },
            (&Method::Get, "/healthz") => Route::Health,
            (&Method::Post, "/search") => Route::Search,
            (&Method::Post, "/settings") => Route::Settings,
            _ => return Ok(Outgoing::text(404, "Not found")),
        };

        let Some(request_host) = self.valid_request_host(request) else {
            return Ok(Outgoing::text(403, "Forbidden"));
        };

        match route {
            Route::Page { saved } => self.page(saved),
            Route::Health => self.health(),
            Route::Search | Route::Settings => self.mutation(request, request_host, route),
        }
    }

    fn page(&self, saved: bool) -> Result<Outgoing, WebError> {
        let (session_id, csrf_token) = self.sessions.lock().map_err(|_| WebError::State)?.create();
        let body = render_page(
            &self.settings.current(),
            &self.local_url,
            &csrf_token,
            SearchState::Idle,
            saved.then_some(PageNotice::Saved),
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
            Route::Settings => self.replace_settings(form, &submitted_csrf),
            Route::Page { .. } | Route::Health => unreachable!("mutation routes are POST-only"),
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
                    SearchState::Unavailable,
                    None,
                );
                return Ok(Outgoing::html(200, body).with_header("Cache-Control", "no-store"));
            }
        };
        let search = if results.is_empty() {
            SearchState::Empty
        } else {
            SearchState::Results(&results)
        };
        let body = render_page(
            &self.settings.current(),
            &self.local_url,
            csrf_token,
            search,
            None,
        );
        Ok(Outgoing::html(200, body).with_header("Cache-Control", "no-store"))
    }

    fn replace_settings(&self, form: Form, csrf_token: &str) -> Result<Outgoing, WebError> {
        let current = self.settings.current();
        let candidate = match candidate_from_form(&current, &form) {
            Ok(candidate) => candidate,
            Err(error) => {
                let body = render_page(
                    &current,
                    &self.local_url,
                    csrf_token,
                    SearchState::Idle,
                    Some(PageNotice::InvalidSettings(error)),
                );
                return Ok(Outgoing::html(400, body).with_header("Cache-Control", "no-store"));
            }
        };
        let value = serde_json::to_value(candidate)?;
        let validated = match validate_settings(value) {
            Ok(validated) => validated,
            Err(_) => {
                let body = render_page(
                    &current,
                    &self.local_url,
                    csrf_token,
                    SearchState::Idle,
                    Some(PageNotice::InvalidSettings(FormError::generic(None))),
                );
                return Ok(Outgoing::html(400, body).with_header("Cache-Control", "no-store"));
            }
        };

        if self.settings.replace(validated).is_err() {
            let body = render_page(
                &current,
                &self.local_url,
                csrf_token,
                SearchState::Idle,
                Some(PageNotice::SaveFailed),
            );
            return Ok(Outgoing::html(500, body).with_header("Cache-Control", "no-store"));
        }
        Ok(Outgoing::text(303, "").with_header("Location", "/?saved=1"))
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
    Page { saved: bool },
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

const INVALID_SETTINGS_MESSAGE: &str =
    "Those settings could not be applied. Check the coordinates and try again.";

#[derive(Clone, Copy, Eq, PartialEq)]
enum SettingsSection {
    Aircraft,
    Footer,
    Traffic,
}

#[derive(Clone, Copy)]
struct FormError {
    section: Option<SettingsSection>,
    message: &'static str,
}

impl FormError {
    fn generic(section: Option<SettingsSection>) -> Self {
        Self {
            section,
            message: INVALID_SETTINGS_MESSAGE,
        }
    }

    fn in_section(mut self, section: SettingsSection) -> Self {
        self.section = Some(section);
        self
    }
}

fn checkbox(form: &Form, name: &str, current: bool) -> Result<bool, FormError> {
    let submitted = form.single(name).map_err(|()| FormError::generic(None))?;
    if !matches!(submitted, None | Some("true" | "on")) {
        return Err(FormError::generic(None));
    }
    let sentinel_name = format!("{name}_present");
    let sentinel = form
        .single(&sentinel_name)
        .map_err(|()| FormError::generic(None))?;
    let Some(sentinel) = sentinel else {
        return Ok(current);
    };
    if sentinel != "true" {
        return Err(FormError::generic(None));
    }
    match submitted {
        Some("true" | "on") => Ok(true),
        None => Ok(false),
        Some(_) => unreachable!("unexpected checkbox values were rejected above"),
    }
}

fn optional_i32(form: &Form, name: &str) -> Result<Option<i32>, FormError> {
    let value = form.single(name).map_err(|()| FormError::generic(None))?;
    let Some(value) = value else {
        return Ok(None);
    };
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }
    value
        .parse()
        .map(Some)
        .map_err(|_| FormError::generic(None))
}

fn parse_temperature_unit(value: &str) -> Result<TemperatureUnit, FormError> {
    match value {
        "celsius" => Ok(TemperatureUnit::Celsius),
        "fahrenheit" => Ok(TemperatureUnit::Fahrenheit),
        _ => Err(FormError::generic(None)),
    }
}

fn parse_time_zone(value: &str) -> Result<TimeZone, FormError> {
    match value {
        "radar_local" => Ok(TimeZone::RadarLocal),
        "zulu" => Ok(TimeZone::Zulu),
        _ => Err(FormError::generic(None)),
    }
}

fn parse_clock_format(value: &str) -> Result<ClockFormat, FormError> {
    match value {
        "twelve" => Ok(ClockFormat::Twelve),
        "twenty_four" => Ok(ClockFormat::TwentyFour),
        _ => Err(FormError::generic(None)),
    }
}

fn candidate_from_form(current: &RadarSettings, form: &Form) -> Result<RadarSettings, FormError> {
    const KNOWN_FIELDS: &[&str] = &[
        "csrf_token",
        "latitude",
        "longitude",
        "label",
        "units",
        "range_index",
        "show_runways_present",
        "show_runways",
        "show_callsign_present",
        "show_callsign",
        "show_route_present",
        "show_route",
        "show_expanded_model_present",
        "show_expanded_model",
        "radar_text_scale_percent",
        "footer_show_condition_present",
        "footer_show_condition",
        "footer_show_temperature_present",
        "footer_show_temperature",
        "footer_show_humidity_present",
        "footer_show_humidity",
        "footer_show_time_present",
        "footer_show_time",
        "footer_show_date_present",
        "footer_show_date",
        "temperature_unit",
        "time_zone",
        "clock_format",
        "minimum_altitude_feet",
        "maximum_altitude_feet",
    ];
    let mut candidate = current.clone();
    candidate.schema_version = SETTINGS_SCHEMA_VERSION;

    if form
        .fields
        .iter()
        .any(|(name, _)| !KNOWN_FIELDS.contains(&name.as_str()))
    {
        return Err(FormError::generic(None));
    }

    let latitude = form
        .single("latitude")
        .map_err(|()| FormError::generic(None))?
        .ok_or_else(|| FormError::generic(None))?
        .parse()
        .map_err(|_| FormError::generic(None))?;
    let longitude = form
        .single("longitude")
        .map_err(|()| FormError::generic(None))?
        .ok_or_else(|| FormError::generic(None))?
        .parse()
        .map_err(|_| FormError::generic(None))?;
    let label = form
        .single("label")
        .map_err(|()| FormError::generic(None))?
        .unwrap_or("")
        .to_owned();
    candidate.location = Some(Location {
        latitude,
        longitude,
        label,
    });

    if let Some(units) = form
        .single("units")
        .map_err(|()| FormError::generic(None))?
    {
        candidate.units = match units {
            "km" => Units::Kilometres,
            "mi" => Units::Miles,
            _ => return Err(FormError::generic(None)),
        };
    }
    if let Some(range_index) = form
        .single("range_index")
        .map_err(|()| FormError::generic(None))?
    {
        candidate.range_index = range_index.parse().map_err(|_| FormError::generic(None))?;
    }
    candidate.show_runways = if form
        .fields
        .iter()
        .any(|(name, _)| name == "show_runways_present")
    {
        checkbox(form, "show_runways", candidate.show_runways)?
    } else {
        match form
            .single("show_runways")
            .map_err(|()| FormError::generic(None))?
        {
            Some("true" | "on") => true,
            Some("false") => false,
            None => candidate.show_runways,
            Some(_) => return Err(FormError::generic(None)),
        }
    };
    candidate.show_callsign = checkbox(form, "show_callsign", candidate.show_callsign)
        .map_err(|error| error.in_section(SettingsSection::Aircraft))?;
    candidate.show_route = checkbox(form, "show_route", candidate.show_route)
        .map_err(|error| error.in_section(SettingsSection::Aircraft))?;
    candidate.show_expanded_model =
        checkbox(form, "show_expanded_model", candidate.show_expanded_model)
            .map_err(|error| error.in_section(SettingsSection::Aircraft))?;

    if let Some(scale) = form
        .single("radar_text_scale_percent")
        .map_err(|()| FormError::generic(None))?
    {
        candidate.radar_text_scale_percent = scale.parse().map_err(|_| FormError::generic(None))?;
        if !matches!(
            candidate.radar_text_scale_percent,
            80 | 90 | 100 | 110 | 120 | 130
        ) {
            return Err(FormError::generic(None));
        }
    }

    candidate.footer.show_condition = checkbox(
        form,
        "footer_show_condition",
        candidate.footer.show_condition,
    )
    .map_err(|error| error.in_section(SettingsSection::Footer))?;
    candidate.footer.show_temperature = checkbox(
        form,
        "footer_show_temperature",
        candidate.footer.show_temperature,
    )
    .map_err(|error| error.in_section(SettingsSection::Footer))?;
    candidate.footer.show_humidity =
        checkbox(form, "footer_show_humidity", candidate.footer.show_humidity)
            .map_err(|error| error.in_section(SettingsSection::Footer))?;
    candidate.footer.show_time = checkbox(form, "footer_show_time", candidate.footer.show_time)
        .map_err(|error| error.in_section(SettingsSection::Footer))?;
    candidate.footer.show_date = checkbox(form, "footer_show_date", candidate.footer.show_date)
        .map_err(|error| error.in_section(SettingsSection::Footer))?;

    if let Some(value) = form
        .single("temperature_unit")
        .map_err(|()| FormError::generic(Some(SettingsSection::Footer)))?
    {
        candidate.footer.temperature_unit = parse_temperature_unit(value)
            .map_err(|error| error.in_section(SettingsSection::Footer))?;
    }
    if let Some(value) = form
        .single("time_zone")
        .map_err(|()| FormError::generic(Some(SettingsSection::Footer)))?
    {
        candidate.footer.time_zone =
            parse_time_zone(value).map_err(|error| error.in_section(SettingsSection::Footer))?;
    }
    if let Some(value) = form
        .single("clock_format")
        .map_err(|()| FormError::generic(Some(SettingsSection::Footer)))?
    {
        candidate.footer.clock_format =
            parse_clock_format(value).map_err(|error| error.in_section(SettingsSection::Footer))?;
    }

    if form
        .fields
        .iter()
        .any(|(name, _)| name == "minimum_altitude_feet")
    {
        candidate.minimum_altitude_feet = optional_i32(form, "minimum_altitude_feet")
            .map_err(|error| error.in_section(SettingsSection::Traffic))?;
    }
    if form
        .fields
        .iter()
        .any(|(name, _)| name == "maximum_altitude_feet")
    {
        candidate.maximum_altitude_feet = optional_i32(form, "maximum_altitude_feet")
            .map_err(|error| error.in_section(SettingsSection::Traffic))?;
    }
    if candidate
        .minimum_altitude_feet
        .is_some_and(|altitude| !(-2000..=100_000).contains(&altitude))
        || candidate
            .maximum_altitude_feet
            .is_some_and(|altitude| !(-2000..=100_000).contains(&altitude))
    {
        return Err(FormError::generic(Some(SettingsSection::Traffic)));
    }
    if matches!(
        (
            candidate.minimum_altitude_feet,
            candidate.maximum_altitude_feet
        ),
        (Some(minimum), Some(maximum)) if minimum > maximum
    ) {
        return Err(FormError {
            section: Some(SettingsSection::Traffic),
            message: "Minimum altitude cannot exceed maximum altitude.",
        });
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

enum SearchState<'a> {
    Idle,
    Results(&'a [GeocodeResult]),
    Empty,
    Unavailable,
}

#[derive(Clone, Copy)]
enum PageNotice {
    Saved,
    InvalidSettings(FormError),
    SaveFailed,
}

fn render_notice(notice: Option<PageNotice>) -> String {
    match notice {
        Some(PageNotice::Saved) => {
            r#"<p class="notice notice--success" role="status">Radar settings applied</p>"#
                .to_owned()
        }
        Some(PageNotice::InvalidSettings(error)) => format!(
            r#"<p class="notice notice--error" role="alert">{}</p>"#,
            error.message
        ),
        Some(PageNotice::SaveFailed) => r#"<p class="notice notice--error" role="alert">Plane Radar could not save those settings. Try again.</p>"#.to_owned(),
        None => String::new(),
    }
}

fn render_status(settings: &RadarSettings) -> String {
    let Some(location) = &settings.location else {
        return r#"<div class="radar-status radar-status--setup" role="status">
<span class="status-mark" aria-hidden="true"></span>
<div><strong>Setup required</strong><span>Choose the radar's home location</span></div>
</div>"#
            .to_owned();
    };
    let location_name = if location.label.trim().is_empty() {
        format!("{:.4}, {:.4}", location.latitude, location.longitude)
    } else {
        escape_html(&location.label)
    };
    format!(
        r#"<div class="radar-status radar-status--ready" role="status">
<span class="status-mark" aria-hidden="true"></span>
<div><strong>Radar configured</strong><span>{location_name}</span></div>
</div>"#
    )
}

fn render_search_results(settings: &RadarSettings, csrf: &str, search: SearchState<'_>) -> String {
    let SearchState::Results(results) = search else {
        return match search {
            SearchState::Unavailable => {
                r#"<p class="notice notice--error" role="alert">Search unavailable. Enter coordinates manually.</p>"#
                    .to_owned()
            }
            SearchState::Empty => r#"<div class="empty-results"><strong>No matching places found</strong><span>Enter coordinates manually instead.</span></div>"#.to_owned(),
            SearchState::Idle | SearchState::Results(_) => String::new(),
        };
    };
    if results.is_empty() {
        return String::new();
    }

    let units = match settings.units {
        Units::Kilometres => "km",
        Units::Miles => "mi",
    };
    let mut forms = String::new();
    for result in results {
        let display_name = escape_html(&result.display_name);
        let result_label = escape_html(&result.location.label);
        forms.push_str(&format!(
            r#"<form class="search-result" action="/settings" method="post">
<input type="hidden" name="csrf_token" value="{csrf}">
<input type="hidden" name="latitude" value="{latitude}">
<input type="hidden" name="longitude" value="{longitude}">
<input type="hidden" name="label" value="{result_label}">
<input type="hidden" name="units" value="{units}">
<input type="hidden" name="show_runways" value="{show_runways}">
<input type="hidden" name="range_index" value="{range_index}">
<span>{display_name}</span>
<button type="submit">Use location</button>
</form>"#,
            latitude = result.location.latitude,
            longitude = result.location.longitude,
            show_runways = settings.show_runways,
            range_index = settings.range_index,
        ));
    }
    let match_label = if results.len() == 1 {
        "1 match".to_owned()
    } else {
        format!("{} matches", results.len())
    };
    format!(
        r#"<section class="search-results" aria-labelledby="search-results-title">
<div class="result-heading"><h3 id="search-results-title">Search results</h3><span>{match_label}</span></div>
{forms}
</section>"#
    )
}

fn render_units(settings: &RadarSettings) -> String {
    let kilometres_checked = if settings.units == Units::Kilometres {
        " checked"
    } else {
        ""
    };
    let miles_checked = if settings.units == Units::Miles {
        " checked"
    } else {
        ""
    };
    format!(
        r#"<div class="segmented segmented--units">
<label class="segment"><input type="radio" name="units" value="km"{kilometres_checked}><span>Kilometres</span></label>
<label class="segment"><input type="radio" name="units" value="mi"{miles_checked}><span>Miles</span></label>
</div>"#
    )
}

fn render_range_options(settings: &RadarSettings) -> String {
    (0_u8..=3)
        .map(|index| {
            let mut label = range_preset(index)
                .map(|preset| format_range_label(preset, settings.units))
                .expect("the web form only renders supported range indices");
            label.insert(label.len() - 2, ' ');
            let checked = if settings.range_index == index {
                " checked"
            } else {
                ""
            };
            format!(
                r#"<label class="segment"><input type="radio" name="range_index" value="{index}"{checked}><span>{label}</span></label>"#
            )
        })
        .collect()
}

fn render_page(
    settings: &RadarSettings,
    local_url: &str,
    csrf_token: &str,
    search: SearchState<'_>,
    notice: Option<PageNotice>,
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
    let runways_checked = if settings.show_runways {
        " checked"
    } else {
        ""
    };
    let checked = |value| if value { " checked" } else { "" };
    let selected = |value| if value { " selected" } else { "" };
    let error_section = match notice {
        Some(PageNotice::InvalidSettings(error)) => error.section,
        _ => None,
    };
    let aircraft_open = if settings.show_callsign != RadarSettings::default().show_callsign
        || settings.show_route
        || settings.show_expanded_model
        || error_section == Some(SettingsSection::Aircraft)
    {
        " open"
    } else {
        ""
    };
    let footer_open = if settings.footer != Default::default()
        || error_section == Some(SettingsSection::Footer)
    {
        " open"
    } else {
        ""
    };
    let traffic_open =
        if settings.altitude_filter_active() || error_section == Some(SettingsSection::Traffic) {
            " open"
        } else {
            ""
        };
    let show_callsign_checked = checked(settings.show_callsign);
    let show_route_checked = checked(settings.show_route);
    let show_expanded_model_checked = checked(settings.show_expanded_model);
    let footer_condition_checked = checked(settings.footer.show_condition);
    let footer_temperature_checked = checked(settings.footer.show_temperature);
    let footer_humidity_checked = checked(settings.footer.show_humidity);
    let footer_time_checked = checked(settings.footer.show_time);
    let footer_date_checked = checked(settings.footer.show_date);
    let celsius_checked = checked(settings.footer.temperature_unit == TemperatureUnit::Celsius);
    let fahrenheit_checked =
        checked(settings.footer.temperature_unit == TemperatureUnit::Fahrenheit);
    let radar_local_checked = checked(settings.footer.time_zone == TimeZone::RadarLocal);
    let zulu_checked = checked(settings.footer.time_zone == TimeZone::Zulu);
    let twelve_checked = checked(settings.footer.clock_format == ClockFormat::Twelve);
    let twenty_four_checked = checked(settings.footer.clock_format == ClockFormat::TwentyFour);
    let minimum_altitude = settings
        .minimum_altitude_feet
        .map(|value| value.to_string())
        .unwrap_or_default();
    let maximum_altitude = settings
        .maximum_altitude_feet
        .map(|value| value.to_string())
        .unwrap_or_default();
    let scale_80_selected = selected(settings.radar_text_scale_percent == 80);
    let scale_90_selected = selected(settings.radar_text_scale_percent == 90);
    let scale_100_selected = selected(settings.radar_text_scale_percent == 100);
    let scale_110_selected = selected(settings.radar_text_scale_percent == 110);
    let scale_120_selected = selected(settings.radar_text_scale_percent == 120);
    let scale_130_selected = selected(settings.radar_text_scale_percent == 130);
    let manual_open = if settings.location.is_none() {
        " open"
    } else {
        ""
    };
    let status = render_status(settings);
    let notice = render_notice(notice);
    let search_results = render_search_results(settings, &csrf, search);
    let units = render_units(settings);
    let range_options = render_range_options(settings);

    format!(
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1, viewport-fit=cover">
<title>Plane Radar local control</title>
<style>
:root {{
  color-scheme: dark;
  --canvas: oklch(14% 0.012 155);
  --surface: oklch(18% 0.014 155);
  --surface-raised: oklch(22% 0.016 155);
  --surface-active: oklch(28% 0.025 155);
  --text: oklch(94% 0.012 100);
  --text-muted: oklch(72% 0.022 165);
  --text-faint: oklch(60% 0.018 165);
  --border: oklch(35% 0.022 155);
  --border-strong: oklch(48% 0.04 150);
  --accent: oklch(78% 0.14 145);
  --accent-hover: oklch(84% 0.15 145);
  --accent-ink: oklch(18% 0.035 145);
  --warning: oklch(80% 0.13 80);
  --warning-surface: oklch(23% 0.035 80);
  --danger: oklch(74% 0.17 28);
  --danger-surface: oklch(22% 0.045 28);
  --success-surface: oklch(23% 0.04 145);
  --radar-line: oklch(31% 0.045 150);
  --focus: oklch(88% 0.14 145);
  --space-xs: 0.25rem;
  --space-sm: 0.5rem;
  --space-md: 0.75rem;
  --space-lg: 1.5rem;
  --space-xl: 2rem;
  --space-2xl: 3rem;
  --radius-sm: 0.5rem;
  --radius-md: 0.75rem;
  --ease-out: cubic-bezier(0.25, 1, 0.5, 1);
}}

* {{ box-sizing: border-box; }}

html {{ background: var(--canvas); }}

body {{
  min-width: 20rem;
  min-height: 100svh;
  margin: 0;
  background: var(--canvas);
  color: var(--text);
  font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", system-ui, sans-serif;
  font-size: 1rem;
  font-weight: 400;
  font-kerning: normal;
  letter-spacing: 0.01em;
  line-height: 1.55;
}}

button, input, select, summary {{
  font: inherit;
}}

button, select, input[type="search"], input[type="number"], input[type="text"],
input:not([type]) {{
  min-height: 44px;
}}

button, a, input, select, summary {{
  -webkit-tap-highlight-color: transparent;
}}

button, input, select, summary, a {{
  transition: color 160ms var(--ease-out), background-color 160ms var(--ease-out),
    border-color 160ms var(--ease-out), transform 160ms var(--ease-out);
}}

button:focus-visible, input:focus-visible, select:focus-visible, summary:focus-visible, a:focus-visible {{
  outline: 3px solid var(--focus);
  outline-offset: 3px;
}}

button {{
  width: 100%;
  min-height: 44px;
  border: 1px solid var(--border-strong);
  border-radius: var(--radius-sm);
  background: var(--surface-raised);
  color: var(--text);
  cursor: pointer;
  font-weight: 700;
  line-height: 1.2;
  padding: 0.75rem 1rem;
}}

button:active {{ transform: translateY(1px); }}
button:disabled {{ cursor: not-allowed; opacity: 0.5; }}

a {{ color: var(--accent); text-underline-offset: 0.22em; }}

h1, h2, h3, p {{ margin: 0; }}
h1, h2, h3 {{ text-wrap: balance; }}
p {{ max-width: 68ch; text-wrap: pretty; }}

.shell {{
  width: min(100%, 74rem);
  margin: 0 auto;
  padding: max(var(--space-lg), env(safe-area-inset-top))
    max(var(--space-lg), env(safe-area-inset-right))
    max(var(--space-2xl), env(safe-area-inset-bottom))
    max(var(--space-lg), env(safe-area-inset-left));
}}

.masthead {{
  position: relative;
  display: grid;
  grid-template-columns: auto minmax(0, 1fr);
  gap: var(--space-lg);
  align-items: center;
  overflow: hidden;
  padding: var(--space-sm) 0 var(--space-xl);
  border-bottom: 1px solid var(--border);
}}

.brand-lockup {{ display: grid; gap: 0.125rem; min-width: 0; }}

.eyebrow {{
  color: var(--accent);
  font-size: 0.75rem;
  font-weight: 800;
  letter-spacing: 0.12em;
  line-height: 1.2;
  text-transform: uppercase;
}}

h1 {{
  margin-left: -0.04em;
  color: var(--text);
  font-size: 2rem;
  font-weight: 750;
  letter-spacing: -0.035em;
  line-height: 1.05;
}}

h2 {{
  color: var(--text);
  font-size: 1.25rem;
  font-weight: 750;
  letter-spacing: -0.015em;
  line-height: 1.2;
}}

h3 {{ font-size: 1rem; font-weight: 700; line-height: 1.25; }}

.radar-mark {{
  position: relative;
  display: block;
  width: 4rem;
  height: 4rem;
  border: 1px solid var(--border-strong);
  border-radius: 50%;
}}

.radar-mark::before {{
  position: absolute;
  inset: 0.85rem;
  border: 1px solid var(--radar-line);
  border-radius: 50%;
  content: "";
}}

.radar-mark::after {{
  position: absolute;
  inset: calc(50% - 0.2rem);
  border-radius: 50%;
  background: var(--accent);
  content: "";
}}

.radar-mark i::before, .radar-mark i::after {{
  position: absolute;
  background: var(--radar-line);
  content: "";
}}

.radar-mark i::before {{ top: 0; bottom: 0; left: calc(50% - 0.5px); width: 1px; }}
.radar-mark i::after {{ right: 0; left: 0; top: calc(50% - 0.5px); height: 1px; }}

.device-url {{
  grid-column: 1 / -1;
  display: grid;
  gap: 0.125rem;
  width: fit-content;
  max-width: 100%;
  color: var(--text-muted);
  font-size: 0.875rem;
  overflow-wrap: anywhere;
  text-decoration-color: var(--border-strong);
}}

.device-url span {{
  color: var(--text-faint);
  font-size: 0.6875rem;
  font-weight: 800;
  letter-spacing: 0.1em;
  text-transform: uppercase;
}}

.radar-status {{
  display: flex;
  gap: var(--space-md);
  align-items: center;
  padding: var(--space-lg) 0;
  border-bottom: 1px solid var(--border);
}}

.radar-status div {{ display: grid; gap: 0.125rem; min-width: 0; }}
.radar-status strong {{ font-size: 0.875rem; }}
.radar-status span:not(.status-mark) {{
  color: var(--text-muted);
  font-size: 0.875rem;
  overflow-wrap: anywhere;
}}

.status-mark {{
  flex: 0 0 auto;
  width: 0.75rem;
  height: 0.75rem;
  border: 2px solid currentColor;
  border-radius: 50%;
  box-shadow: inset 0 0 0 2px var(--canvas);
}}

.radar-status--setup .status-mark {{ color: var(--warning); background: var(--warning); }}
.radar-status--ready .status-mark {{ color: var(--accent); background: var(--accent); }}

.notice {{
  display: flex;
  gap: var(--space-md);
  align-items: flex-start;
  max-width: none;
  margin-top: var(--space-lg);
  padding: var(--space-md) var(--space-lg);
  border: 1px solid currentColor;
  border-radius: var(--radius-sm);
  font-size: 0.9375rem;
  font-weight: 650;
}}

.notice::before {{
  flex: 0 0 auto;
  font-weight: 900;
}}

.notice--error {{ color: var(--danger); background: var(--danger-surface); }}
.notice--error::before {{ content: "!"; }}
.notice--success {{ color: var(--accent); background: var(--success-surface); }}
.notice--success::before {{ content: "✓"; }}

.console-grid {{
  display: grid;
  grid-template-areas:
    "location"
    "manual"
    "preferences";
}}

.location {{
  grid-area: location;
  display: grid;
  gap: var(--space-lg);
  margin: 0;
  padding: var(--space-xl) 0;
}}

.location > p, .preferences > p, fieldset > p {{
  color: var(--text-muted);
  font-size: 0.9375rem;
}}

.location > form {{ display: grid; gap: var(--space-md); }}

label, legend {{ color: var(--text); font-size: 0.875rem; font-weight: 700; }}

input[type="search"], input[type="number"], input:not([type]), select {{
  width: 100%;
  min-width: 0;
  border: 1px solid var(--border-strong);
  border-radius: var(--radius-sm);
  background: var(--surface);
  color: var(--text);
  padding: 0.65rem 0.75rem;
}}

input::placeholder {{ color: var(--text-faint); opacity: 1; }}
input:hover, select:hover {{ border-color: var(--text-muted); }}

.location > form button {{ margin-top: var(--space-xs); }}

.location > form + p {{
  max-width: 62ch;
  color: var(--text-faint);
  font-size: 0.75rem;
  line-height: 1.5;
}}

.search-results {{
  display: grid;
  gap: 0;
  margin: var(--space-sm) 0 0;
  border-bottom: 1px solid var(--border);
}}

.result-heading {{
  display: flex;
  gap: var(--space-md);
  align-items: baseline;
  justify-content: space-between;
  padding-bottom: var(--space-md);
}}

.result-heading span {{ color: var(--text-faint); font-size: 0.75rem; }}

.search-result {{
  display: grid;
  gap: var(--space-md);
  padding: var(--space-md) 0;
  border-top: 1px solid var(--border);
}}

.search-result > span {{ min-width: 0; color: var(--text-muted); overflow-wrap: anywhere; }}

.empty-results {{
  display: grid;
  gap: var(--space-xs);
  padding: var(--space-lg) 0;
  border-top: 1px solid var(--border);
  border-bottom: 1px solid var(--border);
}}

.empty-results span {{ color: var(--text-muted); font-size: 0.875rem; }}

.settings-form {{ display: contents; }}

.manual {{
  grid-area: manual;
  align-self: start;
  border-top: 1px solid var(--border);
  border-bottom: 1px solid var(--border);
}}

.manual summary {{
  min-height: 44px;
  padding: var(--space-md) 2rem var(--space-md) 0;
  color: var(--text-muted);
  cursor: pointer;
  font-size: 0.875rem;
  font-weight: 700;
}}

.manual[open] summary {{ color: var(--text); }}
.manual-fields {{ display: grid; gap: var(--space-lg); padding: var(--space-md) 0 var(--space-xl); }}

.field {{ display: grid; gap: var(--space-sm); min-width: 0; }}
.field label span {{ color: var(--text-faint); font-weight: 500; }}
.field input, .field select {{ font-variant-numeric: tabular-nums; }}

.preferences {{
  grid-area: preferences;
  display: grid;
  gap: var(--space-xl);
  align-content: start;
  margin: 0;
  padding: var(--space-xl) 0 0;
}}

fieldset {{ display: grid; gap: var(--space-md); min-width: 0; margin: 0; padding: 0; border: 0; }}
legend {{ margin-bottom: var(--space-md); padding: 0; }}
fieldset > p {{ margin-top: calc(var(--space-sm) * -1); }}

.segmented {{
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 1px;
  overflow: hidden;
  border: 1px solid var(--border);
  border-radius: var(--radius-sm);
  background: var(--border);
}}

.segmented--range {{ grid-template-columns: repeat(2, minmax(0, 1fr)); }}

.segment {{ position: relative; display: flex; min-width: 0; cursor: pointer; }}

.segment input {{
  position: absolute;
  width: 1px;
  height: 1px;
  opacity: 0;
}}

.segment span {{
  display: flex;
  flex: 1;
  min-height: 44px;
  align-items: center;
  justify-content: center;
  background: var(--surface);
  color: var(--text-muted);
  font-size: 0.875rem;
  font-variant-numeric: tabular-nums;
  font-weight: 700;
  padding: var(--space-sm);
  text-align: center;
}}

.segment input:checked + span {{ background: var(--accent); color: var(--accent-ink); }}

.segment input:focus-visible + span {{
  position: relative;
  z-index: 1;
  outline: 3px solid var(--focus);
  outline-offset: -3px;
}}

.switch {{
  min-height: 44px;
  display: grid;
  grid-template-columns: auto minmax(0, 1fr);
  gap: var(--space-md);
  align-items: center;
  cursor: pointer;
}}

.switch > input {{ position: absolute; width: 1px; height: 1px; opacity: 0; }}

.switch-track {{
  position: relative;
  width: 3rem;
  height: 1.75rem;
  border: 1px solid var(--border-strong);
  border-radius: 999px;
  background: var(--surface-raised);
}}

.switch-track::after {{
  position: absolute;
  top: 0.25rem;
  left: 0.25rem;
  width: 1.125rem;
  height: 1.125rem;
  border-radius: 50%;
  background: var(--text-muted);
  content: "";
  transition: background-color 160ms var(--ease-out), transform 160ms var(--ease-out);
}}

.switch input:checked + .switch-track {{ border-color: var(--accent); background: var(--accent); }}
.switch input:checked + .switch-track::after {{ background: var(--accent-ink); transform: translateX(1.2rem); }}
.switch input:focus-visible + .switch-track {{ outline: 3px solid var(--focus); outline-offset: 3px; }}

.switch-copy {{ display: grid; gap: 0.125rem; }}
.switch-copy strong {{ font-size: 0.875rem; }}
.switch-copy small {{ color: var(--text-muted); font-size: 0.8125rem; font-weight: 400; line-height: 1.45; }}

.option-groups {{
  display: grid;
  gap: 0;
  border-top: 1px solid var(--border);
}}

.option-group {{
  min-width: 0;
  border-bottom: 1px solid var(--border);
}}

.option-group summary {{
  min-height: 44px;
  padding: var(--space-md) 0;
  color: var(--text-muted);
  cursor: pointer;
  font-size: 0.9375rem;
  font-weight: 750;
  overflow-wrap: anywhere;
}}

.option-group[open] summary {{ color: var(--text); }}

.option-content {{
  display: grid;
  gap: var(--space-lg);
  min-width: 0;
  padding: var(--space-sm) 0 var(--space-xl);
}}

.disclosure-copy {{
  color: var(--text-muted);
  font-size: 0.8125rem;
  line-height: 1.5;
  overflow-wrap: anywhere;
}}

.paired-fields {{ display: grid; gap: var(--space-lg); min-width: 0; }}
.compact-fieldset {{ gap: var(--space-sm); }}
.compact-fieldset legend {{ margin-bottom: var(--space-sm); }}

.button-primary {{
  border-color: var(--accent);
  background: var(--accent);
  color: var(--accent-ink);
}}

@media (hover: hover) {{
  button:hover {{ border-color: var(--text-muted); background: var(--surface-active); }}
  .button-primary:hover {{ border-color: var(--accent-hover); background: var(--accent-hover); }}
  a:hover {{ color: var(--accent-hover); }}
  .manual summary:hover, .option-group summary:hover {{ color: var(--accent); }}
}}

@media (min-width: 34rem) {{
  .location > form {{ grid-template-columns: minmax(0, 1fr) auto; align-items: end; }}
  .location > form label {{ grid-column: 1 / -1; }}
  .location > form button {{ width: auto; min-width: 8rem; margin-top: 0; }}
  .search-result {{ grid-template-columns: minmax(0, 1fr) auto; align-items: center; }}
  .search-result button {{ width: auto; }}
  .manual-fields {{ grid-template-columns: repeat(2, minmax(0, 1fr)); }}
  .paired-fields {{ grid-template-columns: repeat(2, minmax(0, 1fr)); }}
  .field--wide {{ grid-column: 1 / -1; }}
  .segmented--range {{ grid-template-columns: repeat(4, minmax(0, 1fr)); }}
}}

@media (min-width: 52rem) {{
  .shell {{ padding-top: var(--space-xl); }}
  .masthead {{ grid-template-columns: auto minmax(0, 1fr) auto; }}
  .device-url {{ grid-column: auto; justify-items: end; text-align: right; }}
  .console-grid {{
    grid-template-columns: minmax(0, 1.35fr) minmax(18rem, 0.85fr);
    grid-template-areas:
      "location preferences"
      "manual preferences";
    column-gap: var(--space-2xl);
    align-items: start;
  }}
  .location {{ padding-top: var(--space-2xl); }}
  .manual {{ margin-bottom: var(--space-2xl); }}
  .preferences {{
    min-height: 100%;
    padding: var(--space-2xl) 0 var(--space-2xl) var(--space-2xl);
    border-left: 1px solid var(--border);
  }}
}}

@media (prefers-reduced-motion: reduce) {{
  *, *::before, *::after {{
    scroll-behavior: auto !important;
    transition-duration: 0.01ms !important;
  }}
}}
</style>
</head>
<body>
<main class="shell">
<header class="masthead">
<span class="radar-mark" aria-hidden="true"><i></i></span>
<div class="brand-lockup">
<p class="eyebrow">Local control</p>
<h1>Plane Radar</h1>
</div>
<a class="device-url" href="{local_url}"><span>Device</span>{local_url}</a>
</header>
{status}
{notice}
<div class="console-grid">
<section class="location" aria-labelledby="location-title">
<h2 id="location-title">Radar location</h2>
<p>Search for the place this radar should watch.</p>
<form action="/search" method="post">
<input type="hidden" name="csrf_token" value="{csrf}">
<label for="place-search">Place or address</label>
<input id="place-search" name="query" type="search" autocomplete="street-address" required>
<button type="submit">Search places</button>
</form>
<p>Your submitted search text is sent to OpenStreetMap. Search data © OpenStreetMap contributors.</p>
{search_results}
</section>
<form class="settings-form" action="/settings" method="post">
<input type="hidden" name="csrf_token" value="{csrf}">
<details class="manual"{manual_open}>
<summary>Manual coordinates</summary>
<div class="manual-fields">
<div class="field">
<label for="latitude">Latitude</label>
<input id="latitude" name="latitude" value="{latitude}" inputmode="decimal" type="number" min="-90" max="90" step="any" required>
</div>
<div class="field">
<label for="longitude">Longitude</label>
<input id="longitude" name="longitude" value="{longitude}" inputmode="decimal" type="number" min="-180" max="180" step="any" required>
</div>
<div class="field field--wide">
<label for="place-name">Place name <span>(optional)</span></label>
<input id="place-name" name="label" value="{label}" autocomplete="off">
</div>
</div>
</details>
<section class="preferences" aria-labelledby="preferences-title">
<h2 id="preferences-title">Radar display</h2>
<fieldset>
<legend>Units</legend>
{units}
</fieldset>
<fieldset>
<legend>Range</legend>
<p>Distance shown at the third radar ring.</p>
<div class="segmented segmented--range">{range_options}</div>
</fieldset>
<label class="field" for="radar-text-size">
Radar text size
<select id="radar-text-size" name="radar_text_scale_percent">
<option value="80"{scale_80_selected}>80% — Small</option>
<option value="90"{scale_90_selected}>90%</option>
<option value="100"{scale_100_selected}>100% — Current</option>
<option value="110"{scale_110_selected}>110%</option>
<option value="120"{scale_120_selected}>120%</option>
<option value="130"{scale_130_selected}>130% — Large</option>
</select>
</label>
<input type="hidden" name="show_runways_present" value="true">
<label class="switch">
<input type="checkbox" name="show_runways" value="true"{runways_checked}>
<span class="switch-track" aria-hidden="true"></span>
<span class="switch-copy"><strong>Show runways</strong><small>Include nearby airport runways on the radar.</small></span>
</label>
<div class="option-groups">
<details class="option-group" data-section="aircraft"{aircraft_open}>
<summary>Aircraft labels</summary>
<div class="option-content">
<p class="disclosure-copy">Route lookups send the flight callsign to ADSBDB. Expanded model lookups send the aircraft identifier. Enabling both may combine them in one request.</p>
<input type="hidden" name="show_callsign_present" value="true">
<label class="switch">
<input type="checkbox" name="show_callsign" value="true"{show_callsign_checked}>
<span class="switch-track" aria-hidden="true"></span>
<span class="switch-copy"><strong>Show callsign</strong><small>Use the flight callsign when one is available.</small></span>
</label>
<input type="hidden" name="show_route_present" value="true">
<label class="switch">
<input type="checkbox" name="show_route" value="true"{show_route_checked}>
<span class="switch-track" aria-hidden="true"></span>
<span class="switch-copy"><strong>Show origin and destination</strong><small>Add the flight route below the callsign.</small></span>
</label>
<input type="hidden" name="show_expanded_model_present" value="true">
<label class="switch">
<input type="checkbox" name="show_expanded_model" value="true"{show_expanded_model_checked}>
<span class="switch-track" aria-hidden="true"></span>
<span class="switch-copy"><strong>Show expanded aircraft model</strong><small>Use a longer aircraft model name when available.</small></span>
</label>
</div>
</details>
<details class="option-group" data-section="footer"{footer_open}>
<summary>Footer</summary>
<div class="option-content">
<p class="disclosure-copy">Weather fields and radar-local time send the configured radar coordinates to Open-Meteo. When only Zulu time or date is enabled, no weather request is made.</p>
<input type="hidden" name="footer_show_condition_present" value="true">
<label class="switch">
<input type="checkbox" name="footer_show_condition" value="true"{footer_condition_checked}>
<span class="switch-track" aria-hidden="true"></span>
<span class="switch-copy"><strong>Weather condition</strong><small>Show the current conditions.</small></span>
</label>
<input type="hidden" name="footer_show_temperature_present" value="true">
<label class="switch">
<input type="checkbox" name="footer_show_temperature" value="true"{footer_temperature_checked}>
<span class="switch-track" aria-hidden="true"></span>
<span class="switch-copy"><strong>Temperature</strong><small>Show the current temperature.</small></span>
</label>
<input type="hidden" name="footer_show_humidity_present" value="true">
<label class="switch">
<input type="checkbox" name="footer_show_humidity" value="true"{footer_humidity_checked}>
<span class="switch-track" aria-hidden="true"></span>
<span class="switch-copy"><strong>Humidity</strong><small>Show the current relative humidity.</small></span>
</label>
<input type="hidden" name="footer_show_time_present" value="true">
<label class="switch">
<input type="checkbox" name="footer_show_time" value="true"{footer_time_checked}>
<span class="switch-track" aria-hidden="true"></span>
<span class="switch-copy"><strong>Time</strong><small>Show the time in the footer.</small></span>
</label>
<input type="hidden" name="footer_show_date_present" value="true">
<label class="switch">
<input type="checkbox" name="footer_show_date" value="true"{footer_date_checked}>
<span class="switch-track" aria-hidden="true"></span>
<span class="switch-copy"><strong>Date</strong><small>Show the date in the footer.</small></span>
</label>
<fieldset class="compact-fieldset">
<legend>Temperature unit</legend>
<div class="segmented segmented--units">
<label class="segment"><input type="radio" name="temperature_unit" value="celsius"{celsius_checked}><span>Celsius</span></label>
<label class="segment"><input type="radio" name="temperature_unit" value="fahrenheit"{fahrenheit_checked}><span>Fahrenheit</span></label>
</div>
</fieldset>
<fieldset class="compact-fieldset">
<legend>Time zone</legend>
<div class="segmented segmented--units">
<label class="segment"><input type="radio" name="time_zone" value="radar_local"{radar_local_checked}><span>Radar location</span></label>
<label class="segment"><input type="radio" name="time_zone" value="zulu"{zulu_checked}><span>Zulu</span></label>
</div>
</fieldset>
<fieldset class="compact-fieldset">
<legend>Clock format</legend>
<div class="segmented segmented--units">
<label class="segment"><input type="radio" name="clock_format" value="twelve"{twelve_checked}><span>12-hour</span></label>
<label class="segment"><input type="radio" name="clock_format" value="twenty_four"{twenty_four_checked}><span>24-hour</span></label>
</div>
</fieldset>
</div>
</details>
<details class="option-group" data-section="traffic"{traffic_open}>
<summary>Traffic filter</summary>
<div class="option-content">
<p class="disclosure-copy">Limit the radar to aircraft within an altitude range. Leave either value blank for no limit.</p>
<div class="paired-fields">
<label class="field" for="minimum-altitude">Minimum altitude <span>(feet)</span>
<input id="minimum-altitude" name="minimum_altitude_feet" value="{minimum_altitude}" inputmode="numeric" type="number" min="-2000" max="100000" step="1">
</label>
<label class="field" for="maximum-altitude">Maximum altitude <span>(feet)</span>
<input id="maximum-altitude" name="maximum_altitude_feet" value="{maximum_altitude}" inputmode="numeric" type="number" min="-2000" max="100000" step="1">
</label>
</div>
</div>
</details>
</div>
<button class="button-primary" type="submit">Apply settings</button>
</section>
</form>
</div>
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

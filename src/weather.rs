use std::time::Duration;

use serde_json::Value;
use thiserror::Error;
use url::Url;

use crate::http::{HttpClient, HttpError, HttpRequest};
use crate::model::{
    ClockFormat, EnvironmentReading, FooterSettings, Location, TemperatureUnit, TimeZone,
};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const READ_TIMEOUT: Duration = Duration::from_secs(4);
const MAX_RESPONSE_BYTES: usize = 64 * 1024;

pub const ENVIRONMENT_STALE_AFTER: Duration = Duration::from_secs(45 * 60);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FooterTone {
    Status,
    Condition,
    Temperature,
    Humidity,
    Time,
    Date,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FooterItem {
    pub text: String,
    pub tone: FooterTone,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct FooterContent {
    pub environment: Vec<FooterItem>,
    pub temporal: Vec<FooterItem>,
}

#[derive(Debug, Error)]
pub enum WeatherError {
    #[error("Open-Meteo HTTP request failed: {0}")]
    Http(#[from] HttpError),
    #[error("Open-Meteo returned HTTP status {0}")]
    Status(u16),
    #[error("Open-Meteo response was not valid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("Open-Meteo response schema was invalid: {0}")]
    Schema(&'static str),
}

pub struct WeatherClient<C> {
    http: C,
    provider_base: String,
}

impl<C: HttpClient> WeatherClient<C> {
    pub fn with_provider_base(http: C, provider_base: String) -> Self {
        Self {
            http,
            provider_base: provider_base.trim_end_matches('/').to_owned(),
        }
    }

    pub fn fetch(
        &self,
        location: &Location,
        fetched_at: Duration,
    ) -> Result<EnvironmentReading, WeatherError> {
        validate_provider_base(&self.provider_base)?;
        let response = self.http.execute(HttpRequest {
            url: self.provider_base.clone(),
            query: vec![
                ("latitude".to_owned(), location.latitude.to_string()),
                ("longitude".to_owned(), location.longitude.to_string()),
                (
                    "current".to_owned(),
                    "temperature_2m,relative_humidity_2m,weather_code".to_owned(),
                ),
                ("temperature_unit".to_owned(), "celsius".to_owned()),
                ("timezone".to_owned(), "auto".to_owned()),
                ("forecast_days".to_owned(), "1".to_owned()),
            ],
            headers: Vec::new(),
            connect_timeout: CONNECT_TIMEOUT,
            read_timeout: READ_TIMEOUT,
            max_response_bytes: MAX_RESPONSE_BYTES,
            verify_tls: true,
        })?;

        if response.status != 200 {
            return Err(WeatherError::Status(response.status));
        }

        parse_response(&response.body, fetched_at)
    }
}

fn validate_provider_base(provider_base: &str) -> Result<(), WeatherError> {
    let url = Url::parse(provider_base)
        .map_err(|_| WeatherError::Schema("provider base must be a valid HTTPS URL"))?;
    if url.scheme() != "https"
        || !url.has_host()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(WeatherError::Schema(
            "provider base must be a valid HTTPS URL",
        ));
    }
    Ok(())
}

fn parse_response(body: &[u8], fetched_at: Duration) -> Result<EnvironmentReading, WeatherError> {
    let root: Value = serde_json::from_slice(body)?;
    let root = root
        .as_object()
        .ok_or(WeatherError::Schema("top level must be an object"))?;
    let current = root
        .get("current")
        .and_then(Value::as_object)
        .ok_or(WeatherError::Schema("current object is required"))?;

    let temperature_celsius = current
        .get("temperature_2m")
        .and_then(Value::as_f64)
        .filter(|value| value.is_finite())
        .ok_or(WeatherError::Schema(
            "temperature_2m must be a finite number",
        ))?;
    let humidity_percent = current
        .get("relative_humidity_2m")
        .and_then(checked_json_u8)
        .filter(|value| *value <= 100)
        .ok_or(WeatherError::Schema(
            "relative_humidity_2m must be an integer from 0 through 100",
        ))?;
    let weather_code = current
        .get("weather_code")
        .and_then(checked_json_u8)
        .ok_or(WeatherError::Schema(
            "weather_code must be an unsigned 8-bit integer",
        ))?;
    let utc_offset_seconds = root
        .get("utc_offset_seconds")
        .and_then(checked_json_i32)
        .filter(|value| (-86_400..=86_400).contains(value))
        .ok_or(WeatherError::Schema(
            "utc_offset_seconds must be an integer from -86400 through 86400",
        ))?;

    Ok(EnvironmentReading {
        temperature_celsius,
        humidity_percent,
        weather_code,
        utc_offset_seconds,
        fetched_at,
    })
}

fn checked_json_u8(value: &Value) -> Option<u8> {
    let value = finite_whole_number(value)?;
    (0.0..=f64::from(u8::MAX))
        .contains(&value)
        .then_some(value as u8)
}

fn checked_json_i32(value: &Value) -> Option<i32> {
    let value = finite_whole_number(value)?;
    (f64::from(i32::MIN)..=f64::from(i32::MAX))
        .contains(&value)
        .then_some(value as i32)
}

fn finite_whole_number(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .filter(|value| value.is_finite() && value.fract() == 0.0)
}

pub fn environment_is_stale(reading: Option<&EnvironmentReading>, monotonic_now: Duration) -> bool {
    reading.is_some_and(|value| {
        monotonic_now.saturating_sub(value.fetched_at) >= ENVIRONMENT_STALE_AFTER
    })
}

pub fn footer_content(
    settings: &FooterSettings,
    reading: Option<&EnvironmentReading>,
    monotonic_now: Duration,
    unix_seconds: u64,
) -> FooterContent {
    let mut content = FooterContent::default();
    let weather_selected =
        settings.show_condition || settings.show_temperature || settings.show_humidity;

    if weather_selected && reading.is_none() {
        content
            .environment
            .push(footer_item("WX --", FooterTone::Status));
    } else if let Some(reading) = reading {
        if settings.needs_environment() && environment_is_stale(Some(reading), monotonic_now) {
            content
                .environment
                .push(footer_item("WX STALE", FooterTone::Status));
        }
        if settings.show_condition {
            content.environment.push(footer_item(
                weather_code_label(reading.weather_code),
                FooterTone::Condition,
            ));
        }
        if settings.show_temperature {
            content.environment.push(footer_item(
                &format_temperature(reading.temperature_celsius, settings.temperature_unit),
                FooterTone::Temperature,
            ));
        }
        if settings.show_humidity {
            content.environment.push(footer_item(
                &format!("RH{}%", reading.humidity_percent),
                FooterTone::Humidity,
            ));
        }
    }

    if settings.show_time || settings.show_date {
        let date_time = display_date_time(settings.time_zone, reading, unix_seconds);
        if settings.show_time {
            let text = date_time
                .map(|value| format_time(value, settings.clock_format, settings.time_zone))
                .unwrap_or_else(|| "--:--".to_owned());
            content.temporal.push(FooterItem {
                text,
                tone: FooterTone::Time,
            });
        }
        if settings.show_date {
            let text = date_time
                .map(format_date)
                .unwrap_or_else(|| "-- ---".to_owned());
            content.temporal.push(FooterItem {
                text,
                tone: FooterTone::Date,
            });
        }
    }

    content
}

fn footer_item(text: &str, tone: FooterTone) -> FooterItem {
    FooterItem {
        text: text.to_owned(),
        tone,
    }
}

fn weather_code_label(weather_code: u8) -> &'static str {
    match weather_code {
        0 => "CLR",
        1 => "FEW",
        2 => "SCT",
        3 => "OVC",
        45 | 48 => "FG",
        51 => "-DZ",
        53 => "DZ",
        55 => "+DZ",
        56 => "-FZDZ",
        57 => "+FZDZ",
        61 => "-RA",
        63 => "RA",
        65 => "+RA",
        66 => "-FZRA",
        67 => "+FZRA",
        71 => "-SN",
        73 => "SN",
        75 => "+SN",
        77 => "SG",
        80 => "-SHRA",
        81 => "SHRA",
        82 => "+SHRA",
        85 => "-SHSN",
        86 => "+SHSN",
        95 => "TS",
        96 | 99 => "TSGR",
        _ => "WX",
    }
}

fn format_temperature(temperature_celsius: f64, unit: TemperatureUnit) -> String {
    let (temperature, suffix) = match unit {
        TemperatureUnit::Celsius => (temperature_celsius, "°C"),
        TemperatureUnit::Fahrenheit => {
            let temperature_fahrenheit = temperature_celsius.mul_add(9.0 / 5.0, 32.0);
            (temperature_fahrenheit, "°F")
        }
    };
    let rounded = temperature.round();
    let rounded = if rounded == 0.0 { 0.0 } else { rounded };
    format!("{rounded:.0}{suffix}")
}

fn display_date_time(
    time_zone: TimeZone,
    reading: Option<&EnvironmentReading>,
    unix_seconds: u64,
) -> Option<time::OffsetDateTime> {
    let timestamp = i64::try_from(unix_seconds).ok()?;
    let date_time = time::OffsetDateTime::from_unix_timestamp(timestamp).ok()?;
    match time_zone {
        TimeZone::Zulu => Some(date_time),
        TimeZone::RadarLocal => {
            let offset = time::UtcOffset::from_whole_seconds(reading?.utc_offset_seconds).ok()?;
            date_time.checked_to_offset(offset)
        }
    }
}

fn format_time(
    date_time: time::OffsetDateTime,
    clock_format: ClockFormat,
    time_zone: TimeZone,
) -> String {
    let zulu_suffix = if time_zone == TimeZone::Zulu { "Z" } else { "" };
    match clock_format {
        ClockFormat::TwentyFour => {
            format!(
                "{:02}:{:02}{zulu_suffix}",
                date_time.hour(),
                date_time.minute()
            )
        }
        ClockFormat::Twelve => {
            let hour = date_time.hour();
            let display_hour = match hour % 12 {
                0 => 12,
                value => value,
            };
            let meridiem = if hour < 12 { "AM" } else { "PM" };
            format!(
                "{display_hour}:{:02}{meridiem}{zulu_suffix}",
                date_time.minute()
            )
        }
    }
}

fn format_date(date_time: time::OffsetDateTime) -> String {
    let month = match date_time.month() {
        time::Month::January => "JAN",
        time::Month::February => "FEB",
        time::Month::March => "MAR",
        time::Month::April => "APR",
        time::Month::May => "MAY",
        time::Month::June => "JUN",
        time::Month::July => "JUL",
        time::Month::August => "AUG",
        time::Month::September => "SEP",
        time::Month::October => "OCT",
        time::Month::November => "NOV",
        time::Month::December => "DEC",
    };
    format!("{:02} {month}", date_time.day())
}

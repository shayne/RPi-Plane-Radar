use std::sync::{Arc, Mutex};
use std::time::Duration;

use planeradar::http::{HttpClient, HttpError, HttpRequest, HttpResponse};
use planeradar::model::{
    ClockFormat, EnvironmentReading, FooterSettings, Location, TemperatureUnit, TimeZone,
};
use planeradar::weather::{
    ENVIRONMENT_STALE_AFTER, FooterContent, FooterItem, FooterTone, WeatherClient, WeatherError,
    environment_is_stale, footer_content,
};
use time::{Date, Month, PrimitiveDateTime, Time};

#[derive(Clone, Debug)]
struct FakeHttpClient {
    response: Arc<Mutex<Option<Result<HttpResponse, HttpError>>>>,
    requests: Arc<Mutex<Vec<HttpRequest>>>,
}

impl FakeHttpClient {
    fn responding(response: Result<HttpResponse, HttpError>) -> Self {
        Self {
            response: Arc::new(Mutex::new(Some(response))),
            requests: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn requests(&self) -> Vec<HttpRequest> {
        self.requests.lock().expect("requests").clone()
    }
}

impl HttpClient for FakeHttpClient {
    fn execute(&self, request: HttpRequest) -> Result<HttpResponse, HttpError> {
        self.requests.lock().expect("requests").push(request);
        self.response
            .lock()
            .expect("response")
            .take()
            .expect("fake response")
    }
}

fn ok(body: &[u8]) -> Result<HttpResponse, HttpError> {
    Ok(HttpResponse {
        status: 200,
        body: body.to_vec(),
    })
}

fn status(status: u16) -> Result<HttpResponse, HttpError> {
    Ok(HttpResponse {
        status,
        body: b"provider response must not escape errors".to_vec(),
    })
}

fn json_response(value: serde_json::Value) -> Result<HttpResponse, HttpError> {
    ok(&serde_json::to_vec(&value).expect("response JSON"))
}

fn location() -> Location {
    Location {
        latitude: 40.7128,
        longitude: -74.006,
        label: "New York, NY".to_owned(),
    }
}

fn client(
    response: Result<HttpResponse, HttpError>,
) -> (WeatherClient<FakeHttpClient>, FakeHttpClient) {
    let http = FakeHttpClient::responding(response);
    let probe = http.clone();
    (
        WeatherClient::with_provider_base(
            http,
            "https://api.open-meteo.test/v1/forecast///".to_owned(),
        ),
        probe,
    )
}

#[test]
fn current_fixture_maps_celsius_environment_and_exact_request_contract() {
    let (client, http) = client(ok(include_bytes!("fixtures/open_meteo/current.json")));
    let fetched_at = Duration::from_secs(987);

    let reading = client.fetch(&location(), fetched_at).expect("weather");

    assert_eq!(
        reading,
        EnvironmentReading {
            temperature_celsius: 22.2,
            humidity_percent: 54,
            weather_code: 2,
            utc_offset_seconds: -14_400,
            fetched_at,
        }
    );
    let requests = http.requests();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_eq!(request.url, "https://api.open-meteo.test/v1/forecast");
    assert_eq!(
        request.query,
        vec![
            ("latitude".to_owned(), "40.7128".to_owned()),
            ("longitude".to_owned(), "-74.006".to_owned()),
            (
                "current".to_owned(),
                "temperature_2m,relative_humidity_2m,weather_code".to_owned(),
            ),
            ("temperature_unit".to_owned(), "celsius".to_owned()),
            ("timezone".to_owned(), "auto".to_owned()),
            ("forecast_days".to_owned(), "1".to_owned()),
        ]
    );
    assert!(request.headers.is_empty());
    assert_eq!(request.connect_timeout, Duration::from_secs(2));
    assert_eq!(request.read_timeout, Duration::from_secs(4));
    assert_eq!(
        request.connect_timeout + request.read_timeout,
        Duration::from_secs(6)
    );
    assert_eq!(request.max_response_bytes, 64 * 1024);
    assert!(request.verify_tls);
}

#[test]
fn malformed_json_missing_fields_and_wrong_numeric_types_are_rejected() {
    let cases = [
        ok(br#"{"current":"#),
        json_response(serde_json::json!({
            "utc_offset_seconds": -14400
        })),
        json_response(serde_json::json!({
            "utc_offset_seconds": -14400,
            "current": {
                "relative_humidity_2m": 54,
                "weather_code": 2
            }
        })),
        json_response(serde_json::json!({
            "utc_offset_seconds": -14400,
            "current": {
                "temperature_2m": 22.2,
                "weather_code": 2
            }
        })),
        json_response(serde_json::json!({
            "utc_offset_seconds": -14400,
            "current": {
                "temperature_2m": 22.2,
                "relative_humidity_2m": 54
            }
        })),
        json_response(serde_json::json!({
            "utc_offset_seconds": -14400,
            "current": {
                "temperature_2m": "22.2",
                "relative_humidity_2m": 54,
                "weather_code": 2
            }
        })),
        json_response(serde_json::json!({
            "utc_offset_seconds": -14400,
            "current": {
                "temperature_2m": 22.2,
                "relative_humidity_2m": 54.5,
                "weather_code": 2
            }
        })),
        json_response(serde_json::json!({
            "utc_offset_seconds": -14400,
            "current": {
                "temperature_2m": 22.2,
                "relative_humidity_2m": 54,
                "weather_code": 2.5
            }
        })),
        json_response(serde_json::json!({
            "utc_offset_seconds": -14400.5,
            "current": {
                "temperature_2m": 22.2,
                "relative_humidity_2m": 54,
                "weather_code": 2
            }
        })),
    ];

    for response in cases {
        let (client, _) = client(response);
        assert!(
            client.fetch(&location(), Duration::ZERO).is_err(),
            "invalid provider payload must fail"
        );
    }
}

#[test]
fn humidity_weather_code_and_utc_offset_ranges_are_checked() {
    let invalid_values = [
        serde_json::json!({
            "utc_offset_seconds": -14400,
            "current": {
                "temperature_2m": 22.2,
                "relative_humidity_2m": -1,
                "weather_code": 2
            }
        }),
        serde_json::json!({
            "utc_offset_seconds": -14400,
            "current": {
                "temperature_2m": 22.2,
                "relative_humidity_2m": 101,
                "weather_code": 2
            }
        }),
        serde_json::json!({
            "utc_offset_seconds": -14400,
            "current": {
                "temperature_2m": 22.2,
                "relative_humidity_2m": 54,
                "weather_code": -1
            }
        }),
        serde_json::json!({
            "utc_offset_seconds": -14400,
            "current": {
                "temperature_2m": 22.2,
                "relative_humidity_2m": 54,
                "weather_code": 256
            }
        }),
        serde_json::json!({
            "utc_offset_seconds": -86401,
            "current": {
                "temperature_2m": 22.2,
                "relative_humidity_2m": 54,
                "weather_code": 2
            }
        }),
        serde_json::json!({
            "utc_offset_seconds": 86401,
            "current": {
                "temperature_2m": 22.2,
                "relative_humidity_2m": 54,
                "weather_code": 2
            }
        }),
        serde_json::json!({
            "utc_offset_seconds": 2147483648_i64,
            "current": {
                "temperature_2m": 22.2,
                "relative_humidity_2m": 54,
                "weather_code": 2
            }
        }),
    ];

    for value in invalid_values {
        let (client, _) = client(json_response(value));
        assert!(matches!(
            client.fetch(&location(), Duration::ZERO),
            Err(WeatherError::Schema(_))
        ));
    }
}

#[test]
fn offset_boundary_values_and_integer_weather_fields_are_accepted() {
    for offset in [-86_400, 86_400] {
        let (client, _) = client(json_response(serde_json::json!({
            "utc_offset_seconds": offset,
            "current": {
                "temperature_2m": -0.25,
                "relative_humidity_2m": 100,
                "weather_code": 255
            }
        })));
        let reading = client.fetch(&location(), Duration::ZERO).expect("boundary");
        assert_eq!(reading.utc_offset_seconds, offset);
        assert_eq!(reading.humidity_percent, 100);
        assert_eq!(reading.weather_code, 255);
    }
}

#[test]
fn integral_float_weather_fields_are_accepted() {
    let (client, _) = client(json_response(serde_json::json!({
        "utc_offset_seconds": -14400.0,
        "current": {
            "temperature_2m": 22.2,
            "relative_humidity_2m": 54.0,
            "weather_code": 2.0
        }
    })));

    assert_eq!(
        client
            .fetch(&location(), Duration::from_secs(123))
            .expect("integral float fields"),
        reading(22.2, 54, 2, -14_400, Duration::from_secs(123))
    );
}

#[test]
fn non_200_and_transport_failures_keep_their_error_categories() {
    let (unavailable, _) = client(status(503));
    assert!(matches!(
        unavailable.fetch(&location(), Duration::ZERO),
        Err(WeatherError::Status(503))
    ));

    let (timed_out, _) = client(Err(HttpError::Timeout));
    assert!(matches!(
        timed_out.fetch(&location(), Duration::ZERO),
        Err(WeatherError::Http(HttpError::Timeout))
    ));
}

#[test]
fn non_https_provider_base_is_rejected_before_http_execution() {
    let http = FakeHttpClient::responding(ok(include_bytes!("fixtures/open_meteo/current.json")));
    let probe = http.clone();
    let client = WeatherClient::with_provider_base(
        http,
        "http://api.open-meteo.test/v1/forecast".to_owned(),
    );

    assert!(matches!(
        client.fetch(&location(), Duration::ZERO),
        Err(WeatherError::Schema(_))
    ));
    assert!(probe.requests().is_empty());
}

fn reading(
    temperature_celsius: f64,
    humidity_percent: u8,
    weather_code: u8,
    utc_offset_seconds: i32,
    fetched_at: Duration,
) -> EnvironmentReading {
    EnvironmentReading {
        temperature_celsius,
        humidity_percent,
        weather_code,
        utc_offset_seconds,
        fetched_at,
    }
}

fn unix_seconds(year: i32, month: Month, day: u8, hour: u8, minute: u8) -> u64 {
    let date = Date::from_calendar_date(year, month, day).expect("date");
    let time = Time::from_hms(hour, minute, 0).expect("time");
    u64::try_from(
        PrimitiveDateTime::new(date, time)
            .assume_utc()
            .unix_timestamp(),
    )
    .expect("positive test epoch")
}

fn item(text: &str, tone: FooterTone) -> FooterItem {
    FooterItem {
        text: text.to_owned(),
        tone,
    }
}

fn condition_settings() -> FooterSettings {
    FooterSettings {
        show_condition: true,
        ..FooterSettings::default()
    }
}

#[test]
fn wmo_codes_use_only_the_approved_metar_style_mapping() {
    let cases = [
        (0, "CLR"),
        (1, "FEW"),
        (2, "SCT"),
        (3, "OVC"),
        (45, "FG"),
        (48, "FG"),
        (51, "-DZ"),
        (53, "DZ"),
        (55, "+DZ"),
        (56, "-FZDZ"),
        (57, "+FZDZ"),
        (61, "-RA"),
        (63, "RA"),
        (65, "+RA"),
        (66, "-FZRA"),
        (67, "+FZRA"),
        (71, "-SN"),
        (73, "SN"),
        (75, "+SN"),
        (77, "SG"),
        (80, "-SHRA"),
        (81, "SHRA"),
        (82, "+SHRA"),
        (85, "-SHSN"),
        (86, "+SHSN"),
        (95, "TS"),
        (96, "TSGR"),
        (99, "TSGR"),
        (4, "WX"),
    ];

    for (code, expected) in cases {
        let reading = reading(22.2, 54, code, 0, Duration::ZERO);
        let content = footer_content(
            &condition_settings(),
            Some(&reading),
            Duration::ZERO,
            unix_seconds(2026, Month::August, 2, 10, 15),
        );
        assert_eq!(
            content.environment,
            vec![item(expected, FooterTone::Condition)],
            "WMO code {code}"
        );
    }

    for code in u8::MIN..=u8::MAX {
        let reading = reading(22.2, 54, code, 0, Duration::ZERO);
        let text = &footer_content(
            &condition_settings(),
            Some(&reading),
            Duration::ZERO,
            unix_seconds(2026, Month::August, 2, 10, 15),
        )
        .environment[0]
            .text;
        assert_ne!(text, "BKN", "WMO code {code}");
        assert!(
            !text.chars().any(|character| character.is_ascii_digit()),
            "WMO code {code} invented a cloud height: {text}"
        );
    }
}

#[test]
fn footer_partitions_environment_then_temporal_with_fixed_tones_and_order() {
    let settings = FooterSettings {
        show_condition: true,
        show_temperature: true,
        show_humidity: true,
        show_time: true,
        show_date: true,
        temperature_unit: TemperatureUnit::Celsius,
        time_zone: TimeZone::Zulu,
        clock_format: ClockFormat::TwentyFour,
    };
    let reading = reading(22.2, 54, 2, -14_400, Duration::from_secs(10));

    assert_eq!(
        footer_content(
            &settings,
            Some(&reading),
            Duration::from_secs(11),
            unix_seconds(2026, Month::August, 2, 10, 15),
        ),
        FooterContent {
            environment: vec![
                item("SCT", FooterTone::Condition),
                item("22°C", FooterTone::Temperature),
                item("RH54%", FooterTone::Humidity),
            ],
            temporal: vec![
                item("10:15Z", FooterTone::Time),
                item("02 AUG", FooterTone::Date),
            ],
        }
    );
}

#[test]
fn temperature_units_round_to_zero_decimals_without_changing_source_data() {
    let reading = reading(22.6, 54, 2, 0, Duration::ZERO);
    let celsius = FooterSettings {
        show_temperature: true,
        temperature_unit: TemperatureUnit::Celsius,
        ..FooterSettings::default()
    };
    let fahrenheit = FooterSettings {
        temperature_unit: TemperatureUnit::Fahrenheit,
        ..celsius.clone()
    };

    assert_eq!(
        footer_content(&celsius, Some(&reading), Duration::ZERO, 0).environment,
        vec![item("23°C", FooterTone::Temperature)]
    );
    assert_eq!(
        footer_content(&fahrenheit, Some(&reading), Duration::ZERO, 0).environment,
        vec![item("73°F", FooterTone::Temperature)]
    );
    assert_eq!(reading.temperature_celsius, 22.6);
}

#[test]
fn temperature_half_ties_round_to_even_in_celsius_and_fahrenheit() {
    let cases = [
        (TemperatureUnit::Celsius, 2.5, "2°C"),
        (TemperatureUnit::Celsius, -2.5, "-2°C"),
        (TemperatureUnit::Fahrenheit, 2.5, "36°F"),
        (TemperatureUnit::Fahrenheit, -42.5, "-44°F"),
    ];

    for (temperature_unit, temperature_celsius, expected) in cases {
        let settings = FooterSettings {
            show_temperature: true,
            temperature_unit,
            ..FooterSettings::default()
        };
        assert_eq!(
            footer_content(
                &settings,
                Some(&reading(temperature_celsius, 54, 0, 0, Duration::ZERO,)),
                Duration::ZERO,
                0,
            )
            .environment,
            vec![item(expected, FooterTone::Temperature)],
            "unit={temperature_unit:?}, celsius={temperature_celsius}",
        );
    }
}

#[test]
fn temperature_rounding_normalizes_negative_zero_in_both_units() {
    let celsius = FooterSettings {
        show_temperature: true,
        temperature_unit: TemperatureUnit::Celsius,
        ..FooterSettings::default()
    };
    let fahrenheit = FooterSettings {
        temperature_unit: TemperatureUnit::Fahrenheit,
        ..celsius.clone()
    };

    assert_eq!(
        footer_content(
            &celsius,
            Some(&reading(-0.4, 54, 0, 0, Duration::ZERO)),
            Duration::ZERO,
            0,
        )
        .environment,
        vec![item("0°C", FooterTone::Temperature)]
    );
    assert_eq!(
        footer_content(
            &fahrenheit,
            Some(&reading(-17.8, 54, 0, 0, Duration::ZERO)),
            Duration::ZERO,
            0,
        )
        .environment,
        vec![item("0°F", FooterTone::Temperature)]
    );

    assert_eq!(
        footer_content(
            &celsius,
            Some(&reading(0.4, 54, 0, 0, Duration::ZERO)),
            Duration::ZERO,
            0,
        )
        .environment,
        vec![item("0°C", FooterTone::Temperature)]
    );
    assert_eq!(
        footer_content(
            &fahrenheit,
            Some(&reading(-17.7, 54, 0, 0, Duration::ZERO)),
            Duration::ZERO,
            0,
        )
        .environment,
        vec![item("0°F", FooterTone::Temperature)]
    );
}

#[test]
fn no_reading_collapses_selected_weather_and_keeps_zulu_time_independent() {
    let settings = FooterSettings {
        show_condition: true,
        show_temperature: true,
        show_humidity: true,
        show_time: true,
        show_date: true,
        temperature_unit: TemperatureUnit::Fahrenheit,
        time_zone: TimeZone::Zulu,
        clock_format: ClockFormat::TwentyFour,
    };

    assert_eq!(
        footer_content(
            &settings,
            None,
            Duration::from_secs(99),
            unix_seconds(2026, Month::August, 2, 10, 15),
        ),
        FooterContent {
            environment: vec![item("WX --", FooterTone::Status)],
            temporal: vec![
                item("10:15Z", FooterTone::Time),
                item("02 AUG", FooterTone::Date),
            ],
        }
    );
}

#[test]
fn local_time_and_date_use_placeholders_until_an_offset_exists() {
    let settings = FooterSettings {
        show_time: true,
        show_date: true,
        time_zone: TimeZone::RadarLocal,
        ..FooterSettings::default()
    };

    assert_eq!(
        footer_content(
            &settings,
            None,
            Duration::ZERO,
            unix_seconds(2026, Month::August, 2, 10, 15),
        ),
        FooterContent {
            environment: Vec::new(),
            temporal: vec![
                item("--:--", FooterTone::Time),
                item("-- ---", FooterTone::Date),
            ],
        }
    );
}

#[test]
fn radar_local_offsets_roll_the_date_forward_and_backward() {
    let settings = FooterSettings {
        show_time: true,
        show_date: true,
        time_zone: TimeZone::RadarLocal,
        ..FooterSettings::default()
    };
    let positive = reading(0.0, 0, 0, 7_200, Duration::ZERO);
    let negative = reading(0.0, 0, 0, -3_600, Duration::ZERO);

    assert_eq!(
        footer_content(
            &settings,
            Some(&positive),
            Duration::ZERO,
            unix_seconds(2026, Month::August, 2, 23, 30),
        )
        .temporal,
        vec![
            item("01:30", FooterTone::Time),
            item("03 AUG", FooterTone::Date),
        ]
    );
    assert_eq!(
        footer_content(
            &settings,
            Some(&negative),
            Duration::ZERO,
            unix_seconds(2026, Month::August, 2, 0, 30),
        )
        .temporal,
        vec![
            item("23:30", FooterTone::Time),
            item("01 AUG", FooterTone::Date),
        ]
    );
}

#[test]
fn out_of_range_local_offset_at_maximum_timestamp_uses_placeholders() {
    let settings = FooterSettings {
        show_time: true,
        show_date: true,
        time_zone: TimeZone::RadarLocal,
        ..FooterSettings::default()
    };
    let reading = reading(0.0, 0, 0, 3_600, Duration::ZERO);
    let maximum_timestamp = u64::try_from(PrimitiveDateTime::MAX.assume_utc().unix_timestamp())
        .expect("maximum timestamp is positive");

    assert_eq!(
        footer_content(&settings, Some(&reading), Duration::ZERO, maximum_timestamp,).temporal,
        vec![
            item("--:--", FooterTone::Time),
            item("-- ---", FooterTone::Date),
        ]
    );
}

#[test]
fn twelve_hour_clock_formats_midnight_and_noon_independently_of_zulu() {
    let settings = FooterSettings {
        show_time: true,
        time_zone: TimeZone::Zulu,
        clock_format: ClockFormat::Twelve,
        ..FooterSettings::default()
    };

    assert_eq!(
        footer_content(
            &settings,
            None,
            Duration::ZERO,
            unix_seconds(2026, Month::August, 2, 0, 0),
        )
        .temporal,
        vec![item("12:00AMZ", FooterTone::Time)]
    );
    assert_eq!(
        footer_content(
            &settings,
            None,
            Duration::ZERO,
            unix_seconds(2026, Month::August, 2, 12, 0),
        )
        .temporal,
        vec![item("12:00PMZ", FooterTone::Time)]
    );
    assert_eq!(
        footer_content(
            &settings,
            None,
            Duration::ZERO,
            unix_seconds(2026, Month::August, 2, 13, 5),
        )
        .temporal,
        vec![item("1:05PMZ", FooterTone::Time)]
    );
}

#[test]
fn stale_boundary_retains_last_known_values_and_underflow_is_fresh() {
    let settings = FooterSettings {
        show_condition: true,
        show_temperature: true,
        show_humidity: true,
        ..FooterSettings::default()
    };
    let fetched_at = Duration::from_secs(10_000);
    let reading = reading(22.2, 54, 2, 0, fetched_at);
    let just_fresh = fetched_at + ENVIRONMENT_STALE_AFTER - Duration::from_secs(1);
    let boundary = fetched_at + ENVIRONMENT_STALE_AFTER;

    assert!(!environment_is_stale(Some(&reading), just_fresh));
    assert!(environment_is_stale(Some(&reading), boundary));
    assert!(!environment_is_stale(
        Some(&reading),
        Duration::from_secs(1)
    ));
    assert!(!environment_is_stale(None, Duration::MAX));

    assert_eq!(
        footer_content(&settings, Some(&reading), just_fresh, 0).environment,
        vec![
            item("SCT", FooterTone::Condition),
            item("22°C", FooterTone::Temperature),
            item("RH54%", FooterTone::Humidity),
        ]
    );
    assert_eq!(
        footer_content(&settings, Some(&reading), boundary, 0).environment,
        vec![
            item("WX STALE", FooterTone::Status),
            item("SCT", FooterTone::Condition),
            item("22°C", FooterTone::Temperature),
            item("RH54%", FooterTone::Humidity),
        ]
    );
}

#[test]
fn disabled_footer_produces_no_content() {
    let reading = reading(22.2, 54, 2, 0, Duration::ZERO);
    assert_eq!(
        footer_content(
            &FooterSettings::default(),
            Some(&reading),
            ENVIRONMENT_STALE_AFTER,
            unix_seconds(2026, Month::August, 2, 10, 15),
        ),
        FooterContent::default()
    );
}

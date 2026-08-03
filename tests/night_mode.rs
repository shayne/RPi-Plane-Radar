use jiff::Timestamp;
use jiff::civil::Date;
use jiff::tz::TimeZone;
use planeradar::model::{
    DisplayPeriod, DisplayPolicy, FrameColorMode, Location, RadarSettings, ScheduleFacts,
    SolarStatus, Transition,
};
use planeradar::night_mode::display_policy;
use planeradar::solar::{SolarDay, SolarSchedule};

const LATITUDE: f64 = 40.7769;
const LONGITUDE: f64 = -73.8740;

fn unix(value: &str) -> i64 {
    value
        .parse::<Timestamp>()
        .expect("literal UTC timestamp")
        .as_second()
}

fn at(value: &str) -> u64 {
    u64::try_from(unix(value)).expect("positive fixture time")
}

fn configured_settings() -> RadarSettings {
    let mut settings = RadarSettings {
        location: Some(Location {
            latitude: LATITUDE,
            longitude: LONGITUDE,
            label: "LaGuardia Airport".to_owned(),
        }),
        ..RadarSettings::default()
    };
    settings.brightness.day_percent = 85;
    settings.brightness.night.enabled = true;
    settings.brightness.night.brightness_percent = 25;
    settings.brightness.night.red_mode = true;
    settings
}

fn schedule_from(
    first_date: &str,
    time_zone: &str,
    sunrise_hour: i8,
    sunrise_minute: i8,
) -> SolarSchedule {
    let zone = TimeZone::get(time_zone).expect("fixture time zone");
    let mut date = first_date.parse::<Date>().expect("fixture first date");
    let mut days = Vec::with_capacity(17);
    for _ in 0..17 {
        let sunrise = zone
            .to_timestamp(date.at(sunrise_hour, sunrise_minute, 0, 0))
            .expect("fixture sunrise")
            .as_second();
        days.push(SolarDay {
            date: date.to_string(),
            sunrise_unix: Some(sunrise),
        });
        date = date.tomorrow().expect("fixture next date");
    }
    SolarSchedule {
        schema_version: 1,
        latitude: LATITUDE,
        longitude: LONGITUDE,
        time_zone: time_zone.to_owned(),
        fetched_at_unix: 0,
        days,
    }
}

fn transition(at: &str, period: DisplayPeriod) -> Option<Transition> {
    Some(Transition {
        at_unix: unix(at),
        period,
    })
}

fn facts(start: &str, end: &str) -> ScheduleFacts {
    ScheduleFacts {
        start_unix: unix(start),
        end_unix: unix(end),
    }
}

fn day(
    brightness_percent: u8,
    next_transition: Option<Transition>,
    solar_status: SolarStatus,
) -> DisplayPolicy {
    DisplayPolicy {
        period: DisplayPeriod::Day,
        brightness_percent,
        color_mode: FrameColorMode::FullColor,
        next_transition,
        solar_status,
    }
}

fn night(
    brightness_percent: u8,
    color_mode: FrameColorMode,
    next_transition: Option<Transition>,
    solar_status: SolarStatus,
) -> DisplayPolicy {
    DisplayPolicy {
        period: DisplayPeriod::Night,
        brightness_percent,
        color_mode,
        next_transition,
        solar_status,
    }
}

#[test]
fn policy_truth_table_fails_closed_and_honors_exact_interval_boundaries() {
    // These literals catch disabled/waiting branches, coordinate borrowing,
    // wrong interval inclusivity, omitted previous-date evaluation, and a
    // policy that applies night colors independently of the active interval.
    let schedule = schedule_from("2026-08-02", "America/New_York", 6, 0);
    let settings = configured_settings();
    let interval = facts("2026-08-03T00:00:00Z", "2026-08-03T10:00:00Z");
    let next_interval = facts("2026-08-04T00:00:00Z", "2026-08-04T10:00:00Z");

    let mut disabled = settings.clone();
    disabled.brightness.night.enabled = false;
    let no_location = RadarSettings {
        location: None,
        ..settings.clone()
    };
    let mut mismatch = schedule.clone();
    mismatch.latitude = 40.776_900_1;

    struct Case {
        name: &'static str,
        settings: RadarSettings,
        schedule: Option<SolarSchedule>,
        now: &'static str,
        expected: DisplayPolicy,
    }

    let cases = [
        Case {
            name: "night disabled",
            settings: disabled,
            schedule: Some(schedule.clone()),
            now: "2026-08-03T01:00:00Z",
            expected: day(85, None, SolarStatus::Disabled),
        },
        Case {
            name: "enabled without a configured location",
            settings: no_location,
            schedule: Some(schedule.clone()),
            now: "2026-08-03T01:00:00Z",
            expected: day(85, None, SolarStatus::Waiting),
        },
        Case {
            name: "new location without matching solar data",
            settings: settings.clone(),
            schedule: None,
            now: "2026-08-03T01:00:00Z",
            expected: day(85, None, SolarStatus::Waiting),
        },
        Case {
            name: "schedule coordinate mismatch",
            settings: settings.clone(),
            schedule: Some(mismatch),
            now: "2026-08-03T01:00:00Z",
            expected: day(85, None, SolarStatus::Waiting),
        },
        Case {
            name: "before start",
            settings: settings.clone(),
            schedule: Some(schedule.clone()),
            now: "2026-08-02T23:59:59Z",
            expected: day(
                85,
                transition("2026-08-03T00:00:00Z", DisplayPeriod::Night),
                SolarStatus::Upcoming(interval),
            ),
        },
        Case {
            name: "exact start enters night",
            settings: settings.clone(),
            schedule: Some(schedule.clone()),
            now: "2026-08-03T00:00:00Z",
            expected: night(
                25,
                FrameColorMode::RedOnly,
                transition("2026-08-03T10:00:00Z", DisplayPeriod::Day),
                SolarStatus::Active(interval),
            ),
        },
        Case {
            name: "after-midnight restart uses the previous local date",
            settings: settings.clone(),
            schedule: Some(schedule.clone()),
            now: "2026-08-03T04:30:00Z",
            expected: night(
                25,
                FrameColorMode::RedOnly,
                transition("2026-08-03T10:00:00Z", DisplayPeriod::Day),
                SolarStatus::Active(interval),
            ),
        },
        Case {
            name: "last instant before sunrise remains night",
            settings: settings.clone(),
            schedule: Some(schedule.clone()),
            now: "2026-08-03T09:59:59Z",
            expected: night(
                25,
                FrameColorMode::RedOnly,
                transition("2026-08-03T10:00:00Z", DisplayPeriod::Day),
                SolarStatus::Active(interval),
            ),
        },
        Case {
            name: "exact sunrise exits night",
            settings: settings.clone(),
            schedule: Some(schedule.clone()),
            now: "2026-08-03T10:00:00Z",
            expected: day(
                85,
                transition("2026-08-04T00:00:00Z", DisplayPeriod::Night),
                SolarStatus::Upcoming(next_interval),
            ),
        },
        Case {
            name: "after sunrise remains day",
            settings,
            schedule: Some(schedule),
            now: "2026-08-03T16:00:00Z",
            expected: day(
                85,
                transition("2026-08-04T00:00:00Z", DisplayPeriod::Night),
                SolarStatus::Upcoming(next_interval),
            ),
        },
    ];

    for case in cases {
        assert_eq!(
            display_policy(
                &case.settings,
                case.schedule.as_ref(),
                u64::try_from(unix(case.now)).expect("positive fixture time"),
            ),
            case.expected,
            "{}",
            case.name,
        );
    }
}

#[test]
fn defaults_preserve_full_brightness_and_full_color_without_solar_data() {
    assert_eq!(
        display_policy(&RadarSettings::default(), None, at("2026-08-03T04:30:00Z"),),
        day(100, None, SolarStatus::Disabled),
    );
}

#[test]
fn night_brightness_and_red_mode_are_applied_only_during_night() {
    let mut settings = configured_settings();
    settings.brightness.night.red_mode = false;
    let schedule = schedule_from("2026-08-02", "America/New_York", 6, 0);

    assert_eq!(
        display_policy(&settings, Some(&schedule), at("2026-08-03T04:30:00Z"),),
        night(
            25,
            FrameColorMode::FullColor,
            transition("2026-08-03T10:00:00Z", DisplayPeriod::Day),
            SolarStatus::Active(facts("2026-08-03T00:00:00Z", "2026-08-03T10:00:00Z",)),
        ),
    );
}

#[test]
fn missing_or_expired_sunrise_coverage_uses_an_exact_seven_am_fallback() {
    // Removing every sunrise after August 18 catches both null coverage and
    // a stale forecast accidentally keeping night active indefinitely.
    let mut schedule = schedule_from("2026-08-02", "America/New_York", 6, 0);
    for day in &mut schedule.days {
        if day.date == "2026-08-18" {
            day.sunrise_unix = None;
        }
    }
    let settings = configured_settings();
    let active_fallback = facts("2026-08-19T00:00:00Z", "2026-08-19T11:00:00Z");
    let next_fallback = facts("2026-08-20T00:00:00Z", "2026-08-20T11:00:00Z");

    let cases = [
        (
            "null sunrise starts a bounded fallback interval",
            "2026-08-19T00:00:00Z",
            night(
                25,
                FrameColorMode::RedOnly,
                transition("2026-08-19T11:00:00Z", DisplayPeriod::Day),
                SolarStatus::Fallback(active_fallback),
            ),
        ),
        (
            "last instant before fallback remains night",
            "2026-08-19T10:59:59Z",
            night(
                25,
                FrameColorMode::RedOnly,
                transition("2026-08-19T11:00:00Z", DisplayPeriod::Day),
                SolarStatus::Fallback(active_fallback),
            ),
        ),
        (
            "exact fallback exits and schedules the next bounded night",
            "2026-08-19T11:00:00Z",
            day(
                85,
                transition("2026-08-20T00:00:00Z", DisplayPeriod::Night),
                SolarStatus::Fallback(next_fallback),
            ),
        ),
        (
            "expired forecast still uses the next radar-local fallback",
            "2026-08-25T01:00:00Z",
            night(
                25,
                FrameColorMode::RedOnly,
                transition("2026-08-25T11:00:00Z", DisplayPeriod::Day),
                SolarStatus::Fallback(facts("2026-08-25T00:00:00Z", "2026-08-25T11:00:00Z")),
            ),
        ),
    ];

    for (name, now, expected) in cases {
        assert_eq!(
            display_policy(
                &settings,
                Some(&schedule),
                u64::try_from(unix(now)).expect("positive fixture time"),
            ),
            expected,
            "{name}",
        );
    }
}

#[test]
fn invalid_schedule_identity_or_timezone_never_enables_night() {
    let settings = configured_settings();
    let mut invalid_zone = schedule_from("2026-08-02", "America/New_York", 6, 0);
    invalid_zone.time_zone = "Not/A_Real_Zone".to_owned();
    let mut invalid_schema = schedule_from("2026-08-02", "America/New_York", 6, 0);
    invalid_schema.schema_version = 2;

    for schedule in [&invalid_zone, &invalid_schema] {
        assert_eq!(
            display_policy(&settings, Some(schedule), at("2026-08-03T04:30:00Z"),),
            day(85, None, SolarStatus::Waiting),
        );
    }
}

#[test]
fn spring_gap_moves_nonexistent_start_to_the_first_valid_instant() {
    // A naive Compatible conversion yields 03:30. The product contract is
    // the transition boundary itself: 03:00 EDT, the first valid wall time.
    let mut settings = configured_settings();
    settings.brightness.night.start_hour = 2;
    settings.brightness.night.start_minute = 30;
    let schedule = schedule_from("2026-03-01", "America/New_York", 6, 30);
    let interval = facts("2026-03-08T07:00:00Z", "2026-03-08T10:30:00Z");

    assert_eq!(
        display_policy(&settings, Some(&schedule), at("2026-03-08T06:59:59Z"),),
        day(
            85,
            transition("2026-03-08T07:00:00Z", DisplayPeriod::Night),
            SolarStatus::Upcoming(interval),
        ),
    );
    assert_eq!(
        display_policy(&settings, Some(&schedule), at("2026-03-08T07:00:00Z"),),
        night(
            25,
            FrameColorMode::RedOnly,
            transition("2026-03-08T10:30:00Z", DisplayPeriod::Day),
            SolarStatus::Active(interval),
        ),
    );
}

#[test]
fn fall_overlap_chooses_the_first_occurrence_of_the_start_time() {
    let mut settings = configured_settings();
    settings.brightness.night.start_hour = 1;
    settings.brightness.night.start_minute = 30;
    let schedule = schedule_from("2026-10-25", "America/New_York", 6, 30);
    let interval = facts("2026-11-01T05:30:00Z", "2026-11-01T11:30:00Z");

    assert_eq!(
        display_policy(&settings, Some(&schedule), at("2026-11-01T05:29:59Z"),),
        day(
            85,
            transition("2026-11-01T05:30:00Z", DisplayPeriod::Night),
            SolarStatus::Upcoming(interval),
        ),
    );
    assert_eq!(
        display_policy(&settings, Some(&schedule), at("2026-11-01T05:30:00Z"),),
        night(
            25,
            FrameColorMode::RedOnly,
            transition("2026-11-01T11:30:00Z", DisplayPeriod::Day),
            SolarStatus::Active(interval),
        ),
    );
    assert_eq!(
        display_policy(&settings, Some(&schedule), at("2026-11-01T06:30:00Z"),),
        night(
            25,
            FrameColorMode::RedOnly,
            transition("2026-11-01T11:30:00Z", DisplayPeriod::Day),
            SolarStatus::Active(interval),
        ),
        "the repeated 01:30 EST is not a second transition",
    );
}

#[test]
fn forward_and_backward_wall_clock_jumps_are_independent_policy_evaluations() {
    let settings = configured_settings();
    let schedule = schedule_from("2026-08-02", "America/New_York", 6, 0);

    let active = display_policy(&settings, Some(&schedule), at("2026-08-03T04:30:00Z"));
    let jumped_forward = display_policy(&settings, Some(&schedule), at("2026-08-03T16:00:00Z"));
    let jumped_backward = display_policy(&settings, Some(&schedule), at("2026-08-02T23:59:59Z"));
    let active_again = display_policy(&settings, Some(&schedule), at("2026-08-03T04:30:00Z"));

    assert_eq!(active.period, DisplayPeriod::Night);
    assert_eq!(jumped_forward.period, DisplayPeriod::Day);
    assert_eq!(jumped_backward.period, DisplayPeriod::Day);
    assert_eq!(active_again, active);
}

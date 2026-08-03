use std::collections::HashSet;

use jiff::Timestamp;
use jiff::civil::{Date, DateTime};
use jiff::tz::{AmbiguousOffset, TimeZone};

use crate::model::{
    DisplayPeriod, DisplayPolicy, FrameColorMode, RadarSettings, ScheduleFacts, SolarStatus,
    Transition,
};
use crate::solar::SolarSchedule;

const SOLAR_SCHEMA_VERSION: u32 = 1;
const SOLAR_DAY_COUNT: usize = 17;
const FALLBACK_HOUR: i8 = 7;

#[derive(Clone, Copy)]
struct Interval {
    facts: ScheduleFacts,
    fallback: bool,
}

/// Resolves the physical display policy from only saved settings, an optional
/// matching solar schedule, and the supplied wall-clock instant.
pub fn display_policy(
    settings: &RadarSettings,
    schedule: Option<&SolarSchedule>,
    unix_seconds: u64,
) -> DisplayPolicy {
    if !settings.brightness.night.enabled {
        return day_policy(settings, None, SolarStatus::Disabled);
    }
    let Some(location) = settings.location.as_ref() else {
        return day_policy(settings, None, SolarStatus::Waiting);
    };
    let Some(schedule) = schedule else {
        return day_policy(settings, None, SolarStatus::Waiting);
    };
    if schedule.latitude != location.latitude || schedule.longitude != location.longitude {
        return day_policy(settings, None, SolarStatus::Waiting);
    }
    if settings.brightness.night.start_hour > 23 || settings.brightness.night.start_minute > 59 {
        return day_policy(settings, None, SolarStatus::Waiting);
    }

    let Some((zone, sunrises)) = validated_schedule(schedule) else {
        return day_policy(settings, None, SolarStatus::Waiting);
    };
    let Ok(now_seconds) = i64::try_from(unix_seconds) else {
        return day_policy(settings, None, SolarStatus::Waiting);
    };
    let Ok(now) = Timestamp::from_second(now_seconds) else {
        return day_policy(settings, None, SolarStatus::Waiting);
    };
    let local_date = now.to_zoned(zone.clone()).date();
    let Ok(previous_date) = local_date.yesterday() else {
        return day_policy(settings, None, SolarStatus::Waiting);
    };
    let Ok(next_date) = local_date.tomorrow() else {
        return day_policy(settings, None, SolarStatus::Waiting);
    };

    let dates = [previous_date, local_date, next_date];
    let mut intervals = Vec::with_capacity(dates.len());
    for date in dates {
        let Some(start) = resolve_local(
            &zone,
            date.at(
                i8::try_from(settings.brightness.night.start_hour).expect("validated hour"),
                i8::try_from(settings.brightness.night.start_minute).expect("validated minute"),
                0,
                0,
            ),
        ) else {
            return day_policy(settings, None, SolarStatus::Waiting);
        };
        let (end, fallback) = match sunrises.iter().copied().find(|sunrise| *sunrise > start) {
            Some(sunrise) => (sunrise, false),
            None => {
                let Some(fallback) = next_fallback(&zone, date, start) else {
                    return day_policy(settings, None, SolarStatus::Waiting);
                };
                (fallback, true)
            }
        };
        intervals.push(Interval {
            facts: ScheduleFacts {
                start_unix: start.as_second(),
                end_unix: end.as_second(),
            },
            fallback,
        });
    }

    if let Some(active) = intervals
        .iter()
        .rev()
        .find(|interval| {
            interval.facts.start_unix <= now_seconds && now_seconds < interval.facts.end_unix
        })
        .copied()
    {
        let solar_status = if active.fallback {
            SolarStatus::Fallback(active.facts)
        } else {
            SolarStatus::Active(active.facts)
        };
        return DisplayPolicy {
            period: DisplayPeriod::Night,
            brightness_percent: settings.brightness.night.brightness_percent,
            color_mode: if settings.brightness.night.red_mode {
                FrameColorMode::RedOnly
            } else {
                FrameColorMode::FullColor
            },
            next_transition: Some(Transition {
                at_unix: active.facts.end_unix,
                period: DisplayPeriod::Day,
            }),
            solar_status,
        };
    }

    let Some(upcoming) = intervals
        .iter()
        .find(|interval| interval.facts.start_unix > now_seconds)
        .copied()
    else {
        return day_policy(settings, None, SolarStatus::Waiting);
    };
    let solar_status = if upcoming.fallback {
        SolarStatus::Fallback(upcoming.facts)
    } else {
        SolarStatus::Upcoming(upcoming.facts)
    };
    day_policy(
        settings,
        Some(Transition {
            at_unix: upcoming.facts.start_unix,
            period: DisplayPeriod::Night,
        }),
        solar_status,
    )
}

fn day_policy(
    settings: &RadarSettings,
    next_transition: Option<Transition>,
    solar_status: SolarStatus,
) -> DisplayPolicy {
    DisplayPolicy {
        period: DisplayPeriod::Day,
        brightness_percent: settings.brightness.day_percent,
        color_mode: FrameColorMode::FullColor,
        next_transition,
        solar_status,
    }
}

fn validated_schedule(schedule: &SolarSchedule) -> Option<(TimeZone, Vec<Timestamp>)> {
    if schedule.schema_version != SOLAR_SCHEMA_VERSION
        || schedule.days.len() != SOLAR_DAY_COUNT
        || !valid_coordinate(schedule.latitude, -90.0, 90.0)
        || !valid_coordinate(schedule.longitude, -180.0, 180.0)
    {
        return None;
    }
    let zone = TimeZone::get(&schedule.time_zone).ok()?;
    if zone.is_unknown() || zone.iana_name().is_none() {
        return None;
    }

    let mut seen = HashSet::with_capacity(schedule.days.len());
    let mut sunrises = Vec::with_capacity(schedule.days.len());
    for day in &schedule.days {
        let date = day.date.parse::<Date>().ok()?;
        if date.to_string() != day.date || !seen.insert(date) {
            return None;
        }
        let Some(sunrise_seconds) = day.sunrise_unix else {
            continue;
        };
        let sunrise = Timestamp::from_second(sunrise_seconds).ok()?;
        if sunrise.to_zoned(zone.clone()).date() != date {
            return None;
        }
        sunrises.push(sunrise);
    }
    sunrises.sort_unstable();
    Some((zone, sunrises))
}

fn valid_coordinate(value: f64, minimum: f64, maximum: f64) -> bool {
    value.is_finite() && (minimum..=maximum).contains(&value)
}

fn resolve_local(zone: &TimeZone, date_time: DateTime) -> Option<Timestamp> {
    let ambiguous = zone.to_ambiguous_timestamp(date_time);
    match ambiguous.offset() {
        AmbiguousOffset::Gap { .. } => {
            let before_transition = ambiguous.earlier().ok()?;
            zone.following(before_transition)
                .next()
                .map(|transition| transition.timestamp())
        }
        AmbiguousOffset::Unambiguous { .. } | AmbiguousOffset::Fold { .. } => {
            ambiguous.compatible().ok()
        }
    }
}

fn next_fallback(zone: &TimeZone, date: Date, start: Timestamp) -> Option<Timestamp> {
    let same_date = resolve_local(zone, date.at(FALLBACK_HOUR, 0, 0, 0))?;
    if same_date > start {
        return Some(same_date);
    }
    resolve_local(zone, date.tomorrow().ok()?.at(FALLBACK_HOUR, 0, 0, 0))
}

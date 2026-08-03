use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::time::Duration;

use jiff::Timestamp;
use jiff::civil::Date;
use jiff::tz::TimeZone;

use crate::http::HttpClient;
use crate::model::{Location, RuntimeModel, SolarFailure};
use crate::solar::{SolarClient, SolarSchedule, load_cache, save_cache};
use crate::time::Clock;

use super::{CommandDrain, Waiter, WorkerCommand, drain_commands, wait_for_command};

const IDLE_INTERVAL: Duration = Duration::from_secs(30);
const SUCCESS_CHECK_INTERVAL: Duration = Duration::from_secs(15 * 60);

pub struct SolarWorker<C, K, W> {
    client: SolarClient<C>,
    cache_path: PathBuf,
    model: RuntimeModel,
    clock: K,
    waiter: W,
}

impl<C: HttpClient, K: Clock, W: Waiter> SolarWorker<C, K, W> {
    pub fn new(
        client: SolarClient<C>,
        cache_path: PathBuf,
        model: RuntimeModel,
        clock: K,
        waiter: W,
    ) -> Self {
        Self {
            client,
            cache_path,
            model,
            clock,
            waiter,
        }
    }

    pub fn run(&self, commands: Receiver<WorkerCommand>, stop: Arc<AtomicBool>) {
        let mut active_location: Option<Location> = None;
        let mut successful_local_days = HashSet::<Date>::new();
        let mut failures = 0_u32;

        loop {
            if stop.load(Ordering::Acquire)
                || matches!(drain_commands(&commands, &stop), CommandDrain::Stop)
            {
                return;
            }

            let snapshot = self.model.snapshot();
            let Some(location) = snapshot.settings.location.clone() else {
                active_location = None;
                successful_local_days.clear();
                failures = 0;
                if !wait_for_command(&self.waiter, &commands, &stop, IDLE_INTERVAL) {
                    return;
                }
                continue;
            };
            if !active_location
                .as_ref()
                .is_some_and(|active| same_coordinates(active, &location))
            {
                active_location = Some(location.clone());
                successful_local_days.clear();
                failures = 0;
            }
            if !snapshot.settings.brightness.night.enabled {
                failures = 0;
                if !wait_for_command(&self.waiter, &commands, &stop, IDLE_INTERVAL) {
                    return;
                }
                continue;
            }
            if snapshot.solar_schedule.is_none()
                && let Some(cached) = load_cache(&self.cache_path, &location)
            {
                let _ = self
                    .model
                    .record_solar_schedule_if_current(&location, Arc::new(cached));
            }

            let schedule = self.model.snapshot().solar_schedule;
            if local_day(
                self.clock.unix_seconds(),
                schedule
                    .as_deref()
                    .map(|schedule| schedule.time_zone.as_str()),
            )
            .is_some_and(|day| successful_local_days.contains(&day))
            {
                if !wait_for_command(&self.waiter, &commands, &stop, SUCCESS_CHECK_INTERVAL) {
                    return;
                }
                continue;
            }

            let result = self.client.fetch(&location, self.clock.unix_seconds());
            let command_drain = drain_commands(&commands, &stop);
            if stop.load(Ordering::Acquire) || matches!(command_drain, CommandDrain::Stop) {
                return;
            }

            match result {
                Ok(schedule) => {
                    let successful_day =
                        local_day(self.clock.unix_seconds(), Some(schedule.time_zone.as_str()));
                    let accepted = self.persist_if_current(&location, schedule);
                    match accepted {
                        Ok(true) => {
                            failures = 0;
                            if let Some(successful_day) = successful_day {
                                successful_local_days.insert(successful_day);
                            } else if !wait_for_command(
                                &self.waiter,
                                &commands,
                                &stop,
                                SUCCESS_CHECK_INTERVAL,
                            ) {
                                return;
                            }
                        }
                        Ok(false) => continue,
                        Err(failure) => {
                            if !self.publish_failure(&location, failure) {
                                continue;
                            }
                            failures = failures.saturating_add(1);
                            if !wait_for_command(
                                &self.waiter,
                                &commands,
                                &stop,
                                failure_interval(failures),
                            ) {
                                return;
                            }
                        }
                    }
                }
                Err(error) => {
                    if !self.publish_failure(
                        &location,
                        SolarFailure {
                            category: error.category(),
                            at: self.clock.monotonic(),
                        },
                    ) {
                        continue;
                    }
                    failures = failures.saturating_add(1);
                    if !wait_for_command(&self.waiter, &commands, &stop, failure_interval(failures))
                    {
                        return;
                    }
                }
            }
        }
    }

    fn persist_if_current(
        &self,
        location: &Location,
        schedule: SolarSchedule,
    ) -> Result<bool, SolarFailure> {
        let completed_at = self.clock.monotonic();
        self.model
            .persist_and_record_solar_schedule_if_current(location, schedule, |schedule| {
                save_cache(&self.cache_path, schedule)
            })
            .map(|result| {
                result.is_some()
                    || self
                        .model
                        .snapshot()
                        .solar_schedule
                        .is_some_and(|schedule| {
                            schedule.latitude == location.latitude
                                && schedule.longitude == location.longitude
                        })
            })
            .map_err(|error| SolarFailure {
                category: error.category(),
                at: completed_at,
            })
    }

    fn publish_failure(&self, location: &Location, failure: SolarFailure) -> bool {
        let published = self
            .model
            .record_solar_failure_if_current(location, failure)
            .is_some();
        if published {
            log::warn!("provider=Open-Meteo category={}", failure.category.as_str());
        }
        published || {
            let snapshot = self.model.snapshot();
            snapshot.settings.brightness.night.enabled
                && snapshot
                    .settings
                    .location
                    .as_ref()
                    .is_some_and(|current| same_coordinates(current, location))
                && snapshot.solar_last_error == Some(failure)
        }
    }
}

fn failure_interval(failures: u32) -> Duration {
    Duration::from_secs(match failures {
        1 => 30,
        2 => 60,
        3 => 5 * 60,
        _ => 15 * 60,
    })
}

fn local_day(unix_seconds: u64, time_zone: Option<&str>) -> Option<Date> {
    let time_zone = TimeZone::get(time_zone?).ok()?;
    if time_zone.is_unknown() || time_zone.iana_name().is_none() {
        return None;
    }
    let unix_seconds = i64::try_from(unix_seconds).ok()?;
    let timestamp = Timestamp::from_second(unix_seconds).ok()?;
    Some(timestamp.to_zoned(time_zone).date())
}

fn same_coordinates(left: &Location, right: &Location) -> bool {
    left.latitude == right.latitude && left.longitude == right.longitude
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_intervals_follow_the_solar_schedule() {
        assert_eq!(failure_interval(1), Duration::from_secs(30));
        assert_eq!(failure_interval(2), Duration::from_secs(60));
        assert_eq!(failure_interval(3), Duration::from_secs(5 * 60));
        assert_eq!(failure_interval(4), Duration::from_secs(15 * 60));
        assert_eq!(failure_interval(u32::MAX), Duration::from_secs(15 * 60));
    }
}

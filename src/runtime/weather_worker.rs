use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::time::Duration;

use crate::http::{HttpClient, HttpError};
use crate::model::{Location, RuntimeModel};
use crate::time::Clock;
use crate::weather::{WeatherClient, WeatherError};

use super::{CommandDrain, Waiter, WorkerCommand, drain_commands, wait_for_command};

const SUCCESS_INTERVAL: Duration = Duration::from_secs(15 * 60);
const IDLE_INTERVAL: Duration = Duration::from_secs(30);

pub struct WeatherWorker<C, K, W> {
    client: WeatherClient<C>,
    model: RuntimeModel,
    clock: K,
    waiter: W,
}

impl<C: HttpClient, K: Clock, W: Waiter> WeatherWorker<C, K, W> {
    pub fn new(client: WeatherClient<C>, model: RuntimeModel, clock: K, waiter: W) -> Self {
        Self {
            client,
            model,
            clock,
            waiter,
        }
    }

    pub fn run(&self, commands: Receiver<WorkerCommand>, stop: Arc<AtomicBool>) {
        let mut failures = 0_u32;
        let mut active_location: Option<Location> = None;
        let mut deadline: Option<Duration> = None;

        loop {
            if stop.load(Ordering::Acquire)
                || matches!(drain_commands(&commands, &stop), CommandDrain::Stop)
            {
                return;
            }

            let snapshot = self.model.snapshot();
            let Some(location) = snapshot.settings.location.clone() else {
                active_location = None;
                deadline = None;
                if !wait_for_command(&self.waiter, &commands, &stop, IDLE_INTERVAL) {
                    return;
                }
                continue;
            };
            if !snapshot.settings.footer.needs_environment() {
                active_location = None;
                deadline = None;
                if !wait_for_command(&self.waiter, &commands, &stop, IDLE_INTERVAL) {
                    return;
                }
                continue;
            }

            if active_location.as_ref() == Some(&location) {
                if let Some(deadline) = deadline {
                    let remaining = deadline.saturating_sub(self.clock.monotonic());
                    if !remaining.is_zero() {
                        if !wait_for_command(&self.waiter, &commands, &stop, remaining) {
                            return;
                        }
                        continue;
                    }
                }
            } else {
                active_location = Some(location.clone());
            }

            let started_at = self.clock.monotonic();
            let result = self.client.fetch(&location, started_at);
            if stop.load(Ordering::Acquire)
                || matches!(drain_commands(&commands, &stop), CommandDrain::Stop)
            {
                return;
            }
            let completed_at = self.clock.monotonic();

            let interval = match result {
                Ok(mut reading) => {
                    reading.fetched_at = completed_at;
                    if self
                        .model
                        .record_environment_if_current(&location, reading)
                        .is_none()
                    {
                        continue;
                    }
                    failures = 0;
                    SUCCESS_INTERVAL
                }
                Err(error) => {
                    if self
                        .model
                        .record_environment_error_if_current(&location, completed_at)
                        .is_none()
                    {
                        continue;
                    }
                    failures = failures.saturating_add(1);
                    log::warn!("provider=Open-Meteo category={}", error_category(&error));
                    failure_interval(failures)
                }
            };
            deadline = Some(completed_at.saturating_add(interval));
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

fn error_category(error: &WeatherError) -> &'static str {
    match error {
        WeatherError::Http(HttpError::InvalidTimeout) => "invalid-timeout",
        WeatherError::Http(HttpError::InvalidBodyLimit) => "invalid-body-limit",
        WeatherError::Http(HttpError::Timeout) => "timeout",
        WeatherError::Http(HttpError::Transport) => "transport",
        WeatherError::Http(HttpError::Body) => "response-body",
        WeatherError::Http(HttpError::BodyTooLarge) => "response-too-large",
        WeatherError::Http(HttpError::TlsVerificationRequired) => "tls-verification",
        WeatherError::Status(_) => "http-status",
        WeatherError::Json(_) => "invalid-json",
        WeatherError::Schema(_) => "invalid-schema",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_intervals_follow_the_weather_schedule() {
        assert_eq!(failure_interval(1), Duration::from_secs(30));
        assert_eq!(failure_interval(2), Duration::from_secs(60));
        assert_eq!(failure_interval(3), Duration::from_secs(5 * 60));
        assert_eq!(failure_interval(4), Duration::from_secs(15 * 60));
        assert_eq!(failure_interval(u32::MAX), Duration::from_secs(15 * 60));
    }

    #[test]
    fn log_categories_do_not_include_provider_payloads() {
        assert_eq!(
            error_category(&WeatherError::Http(HttpError::Timeout)),
            "timeout"
        );
        assert_eq!(error_category(&WeatherError::Status(503)), "http-status");
        assert_eq!(
            error_category(&WeatherError::Schema("latitude=40&longitude=-74")),
            "invalid-schema"
        );
    }
}

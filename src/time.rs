use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub trait Clock: Send + Sync {
    fn monotonic(&self) -> Duration;
    fn unix_seconds(&self) -> u64;
}

pub trait Sleeper: Send + Sync {
    fn sleep(&self, duration: Duration);
}

#[derive(Debug)]
pub struct SystemClock {
    started_at: Instant,
}

impl SystemClock {
    pub fn new() -> Self {
        Self {
            started_at: Instant::now(),
        }
    }
}

impl Default for SystemClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for SystemClock {
    fn monotonic(&self) -> Duration {
        self.started_at.elapsed()
    }

    fn unix_seconds(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::ZERO)
            .as_secs()
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ThreadSleeper;

impl Sleeper for ThreadSleeper {
    fn sleep(&self, duration: Duration) {
        thread::sleep(duration);
    }
}

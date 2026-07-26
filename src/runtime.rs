use std::collections::HashSet;
use std::fs;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use signal_hook::consts::signal::{SIGINT, SIGTERM};
use thiserror::Error;
use url::Url;

use crate::adsb::AdsbClient;
use crate::geocode::{GeocodeService, Geocoder};
use crate::http::{HttpClient, UreqHttpClient};
use crate::model::{AppState, RadarSettings};
use crate::network::{current_interfaces, discover_ip_url};
use crate::range::range_preset;
use crate::settings::SettingsStore;
use crate::time::{Clock, SystemClock, ThreadSleeper};
use crate::web::{HealthSnapshot, HealthSource, SettingsServer, SettingsService, WebError};

const SUCCESS_INTERVAL: Duration = Duration::from_secs(3);
const IDLE_INTERVAL: Duration = Duration::from_secs(30);
const STOP_CHECK_INTERVAL: Duration = Duration::from_millis(50);
const STALE_AFTER: Duration = Duration::from_secs(30);

pub use crate::model::{RuntimeModel, RuntimeSnapshot};

#[derive(Clone, Debug)]
pub struct RuntimeConfig {
    pub settings_path: PathBuf,
    pub geocode_cache_path: PathBuf,
    pub http_address: SocketAddr,
    pub local_url: String,
    pub nominatim_url: String,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            settings_path: PathBuf::from("/var/lib/planeradar/settings.json"),
            geocode_cache_path: PathBuf::from("/var/lib/planeradar/geocode-cache.json"),
            http_address: "0.0.0.0:80".parse().expect("valid default HTTP address"),
            local_url: "http://planeradar.local".to_owned(),
            nominatim_url: "https://nominatim.openstreetmap.org/search".to_owned(),
        }
    }
}

#[derive(Clone, Debug)]
pub enum WorkerCommand {
    SettingsChanged(RadarSettings),
    Stop,
}

pub enum WaitResult {
    TimedOut,
    Command(WorkerCommand),
}

pub trait Waiter: Send + Sync + 'static {
    fn wait(
        &self,
        commands: &Receiver<WorkerCommand>,
        stop: &AtomicBool,
        duration: Duration,
    ) -> WaitResult;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ChannelWaiter;

impl Waiter for ChannelWaiter {
    fn wait(
        &self,
        commands: &Receiver<WorkerCommand>,
        stop: &AtomicBool,
        duration: Duration,
    ) -> WaitResult {
        let mut remaining = duration;
        while !stop.load(Ordering::Acquire) {
            let slice = remaining.min(STOP_CHECK_INTERVAL);
            match commands.recv_timeout(slice) {
                Ok(command) => return WaitResult::Command(command),
                Err(RecvTimeoutError::Disconnected) => {
                    return WaitResult::Command(WorkerCommand::Stop);
                }
                Err(RecvTimeoutError::Timeout) => {
                    remaining = remaining.saturating_sub(slice);
                    if remaining.is_zero() {
                        return WaitResult::TimedOut;
                    }
                }
            }
        }
        WaitResult::Command(WorkerCommand::Stop)
    }
}

pub struct AdsbWorker<C, K, W> {
    client: AdsbClient<C>,
    model: RuntimeModel,
    clock: K,
    waiter: W,
}

impl<C: HttpClient, K: Clock, W: Waiter> AdsbWorker<C, K, W> {
    pub fn new(client: AdsbClient<C>, model: RuntimeModel, clock: K, waiter: W) -> Self {
        Self {
            client,
            model,
            clock,
            waiter,
        }
    }

    pub fn run(&self, commands: Receiver<WorkerCommand>, stop: Arc<AtomicBool>) {
        let mut failures = 0_u32;
        loop {
            if stop.load(Ordering::Acquire)
                || matches!(drain_commands(&commands, &stop), CommandDrain::Stop)
            {
                return;
            }
            let snapshot = self.model.snapshot();
            let Some(location) = snapshot.settings.location.clone() else {
                if !wait_for_command(&self.waiter, &commands, &stop, IDLE_INTERVAL) {
                    return;
                }
                continue;
            };
            let range_index = snapshot.settings.range_index;
            let Ok(range) = range_preset(range_index) else {
                return;
            };
            let started_at = self.clock.monotonic();
            let result = self.client.fetch(&location, range.outer_km);

            let command_drain = drain_commands(&commands, &stop);
            if stop.load(Ordering::Acquire) || matches!(command_drain, CommandDrain::Stop) {
                return;
            }
            match result {
                Ok(aircraft) => {
                    if self
                        .model
                        .record_aircraft_if_query(
                            &location,
                            range_index,
                            aircraft,
                            self.clock.monotonic(),
                        )
                        .is_none()
                    {
                        continue;
                    }
                    failures = 0;
                }
                Err(_) => {
                    if self
                        .model
                        .record_adsb_error_if_query(&location, range_index, self.clock.monotonic())
                        .is_none()
                    {
                        continue;
                    }
                    failures = failures.saturating_add(1);
                }
            }
            let interval = if failures == 0 {
                SUCCESS_INTERVAL
            } else {
                Duration::from_secs(match failures {
                    1 => 3,
                    2 => 6,
                    3 => 12,
                    4 => 24,
                    _ => 30,
                })
            };
            let elapsed = self.clock.monotonic().saturating_sub(started_at);
            if !wait_for_command(
                &self.waiter,
                &commands,
                &stop,
                interval.saturating_sub(elapsed),
            ) {
                return;
            }
        }
    }
}

enum CommandDrain {
    Unchanged,
    Changed,
    Stop,
}

fn drain_commands(commands: &Receiver<WorkerCommand>, stop: &AtomicBool) -> CommandDrain {
    let mut changed = false;
    for command in commands.try_iter() {
        match command {
            WorkerCommand::SettingsChanged(_) => changed = true,
            WorkerCommand::Stop => {
                stop.store(true, Ordering::Release);
                return CommandDrain::Stop;
            }
        }
    }
    if changed {
        CommandDrain::Changed
    } else {
        CommandDrain::Unchanged
    }
}

fn wait_for_command<W: Waiter>(
    waiter: &W,
    commands: &Receiver<WorkerCommand>,
    stop: &AtomicBool,
    duration: Duration,
) -> bool {
    match waiter.wait(commands, stop, duration) {
        WaitResult::TimedOut => !stop.load(Ordering::Acquire),
        WaitResult::Command(WorkerCommand::SettingsChanged(_)) => !stop.load(Ordering::Acquire),
        WaitResult::Command(WorkerCommand::Stop) => {
            stop.store(true, Ordering::Release);
            false
        }
    }
}

trait SettingsWriter: Send + Sync + 'static {
    fn save(&self, settings: &RadarSettings) -> Result<(), crate::settings::SettingsError>;
}

impl SettingsWriter for SettingsStore {
    fn save(&self, settings: &RadarSettings) -> Result<(), crate::settings::SettingsError> {
        SettingsStore::save(self, settings)
    }
}

trait SettingsNotifier: Send + Sync + 'static {
    fn settings_changed(&self, settings: RadarSettings) -> Result<(), ()>;
}

struct ChannelSettingsNotifier(Sender<WorkerCommand>);

impl SettingsNotifier for ChannelSettingsNotifier {
    fn settings_changed(&self, settings: RadarSettings) -> Result<(), ()> {
        self.0
            .send(WorkerCommand::SettingsChanged(settings))
            .map_err(|_| ())
    }
}

pub struct RuntimeSettingsService {
    model: RuntimeModel,
    store: Arc<dyn SettingsWriter>,
    notifier: Arc<dyn SettingsNotifier>,
    update: Mutex<()>,
}

impl RuntimeSettingsService {
    pub fn new(
        model: RuntimeModel,
        store: Arc<SettingsStore>,
        commands: Sender<WorkerCommand>,
    ) -> Self {
        Self {
            model,
            store,
            notifier: Arc::new(ChannelSettingsNotifier(commands)),
            update: Mutex::new(()),
        }
    }

    #[cfg(test)]
    fn with_components(
        model: RuntimeModel,
        store: Arc<dyn SettingsWriter>,
        notifier: Arc<dyn SettingsNotifier>,
    ) -> Self {
        Self {
            model,
            store,
            notifier,
            update: Mutex::new(()),
        }
    }
}

impl SettingsService for RuntimeSettingsService {
    fn current(&self) -> RadarSettings {
        self.model.snapshot().settings
    }

    fn replace(&self, candidate: RadarSettings) -> Result<(), WebError> {
        let _update = self.update.lock().map_err(|_| WebError::State)?;
        self.store
            .save(&candidate)
            .map_err(|_| WebError::Settings)?;
        self.model.replace_settings(candidate.clone());
        self.notifier
            .settings_changed(candidate)
            .map_err(|_| WebError::WorkerUnavailable)
    }
}

pub struct RuntimeHealthSource<C> {
    model: RuntimeModel,
    now: Arc<C>,
}

impl<C: Fn() -> Duration + Send + Sync + 'static> RuntimeHealthSource<C> {
    pub fn new(model: RuntimeModel, now: Arc<C>) -> Self {
        Self { model, now }
    }
}

impl<C: Fn() -> Duration + Send + Sync + 'static> HealthSource for RuntimeHealthSource<C> {
    fn health(&self) -> HealthSnapshot {
        let snapshot = self.model.snapshot();
        HealthSnapshot {
            configured: snapshot.settings.location.is_some(),
            state: state_for(&snapshot),
            data_stale: snapshot
                .fetched_at
                .is_some_and(|at| (self.now)().saturating_sub(at) >= STALE_AFTER),
            revision: env!("PLANERADAR_REVISION"),
        }
    }
}

fn state_for(snapshot: &RuntimeSnapshot) -> AppState {
    if snapshot.settings.location.is_none() {
        AppState::SetupRequired
    } else if snapshot.fetched_at.is_some() {
        AppState::Radar
    } else {
        AppState::WaitingForNetwork
    }
}

#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error("runtime settings failed: {0}")]
    Settings(#[from] crate::settings::SettingsError),
    #[error("runtime network discovery failed: {0}")]
    Network(#[from] nix::Error),
    #[error("runtime web worker failed: {0}")]
    Web(#[from] WebError),
    #[error("runtime worker panicked")]
    WorkerPanic,
}

pub struct RuntimeHandle {
    pub model: RuntimeModel,
    pub commands: Sender<WorkerCommand>,
    pub stop: Arc<AtomicBool>,
    web_worker: Option<JoinHandle<Result<(), WebError>>>,
    adsb_worker: Option<JoinHandle<()>>,
}

impl RuntimeHandle {
    pub fn shutdown(mut self) -> Result<(), RuntimeError> {
        self.stop.store(true, Ordering::Release);
        let _ = self.commands.send(WorkerCommand::Stop);
        let mut failed = false;
        if let Some(worker) = self.adsb_worker.take() {
            failed |= worker.join().is_err();
        }
        if let Some(worker) = self.web_worker.take() {
            failed |= worker.join().map_or(true, |result| result.is_err());
        }
        if failed {
            Err(RuntimeError::WorkerPanic)
        } else {
            Ok(())
        }
    }
}

pub struct RuntimeCoordinator;

#[derive(Clone)]
struct SharedClock(Arc<SystemClock>);

impl Clock for SharedClock {
    fn monotonic(&self) -> Duration {
        self.0.monotonic()
    }

    fn unix_seconds(&self) -> u64 {
        self.0.unix_seconds()
    }
}

impl RuntimeCoordinator {
    pub fn start(config: RuntimeConfig) -> Result<RuntimeHandle, RuntimeError> {
        let store = Arc::new(SettingsStore::new(config.settings_path));
        let model = RuntimeModel::new(store.load()?, config.local_url.clone());
        let route_table = fs::read_to_string("/proc/net/route").unwrap_or_default();
        let ip_url = discover_ip_url(&route_table, current_interfaces()?.into_iter());
        model.set_urls(config.local_url.clone(), ip_url);

        let (commands, receiver) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        signal_hook::flag::register(SIGINT, stop.clone()).map_err(|_| RuntimeError::WorkerPanic)?;
        signal_hook::flag::register(SIGTERM, stop.clone())
            .map_err(|_| RuntimeError::WorkerPanic)?;

        let settings: Arc<dyn SettingsService> = Arc::new(RuntimeSettingsService::new(
            model.clone(),
            store,
            commands.clone(),
        ));
        let geocoder: Box<dyn GeocodeService> = Box::new(Geocoder::with_provider_base(
            UreqHttpClient,
            SystemClock::new(),
            ThreadSleeper,
            config.geocode_cache_path,
            config.nominatim_url,
        ));
        let clock = SharedClock(Arc::new(SystemClock::new()));
        let health: Arc<dyn HealthSource> = Arc::new(RuntimeHealthSource::new(model.clone(), {
            let clock = clock.clone();
            Arc::new(move || clock.monotonic())
        }));
        let allowed_hosts = {
            let model = model.clone();
            Arc::new(move || allowed_hosts(&model.snapshot()))
        };
        let server = Arc::new(SettingsServer::bind(
            config.http_address,
            settings,
            Arc::new(Mutex::new(geocoder)),
            health,
            allowed_hosts,
        )?);
        let web_stop = stop.clone();
        let web_worker = thread::spawn(move || server.run(&web_stop));
        let adsb_model = model.clone();
        let adsb_stop = stop.clone();
        let adsb_worker = thread::spawn(move || {
            AdsbWorker::new(
                AdsbClient::new(UreqHttpClient),
                adsb_model,
                clock,
                ChannelWaiter,
            )
            .run(receiver, adsb_stop);
        });
        Ok(RuntimeHandle {
            model,
            commands,
            stop,
            web_worker: Some(web_worker),
            adsb_worker: Some(adsb_worker),
        })
    }
}

fn allowed_hosts(snapshot: &RuntimeSnapshot) -> HashSet<String> {
    [
        Some(snapshot.local_url.as_str()),
        snapshot.ip_url.as_deref(),
    ]
    .into_iter()
    .flatten()
    .filter_map(|value| Url::parse(value).ok())
    .filter_map(|url| {
        url.host_str().map(|host| match url.port() {
            Some(port) => format!("{host}:{port}"),
            None => host.to_owned(),
        })
    })
    .collect()
}

#[cfg(test)]
mod tests {
    use std::sync::TryLockError;
    use std::sync::mpsc::{Receiver, SyncSender, sync_channel};

    use super::*;

    #[derive(Default)]
    struct RecordingWriter {
        persisted: Mutex<Option<RadarSettings>>,
    }

    impl SettingsWriter for RecordingWriter {
        fn save(&self, settings: &RadarSettings) -> Result<(), crate::settings::SettingsError> {
            *self.persisted.lock().expect("persisted settings") = Some(settings.clone());
            Ok(())
        }
    }

    struct BlockingNotifier {
        notified: Mutex<Option<RadarSettings>>,
        entered: SyncSender<()>,
        release: Mutex<Receiver<()>>,
    }

    impl SettingsNotifier for BlockingNotifier {
        fn settings_changed(&self, settings: RadarSettings) -> Result<(), ()> {
            *self.notified.lock().expect("notified settings") = Some(settings);
            self.entered.send(()).expect("notify test");
            self.release
                .lock()
                .expect("notify release")
                .recv()
                .expect("release notification");
            Ok(())
        }
    }

    #[test]
    fn settings_transaction_guard_spans_save_publish_and_notify() {
        let model = RuntimeModel::new(
            RadarSettings::default(),
            "http://planeradar.local".to_owned(),
        );
        let writer = Arc::new(RecordingWriter::default());
        let (entered_sender, entered_receiver) = sync_channel(0);
        let (release_sender, release_receiver) = sync_channel(0);
        let notifier = Arc::new(BlockingNotifier {
            notified: Mutex::new(None),
            entered: entered_sender,
            release: Mutex::new(release_receiver),
        });
        let service = Arc::new(RuntimeSettingsService::with_components(
            model.clone(),
            writer.clone(),
            notifier.clone(),
        ));
        let candidate = RadarSettings {
            location: Some(crate::model::Location {
                latitude: 40.7,
                longitude: -74.0,
                label: "test".to_owned(),
            }),
            ..RadarSettings::default()
        };

        let worker = {
            let service = service.clone();
            let candidate = candidate.clone();
            thread::spawn(move || service.replace(candidate))
        };
        entered_receiver.recv().expect("notification entered");

        assert!(matches!(
            service.update.try_lock(),
            Err(TryLockError::WouldBlock)
        ));
        assert_eq!(
            writer
                .persisted
                .lock()
                .expect("persisted settings")
                .as_ref(),
            Some(&candidate)
        );
        assert_eq!(model.snapshot().settings, candidate);
        assert_eq!(
            notifier
                .notified
                .lock()
                .expect("notified settings")
                .as_ref(),
            Some(&candidate)
        );

        release_sender.send(()).expect("release notification");
        worker.join().expect("settings worker").expect("replace");
    }
}

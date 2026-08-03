mod flight_worker;
mod solar_worker;
mod weather_worker;
pub use flight_worker::FlightDataWorker;
pub use solar_worker::SolarWorker;
pub use weather_worker::WeatherWorker;

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

use crate::adsb::{AdsbClient, AltitudeFilter};
use crate::flight_data::FlightDataClient;
use crate::geocode::{GeocodeService, Geocoder};
use crate::http::{HttpClient, UreqHttpClient};
use crate::model::{AppState, RadarSettings};
use crate::network::{current_interfaces, discover_ip_url};
use crate::range::{next_range_index, range_preset};
use crate::settings::SettingsStore;
use crate::solar::{SolarClient, load_cache};
use crate::time::{Clock, SystemClock, ThreadSleeper};
use crate::weather::WeatherClient;
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
    pub solar_cache_path: PathBuf,
    pub http_address: SocketAddr,
    pub local_url: String,
    pub nominatim_url: String,
    pub flight_data_url: String,
    pub weather_url: String,
    pub solar_url: String,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            settings_path: PathBuf::from("/var/lib/planeradar/settings.json"),
            geocode_cache_path: PathBuf::from("/var/lib/planeradar/geocode-cache.json"),
            solar_cache_path: PathBuf::from("/var/lib/planeradar/solar-schedule.json"),
            http_address: "0.0.0.0:80".parse().expect("valid default HTTP address"),
            local_url: "http://planeradar.local".to_owned(),
            nominatim_url: "https://nominatim.openstreetmap.org/search".to_owned(),
            flight_data_url: "https://api.adsbdb.com/v0".to_owned(),
            weather_url: "https://api.open-meteo.com/v1/forecast".to_owned(),
            solar_url: "https://api.open-meteo.com/v1/forecast".to_owned(),
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
            let filter = AltitudeFilter::from(&snapshot.settings);
            let Ok(range) = range_preset(range_index) else {
                return;
            };
            let started_at = self.clock.monotonic();
            let result = self.client.fetch(&location, range.outer_km, filter);

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
                            filter,
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
                        .record_adsb_error_if_query(
                            &location,
                            range_index,
                            filter,
                            self.clock.monotonic(),
                        )
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

pub(crate) enum CommandDrain {
    Unchanged,
    Changed,
    Stop,
}

pub(crate) fn drain_commands(
    commands: &Receiver<WorkerCommand>,
    stop: &AtomicBool,
) -> CommandDrain {
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

pub(crate) fn wait_for_command<W: Waiter>(
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

struct ChannelSettingsNotifier {
    senders: Vec<Sender<WorkerCommand>>,
}

impl SettingsNotifier for ChannelSettingsNotifier {
    fn settings_changed(&self, settings: RadarSettings) -> Result<(), ()> {
        let mut unavailable = false;
        for sender in &self.senders {
            if sender
                .send(WorkerCommand::SettingsChanged(settings.clone()))
                .is_err()
            {
                unavailable = true;
            }
        }
        if unavailable { Err(()) } else { Ok(()) }
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
        commands: Vec<Sender<WorkerCommand>>,
    ) -> Self {
        Self {
            model,
            store,
            notifier: Arc::new(ChannelSettingsNotifier { senders: commands }),
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

    fn cycle_range(&self) -> Result<RadarSettings, WebError> {
        self.update_settings(|model| {
            let mut candidate = model.snapshot().settings;
            candidate.range_index = next_range_index(candidate.range_index);
            candidate
        })
    }

    fn update_settings<F>(&self, transform: F) -> Result<RadarSettings, WebError>
    where
        F: FnOnce(&RuntimeModel) -> RadarSettings,
    {
        let _update = self.update.lock().map_err(|_| WebError::State)?;
        let candidate = transform(&self.model);
        self.commit(candidate.clone())?;
        Ok(candidate)
    }

    fn commit(&self, candidate: RadarSettings) -> Result<(), WebError> {
        self.store
            .save(&candidate)
            .map_err(|_| WebError::Settings)?;
        self.model.replace_settings(candidate.clone());
        self.notifier
            .settings_changed(candidate)
            .map_err(|_| WebError::WorkerUnavailable)
    }
}

impl SettingsService for RuntimeSettingsService {
    fn current(&self) -> RadarSettings {
        self.model.snapshot().settings
    }

    fn replace(&self, candidate: RadarSettings) -> Result<(), WebError> {
        let _update = self.update.lock().map_err(|_| WebError::State)?;
        self.commit(candidate)
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
    } else if snapshot.has_successful_fetch_for_current_location {
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
    pub stop: Arc<AtomicBool>,
    settings: Arc<RuntimeSettingsService>,
    clock: SharedClock,
    commands: Vec<Sender<WorkerCommand>>,
    web_worker: Option<JoinHandle<Result<(), WebError>>>,
    adsb_worker: Option<JoinHandle<()>>,
    flight_worker: Option<JoinHandle<()>>,
    weather_worker: Option<JoinHandle<()>>,
    solar_worker: Option<JoinHandle<()>>,
}

impl RuntimeHandle {
    pub fn cycle_range(&self) -> Result<RadarSettings, RuntimeError> {
        Ok(self.settings.cycle_range()?)
    }

    pub fn snapshot(&self) -> RuntimeSnapshot {
        self.model.snapshot()
    }

    pub fn monotonic(&self) -> Duration {
        self.clock.monotonic()
    }

    pub fn unix_seconds(&self) -> u64 {
        self.clock.unix_seconds()
    }

    pub fn stop_requested(&self) -> bool {
        self.stop.load(Ordering::Acquire)
    }

    pub fn shutdown(mut self) -> Result<(), RuntimeError> {
        self.stop.store(true, Ordering::Release);
        for commands in &self.commands {
            let _ = commands.send(WorkerCommand::Stop);
        }
        let mut failed = false;
        if let Some(worker) = self.adsb_worker.take() {
            failed |= worker.join().is_err();
        }
        if let Some(worker) = self.flight_worker.take() {
            failed |= worker.join().is_err();
        }
        if let Some(worker) = self.weather_worker.take() {
            failed |= worker.join().is_err();
        }
        if let Some(worker) = self.solar_worker.take() {
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
    fn load_initial_model(
        config: &RuntimeConfig,
    ) -> Result<(Arc<SettingsStore>, RuntimeModel), RuntimeError> {
        let store = Arc::new(SettingsStore::new(config.settings_path.clone()));
        let model = RuntimeModel::new(store.load()?, config.local_url.clone());
        let snapshot = model.snapshot();
        if snapshot.settings.brightness.night.enabled
            && let Some(location) = snapshot.settings.location.as_ref()
            && let Some(schedule) = load_cache(&config.solar_cache_path, location)
        {
            let _ = model.record_solar_schedule_if_current(location, Arc::new(schedule));
        }
        Ok((store, model))
    }

    pub fn start(config: RuntimeConfig) -> Result<RuntimeHandle, RuntimeError> {
        let (store, model) = Self::load_initial_model(&config)?;
        let route_table = fs::read_to_string("/proc/net/route").unwrap_or_default();
        let ip_url = discover_ip_url(&route_table, current_interfaces()?.into_iter());
        model.set_urls(config.local_url.clone(), ip_url);

        let (adsb_commands, adsb_receiver) = mpsc::channel();
        let (flight_commands, flight_receiver) = mpsc::channel();
        let (weather_commands, weather_receiver) = mpsc::channel();
        let (solar_commands, solar_receiver) = mpsc::channel();
        let commands = vec![
            adsb_commands.clone(),
            flight_commands.clone(),
            weather_commands.clone(),
            solar_commands.clone(),
        ];
        let stop = Arc::new(AtomicBool::new(false));
        signal_hook::flag::register(SIGINT, stop.clone()).map_err(|_| RuntimeError::WorkerPanic)?;
        signal_hook::flag::register(SIGTERM, stop.clone())
            .map_err(|_| RuntimeError::WorkerPanic)?;

        let settings = Arc::new(RuntimeSettingsService::new(
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
            settings.clone(),
            Arc::new(Mutex::new(geocoder)),
            health,
            config.local_url,
            allowed_hosts,
        )?);
        let web_stop = stop.clone();
        let web_worker = thread::spawn(move || server.run(&web_stop));
        let adsb_model = model.clone();
        let adsb_stop = stop.clone();
        let adsb_clock = clock.clone();
        let adsb_worker = thread::spawn(move || {
            AdsbWorker::new(
                AdsbClient::new(UreqHttpClient),
                adsb_model,
                adsb_clock,
                ChannelWaiter,
            )
            .run(adsb_receiver, adsb_stop);
        });
        let flight_model = model.clone();
        let flight_stop = stop.clone();
        let flight_clock = clock.clone();
        let flight_data_url = config.flight_data_url;
        let flight_worker = thread::spawn(move || {
            FlightDataWorker::new(
                FlightDataClient::with_provider_base(
                    UreqHttpClient,
                    flight_clock.clone(),
                    ThreadSleeper,
                    flight_data_url,
                ),
                flight_model,
                flight_clock,
                ChannelWaiter,
            )
            .run(flight_receiver, flight_stop);
        });
        let weather_model = model.clone();
        let weather_stop = stop.clone();
        let weather_clock = clock.clone();
        let weather_url = config.weather_url;
        let weather_worker = thread::spawn(move || {
            WeatherWorker::new(
                WeatherClient::with_provider_base(UreqHttpClient, weather_url),
                weather_model,
                weather_clock,
                ChannelWaiter,
            )
            .run(weather_receiver, weather_stop);
        });
        let solar_model = model.clone();
        let solar_stop = stop.clone();
        let solar_clock = clock.clone();
        let solar_url = config.solar_url;
        let solar_cache_path = config.solar_cache_path;
        let solar_worker = thread::spawn(move || {
            SolarWorker::new(
                SolarClient::with_provider_base(UreqHttpClient, solar_url),
                solar_cache_path,
                solar_model,
                solar_clock,
                ChannelWaiter,
            )
            .run(solar_receiver, solar_stop);
        });
        Ok(RuntimeHandle {
            model,
            stop,
            settings,
            clock,
            commands,
            web_worker: Some(web_worker),
            adsb_worker: Some(adsb_worker),
            flight_worker: Some(flight_worker),
            weather_worker: Some(weather_worker),
            solar_worker: Some(solar_worker),
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

    #[derive(Default)]
    struct RecordingNotifier {
        notified: Mutex<Option<RadarSettings>>,
    }

    impl SettingsNotifier for RecordingNotifier {
        fn settings_changed(&self, settings: RadarSettings) -> Result<(), ()> {
            *self.notified.lock().expect("notified settings") = Some(settings);
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

    #[test]
    fn range_cycle_reads_the_latest_settings_inside_the_transaction_guard() {
        let model = RuntimeModel::new(
            RadarSettings::default(),
            "http://planeradar.local".to_owned(),
        );
        let writer = Arc::new(RecordingWriter::default());
        let notifier = Arc::new(RecordingNotifier {
            notified: Mutex::new(None),
        });
        let service = RuntimeSettingsService::with_components(
            model.clone(),
            writer.clone(),
            notifier.clone(),
        );
        let web_candidate = RadarSettings {
            location: Some(crate::model::Location {
                latitude: 51.5,
                longitude: -0.1,
                label: "web".to_owned(),
            }),
            units: crate::model::Units::Miles,
            show_runways: false,
            range_index: 2,
            ..RadarSettings::default()
        };

        service.replace(web_candidate.clone()).expect("web replace");
        let cycled = service.cycle_range().expect("cycle range");
        let expected = RadarSettings {
            range_index: 3,
            ..web_candidate
        };
        assert_eq!(cycled, expected);
        assert_eq!(model.snapshot().settings, expected);
        assert_eq!(
            writer
                .persisted
                .lock()
                .expect("persisted settings")
                .as_ref(),
            Some(&expected)
        );
        assert_eq!(
            notifier
                .notified
                .lock()
                .expect("notified settings")
                .as_ref(),
            Some(&expected)
        );
    }

    #[test]
    fn settings_transform_runs_while_the_transaction_guard_is_held() {
        let model = RuntimeModel::new(
            RadarSettings::default(),
            "http://planeradar.local".to_owned(),
        );
        let writer = Arc::new(RecordingWriter::default());
        let notifier = Arc::new(RecordingNotifier::default());
        let service = Arc::new(RuntimeSettingsService::with_components(
            model.clone(),
            writer,
            notifier,
        ));

        let transformed = service
            .update_settings({
                let service = service.clone();
                move |model| {
                    assert!(matches!(
                        service.update.try_lock(),
                        Err(TryLockError::WouldBlock)
                    ));
                    let mut candidate = model.snapshot().settings;
                    candidate.range_index = next_range_index(candidate.range_index);
                    candidate
                }
            })
            .expect("transform");
        assert_eq!(transformed.range_index, 2);
        assert_eq!(model.snapshot().settings, transformed);
    }

    #[test]
    fn shutdown_stops_and_joins_all_four_independent_workers() {
        let model = RuntimeModel::new(
            RadarSettings::default(),
            "http://planeradar.local".to_owned(),
        );
        let writer = Arc::new(RecordingWriter::default());
        let notifier = Arc::new(RecordingNotifier::default());
        let settings = Arc::new(RuntimeSettingsService::with_components(
            model.clone(),
            writer,
            notifier,
        ));
        let stop = Arc::new(AtomicBool::new(false));
        let mut commands = Vec::new();
        let joined = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut workers = Vec::new();
        for _ in 0..4 {
            let (sender, receiver) = mpsc::channel();
            commands.push(sender);
            let joined = joined.clone();
            workers.push(thread::spawn(move || {
                assert!(matches!(receiver.recv(), Ok(WorkerCommand::Stop)));
                joined.fetch_add(1, Ordering::AcqRel);
            }));
        }
        let weather_worker = workers.pop();
        let solar_worker = workers.pop();
        let flight_worker = workers.pop();
        let adsb_worker = workers.pop();
        let handle = RuntimeHandle {
            model,
            stop: stop.clone(),
            settings,
            clock: SharedClock(Arc::new(SystemClock::new())),
            commands,
            web_worker: Some(thread::spawn(|| Ok(()))),
            adsb_worker,
            flight_worker,
            weather_worker,
            solar_worker,
        };

        handle.shutdown().expect("shutdown");

        assert!(stop.load(Ordering::Acquire));
        assert_eq!(joined.load(Ordering::Acquire), 4);
    }

    #[test]
    fn shutdown_preserves_worker_panic_error_after_joining_other_workers() {
        let model = RuntimeModel::new(
            RadarSettings::default(),
            "http://planeradar.local".to_owned(),
        );
        let settings = Arc::new(RuntimeSettingsService::with_components(
            model.clone(),
            Arc::new(RecordingWriter::default()),
            Arc::new(RecordingNotifier::default()),
        ));
        let stop = Arc::new(AtomicBool::new(false));
        let joined = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut commands = Vec::new();
        let mut workers = Vec::new();
        for _ in 0..3 {
            let (sender, receiver) = mpsc::channel();
            commands.push(sender);
            let joined = joined.clone();
            workers.push(thread::spawn(move || {
                assert!(matches!(receiver.recv(), Ok(WorkerCommand::Stop)));
                joined.fetch_add(1, Ordering::AcqRel);
            }));
        }
        let (panic_sender, _panic_receiver) = mpsc::channel();
        commands.push(panic_sender);
        let handle = RuntimeHandle {
            model,
            stop,
            settings,
            clock: SharedClock(Arc::new(SystemClock::new())),
            commands,
            web_worker: Some(thread::spawn(|| Ok(()))),
            adsb_worker: Some(thread::spawn(|| panic!("test worker panic"))),
            flight_worker: workers.pop(),
            weather_worker: workers.pop(),
            solar_worker: workers.pop(),
        };

        assert!(matches!(handle.shutdown(), Err(RuntimeError::WorkerPanic)));
        assert_eq!(joined.load(Ordering::Acquire), 3);
    }

    #[test]
    fn matching_cache_is_in_the_handle_snapshot_before_any_worker_starts() {
        use crate::model::Location;
        use crate::solar::{SolarDay, SolarSchedule, save_cache};

        let directory = tempfile::tempdir().expect("state directory");
        let settings_path = directory.path().join("settings.json");
        let solar_cache_path = directory.path().join("solar.json");
        let location = Location {
            latitude: 40.7769,
            longitude: -73.874,
            label: "Radar".to_owned(),
        };
        let mut configured = RadarSettings {
            location: Some(location.clone()),
            ..RadarSettings::default()
        };
        configured.brightness.night.enabled = true;
        let store = Arc::new(SettingsStore::new(settings_path.clone()));
        store.save(&configured).expect("settings");
        let schedule = SolarSchedule {
            schema_version: 1,
            latitude: location.latitude,
            longitude: location.longitude,
            time_zone: "America/New_York".to_owned(),
            fetched_at_unix: 1_785_700_000,
            days: (2..=18)
                .map(|day| SolarDay {
                    date: format!("2026-08-{day:02}"),
                    sunrise_unix: None,
                })
                .collect(),
        };
        save_cache(&solar_cache_path, &schedule).expect("solar cache");
        let config = RuntimeConfig {
            settings_path,
            solar_cache_path,
            ..RuntimeConfig::default()
        };
        let (store, model) =
            RuntimeCoordinator::load_initial_model(&config).expect("initial model");
        let settings = Arc::new(RuntimeSettingsService::new(
            model.clone(),
            store,
            Vec::new(),
        ));
        let handle = RuntimeHandle {
            model,
            stop: Arc::new(AtomicBool::new(false)),
            settings,
            clock: SharedClock(Arc::new(SystemClock::new())),
            commands: Vec::new(),
            web_worker: None,
            adsb_worker: None,
            flight_worker: None,
            weather_worker: None,
            solar_worker: None,
        };

        assert_eq!(handle.snapshot().solar_schedule.as_deref(), Some(&schedule));
        handle.shutdown().expect("shutdown");
    }
}

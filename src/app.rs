use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use signal_hook::consts::signal::SIGUSR1;
use thiserror::Error;

use crate::airports::{airports_within, load_embedded};
use crate::display::{DisplayHandler, DisplayUpdate, InputEvent};
use crate::model::{Airport, AppState, RadarSettings, RadarSnapshot};
use crate::range::range_preset;
use crate::render::radar::{BackgroundKey, RadarRenderer};
use crate::render::setup::SetupRenderer;
use crate::render::{Frame, RenderError};
use crate::runtime::{RuntimeError, RuntimeHandle, RuntimeSnapshot};
use crate::touch::{Gesture, GestureRecognizer};

const STALE_AFTER: Duration = Duration::from_secs(30);
const MAX_NEARBY_AIRPORTS: usize = 64;
const DEFAULT_DEBUG_FRAME: &str = "/var/lib/planeradar/debug.png";
const SETUP_MESSAGE: &str = "Open this page to set the radar location";
const WAITING_MESSAGE: &str = "WAITING FOR NETWORK";
const SETTINGS_MESSAGE: &str = "Settings are available on this page";

pub trait AppRuntime: Send {
    fn snapshot(&self) -> RuntimeSnapshot;
    fn cycle_range(&self) -> Result<RadarSettings, RuntimeError>;
    fn monotonic(&self) -> Duration;
    fn stop_requested(&self) -> bool;
    fn shutdown(self: Box<Self>) -> Result<(), RuntimeError>;
}

impl AppRuntime for RuntimeHandle {
    fn snapshot(&self) -> RuntimeSnapshot {
        self.model.snapshot()
    }

    fn cycle_range(&self) -> Result<RadarSettings, RuntimeError> {
        RuntimeHandle::cycle_range(self)
    }

    fn monotonic(&self) -> Duration {
        RuntimeHandle::monotonic(self)
    }

    fn stop_requested(&self) -> bool {
        RuntimeHandle::stop_requested(self)
    }

    fn shutdown(self: Box<Self>) -> Result<(), RuntimeError> {
        RuntimeHandle::shutdown(*self)
    }
}

#[derive(Debug, Error)]
pub enum AppError {
    #[error("Plane Radar runtime is unavailable")]
    RuntimeUnavailable,
    #[error("Plane Radar runtime failed: {0}")]
    Runtime(#[from] RuntimeError),
    #[error("Plane Radar rendering failed: {0}")]
    Render(#[from] RenderError),
    #[error("failed to register the debug-frame signal: {0}")]
    Signal(#[from] std::io::Error),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RenderKey {
    generation: u64,
    state: AppState,
    stale: bool,
}

pub struct PlaneRadarApp {
    runtime: Option<Box<dyn AppRuntime>>,
    radar: RadarRenderer,
    setup: SetupRenderer,
    airports: Vec<Airport>,
    visible_airports: Vec<Airport>,
    airport_key: Option<BackgroundKey>,
    gesture: GestureRecognizer,
    snapshot: RuntimeSnapshot,
    settings_open: bool,
    last_render_key: Option<RenderKey>,
    current_frame: Option<Frame>,
    debug_path: PathBuf,
    debug_requested: Arc<AtomicBool>,
}

impl PlaneRadarApp {
    pub fn new(runtime: RuntimeHandle, radar: RadarRenderer, setup: SetupRenderer) -> Self {
        let airports = match load_embedded() {
            Ok(airports) => airports,
            Err(_) => {
                log::error!("embedded airport data is unavailable");
                Vec::new()
            }
        };
        Self::with_runtime(
            Box::new(runtime),
            radar,
            setup,
            airports,
            PathBuf::from(DEFAULT_DEBUG_FRAME),
            Arc::new(AtomicBool::new(false)),
        )
    }

    pub fn with_runtime(
        runtime: Box<dyn AppRuntime>,
        radar: RadarRenderer,
        setup: SetupRenderer,
        airports: Vec<Airport>,
        debug_path: PathBuf,
        debug_requested: Arc<AtomicBool>,
    ) -> Self {
        let snapshot = runtime.snapshot();
        Self {
            runtime: Some(runtime),
            radar,
            setup,
            airports,
            visible_airports: Vec::new(),
            airport_key: None,
            gesture: GestureRecognizer::default(),
            snapshot,
            settings_open: false,
            last_render_key: None,
            current_frame: None,
            debug_path,
            debug_requested,
        }
    }

    pub fn install_debug_signal(&mut self, path: PathBuf) -> Result<(), AppError> {
        self.debug_path = path;
        signal_hook::flag::register(SIGUSR1, self.debug_requested.clone())?;
        Ok(())
    }

    pub fn state(&self) -> AppState {
        select_state(&self.snapshot, self.settings_open)
    }

    pub fn settings(&self) -> &RadarSettings {
        &self.snapshot.settings
    }

    pub fn handle_gesture(&mut self, gesture: Gesture) -> Result<(), AppError> {
        self.refresh_snapshot()?;
        match (self.state(), gesture) {
            (AppState::Radar, Gesture::Tap) => {
                self.runtime()?.cycle_range().map_err(AppError::Runtime)?;
                self.settings_open = false;
                self.refresh_snapshot()?;
            }
            (AppState::Settings, Gesture::Tap) => {
                self.settings_open = false;
            }
            (AppState::SetupRequired, Gesture::Tap)
            | (AppState::WaitingForNetwork, Gesture::Tap) => {}
            (_, Gesture::LongPress) if self.snapshot.settings.location.is_some() => {
                self.settings_open = true;
            }
            (_, Gesture::LongPress) => {}
        }
        Ok(())
    }

    pub fn shutdown(&mut self) -> Result<(), AppError> {
        let Some(runtime) = self.runtime.take() else {
            return Ok(());
        };
        runtime.shutdown()?;
        Ok(())
    }

    fn runtime(&self) -> Result<&dyn AppRuntime, AppError> {
        self.runtime.as_deref().ok_or(AppError::RuntimeUnavailable)
    }

    fn refresh_snapshot(&mut self) -> Result<(), AppError> {
        let snapshot = self.runtime()?.snapshot();
        self.snapshot = snapshot;
        if self.snapshot.settings.location.is_none() {
            self.settings_open = false;
        }
        Ok(())
    }

    fn render_key(&self, now: Duration) -> RenderKey {
        RenderKey {
            generation: self.snapshot.generation,
            state: self.state(),
            stale: is_stale(&self.snapshot, now),
        }
    }

    fn render_frame(&mut self, now: Duration) -> Result<Frame, AppError> {
        match self.state() {
            AppState::SetupRequired => Ok(self.setup.render(
                &self.snapshot.local_url,
                self.snapshot.ip_url.as_deref(),
                false,
                SETUP_MESSAGE,
            )?),
            AppState::WaitingForNetwork => Ok(self.setup.render(
                &self.snapshot.local_url,
                self.snapshot.ip_url.as_deref(),
                true,
                WAITING_MESSAGE,
            )?),
            AppState::Settings => Ok(self.setup.render(
                &self.snapshot.local_url,
                self.snapshot.ip_url.as_deref(),
                true,
                SETTINGS_MESSAGE,
            )?),
            AppState::Radar => {
                self.refresh_visible_airports()?;
                let radar_snapshot = RadarSnapshot {
                    aircraft: self.snapshot.aircraft.clone(),
                    enrichment: self.snapshot.enrichment.clone(),
                    environment: self.snapshot.environment.clone(),
                    fetched_at: self.snapshot.fetched_at,
                    last_error_at: self.snapshot.last_error_at,
                };
                Ok(self.radar.render(
                    &radar_snapshot,
                    &self.snapshot.settings,
                    &self.visible_airports,
                    now,
                )?)
            }
        }
    }

    fn refresh_visible_airports(&mut self) -> Result<(), AppError> {
        let key = BackgroundKey::from_settings(&self.snapshot.settings)?;
        if self.airport_key == Some(key) {
            return Ok(());
        }
        let location = self
            .snapshot
            .settings
            .location
            .as_ref()
            .ok_or(RenderError::UnconfiguredLocation)?;
        let range = range_preset(self.snapshot.settings.range_index).map_err(RenderError::Range)?;
        self.visible_airports = airports_within(
            &self.airports,
            location,
            range.outer_km,
            MAX_NEARBY_AIRPORTS,
        )
        .into_iter()
        .cloned()
        .collect();
        self.airport_key = Some(key);
        Ok(())
    }

    fn save_debug_frame(&self) {
        let Some(frame) = self.current_frame.as_ref() else {
            return;
        };
        if frame.save_png(self.debug_path.as_path()).is_err() {
            log::error!("failed to write debug frame");
        }
    }

    fn stop_requested(&self) -> bool {
        self.runtime
            .as_deref()
            .is_none_or(|runtime| runtime.stop_requested())
    }
}

impl DisplayHandler for PlaneRadarApp {
    fn step(&mut self, events: &[InputEvent], _now: Instant) -> DisplayUpdate {
        let now = self
            .runtime
            .as_deref()
            .map_or(Duration::ZERO, AppRuntime::monotonic);
        let mut exit = self.stop_requested();

        for gesture in self.gesture.tick(now) {
            if self.handle_gesture(gesture).is_err() {
                log::error!("display gesture update failed");
            }
        }
        for event in events {
            if matches!(event, InputEvent::Quit) {
                exit = true;
                continue;
            }
            for gesture in self.gesture.handle(event, now) {
                if self.handle_gesture(gesture).is_err() {
                    log::error!("display gesture update failed");
                }
            }
        }

        if self.runtime.is_some() && self.refresh_snapshot().is_err() {
            log::error!("runtime snapshot update failed");
            exit = true;
        }

        let mut frame = None;
        if !exit {
            let key = self.render_key(now);
            if self.last_render_key != Some(key) {
                match self.render_frame(now) {
                    Ok(rendered) => {
                        frame = Some(rendered.pixels().to_vec());
                        self.current_frame = Some(rendered);
                        self.last_render_key = Some(key);
                    }
                    Err(_) => {
                        log::error!("display rendering failed");
                        exit = true;
                    }
                }
            }
        }

        if self.debug_requested.swap(false, Ordering::AcqRel) {
            self.save_debug_frame();
        }

        if exit && self.shutdown().is_err() {
            log::error!("coordinated runtime shutdown failed");
        }
        DisplayUpdate { frame, exit }
    }

    fn shutdown(&mut self) {
        if PlaneRadarApp::shutdown(self).is_err() {
            log::error!("coordinated runtime shutdown failed");
        }
    }
}

impl Drop for PlaneRadarApp {
    fn drop(&mut self) {
        if self.shutdown().is_err() {
            log::error!("coordinated runtime shutdown failed");
        }
    }
}

fn select_state(snapshot: &RuntimeSnapshot, settings_open: bool) -> AppState {
    match (
        snapshot.settings.location.is_some(),
        settings_open,
        snapshot.has_successful_fetch_for_current_location,
    ) {
        (false, _, _) => AppState::SetupRequired,
        (true, true, _) => AppState::Settings,
        (true, false, true) => AppState::Radar,
        (true, false, false) => AppState::WaitingForNetwork,
    }
}

fn is_stale(snapshot: &RuntimeSnapshot, now: Duration) -> bool {
    snapshot
        .fetched_at
        .is_some_and(|fetched_at| now.saturating_sub(fetched_at) >= STALE_AFTER)
}

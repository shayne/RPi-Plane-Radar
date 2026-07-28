use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Deserializer, Serialize};
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use thiserror::Error;

use crate::state::{
    ArtifactIdentity, InstallPhase, InstallState, STATE_SCHEMA_VERSION, StateError, StateStore,
    TargetHardwareIdentity, TargetInstallState, TargetStateStore,
};
use crate::target::TargetIdentity;

const MAX_TARGET_INSTALL_JSON_BYTES: usize = 4 * 1024;
const MAX_APPLICATION_ARCHIVE_BYTES: u64 = 128 * 1024 * 1024;
const MAX_APPLICATION_PAYLOAD_BYTES: u64 = 64 * 1024 * 1024;

pub const TRYBOOT_TIMEOUT_GUIDANCE: &str = "The one-shot tryboot did not return. The target will fall back to the normal boot on its next power cycle; the staged transaction remains available to doctor, rollback, or a resumed install.";

#[derive(Debug, Error)]
pub enum ApplicationArchiveError {
    #[error("application archive I/O failed")]
    Io(#[from] io::Error),
    #[error("application archive is not a valid zstd-compressed tar archive")]
    InvalidArchive,
    #[error("application archive does not contain exactly one normalized planeradar payload")]
    InvalidMember,
}

#[derive(Debug)]
pub struct ApplicationPayload {
    path: PathBuf,
    sha256: String,
    size: u64,
    _private_directory: TempDir,
}

impl ApplicationPayload {
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn sha256(&self) -> &str {
        &self.sha256
    }

    pub fn size(&self) -> u64 {
        self.size
    }
}

pub fn extract_application_payload(
    archive_path: &Path,
    private_cache_root: &Path,
) -> Result<ApplicationPayload, ApplicationArchiveError> {
    let archive_metadata = fs::symlink_metadata(archive_path)?;
    if !archive_metadata.file_type().is_file()
        || archive_metadata.len() == 0
        || archive_metadata.len() > MAX_APPLICATION_ARCHIVE_BYTES
    {
        return Err(ApplicationArchiveError::InvalidArchive);
    }
    match fs::symlink_metadata(private_cache_root) {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let mut builder = fs::DirBuilder::new();
            builder.mode(0o700);
            builder.create(private_cache_root)?;
        }
        Err(error) => return Err(error.into()),
    }
    let cache_metadata = fs::symlink_metadata(private_cache_root)?;
    if !cache_metadata.file_type().is_dir()
        || cache_metadata.file_type().is_symlink()
        || cache_metadata.permissions().mode() & 0o777 != 0o700
    {
        return Err(ApplicationArchiveError::InvalidArchive);
    }
    let private_directory = tempfile::Builder::new()
        .prefix(".planeradar-payload-")
        .tempdir_in(private_cache_root)?;
    let staging_path = private_directory.path().join("payload");
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o700)
        .open(&staging_path)?;

    let mut archive_options = OpenOptions::new();
    archive_options.read(true).custom_flags(libc::O_NOFOLLOW);
    let archive_file = archive_options.open(archive_path)?;
    let opened_metadata = archive_file.metadata()?;
    if !opened_metadata.is_file() || opened_metadata.len() != archive_metadata.len() {
        return Err(ApplicationArchiveError::InvalidArchive);
    }
    let decoder = zstd::stream::read::Decoder::new(archive_file)
        .map_err(|_| ApplicationArchiveError::InvalidArchive)?;
    let mut archive = tar::Archive::new(decoder);
    archive.set_ignore_zeros(true);
    let mut entries = archive
        .entries()
        .map_err(|_| ApplicationArchiveError::InvalidArchive)?;
    let mut entry = entries
        .next()
        .ok_or(ApplicationArchiveError::InvalidMember)?
        .map_err(|_| ApplicationArchiveError::InvalidArchive)?;
    let header = entry.header();
    if entry.path_bytes().as_ref() != b"planeradar"
        || !header.entry_type().is_file()
        || header.mode().ok() != Some(0o755)
        || header.uid().ok() != Some(0)
        || header.gid().ok() != Some(0)
        || header.mtime().ok() != Some(0)
        || header
            .size()
            .ok()
            .is_none_or(|size| size == 0 || size > MAX_APPLICATION_PAYLOAD_BYTES)
    {
        return Err(ApplicationArchiveError::InvalidMember);
    }
    let declared_size = header
        .size()
        .map_err(|_| ApplicationArchiveError::InvalidMember)?;
    let mut hasher = Sha256::new();
    let mut copied = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = entry
            .read(&mut buffer)
            .map_err(|_| ApplicationArchiveError::InvalidArchive)?;
        if read == 0 {
            break;
        }
        copied = copied
            .checked_add(read as u64)
            .ok_or(ApplicationArchiveError::InvalidMember)?;
        if copied > declared_size || copied > MAX_APPLICATION_PAYLOAD_BYTES {
            return Err(ApplicationArchiveError::InvalidMember);
        }
        hasher.update(&buffer[..read]);
        output.write_all(&buffer[..read])?;
    }
    if copied != declared_size {
        return Err(ApplicationArchiveError::InvalidMember);
    }
    drop(entry);
    if entries.next().is_some() {
        return Err(ApplicationArchiveError::InvalidMember);
    }
    output.sync_all()?;
    drop(output);

    let sha256 = format!("{:x}", hasher.finalize());
    let content_directory = private_directory.path().join(&sha256);
    fs::create_dir(&content_directory)?;
    let path = content_directory.join("planeradar");
    fs::rename(staging_path, &path)?;
    fs::File::open(&content_directory)?.sync_all()?;

    Ok(ApplicationPayload {
        path,
        sha256,
        size: copied,
        _private_directory: private_directory,
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstallRequest {
    pub target: TargetIdentity,
    pub application: ArtifactIdentity,
    pub driver: ArtifactIdentity,
    pub desired_hostname: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PhaseVerification {
    Valid,
    Drifted,
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum BackendFailure {
    #[error("SSH connection was lost")]
    SshLost,
    #[error("the local installer process was interrupted")]
    MacProcessInterrupted,
    #[error("the one-shot tryboot target did not return")]
    TrybootTimedOut,
    #[error("tryboot verification failed")]
    TrybootVerificationFailed,
    #[error("the final Plane Radar service verification failed")]
    FinalServiceFailed,
    #[error("installation backend operation failed")]
    OperationFailed,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstallStatusEvent {
    pub phase: InstallPhase,
    pub message: &'static str,
}

pub trait InstallBackend: TargetStateStore {
    fn discover(&self, request: &InstallRequest) -> Result<TargetIdentity, BackendFailure>;
    fn run_preflight(&self, request: &InstallRequest) -> Result<(), BackendFailure>;
    fn acquire_application(
        &self,
        request: &InstallRequest,
    ) -> Result<ArtifactIdentity, BackendFailure>;
    fn prepare_driver(&self, request: &InstallRequest) -> Result<ArtifactIdentity, BackendFailure>;
    fn stage_tryboot(&self, request: &InstallRequest) -> Result<(), BackendFailure>;
    fn boot_and_verify_tryboot(&self, request: &InstallRequest) -> Result<(), BackendFailure>;
    fn accept_driver(&self, request: &InstallRequest) -> Result<(), BackendFailure>;
    fn install_application(
        &self,
        request: &InstallRequest,
    ) -> Result<TargetInstallResult, BackendFailure>;
    fn change_hostname_and_reconnect(
        &self,
        expected_identity: &TargetIdentity,
        desired_hostname: &str,
    ) -> Result<(), BackendFailure>;
    fn reboot_final(&self, request: &InstallRequest) -> Result<(), BackendFailure>;
    fn verify_final_service(&self, request: &InstallRequest) -> Result<(), BackendFailure>;
    fn finish(&self, request: &InstallRequest) -> Result<(), BackendFailure>;
    fn verify_phase(
        &self,
        phase: InstallPhase,
        request: &InstallRequest,
        state: &InstallState,
    ) -> Result<PhaseVerification, BackendFailure>;
    fn emit_status(&self, event: InstallStatusEvent) -> Result<(), BackendFailure>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InterruptionReason {
    SshLost,
    MacProcessInterrupted,
    TrybootTimedOut,
    TrybootVerificationFailed,
    FinalServiceFailed,
    PostconditionFailed(InstallPhase),
    BackendOperationFailed,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InstallOutcome {
    Complete,
    AlreadyComplete,
    Interrupted {
        phase: InstallPhase,
        reason: InterruptionReason,
        guidance: Option<&'static str>,
    },
}

pub struct Installer<'a, B, S> {
    backend: &'a B,
    state_store: &'a S,
}

impl<'a, B: InstallBackend, S: StateStore> Installer<'a, B, S> {
    pub fn new(backend: &'a B, state_store: &'a S) -> Self {
        Self {
            backend,
            state_store,
        }
    }

    pub fn run(&self, request: InstallRequest) -> Result<InstallOutcome, InstallError> {
        let mac_state = self.state_store.load()?;
        let target_state = self.backend.load_target_state()?;
        let (mut state, owned_files) = match (mac_state, target_state) {
            (None, None) => return self.run_first_pass(request),
            (Some(mac), None) if mac.phase <= InstallPhase::PreflightPassed => (mac, vec![]),
            (Some(mac), Some(target)) if records_agree(&mac, &target, &request) => {
                (mac, target.owned_files)
            }
            (Some(mac), Some(target))
                if target_is_reconcilable_one_phase_ahead(&mac, &target, &request) =>
            {
                let candidate = state_at_phase(&mac, target.last_verified_phase, &request);
                match self
                    .backend
                    .verify_phase(candidate.phase, &request, &candidate)
                {
                    Ok(PhaseVerification::Valid) => {
                        self.state_store.save(&candidate)?;
                        (candidate, target.owned_files)
                    }
                    Ok(PhaseVerification::Drifted) => {
                        return Err(InstallError::StateDisagreement);
                    }
                    Err(failure) => return Ok(interrupted(mac.phase, failure)),
                }
            }
            _ => return Err(InstallError::StateDisagreement),
        };

        let mut drifted = None;
        for phase in InstallPhase::ALL
            .into_iter()
            .take_while(|phase| *phase <= state.phase)
        {
            let phase_state = state_at_phase(&state, phase, &request);
            match self.backend.verify_phase(phase, &request, &phase_state) {
                Ok(PhaseVerification::Valid) => {}
                Ok(PhaseVerification::Drifted) => {
                    drifted = Some(phase);
                    break;
                }
                Err(failure) => return Ok(interrupted(state.phase, failure)),
            }
        }

        let start = if let Some(drifted) = drifted {
            match previous_phase(drifted) {
                Some(previous) => {
                    state = state_at_phase(&state, previous, &request);
                    self.persist(&state, &owned_files)?;
                    phase_index(drifted)
                }
                None => {
                    return self.run_from_phase(None, &owned_files, 0, &request);
                }
            }
        } else if state.phase == InstallPhase::Complete {
            return Ok(InstallOutcome::AlreadyComplete);
        } else {
            phase_index(state.phase) + 1
        };

        self.run_from_phase(Some(state), &owned_files, start, &request)
    }

    fn run_from_phase(
        &self,
        mut state: Option<InstallState>,
        owned_files: &[crate::state::OwnedFile],
        start: usize,
        request: &InstallRequest,
    ) -> Result<InstallOutcome, InstallError> {
        for phase in InstallPhase::ALL.into_iter().skip(start) {
            let durable_phase = state
                .as_ref()
                .map(|state| state.phase)
                .unwrap_or(InstallPhase::Discovered);
            let candidate = match self.perform_phase(phase, state.as_ref(), request) {
                Ok(candidate) => candidate,
                Err(ActionError::Backend(failure)) => {
                    return Ok(interrupted(durable_phase, failure));
                }
                Err(ActionError::Install(error)) => return Err(error),
            };
            match self.backend.verify_phase(phase, request, &candidate) {
                Ok(PhaseVerification::Valid) => {}
                Ok(PhaseVerification::Drifted) => {
                    return Ok(InstallOutcome::Interrupted {
                        phase: durable_phase,
                        reason: InterruptionReason::PostconditionFailed(phase),
                        guidance: None,
                    });
                }
                Err(failure) => return Ok(interrupted(durable_phase, failure)),
            }
            self.persist(&candidate, owned_files)?;
            if let Err(failure) = self.backend.emit_status(status(phase)) {
                return Ok(interrupted(phase, failure));
            }
            state = Some(candidate);
        }
        Ok(InstallOutcome::Complete)
    }

    fn perform_phase(
        &self,
        phase: InstallPhase,
        state: Option<&InstallState>,
        request: &InstallRequest,
    ) -> Result<InstallState, ActionError> {
        let mut next = match state {
            Some(state) => state.clone(),
            None => InstallState {
                schema_version: STATE_SCHEMA_VERSION,
                target: request.target.clone(),
                phase: InstallPhase::Discovered,
                application: None,
                driver: None,
            },
        };
        match phase {
            InstallPhase::Discovered => {
                let observed = self
                    .backend
                    .discover(request)
                    .map_err(ActionError::Backend)?;
                if observed != request.target {
                    return Err(ActionError::Install(
                        InstallError::DiscoveredIdentityMismatch,
                    ));
                }
                next.target = observed;
            }
            InstallPhase::PreflightPassed => {
                self.backend
                    .run_preflight(request)
                    .map_err(ActionError::Backend)?;
            }
            InstallPhase::ApplicationAcquired => {
                let acquired = self
                    .backend
                    .acquire_application(request)
                    .map_err(ActionError::Backend)?;
                if acquired != request.application {
                    return Err(ActionError::Install(
                        InstallError::ArtifactIdentityMismatch { phase },
                    ));
                }
                next.application = Some(acquired);
            }
            InstallPhase::DriverReady => {
                let prepared = self
                    .backend
                    .prepare_driver(request)
                    .map_err(ActionError::Backend)?;
                if prepared != request.driver {
                    return Err(ActionError::Install(
                        InstallError::ArtifactIdentityMismatch { phase },
                    ));
                }
                next.driver = Some(prepared);
            }
            InstallPhase::TrybootStaged => self
                .backend
                .stage_tryboot(request)
                .map_err(ActionError::Backend)?,
            InstallPhase::TrybootVerified => self
                .backend
                .boot_and_verify_tryboot(request)
                .map_err(ActionError::Backend)?,
            InstallPhase::DriverAccepted => self
                .backend
                .accept_driver(request)
                .map_err(ActionError::Backend)?,
            InstallPhase::ApplicationInstalled => {
                let installed = self
                    .backend
                    .install_application(request)
                    .map_err(ActionError::Backend)?;
                installed
                    .validate()
                    .map_err(|_| ActionError::Install(InstallError::InvalidTargetInstallResult))?;
                if installed.revision != request.application.source_commit
                    || installed.sha256 != request.application.sha256
                {
                    return Err(ActionError::Install(
                        InstallError::ArtifactIdentityMismatch { phase },
                    ));
                }
            }
            InstallPhase::HostnameChanged => self
                .backend
                .change_hostname_and_reconnect(&next.target, &request.desired_hostname)
                .map_err(ActionError::Backend)?,
            InstallPhase::FinalRebooted => self
                .backend
                .reboot_final(request)
                .map_err(ActionError::Backend)?,
            InstallPhase::FinalVerified => self
                .backend
                .verify_final_service(request)
                .map_err(ActionError::Backend)?,
            InstallPhase::Complete => self.backend.finish(request).map_err(ActionError::Backend)?,
        }
        next.phase = phase;
        Ok(next)
    }

    fn run_first_pass(&self, request: InstallRequest) -> Result<InstallOutcome, InstallError> {
        let mac_state = self.state_store.load()?;
        let target_state = self.backend.load_target_state()?;
        match (mac_state, target_state) {
            (None, None) => {}
            (Some(mac), Some(target)) if records_agree(&mac, &target, &request) => {
                return Ok(InstallOutcome::AlreadyComplete);
            }
            _ => return Err(InstallError::StateDisagreement),
        }

        let observed = match self.backend.discover(&request) {
            Ok(observed) => observed,
            Err(failure) => return Ok(interrupted(InstallPhase::Discovered, failure)),
        };
        if observed != request.target {
            return Err(InstallError::DiscoveredIdentityMismatch);
        }
        let state = InstallState {
            schema_version: STATE_SCHEMA_VERSION,
            target: observed,
            phase: InstallPhase::Discovered,
            application: None,
            driver: None,
        };
        match self
            .backend
            .verify_phase(InstallPhase::Discovered, &request, &state)
        {
            Ok(PhaseVerification::Valid) => {}
            Ok(PhaseVerification::Drifted) => {
                return Ok(InstallOutcome::Interrupted {
                    phase: InstallPhase::Discovered,
                    reason: InterruptionReason::PostconditionFailed(InstallPhase::Discovered),
                    guidance: None,
                });
            }
            Err(failure) => return Ok(interrupted(InstallPhase::Discovered, failure)),
        }
        self.persist(&state, &[])?;
        if let Err(failure) = self.backend.emit_status(status(InstallPhase::Discovered)) {
            return Ok(interrupted(InstallPhase::Discovered, failure));
        }

        if let Err(failure) = self.backend.run_preflight(&request) {
            return Ok(interrupted(InstallPhase::Discovered, failure));
        }
        let state = InstallState {
            phase: InstallPhase::PreflightPassed,
            ..state
        };
        match self
            .backend
            .verify_phase(InstallPhase::PreflightPassed, &request, &state)
        {
            Ok(PhaseVerification::Valid) => {}
            Ok(PhaseVerification::Drifted) => {
                return Ok(InstallOutcome::Interrupted {
                    phase: InstallPhase::Discovered,
                    reason: InterruptionReason::PostconditionFailed(InstallPhase::PreflightPassed),
                    guidance: None,
                });
            }
            Err(failure) => return Ok(interrupted(InstallPhase::Discovered, failure)),
        }
        self.persist(&state, &[])?;
        if let Err(failure) = self
            .backend
            .emit_status(status(InstallPhase::PreflightPassed))
        {
            return Ok(interrupted(InstallPhase::PreflightPassed, failure));
        }

        let acquired = match self.backend.acquire_application(&request) {
            Ok(acquired) => acquired,
            Err(failure) => return Ok(interrupted(InstallPhase::PreflightPassed, failure)),
        };
        if acquired != request.application {
            return Err(InstallError::ArtifactIdentityMismatch {
                phase: InstallPhase::ApplicationAcquired,
            });
        }
        let state = InstallState {
            phase: InstallPhase::ApplicationAcquired,
            application: Some(acquired),
            ..state
        };
        match self
            .backend
            .verify_phase(InstallPhase::ApplicationAcquired, &request, &state)
        {
            Ok(PhaseVerification::Valid) => {}
            Ok(PhaseVerification::Drifted) => {
                return Ok(InstallOutcome::Interrupted {
                    phase: InstallPhase::PreflightPassed,
                    reason: InterruptionReason::PostconditionFailed(
                        InstallPhase::ApplicationAcquired,
                    ),
                    guidance: None,
                });
            }
            Err(failure) => return Ok(interrupted(InstallPhase::PreflightPassed, failure)),
        }
        self.persist(&state, &[])?;
        if let Err(failure) = self
            .backend
            .emit_status(status(InstallPhase::ApplicationAcquired))
        {
            return Ok(interrupted(InstallPhase::ApplicationAcquired, failure));
        }

        let prepared = match self.backend.prepare_driver(&request) {
            Ok(prepared) => prepared,
            Err(failure) => return Ok(interrupted(InstallPhase::ApplicationAcquired, failure)),
        };
        if prepared != request.driver {
            return Err(InstallError::ArtifactIdentityMismatch {
                phase: InstallPhase::DriverReady,
            });
        }
        let state = InstallState {
            phase: InstallPhase::DriverReady,
            driver: Some(prepared),
            ..state
        };
        match self
            .backend
            .verify_phase(InstallPhase::DriverReady, &request, &state)
        {
            Ok(PhaseVerification::Valid) => {}
            Ok(PhaseVerification::Drifted) => {
                return Ok(InstallOutcome::Interrupted {
                    phase: InstallPhase::ApplicationAcquired,
                    reason: InterruptionReason::PostconditionFailed(InstallPhase::DriverReady),
                    guidance: None,
                });
            }
            Err(failure) => return Ok(interrupted(InstallPhase::ApplicationAcquired, failure)),
        }
        self.persist(&state, &[])?;
        if let Err(failure) = self.backend.emit_status(status(InstallPhase::DriverReady)) {
            return Ok(interrupted(InstallPhase::DriverReady, failure));
        }

        if let Err(failure) = self.backend.stage_tryboot(&request) {
            return Ok(interrupted(InstallPhase::DriverReady, failure));
        }
        let state = InstallState {
            phase: InstallPhase::TrybootStaged,
            ..state
        };
        match self
            .backend
            .verify_phase(InstallPhase::TrybootStaged, &request, &state)
        {
            Ok(PhaseVerification::Valid) => {}
            Ok(PhaseVerification::Drifted) => {
                return Ok(InstallOutcome::Interrupted {
                    phase: InstallPhase::DriverReady,
                    reason: InterruptionReason::PostconditionFailed(InstallPhase::TrybootStaged),
                    guidance: None,
                });
            }
            Err(failure) => return Ok(interrupted(InstallPhase::DriverReady, failure)),
        }
        self.persist(&state, &[])?;
        if let Err(failure) = self
            .backend
            .emit_status(status(InstallPhase::TrybootStaged))
        {
            return Ok(interrupted(InstallPhase::TrybootStaged, failure));
        }

        if let Err(failure) = self.backend.boot_and_verify_tryboot(&request) {
            return Ok(interrupted(InstallPhase::TrybootStaged, failure));
        }
        let state = InstallState {
            phase: InstallPhase::TrybootVerified,
            ..state
        };
        match self
            .backend
            .verify_phase(InstallPhase::TrybootVerified, &request, &state)
        {
            Ok(PhaseVerification::Valid) => {}
            Ok(PhaseVerification::Drifted) => {
                return Ok(InstallOutcome::Interrupted {
                    phase: InstallPhase::TrybootStaged,
                    reason: InterruptionReason::PostconditionFailed(InstallPhase::TrybootVerified),
                    guidance: None,
                });
            }
            Err(failure) => return Ok(interrupted(InstallPhase::TrybootStaged, failure)),
        }
        self.persist(&state, &[])?;
        if let Err(failure) = self
            .backend
            .emit_status(status(InstallPhase::TrybootVerified))
        {
            return Ok(interrupted(InstallPhase::TrybootVerified, failure));
        }

        if let Err(failure) = self.backend.accept_driver(&request) {
            return Ok(interrupted(InstallPhase::TrybootVerified, failure));
        }
        let state = InstallState {
            phase: InstallPhase::DriverAccepted,
            ..state
        };
        match self
            .backend
            .verify_phase(InstallPhase::DriverAccepted, &request, &state)
        {
            Ok(PhaseVerification::Valid) => {}
            Ok(PhaseVerification::Drifted) => {
                return Ok(InstallOutcome::Interrupted {
                    phase: InstallPhase::TrybootVerified,
                    reason: InterruptionReason::PostconditionFailed(InstallPhase::DriverAccepted),
                    guidance: None,
                });
            }
            Err(failure) => return Ok(interrupted(InstallPhase::TrybootVerified, failure)),
        }
        self.persist(&state, &[])?;
        if let Err(failure) = self
            .backend
            .emit_status(status(InstallPhase::DriverAccepted))
        {
            return Ok(interrupted(InstallPhase::DriverAccepted, failure));
        }

        let installed = match self.backend.install_application(&request) {
            Ok(installed) => installed,
            Err(failure) => return Ok(interrupted(InstallPhase::DriverAccepted, failure)),
        };
        installed
            .validate()
            .map_err(|_| InstallError::InvalidTargetInstallResult)?;
        if installed.revision != request.application.source_commit
            || installed.sha256 != request.application.sha256
        {
            return Err(InstallError::ArtifactIdentityMismatch {
                phase: InstallPhase::ApplicationInstalled,
            });
        }
        let state = InstallState {
            phase: InstallPhase::ApplicationInstalled,
            ..state
        };
        match self
            .backend
            .verify_phase(InstallPhase::ApplicationInstalled, &request, &state)
        {
            Ok(PhaseVerification::Valid) => {}
            Ok(PhaseVerification::Drifted) => {
                return Ok(InstallOutcome::Interrupted {
                    phase: InstallPhase::DriverAccepted,
                    reason: InterruptionReason::PostconditionFailed(
                        InstallPhase::ApplicationInstalled,
                    ),
                    guidance: None,
                });
            }
            Err(failure) => return Ok(interrupted(InstallPhase::DriverAccepted, failure)),
        }
        self.persist(&state, &[])?;
        if let Err(failure) = self
            .backend
            .emit_status(status(InstallPhase::ApplicationInstalled))
        {
            return Ok(interrupted(InstallPhase::ApplicationInstalled, failure));
        }

        if let Err(failure) = self
            .backend
            .change_hostname_and_reconnect(&state.target, &request.desired_hostname)
        {
            return Ok(interrupted(InstallPhase::ApplicationInstalled, failure));
        }
        let state = InstallState {
            phase: InstallPhase::HostnameChanged,
            ..state
        };
        match self
            .backend
            .verify_phase(InstallPhase::HostnameChanged, &request, &state)
        {
            Ok(PhaseVerification::Valid) => {}
            Ok(PhaseVerification::Drifted) => {
                return Ok(InstallOutcome::Interrupted {
                    phase: InstallPhase::ApplicationInstalled,
                    reason: InterruptionReason::PostconditionFailed(InstallPhase::HostnameChanged),
                    guidance: None,
                });
            }
            Err(failure) => return Ok(interrupted(InstallPhase::ApplicationInstalled, failure)),
        }
        self.persist(&state, &[])?;
        if let Err(failure) = self
            .backend
            .emit_status(status(InstallPhase::HostnameChanged))
        {
            return Ok(interrupted(InstallPhase::HostnameChanged, failure));
        }

        if let Err(failure) = self.backend.reboot_final(&request) {
            return Ok(interrupted(InstallPhase::HostnameChanged, failure));
        }
        let state = InstallState {
            phase: InstallPhase::FinalRebooted,
            ..state
        };
        match self
            .backend
            .verify_phase(InstallPhase::FinalRebooted, &request, &state)
        {
            Ok(PhaseVerification::Valid) => {}
            Ok(PhaseVerification::Drifted) => {
                return Ok(InstallOutcome::Interrupted {
                    phase: InstallPhase::HostnameChanged,
                    reason: InterruptionReason::PostconditionFailed(InstallPhase::FinalRebooted),
                    guidance: None,
                });
            }
            Err(failure) => return Ok(interrupted(InstallPhase::HostnameChanged, failure)),
        }
        self.persist(&state, &[])?;
        if let Err(failure) = self
            .backend
            .emit_status(status(InstallPhase::FinalRebooted))
        {
            return Ok(interrupted(InstallPhase::FinalRebooted, failure));
        }

        if let Err(failure) = self.backend.verify_final_service(&request) {
            return Ok(interrupted(InstallPhase::FinalRebooted, failure));
        }
        let state = InstallState {
            phase: InstallPhase::FinalVerified,
            ..state
        };
        match self
            .backend
            .verify_phase(InstallPhase::FinalVerified, &request, &state)
        {
            Ok(PhaseVerification::Valid) => {}
            Ok(PhaseVerification::Drifted) => {
                return Ok(InstallOutcome::Interrupted {
                    phase: InstallPhase::FinalRebooted,
                    reason: InterruptionReason::PostconditionFailed(InstallPhase::FinalVerified),
                    guidance: None,
                });
            }
            Err(failure) => return Ok(interrupted(InstallPhase::FinalRebooted, failure)),
        }
        self.persist(&state, &[])?;
        if let Err(failure) = self
            .backend
            .emit_status(status(InstallPhase::FinalVerified))
        {
            return Ok(interrupted(InstallPhase::FinalVerified, failure));
        }

        if let Err(failure) = self.backend.finish(&request) {
            return Ok(interrupted(InstallPhase::FinalVerified, failure));
        }
        let state = InstallState {
            phase: InstallPhase::Complete,
            ..state
        };
        match self
            .backend
            .verify_phase(InstallPhase::Complete, &request, &state)
        {
            Ok(PhaseVerification::Valid) => {}
            Ok(PhaseVerification::Drifted) => {
                return Ok(InstallOutcome::Interrupted {
                    phase: InstallPhase::FinalVerified,
                    reason: InterruptionReason::PostconditionFailed(InstallPhase::Complete),
                    guidance: None,
                });
            }
            Err(failure) => return Ok(interrupted(InstallPhase::FinalVerified, failure)),
        }
        self.persist(&state, &[])?;
        if let Err(failure) = self.backend.emit_status(status(InstallPhase::Complete)) {
            return Ok(interrupted(InstallPhase::Complete, failure));
        }
        Ok(InstallOutcome::Complete)
    }

    fn persist(
        &self,
        state: &InstallState,
        owned_files: &[crate::state::OwnedFile],
    ) -> Result<(), InstallError> {
        let target_state = TargetInstallState {
            schema_version: STATE_SCHEMA_VERSION,
            hardware: TargetHardwareIdentity {
                model: state.target.model.clone(),
                serial: state.target.serial.clone(),
            },
            application: state.application.clone(),
            driver: state.driver.clone(),
            owned_files: owned_files.to_vec(),
            last_verified_phase: state.phase,
        };
        if state.phase >= InstallPhase::ApplicationAcquired {
            self.backend.save_target_state(&target_state)?;
            let saved = self
                .backend
                .load_target_state()?
                .ok_or(InstallError::StateDisagreement)?;
            if !target_matches_mac_state(state, &saved) {
                return Err(InstallError::StateDisagreement);
            }
        }
        self.state_store.save(state)?;
        Ok(())
    }
}

enum ActionError {
    Backend(BackendFailure),
    Install(InstallError),
}

fn phase_index(phase: InstallPhase) -> usize {
    InstallPhase::ALL
        .iter()
        .position(|candidate| *candidate == phase)
        .expect("InstallPhase::ALL contains every phase")
}

fn previous_phase(phase: InstallPhase) -> Option<InstallPhase> {
    phase_index(phase)
        .checked_sub(1)
        .map(|index| InstallPhase::ALL[index])
}

fn state_at_phase(
    state: &InstallState,
    phase: InstallPhase,
    request: &InstallRequest,
) -> InstallState {
    InstallState {
        schema_version: STATE_SCHEMA_VERSION,
        target: state.target.clone(),
        phase,
        application: (phase >= InstallPhase::ApplicationAcquired)
            .then(|| request.application.clone()),
        driver: (phase >= InstallPhase::DriverReady).then(|| request.driver.clone()),
    }
}

fn status(phase: InstallPhase) -> InstallStatusEvent {
    InstallStatusEvent {
        phase,
        message: match phase {
            InstallPhase::Discovered => "target discovered",
            InstallPhase::PreflightPassed => "preflight passed",
            InstallPhase::ApplicationAcquired => "application acquired",
            InstallPhase::DriverReady => "driver ready",
            InstallPhase::TrybootStaged => "tryboot staged",
            InstallPhase::TrybootVerified => "tryboot verified",
            InstallPhase::DriverAccepted => "driver accepted",
            InstallPhase::ApplicationInstalled => "application installed",
            InstallPhase::HostnameChanged => "hostname changed",
            InstallPhase::FinalRebooted => "final reboot complete",
            InstallPhase::FinalVerified => "final service verified",
            InstallPhase::Complete => "installation complete",
        },
    }
}

fn interrupted(phase: InstallPhase, failure: BackendFailure) -> InstallOutcome {
    let (reason, guidance) = match failure {
        BackendFailure::SshLost => (InterruptionReason::SshLost, None),
        BackendFailure::MacProcessInterrupted => (InterruptionReason::MacProcessInterrupted, None),
        BackendFailure::TrybootTimedOut => (
            InterruptionReason::TrybootTimedOut,
            Some(TRYBOOT_TIMEOUT_GUIDANCE),
        ),
        BackendFailure::TrybootVerificationFailed => {
            (InterruptionReason::TrybootVerificationFailed, None)
        }
        BackendFailure::FinalServiceFailed => (InterruptionReason::FinalServiceFailed, None),
        BackendFailure::OperationFailed => (InterruptionReason::BackendOperationFailed, None),
    };
    InstallOutcome::Interrupted {
        phase,
        reason,
        guidance,
    }
}

fn records_agree(
    mac: &InstallState,
    target: &TargetInstallState,
    request: &InstallRequest,
) -> bool {
    mac.target == request.target
        && target_matches_mac_state(mac, target)
        && mac
            .application
            .as_ref()
            .is_none_or(|application| application == &request.application)
        && mac
            .driver
            .as_ref()
            .is_none_or(|driver| driver == &request.driver)
}

fn target_matches_mac_state(mac: &InstallState, target: &TargetInstallState) -> bool {
    target.hardware.model == mac.target.model
        && target.hardware.serial == mac.target.serial
        && target.last_verified_phase == mac.phase
        && target.application == mac.application
        && target.driver == mac.driver
}

fn target_is_reconcilable_one_phase_ahead(
    mac: &InstallState,
    target: &TargetInstallState,
    request: &InstallRequest,
) -> bool {
    previous_phase(target.last_verified_phase) == Some(mac.phase)
        && target.last_verified_phase >= InstallPhase::ApplicationAcquired
        && target.hardware.model == mac.target.model
        && target.hardware.serial == mac.target.serial
        && target.application
            == (target.last_verified_phase >= InstallPhase::ApplicationAcquired)
                .then(|| request.application.clone())
        && target.driver
            == (target.last_verified_phase >= InstallPhase::DriverReady)
                .then(|| request.driver.clone())
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct TargetInstallResult {
    pub schema_version: u32,
    pub files_changed: bool,
    pub boot_config_changed: bool,
    pub reboot_required: bool,
    pub revision: String,
    pub sha256: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTargetInstallResult {
    schema_version: u32,
    files_changed: bool,
    boot_config_changed: bool,
    reboot_required: bool,
    revision: String,
    sha256: String,
}

impl<'de> Deserialize<'de> for TargetInstallResult {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = RawTargetInstallResult::deserialize(deserializer)?;
        let result = Self {
            schema_version: raw.schema_version,
            files_changed: raw.files_changed,
            boot_config_changed: raw.boot_config_changed,
            reboot_required: raw.reboot_required,
            revision: raw.revision,
            sha256: raw.sha256,
        };
        result
            .validate()
            .map_err(serde::de::Error::custom)
            .map(|()| result)
    }
}

impl TargetInstallResult {
    pub fn from_json(contents: &[u8]) -> Result<Self, TargetInstallJsonError> {
        if contents.len() > MAX_TARGET_INSTALL_JSON_BYTES {
            return Err(TargetInstallJsonError);
        }
        let mut deserializer = serde_json::Deserializer::from_slice(contents);
        let result = Self::deserialize(&mut deserializer).map_err(|_| TargetInstallJsonError)?;
        deserializer.end().map_err(|_| TargetInstallJsonError)?;
        Ok(result)
    }

    pub fn to_json(&self) -> Result<String, TargetInstallJsonError> {
        self.validate().map_err(|_| TargetInstallJsonError)?;
        serde_json::to_string(self).map_err(|_| TargetInstallJsonError)
    }

    fn validate(&self) -> Result<(), &'static str> {
        if self.schema_version != 1
            || !is_lower_hex(&self.revision, 40)
            || !is_lower_hex(&self.sha256, 64)
        {
            return Err("invalid target install result");
        }
        Ok(())
    }
}

fn is_lower_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
#[error("target install JSON is invalid")]
pub struct TargetInstallJsonError;

#[derive(Debug, Error)]
pub enum InstallError {
    #[error("Mac and target installer records do not agree")]
    StateDisagreement,
    #[error("discovered target identity does not match the requested target")]
    DiscoveredIdentityMismatch,
    #[error("installer artifact identity did not match at {phase:?}")]
    ArtifactIdentityMismatch { phase: InstallPhase },
    #[error("target installer returned an invalid machine result")]
    InvalidTargetInstallResult,
    #[error("installer state operation failed")]
    State(#[from] StateError),
}

impl InstallError {
    pub fn is_state_disagreement(&self) -> bool {
        matches!(self, Self::StateDisagreement)
    }
}

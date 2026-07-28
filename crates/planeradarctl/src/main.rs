use std::cell::RefCell;
use std::io;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::thread;
use std::time::{Duration, Instant};
use std::{env, fs};

use clap::Parser;
use planeradarctl::{
    DriverLock,
    cli::{Cli, Command, DriverCommand},
    config::{Environment, InstallConfig},
    driver::{
        DriverAction, DriverContext, DriverManager, DriverTool, GhDriverReleaseSource,
        GhDriverReleaseVerifier, TargetProbe as DriverTargetProbe,
    },
    install::{
        ApplicationPayload, BackendFailure, InstallBackend, InstallOutcome, InstallRequest,
        InstallStatusEvent, Installer, PhaseVerification, TargetInstallResult,
        extract_application_payload,
    },
    preflight::{HostPreflight, SystemUnixClock, TargetPreflight},
    release::{GhReleaseSource, MANIFEST_NAME, ReleaseClient, ReleaseInput, Verifier},
    state::{
        ArtifactIdentity, LocalStateStore, StateError, TARGET_STATE_PATH, TargetInstallState,
        TargetStateStore,
    },
    target::{SshTarget, TargetIdentity},
    transport::{
        OpenSshTransport, ReconnectPolicy, RemoteCommand, SystemCommandRunner, Transport,
        TransportConfig, TransportError,
    },
};
use semver::Version;
use serde::Deserialize;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("planeradarctl: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    if let Command::Driver { command } = cli.command.clone() {
        return run_driver(command);
    }
    let environment = Environment::from_dotenv_path(Path::new(".env"))?;
    if cli.command.is_mutating() {
        let is_install = matches!(cli.command, Command::Install(_));
        let config = InstallConfig::resolve(cli, environment)?;
        if is_install {
            return run_install(config);
        }
    }
    Ok(())
}

fn run_install(config: InstallConfig) -> Result<(), Box<dyn std::error::Error>> {
    let target_text = config
        .target
        .as_deref()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "target is required"))?;
    let target = target_text.parse::<SshTarget>()?;
    let lock = DriverLock::checked_in()?;
    let home = env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "an absolute home directory is required",
            )
        })?;
    let cache_root = home.join(".cache").join("planeradar");
    ensure_private_cache_root(&cache_root)?;
    let version = requested_version(&config)?;
    let release_input = config
        .release_dir
        .as_deref()
        .map_or(ReleaseInput::Downloaded, ReleaseInput::Local);
    let release = ReleaseClient::new(GhReleaseSource::system(), cache_root.join("release"))
        .resolve(&version, &lock, release_input)?;
    Verifier::new(SystemCommandRunner).verify(&version, &release)?;
    let application_artifact = release
        .artifacts
        .iter()
        .find(|artifact| artifact.artifact.name == "planeradar-aarch64-linux-gnu.tar.zst")
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "release has no supported application artifact",
            )
        })?;
    let application_payload =
        extract_application_payload(&application_artifact.path, &cache_root.join("payloads"))?;
    let payload_sha256 = application_payload.sha256().to_owned();
    let source_commit = release.manifest.source_commit.clone();
    let release_version = release.manifest.version.clone();

    let transport =
        OpenSshTransport::system(TransportConfig::new(home.join(".ssh").join("known_hosts"))?);
    let observed = transport.probe(&target)?.identity;
    let state_store = LocalStateStore::from_environment(&home, observed.clone())?;
    let backend = SystemInstallBackend {
        transport,
        target: RefCell::new(target),
        expected_identity: observed.clone(),
        repository: env::current_dir()?,
        docker_context: config.docker_context,
        lock: lock.clone(),
        cache_root,
        application_payload,
        driver_tool: RefCell::new(None),
        persisted_target_phase: RefCell::new(None),
        helper_path: format!("/var/tmp/planeradar-installer-{}", &payload_sha256[..12]),
    };
    let request = InstallRequest {
        target: observed,
        application: ArtifactIdentity {
            version: release_version.to_string(),
            source_commit,
            sha256: payload_sha256,
        },
        driver: ArtifactIdentity {
            version: lock.version.to_string(),
            source_commit: lock.commit.clone(),
            sha256: lock.manifest_sha256.clone(),
        },
        desired_hostname: config.hostname,
    };

    match Installer::new(&backend, &state_store).run(request)? {
        InstallOutcome::Complete => {
            println!("Installation complete.");
            Ok(())
        }
        InstallOutcome::AlreadyComplete => {
            println!("Installation is already complete.");
            Ok(())
        }
        InstallOutcome::Interrupted {
            phase,
            reason,
            guidance,
        } => {
            if let Some(guidance) = guidance {
                eprintln!("{guidance}");
            }
            Err(io::Error::other(format!(
                "installation interrupted after {phase:?}: {reason:?}"
            ))
            .into())
        }
    }
}

fn requested_version(config: &InstallConfig) -> Result<Version, Box<dyn std::error::Error>> {
    if let Some(version) = &config.version {
        return Ok(version.clone());
    }
    let release_directory = config.release_dir.as_deref().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "an exact --version or verified --release-dir is required",
        )
    })?;
    let manifest_bytes = fs::read(release_directory.join(MANIFEST_NAME))?;
    let manifest_value: serde_json::Value = serde_json::from_slice(&manifest_bytes)?;
    let version_text = manifest_value
        .get("version")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "release version is missing"))?;
    Ok(Version::parse(version_text)?)
}

fn ensure_private_cache_root(path: &Path) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("cache root has no parent"))?;
    if !parent.exists() {
        let mut builder = fs::DirBuilder::new();
        builder.mode(0o700);
        builder.create(parent)?;
    }
    let parent_metadata = fs::symlink_metadata(parent)?;
    if !parent_metadata.file_type().is_dir() || parent_metadata.file_type().is_symlink() {
        return Err(io::Error::other("cache parent is not a safe directory"));
    }
    match fs::symlink_metadata(path) {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let mut builder = fs::DirBuilder::new();
            builder.mode(0o700);
            builder.create(path)?;
        }
        Err(error) => return Err(error),
    }
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_dir()
        || metadata.file_type().is_symlink()
        || metadata.permissions().mode() & 0o777 != 0o700
    {
        return Err(io::Error::other("cache root is not a private directory"));
    }
    Ok(())
}

fn tryboot_reboot_command() -> Result<RemoteCommand, TransportError> {
    RemoteCommand::interactive_sudo(["sudo", "reboot", "0 tryboot"])
}

fn final_reboot_command() -> Result<RemoteCommand, TransportError> {
    RemoteCommand::interactive_sudo(["sudo", "systemctl", "reboot"])
}

fn hostname_command(hostname: &str) -> Result<RemoteCommand, TransportError> {
    RemoteCommand::interactive_sudo(["sudo", "hostnamectl", "set-hostname", hostname])
}

fn target_install_command(
    helper_path: &str,
    checksum_path: &str,
    revision_path: &str,
) -> Result<RemoteCommand, TransportError> {
    RemoteCommand::interactive_sudo([
        "sudo",
        helper_path,
        "install",
        "--artifact",
        helper_path,
        "--checksum-file",
        checksum_path,
        "--revision-file",
        revision_path,
        "--json",
    ])
}

struct SystemInstallBackend {
    transport: OpenSshTransport<SystemCommandRunner>,
    target: RefCell<SshTarget>,
    expected_identity: TargetIdentity,
    repository: PathBuf,
    docker_context: Option<String>,
    lock: DriverLock,
    cache_root: PathBuf,
    application_payload: ApplicationPayload,
    driver_tool: RefCell<Option<DriverTool<SystemCommandRunner>>>,
    persisted_target_phase: RefCell<Option<planeradarctl::state::InstallPhase>>,
    helper_path: String,
}

impl SystemInstallBackend {
    fn current_target(&self) -> SshTarget {
        self.target.borrow().clone()
    }

    fn reconnect_policy(
        &self,
        desired_hostname: Option<&str>,
    ) -> Result<ReconnectPolicy, BackendFailure> {
        let policy = ReconnectPolicy::new(
            Duration::from_secs(30),
            Duration::from_secs(300),
            Duration::from_secs(1),
            Duration::from_secs(10),
            Duration::from_secs(10),
        )
        .map_err(|_| BackendFailure::OperationFailed)?;
        match desired_hostname {
            Some(hostname) => policy
                .with_desired_local_hostname(format!("{hostname}.local"))
                .map_err(|_| BackendFailure::OperationFailed),
            None => Ok(policy),
        }
    }

    fn ensure_driver_tool(&self) -> Result<(), BackendFailure> {
        if self.driver_tool.borrow().is_some() {
            return Ok(());
        }
        let target = self.current_target();
        let facts = TargetPreflight::new(&self.transport, SystemUnixClock)
            .facts(&target)
            .map_err(|_| BackendFailure::OperationFailed)?;
        let probe = DriverTargetProbe::new(facts.kernel_release.clone(), facts.kernel_vermagic)
            .map_err(|_| BackendFailure::OperationFailed)?;
        let manager = DriverManager::new(
            GhDriverReleaseSource::system(),
            GhDriverReleaseVerifier::system(),
            self.cache_root.join("driver"),
        );
        let synced = manager
            .sync(&self.lock)
            .map_err(|_| BackendFailure::OperationFailed)?;
        let tool = synced
            .tool(
                SystemCommandRunner,
                &probe,
                DriverContext {
                    target: target.ssh_destination(),
                    kernel_release: facts.kernel_release.clone(),
                    kernel_export: self
                        .cache_root
                        .join("kernel-export")
                        .join(&facts.kernel_release),
                    artifacts: self.cache_root.join("driver-artifacts"),
                    replace_overlay: "vc4-kms-dpi-hyperpixel2r".into(),
                },
            )
            .map_err(|_| BackendFailure::OperationFailed)?;
        *self.driver_tool.borrow_mut() = Some(tool);
        Ok(())
    }

    fn run_remote_check(&self, request: RemoteCommand) -> Result<bool, BackendFailure> {
        match self.transport.run(&self.current_target(), request) {
            Ok(_) => Ok(true),
            Err(TransportError::CommandFailed) => Ok(false),
            Err(
                TransportError::ConnectionUnavailable
                | TransportError::ProbeFailed
                | TransportError::Runner(_),
            ) => Err(BackendFailure::SshLost),
            Err(_) => Err(BackendFailure::OperationFailed),
        }
    }

    fn verify_remote_helper(&self, expected_sha256: &str) -> Result<bool, BackendFailure> {
        let request = RemoteCommand::ordinary([
            "sh",
            "-c",
            "test ! -L \"$1\" && test -f \"$1\" && test -x \"$1\" && test \"$(sha256sum -- \"$1\" | awk '{print $1}')\" = \"$2\"",
            "planeradar-helper",
            &self.helper_path,
            expected_sha256,
        ])
        .map_err(|_| BackendFailure::OperationFailed)?;
        self.run_remote_check(request)
    }

    fn verify_installed_application(
        &self,
        request: &InstallRequest,
    ) -> Result<bool, BackendFailure> {
        let remote = RemoteCommand::ordinary([
            "sh",
            "-c",
            "test ! -L /opt/planeradar/bin/planeradar && test -x /opt/planeradar/bin/planeradar && test \"$(sha256sum -- /opt/planeradar/bin/planeradar | awk '{print $1}')\" = \"$1\" && test \"$(cat /opt/planeradar/REVISION)\" = \"$2\"",
            "planeradar-installed",
            &request.application.sha256,
            &request.application.source_commit,
        ])
        .map_err(|_| BackendFailure::OperationFailed)?;
        self.run_remote_check(remote)
    }

    fn verify_service_health(&self) -> Result<bool, BackendFailure> {
        let request = RemoteCommand::ordinary([
            "sh",
            "-c",
            "systemctl is-enabled --quiet planeradar.service && systemctl is-active --quiet planeradar.service && /opt/planeradar/bin/planeradar probe >/dev/null",
        ])
        .map_err(|_| BackendFailure::FinalServiceFailed)?;
        self.run_remote_check(request)
    }

    fn desired_target(&self, hostname: &str) -> Result<SshTarget, BackendFailure> {
        format!(
            "{}@{hostname}.local",
            self.current_target().username().as_str()
        )
        .parse()
        .map_err(|_| BackendFailure::OperationFailed)
    }

    fn update_reconnected_target(
        &self,
        target: SshTarget,
        expected_identity: &TargetIdentity,
    ) -> Result<(), BackendFailure> {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if let Ok(observed) = self.transport.probe(&target) {
                if !expected_identity.matches(&observed.identity) {
                    return Err(BackendFailure::OperationFailed);
                }
                *self.target.borrow_mut() = target;
                *self.driver_tool.borrow_mut() = None;
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(BackendFailure::SshLost);
            }
            thread::sleep(Duration::from_secs(1));
        }
    }
}

impl TargetStateStore for SystemInstallBackend {
    fn load_target_state(&self) -> Result<Option<TargetInstallState>, StateError> {
        let target = self.target.borrow().clone();
        let exists = RemoteCommand::ordinary(["test", "-x", &self.helper_path])
            .map_err(target_state_transport_error)?;
        if self.transport.run(&target, exists).is_err() {
            return Ok(None);
        }
        let request =
            RemoteCommand::interactive_sudo(["sudo", &self.helper_path, "installer-state", "read"])
                .map_err(target_state_transport_error)?;
        let output = self
            .transport
            .run(&target, request)
            .map_err(target_state_transport_error)?;
        if output.stdout().len() > 64 * 1024 {
            return Err(target_state_transport_error(()));
        }
        let mut deserializer = serde_json::Deserializer::from_slice(output.stdout());
        let state = Option::<TargetInstallState>::deserialize(&mut deserializer)
            .map_err(target_state_transport_error)?;
        deserializer.end().map_err(target_state_transport_error)?;
        *self.persisted_target_phase.borrow_mut() =
            state.as_ref().map(|state| state.last_verified_phase);
        Ok(state)
    }

    fn save_target_state(&self, state: &TargetInstallState) -> Result<(), StateError> {
        let target = self.target.borrow().clone();
        let json = state.to_json()?;
        let request = RemoteCommand::interactive_sudo([
            "sudo",
            &self.helper_path,
            "installer-state",
            "write",
            "--json",
            &json,
        ])
        .map_err(target_state_transport_error)?;
        let output = self
            .transport
            .run(&target, request)
            .map_err(target_state_transport_error)?;
        let returned = TargetInstallState::from_json(
            std::str::from_utf8(output.stdout()).map_err(target_state_transport_error)?,
        )?;
        if &returned != state {
            return Err(target_state_transport_error(()));
        }
        *self.persisted_target_phase.borrow_mut() = Some(state.last_verified_phase);
        Ok(())
    }
}

impl InstallBackend for SystemInstallBackend {
    fn discover(&self, _request: &InstallRequest) -> Result<TargetIdentity, BackendFailure> {
        let observed = self
            .transport
            .probe(&self.current_target())
            .map_err(|_| BackendFailure::SshLost)?;
        if !self.expected_identity.matches(&observed.identity) {
            return Err(BackendFailure::OperationFailed);
        }
        Ok(observed.identity)
    }

    fn run_preflight(&self, _request: &InstallRequest) -> Result<(), BackendFailure> {
        HostPreflight::new(&SystemCommandRunner)
            .run(&self.repository, self.docker_context.as_deref())
            .require_success()
            .map_err(|_| BackendFailure::OperationFailed)?;
        TargetPreflight::new(&self.transport, SystemUnixClock)
            .run(&self.current_target(), &self.expected_identity)
            .require_success()
            .map_err(|_| BackendFailure::OperationFailed)?;
        Ok(())
    }

    fn acquire_application(
        &self,
        request: &InstallRequest,
    ) -> Result<ArtifactIdentity, BackendFailure> {
        if self.application_payload.sha256() != request.application.sha256 {
            return Err(BackendFailure::OperationFailed);
        }
        self.transport
            .copy_to(
                &self.current_target(),
                self.application_payload.path(),
                Path::new(&self.helper_path),
            )
            .map_err(|_| BackendFailure::SshLost)?;
        let chmod = RemoteCommand::ordinary(["chmod", "700", &self.helper_path])
            .map_err(|_| BackendFailure::OperationFailed)?;
        self.transport
            .run(&self.current_target(), chmod)
            .map_err(|_| BackendFailure::OperationFailed)?;
        if !self.verify_remote_helper(&request.application.sha256)? {
            return Err(BackendFailure::OperationFailed);
        }
        Ok(request.application.clone())
    }

    fn prepare_driver(&self, request: &InstallRequest) -> Result<ArtifactIdentity, BackendFailure> {
        self.ensure_driver_tool()?;
        Ok(request.driver.clone())
    }

    fn stage_tryboot(&self, _request: &InstallRequest) -> Result<(), BackendFailure> {
        self.ensure_driver_tool()?;
        self.driver_tool
            .borrow()
            .as_ref()
            .ok_or(BackendFailure::OperationFailed)?
            .prepare_and_stage()
            .map_err(|_| BackendFailure::OperationFailed)
    }

    fn boot_and_verify_tryboot(&self, _request: &InstallRequest) -> Result<(), BackendFailure> {
        let original = self.current_target();
        let reboot = tryboot_reboot_command().map_err(|_| BackendFailure::OperationFailed)?;
        let _expected_disconnect = self.transport.run(&original, reboot);
        let reconnected = self
            .transport
            .wait_for_reboot(
                &self.expected_identity,
                std::slice::from_ref(&original),
                self.reconnect_policy(None)?,
            )
            .map_err(|error| match error {
                TransportError::ReconnectTimedOut | TransportError::NeverDisconnected => {
                    BackendFailure::TrybootTimedOut
                }
                _ => BackendFailure::OperationFailed,
            })?;
        *self.target.borrow_mut() = reconnected;
        self.ensure_driver_tool()?;
        self.driver_tool
            .borrow()
            .as_ref()
            .ok_or(BackendFailure::OperationFailed)?
            .run(DriverAction::VerifyBoot)
            .map_err(|_| BackendFailure::TrybootVerificationFailed)?;
        Ok(())
    }

    fn accept_driver(&self, _request: &InstallRequest) -> Result<(), BackendFailure> {
        self.ensure_driver_tool()?;
        self.driver_tool
            .borrow()
            .as_ref()
            .ok_or(BackendFailure::OperationFailed)?
            .run(DriverAction::CommitBoot)
            .map(|_| ())
            .map_err(|_| BackendFailure::OperationFailed)
    }

    fn install_application(
        &self,
        request: &InstallRequest,
    ) -> Result<TargetInstallResult, BackendFailure> {
        if !self.verify_remote_helper(&request.application.sha256)? {
            return Err(BackendFailure::OperationFailed);
        }
        let sidecars = RemoteCommand::ordinary([
            "sh",
            "-c",
            "umask 077; printf '%s  planeradar\\n' \"$2\" >\"$1.sha256\" && printf '%s\\n' \"$3\" >\"$1.revision\"",
            "planeradar-sidecars",
            &self.helper_path,
            &request.application.sha256,
            &request.application.source_commit,
        ])
        .map_err(|_| BackendFailure::OperationFailed)?;
        self.transport
            .run(&self.current_target(), sidecars)
            .map_err(|_| BackendFailure::SshLost)?;
        let checksum_path = format!("{}.sha256", self.helper_path);
        let revision_path = format!("{}.revision", self.helper_path);
        let install = target_install_command(&self.helper_path, &checksum_path, &revision_path)
            .map_err(|_| BackendFailure::OperationFailed)?;
        let output = self
            .transport
            .run(&self.current_target(), install)
            .map_err(|_| BackendFailure::SshLost)?;
        TargetInstallResult::from_json(output.stdout()).map_err(|_| BackendFailure::OperationFailed)
    }

    fn change_hostname_and_reconnect(
        &self,
        expected_identity: &TargetIdentity,
        desired_hostname: &str,
    ) -> Result<(), BackendFailure> {
        let command =
            hostname_command(desired_hostname).map_err(|_| BackendFailure::OperationFailed)?;
        self.transport
            .run(&self.current_target(), command)
            .map_err(|_| BackendFailure::SshLost)?;
        let desired = self.desired_target(desired_hostname)?;
        self.update_reconnected_target(desired, expected_identity)
    }

    fn reboot_final(&self, request: &InstallRequest) -> Result<(), BackendFailure> {
        let original = self.current_target();
        let reboot = final_reboot_command().map_err(|_| BackendFailure::OperationFailed)?;
        let _expected_disconnect = self.transport.run(&original, reboot);
        let reconnected = self
            .transport
            .wait_for_reboot(
                &request.target,
                std::slice::from_ref(&original),
                self.reconnect_policy(Some(&request.desired_hostname))?,
            )
            .map_err(|_| BackendFailure::SshLost)?;
        *self.target.borrow_mut() = reconnected;
        Ok(())
    }

    fn verify_final_service(&self, _request: &InstallRequest) -> Result<(), BackendFailure> {
        self.ensure_driver_tool()?;
        self.driver_tool
            .borrow()
            .as_ref()
            .ok_or(BackendFailure::OperationFailed)?
            .verify_normal_boot()
            .map_err(|_| BackendFailure::FinalServiceFailed)?;
        if self.verify_service_health()? {
            Ok(())
        } else {
            Err(BackendFailure::FinalServiceFailed)
        }
    }

    fn finish(&self, _request: &InstallRequest) -> Result<(), BackendFailure> {
        Ok(())
    }

    fn verify_phase(
        &self,
        phase: planeradarctl::state::InstallPhase,
        request: &InstallRequest,
        _state: &planeradarctl::state::InstallState,
    ) -> Result<PhaseVerification, BackendFailure> {
        use planeradarctl::state::InstallPhase;
        let verification = match phase {
            InstallPhase::Discovered => self
                .transport
                .probe(&self.current_target())
                .map(|observed| self.expected_identity.matches(&observed.identity))
                .map_err(|_| BackendFailure::SshLost),
            InstallPhase::PreflightPassed
                if self
                    .persisted_target_phase
                    .borrow()
                    .is_some_and(|phase| phase >= InstallPhase::TrybootStaged) =>
            {
                let observed = self
                    .transport
                    .probe(&self.current_target())
                    .map_err(|_| BackendFailure::SshLost)?;
                if !request.target.matches(&observed.identity) {
                    Ok(false)
                } else {
                    TargetPreflight::new(&self.transport, SystemUnixClock)
                        .facts(&self.current_target())
                        .map(|_| true)
                        .or(Ok(false))
                }
            }
            InstallPhase::PreflightPassed => Ok(self.run_preflight(request).is_ok()),
            InstallPhase::ApplicationAcquired => {
                self.verify_remote_helper(&request.application.sha256)
            }
            InstallPhase::DriverReady => self.ensure_driver_tool().map(|()| true),
            InstallPhase::TrybootStaged => {
                let persisted = *self.persisted_target_phase.borrow();
                if persisted.is_some_and(|phase| phase >= InstallPhase::FinalRebooted) {
                    self.ensure_driver_tool()?;
                    self.driver_tool
                        .borrow()
                        .as_ref()
                        .ok_or(BackendFailure::OperationFailed)?
                        .verify_normal_boot()
                        .map(|_| true)
                        .or(Ok(false))
                } else if persisted.is_some_and(|phase| phase >= InstallPhase::TrybootVerified) {
                    self.ensure_driver_tool()?;
                    self.driver_tool
                        .borrow()
                        .as_ref()
                        .ok_or(BackendFailure::OperationFailed)?
                        .run(DriverAction::VerifyBoot)
                        .map(|_| true)
                        .or(Ok(false))
                } else {
                    let command = RemoteCommand::interactive_sudo([
                        "sudo",
                        "test",
                        "-f",
                        "/var/lib/hyperpixel2r-kms/tryboot-state",
                    ])
                    .map_err(|_| BackendFailure::OperationFailed)?;
                    self.run_remote_check(command)
                }
            }
            InstallPhase::TrybootVerified | InstallPhase::DriverAccepted => {
                self.ensure_driver_tool()?;
                if self
                    .persisted_target_phase
                    .borrow()
                    .is_some_and(|phase| phase >= InstallPhase::FinalRebooted)
                {
                    self.driver_tool
                        .borrow()
                        .as_ref()
                        .ok_or(BackendFailure::OperationFailed)?
                        .verify_normal_boot()
                        .map(|_| true)
                        .or(Ok(false))
                } else {
                    self.driver_tool
                        .borrow()
                        .as_ref()
                        .ok_or(BackendFailure::OperationFailed)?
                        .run(DriverAction::VerifyBoot)
                        .map(|_| true)
                        .or(Ok(false))
                }
            }
            InstallPhase::ApplicationInstalled => self.verify_installed_application(request),
            InstallPhase::HostnameChanged => {
                let desired = self.desired_target(&request.desired_hostname)?;
                self.update_reconnected_target(desired, &request.target)
                    .map(|()| true)
            }
            InstallPhase::FinalRebooted => {
                self.ensure_driver_tool()?;
                self.driver_tool
                    .borrow()
                    .as_ref()
                    .ok_or(BackendFailure::OperationFailed)?
                    .verify_normal_boot()
                    .map(|_| true)
                    .or(Ok(false))
            }
            InstallPhase::FinalVerified | InstallPhase::Complete => self.verify_service_health(),
        };
        Ok(if verification? {
            PhaseVerification::Valid
        } else {
            PhaseVerification::Drifted
        })
    }

    fn emit_status(&self, event: InstallStatusEvent) -> Result<(), BackendFailure> {
        println!("{}", event.message);
        Ok(())
    }
}

fn target_state_transport_error<E>(_error: E) -> StateError {
    StateError::Io {
        path: PathBuf::from(TARGET_STATE_PATH),
        source: io::Error::other("target installer state operation failed"),
    }
}

fn run_driver(command: DriverCommand) -> Result<(), Box<dyn std::error::Error>> {
    let repository = std::env::current_dir()?;
    let cache = repository.join(".cache/driver");
    let manager = DriverManager::new(
        GhDriverReleaseSource::system(),
        GhDriverReleaseVerifier::system(),
        cache,
    );
    match command {
        DriverCommand::Sync => {
            let lock =
                DriverLock::parse(&fs::read_to_string(repository.join("driver.lock.toml"))?)?;
            let synced = manager.sync(&lock)?;
            println!("Synced locked HyperPixel driver {}", synced.lock().version);
        }
        DriverCommand::Update { version } => {
            let version = Version::parse(&version)?;
            let lock = manager.update(&repository.join("driver.lock.toml"), &version)?;
            println!("Updated HyperPixel driver lock to {}", lock.version);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        final_reboot_command, hostname_command, target_install_command, tryboot_reboot_command,
    };

    #[test]
    fn production_adapter_uses_exact_typed_reboot_and_hostname_commands() {
        let tryboot = tryboot_reboot_command().expect("tryboot command");
        assert!(tryboot.is_interactive_sudo());
        assert_eq!(tryboot.arguments(), ["sudo", "reboot", "0 tryboot"]);

        let hostname = hostname_command("planeradar").expect("hostname command");
        assert!(hostname.is_interactive_sudo());
        assert_eq!(
            hostname.arguments(),
            ["sudo", "hostnamectl", "set-hostname", "planeradar"]
        );

        let final_reboot = final_reboot_command().expect("final reboot command");
        assert!(final_reboot.is_interactive_sudo());
        assert_eq!(final_reboot.arguments(), ["sudo", "systemctl", "reboot"]);
    }

    #[test]
    fn production_adapter_invokes_the_versioned_helper_with_exact_machine_arguments() {
        let helper = "/var/tmp/planeradar-installer-0123456789ab";
        let command = target_install_command(
            helper,
            "/var/tmp/planeradar-installer-0123456789ab.sha256",
            "/var/tmp/planeradar-installer-0123456789ab.revision",
        )
        .expect("target install command");

        assert!(command.is_interactive_sudo());
        assert_eq!(
            command.arguments(),
            [
                "sudo",
                helper,
                "install",
                "--artifact",
                helper,
                "--checksum-file",
                "/var/tmp/planeradar-installer-0123456789ab.sha256",
                "--revision-file",
                "/var/tmp/planeradar-installer-0123456789ab.revision",
                "--json",
            ]
        );
    }
}

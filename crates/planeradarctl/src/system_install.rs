use std::cell::RefCell;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::Deserialize;

use crate::DriverLock;
use crate::driver::{
    DriverAction, DriverContext, DriverManager, DriverPostconditions, DriverTool,
    GhDriverReleaseSource, GhDriverReleaseVerifier, TargetProbe as DriverTargetProbe,
};
use crate::install::{
    ApplicationPayload, BackendFailure, InstallBackend, InstallRequest, InstallStatusEvent,
    PhaseVerification, TargetApplicationInstall, TargetInstallOwnership, TargetInstallResult,
};
use crate::preflight::{HostPreflight, SystemUnixClock, TargetPreflight};
use crate::state::{
    InstallPhase, InstallState, StateError, TARGET_STATE_PATH, TargetInstallState, TargetStateStore,
};
use crate::target::{SshTarget, TargetIdentity};
use crate::transport::{
    ReconnectPolicy, RemoteCommand, SystemCommandRunner, Transport, TransportError,
};

pub trait HostPreflightGate {
    fn verify(&self, repository: &Path, docker_context: Option<&str>)
    -> Result<(), BackendFailure>;
}

#[derive(Clone, Copy, Default)]
pub struct SystemHostPreflight;

impl HostPreflightGate for SystemHostPreflight {
    fn verify(
        &self,
        repository: &Path,
        docker_context: Option<&str>,
    ) -> Result<(), BackendFailure> {
        HostPreflight::new(&SystemCommandRunner)
            .run(repository, docker_context)
            .require_success()
            .map(|_| ())
            .map_err(|_| BackendFailure::OperationFailed)
    }
}

pub trait InstallClock {
    fn now(&self) -> Duration;
    fn sleep(&self, duration: Duration);
}

pub struct SystemInstallClock {
    started_at: Instant,
}

impl Default for SystemInstallClock {
    fn default() -> Self {
        Self {
            started_at: Instant::now(),
        }
    }
}

impl InstallClock for SystemInstallClock {
    fn now(&self) -> Duration {
        self.started_at.elapsed()
    }

    fn sleep(&self, duration: Duration) {
        std::thread::sleep(duration);
    }
}

pub trait DriverActions<T: Transport> {
    fn ensure_ready(&self, transport: &T, target: &SshTarget) -> Result<(), BackendFailure>;
    fn accepted_identity(&self) -> Result<AcceptedDriverIdentity, BackendFailure>;
    fn postconditions(&self) -> Result<DriverPostconditions, BackendFailure>;
    fn stage(&self, transport: &T, target: &SshTarget) -> Result<(), BackendFailure>;
    fn verify_tryboot(&self, transport: &T, target: &SshTarget) -> Result<bool, BackendFailure>;
    fn accept(&self, transport: &T, target: &SshTarget) -> Result<(), BackendFailure>;
    fn verify_normal_boot(&self, transport: &T, target: &SshTarget)
    -> Result<bool, BackendFailure>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AcceptedDriverIdentity {
    pub driver_version: String,
    pub source_revision: String,
    pub kernel_release: String,
    pub overlay_file: String,
}

pub struct SystemDriverActions {
    lock: DriverLock,
    cache_root: PathBuf,
    tool: RefCell<Option<(SshTarget, DriverTool<SystemCommandRunner>)>>,
}

impl SystemDriverActions {
    pub fn new(lock: DriverLock, cache_root: PathBuf) -> Self {
        Self {
            lock,
            cache_root,
            tool: RefCell::new(None),
        }
    }

    fn with_tool<R>(
        &self,
        operation: impl FnOnce(&DriverTool<SystemCommandRunner>) -> Result<R, BackendFailure>,
    ) -> Result<R, BackendFailure> {
        let tool = self.tool.borrow();
        operation(&tool.as_ref().ok_or(BackendFailure::OperationFailed)?.1)
    }
}

impl<T: Transport> DriverActions<T> for SystemDriverActions {
    fn ensure_ready(&self, transport: &T, target: &SshTarget) -> Result<(), BackendFailure> {
        if self
            .tool
            .borrow()
            .as_ref()
            .is_some_and(|(cached_target, _)| cached_target == target)
        {
            return Ok(());
        }
        *self.tool.borrow_mut() = None;
        let facts = TargetPreflight::new(transport, SystemUnixClock)
            .facts(target)
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
                    replace_overlay: facts.replace_overlay.clone(),
                },
            )
            .map_err(|_| BackendFailure::OperationFailed)?;
        *self.tool.borrow_mut() = Some((target.clone(), tool));
        Ok(())
    }

    fn stage(&self, transport: &T, target: &SshTarget) -> Result<(), BackendFailure> {
        self.ensure_ready(transport, target)?;
        self.with_tool(|tool| {
            tool.prepare_and_stage()
                .map_err(|_| BackendFailure::OperationFailed)
        })
    }

    fn postconditions(&self) -> Result<DriverPostconditions, BackendFailure> {
        self.with_tool(|tool| {
            tool.postconditions()
                .map_err(|_| BackendFailure::OperationFailed)
        })
    }

    fn accepted_identity(&self) -> Result<AcceptedDriverIdentity, BackendFailure> {
        self.with_tool(|tool| {
            Ok(AcceptedDriverIdentity {
                driver_version: tool.driver_version().to_owned(),
                source_revision: tool.source_revision().to_owned(),
                kernel_release: tool.kernel_release().to_owned(),
                overlay_file: tool.expected_overlay_file().to_owned(),
            })
        })
    }

    fn verify_tryboot(&self, transport: &T, target: &SshTarget) -> Result<bool, BackendFailure> {
        self.ensure_ready(transport, target)?;
        self.with_tool(|tool| {
            tool.run(DriverAction::VerifyBoot)
                .map(|_| true)
                .or(Ok(false))
        })
    }

    fn accept(&self, transport: &T, target: &SshTarget) -> Result<(), BackendFailure> {
        self.ensure_ready(transport, target)?;
        self.with_tool(|tool| {
            tool.run(DriverAction::CommitBoot)
                .map(|_| ())
                .map_err(|_| BackendFailure::OperationFailed)
        })
    }

    fn verify_normal_boot(
        &self,
        transport: &T,
        target: &SshTarget,
    ) -> Result<bool, BackendFailure> {
        self.ensure_ready(transport, target)?;
        self.with_tool(|tool| tool.verify_normal_boot().map(|_| true).or(Ok(false)))
    }
}

pub struct SystemInstallBackend<T, H, D, C> {
    transport: T,
    target: RefCell<SshTarget>,
    expected_identity: TargetIdentity,
    repository: PathBuf,
    docker_context: Option<String>,
    application_payload: ApplicationPayload,
    persisted_target_phase: RefCell<Option<InstallPhase>>,
    helper_path: String,
    host_preflight: H,
    driver: D,
    clock: C,
}

impl<T, H, D, C> SystemInstallBackend<T, H, D, C>
where
    T: Transport,
    H: HostPreflightGate,
    D: DriverActions<T>,
    C: InstallClock,
{
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        transport: T,
        target: SshTarget,
        expected_identity: TargetIdentity,
        repository: PathBuf,
        docker_context: Option<String>,
        application_payload: ApplicationPayload,
        host_preflight: H,
        driver: D,
        clock: C,
    ) -> Self {
        let helper_path = format!(
            "/var/lib/planeradar-installer/helpers/{}/planeradar",
            application_payload.sha256()
        );
        Self {
            transport,
            target: RefCell::new(target),
            expected_identity,
            repository,
            docker_context,
            application_payload,
            persisted_target_phase: RefCell::new(None),
            helper_path,
            host_preflight,
            driver,
            clock,
        }
    }

    pub fn transport(&self) -> &T {
        &self.transport
    }

    pub fn current_target(&self) -> SshTarget {
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
        let request = RemoteCommand::interactive_sudo([
            "sudo",
            "sh",
            "-c",
            "test ! -L \"$1\" && test -f \"$1\" && test -x \"$1\" && test \"$(stat -c '%u:%g:%a' -- \"$1\")\" = '0:0:700' && test \"$(sha256sum -- \"$1\" | awk '{print $1}')\" = \"$2\"",
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

    fn verify_staged_driver_transaction(&self) -> Result<bool, BackendFailure> {
        self.driver
            .ensure_ready(&self.transport, &self.current_target())?;
        let expected = self.driver.postconditions()?;
        let command = staged_driver_transaction_command(&expected)
            .map_err(|_| BackendFailure::OperationFailed)?;
        self.run_remote_check(command)
    }

    fn verify_committed_driver(&self) -> Result<bool, BackendFailure> {
        self.driver
            .ensure_ready(&self.transport, &self.current_target())?;
        let expected = self.driver.postconditions()?;
        let command =
            committed_driver_command(&expected).map_err(|_| BackendFailure::OperationFailed)?;
        self.run_remote_check(command)
    }

    fn verify_reusable_accepted_driver(&self) -> Result<bool, BackendFailure> {
        self.driver
            .ensure_ready(&self.transport, &self.current_target())?;
        let expected = self.driver.accepted_identity()?;
        let receipt = accepted_driver_receipt_command(&expected)
            .map_err(|_| BackendFailure::OperationFailed)?;
        let reusable = self.run_remote_check(receipt)?
            && self
                .driver
                .verify_normal_boot(&self.transport, &self.current_target())?;
        Ok(reusable)
    }

    fn verify_accepted_driver(&self, normal_boot: bool) -> Result<bool, BackendFailure> {
        if !self.verify_committed_driver()? {
            return Ok(false);
        }
        if normal_boot {
            self.driver
                .verify_normal_boot(&self.transport, &self.current_target())
        } else {
            self.driver
                .verify_tryboot(&self.transport, &self.current_target())
        }
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
        let deadline = self.clock.now() + Duration::from_secs(30);
        loop {
            if let Ok(observed) = self.transport.probe(&target) {
                if !expected_identity.matches(&observed.identity) {
                    return Err(BackendFailure::OperationFailed);
                }
                *self.target.borrow_mut() = target;
                return Ok(());
            }
            if self.clock.now() >= deadline {
                return Err(BackendFailure::SshLost);
            }
            self.clock.sleep(Duration::from_secs(1));
        }
    }
}

impl<T, H, D, C> TargetStateStore for SystemInstallBackend<T, H, D, C>
where
    T: Transport,
    H: HostPreflightGate,
    D: DriverActions<T>,
    C: InstallClock,
{
    fn load_target_state(&self) -> Result<Option<TargetInstallState>, StateError> {
        let target = self.current_target();
        let exists = RemoteCommand::interactive_sudo(["sudo", "test", "-x", &self.helper_path])
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
            .run(&self.current_target(), request)
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

impl<T, H, D, C> InstallBackend for SystemInstallBackend<T, H, D, C>
where
    T: Transport,
    H: HostPreflightGate,
    D: DriverActions<T>,
    C: InstallClock,
{
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
        self.host_preflight
            .verify(&self.repository, self.docker_context.as_deref())?;
        TargetPreflight::new(&self.transport, SystemUnixClock)
            .run(&self.current_target(), &self.expected_identity)
            .require_success()
            .map(|_| ())
            .map_err(|_| BackendFailure::OperationFailed)
    }

    fn acquire_application(
        &self,
        request: &InstallRequest,
    ) -> Result<crate::state::ArtifactIdentity, BackendFailure> {
        if self.application_payload.sha256() != request.application.sha256 {
            return Err(BackendFailure::OperationFailed);
        }
        let create_upload = RemoteCommand::ordinary([
            "sh",
            "-c",
            "umask 077; mktemp -d /var/tmp/planeradar-upload.XXXXXXXXXX",
        ])
        .map_err(|_| BackendFailure::OperationFailed)?;
        let output = self
            .transport
            .run(&self.current_target(), create_upload)
            .map_err(|_| BackendFailure::SshLost)?;
        let upload_directory = std::str::from_utf8(output.stdout())
            .ok()
            .map(str::trim)
            .filter(|path| {
                path.strip_prefix("/var/tmp/planeradar-upload.")
                    .is_some_and(|suffix| {
                        suffix.len() == 10
                            && suffix.bytes().all(|byte| byte.is_ascii_alphanumeric())
                    })
            })
            .ok_or(BackendFailure::OperationFailed)?
            .to_owned();
        let upload_path = format!("{upload_directory}/payload");
        self.transport
            .copy_to(
                &self.current_target(),
                self.application_payload.path(),
                Path::new(&upload_path),
            )
            .map_err(|_| BackendFailure::SshLost)?;
        let deploy = deploy_helper_command(
            &upload_path,
            &self.helper_path,
            &request.application.sha256,
            &request.application.source_commit,
        )
        .map_err(|_| BackendFailure::OperationFailed)?;
        let deployed = self
            .transport
            .run(&self.current_target(), deploy)
            .map_err(|_| BackendFailure::OperationFailed);
        let cleanup = RemoteCommand::ordinary(["rm", "-rf", "--", &upload_directory])
            .map_err(|_| BackendFailure::OperationFailed)?;
        let cleaned = self
            .transport
            .run(&self.current_target(), cleanup)
            .map_err(|_| BackendFailure::OperationFailed);
        deployed?;
        cleaned?;
        if !self.verify_remote_helper(&request.application.sha256)? {
            return Err(BackendFailure::OperationFailed);
        }
        Ok(request.application.clone())
    }

    fn prepare_driver(
        &self,
        request: &InstallRequest,
    ) -> Result<crate::state::ArtifactIdentity, BackendFailure> {
        self.driver
            .ensure_ready(&self.transport, &self.current_target())?;
        Ok(request.driver.clone())
    }

    fn stage_tryboot(&self, _request: &InstallRequest) -> Result<(), BackendFailure> {
        if self.verify_reusable_accepted_driver()? {
            return Ok(());
        }
        self.driver.stage(&self.transport, &self.current_target())
    }

    fn boot_and_verify_tryboot(&self, _request: &InstallRequest) -> Result<(), BackendFailure> {
        if self.verify_reusable_accepted_driver()? {
            return Ok(());
        }
        let original = self.current_target();
        self.transport
            .run(
                &original,
                sudo_reboot_validation_command().map_err(|_| BackendFailure::OperationFailed)?,
            )
            .map_err(|_| BackendFailure::OperationFailed)?;
        let _expected_disconnect = self.transport.run(
            &original,
            tryboot_reboot_command().map_err(|_| BackendFailure::OperationFailed)?,
        );
        let reconnected = self
            .transport
            .wait_for_reboot(
                &self.expected_identity,
                std::slice::from_ref(&original),
                self.reconnect_policy(None)?,
            )
            .map_err(tryboot_wait_failure)?;
        *self.target.borrow_mut() = reconnected;
        if self
            .driver
            .verify_tryboot(&self.transport, &self.current_target())?
        {
            Ok(())
        } else {
            Err(BackendFailure::TrybootVerificationFailed)
        }
    }

    fn accept_driver(&self, _request: &InstallRequest) -> Result<(), BackendFailure> {
        if self.verify_reusable_accepted_driver()? {
            return Ok(());
        }
        self.driver
            .accept(&self.transport, &self.current_target())?;
        if self.verify_accepted_driver(false)? {
            Ok(())
        } else {
            Err(BackendFailure::OperationFailed)
        }
    }

    fn install_application(
        &self,
        request: &InstallRequest,
    ) -> Result<TargetApplicationInstall, BackendFailure> {
        if !self.verify_remote_helper(&request.application.sha256)? {
            return Err(BackendFailure::OperationFailed);
        }
        let checksum_path = format!("{}.sha256", self.helper_path);
        let revision_path = format!("{}.revision", self.helper_path);
        let install = target_install_command(&self.helper_path, &checksum_path, &revision_path)
            .map_err(|_| BackendFailure::OperationFailed)?;
        let output = self
            .transport
            .run(&self.current_target(), install)
            .map_err(|_| BackendFailure::SshLost)?;
        let result = TargetInstallResult::from_json(output.stdout())
            .map_err(|_| BackendFailure::OperationFailed)?;
        let ownership = self
            .transport
            .run(
                &self.current_target(),
                target_install_ownership_command(&self.helper_path)
                    .map_err(|_| BackendFailure::OperationFailed)?,
            )
            .map_err(|_| BackendFailure::SshLost)?;
        Ok(TargetApplicationInstall {
            result,
            ownership: TargetInstallOwnership::from_json(ownership.stdout())
                .map_err(|_| BackendFailure::OperationFailed)?,
        })
    }

    fn change_hostname_and_reconnect(
        &self,
        expected_identity: &TargetIdentity,
        desired_hostname: &str,
    ) -> Result<(), BackendFailure> {
        self.transport
            .run(
                &self.current_target(),
                hostname_command(desired_hostname).map_err(|_| BackendFailure::OperationFailed)?,
            )
            .map_err(|_| BackendFailure::SshLost)?;
        let desired = self.desired_target(desired_hostname)?;
        self.update_reconnected_target(desired, expected_identity)
    }

    fn reboot_final(&self, request: &InstallRequest) -> Result<(), BackendFailure> {
        let original = self.current_target();
        let _expected_disconnect = self.transport.run(
            &original,
            final_reboot_command().map_err(|_| BackendFailure::OperationFailed)?,
        );
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
        if !self.verify_accepted_driver(true)? || !self.verify_service_health()? {
            return Err(BackendFailure::FinalServiceFailed);
        }
        Ok(())
    }

    fn finish(&self, _request: &InstallRequest) -> Result<(), BackendFailure> {
        if self.verify_service_health()? {
            Ok(())
        } else {
            Err(BackendFailure::FinalServiceFailed)
        }
    }

    fn verify_phase(
        &self,
        phase: InstallPhase,
        request: &InstallRequest,
        _state: &InstallState,
    ) -> Result<PhaseVerification, BackendFailure> {
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
            InstallPhase::DriverReady => self
                .driver
                .ensure_ready(&self.transport, &self.current_target())
                .map(|()| true),
            InstallPhase::TrybootStaged => {
                if self.verify_reusable_accepted_driver()? {
                    return Ok(PhaseVerification::Valid);
                }
                let persisted = *self.persisted_target_phase.borrow();
                if persisted.is_some_and(|phase| phase >= InstallPhase::DriverAccepted) {
                    self.verify_accepted_driver(
                        persisted.is_some_and(|phase| phase >= InstallPhase::FinalRebooted),
                    )
                } else {
                    self.verify_staged_driver_transaction()
                }
            }
            InstallPhase::TrybootVerified => {
                if self.verify_reusable_accepted_driver()? {
                    return Ok(PhaseVerification::Valid);
                }
                if self
                    .persisted_target_phase
                    .borrow()
                    .is_some_and(|phase| phase >= InstallPhase::FinalRebooted)
                {
                    self.verify_accepted_driver(true)
                } else {
                    self.driver
                        .verify_tryboot(&self.transport, &self.current_target())
                }
            }
            InstallPhase::DriverAccepted => {
                if self.verify_reusable_accepted_driver()? {
                    Ok(true)
                } else {
                    self.verify_accepted_driver(
                        self.persisted_target_phase
                            .borrow()
                            .is_some_and(|phase| phase >= InstallPhase::FinalRebooted),
                    )
                }
            }
            InstallPhase::ApplicationInstalled => self.verify_installed_application(request),
            InstallPhase::HostnameChanged => {
                let desired = self.desired_target(&request.desired_hostname)?;
                self.update_reconnected_target(desired, &request.target)
                    .map(|()| true)
            }
            InstallPhase::FinalRebooted => self.verify_accepted_driver(true),
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

#[doc(hidden)]
pub fn tryboot_wait_failure(error: TransportError) -> BackendFailure {
    match error {
        TransportError::ReconnectTimedOut => BackendFailure::TrybootTimedOut,
        _ => BackendFailure::OperationFailed,
    }
}

fn tryboot_reboot_command() -> Result<RemoteCommand, TransportError> {
    RemoteCommand::interactive_sudo(["sudo", "reboot", "0 tryboot"])
}

#[doc(hidden)]
pub fn sudo_reboot_validation_command() -> Result<RemoteCommand, TransportError> {
    RemoteCommand::interactive_sudo(["sudo", "-v"])
}

fn final_reboot_command() -> Result<RemoteCommand, TransportError> {
    RemoteCommand::interactive_sudo(["sudo", "systemctl", "reboot"])
}

#[doc(hidden)]
pub fn hostname_command(hostname: &str) -> Result<RemoteCommand, TransportError> {
    RemoteCommand::interactive_sudo(["sudo", "hostnamectl", "set-hostname", hostname])
}

#[doc(hidden)]
pub fn target_install_command(
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

#[doc(hidden)]
pub fn target_install_ownership_command(
    helper_path: &str,
) -> Result<RemoteCommand, TransportError> {
    RemoteCommand::interactive_sudo(["sudo", helper_path, "installer-ownership"])
}

fn deploy_helper_command(
    upload_path: &str,
    helper_path: &str,
    sha256: &str,
    revision: &str,
) -> Result<RemoteCommand, TransportError> {
    RemoteCommand::interactive_sudo([
        "sudo",
        "sh",
        "-c",
        r#"set -eu; upload=$1; helper=$2; digest=$3; revision=$4; case "$helper" in /var/lib/planeradar-installer/helpers/"$digest"/planeradar) ;; *) exit 64 ;; esac; test ! -L "$upload" && test -f "$upload"; test "$(sha256sum -- "$upload" | awk '{print $1}')" = "$digest"; root=${helper%/planeradar}; install -d -o root -g root -m 0700 /var/lib/planeradar-installer; install -d -o root -g root -m 0700 /var/lib/planeradar-installer/helpers; install -d -o root -g root -m 0700 "$root"; binary_tmp="$root/.planeradar.$$"; checksum_tmp="$root/.planeradar.sha256.$$"; revision_tmp="$root/.planeradar.revision.$$"; trap 'rm -f -- "$binary_tmp" "$checksum_tmp" "$revision_tmp"' EXIT HUP INT TERM; install -o root -g root -m 0700 -- "$upload" "$binary_tmp"; printf '%s  planeradar\n' "$digest" >"$checksum_tmp"; printf '%s\n' "$revision" >"$revision_tmp"; chown root:root "$checksum_tmp" "$revision_tmp"; chmod 0600 "$checksum_tmp" "$revision_tmp"; test "$(sha256sum -- "$binary_tmp" | awk '{print $1}')" = "$digest"; mv -f -- "$binary_tmp" "$helper"; mv -f -- "$checksum_tmp" "$helper.sha256"; mv -f -- "$revision_tmp" "$helper.revision"; trap - EXIT HUP INT TERM; test ! -L "$helper" && test -f "$helper" && test -x "$helper"; test "$(stat -c '%u:%g:%a' -- "$helper")" = "0:0:700"; test "$(sha256sum -- "$helper" | awk '{print $1}')" = "$digest""#,
        "planeradar-helper-deploy",
        upload_path,
        helper_path,
        sha256,
        revision,
    ])
}

#[doc(hidden)]
pub fn staged_driver_transaction_command(
    expected: &DriverPostconditions,
) -> Result<RemoteCommand, TransportError> {
    RemoteCommand::interactive_sudo([
        "sudo",
        "sh",
        "-c",
        r#"set -eu; state=/var/lib/hyperpixel2r-kms/tryboot-state; regular() { test ! -L "$1" && test -f "$1" && test "$(stat -c '%u:%g:%a' -- "$1")" = "0:0:$2"; }; digest() { test "$(sha256sum -- "$1" | awk '{print $1}')" = "$2"; }; test ! -L "$state" && regular "$state" 600; test "$(awk -F= 'NF != 2 || $1 == "" || $2 == "" || seen[$1]++ { bad=1 } END { print NR ":" bad+0 }' "$state")" = "16:0"; value() { awk -F= -v key="$1" '$1 == key { print $2 }' "$state"; }; test "$(value schema_version)" = 1; test "$(value driver_version)" = "$1"; test "$(value source_revision)" = "$2"; test "$(value source_tree)" = "$3"; test "$(value kernel_release)" = "$4"; test "$(value module_file)" = "$7"; test "$(value module_sha256)" = "$8"; test "$(value overlay_file)" = "$9"; test "$(value overlay_sha256)" = "${10}"; test "$(value applied_dtb_file)" = "${11}"; test "$(value applied_dtb_sha256)" = "${12}"; test "$(value replaced_overlay)" = "${13}"; prior=$(value prior_tryboot_sha256); case "$(value tryboot_existed)" in true) case "$prior" in *[!0-9a-f]*|'') exit 1;; esac; test "${#prior}" = 64;; false) test "$prior" = none;; *) exit 1;; esac; for key in normal_config_sha256 candidate_config_sha256; do sha=$(value "$key"); case "$sha" in *[!0-9a-f]*|'') exit 1;; esac; test "${#sha}" = 64; done; artifact="/usr/lib/hyperpixel2r-kms/$1/$2/$4"; test ! -L "$artifact" && test -d "$artifact" && test "$(stat -c '%u:%g:%a' -- "$artifact")" = "0:0:755"; manifest="$artifact/manifest.txt"; regular "$manifest" 644 && digest "$manifest" "$6"; field() { awk -F '\t' -v key="$1" '$1 == key { print $2 }' "$manifest"; }; test "$(field driver_version)" = "$1"; test "$(field source_revision)" = "$2"; test "$(field source_tree)" = "$3"; test "$(field kernel_release)" = "$4"; test "$(field module_vermagic)" = "$5"; test "$(field module_file)" = "$7"; test "$(field module_sha256)" = "$8"; test "$(field overlay_file)" = "$9"; test "$(field overlay_sha256)" = "${10}"; test "$(field applied_dtb_file)" = "${11}"; test "$(field applied_dtb_sha256)" = "${12}"; regular "$artifact/$7" 644 && digest "$artifact/$7" "$8"; regular "$artifact/$9" 644 && digest "$artifact/$9" "${10}"; regular "$artifact/${11}" 644 && digest "$artifact/${11}" "${12}"; module="/lib/modules/$4/extra/$7"; overlay="/boot/firmware/overlays/$9"; normal=/boot/firmware/config.txt; candidate=/boot/firmware/tryboot.txt; regular "$module" 644 && digest "$module" "$8"; regular "$overlay" 644 && digest "$overlay" "${10}"; regular "$normal" 644 && digest "$normal" "$(value normal_config_sha256)"; regular "$candidate" 644 && digest "$candidate" "$(value candidate_config_sha256)"; test "$(awk -v wanted="dtoverlay=$9" '{ line=$0; sub(/^[[:space:]]+/, "", line); sub(/[[:space:]]+$/, "", line); if (line !~ /^dtoverlay=/) next; if (line == wanted) { count++; next } if (line ~ /hyperpixel2r/) bad=1 } END { print count ":" bad+0 }' "$candidate")" = "1:0""#,
        "planeradar-driver-transaction",
        &expected.driver_version,
        &expected.source_revision,
        &expected.source_tree,
        &expected.kernel_release,
        &expected.module_vermagic,
        &expected.manifest_sha256,
        &expected.module_file,
        &expected.module_sha256,
        &expected.overlay_file,
        &expected.overlay_sha256,
        &expected.applied_dtb_file,
        &expected.applied_dtb_sha256,
        &expected.replaced_overlay,
    ])
}

#[doc(hidden)]
pub fn accepted_driver_receipt_command(
    expected: &AcceptedDriverIdentity,
) -> Result<RemoteCommand, TransportError> {
    RemoteCommand::interactive_sudo([
        "sudo",
        "sh",
        "-c",
        r#"set -eu; root=/var/lib/hyperpixel2r-kms; state=$root/accepted-state; stock=$root/accepted-stock-config.txt; config=/boot/firmware/config.txt; directory() { test ! -L "$1" && test -d "$1" && test "$(stat -c '%u:%g:%a' -- "$1")" = "0:0:$2"; }; regular() { test ! -L "$1" && test -f "$1" && test "$(stat -c '%u:%g:%a:%h' -- "$1")" = "0:0:$2:1"; }; boot_regular() { test ! -L "$1" && test -f "$1" && test "$(stat -c '%u:%g:%h' -- "$1")" = "0:0:1"; case "$(stat -c '%a' -- "$1")" in 644|755) ;; *) return 1;; esac; }; digest() { test "$(sha256sum -- "$1" | awk '{print $1}')" = "$2"; }; absent() { test ! -L "$1" && test ! -e "$1"; }; valid_sha() { candidate=$1; case "$candidate" in *[!0-9a-f]*|'') return 1;; esac; test "${#candidate}" = 64; }; valid_revision() { candidate=$1; case "$candidate" in *[!0-9a-f]*|'') return 1;; esac; test "${#candidate}" = 40; }; directory "$root" 755; directory /usr/lib/hyperpixel2r-kms 755; regular "$state" 600; regular "$stock" 600; boot_regular "$config"; test "$(awk -F= 'NF != 2 || $1 == "" || $2 == "" || seen[$1]++ { bad=1 } END { print NR ":" bad+0 }' "$state")" = "12:0"; value() { awk -F= -v key="$1" '$1 == key { print $2 }' "$state"; }; for key in schema_version driver_version source_revision kernel_release manifest_sha256 module_file module_sha256 overlay_file overlay_sha256 normal_config_sha256 stock_config_sha256 prior_dkms_inventory_sha256; do test "$(awk -F= -v key="$key" '$1 == key { count++ } END { print count+0 }' "$state")" = 1; done; test "$(value schema_version)" = 2; test "$(value driver_version)" = "$1"; test "$(value source_revision)" = "$2"; test "$(value kernel_release)" = "$3"; test "$(value module_file)" = hyperpixel2r_kms.ko; test "$(value overlay_file)" = "$4"; for key in manifest_sha256 module_sha256 overlay_sha256 normal_config_sha256 stock_config_sha256 prior_dkms_inventory_sha256; do valid_sha "$(value "$key")"; done; digest "$config" "$(value normal_config_sha256)"; digest "$stock" "$(value stock_config_sha256)"; version_root="/usr/lib/hyperpixel2r-kms/$1"; revision_root="$version_root/$2"; artifact="$revision_root/$3"; directory "$version_root" 755; directory "$revision_root" 755; directory "$artifact" 755; manifest=$artifact/manifest.txt; regular "$manifest" 644; digest "$manifest" "$(value manifest_sha256)"; test "$(awk -F '\t' 'NF != 2 || $1 == "" || $2 == "" || seen[$1]++ { bad=1 } END { print NR ":" bad+0 }' "$manifest")" = "14:0"; field() { awk -F '\t' -v key="$1" '$1 == key { print $2 }' "$manifest"; }; for key in schema_version driver_version source_revision source_tree kernel_release architecture base_dtb_sha256 module_file module_sha256 module_vermagic overlay_file overlay_sha256 applied_dtb_file applied_dtb_sha256; do test "$(awk -F '\t' -v key="$key" '$1 == key { count++ } END { print count+0 }' "$manifest")" = 1; done; test "$(field schema_version)" = 1; test "$(field driver_version)" = "$1"; test "$(field source_revision)" = "$2"; test "$(field kernel_release)" = "$3"; test "$(field architecture)" = aarch64; valid_revision "$(field source_tree)"; valid_sha "$(field base_dtb_sha256)"; test "$(field module_file)" = "$(value module_file)"; test "$(field module_sha256)" = "$(value module_sha256)"; test "$(field overlay_file)" = "$(value overlay_file)"; test "$(field overlay_sha256)" = "$(value overlay_sha256)"; test "$(field applied_dtb_file)" = hyperpixel2r-kms-applied.dtb; valid_sha "$(field applied_dtb_sha256)"; case "$(field module_vermagic)" in "$3 "*) ;; *) exit 1;; esac; module=$artifact/$(value module_file); overlay=$artifact/$(value overlay_file); applied=$artifact/$(field applied_dtb_file); regular "$module" 644; digest "$module" "$(value module_sha256)"; regular "$overlay" 644; digest "$overlay" "$(value overlay_sha256)"; regular "$applied" 644; digest "$applied" "$(field applied_dtb_sha256)"; marker=$artifact/dkms-prior-state; regular "$marker" 600; digest "$marker" "$(value prior_dkms_inventory_sha256)"; installed_module=/lib/modules/$3/extra/$(value module_file); installed_overlay=/boot/firmware/overlays/$(value overlay_file); regular "$installed_module" 644; digest "$installed_module" "$(value module_sha256)"; boot_regular "$installed_overlay"; digest "$installed_overlay" "$(value overlay_sha256)"; test "$(awk -v wanted="dtoverlay=$4" '{ line=$0; sub(/^[[:space:]]+/, "", line); sub(/[[:space:]]+$/, "", line); if (line !~ /^dtoverlay=/) next; if (line == wanted) { count++; next } if (line ~ /hyperpixel2r/) bad=1 } END { print count ":" bad+0 }' "$config")" = "1:0"; for path in "$root/tryboot-state" "$root/rollback-state" "$root/accepted-transition" "$root/accepted-transition-prior-config.txt" "$root/accepted-uninstall" "$root/accepted-uninstall-stock.txt" "$root/rollback-candidate-dkms-state" "$root/rollback-candidate-tryboot.txt" /boot/firmware/tryboot.txt; do absent "$path"; done"#,
        "planeradar-driver-accepted-reuse",
        &expected.driver_version,
        &expected.source_revision,
        &expected.kernel_release,
        &expected.overlay_file,
    ])
}

#[doc(hidden)]
pub fn committed_driver_command(
    expected: &DriverPostconditions,
) -> Result<RemoteCommand, TransportError> {
    RemoteCommand::interactive_sudo([
        "sudo",
        "sh",
        "-c",
        r#"set -eu; state=/var/lib/hyperpixel2r-kms/tryboot-state; regular() { test ! -L "$1" && test -f "$1" && test "$(stat -c '%u:%g:%a' -- "$1")" = "0:0:$2"; }; digest() { test "$(sha256sum -- "$1" | awk '{print $1}')" = "$2"; }; test ! -L "$state" && test ! -e "$state"; artifact="/usr/lib/hyperpixel2r-kms/$1/$2/$4"; test ! -L "$artifact" && test -d "$artifact" && test "$(stat -c '%u:%g:%a' -- "$artifact")" = "0:0:755"; manifest="$artifact/manifest.txt"; regular "$manifest" 644 && digest "$manifest" "$6"; field() { awk -F '\t' -v key="$1" '$1 == key { print $2 }' "$manifest"; }; test "$(field driver_version)" = "$1"; test "$(field source_revision)" = "$2"; test "$(field source_tree)" = "$3"; test "$(field kernel_release)" = "$4"; test "$(field module_vermagic)" = "$5"; test "$(field module_file)" = "$7"; test "$(field module_sha256)" = "$8"; test "$(field overlay_file)" = "$9"; test "$(field overlay_sha256)" = "${10}"; test "$(field applied_dtb_file)" = "${11}"; test "$(field applied_dtb_sha256)" = "${12}"; regular "$artifact/$7" 644 && digest "$artifact/$7" "$8"; regular "$artifact/$9" 644 && digest "$artifact/$9" "${10}"; regular "$artifact/${11}" 644 && digest "$artifact/${11}" "${12}"; module="/lib/modules/$4/extra/$7"; overlay="/boot/firmware/overlays/$9"; config=/boot/firmware/config.txt; regular "$module" 644 && digest "$module" "$8"; regular "$overlay" 644 && digest "$overlay" "${10}"; regular "$config" 644; test "$(awk -v wanted="dtoverlay=$9" '{ line=$0; sub(/^[[:space:]]+/, "", line); sub(/[[:space:]]+$/, "", line); if (line !~ /^dtoverlay=/) next; if (line == wanted) { count++; next } if (line ~ /hyperpixel2r/) bad=1 } END { print count ":" bad+0 }' "$config")" = "1:0""#,
        "planeradar-driver-committed",
        &expected.driver_version,
        &expected.source_revision,
        &expected.source_tree,
        &expected.kernel_release,
        &expected.module_vermagic,
        &expected.manifest_sha256,
        &expected.module_file,
        &expected.module_sha256,
        &expected.overlay_file,
        &expected.overlay_sha256,
        &expected.applied_dtb_file,
        &expected.applied_dtb_sha256,
        &expected.replaced_overlay,
    ])
}

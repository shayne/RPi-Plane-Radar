use std::cell::{Cell, RefCell};
use std::collections::BTreeSet;
use std::fs;
use std::os::unix::fs::symlink;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;
use std::time::SystemTime;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use planeradar::install::{
    CommandRunner as TargetCommandRunner, InstallError as TargetInstallError,
    InstallOptions as TargetInstallOptions, Installer as TargetInstaller, installer_ownership_json,
    read_optional_installer_state_json, write_installer_state_json,
};
use planeradarctl::DriverLock;
use planeradarctl::driver::DriverPostconditions;
use planeradarctl::install::{
    ApplicationPayload, BackendFailure, InstallOutcome, InstallRequest,
    Installer as ControllerInstaller, InterruptionReason, extract_application_payload_at_mtime,
};
use planeradarctl::operations::{
    CaptureClock, CaptureMetadata, CaptureTransfer, DiagnosticFacts, OperationError,
    OperationsBackend, OperationsClient, SshOperationsBackend,
};
use planeradarctl::preflight::TARGET_FACTS_SCRIPT;
use planeradarctl::release::{ArtifactKind, ReleaseManifest};
use planeradarctl::smoke::verify_smoke_artifacts;
use planeradarctl::state::{
    ArtifactIdentity, InstallPhase, LocalStateStore, StateStore, TargetInstallState,
};
use planeradarctl::system_install::{
    DriverActions, HostPreflightGate, InstallClock, SystemInstallBackend,
};
use planeradarctl::target::{SshTarget, TargetIdentity};
use planeradarctl::transport::{
    Output, ReconnectPolicy, RemoteCommand, TargetProbe, Transport, TransportError,
};
use semver::Version;
use sha2::{Digest, Sha256};

#[path = "support/release_fixture.rs"]
mod release_fixture;

const FIXTURE_ROOT: &str = "tests/fixtures/pi-os-trixie";

fn fixture_files(root: &Path) -> Vec<PathBuf> {
    fn visit(root: &Path, current: &Path, files: &mut Vec<PathBuf>) {
        for entry in fs::read_dir(current).expect("read fixture directory") {
            let path = entry.expect("fixture entry").path();
            let metadata = fs::symlink_metadata(&path).expect("fixture metadata");
            assert!(
                metadata.file_type().is_dir() || metadata.file_type().is_file(),
                "fixture contains a symlink or special file: {}",
                path.display()
            );
            if metadata.file_type().is_dir() {
                visit(root, &path, files);
            } else {
                files.push(
                    path.strip_prefix(root)
                        .expect("fixture-relative path")
                        .to_owned(),
                );
            }
        }
    }

    let mut files = Vec::new();
    visit(root, root, &mut files);
    files.sort();
    files
}

#[test]
fn checked_in_pi_os_fixture_is_minimal_regular_and_private() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURE_ROOT);
    let actual = fixture_files(&root)
        .into_iter()
        .map(|path| path.to_string_lossy().into_owned())
        .collect::<BTreeSet<_>>();
    let expected = [
        "boot/firmware/config.txt",
        "etc/hostname",
        "etc/os-release",
        "etc/passwd",
        "lib/modules/6.12.47+rpt-rpi-v8/build/include/config/kernel.release",
        "proc/device-tree/model",
        "proc/device-tree/serial-number",
        "proc/modules",
        "proc/sys/kernel/osrelease",
        "run/planeradar-fixture/module-state",
        "run/planeradar-fixture/systemd-state",
        "sys/class/drm/card0-DPI-1/modes",
        "sys/class/input/event0/device/name",
        "var/lib/dpkg/arch",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<BTreeSet<_>>();
    assert_eq!(actual, expected);

    for relative in &actual {
        let metadata = fs::symlink_metadata(root.join(relative)).expect("fixture file");
        assert!(metadata.is_file(), "{relative} is not a regular file");
        assert_eq!(metadata.nlink(), 1, "{relative} is hard-linked");
        assert!(metadata.len() <= 4096, "{relative} is unexpectedly large");
    }

    let all_bytes = actual
        .iter()
        .flat_map(|relative| fs::read(root.join(relative)).expect("fixture bytes"))
        .collect::<Vec<_>>();
    let searchable = String::from_utf8_lossy(&all_bytes);
    for forbidden in [
        "maintainer-user",
        "planeradar.local",
        "ssh-",
        "BEGIN OPENSSH",
        "wpa_supplicant",
        "real-wifi-password",
    ] {
        assert!(
            !searchable.contains(forbidden),
            "fixture contains maintainer or secret token {forbidden:?}"
        );
    }
    assert!(searchable.contains("Raspberry Pi Zero 2 W"));
    assert!(searchable.contains("Raspberry Pi OS Lite Trixie"));
    assert!(searchable.contains("HyperPixel 2.1 Round Touch"));
    assert!(searchable.contains("pi:x:1000:1000:"));
}

#[test]
fn fresh_fixture_copies_are_byte_for_byte_deterministic() {
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURE_ROOT);
    let first = tempfile::tempdir().expect("first fixture copy");
    let second = tempfile::tempdir().expect("second fixture copy");
    copy_regular_tree(&source, first.path());
    copy_regular_tree(&source, second.path());

    for relative in fixture_files(&source) {
        assert_eq!(
            fs::read(first.path().join(&relative)).expect("first copy"),
            fs::read(second.path().join(&relative)).expect("second copy"),
            "{}",
            relative.display()
        );
    }
}

fn copy_regular_tree(source: &Path, destination: &Path) {
    for relative in fixture_files(source) {
        let target = destination.join(&relative);
        fs::create_dir_all(target.parent().expect("copy parent")).expect("create copy parent");
        fs::copy(source.join(relative), target).expect("copy fixture file");
    }
}

#[derive(Clone)]
struct FixtureCommandRunner {
    root: PathBuf,
}

impl TargetCommandRunner for FixtureCommandRunner {
    fn run(&self, program: &str, args: &[&str]) -> Result<(), TargetInstallError> {
        let log = self.root.join("run/planeradar-fixture/commands.log");
        let mut contents = fs::read_to_string(&log).unwrap_or_default();
        contents.push_str(program);
        for argument in args {
            contents.push('\t');
            contents.push_str(argument);
        }
        contents.push('\n');
        fs::write(log, contents)?;
        if program == "useradd" {
            let passwd = self.root.join("etc/passwd");
            let mut contents = fs::read_to_string(&passwd)?;
            if !contents.lines().any(|line| line.starts_with("planeradar:")) {
                contents.push_str(
                    "planeradar:x:991:991:Plane Radar:/var/lib/planeradar:/usr/sbin/nologin\n",
                );
                fs::write(passwd, contents)?;
            }
        }
        if program == "systemctl" {
            let state = self.root.join("run/planeradar-fixture/systemd-state");
            match args {
                ["enable", "planeradar.service"] => {
                    fs::write(state, "planeradar.service=enabled,inactive,restarts=0\n")?
                }
                ["restart", "planeradar.service"] => {
                    fs::write(state, "planeradar.service=enabled,active,restarts=0\n")?
                }
                _ => {}
            }
        }
        Ok(())
    }
}

#[derive(Clone)]
struct FixtureSystem {
    shared: Rc<FixtureSystemState>,
}

struct FixtureSystemState {
    root: PathBuf,
    identity: TargetIdentity,
    driver: ArtifactIdentity,
    interrupt_hostname_once: Cell<bool>,
    monotonic: Cell<Duration>,
    remote_commands: RefCell<Vec<Vec<String>>>,
    driver_actions: RefCell<Vec<&'static str>>,
    driver_targets: RefCell<Vec<(&'static str, SshTarget)>>,
    driver_tool_target: RefCell<Option<SshTarget>>,
    normal_boot_tool_targets: RefCell<Vec<SshTarget>>,
}

impl FixtureSystem {
    fn new(root: PathBuf, identity: TargetIdentity, driver: ArtifactIdentity) -> Self {
        Self {
            shared: Rc::new(FixtureSystemState {
                root,
                identity,
                driver,
                interrupt_hostname_once: Cell::new(true),
                monotonic: Cell::new(Duration::ZERO),
                remote_commands: RefCell::new(Vec::new()),
                driver_actions: RefCell::new(Vec::new()),
                driver_targets: RefCell::new(Vec::new()),
                driver_tool_target: RefCell::new(None),
                normal_boot_tool_targets: RefCell::new(Vec::new()),
            }),
        }
    }

    fn root(&self) -> &Path {
        &self.shared.root
    }

    fn path(&self, remote: impl AsRef<Path>) -> PathBuf {
        let relative = remote
            .as_ref()
            .strip_prefix("/")
            .unwrap_or_else(|_| remote.as_ref());
        assert!(
            relative
                .components()
                .all(|component| matches!(component, std::path::Component::Normal(_))),
            "remote path escaped fixture: {}",
            remote.as_ref().display()
        );
        self.root().join(relative)
    }

    fn sha256(&self, path: impl AsRef<Path>) -> String {
        format!(
            "{:x}",
            Sha256::digest(fs::read(path).expect("fixture digest input"))
        )
    }

    fn command_count(&self, marker: &str) -> usize {
        self.shared
            .remote_commands
            .borrow()
            .iter()
            .filter(|arguments| arguments.iter().any(|argument| argument.contains(marker)))
            .count()
    }

    fn target_facts(&self) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "model": "Raspberry Pi Zero 2 W Rev 1.0",
            "os_id": "raspbian",
            "os_version": "13",
            "architecture": "arm64",
            "kernel_release": "6.12.47+rpt-rpi-v8",
            "kernel_vermagic": "6.12.47+rpt-rpi-v8 SMP preempt mod_unload modversions aarch64",
            "default_target": "multi-user.target",
            "display_manager_active": false,
            "boot_config": "/boot/firmware/config.txt",
            "boot_config_regular": true,
            "tryboot_supported": true,
            "clock_synchronized": true,
            "system_time_unix": SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).expect("system time").as_secs(),
            "package_repository_reachable": true,
            "port_80_free": true,
            "root_available_bytes": 4_u64 * 1024 * 1024 * 1024,
            "boot_available_bytes": 512_u64 * 1024 * 1024,
            "running_headers_available": true,
            "running_headers_release": "6.12.47+rpt-rpi-v8",
            "installed_kernel_header_pair_count": 1,
            "installed_kernel_release": "6.12.47+rpt-rpi-v8",
            "installed_headers_release": "6.12.47+rpt-rpi-v8",
            "boot_selected_kernel_match_count": 1,
            "boot_selected_kernel_release": "6.12.47+rpt-rpi-v8",
            "boot_kernel_override_conflicting": false,
            "unsafe_overlay_present": false,
            "external_hyperpixel_overlay_count": 0,
            "external_hyperpixel_module_loaded": false,
            "unexpected_hyperpixel_module_loaded": false,
            "hyperpixel_state_dir_safe": true,
            "hyperpixel_transaction_active": false,
            "legacy_checkpoint_active": false,
            "external_hyperpixel_binding_count": 0,
            "gpio_display_state_safe": true
        }))
        .expect("target preflight JSON")
    }

    fn diagnostic_probe(&self) -> Vec<u8> {
        let state = TargetInstallState::from_json(
            fs::read_to_string(self.path("/var/lib/planeradar-installer/state.json"))
                .expect("diagnostic target state")
                .trim(),
        )
        .expect("diagnostic target state JSON");
        let application = state.application.expect("installed application identity");
        let driver = state.driver.expect("installed driver identity");
        let kernel = "6.12.47+rpt-rpi-v8";
        let module = self.path(format!("/lib/modules/{kernel}/extra/hyperpixel2r_kms.ko"));
        let overlay_file = format!("hyperpixel2r-kms-{}.dtbo", &driver.source_commit[..12]);
        let overlay = self.path(format!("/boot/firmware/overlays/{overlay_file}"));
        let health = serde_json::to_vec(&serde_json::json!({
            "configured": true,
            "state": "RADAR",
            "data_stale": false,
            "revision": application.source_commit
        }))
        .expect("health JSON");
        serde_json::to_vec(&serde_json::json!({
            "schema_version": 1,
            "os_id": "raspbian",
            "os_version": "13",
            "architecture": "arm64",
            "application_version": application.version,
            "application_revision": application.source_commit,
            "application_sha256": self.sha256(self.path("/opt/planeradar/bin/planeradar")),
            "driver_version": driver.version,
            "driver_revision": driver.source_commit,
            "driver_manifest_sha256": fs::read_to_string(self.path("/var/lib/planeradar-installer/driver-manifest.sha256")).expect("driver identity").trim(),
            "expected_kernel": kernel,
            "running_kernel": kernel,
            "module_loaded": true,
            "module_vermagic": format!("{kernel} SMP preempt mod_unload modversions aarch64"),
            "expected_module_vermagic": format!("{kernel} SMP preempt mod_unload modversions aarch64"),
            "module_sha256": self.sha256(&module),
            "expected_module_sha256": self.sha256(&module),
            "overlay_file": overlay_file,
            "expected_overlay_file": overlay_file,
            "overlay_sha256": self.sha256(&overlay),
            "expected_overlay_sha256": self.sha256(&overlay),
            "boot_config_sha256": self.sha256(self.path("/boot/firmware/config.txt")),
            "overlay_configured": true,
            "drm_device": "/dev/dri/card0",
            "drm_mode": "480x480",
            "renderer": "opengles2",
            "touch_device": "HyperPixel 2.1 Round Touch",
            "service_active": true,
            "service_restart_count": 0,
            "health_base64": STANDARD.encode(health),
            "hostname": fs::read_to_string(self.path("/etc/hostname")).expect("hostname").trim()
        }))
        .expect("diagnostic probe JSON")
    }

    fn deploy_helper(&self, arguments: &[String]) -> Result<Output, TransportError> {
        let upload = self.path(&arguments[5]);
        let helper = self.path(&arguments[6]);
        let digest = &arguments[7];
        let revision = &arguments[8];
        if self.sha256(&upload) != *digest {
            return Err(TransportError::CommandFailed);
        }
        fs::create_dir_all(helper.parent().expect("helper parent"))
            .map_err(|_| TransportError::CommandFailed)?;
        for directory in [
            self.path("/var/lib/planeradar-installer"),
            self.path("/var/lib/planeradar-installer/helpers"),
            helper.parent().expect("helper parent").to_owned(),
        ] {
            fs::set_permissions(directory, fs::Permissions::from_mode(0o700))
                .map_err(|_| TransportError::CommandFailed)?;
        }
        fs::copy(upload, &helper).map_err(|_| TransportError::CommandFailed)?;
        fs::set_permissions(&helper, fs::Permissions::from_mode(0o700))
            .map_err(|_| TransportError::CommandFailed)?;
        fs::write(
            helper.with_extension("sha256"),
            format!("{digest}  planeradar\n"),
        )
        .map_err(|_| TransportError::CommandFailed)?;
        fs::write(helper.with_extension("revision"), format!("{revision}\n"))
            .map_err(|_| TransportError::CommandFailed)?;
        fs::set_permissions(
            helper.with_extension("sha256"),
            fs::Permissions::from_mode(0o600),
        )
        .map_err(|_| TransportError::CommandFailed)?;
        fs::set_permissions(
            helper.with_extension("revision"),
            fs::Permissions::from_mode(0o600),
        )
        .map_err(|_| TransportError::CommandFailed)?;
        Ok(Output::success(Vec::new(), Vec::new()))
    }

    fn target_install(&self, helper: &str) -> Result<Output, TransportError> {
        let helper = self.path(helper);
        let published_revision = fs::read_to_string(helper.with_extension("revision"))
            .map_err(|_| TransportError::CommandFailed)?;
        fs::write(
            helper.with_extension("revision"),
            format!("{}\n", env!("PLANERADAR_REVISION")),
        )
        .map_err(|_| TransportError::CommandFailed)?;
        let runner = FixtureCommandRunner {
            root: self.root().to_owned(),
        };
        let result = TargetInstaller::new(&runner)
            .install(&TargetInstallOptions {
                root: self.root().to_owned(),
                boot_config: self.path("/boot/firmware/config.txt"),
                artifact: helper.clone(),
                checksum_file: helper.with_extension("sha256"),
                revision_file: helper.with_extension("revision"),
                reboot: false,
            })
            .map_err(|_| TransportError::CommandFailed)?;
        fs::write(self.path("/opt/planeradar/REVISION"), &published_revision)
            .map_err(|_| TransportError::CommandFailed)?;
        fs::write(helper.with_extension("revision"), &published_revision)
            .map_err(|_| TransportError::CommandFailed)?;
        let published_revision = published_revision.trim();
        let output = serde_json::to_vec(&serde_json::json!({
            "schema_version": 1,
            "files_changed": result.files_changed,
            "boot_config_changed": result.boot_config_changed,
            "reboot_required": result.reboot_required,
            "revision": published_revision,
            "sha256": self.sha256(&helper)
        }))
        .map_err(|_| TransportError::CommandFailed)?;
        Ok(Output::success(output, Vec::new()))
    }
}

impl HostPreflightGate for FixtureSystem {
    fn verify(
        &self,
        _repository: &Path,
        _docker_context: Option<&str>,
    ) -> Result<(), BackendFailure> {
        Ok(())
    }
}

impl InstallClock for FixtureSystem {
    fn now(&self) -> Duration {
        self.shared.monotonic.get()
    }

    fn sleep(&self, duration: Duration) {
        self.shared.monotonic.set(self.now() + duration);
    }
}

impl DriverActions<FixtureSystem> for FixtureSystem {
    fn ensure_ready(
        &self,
        _transport: &FixtureSystem,
        target: &SshTarget,
    ) -> Result<(), BackendFailure> {
        self.shared.driver_actions.borrow_mut().push("ensure");
        self.shared
            .driver_targets
            .borrow_mut()
            .push(("ensure", target.clone()));
        if self.shared.driver_tool_target.borrow().as_ref() != Some(target) {
            *self.shared.driver_tool_target.borrow_mut() = Some(target.clone());
        }
        Ok(())
    }

    fn postconditions(&self) -> Result<DriverPostconditions, BackendFailure> {
        let driver = &self.shared.driver;
        let kernel = "6.12.47+rpt-rpi-v8";
        let overlay = format!("hyperpixel2r-kms-{}.dtbo", &driver.source_commit[..12]);
        let artifact = self.path(format!(
            "/usr/lib/hyperpixel2r-kms/{}/{}/{kernel}",
            driver.version, driver.source_commit
        ));
        Ok(DriverPostconditions {
            driver_version: driver.version.clone(),
            source_revision: driver.source_commit.clone(),
            source_tree: release_fixture::SOURCE_TREE.into(),
            kernel_release: kernel.into(),
            module_vermagic: format!("{kernel} SMP preempt mod_unload modversions aarch64"),
            manifest_sha256: driver.sha256.clone(),
            module_file: "hyperpixel2r_kms.ko".into(),
            module_sha256: self.sha256(artifact.join("hyperpixel2r_kms.ko")),
            overlay_file: overlay.clone(),
            overlay_sha256: self.sha256(artifact.join(&overlay)),
            applied_dtb_file: "hyperpixel2r-kms-applied.dtb".into(),
            applied_dtb_sha256: self.sha256(artifact.join("hyperpixel2r-kms-applied.dtb")),
            replaced_overlay: "vc4-kms-dpi-hyperpixel2r".into(),
        })
    }

    fn stage(&self, _transport: &FixtureSystem, target: &SshTarget) -> Result<(), BackendFailure> {
        self.shared.driver_actions.borrow_mut().push("stage");
        self.shared
            .driver_targets
            .borrow_mut()
            .push(("stage", target.clone()));
        let driver = &self.shared.driver;
        let kernel = "6.12.47+rpt-rpi-v8";
        let overlay = format!("hyperpixel2r-kms-{}.dtbo", &driver.source_commit[..12]);
        let artifact = self.path(format!(
            "/usr/lib/hyperpixel2r-kms/{}/{}/{kernel}",
            driver.version, driver.source_commit
        ));
        fs::create_dir_all(&artifact).map_err(|_| BackendFailure::OperationFailed)?;
        let module_bytes = format!("module:{}\n", driver.source_commit);
        let overlay_bytes = format!("overlay:{}\n", driver.source_commit);
        let applied_dtb_bytes = format!("applied-dtb:{}\n", driver.source_commit);
        fs::write(artifact.join("hyperpixel2r_kms.ko"), &module_bytes)
            .map_err(|_| BackendFailure::OperationFailed)?;
        fs::write(artifact.join(&overlay), &overlay_bytes)
            .map_err(|_| BackendFailure::OperationFailed)?;
        fs::write(
            artifact.join("hyperpixel2r-kms-applied.dtb"),
            &applied_dtb_bytes,
        )
        .map_err(|_| BackendFailure::OperationFailed)?;
        fs::write(
            artifact.join("manifest.txt"),
            format!(
                "driver_version\t{}\nsource_revision\t{}\nsource_tree\t{}\nkernel_release\t{kernel}\nmodule_vermagic\t{kernel} SMP preempt mod_unload modversions aarch64\nmodule_file\thyperpixel2r_kms.ko\nmodule_sha256\t{}\noverlay_file\t{overlay}\noverlay_sha256\t{}\napplied_dtb_file\thyperpixel2r-kms-applied.dtb\napplied_dtb_sha256\t{}\n",
                driver.version,
                driver.source_commit,
                release_fixture::SOURCE_TREE,
                self.sha256(artifact.join("hyperpixel2r_kms.ko")),
                self.sha256(artifact.join(&overlay)),
                self.sha256(artifact.join("hyperpixel2r-kms-applied.dtb")),
            ),
        )
        .map_err(|_| BackendFailure::OperationFailed)?;
        let module = self.path(format!("/lib/modules/{kernel}/extra/hyperpixel2r_kms.ko"));
        fs::create_dir_all(module.parent().expect("module parent"))
            .map_err(|_| BackendFailure::OperationFailed)?;
        fs::write(module, module_bytes).map_err(|_| BackendFailure::OperationFailed)?;
        let target_overlay = self.path(format!("/boot/firmware/overlays/{overlay}"));
        fs::create_dir_all(target_overlay.parent().expect("overlay parent"))
            .map_err(|_| BackendFailure::OperationFailed)?;
        fs::write(target_overlay, overlay_bytes).map_err(|_| BackendFailure::OperationFailed)?;
        fs::write(
            self.path("/var/lib/planeradar-installer/driver-manifest.sha256"),
            format!("{}\n", driver.sha256),
        )
        .map_err(|_| BackendFailure::OperationFailed)?;
        fs::create_dir_all(self.path("/var/lib/hyperpixel2r-kms"))
            .map_err(|_| BackendFailure::OperationFailed)?;
        fs::write(
            self.path("/var/lib/hyperpixel2r-kms/tryboot-state"),
            "schema_version=1\n",
        )
        .map_err(|_| BackendFailure::OperationFailed)?;
        fs::write(
            self.path("/boot/firmware/tryboot.txt"),
            format!("[all]\ndtoverlay={overlay}\n"),
        )
        .map_err(|_| BackendFailure::OperationFailed)
    }

    fn verify_tryboot(
        &self,
        _transport: &FixtureSystem,
        target: &SshTarget,
    ) -> Result<bool, BackendFailure> {
        self.shared
            .driver_targets
            .borrow_mut()
            .push(("verify_tryboot", target.clone()));
        Ok(
            fs::read_to_string(self.path("/run/planeradar-fixture/module-state"))
                .is_ok_and(|state| state.trim() == "hyperpixel2r_kms=loaded"),
        )
    }

    fn accept(&self, _transport: &FixtureSystem, target: &SshTarget) -> Result<(), BackendFailure> {
        self.shared.driver_actions.borrow_mut().push("accept");
        self.shared
            .driver_targets
            .borrow_mut()
            .push(("accept", target.clone()));
        let overlay = format!(
            "hyperpixel2r-kms-{}.dtbo",
            &self.shared.driver.source_commit[..12]
        );
        fs::write(
            self.path("/boot/firmware/config.txt"),
            format!("[all]\ndtoverlay={overlay}\n"),
        )
        .map_err(|_| BackendFailure::OperationFailed)?;
        let _ = fs::remove_file(self.path("/boot/firmware/tryboot.txt"));
        let _ = fs::remove_file(self.path("/var/lib/hyperpixel2r-kms/tryboot-state"));
        Ok(())
    }

    fn verify_normal_boot(
        &self,
        _transport: &FixtureSystem,
        target: &SshTarget,
    ) -> Result<bool, BackendFailure> {
        self.shared
            .driver_targets
            .borrow_mut()
            .push(("verify_normal_boot", target.clone()));
        self.shared.normal_boot_tool_targets.borrow_mut().push(
            self.shared
                .driver_tool_target
                .borrow()
                .clone()
                .expect("fixture driver tool target"),
        );
        let overlay = format!(
            "dtoverlay=hyperpixel2r-kms-{}.dtbo",
            &self.shared.driver.source_commit[..12]
        );
        Ok(fs::read_to_string(self.path("/boot/firmware/config.txt"))
            .is_ok_and(|config| config.lines().any(|line| line == overlay)))
    }
}

impl Transport for FixtureSystem {
    fn probe(&self, _target: &SshTarget) -> Result<TargetProbe, TransportError> {
        Ok(TargetProbe {
            identity: self.shared.identity.clone(),
        })
    }

    fn run(&self, _target: &SshTarget, request: RemoteCommand) -> Result<Output, TransportError> {
        let arguments = request.arguments().to_vec();
        self.shared
            .remote_commands
            .borrow_mut()
            .push(arguments.clone());
        if arguments == ["sudo", "-v"] {
            return Ok(Output::success(Vec::new(), Vec::new()));
        }
        if arguments.len() == 3
            && arguments[0] == "sh"
            && arguments[1] == "-c"
            && arguments[2] == TARGET_FACTS_SCRIPT
        {
            return Ok(Output::success(self.target_facts(), Vec::new()));
        }
        if arguments
            .iter()
            .any(|value| value == "planeradar-helper-deploy")
        {
            return self.deploy_helper(&arguments);
        }
        if arguments
            .iter()
            .any(|value| value == "planeradar-driver-transaction")
        {
            return if self
                .path("/var/lib/hyperpixel2r-kms/tryboot-state")
                .is_file()
            {
                Ok(Output::success(Vec::new(), Vec::new()))
            } else {
                Err(TransportError::CommandFailed)
            };
        }
        if arguments
            .iter()
            .any(|value| value == "planeradar-driver-committed")
        {
            return if !self
                .path("/var/lib/hyperpixel2r-kms/tryboot-state")
                .exists()
            {
                Ok(Output::success(Vec::new(), Vec::new()))
            } else {
                Err(TransportError::CommandFailed)
            };
        }
        if arguments
            .iter()
            .any(|value| value.contains("mktemp -d /var/tmp/planeradar-upload.XXXXXXXXXX"))
        {
            let remote = "/var/tmp/planeradar-upload.ABCDEF1234";
            fs::create_dir_all(self.path(remote)).map_err(|_| TransportError::CommandFailed)?;
            return Ok(Output::success(
                format!("{remote}\n").into_bytes(),
                Vec::new(),
            ));
        }
        if arguments.first().is_some_and(|value| value == "rm")
            && arguments.get(1).is_some_and(|value| value == "-rf")
        {
            let _ = fs::remove_dir_all(self.path(arguments.last().expect("cleanup path")));
            return Ok(Output::success(Vec::new(), Vec::new()));
        }
        if arguments.len() >= 4
            && arguments[0] == "sudo"
            && arguments[1] == "test"
            && arguments[2] == "-x"
        {
            return if self.path(&arguments[3]).is_file() {
                Ok(Output::success(Vec::new(), Vec::new()))
            } else {
                Err(TransportError::CommandFailed)
            };
        }
        if arguments.iter().any(|value| value == "planeradar-helper") {
            let helper = self.path(&arguments[5]);
            return if helper.is_file() && self.sha256(helper) == arguments[6] {
                Ok(Output::success(Vec::new(), Vec::new()))
            } else {
                Err(TransportError::CommandFailed)
            };
        }
        if arguments.len() >= 4 && arguments[0] == "sudo" && arguments[2] == "installer-state" {
            let state_path = self.path("/var/lib/planeradar-installer/state.json");
            return match arguments[3].as_str() {
                "read" => Ok(Output::success(
                    read_optional_installer_state_json(&state_path)
                        .map_err(|_| TransportError::CommandFailed)?
                        .into_bytes(),
                    Vec::new(),
                )),
                "write" => {
                    let json = arguments.get(5).ok_or(TransportError::CommandFailed)?;
                    write_installer_state_json(&state_path, json.as_bytes())
                        .map_err(|_| TransportError::CommandFailed)?;
                    Ok(Output::success(json.as_bytes().to_vec(), Vec::new()))
                }
                _ => Err(TransportError::CommandFailed),
            };
        }
        if arguments.len() >= 3 && arguments[0] == "sudo" && arguments[2] == "install" {
            return self.target_install(&arguments[1]);
        }
        if arguments.len() == 3 && arguments[0] == "sudo" && arguments[2] == "installer-ownership" {
            let output = installer_ownership_json(self.root())
                .map_err(|_| TransportError::CommandFailed)?
                .into_bytes();
            return Ok(Output::success(output, Vec::new()));
        }
        if arguments
            .iter()
            .any(|value| value == "planeradar-installed")
        {
            let application = self.path("/opt/planeradar/bin/planeradar");
            let revision = fs::read_to_string(self.path("/opt/planeradar/REVISION"))
                .map_err(|_| TransportError::CommandFailed)?;
            return if application.is_file()
                && self.sha256(application) == arguments[4]
                && revision.trim() == arguments[5]
            {
                Ok(Output::success(Vec::new(), Vec::new()))
            } else {
                Err(TransportError::CommandFailed)
            };
        }
        if arguments == ["sudo", "hostnamectl", "set-hostname", "planeradar"] {
            if self.shared.interrupt_hostname_once.replace(false) {
                return Err(TransportError::ConnectionUnavailable);
            }
            fs::write(self.path("/etc/hostname"), "planeradar\n")
                .map_err(|_| TransportError::CommandFailed)?;
            return Ok(Output::success(Vec::new(), Vec::new()));
        }
        if arguments == ["sudo", "reboot", "0 tryboot"]
            || arguments == ["sudo", "systemctl", "reboot"]
        {
            return Err(TransportError::ConnectionUnavailable);
        }
        if arguments
            .iter()
            .any(|value| value.contains("systemctl is-enabled"))
        {
            return if fs::read_to_string(self.path("/run/planeradar-fixture/systemd-state"))
                .is_ok_and(|state| state.trim() == "planeradar.service=enabled,active,restarts=0")
            {
                Ok(Output::success(Vec::new(), Vec::new()))
            } else {
                Err(TransportError::CommandFailed)
            };
        }
        if arguments
            == [
                "/usr/bin/timeout",
                "10",
                "sudo",
                "-n",
                "/opt/planeradar/bin/planeradar",
                "installer-state",
                "read",
            ]
        {
            return Ok(Output::success(
                fs::read(self.path("/var/lib/planeradar-installer/state.json"))
                    .map_err(|_| TransportError::CommandFailed)?,
                Vec::new(),
            ));
        }
        if arguments
            .iter()
            .any(|value| value == "planeradar-diagnostics")
        {
            return Ok(Output::success(self.diagnostic_probe(), Vec::new()));
        }
        Err(TransportError::CommandFailed)
    }

    fn copy_to(
        &self,
        _target: &SshTarget,
        local: &Path,
        remote: &Path,
    ) -> Result<(), TransportError> {
        let destination = self.path(remote);
        fs::create_dir_all(
            destination
                .parent()
                .ok_or(TransportError::InvalidRemoteCopyPath)?,
        )
        .map_err(|_| TransportError::CommandFailed)?;
        fs::copy(local, destination).map_err(|_| TransportError::CommandFailed)?;
        Ok(())
    }

    fn copy_from(
        &self,
        _target: &SshTarget,
        remote: &Path,
        local: &Path,
    ) -> Result<(), TransportError> {
        fs::copy(self.path(remote), local).map_err(|_| TransportError::CommandFailed)?;
        Ok(())
    }

    fn wait_for_reboot(
        &self,
        _identity: &TargetIdentity,
        addresses: &[SshTarget],
        _policy: ReconnectPolicy,
    ) -> Result<SshTarget, TransportError> {
        fs::write(
            self.path("/run/planeradar-fixture/module-state"),
            "hyperpixel2r_kms=loaded\n",
        )
        .map_err(|_| TransportError::CommandFailed)?;
        addresses
            .first()
            .cloned()
            .ok_or(TransportError::NoReconnectCandidates)
    }
}

#[derive(Clone, Copy, Default)]
struct ZeroClock;

impl CaptureClock for ZeroClock {
    fn now(&self) -> Duration {
        Duration::ZERO
    }
}

struct SmokeDoctorBackend {
    facts: DiagnosticFacts,
}

impl OperationsBackend for SmokeDoctorBackend {
    fn diagnostic_facts(&self) -> Result<DiagnosticFacts, OperationError> {
        Ok(self.facts.clone())
    }

    fn debug_frame_metadata(
        &self,
        _timeout: Duration,
    ) -> Result<Option<CaptureMetadata>, OperationError> {
        Ok(None)
    }

    fn signal_debug_frame(&self, _timeout: Duration) -> Result<(), OperationError> {
        Err(OperationError::CaptureTimedOut)
    }

    fn capture_debug_frame(
        &self,
        _before: Option<&CaptureMetadata>,
        _timeout: Duration,
    ) -> Result<CaptureTransfer, OperationError> {
        Err(OperationError::CaptureTimedOut)
    }
}

fn fixture_release(root: &Path) -> (ArtifactIdentity, ArtifactIdentity, ApplicationPayload) {
    let generated = release_fixture::build_release(&root.join("release"));
    let release = generated.directory;
    let raw: serde_json::Value = serde_json::from_slice(
        &fs::read(release.join("release-manifest.json")).expect("assembled release manifest"),
    )
    .expect("release manifest JSON");
    let version = Version::parse(raw["version"].as_str().expect("release version"))
        .expect("semantic release version");
    let lock = DriverLock::checked_in().expect("driver lock");
    let manifest = ReleaseManifest::parse(
        &fs::read(release.join("release-manifest.json")).expect("release manifest"),
        &version,
        &lock,
    )
    .expect("verified local manifest");
    let application_archive = manifest
        .artifacts
        .iter()
        .find(|artifact| artifact.kind == ArtifactKind::Application)
        .expect("application artifact");
    let payload = extract_application_payload_at_mtime(
        &release.join(&application_archive.name),
        &application_archive.sha256,
        &root.join("payload-cache"),
        manifest.source_date_epoch,
    )
    .expect("verified assembled application");
    let application = ArtifactIdentity {
        version: manifest.version.to_string(),
        source_commit: manifest.source_commit,
        sha256: payload.sha256().into(),
    };
    let driver = ArtifactIdentity {
        version: manifest.driver.version.to_string(),
        source_commit: manifest.driver.commit,
        sha256: manifest.driver.manifest_sha256,
    };
    (application, driver, payload)
}

fn healthy_fixture_facts(
    root: &Path,
    application: &ArtifactIdentity,
    driver: &ArtifactIdentity,
) -> DiagnosticFacts {
    let module = root.join("lib/modules/6.12.47+rpt-rpi-v8/extra/hyperpixel2r_kms.ko");
    let overlay_name = format!("hyperpixel2r-kms-{}.dtbo", &driver.source_commit[..12]);
    let overlay = root.join("boot/firmware/overlays").join(&overlay_name);
    let digest =
        |path: &Path| format!("{:x}", Sha256::digest(fs::read(path).expect("health file")));
    let vermagic = "6.12.47+rpt-rpi-v8 SMP preempt mod_unload modversions aarch64";
    DiagnosticFacts {
        target_model: "Raspberry Pi Zero 2 W Rev 1.0".into(),
        target_serial: "10000000deadbeef".into(),
        expected_target_model: "Raspberry Pi Zero 2 W Rev 1.0".into(),
        expected_target_serial: "10000000deadbeef".into(),
        target_os_id: "raspbian".into(),
        target_os_version: "13".into(),
        target_architecture: "arm64".into(),
        installed_application: application.clone(),
        expected_application: application.clone(),
        running_application_revision: application.source_commit.clone(),
        installed_driver: driver.clone(),
        persisted_driver_manifest_sha256: driver.sha256.clone(),
        expected_driver: driver.clone(),
        running_kernel: "6.12.47+rpt-rpi-v8".into(),
        expected_kernel: "6.12.47+rpt-rpi-v8".into(),
        module_name: "hyperpixel2r_kms".into(),
        module_loaded: true,
        module_vermagic: vermagic.into(),
        expected_module_vermagic: vermagic.into(),
        module_sha256: digest(&module),
        expected_module_sha256: digest(&module),
        overlay_file: overlay_name.clone(),
        expected_overlay_file: overlay_name,
        overlay_sha256: digest(&overlay),
        expected_overlay_sha256: digest(&overlay),
        boot_config_sha256: digest(&root.join("boot/firmware/config.txt")),
        overlay_configured: true,
        drm_device: "/dev/dri/card0".into(),
        drm_mode: "480x480".into(),
        renderer: "opengles2".into(),
        touch_device: Some("HyperPixel 2.1 Round Touch".into()),
        service_active: true,
        service_restart_count: 0,
        http_healthy: true,
        mdns_hostname: "planeradar.local".into(),
        mdns_reachable: true,
        settings_configured: true,
    }
}

fn write_rgba_png(path: &Path, color: png::ColorType) {
    let channels = match color {
        png::ColorType::Rgba => 4,
        png::ColorType::Rgb => 3,
        _ => panic!("unsupported test color"),
    };
    let file = fs::File::create(path).expect("PNG output");
    let mut encoder = png::Encoder::new(file, 480, 480);
    encoder.set_color(color);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().expect("PNG header");
    writer
        .write_image_data(&vec![0x2a; 480 * 480 * channels])
        .expect("PNG pixels");
}

#[test]
fn smoke_verifier_binds_doctor_screenshot_and_local_release_identities() {
    let temporary = tempfile::tempdir().expect("smoke verifier fixture");
    let release = release_fixture::build_release(&temporary.path().join("release")).directory;
    let (application, driver, _payload) = fixture_release(temporary.path());
    let target = temporary.path().join("target-health");
    fs::create_dir_all(target.join("lib/modules/6.12.47+rpt-rpi-v8/extra"))
        .expect("health module parent");
    fs::create_dir_all(target.join("boot/firmware/overlays")).expect("health overlay parent");
    fs::write(
        target.join("lib/modules/6.12.47+rpt-rpi-v8/extra/hyperpixel2r_kms.ko"),
        b"module",
    )
    .expect("health module");
    fs::write(
        target.join("boot/firmware/overlays").join(format!(
            "hyperpixel2r-kms-{}.dtbo",
            &driver.source_commit[..12]
        )),
        b"overlay",
    )
    .expect("health overlay");
    fs::write(
        target.join("boot/firmware/config.txt"),
        format!(
            "[all]\ndtoverlay=hyperpixel2r-kms-{}.dtbo\n",
            &driver.source_commit[..12]
        ),
    )
    .expect("health boot config");
    let operations = SmokeDoctorBackend {
        facts: healthy_fixture_facts(&target, &application, &driver),
    };
    let doctor = temporary.path().join("doctor.json");
    fs::write(
        &doctor,
        OperationsClient::new(&operations, ZeroClock)
            .doctor()
            .expect("doctor")
            .to_json()
            .expect("doctor JSON"),
    )
    .expect("write doctor JSON");
    let screenshot = temporary.path().join("smoke-radar.png");
    write_rgba_png(&screenshot, png::ColorType::Rgba);

    let verified = verify_smoke_artifacts(&release, &doctor, &screenshot, SystemTime::UNIX_EPOCH)
        .expect("verified smoke");
    assert_eq!(verified.application, application);
    assert_eq!(verified.driver, driver);
    assert_eq!(verified.width, 480);
    assert_eq!(verified.height, 480);

    write_rgba_png(&screenshot, png::ColorType::Rgb);
    assert!(
        verify_smoke_artifacts(&release, &doctor, &screenshot, SystemTime::UNIX_EPOCH).is_err(),
        "RGB screenshot was accepted"
    );
    write_rgba_png(&screenshot, png::ColorType::Rgba);
    let mut trailing = fs::read(&screenshot).expect("valid screenshot");
    trailing.extend_from_slice(b"trailing-data");
    fs::write(&screenshot, trailing).expect("PNG with trailing bytes");
    assert!(
        verify_smoke_artifacts(&release, &doctor, &screenshot, SystemTime::UNIX_EPOCH).is_err(),
        "PNG with trailing bytes was accepted"
    );
    write_rgba_png(&screenshot, png::ColorType::Rgba);
    assert!(
        verify_smoke_artifacts(
            &release,
            &doctor,
            &screenshot,
            SystemTime::now() + Duration::from_secs(5)
        )
        .is_err(),
        "stale screenshot was accepted"
    );

    write_rgba_png(&screenshot, png::ColorType::Rgba);
    let mismatched_application = ArtifactIdentity {
        sha256: "f".repeat(64),
        ..application.clone()
    };
    let mismatched_operations = SmokeDoctorBackend {
        facts: healthy_fixture_facts(&target, &mismatched_application, &driver),
    };
    fs::write(
        &doctor,
        OperationsClient::new(&mismatched_operations, ZeroClock)
            .doctor()
            .expect("internally healthy mismatched doctor")
            .to_json()
            .expect("mismatched doctor JSON"),
    )
    .expect("write mismatched doctor");
    assert!(
        verify_smoke_artifacts(&release, &doctor, &screenshot, SystemTime::UNIX_EPOCH).is_err(),
        "application identity mismatch was accepted"
    );

    let real_png = temporary.path().join("real.png");
    write_rgba_png(&real_png, png::ColorType::Rgba);
    fs::remove_file(&screenshot).expect("remove screenshot before symlink");
    symlink(&real_png, &screenshot).expect("screenshot symlink");
    assert!(
        verify_smoke_artifacts(&release, &doctor, &screenshot, SystemTime::UNIX_EPOCH).is_err(),
        "screenshot symlink was accepted"
    );

    fs::remove_file(&screenshot).expect("remove screenshot symlink");
    write_rgba_png(&screenshot, png::ColorType::Rgba);
    let archive = release.join("planeradar-aarch64-linux-gnu.tar.zst");
    let mut drifted = fs::read(&archive).expect("application archive");
    drifted[0] ^= 0x01;
    fs::write(&archive, drifted).expect("drifted application archive");
    assert!(
        verify_smoke_artifacts(&release, &doctor, &screenshot, SystemTime::UNIX_EPOCH).is_err(),
        "application archive drift was accepted"
    );
}

#[test]
fn ephemeral_release_is_deterministic_and_independent_of_ignored_dist() {
    let temporary = tempfile::tempdir().expect("fresh-clone release fixture");
    let first = release_fixture::build_release(&temporary.path().join("first"));
    let second = release_fixture::build_release(&temporary.path().join("second"));
    assert!(first.directory.starts_with(temporary.path()));
    assert!(second.directory.starts_with(temporary.path()));
    for name in [
        "SHA256SUMS",
        "SBOM.spdx.json",
        "install.sh",
        "planeradar-aarch64-linux-gnu.tar.zst",
        "planeradarctl-aarch64-apple-darwin.tar.zst",
        "planeradarctl-x86_64-apple-darwin.tar.zst",
        "release-manifest.json",
    ] {
        assert_eq!(
            fs::read(first.directory.join(name)).expect("first generated asset"),
            fs::read(second.directory.join(name)).expect("second generated asset"),
            "generated release asset {name} drifted"
        );
    }
    let ignored_release = ["dist", "release"].join("/");
    assert!(
        !include_str!("ctl_end_to_end.rs").contains(&ignored_release)
            && !include_str!("support/release_fixture.rs").contains(&ignored_release),
        "the clean-clone E2E must not read ignored release state"
    );
}

#[test]
fn smoke_accepts_application_archives_above_the_control_ceiling() {
    let temporary = tempfile::tempdir().expect("large application fixture");
    let generated = release_fixture::build_release_with_payload(
        &temporary.path().join("release"),
        release_fixture::fixture_payload(17 * 1024 * 1024),
    );
    let archive = generated
        .directory
        .join("planeradar-aarch64-linux-gnu.tar.zst");
    assert!(
        fs::metadata(&archive)
            .expect("large application archive")
            .len()
            > 16 * 1024 * 1024,
        "fixture must cross the control artifact ceiling"
    );
    let target = temporary.path().join("target-health");
    fs::create_dir_all(target.join("lib/modules/6.12.47+rpt-rpi-v8/extra"))
        .expect("health module parent");
    fs::create_dir_all(target.join("boot/firmware/overlays")).expect("health overlay parent");
    fs::write(
        target.join("lib/modules/6.12.47+rpt-rpi-v8/extra/hyperpixel2r_kms.ko"),
        b"module",
    )
    .expect("health module");
    fs::write(
        target.join("boot/firmware/overlays").join(format!(
            "hyperpixel2r-kms-{}.dtbo",
            &generated.driver.source_commit[..12]
        )),
        b"overlay",
    )
    .expect("health overlay");
    fs::write(
        target.join("boot/firmware/config.txt"),
        format!(
            "[all]\ndtoverlay=hyperpixel2r-kms-{}.dtbo\n",
            &generated.driver.source_commit[..12]
        ),
    )
    .expect("health boot config");
    let operations = SmokeDoctorBackend {
        facts: healthy_fixture_facts(&target, &generated.application, &generated.driver),
    };
    let doctor = temporary.path().join("doctor.json");
    fs::write(
        &doctor,
        OperationsClient::new(&operations, ZeroClock)
            .doctor()
            .expect("doctor")
            .to_json()
            .expect("doctor JSON"),
    )
    .expect("write doctor JSON");
    let screenshot = temporary.path().join("smoke-radar.png");
    write_rgba_png(&screenshot, png::ColorType::Rgba);
    verify_smoke_artifacts(
        &generated.directory,
        &doctor,
        &screenshot,
        SystemTime::UNIX_EPOCH,
    )
    .expect("application archives may use the 128 MiB ceiling");
}

#[test]
fn controller_install_resume_reaches_every_real_phase() {
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURE_ROOT);
    let temporary = tempfile::tempdir().expect("end-to-end fixture");
    let root = temporary.path().join("target");
    fs::create_dir(&root).expect("target fixture root");
    let root = fs::canonicalize(root).expect("canonical target fixture root");
    copy_regular_tree(&source, &root);
    let (application, driver, payload) = fixture_release(&temporary.path().join("first-release"));
    let target = TargetIdentity {
        host_key_sha256: format!("SHA256:{}", "a".repeat(43)),
        model: "Raspberry Pi Zero 2 W Rev 1.0".into(),
        serial: "10000000deadbeef".into(),
    };
    let request = InstallRequest {
        target: target.clone(),
        application: application.clone(),
        driver: driver.clone(),
        desired_hostname: "planeradar".into(),
    };
    let home = temporary.path().join("home");
    let state = temporary.path().join("state");
    fs::create_dir(&home).expect("controller home");
    fs::create_dir(&state).expect("controller state");
    let first_store =
        LocalStateStore::new(&home, Some(&state), target.clone()).expect("first state store");
    let fixture = FixtureSystem::new(root.clone(), target.clone(), driver.clone());
    let first_backend = SystemInstallBackend::new(
        fixture.clone(),
        "pi@raspberrypi.local".parse().expect("fixture SSH target"),
        target.clone(),
        Path::new(env!("CARGO_MANIFEST_DIR")).to_owned(),
        None,
        payload,
        fixture.clone(),
        fixture.clone(),
        fixture.clone(),
    );
    assert_eq!(
        ControllerInstaller::new(&first_backend, &first_store)
            .run(request.clone())
            .expect("interrupted install"),
        InstallOutcome::Interrupted {
            phase: InstallPhase::ApplicationInstalled,
            reason: InterruptionReason::SshLost,
            guidance: None,
        }
    );
    let durable = first_store
        .load()
        .expect("first controller state")
        .expect("durable first controller state");
    assert_eq!(durable.phase, InstallPhase::ApplicationInstalled);
    let target_commands_before_resume =
        fs::read_to_string(root.join("run/planeradar-fixture/commands.log"))
            .expect("first command log");
    let helper_installs_before_resume = fixture.command_count("installer-ownership");
    let driver_mutations_before_resume = fixture
        .shared
        .driver_actions
        .borrow()
        .iter()
        .filter(|action| matches!(**action, "stage" | "accept"))
        .count();

    let resumed_store =
        LocalStateStore::new(&home, Some(&state), target.clone()).expect("fresh state store");
    *fixture.shared.driver_tool_target.borrow_mut() = None;
    let (_, _, resumed_payload) = fixture_release(&temporary.path().join("resumed-release"));
    let resumed_backend = SystemInstallBackend::new(
        fixture.clone(),
        "pi@raspberrypi.local".parse().expect("resumed SSH target"),
        target.clone(),
        Path::new(env!("CARGO_MANIFEST_DIR")).to_owned(),
        None,
        resumed_payload,
        fixture.clone(),
        fixture.clone(),
        fixture.clone(),
    );
    assert_eq!(
        ControllerInstaller::new(&resumed_backend, &resumed_store)
            .run(request)
            .expect("resumed install"),
        InstallOutcome::Complete
    );
    assert_eq!(
        fs::read_to_string(root.join("run/planeradar-fixture/commands.log"))
            .expect("resumed command log"),
        target_commands_before_resume,
        "resume repeated completed target installer mutations"
    );
    assert_eq!(
        fixture.command_count("installer-ownership"),
        helper_installs_before_resume,
        "resume repeated the target helper install protocol"
    );
    assert_eq!(
        fixture
            .shared
            .driver_actions
            .borrow()
            .iter()
            .filter(|action| matches!(**action, "stage" | "accept"))
            .count(),
        driver_mutations_before_resume,
        "resume repeated completed driver mutations"
    );

    let commands = target_commands_before_resume.lines().collect::<Vec<_>>();
    let update = commands
        .iter()
        .position(|line| *line == "apt-get\tupdate")
        .expect("apt update");
    let install = commands
        .iter()
        .position(|line| line.starts_with("apt-get\tinstall\t--yes"))
        .expect("apt install");
    let enable = commands
        .iter()
        .position(|line| *line == "systemctl\tenable\tplaneradar.service")
        .expect("service enable");
    let restart = commands
        .iter()
        .position(|line| *line == "systemctl\trestart\tplaneradar.service")
        .expect("service restart");
    assert!(update < install && install < enable && enable < restart);
    for package in [
        "libsdl2-2.0-0",
        "libegl1",
        "libgles2",
        "libgl1-mesa-dri",
        "avahi-daemon",
        "linux-headers-rpi-v8",
    ] {
        assert!(commands[install].split('\t').any(|value| value == package));
    }
    assert!(
        !commands
            .iter()
            .any(|line| { line.contains("full-upgrade") || line.contains("dist-upgrade") }),
        "fixture install escaped the package contract"
    );

    let helper = root
        .join("var/lib/planeradar-installer/helpers")
        .join(&application.sha256)
        .join("planeradar");
    assert_eq!(fixture.sha256(&helper), application.sha256);
    assert_eq!(
        fixture.sha256(root.join("opt/planeradar/bin/planeradar")),
        application.sha256
    );
    assert_eq!(
        fs::read_to_string(root.join("opt/planeradar/REVISION"))
            .expect("installed revision")
            .trim(),
        application.source_commit
    );
    let driver_root = root
        .join("usr/lib/hyperpixel2r-kms")
        .join(&driver.version)
        .join(&driver.source_commit)
        .join("6.12.47+rpt-rpi-v8");
    assert!(driver_root.join("manifest.txt").is_file());
    assert_eq!(
        fs::read_to_string(root.join("var/lib/planeradar-installer/driver-manifest.sha256"))
            .expect("driver manifest identity")
            .trim(),
        driver.sha256
    );
    assert_eq!(
        fs::read_to_string(root.join("etc/hostname"))
            .expect("hostname")
            .trim(),
        "planeradar"
    );
    assert_eq!(
        fs::read_to_string(root.join("run/planeradar-fixture/systemd-state"))
            .expect("systemd state")
            .trim(),
        "planeradar.service=enabled,active,restarts=0"
    );
    let final_state = TargetInstallState::from_json(
        fs::read_to_string(root.join("var/lib/planeradar-installer/state.json"))
            .expect("target installer state")
            .trim(),
    )
    .expect("valid target installer state");
    assert_eq!(final_state.last_verified_phase, InstallPhase::Complete);
    assert_eq!(final_state.application.as_ref(), Some(&application));
    assert_eq!(final_state.driver.as_ref(), Some(&driver));
    assert_eq!(final_state.owned_files.len(), 6);
    let operations = SshOperationsBackend::new(
        &fixture,
        "pi@planeradar.local"
            .parse()
            .expect("diagnostic fixture target"),
        DriverLock::checked_in().expect("diagnostic driver lock"),
    );
    let client = OperationsClient::new(&operations, ZeroClock);
    let doctor = client.doctor().expect("healthy fixture doctor");
    assert!(doctor.healthy);
    assert_eq!(doctor.facts.installed_application, application);
    assert_eq!(doctor.facts.installed_driver, driver);
    assert_eq!(
        client.status().expect("healthy fixture status").to_string(),
        format!(
            "Plane Radar healthy: app {}@{}, driver {}@{}, 480x480 opengles2",
            doctor.facts.installed_application.version,
            &doctor.facts.installed_application.source_commit[..12],
            doctor.facts.installed_driver.version,
            &doctor.facts.installed_driver.source_commit[..12],
        )
    );
    assert_eq!(
        format!(
            "http://{}.local",
            fs::read_to_string(root.join("etc/hostname"))
                .expect("URL hostname")
                .trim()
        ),
        "http://planeradar.local"
    );
    assert!(
        fixture.command_count("planeradar-driver-transaction") >= 1,
        "production staged-driver command builder was bypassed"
    );
    assert!(
        fixture.command_count("planeradar-driver-committed") >= 1,
        "production committed-driver command builder was bypassed"
    );
    let desired_driver_target: SshTarget = "pi@planeradar.local"
        .parse()
        .expect("desired driver target");
    let requested_normal_boot_targets = fixture
        .shared
        .driver_targets
        .borrow()
        .iter()
        .filter(|(action, _)| *action == "verify_normal_boot")
        .map(|(_, target)| target.clone())
        .collect::<Vec<_>>();
    assert!(
        !requested_normal_boot_targets.is_empty()
            && requested_normal_boot_targets
                .iter()
                .all(|target| target == &desired_driver_target),
        "backend did not pass the post-rename target to normal-boot verification"
    );
    let normal_boot_tool_targets = fixture.shared.normal_boot_tool_targets.borrow();
    assert!(
        !normal_boot_tool_targets.is_empty()
            && normal_boot_tool_targets
                .iter()
                .all(|target| target == &desired_driver_target),
        "normal-boot verification reused the pre-rename driver tool target"
    );
}

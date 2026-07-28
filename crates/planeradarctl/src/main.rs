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
        DriverAction, DriverContext, DriverManager, DriverPostconditions, DriverTool,
        GhDriverReleaseSource, GhDriverReleaseVerifier, TargetProbe as DriverTargetProbe,
    },
    install::{
        ApplicationPayload, BackendFailure, InstallBackend, InstallOutcome, InstallRequest,
        InstallStatusEvent, Installer, PhaseVerification, TargetApplicationInstall,
        TargetInstallOwnership, TargetInstallResult, extract_application_payload,
    },
    preflight::{HostPreflight, SystemUnixClock, TargetPreflight},
    release::{GhReleaseSource, MANIFEST_NAME, ReleaseClient, ReleaseInput, Verifier},
    state::{
        ArtifactIdentity, InstallPhase, InstallState, LocalStateStore, StateError, StateStore,
        TARGET_STATE_PATH, TargetInstallState, TargetStateStore,
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
    let application_payload = extract_application_payload(
        &application_artifact.path,
        &application_artifact.artifact.sha256,
        &cache_root.join("payloads"),
    )?;
    let payload_sha256 = application_payload.sha256().to_owned();
    let source_commit = release.manifest.source_commit.clone();
    let release_version = release.manifest.version.clone();

    let application_identity = ArtifactIdentity {
        version: release_version.to_string(),
        source_commit,
        sha256: payload_sha256.clone(),
    };
    let driver_identity = ArtifactIdentity {
        version: lock.version.to_string(),
        source_commit: lock.commit.clone(),
        sha256: lock.manifest_sha256.clone(),
    };
    let transport =
        OpenSshTransport::system(TransportConfig::new(home.join(".ssh").join("known_hosts"))?);
    let (target, observed, state_store) = select_install_target(
        &transport,
        target,
        &config.hostname,
        &home,
        &application_identity,
        &driver_identity,
    )?;
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
        helper_path: format!("/var/lib/planeradar-installer/helpers/{payload_sha256}/planeradar"),
    };
    let request = InstallRequest {
        target: observed,
        application: application_identity,
        driver: driver_identity,
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

fn select_install_target(
    transport: &OpenSshTransport<SystemCommandRunner>,
    original: SshTarget,
    desired_hostname: &str,
    home: &Path,
    application: &ArtifactIdentity,
    driver: &ArtifactIdentity,
) -> Result<(SshTarget, TargetIdentity, LocalStateStore), Box<dyn std::error::Error>> {
    let desired = format!("{}@{desired_hostname}.local", original.username().as_str())
        .parse::<SshTarget>()?;
    let mut candidates = Vec::new();
    for (is_original, target) in [(true, original), (false, desired)] {
        if candidates
            .iter()
            .any(|candidate: &InstallCandidate| candidate.target == target)
        {
            continue;
        }
        if let Ok(probe) = transport.probe(&target) {
            let store = LocalStateStore::from_environment(home, probe.identity.clone())?;
            let persisted = store.load()?;
            candidates.push(InstallCandidate {
                target,
                observed: probe.identity,
                store,
                persisted,
                is_original,
            });
        }
    }
    if candidates.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "neither the original target nor the desired hostname is reachable",
        )
        .into());
    }
    let candidate_states = candidates
        .iter()
        .map(|candidate| InstallCandidateState {
            is_original: candidate.is_original,
            observed: candidate.observed.clone(),
            persisted: candidate.persisted.clone(),
        })
        .collect::<Vec<_>>();
    let selected = select_candidate_index(&candidate_states, application, driver)?;
    let candidate = candidates.swap_remove(selected);
    Ok((candidate.target, candidate.observed, candidate.store))
}

struct InstallCandidate {
    target: SshTarget,
    observed: TargetIdentity,
    store: LocalStateStore,
    persisted: Option<InstallState>,
    is_original: bool,
}

struct InstallCandidateState {
    is_original: bool,
    observed: TargetIdentity,
    persisted: Option<InstallState>,
}

fn select_candidate_index(
    candidates: &[InstallCandidateState],
    application: &ArtifactIdentity,
    driver: &ArtifactIdentity,
) -> Result<usize, io::Error> {
    let matching = candidates
        .iter()
        .enumerate()
        .filter_map(|(index, candidate)| {
            candidate
                .persisted
                .as_ref()
                .is_some_and(|persisted| {
                    resume_state_matches(persisted, &candidate.observed, application, driver)
                })
                .then_some(index)
        })
        .collect::<Vec<_>>();
    match matching.as_slice() {
        [index] => return Ok(*index),
        [_, _, ..] => {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "multiple reachable targets match the persisted installation identity",
            ));
        }
        [] => {}
    }
    if candidates
        .iter()
        .any(|candidate| candidate.persisted.is_some())
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "reachable target does not match its persisted installation identity and artifacts",
        ));
    }
    candidates
        .iter()
        .position(|candidate| candidate.is_original)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "desired hostname has no persisted installation identity",
            )
        })
}

fn resume_state_matches(
    persisted: &InstallState,
    observed: &TargetIdentity,
    application: &ArtifactIdentity,
    driver: &ArtifactIdentity,
) -> bool {
    &persisted.target == observed
        && persisted.application
            == (persisted.phase >= InstallPhase::ApplicationAcquired).then(|| application.clone())
        && persisted.driver
            == (persisted.phase >= InstallPhase::DriverReady).then(|| driver.clone())
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

fn sudo_reboot_validation_command() -> Result<RemoteCommand, TransportError> {
    RemoteCommand::interactive_sudo(["sudo", "-v"])
}

fn tryboot_wait_failure(error: TransportError) -> BackendFailure {
    match error {
        TransportError::ReconnectTimedOut => BackendFailure::TrybootTimedOut,
        _ => BackendFailure::OperationFailed,
    }
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

fn target_install_ownership_command(helper_path: &str) -> Result<RemoteCommand, TransportError> {
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

fn staged_driver_transaction_command(
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

fn committed_driver_command(
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
        self.ensure_driver_tool()?;
        let tool = self.driver_tool.borrow();
        let tool = tool.as_ref().ok_or(BackendFailure::OperationFailed)?;
        let expected = tool
            .postconditions()
            .map_err(|_| BackendFailure::OperationFailed)?;
        let command = staged_driver_transaction_command(&expected)
            .map_err(|_| BackendFailure::OperationFailed)?;
        self.run_remote_check(command)
    }

    fn verify_committed_driver(&self) -> Result<bool, BackendFailure> {
        self.ensure_driver_tool()?;
        let tool = self.driver_tool.borrow();
        let tool = tool.as_ref().ok_or(BackendFailure::OperationFailed)?;
        let expected = tool
            .postconditions()
            .map_err(|_| BackendFailure::OperationFailed)?;
        let command =
            committed_driver_command(&expected).map_err(|_| BackendFailure::OperationFailed)?;
        self.run_remote_check(command)
    }

    fn verify_accepted_driver(&self, normal_boot: bool) -> Result<bool, BackendFailure> {
        if !self.verify_committed_driver()? {
            return Ok(false);
        }
        self.ensure_driver_tool()?;
        let tool = self.driver_tool.borrow();
        let tool = tool.as_ref().ok_or(BackendFailure::OperationFailed)?;
        if normal_boot {
            tool.verify_normal_boot().map(|_| true).or(Ok(false))
        } else {
            tool.run(DriverAction::VerifyBoot)
                .map(|_| true)
                .or(Ok(false))
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
        let create_upload = RemoteCommand::ordinary([
            "sh",
            "-c",
            "umask 077; mktemp -d /var/tmp/planeradar-upload.XXXXXXXXXX",
        ])
        .map_err(|_| BackendFailure::OperationFailed)?;
        let upload_output = self
            .transport
            .run(&self.current_target(), create_upload)
            .map_err(|_| BackendFailure::SshLost)?;
        let upload_directory = std::str::from_utf8(upload_output.stdout())
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
        let validate =
            sudo_reboot_validation_command().map_err(|_| BackendFailure::OperationFailed)?;
        self.transport
            .run(&original, validate)
            .map_err(|_| BackendFailure::OperationFailed)?;
        let reboot = tryboot_reboot_command().map_err(|_| BackendFailure::OperationFailed)?;
        let _expected_disconnect = self.transport.run(&original, reboot);
        let reconnected = self
            .transport
            .wait_for_reboot(
                &self.expected_identity,
                std::slice::from_ref(&original),
                self.reconnect_policy(None)?,
            )
            .map_err(tryboot_wait_failure)?;
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
            .map_err(|_| BackendFailure::OperationFailed)?;
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
        if self.verify_service_health()? {
            Ok(())
        } else {
            Err(BackendFailure::FinalServiceFailed)
        }
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
                if persisted.is_some_and(|phase| phase >= InstallPhase::DriverAccepted) {
                    self.verify_accepted_driver(
                        persisted.is_some_and(|phase| phase >= InstallPhase::FinalRebooted),
                    )
                } else {
                    self.verify_staged_driver_transaction()
                }
            }
            InstallPhase::TrybootVerified => {
                self.ensure_driver_tool()?;
                if self
                    .persisted_target_phase
                    .borrow()
                    .is_some_and(|phase| phase >= InstallPhase::FinalRebooted)
                {
                    self.ensure_driver_tool()?;
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
            InstallPhase::DriverAccepted => self.verify_accepted_driver(
                self.persisted_target_phase
                    .borrow()
                    .is_some_and(|phase| phase >= InstallPhase::FinalRebooted),
            ),
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
        ArtifactIdentity, BackendFailure, InstallCandidateState, InstallPhase, InstallState,
        TargetIdentity, TransportError, committed_driver_command, deploy_helper_command,
        final_reboot_command, hostname_command, resume_state_matches, select_candidate_index,
        staged_driver_transaction_command, target_install_command,
        target_install_ownership_command, tryboot_reboot_command, tryboot_wait_failure,
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
    fn tryboot_timeout_requires_an_observed_disconnect() {
        assert_eq!(
            tryboot_wait_failure(TransportError::ReconnectTimedOut),
            BackendFailure::TrybootTimedOut
        );
        assert_eq!(
            tryboot_wait_failure(TransportError::NeverDisconnected),
            BackendFailure::OperationFailed
        );
        assert_eq!(
            tryboot_wait_failure(TransportError::CommandFailed),
            BackendFailure::OperationFailed
        );
    }

    #[test]
    fn fresh_process_hostname_resume_requires_the_persisted_exact_identity_and_artifacts() {
        let target = TargetIdentity {
            host_key_sha256: "SHA256:8R2K6pFDwIKY2fWb/4mMxwAA7PY8VYyLmWucTx7D99A".into(),
            model: "Raspberry Pi Zero 2 W Rev 1.0".into(),
            serial: "10000000abcdef01".into(),
        };
        let application = ArtifactIdentity {
            version: "1.2.3".into(),
            source_commit: "1".repeat(40),
            sha256: "2".repeat(64),
        };
        let driver = ArtifactIdentity {
            version: "1.2.3".into(),
            source_commit: "3".repeat(40),
            sha256: "4".repeat(64),
        };
        let resumed = InstallState {
            schema_version: 1,
            target: target.clone(),
            phase: InstallPhase::HostnameChanged,
            application: Some(application.clone()),
            driver: Some(driver.clone()),
        };

        assert!(resume_state_matches(
            &resumed,
            &target,
            &application,
            &driver
        ));
        let mut wrong_identity = target.clone();
        wrong_identity.serial = "10000000abcdef02".into();
        assert!(!resume_state_matches(
            &resumed,
            &wrong_identity,
            &application,
            &driver
        ));
        let mut wrong_artifact = resumed;
        wrong_artifact
            .application
            .as_mut()
            .expect("application")
            .sha256 = "5".repeat(64);
        assert!(!resume_state_matches(
            &wrong_artifact,
            &target,
            &application,
            &driver
        ));
    }

    #[test]
    fn durable_matching_identity_wins_over_a_reachable_reused_original_hostname() {
        let application = ArtifactIdentity {
            version: "1.2.3".into(),
            source_commit: "1".repeat(40),
            sha256: "2".repeat(64),
        };
        let driver = ArtifactIdentity {
            version: "1.2.3".into(),
            source_commit: "3".repeat(40),
            sha256: "4".repeat(64),
        };
        let installed = TargetIdentity {
            host_key_sha256: "SHA256:8R2K6pFDwIKY2fWb/4mMxwAA7PY8VYyLmWucTx7D99A".into(),
            model: "Raspberry Pi Zero 2 W Rev 1.0".into(),
            serial: "10000000abcdef01".into(),
        };
        let reused_original = TargetIdentity {
            host_key_sha256: "SHA256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".into(),
            model: "Raspberry Pi Zero 2 W Rev 1.0".into(),
            serial: "10000000abcdef02".into(),
        };
        let persisted = InstallState {
            schema_version: 1,
            target: installed.clone(),
            phase: InstallPhase::HostnameChanged,
            application: Some(application.clone()),
            driver: Some(driver.clone()),
        };
        let candidates = [
            InstallCandidateState {
                is_original: true,
                observed: reused_original,
                persisted: None,
            },
            InstallCandidateState {
                is_original: false,
                observed: installed,
                persisted: Some(persisted),
            },
        ];

        assert_eq!(
            select_candidate_index(&candidates, &application, &driver).expect("safe candidate"),
            1
        );
        assert_eq!(
            select_candidate_index(&candidates[..1], &application, &driver)
                .expect("fresh original"),
            0
        );
    }

    #[test]
    fn driver_postconditions_are_bound_to_exact_transaction_and_committed_identity() {
        let expected = planeradarctl::driver::DriverPostconditions {
            driver_version: "0.1.0".into(),
            source_revision: "f6213007a8e780309e34b220351fc229e3c7d554".into(),
            source_tree: "1111111111111111111111111111111111111111".into(),
            kernel_release: "6.12.47+rpt-rpi-v8".into(),
            module_vermagic: "6.12.47+rpt-rpi-v8 SMP preempt mod_unload aarch64".into(),
            manifest_sha256: "2222222222222222222222222222222222222222222222222222222222222222"
                .into(),
            module_file: "hyperpixel2r_kms.ko".into(),
            module_sha256: "3333333333333333333333333333333333333333333333333333333333333333"
                .into(),
            overlay_file: "hyperpixel2r-kms-f6213007a8e7.dtbo".into(),
            overlay_sha256: "4444444444444444444444444444444444444444444444444444444444444444"
                .into(),
            applied_dtb_file: "hyperpixel2r-kms-applied.dtb".into(),
            applied_dtb_sha256: "5555555555555555555555555555555555555555555555555555555555555555"
                .into(),
            replaced_overlay: "vc4-kms-dpi-hyperpixel2r".into(),
        };
        let command =
            staged_driver_transaction_command(&expected).expect("staged transaction command");
        assert!(command.is_interactive_sudo());
        assert_eq!(
            &command.arguments()[4..],
            [
                "planeradar-driver-transaction",
                "0.1.0",
                "f6213007a8e780309e34b220351fc229e3c7d554",
                "1111111111111111111111111111111111111111",
                "6.12.47+rpt-rpi-v8",
                "6.12.47+rpt-rpi-v8 SMP preempt mod_unload aarch64",
                "2222222222222222222222222222222222222222222222222222222222222222",
                "hyperpixel2r_kms.ko",
                "3333333333333333333333333333333333333333333333333333333333333333",
                "hyperpixel2r-kms-f6213007a8e7.dtbo",
                "4444444444444444444444444444444444444444444444444444444444444444",
                "hyperpixel2r-kms-applied.dtb",
                "5555555555555555555555555555555555555555555555555555555555555555",
                "vc4-kms-dpi-hyperpixel2r",
            ]
        );

        let committed = committed_driver_command(&expected).expect("committed");
        assert!(committed.is_interactive_sudo());
        assert_eq!(
            &committed.arguments()[4..],
            [
                "planeradar-driver-committed",
                "0.1.0",
                "f6213007a8e780309e34b220351fc229e3c7d554",
                "1111111111111111111111111111111111111111",
                "6.12.47+rpt-rpi-v8",
                "6.12.47+rpt-rpi-v8 SMP preempt mod_unload aarch64",
                "2222222222222222222222222222222222222222222222222222222222222222",
                "hyperpixel2r_kms.ko",
                "3333333333333333333333333333333333333333333333333333333333333333",
                "hyperpixel2r-kms-f6213007a8e7.dtbo",
                "4444444444444444444444444444444444444444444444444444444444444444",
                "hyperpixel2r-kms-applied.dtb",
                "5555555555555555555555555555555555555555555555555555555555555555",
                "vc4-kms-dpi-hyperpixel2r",
            ]
        );
    }

    #[test]
    fn production_adapter_invokes_the_versioned_helper_with_exact_machine_arguments() {
        let digest = "2".repeat(64);
        let helper = format!("/var/lib/planeradar-installer/helpers/{digest}/planeradar");
        let checksum = format!("{helper}.sha256");
        let revision = format!("{helper}.revision");
        let command =
            target_install_command(&helper, &checksum, &revision).expect("target install command");

        assert!(command.is_interactive_sudo());
        assert_eq!(
            command.arguments(),
            [
                "sudo",
                helper.as_str(),
                "install",
                "--artifact",
                helper.as_str(),
                "--checksum-file",
                checksum.as_str(),
                "--revision-file",
                revision.as_str(),
                "--json",
            ]
        );
        let ownership =
            target_install_ownership_command(&helper).expect("target ownership command");
        assert!(ownership.is_interactive_sudo());
        assert_eq!(
            ownership.arguments(),
            ["sudo", helper.as_str(), "installer-ownership"]
        );

        let revision_identity = "1".repeat(40);
        let deploy = deploy_helper_command(
            "/var/tmp/planeradar-upload.ABCDEF1234/payload",
            &helper,
            &digest,
            &revision_identity,
        )
        .expect("deployment command");
        assert!(deploy.is_interactive_sudo());
        assert_eq!(deploy.arguments()[0..3], ["sudo", "sh", "-c"]);
        assert_eq!(
            &deploy.arguments()[4..],
            [
                "planeradar-helper-deploy",
                "/var/tmp/planeradar-upload.ABCDEF1234/payload",
                helper.as_str(),
                digest.as_str(),
                revision_identity.as_str(),
            ]
        );
    }
}

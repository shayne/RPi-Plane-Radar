use std::fmt;
use std::fs;
use std::io::{Cursor, Read};
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tempfile::NamedTempFile;
use thiserror::Error;

use crate::config::DriverLock;
use crate::state::{ArtifactIdentity, TargetInstallState};
use crate::target::SshTarget;
use crate::transport::{RemoteCommand, Transport};

pub const DOCTOR_SCHEMA_VERSION: u32 = 1;
pub const MAX_DOCTOR_JSON_BYTES: usize = 32 * 1024;
pub const MAX_CAPTURE_BYTES: u64 = 8 * 1024 * 1024;
const CAPTURE_POLL_INTERVAL: Duration = Duration::from_millis(100);
const EXPECTED_WIDTH: u32 = 480;
const EXPECTED_HEIGHT: u32 = 480;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticCode {
    Healthy,
    TargetIdentityMismatch,
    TargetPlatformMismatch,
    ApplicationVersionMismatch,
    ApplicationRevisionMismatch,
    ApplicationChecksumMismatch,
    DriverVersionMismatch,
    DriverRevisionMismatch,
    DriverManifestMismatch,
    KernelMismatch,
    ModuleMismatch,
    OverlayMismatch,
    ServiceInactive,
    UnexpectedRestartCount,
    HttpFailure,
    TouchMissing,
    DrmModeWrong,
    RendererWrong,
    MdnsFailure,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DiagnosticFacts {
    pub target_model: String,
    pub target_serial: String,
    pub expected_target_model: String,
    pub expected_target_serial: String,
    pub target_os_id: String,
    pub target_os_version: String,
    pub target_architecture: String,
    pub installed_application: ArtifactIdentity,
    pub expected_application: ArtifactIdentity,
    pub installed_driver: ArtifactIdentity,
    pub expected_driver: ArtifactIdentity,
    pub running_kernel: String,
    pub expected_kernel: String,
    pub module_name: String,
    pub module_loaded: bool,
    pub module_vermagic: String,
    pub expected_module_vermagic: String,
    pub overlay_file: String,
    pub expected_overlay_file: String,
    pub overlay_configured: bool,
    pub drm_device: String,
    pub drm_mode: String,
    pub renderer: String,
    pub touch_device: Option<String>,
    pub service_active: bool,
    pub service_restart_count: u64,
    pub http_healthy: bool,
    pub mdns_hostname: String,
    pub mdns_reachable: bool,
    pub settings_configured: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DoctorReport {
    pub schema_version: u32,
    pub healthy: bool,
    pub diagnostics: Vec<DiagnosticCode>,
    pub facts: DiagnosticFacts,
}

impl DoctorReport {
    pub fn to_json(&self) -> Result<String, OperationError> {
        self.validate()?;
        let json = serde_json::to_string(self).map_err(|_| OperationError::InvalidDoctorJson)?;
        if json.len() > MAX_DOCTOR_JSON_BYTES {
            return Err(OperationError::DoctorOutputTooLarge);
        }
        Ok(json)
    }

    pub fn from_json(input: &[u8]) -> Result<Self, OperationError> {
        if input.len() > MAX_DOCTOR_JSON_BYTES {
            return Err(OperationError::DoctorOutputTooLarge);
        }
        let mut deserializer = serde_json::Deserializer::from_slice(input);
        let report =
            Self::deserialize(&mut deserializer).map_err(|_| OperationError::InvalidDoctorJson)?;
        deserializer
            .end()
            .map_err(|_| OperationError::InvalidDoctorJson)?;
        report.validate()?;
        Ok(report)
    }

    fn validate(&self) -> Result<(), OperationError> {
        if self.schema_version != DOCTOR_SCHEMA_VERSION {
            return Err(OperationError::InvalidDoctorJson);
        }
        validate_facts(&self.facts)?;
        let expected = evaluate(&self.facts);
        if self.diagnostics != expected || self.healthy != (expected == [DiagnosticCode::Healthy]) {
            return Err(OperationError::InvalidDoctorJson);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StatusReport {
    application_version: String,
    application_revision: String,
    driver_version: String,
    driver_revision: String,
    drm_mode: String,
    renderer: String,
}

impl fmt::Display for StatusReport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "Plane Radar healthy: app {}@{}, driver {}@{}, {} {}",
            self.application_version,
            &self.application_revision[..12],
            self.driver_version,
            &self.driver_revision[..12],
            self.drm_mode,
            self.renderer
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScreenshotResult {
    pub destination: PathBuf,
    pub sha256: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureMetadata {
    pub inode: u64,
    pub modified_ns: u64,
    pub size: u64,
    pub sha256: String,
    pub uid: u32,
    pub gid: u32,
    pub mode: u32,
    pub links: u64,
    pub regular: bool,
    pub symlink: bool,
}

pub trait OperationsBackend {
    fn diagnostic_facts(&self) -> Result<DiagnosticFacts, OperationError>;
    fn debug_frame_metadata(&self) -> Result<Option<CaptureMetadata>, OperationError>;
    fn signal_debug_frame(&self) -> Result<(), OperationError>;
    fn publish_debug_frame(&self) -> Result<CaptureMetadata, OperationError>;
    fn published_frame_metadata(&self) -> Result<CaptureMetadata, OperationError>;
    fn fetch_published_frame(&self, destination: &Path) -> Result<(), OperationError>;
}

const TARGET_STATE_COMMAND: [&str; 7] = [
    "/usr/bin/timeout",
    "10",
    "sudo",
    "-n",
    "/opt/planeradar/bin/planeradar",
    "installer-state",
    "read",
];
const DEBUG_FRAME_PATH: &str = "/var/lib/planeradar/debug.png";
const PUBLISHED_FRAME_PATH: &str = "/var/lib/planeradar-installer/captures/current.png";
const MAX_TARGET_STATE_BYTES: usize = 64 * 1024;
const MAX_PROBE_BYTES: usize = 32 * 1024;
const MAX_METADATA_BYTES: usize = 2 * 1024;

const DIAGNOSTIC_SCRIPT: &str = concat!(
    r#"set -eu; "#,
    r#"os_id=$(sed -n 's/^ID=//p' /etc/os-release | tr -d '"'); os_version=$(sed -n 's/^VERSION_ID=//p' /etc/os-release | tr -d '"'); architecture=$(dpkg --print-architecture); "#,
    r#"app=/opt/planeradar/bin/planeradar; test ! -L "$app" && test -x "$app"; "#,
    r#"test ! -L /opt/planeradar/REVISION && test -f /opt/planeradar/REVISION; "#,
    r#"application_line=$("$app" version); application_version=$(printf '%s\n' "$application_line" | awk 'NF == 3 && $1 == "planeradar" { print $2 }'); "#,
    r#"application_revision=$(tr -d '\r\n' </opt/planeradar/REVISION); application_sha256=$(sha256sum -- "$app" | awk '{print $1}'); "#,
    r#"driver_root="/usr/lib/hyperpixel2r-kms/$1/$2"; test ! -L "$driver_root" && test -d "$driver_root"; "#,
    r#"kernel_count=0; driver_dir=; for candidate in "$driver_root"/*; do test ! -L "$candidate" && test -d "$candidate" || continue; kernel_count=$((kernel_count + 1)); driver_dir=$candidate; done; test "$kernel_count" = 1; "#,
    r#"manifest="$driver_dir/manifest.txt"; test ! -L "$manifest" && test -f "$manifest" && test "$(stat -c '%u:%g:%a' -- "$manifest")" = 0:0:644; "#,
    r#"field() { awk -F '\t' -v key="$1" '$1 == key { if (seen++) exit 2; value=$2 } END { if (!seen || value == "") exit 1; print value }' "$manifest"; }; "#,
    r#"test "$(field driver_version)" = "$1"; test "$(field source_revision)" = "$2"; expected_kernel=$(field kernel_release); expected_module_vermagic=$(field module_vermagic); expected_overlay_file=$(field overlay_file); "#,
    r#"running_kernel=$(uname -r); module_loaded=false; if awk '$1 == "hyperpixel2r_kms" { count++ } END { exit count != 1 }' /proc/modules; then module_loaded=true; fi; "#,
    r#"module_vermagic=$(/usr/sbin/modinfo -F vermagic hyperpixel2r_kms 2>/dev/null || printf unavailable); "#,
    r#"overlay_count=$(awk '{ line=$0; sub(/^[[:space:]]+/, "", line); sub(/[[:space:]]+$/, "", line); if (line ~ /^dtoverlay=.*hyperpixel2r/) count++ } END { print count+0 }' /boot/firmware/config.txt); overlay_file=unavailable; if test "$overlay_count" = 1; then overlay_file=$(awk '{ line=$0; sub(/^[[:space:]]+/, "", line); sub(/[[:space:]]+$/, "", line); if (line ~ /^dtoverlay=.*hyperpixel2r/) { sub(/^dtoverlay=/, "", line); print line } }' /boot/firmware/config.txt); fi; overlay_configured=false; if test "$overlay_file" = "$expected_overlay_file"; then overlay_configured=true; fi; "#,
    r#"drm_device=/dev/dri/card0; drm_mode=$(sed -n '1p' /sys/class/drm/card0-DPI-1/modes 2>/dev/null || true); test -n "$drm_mode" || drm_mode=unavailable; "#,
    r#"renderer=$(journalctl -u planeradar.service -b --no-pager -o cat 2>/dev/null | sed -n 's/.*render_driver=\([^ ]*\).*/\1/p' | tail -n 1); test -n "$renderer" || renderer=unavailable; "#,
    r#"touch_device=; touch_count=0; for name_file in /sys/class/input/event*/device/name; do test ! -L "$name_file" && test -f "$name_file" || continue; candidate=$(tr -d '\r\n' <"$name_file"); case "$candidate" in *HyperPixel*) touch_count=$((touch_count + 1)); touch_device=$candidate;; esac; done; test "$touch_count" -le 1; "#,
    r#"service_active=false; if systemctl is-active --quiet planeradar.service; then service_active=true; fi; service_restart_count=$(systemctl show planeradar.service --property=NRestarts --value); "#,
    r#"hostname=$(tr -d '\r\n' </etc/hostname); health=$(curl --fail --silent --show-error --max-time 5 -H "Host: $hostname.local" http://127.0.0.1/healthz 2>/dev/null || true); http_healthy=false; settings_configured=false; "#,
    r#"case "$health" in *'"configured":true'*) http_healthy=true; settings_configured=true;; *'"configured":false'*) http_healthy=true;; esac; "#,
    r#"printf '{"schema_version":1,"os_id":"%s","os_version":"%s","architecture":"%s","application_version":"%s","application_revision":"%s","application_sha256":"%s","expected_kernel":"%s","running_kernel":"%s","module_loaded":%s,"module_vermagic":"%s","expected_module_vermagic":"%s","overlay_file":"%s","expected_overlay_file":"%s","overlay_configured":%s,"drm_device":"%s","drm_mode":"%s","renderer":"%s","touch_device":"%s","service_active":%s,"service_restart_count":%s,"http_healthy":%s,"hostname":"%s","settings_configured":%s}' "#,
    r#""$os_id" "$os_version" "$architecture" "$application_version" "$application_revision" "$application_sha256" "$expected_kernel" "$running_kernel" "$module_loaded" "$module_vermagic" "$expected_module_vermagic" "$overlay_file" "$expected_overlay_file" "$overlay_configured" "$drm_device" "$drm_mode" "$renderer" "$touch_device" "$service_active" "$service_restart_count" "$http_healthy" "$hostname" "$settings_configured""#,
);

const METADATA_SCRIPT: &str = concat!(
    r#"set -eu; path=$1; if test ! -e "$path" && test ! -L "$path"; then printf null; exit 0; fi; "#,
    r#"symlink=false; regular=false; if test -L "$path"; then symlink=true; elif test -f "$path"; then regular=true; fi; "#,
    r#"inode=$(stat -c %i -- "$path" 2>/dev/null || printf 0); seconds=$(stat -c %Y -- "$path" 2>/dev/null || printf 0); modified_ns=$((seconds * 1000000000)); size=$(stat -c %s -- "$path" 2>/dev/null || printf 0); "#,
    r#"uid=$(stat -c %u -- "$path" 2>/dev/null || printf 0); gid=$(stat -c %g -- "$path" 2>/dev/null || printf 0); mode_text=$(stat -c %a -- "$path" 2>/dev/null || printf 0); mode=$(printf '%d' "0$mode_text"); links=$(stat -c %h -- "$path" 2>/dev/null || printf 0); "#,
    r#"sha256=0000000000000000000000000000000000000000000000000000000000000000; if test "$regular" = true && test "$symlink" = false; then sha256=$(sha256sum -- "$path" | awk '{print $1}'); fi; "#,
    r#"printf '{"inode":%s,"modified_ns":%s,"size":%s,"sha256":"%s","uid":%s,"gid":%s,"mode":%s,"links":%s,"regular":%s,"symlink":%s}' "$inode" "$modified_ns" "$size" "$sha256" "$uid" "$gid" "$mode" "$links" "$regular" "$symlink""#,
);

const PUBLISH_SCRIPT: &str = concat!(
    r#"set -eu; src=$1; dst=$2; directory=${dst%/*}; "#,
    r#"test ! -L "$src" && test -f "$src" && test "$(stat -c %h -- "$src")" = 1; service_uid=$(id -u planeradar); test "$(stat -c %u -- "$src")" = "$service_uid"; case "$(stat -c %a -- "$src")" in 600|640) ;; *) exit 1;; esac; "#,
    r#"size=$(stat -c %s -- "$src"); test "$size" -gt 0 && test "$size" -le 8388608; before=$(stat -c '%i:%Y:%s:%u:%g:%a:%h' -- "$src"); before_sha=$(sha256sum -- "$src" | awk '{print $1}'); "#,
    r#"if test -e "$directory" || test -L "$directory"; then test ! -L "$directory" && test -d "$directory" && test "$(stat -c '%u:%g:%a' -- "$directory")" = 0:0:700; else install -d -m 700 -o root -g root -- "$directory"; fi; "#,
    r#"if test -e "$dst" || test -L "$dst"; then test ! -L "$dst" && test -f "$dst" && test "$(stat -c '%u:%g:%a:%h' -- "$dst")" = 0:0:600:1; fi; "#,
    r#"tmp=$(mktemp "$directory/.capture.XXXXXXXX"); trap 'rm -f -- "$tmp"' EXIT HUP INT TERM; dd if="$src" of="$tmp" iflag=nofollow status=none; "#,
    r#"after=$(stat -c '%i:%Y:%s:%u:%g:%a:%h' -- "$src"); after_sha=$(sha256sum -- "$src" | awk '{print $1}'); test "$before" = "$after" && test "$before_sha" = "$after_sha"; test "$(sha256sum -- "$tmp" | awk '{print $1}')" = "$before_sha"; "#,
    r#"chown root:root -- "$tmp"; chmod 600 -- "$tmp"; sync -f "$tmp"; mv -fT -- "$tmp" "$dst"; trap - EXIT HUP INT TERM; sync -f "$directory"; "#,
    r#"path=$dst; inode=$(stat -c %i -- "$path"); seconds=$(stat -c %Y -- "$path"); modified_ns=$((seconds * 1000000000)); size=$(stat -c %s -- "$path"); uid=$(stat -c %u -- "$path"); gid=$(stat -c %g -- "$path"); mode_text=$(stat -c %a -- "$path"); mode=$(printf '%d' "0$mode_text"); links=$(stat -c %h -- "$path"); sha256=$(sha256sum -- "$path" | awk '{print $1}'); "#,
    r#"printf '{"inode":%s,"modified_ns":%s,"size":%s,"sha256":"%s","uid":%s,"gid":%s,"mode":%s,"links":%s,"regular":true,"symlink":false}' "$inode" "$modified_ns" "$size" "$sha256" "$uid" "$gid" "$mode" "$links""#,
);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DiagnosticProbe {
    schema_version: u32,
    os_id: String,
    os_version: String,
    architecture: String,
    application_version: String,
    application_revision: String,
    application_sha256: String,
    expected_kernel: String,
    running_kernel: String,
    module_loaded: bool,
    module_vermagic: String,
    expected_module_vermagic: String,
    overlay_file: String,
    expected_overlay_file: String,
    overlay_configured: bool,
    drm_device: String,
    drm_mode: String,
    renderer: String,
    touch_device: String,
    service_active: bool,
    service_restart_count: u64,
    http_healthy: bool,
    hostname: String,
    settings_configured: bool,
}

pub struct SshOperationsBackend<'a, T> {
    transport: &'a T,
    target: SshTarget,
    expected_driver: DriverLock,
}

impl<'a, T: Transport> SshOperationsBackend<'a, T> {
    pub fn new(transport: &'a T, target: SshTarget, expected_driver: DriverLock) -> Self {
        Self {
            transport,
            target,
            expected_driver,
        }
    }

    fn run(&self, request: RemoteCommand) -> Result<Vec<u8>, OperationError> {
        self.transport
            .run(&self.target, request)
            .map(|output| output.stdout().to_vec())
            .map_err(|_| OperationError::Transport)
    }

    fn target_state(&self) -> Result<TargetInstallState, OperationError> {
        let request =
            RemoteCommand::ordinary(TARGET_STATE_COMMAND).map_err(|_| OperationError::Transport)?;
        let output = self.run(request)?;
        if output.len() > MAX_TARGET_STATE_BYTES {
            return Err(OperationError::MalformedFacts);
        }
        let mut deserializer = serde_json::Deserializer::from_slice(&output);
        let state = Option::<TargetInstallState>::deserialize(&mut deserializer)
            .map_err(|_| OperationError::MalformedFacts)?;
        deserializer
            .end()
            .map_err(|_| OperationError::MalformedFacts)?;
        state.ok_or(OperationError::MalformedFacts)
    }

    fn diagnostic_probe(
        &self,
        installed_driver: &ArtifactIdentity,
    ) -> Result<DiagnosticProbe, OperationError> {
        let request = RemoteCommand::ordinary([
            "/usr/bin/timeout",
            "15",
            "sudo",
            "-n",
            "sh",
            "-c",
            DIAGNOSTIC_SCRIPT,
            "planeradar-diagnostics",
            &installed_driver.version,
            &installed_driver.source_commit,
        ])
        .map_err(|_| OperationError::Transport)?;
        let output = self.run(request)?;
        parse_bounded_json(&output, MAX_PROBE_BYTES)
    }

    fn capture_metadata_at(
        &self,
        path: &'static str,
    ) -> Result<Option<CaptureMetadata>, OperationError> {
        let request = RemoteCommand::ordinary([
            "/usr/bin/timeout",
            "10",
            "sudo",
            "-n",
            "sh",
            "-c",
            METADATA_SCRIPT,
            "planeradar-capture-metadata",
            path,
        ])
        .map_err(|_| OperationError::Transport)?;
        let output = self.run(request)?;
        parse_bounded_json(&output, MAX_METADATA_BYTES)
    }
}

impl<T: Transport> OperationsBackend for SshOperationsBackend<'_, T> {
    fn diagnostic_facts(&self) -> Result<DiagnosticFacts, OperationError> {
        let observed = self
            .transport
            .probe(&self.target)
            .map_err(|_| OperationError::Transport)?
            .identity;
        let state = self.target_state()?;
        let expected_application = state.application.ok_or(OperationError::MalformedFacts)?;
        let installed_driver = state.driver.ok_or(OperationError::MalformedFacts)?;
        let probe = self.diagnostic_probe(&installed_driver)?;
        if probe.schema_version != DOCTOR_SCHEMA_VERSION {
            return Err(OperationError::MalformedFacts);
        }
        let mdns_hostname = format!("{}.local", probe.hostname);
        let mdns_reachable = format!("{}@{}", self.target.username().as_str(), mdns_hostname)
            .parse::<SshTarget>()
            .ok()
            .and_then(|target| self.transport.probe(&target).ok())
            .is_some_and(|probe| observed.matches(&probe.identity));
        Ok(DiagnosticFacts {
            target_model: observed.model,
            target_serial: observed.serial,
            expected_target_model: state.hardware.model,
            expected_target_serial: state.hardware.serial,
            target_os_id: probe.os_id,
            target_os_version: probe.os_version,
            target_architecture: probe.architecture,
            installed_application: ArtifactIdentity {
                version: probe.application_version,
                source_commit: probe.application_revision,
                sha256: probe.application_sha256,
            },
            expected_application,
            installed_driver,
            expected_driver: ArtifactIdentity {
                version: self.expected_driver.version.to_string(),
                source_commit: self.expected_driver.commit.clone(),
                sha256: self.expected_driver.manifest_sha256.clone(),
            },
            running_kernel: probe.running_kernel,
            expected_kernel: probe.expected_kernel,
            module_name: "hyperpixel2r_kms".into(),
            module_loaded: probe.module_loaded,
            module_vermagic: probe.module_vermagic,
            expected_module_vermagic: probe.expected_module_vermagic,
            overlay_file: probe.overlay_file,
            expected_overlay_file: probe.expected_overlay_file,
            overlay_configured: probe.overlay_configured,
            drm_device: probe.drm_device,
            drm_mode: probe.drm_mode,
            renderer: probe.renderer,
            touch_device: (!probe.touch_device.is_empty()).then_some(probe.touch_device),
            service_active: probe.service_active,
            service_restart_count: probe.service_restart_count,
            http_healthy: probe.http_healthy,
            mdns_hostname,
            mdns_reachable,
            settings_configured: probe.settings_configured,
        })
    }

    fn debug_frame_metadata(&self) -> Result<Option<CaptureMetadata>, OperationError> {
        self.capture_metadata_at(DEBUG_FRAME_PATH)
    }

    fn signal_debug_frame(&self) -> Result<(), OperationError> {
        let request = RemoteCommand::ordinary([
            "/usr/bin/timeout",
            "10",
            "sudo",
            "-n",
            "systemctl",
            "kill",
            "--signal=SIGUSR1",
            "planeradar.service",
        ])
        .map_err(|_| OperationError::Transport)?;
        self.run(request).map(|_| ())
    }

    fn publish_debug_frame(&self) -> Result<CaptureMetadata, OperationError> {
        let request = RemoteCommand::ordinary([
            "/usr/bin/timeout",
            "15",
            "sudo",
            "-n",
            "sh",
            "-c",
            PUBLISH_SCRIPT,
            "planeradar-capture-publish",
            DEBUG_FRAME_PATH,
            PUBLISHED_FRAME_PATH,
        ])
        .map_err(|_| OperationError::Transport)?;
        let output = self.run(request)?;
        parse_bounded_json(&output, MAX_METADATA_BYTES)
    }

    fn published_frame_metadata(&self) -> Result<CaptureMetadata, OperationError> {
        self.capture_metadata_at(PUBLISHED_FRAME_PATH)?
            .ok_or(OperationError::UnsafeRemoteCapture)
    }

    fn fetch_published_frame(&self, destination: &Path) -> Result<(), OperationError> {
        self.transport
            .copy_from(&self.target, Path::new(PUBLISHED_FRAME_PATH), destination)
            .map_err(|_| OperationError::Transport)
    }
}

fn parse_bounded_json<T: for<'de> Deserialize<'de>>(
    input: &[u8],
    limit: usize,
) -> Result<T, OperationError> {
    if input.len() > limit {
        return Err(OperationError::MalformedFacts);
    }
    let mut deserializer = serde_json::Deserializer::from_slice(input);
    let value = T::deserialize(&mut deserializer).map_err(|_| OperationError::MalformedFacts)?;
    deserializer
        .end()
        .map_err(|_| OperationError::MalformedFacts)?;
    Ok(value)
}

pub trait CaptureClock {
    fn now(&self) -> Duration;
    fn sleep(&self, duration: Duration);
}

#[derive(Debug)]
pub struct SystemCaptureClock {
    started_at: Instant,
}

impl Default for SystemCaptureClock {
    fn default() -> Self {
        Self {
            started_at: Instant::now(),
        }
    }
}

impl CaptureClock for SystemCaptureClock {
    fn now(&self) -> Duration {
        self.started_at.elapsed()
    }

    fn sleep(&self, duration: Duration) {
        std::thread::sleep(duration);
    }
}

pub struct OperationsClient<'a, B, C> {
    backend: &'a B,
    clock: C,
}

impl<'a, B: OperationsBackend, C: CaptureClock> OperationsClient<'a, B, C> {
    pub fn new(backend: &'a B, clock: C) -> Self {
        Self { backend, clock }
    }

    pub fn doctor(&self) -> Result<DoctorReport, OperationError> {
        let facts = self.backend.diagnostic_facts()?;
        validate_facts(&facts)?;
        let diagnostics = evaluate(&facts);
        Ok(DoctorReport {
            schema_version: DOCTOR_SCHEMA_VERSION,
            healthy: diagnostics == [DiagnosticCode::Healthy],
            diagnostics,
            facts,
        })
    }

    pub fn status(&self) -> Result<StatusReport, OperationError> {
        let report = self.doctor()?;
        if let Some(diagnostic) = report
            .diagnostics
            .iter()
            .copied()
            .find(|diagnostic| *diagnostic != DiagnosticCode::Healthy)
        {
            return Err(OperationError::Unhealthy(diagnostic));
        }
        Ok(StatusReport {
            application_version: report.facts.installed_application.version,
            application_revision: report.facts.installed_application.source_commit,
            driver_version: report.facts.installed_driver.version,
            driver_revision: report.facts.installed_driver.source_commit,
            drm_mode: report.facts.drm_mode,
            renderer: report.facts.renderer,
        })
    }

    pub fn screenshot(
        &self,
        destination: &Path,
        timeout: Duration,
    ) -> Result<ScreenshotResult, OperationError> {
        let parent = prepare_local_destination(destination)?;
        let before = self.backend.debug_frame_metadata()?;
        if let Some(metadata) = before.as_ref() {
            validate_source_metadata(metadata)?;
        }

        self.backend.signal_debug_frame()?;
        let deadline = self.clock.now().saturating_add(timeout);
        loop {
            if let Some(metadata) = self.backend.debug_frame_metadata()? {
                validate_source_metadata(&metadata)?;
                if capture_is_fresh(before.as_ref(), &metadata) {
                    break;
                }
            }
            if self.clock.now() >= deadline {
                return Err(OperationError::CaptureTimedOut);
            }
            let remaining = deadline.saturating_sub(self.clock.now());
            self.clock.sleep(CAPTURE_POLL_INTERVAL.min(remaining));
        }

        let published = self.backend.publish_debug_frame()?;
        validate_published_metadata(&published)?;
        let temporary = NamedTempFile::new_in(&parent).map_err(|_| OperationError::LocalIo)?;
        self.backend
            .fetch_published_frame(temporary.path())
            .inspect_err(|_| {
                let _ = temporary.as_file().sync_all();
            })?;
        let after = self.backend.published_frame_metadata()?;
        validate_published_metadata(&after)?;
        if published != after {
            return Err(OperationError::RemoteCaptureChanged);
        }

        let bytes = read_bounded(temporary.path())?;
        let digest = sha256(&bytes);
        if digest != published.sha256 || bytes.len() as u64 != published.size {
            return Err(OperationError::RemoteCaptureChanged);
        }
        validate_png(&bytes)?;
        temporary
            .as_file()
            .sync_all()
            .map_err(|_| OperationError::LocalIo)?;
        temporary
            .persist(destination)
            .map_err(|_| OperationError::LocalIo)?;
        fs::File::open(&parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|_| OperationError::LocalIo)?;
        Ok(ScreenshotResult {
            destination: destination.to_owned(),
            sha256: digest,
        })
    }
}

fn evaluate(facts: &DiagnosticFacts) -> Vec<DiagnosticCode> {
    let mut diagnostics = Vec::new();
    if facts.target_model != facts.expected_target_model
        || facts.target_serial != facts.expected_target_serial
    {
        diagnostics.push(DiagnosticCode::TargetIdentityMismatch);
    }
    if !facts.target_model.starts_with("Raspberry Pi Zero 2 W Rev ")
        || !matches!(facts.target_os_id.as_str(), "debian" | "raspbian")
        || facts.target_os_version != "13"
        || facts.target_architecture != "arm64"
    {
        diagnostics.push(DiagnosticCode::TargetPlatformMismatch);
    }
    if facts.installed_application.version != facts.expected_application.version {
        diagnostics.push(DiagnosticCode::ApplicationVersionMismatch);
    }
    if facts.installed_application.source_commit != facts.expected_application.source_commit {
        diagnostics.push(DiagnosticCode::ApplicationRevisionMismatch);
    }
    if facts.installed_application.sha256 != facts.expected_application.sha256 {
        diagnostics.push(DiagnosticCode::ApplicationChecksumMismatch);
    }
    if facts.installed_driver.version != facts.expected_driver.version {
        diagnostics.push(DiagnosticCode::DriverVersionMismatch);
    }
    if facts.installed_driver.source_commit != facts.expected_driver.source_commit {
        diagnostics.push(DiagnosticCode::DriverRevisionMismatch);
    }
    if facts.installed_driver.sha256 != facts.expected_driver.sha256 {
        diagnostics.push(DiagnosticCode::DriverManifestMismatch);
    }
    if facts.running_kernel != facts.expected_kernel {
        diagnostics.push(DiagnosticCode::KernelMismatch);
    }
    if !facts.module_loaded
        || facts.module_name != "hyperpixel2r_kms"
        || facts.module_vermagic != facts.expected_module_vermagic
    {
        diagnostics.push(DiagnosticCode::ModuleMismatch);
    }
    if !facts.overlay_configured || facts.overlay_file != facts.expected_overlay_file {
        diagnostics.push(DiagnosticCode::OverlayMismatch);
    }
    if !facts.service_active {
        diagnostics.push(DiagnosticCode::ServiceInactive);
    }
    if facts.service_restart_count != 0 {
        diagnostics.push(DiagnosticCode::UnexpectedRestartCount);
    }
    if !facts.http_healthy {
        diagnostics.push(DiagnosticCode::HttpFailure);
    }
    if facts.touch_device.is_none() {
        diagnostics.push(DiagnosticCode::TouchMissing);
    }
    if facts.drm_mode != "480x480" {
        diagnostics.push(DiagnosticCode::DrmModeWrong);
    }
    if facts.renderer != "opengles2" {
        diagnostics.push(DiagnosticCode::RendererWrong);
    }
    if !facts.mdns_reachable {
        diagnostics.push(DiagnosticCode::MdnsFailure);
    }
    if diagnostics.is_empty() {
        diagnostics.push(DiagnosticCode::Healthy);
    }
    diagnostics
}

fn validate_facts(facts: &DiagnosticFacts) -> Result<(), OperationError> {
    for value in [
        &facts.target_model,
        &facts.target_serial,
        &facts.expected_target_model,
        &facts.expected_target_serial,
        &facts.target_os_id,
        &facts.target_os_version,
        &facts.target_architecture,
        &facts.installed_application.version,
        &facts.installed_application.source_commit,
        &facts.installed_application.sha256,
        &facts.expected_application.version,
        &facts.expected_application.source_commit,
        &facts.expected_application.sha256,
        &facts.installed_driver.version,
        &facts.installed_driver.source_commit,
        &facts.installed_driver.sha256,
        &facts.expected_driver.version,
        &facts.expected_driver.source_commit,
        &facts.expected_driver.sha256,
        &facts.running_kernel,
        &facts.expected_kernel,
        &facts.module_name,
        &facts.module_vermagic,
        &facts.expected_module_vermagic,
        &facts.overlay_file,
        &facts.expected_overlay_file,
        &facts.drm_device,
        &facts.drm_mode,
        &facts.renderer,
        &facts.mdns_hostname,
    ] {
        validate_field(value)?;
    }
    if let Some(touch) = &facts.touch_device {
        validate_field(touch)?;
    }
    for artifact in [
        &facts.installed_application,
        &facts.expected_application,
        &facts.installed_driver,
        &facts.expected_driver,
    ] {
        semver::Version::parse(&artifact.version).map_err(|_| OperationError::MalformedFacts)?;
        if !is_lower_hex(&artifact.source_commit, 40) || !is_lower_hex(&artifact.sha256, 64) {
            return Err(OperationError::MalformedFacts);
        }
    }
    Ok(())
}

fn validate_field(value: &str) -> Result<(), OperationError> {
    if value.is_empty()
        || value.len() > 512
        || !value.is_ascii()
        || value
            .bytes()
            .any(|byte| byte.is_ascii_control() || (!byte.is_ascii_graphic() && byte != b' '))
    {
        return Err(OperationError::MalformedFacts);
    }
    Ok(())
}

fn validate_source_metadata(metadata: &CaptureMetadata) -> Result<(), OperationError> {
    if !metadata.regular
        || metadata.symlink
        || metadata.links != 1
        || !matches!(metadata.mode, 0o600 | 0o640)
    {
        return Err(OperationError::UnsafeRemoteCapture);
    }
    validate_capture_size_and_digest(metadata)
}

fn validate_published_metadata(metadata: &CaptureMetadata) -> Result<(), OperationError> {
    if !metadata.regular
        || metadata.symlink
        || metadata.links != 1
        || metadata.uid != 0
        || metadata.gid != 0
        || metadata.mode != 0o600
    {
        return Err(OperationError::UnsafeRemoteCapture);
    }
    validate_capture_size_and_digest(metadata)
}

fn validate_capture_size_and_digest(metadata: &CaptureMetadata) -> Result<(), OperationError> {
    if metadata.size == 0 || metadata.size > MAX_CAPTURE_BYTES {
        return Err(OperationError::CaptureTooLarge);
    }
    if !is_lower_hex(&metadata.sha256, 64) {
        return Err(OperationError::UnsafeRemoteCapture);
    }
    Ok(())
}

fn capture_is_fresh(before: Option<&CaptureMetadata>, after: &CaptureMetadata) -> bool {
    before
        .is_none_or(|before| after.inode != before.inode && after.modified_ns >= before.modified_ns)
}

fn prepare_local_destination(destination: &Path) -> Result<PathBuf, OperationError> {
    if destination.as_os_str().is_empty()
        || destination.file_name().is_none()
        || destination
            .components()
            .any(|component| matches!(component, Component::ParentDir | Component::CurDir))
    {
        return Err(OperationError::UnsafeLocalDestination);
    }
    if let Ok(metadata) = fs::symlink_metadata(destination)
        && (metadata.file_type().is_symlink() || !metadata.is_file())
    {
        return Err(OperationError::UnsafeLocalDestination);
    }
    let parent = destination
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    ensure_safe_parent(parent)?;
    Ok(parent.to_owned())
}

fn ensure_safe_parent(parent: &Path) -> Result<(), OperationError> {
    reject_symlink_ancestors(parent)?;
    if parent.exists() {
        let metadata = fs::symlink_metadata(parent).map_err(|_| OperationError::LocalIo)?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(OperationError::UnsafeLocalDestination);
        }
        return Ok(());
    }
    let ancestor = parent
        .ancestors()
        .find(|candidate| candidate.exists())
        .ok_or(OperationError::UnsafeLocalDestination)?;
    let metadata = fs::symlink_metadata(ancestor).map_err(|_| OperationError::LocalIo)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(OperationError::UnsafeLocalDestination);
    }
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true).mode(0o700);
    builder
        .create(parent)
        .map_err(|_| OperationError::LocalIo)?;
    reject_symlink_ancestors(parent)?;
    Ok(())
}

fn reject_symlink_ancestors(path: &Path) -> Result<(), OperationError> {
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() {
                    if metadata.uid() != 0
                        || !fs::metadata(&current).is_ok_and(|target| target.is_dir())
                    {
                        return Err(OperationError::UnsafeLocalDestination);
                    }
                } else if !metadata.is_dir() {
                    return Err(OperationError::UnsafeLocalDestination);
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(OperationError::LocalIo),
        }
    }
    Ok(())
}

fn read_bounded(path: &Path) -> Result<Vec<u8>, OperationError> {
    let metadata = fs::symlink_metadata(path).map_err(|_| OperationError::LocalIo)?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.nlink() != 1
        || metadata.len() > MAX_CAPTURE_BYTES
    {
        return Err(OperationError::UnsafeLocalDestination);
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    fs::File::open(path)
        .map_err(|_| OperationError::LocalIo)?
        .take(MAX_CAPTURE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| OperationError::LocalIo)?;
    if bytes.len() as u64 > MAX_CAPTURE_BYTES {
        return Err(OperationError::CaptureTooLarge);
    }
    Ok(bytes)
}

fn validate_png(bytes: &[u8]) -> Result<(), OperationError> {
    let mut decoder = png::Decoder::new(Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::IDENTITY);
    let mut reader = decoder
        .read_info()
        .map_err(|_| OperationError::InvalidPng)?;
    if reader.info().width != EXPECTED_WIDTH || reader.info().height != EXPECTED_HEIGHT {
        return Err(OperationError::WrongPngDimensions);
    }
    if reader.info().color_type != png::ColorType::Rgba
        || reader.info().bit_depth != png::BitDepth::Eight
    {
        return Err(OperationError::WrongPngFormat);
    }
    let size = reader
        .output_buffer_size()
        .ok_or(OperationError::InvalidPng)?;
    if size != EXPECTED_WIDTH as usize * EXPECTED_HEIGHT as usize * 4 {
        return Err(OperationError::WrongPngFormat);
    }
    let mut output = vec![0; size];
    let frame = reader
        .next_frame(&mut output)
        .map_err(|_| OperationError::InvalidPng)?;
    if frame.buffer_size() != output.len() {
        return Err(OperationError::InvalidPng);
    }
    reader.finish().map_err(|_| OperationError::InvalidPng)?;
    Ok(())
}

fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn is_lower_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum OperationError {
    #[error("target diagnostic facts are malformed")]
    MalformedFacts,
    #[error("doctor output is invalid")]
    InvalidDoctorJson,
    #[error("doctor output exceeds its bounded size")]
    DoctorOutputTooLarge,
    #[error("target is unhealthy: {0:?}")]
    Unhealthy(DiagnosticCode),
    #[error("target operation transport failed")]
    Transport,
    #[error("remote debug capture is unsafe")]
    UnsafeRemoteCapture,
    #[error("remote debug capture is too large")]
    CaptureTooLarge,
    #[error("no fresh debug capture arrived before the deadline")]
    CaptureTimedOut,
    #[error("remote debug capture changed during transfer")]
    RemoteCaptureChanged,
    #[error("local screenshot destination is unsafe")]
    UnsafeLocalDestination,
    #[error("local screenshot I/O failed")]
    LocalIo,
    #[error("debug capture is not a valid PNG")]
    InvalidPng,
    #[error("debug capture is not 480 by 480 pixels")]
    WrongPngDimensions,
    #[error("debug capture is not 8-bit RGBA")]
    WrongPngFormat,
}

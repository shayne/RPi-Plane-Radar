use std::ffi::OsString;
use std::fmt;
use std::fs::File;
use std::io::{Cursor, Write};
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use nix::errno::Errno;
use nix::fcntl::{AtFlags, OFlag, open, openat, renameat};
use nix::sys::stat::{Mode, SFlag, fstat, fstatat, mkdirat};
use nix::unistd::{UnlinkatFlags, geteuid, unlinkat};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::config::DriverLock;
use crate::state::{ArtifactIdentity, TargetInstallState};
use crate::target::SshTarget;
use crate::transport::{RemoteCommand, Transport};

pub const DOCTOR_SCHEMA_VERSION: u32 = 1;
pub const MAX_DOCTOR_JSON_BYTES: usize = 32 * 1024;
pub const MAX_CAPTURE_BYTES: u64 = 8 * 1024 * 1024;
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
    DrmDeviceWrong,
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
    pub running_application_revision: String,
    pub installed_driver: ArtifactIdentity,
    pub persisted_driver_manifest_sha256: String,
    pub expected_driver: ArtifactIdentity,
    pub running_kernel: String,
    pub expected_kernel: String,
    pub module_name: String,
    pub module_loaded: bool,
    pub module_vermagic: String,
    pub expected_module_vermagic: String,
    pub module_sha256: String,
    pub expected_module_sha256: String,
    pub overlay_file: String,
    pub expected_overlay_file: String,
    pub overlay_sha256: String,
    pub expected_overlay_sha256: String,
    pub boot_config_sha256: String,
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CaptureTransfer {
    pub source: CaptureMetadata,
    pub published: CaptureMetadata,
    pub rechecked: CaptureMetadata,
    pub bytes: Vec<u8>,
}

pub trait OperationsBackend {
    fn diagnostic_facts(&self) -> Result<DiagnosticFacts, OperationError>;
    fn debug_frame_metadata(
        &self,
        timeout: Duration,
    ) -> Result<Option<CaptureMetadata>, OperationError>;
    fn signal_debug_frame(&self, timeout: Duration) -> Result<(), OperationError>;
    fn capture_debug_frame(
        &self,
        before: Option<&CaptureMetadata>,
        timeout: Duration,
    ) -> Result<CaptureTransfer, OperationError>;
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
const MAX_TARGET_STATE_BYTES: usize = 64 * 1024;
const MAX_PROBE_BYTES: usize = 32 * 1024;
const MAX_HEALTH_BYTES: usize = 4 * 1024;
const MAX_METADATA_BYTES: usize = 2 * 1024;
const MAX_CAPTURE_PROTOCOL_BYTES: usize = MAX_CAPTURE_BYTES as usize + (MAX_METADATA_BYTES * 2) + 8;

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
    r#"driver_version=$(field driver_version); driver_revision=$(field source_revision); driver_manifest_sha256=$(sha256sum -- "$manifest" | awk '{print $1}'); expected_kernel=$(field kernel_release); expected_module_vermagic=$(field module_vermagic); module_file=$(field module_file); expected_module_sha256=$(field module_sha256); expected_overlay_file=$(field overlay_file); expected_overlay_sha256=$(field overlay_sha256); "#,
    r#"test "$driver_version" = "$1"; test "$driver_revision" = "$2"; test "$expected_overlay_file" = "hyperpixel2r-kms-${2%${2#????????????}}.dtbo"; test "$module_file" = hyperpixel2r_kms.ko; "#,
    r#"running_kernel=$(uname -r); module_loaded=false; if awk '$1 == "hyperpixel2r_kms" { count++ } END { exit count != 1 }' /proc/modules; then module_loaded=true; fi; "#,
    r#"module_vermagic=$(/usr/sbin/modinfo -F vermagic hyperpixel2r_kms 2>/dev/null || printf unavailable); "#,
    r#"module_sha256=0000000000000000000000000000000000000000000000000000000000000000; module="/lib/modules/$expected_kernel/extra/$module_file"; if test ! -L "$module" && test -f "$module" && test "$(stat -c '%u:%g:%a' -- "$module")" = 0:0:644; then module_sha256=$(sha256sum -- "$module" | awk '{print $1}'); fi; "#,
    r#"overlay_sha256=0000000000000000000000000000000000000000000000000000000000000000; overlay="/boot/firmware/overlays/$expected_overlay_file"; if test ! -L "$overlay" && test -f "$overlay" && test "$(stat -c '%u:%g:%a' -- "$overlay")" = 0:0:644; then overlay_sha256=$(sha256sum -- "$overlay" | awk '{print $1}'); fi; "#,
    r#"config=/boot/firmware/config.txt; boot_config_sha256=0000000000000000000000000000000000000000000000000000000000000000; overlay_file=unavailable; overlay_configured=false; if test ! -L "$config" && test -f "$config" && test "$(stat -c '%u:%g:%a' -- "$config")" = 0:0:644; then boot_config_sha256=$(sha256sum -- "$config" | awk '{print $1}'); overlay_result=$(awk -v wanted="dtoverlay=$expected_overlay_file" '{ line=$0; sub(/^[[:space:]]+/, "", line); sub(/[[:space:]]+$/, "", line); if (line !~ /^dtoverlay=/) next; if (line == wanted) { count++; selected=line; next } if (line ~ /hyperpixel2r/) bad=1 } END { if (count == 1 && !bad) { sub(/^dtoverlay=/, "", selected); print "true:" selected } else print "false:unavailable" }' "$config"); overlay_configured=${overlay_result%%:*}; overlay_file=${overlay_result#*:}; fi; "#,
    r#"service_active=false; if systemctl is-active --quiet planeradar.service; then service_active=true; fi; service_restart_count=$(systemctl show planeradar.service --property=NRestarts --value); service_main_pid=$(systemctl show planeradar.service --property=MainPID --value); service_invocation=$(systemctl show planeradar.service --property=InvocationID --value); "#,
    r#"drm_device=unavailable; drm_mode=unavailable; renderer=unavailable; case "$service_main_pid:$service_invocation" in 0:*|*[!0-9]*:*|*:*[!0-9a-f]*) ;; *) card_count=0; for fd in /proc/"$service_main_pid"/fd/*; do target=$(readlink -- "$fd" 2>/dev/null || true); case "$target" in /dev/dri/card[0-9]*) if test "$drm_device" != unavailable && test "$drm_device" != "$target"; then card_count=2; break; fi; drm_device=$target; card_count=1;; esac; done; if test "$card_count" = 1 && test -c "$drm_device"; then card_name=${drm_device#/dev/dri/}; mode_file="/sys/class/drm/$card_name-DPI-1/modes"; drm_mode=$(sed -n '1p' "$mode_file" 2>/dev/null || true); test -n "$drm_mode" || drm_mode=unavailable; else drm_device=unavailable; fi; renderer=$(journalctl -b -u planeradar.service "_PID=$service_main_pid" "_SYSTEMD_INVOCATION_ID=$service_invocation" --no-pager -o cat 2>/dev/null | awk 'match($0, /render_driver=[^ ]+/) { value=substr($0, RSTART+14, RLENGTH-14); count++ } END { if (count == 1) print value }'); test -n "$renderer" || renderer=unavailable;; esac; "#,
    r#"touch_device=; touch_count=0; for name_file in /sys/class/input/event*/device/name; do test ! -L "$name_file" && test -f "$name_file" || continue; candidate=$(tr -d '\r\n' <"$name_file"); case "$candidate" in *HyperPixel*) touch_count=$((touch_count + 1)); touch_device=$candidate;; esac; done; test "$touch_count" -le 1; "#,
    r#"hostname=$(tr -d '\r\n' </etc/hostname); health_base64=; if health=$(curl --fail --silent --show-error --max-time 5 --max-filesize 4096 -H "Host: $hostname.local" http://127.0.0.1/healthz 2>/dev/null); then health_base64=$(printf %s "$health" | base64 -w0); fi; "#,
    r#"printf '{"schema_version":1,"os_id":"%s","os_version":"%s","architecture":"%s","application_version":"%s","application_revision":"%s","application_sha256":"%s","driver_version":"%s","driver_revision":"%s","driver_manifest_sha256":"%s","expected_kernel":"%s","running_kernel":"%s","module_loaded":%s,"module_vermagic":"%s","expected_module_vermagic":"%s","module_sha256":"%s","expected_module_sha256":"%s","overlay_file":"%s","expected_overlay_file":"%s","overlay_sha256":"%s","expected_overlay_sha256":"%s","boot_config_sha256":"%s","overlay_configured":%s,"drm_device":"%s","drm_mode":"%s","renderer":"%s","touch_device":"%s","service_active":%s,"service_restart_count":%s,"health_base64":"%s","hostname":"%s"}' "#,
    r#""$os_id" "$os_version" "$architecture" "$application_version" "$application_revision" "$application_sha256" "$driver_version" "$driver_revision" "$driver_manifest_sha256" "$expected_kernel" "$running_kernel" "$module_loaded" "$module_vermagic" "$expected_module_vermagic" "$module_sha256" "$expected_module_sha256" "$overlay_file" "$expected_overlay_file" "$overlay_sha256" "$expected_overlay_sha256" "$boot_config_sha256" "$overlay_configured" "$drm_device" "$drm_mode" "$renderer" "$touch_device" "$service_active" "$service_restart_count" "$health_base64" "$hostname""#,
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
    driver_version: String,
    driver_revision: String,
    driver_manifest_sha256: String,
    expected_kernel: String,
    running_kernel: String,
    module_loaded: bool,
    module_vermagic: String,
    expected_module_vermagic: String,
    module_sha256: String,
    expected_module_sha256: String,
    overlay_file: String,
    expected_overlay_file: String,
    overlay_sha256: String,
    expected_overlay_sha256: String,
    boot_config_sha256: String,
    overlay_configured: bool,
    drm_device: String,
    drm_mode: String,
    renderer: String,
    touch_device: String,
    service_active: bool,
    service_restart_count: u64,
    health_base64: String,
    hostname: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HealthProbe {
    configured: bool,
    state: String,
    data_stale: bool,
    revision: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CaptureProtocolHeader {
    schema_version: u32,
    source: CaptureMetadata,
    published: CaptureMetadata,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CaptureProtocolFooter {
    schema_version: u32,
    rechecked: CaptureMetadata,
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

    fn run_bounded(
        &self,
        request: RemoteCommand,
        timeout: Duration,
        stdout_limit: usize,
    ) -> Result<Vec<u8>, OperationError> {
        self.transport
            .run_bounded(&self.target, request, timeout, stdout_limit)
            .map(|output| output.stdout().to_vec())
            .map_err(|_| OperationError::Transport)
    }

    fn target_state(&self) -> Result<TargetInstallState, OperationError> {
        let request =
            RemoteCommand::ordinary(TARGET_STATE_COMMAND).map_err(|_| OperationError::Transport)?;
        let output =
            self.run_bounded(request, Duration::from_secs(10), MAX_TARGET_STATE_BYTES + 1)?;
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
        let output = self.run_bounded(request, Duration::from_secs(15), MAX_PROBE_BYTES + 1)?;
        parse_bounded_json(&output, MAX_PROBE_BYTES)
    }

    fn capture_metadata_at(
        &self,
        timeout: Duration,
    ) -> Result<Option<CaptureMetadata>, OperationError> {
        let request = RemoteCommand::ordinary([
            "/usr/bin/timeout",
            "10",
            "sudo",
            "-n",
            "/opt/planeradar/bin/planeradar",
            "capture-metadata",
        ])
        .map_err(|_| OperationError::Transport)?;
        let output = self.run_bounded(request, timeout, MAX_METADATA_BYTES + 1)?;
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
        let persisted_driver = state.driver.ok_or(OperationError::MalformedFacts)?;
        let probe = self.diagnostic_probe(&persisted_driver)?;
        if probe.schema_version != DOCTOR_SCHEMA_VERSION {
            return Err(OperationError::MalformedFacts);
        }
        let health = parse_health_probe(&probe.health_base64)?;
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
            running_application_revision: health
                .as_ref()
                .map_or_else(|| "0".repeat(40), |health| health.revision.clone()),
            installed_driver: ArtifactIdentity {
                version: probe.driver_version,
                source_commit: probe.driver_revision,
                sha256: probe.driver_manifest_sha256,
            },
            persisted_driver_manifest_sha256: persisted_driver.sha256,
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
            module_sha256: probe.module_sha256,
            expected_module_sha256: probe.expected_module_sha256,
            overlay_file: probe.overlay_file,
            expected_overlay_file: probe.expected_overlay_file,
            overlay_sha256: probe.overlay_sha256,
            expected_overlay_sha256: probe.expected_overlay_sha256,
            boot_config_sha256: probe.boot_config_sha256,
            overlay_configured: probe.overlay_configured,
            drm_device: probe.drm_device,
            drm_mode: probe.drm_mode,
            renderer: probe.renderer,
            touch_device: (!probe.touch_device.is_empty()).then_some(probe.touch_device),
            service_active: probe.service_active,
            service_restart_count: probe.service_restart_count,
            http_healthy: health.is_some(),
            mdns_hostname,
            mdns_reachable,
            settings_configured: health.is_some_and(|health| health.configured),
        })
    }

    fn debug_frame_metadata(
        &self,
        timeout: Duration,
    ) -> Result<Option<CaptureMetadata>, OperationError> {
        self.capture_metadata_at(timeout)
    }

    fn signal_debug_frame(&self, timeout: Duration) -> Result<(), OperationError> {
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
        self.run_bounded(request, timeout, 1).map(|_| ())
    }

    fn capture_debug_frame(
        &self,
        before: Option<&CaptureMetadata>,
        timeout: Duration,
    ) -> Result<CaptureTransfer, OperationError> {
        let before = before
            .map(serde_json::to_string)
            .transpose()
            .map_err(|_| OperationError::UnsafeRemoteCapture)?
            .unwrap_or_else(|| "none".to_owned());
        let timeout_ms = u64::try_from(timeout.as_millis())
            .unwrap_or(30_000)
            .clamp(1, 30_000)
            .to_string();
        let remote_timeout_seconds = timeout
            .as_secs()
            .saturating_add(u64::from(timeout.subsec_nanos() > 0))
            .clamp(1, 30)
            .to_string();
        let request = RemoteCommand::ordinary([
            "/usr/bin/timeout",
            &remote_timeout_seconds,
            "sudo",
            "-n",
            "/opt/planeradar/bin/planeradar",
            "capture-snapshot",
            "--before",
            &before,
            "--timeout-ms",
            &timeout_ms,
        ])
        .map_err(|_| OperationError::Transport)?;
        let output = self.run_bounded(request, timeout, MAX_CAPTURE_PROTOCOL_BYTES)?;
        parse_capture_protocol(&output)
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

fn parse_health_probe(encoded: &str) -> Result<Option<HealthProbe>, OperationError> {
    if encoded.is_empty() {
        return Ok(None);
    }
    if encoded.len() > MAX_HEALTH_BYTES.saturating_mul(2) {
        return Err(OperationError::MalformedFacts);
    }
    let bytes = STANDARD
        .decode(encoded)
        .map_err(|_| OperationError::MalformedFacts)?;
    if bytes.len() > MAX_HEALTH_BYTES {
        return Err(OperationError::MalformedFacts);
    }
    let health: HealthProbe = parse_bounded_json(&bytes, MAX_HEALTH_BYTES)?;
    if !matches!(
        health.state.as_str(),
        "SETUP_REQUIRED" | "WAITING_FOR_NETWORK" | "RADAR" | "SETTINGS"
    ) || !is_lower_hex(&health.revision, 40)
    {
        return Err(OperationError::MalformedFacts);
    }
    let _ = health.data_stale;
    Ok(Some(health))
}

fn parse_capture_protocol(input: &[u8]) -> Result<CaptureTransfer, OperationError> {
    if input.len() > MAX_CAPTURE_PROTOCOL_BYTES {
        return Err(OperationError::CaptureTooLarge);
    }
    let (header_bytes, rest) = take_protocol_json(input)?;
    let header: CaptureProtocolHeader = parse_bounded_json(header_bytes, MAX_METADATA_BYTES)?;
    if header.schema_version != DOCTOR_SCHEMA_VERSION {
        return Err(OperationError::UnsafeRemoteCapture);
    }
    validate_source_metadata(&header.source)?;
    validate_published_metadata(&header.published)?;
    let size =
        usize::try_from(header.published.size).map_err(|_| OperationError::CaptureTooLarge)?;
    if rest.len() < size {
        return Err(OperationError::RemoteCaptureChanged);
    }
    let (bytes, footer_input) = rest.split_at(size);
    let (footer_bytes, trailing) = take_protocol_json(footer_input)?;
    if !trailing.is_empty() {
        return Err(OperationError::RemoteCaptureChanged);
    }
    let footer: CaptureProtocolFooter = parse_bounded_json(footer_bytes, MAX_METADATA_BYTES)?;
    if footer.schema_version != DOCTOR_SCHEMA_VERSION {
        return Err(OperationError::UnsafeRemoteCapture);
    }
    validate_published_metadata(&footer.rechecked)?;
    if header.published != footer.rechecked
        || header.source.sha256 != header.published.sha256
        || header.source.size != header.published.size
        || sha256(bytes) != header.published.sha256
    {
        return Err(OperationError::RemoteCaptureChanged);
    }
    Ok(CaptureTransfer {
        source: header.source,
        published: header.published,
        rechecked: footer.rechecked,
        bytes: bytes.to_vec(),
    })
}

fn take_protocol_json(input: &[u8]) -> Result<(&[u8], &[u8]), OperationError> {
    let length: [u8; 4] = input
        .get(..4)
        .ok_or(OperationError::RemoteCaptureChanged)?
        .try_into()
        .map_err(|_| OperationError::RemoteCaptureChanged)?;
    let length = u32::from_be_bytes(length) as usize;
    if length == 0 || length > MAX_METADATA_BYTES || input.len() < 4 + length {
        return Err(OperationError::RemoteCaptureChanged);
    }
    Ok((&input[4..4 + length], &input[4 + length..]))
}

pub trait CaptureClock {
    fn now(&self) -> Duration;
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
        if timeout.is_zero() {
            return Err(OperationError::CaptureTimedOut);
        }
        let deadline = self.clock.now().saturating_add(timeout);
        let local = PinnedDestination::prepare(destination)?;
        let before = self
            .backend
            .debug_frame_metadata(remaining(&self.clock, deadline)?)?;
        if let Some(metadata) = before.as_ref() {
            validate_source_metadata(metadata)?;
        }

        self.backend
            .signal_debug_frame(remaining(&self.clock, deadline)?)?;
        let capture = self
            .backend
            .capture_debug_frame(before.as_ref(), remaining(&self.clock, deadline)?)?;
        remaining(&self.clock, deadline)?;
        validate_source_metadata(&capture.source)?;
        validate_published_metadata(&capture.published)?;
        validate_published_metadata(&capture.rechecked)?;
        if !capture_is_fresh(before.as_ref(), &capture.source)
            || capture.published != capture.rechecked
            || capture.source.sha256 != capture.published.sha256
            || capture.source.size != capture.published.size
        {
            return Err(OperationError::RemoteCaptureChanged);
        }

        let digest = sha256(&capture.bytes);
        if digest != capture.published.sha256
            || capture.bytes.len() as u64 != capture.published.size
        {
            return Err(OperationError::RemoteCaptureChanged);
        }
        validate_png(&capture.bytes)?;
        remaining(&self.clock, deadline)?;
        local.persist(&capture.bytes, &self.clock, deadline)?;
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
    if facts.installed_application.source_commit != facts.expected_application.source_commit
        || facts.running_application_revision != facts.installed_application.source_commit
        || facts.running_application_revision != facts.expected_application.source_commit
    {
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
    if facts.installed_driver.sha256 != facts.persisted_driver_manifest_sha256
        || facts.installed_driver.sha256 != facts.expected_driver.sha256
    {
        diagnostics.push(DiagnosticCode::DriverManifestMismatch);
    }
    if facts.running_kernel != facts.expected_kernel {
        diagnostics.push(DiagnosticCode::KernelMismatch);
    }
    if !facts.module_loaded
        || facts.module_name != "hyperpixel2r_kms"
        || facts.module_vermagic != facts.expected_module_vermagic
        || facts.module_sha256 != facts.expected_module_sha256
    {
        diagnostics.push(DiagnosticCode::ModuleMismatch);
    }
    if !facts.overlay_configured
        || facts.overlay_file != facts.expected_overlay_file
        || facts.overlay_sha256 != facts.expected_overlay_sha256
    {
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
    if facts.drm_device != "/dev/dri/card0" {
        diagnostics.push(DiagnosticCode::DrmDeviceWrong);
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
        &facts.running_application_revision,
        &facts.installed_driver.version,
        &facts.installed_driver.source_commit,
        &facts.installed_driver.sha256,
        &facts.persisted_driver_manifest_sha256,
        &facts.expected_driver.version,
        &facts.expected_driver.source_commit,
        &facts.expected_driver.sha256,
        &facts.running_kernel,
        &facts.expected_kernel,
        &facts.module_name,
        &facts.module_vermagic,
        &facts.expected_module_vermagic,
        &facts.module_sha256,
        &facts.expected_module_sha256,
        &facts.overlay_file,
        &facts.expected_overlay_file,
        &facts.overlay_sha256,
        &facts.expected_overlay_sha256,
        &facts.boot_config_sha256,
        &facts.persisted_driver_manifest_sha256,
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
    for digest in [
        &facts.module_sha256,
        &facts.expected_module_sha256,
        &facts.overlay_sha256,
        &facts.expected_overlay_sha256,
        &facts.boot_config_sha256,
    ] {
        if !is_lower_hex(digest, 64) {
            return Err(OperationError::MalformedFacts);
        }
    }
    if !is_lower_hex(&facts.running_application_revision, 40) {
        return Err(OperationError::MalformedFacts);
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

struct PinnedDestination {
    directory: File,
    parent_path: PathBuf,
    directory_device: i128,
    directory_inode: i128,
    file_name: OsString,
}

impl PinnedDestination {
    fn prepare(destination: &Path) -> Result<Self, OperationError> {
        let (directory, parent_path, file_name) = open_destination_parent(destination, true)?;
        validate_final_entry(&directory, &file_name)?;
        let identity = fstat(&directory).map_err(|_| OperationError::LocalIo)?;
        Ok(Self {
            directory,
            parent_path,
            directory_device: i128::from(identity.st_dev),
            directory_inode: i128::from(identity.st_ino),
            file_name,
        })
    }

    fn persist<C: CaptureClock>(
        &self,
        bytes: &[u8],
        clock: &C,
        deadline: Duration,
    ) -> Result<(), OperationError> {
        self.verify_parent_identity()?;
        remaining(clock, deadline)?;
        let mut random = rand::rng();
        let mut temporary = None;
        for _ in 0..16 {
            let name = format!(".planeradar-capture-{:016x}", random.next_u64());
            match openat(
                &self.directory,
                name.as_str(),
                OFlag::O_CREAT
                    | OFlag::O_EXCL
                    | OFlag::O_RDWR
                    | OFlag::O_CLOEXEC
                    | OFlag::O_NOFOLLOW,
                Mode::from_bits_truncate(0o600),
            ) {
                Ok(file) => {
                    temporary = Some((name, File::from(file)));
                    break;
                }
                Err(Errno::EEXIST) => continue,
                Err(_) => return Err(OperationError::LocalIo),
            }
        }
        let (temporary_name, mut temporary_file) = temporary.ok_or(OperationError::LocalIo)?;
        let result = (|| {
            temporary_file
                .write_all(bytes)
                .and_then(|()| temporary_file.sync_all())
                .map_err(|_| OperationError::LocalIo)?;
            let metadata = fstat(&temporary_file).map_err(|_| OperationError::LocalIo)?;
            if SFlag::from_bits_truncate(metadata.st_mode) != SFlag::S_IFREG
                || metadata.st_nlink != 1
                || metadata.st_size != bytes.len() as i64
                || metadata.st_mode & 0o777 != 0o600
                || metadata.st_uid != geteuid().as_raw()
            {
                return Err(OperationError::UnsafeLocalDestination);
            }
            remaining(clock, deadline)?;
            self.verify_parent_identity()?;
            validate_final_entry(&self.directory, &self.file_name)?;
            self.directory
                .sync_all()
                .map_err(|_| OperationError::LocalIo)?;
            remaining(clock, deadline)?;
            // The final parent is owned by the invoking uid; another process
            // with that same uid already has authority to replace the final
            // destination after success. Bind our random entry to its open fd
            // with no callback or other fallible work between this check and
            // the atomic rename.
            validate_temporary_entry(&self.directory, &temporary_name, &temporary_file)?;
            renameat(
                &self.directory,
                temporary_name.as_str(),
                &self.directory,
                Path::new(&self.file_name),
            )
            .map_err(|_| OperationError::LocalIo)
        })();
        if result.is_err() {
            let _ = unlinkat(
                &self.directory,
                temporary_name.as_str(),
                UnlinkatFlags::NoRemoveDir,
            );
        }
        result
    }

    fn verify_parent_identity(&self) -> Result<(), OperationError> {
        let destination = if self.parent_path == Path::new(".") {
            PathBuf::from(&self.file_name)
        } else {
            self.parent_path.join(&self.file_name)
        };
        let (directory, _, file_name) = open_destination_parent(&destination, false)?;
        let identity = fstat(&directory).map_err(|_| OperationError::LocalIo)?;
        if i128::from(identity.st_dev) != self.directory_device
            || i128::from(identity.st_ino) != self.directory_inode
            || file_name != self.file_name
        {
            return Err(OperationError::UnsafeLocalDestination);
        }
        Ok(())
    }
}

fn open_destination_parent(
    destination: &Path,
    create: bool,
) -> Result<(File, PathBuf, OsString), OperationError> {
    if destination.as_os_str().is_empty()
        || destination
            .components()
            .any(|component| matches!(component, Component::ParentDir | Component::CurDir))
    {
        return Err(OperationError::UnsafeLocalDestination);
    }
    let file_name = match destination.components().next_back() {
        Some(Component::Normal(name)) if !name.as_bytes().contains(&0) => name.to_os_string(),
        _ => return Err(OperationError::UnsafeLocalDestination),
    };
    let parent_path = destination
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
        .to_owned();
    let mut directory = if destination.is_absolute() {
        File::from(
            open(
                Path::new("/"),
                OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_CLOEXEC | OFlag::O_NOFOLLOW,
                Mode::empty(),
            )
            .map_err(|_| OperationError::UnsafeLocalDestination)?,
        )
    } else {
        File::from(
            open(
                Path::new("."),
                OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_CLOEXEC | OFlag::O_NOFOLLOW,
                Mode::empty(),
            )
            .map_err(|_| OperationError::UnsafeLocalDestination)?,
        )
    };
    validate_open_directory(&directory, false)?;
    for component in parent_path.components() {
        let name = match component {
            Component::RootDir | Component::CurDir => continue,
            Component::Normal(name) => name,
            _ => return Err(OperationError::UnsafeLocalDestination),
        };
        let flags = OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_CLOEXEC | OFlag::O_NOFOLLOW;
        let next = match openat(&directory, Path::new(name), flags, Mode::empty()) {
            Ok(next) => next,
            Err(Errno::ENOENT) if create => {
                mkdirat(&directory, Path::new(name), Mode::from_bits_truncate(0o700))
                    .map_err(|_| OperationError::LocalIo)?;
                openat(&directory, Path::new(name), flags, Mode::empty())
                    .map_err(|_| OperationError::UnsafeLocalDestination)?
            }
            Err(_) => return Err(OperationError::UnsafeLocalDestination),
        };
        directory = File::from(next);
        validate_open_directory(&directory, false)?;
    }
    validate_open_directory(&directory, true)?;
    Ok((directory, parent_path, file_name))
}

fn validate_open_directory(
    directory: &File,
    require_effective_user_owner: bool,
) -> Result<(), OperationError> {
    let metadata = fstat(directory).map_err(|_| OperationError::UnsafeLocalDestination)?;
    let effective_uid = geteuid().as_raw();
    let permissions = metadata.st_mode & 0o7777;
    let owner_is_trusted = metadata.st_uid == 0 || metadata.st_uid == effective_uid;
    let sticky_root_directory =
        metadata.st_uid == 0 && permissions & libc::S_ISVTX as libc::mode_t != 0;
    if SFlag::from_bits_truncate(metadata.st_mode) != SFlag::S_IFDIR
        || !owner_is_trusted
        || permissions & 0o700 != 0o700
        || (permissions & 0o022 != 0 && !sticky_root_directory)
        || (require_effective_user_owner && metadata.st_uid != effective_uid)
    {
        return Err(OperationError::UnsafeLocalDestination);
    }
    Ok(())
}

fn validate_temporary_entry(
    directory: &File,
    temporary_name: &str,
    temporary_file: &File,
) -> Result<(), OperationError> {
    let opened = fstat(temporary_file).map_err(|_| OperationError::LocalIo)?;
    let entry = fstatat(directory, temporary_name, AtFlags::AT_SYMLINK_NOFOLLOW)
        .map_err(|_| OperationError::UnsafeLocalDestination)?;
    if SFlag::from_bits_truncate(opened.st_mode) != SFlag::S_IFREG
        || opened.st_dev != entry.st_dev
        || opened.st_ino != entry.st_ino
        || opened.st_mode != entry.st_mode
        || opened.st_uid != entry.st_uid
        || opened.st_gid != entry.st_gid
        || opened.st_nlink != entry.st_nlink
        || opened.st_size != entry.st_size
        || opened.st_uid != geteuid().as_raw()
        || opened.st_nlink != 1
        || opened.st_mode & 0o777 != 0o600
    {
        return Err(OperationError::UnsafeLocalDestination);
    }
    Ok(())
}

fn validate_final_entry(directory: &File, file_name: &OsString) -> Result<(), OperationError> {
    match fstatat(
        directory,
        Path::new(file_name),
        AtFlags::AT_SYMLINK_NOFOLLOW,
    ) {
        Ok(metadata) => {
            if SFlag::from_bits_truncate(metadata.st_mode) != SFlag::S_IFREG
                || metadata.st_nlink != 1
            {
                return Err(OperationError::UnsafeLocalDestination);
            }
        }
        Err(Errno::ENOENT) => {}
        Err(_) => return Err(OperationError::LocalIo),
    }
    Ok(())
}

fn remaining<C: CaptureClock>(clock: &C, deadline: Duration) -> Result<Duration, OperationError> {
    let remaining = deadline.saturating_sub(clock.now());
    if remaining.is_zero() {
        Err(OperationError::CaptureTimedOut)
    } else {
        Ok(remaining)
    }
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

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
use crate::state::{
    ArtifactIdentity, InstallPhase, OwnedFile, TargetHardwareIdentity, TargetInstallState,
};
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
    pub accepted_driver_manifest_sha256: String,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub normal_kernel_file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub normal_kernel_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub normal_initramfs_file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub normal_initramfs_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_dtb_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vc4_overlay_sha256: Option<String>,
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
    r#"application_version=$3; "#,
    r#"application_revision=$(tr -d '\r\n' </opt/planeradar/REVISION); application_sha256=$(sha256sum -- "$app" | awk '{print $1}'); "#,
    r#"driver_root="/usr/lib/hyperpixel2r-kms/$1/$2"; test ! -L "$driver_root" && test -d "$driver_root"; "#,
    r#"kernel_count=0; driver_dir=; for candidate in "$driver_root"/*; do test ! -L "$candidate" && test -d "$candidate" || continue; kernel_count=$((kernel_count + 1)); driver_dir=$candidate; done; test "$kernel_count" = 1; "#,
    r#"regular() { test ! -L "$1" && test -f "$1" && test "$(stat -c '%u:%g:%a:%h' -- "$1")" = "0:0:$2:1"; }; boot_regular() { test ! -L "$1" && test -f "$1" && test "$(stat -c '%u:%g:%h' -- "$1")" = 0:0:1; case "$(stat -c '%a' -- "$1")" in 644|755) ;; *) return 1;; esac; }; digest() { test "$(sha256sum -- "$1" | awk '{print $1}')" = "$2"; }; absent() { test ! -L "$1" && test ! -e "$1"; }; valid_sha() { candidate=$1; case "$candidate" in *[!0-9a-f]*|'') return 1;; esac; test "${#candidate}" = 64; }; valid_revision() { candidate=$1; case "$candidate" in *[!0-9a-f]*|'') return 1;; esac; test "${#candidate}" = 40; }; manifest="$driver_dir/manifest.txt"; regular "$manifest" 644; "#,
    r#"field() { awk -F '\t' -v key="$1" '$1 == key { if (seen++) exit 2; value=$2 } END { if (!seen || value == "") exit 1; print value }' "$manifest"; }; "#,
    r#"test "$(awk -F '\t' 'NF != 2 || $1 == "" || $2 == "" || seen[$1]++ { bad=1 } END { print NR ":" bad+0 }' "$manifest")" = 17:0; for key in schema_version driver_version source_revision source_tree kernel_release architecture base_dtb_sha256 capability module_file module_sha256 module_vermagic overlay_file overlay_sha256 applied_dtb_file applied_dtb_sha256 backlight_rule_file backlight_rule_sha256; do test "$(awk -F '\t' -v key="$key" '$1 == key { count++ } END { print count+0 }' "$manifest")" = 1; done; driver_version=$(field driver_version); driver_revision=$(field source_revision); driver_manifest_sha256=$(sha256sum -- "$manifest" | awk '{print $1}'); expected_kernel=$(field kernel_release); expected_module_vermagic=$(field module_vermagic); module_file=$(field module_file); expected_module_sha256=$(field module_sha256); expected_overlay_file=$(field overlay_file); expected_overlay_sha256=$(field overlay_sha256); applied_dtb_file=$(field applied_dtb_file); expected_applied_dtb_sha256=$(field applied_dtb_sha256); backlight_rule_file=$(field backlight_rule_file); expected_backlight_rule_sha256=$(field backlight_rule_sha256); "#,
    r#"test "$(field schema_version)" = 2; test "$(field architecture)" = aarch64; test "$(field capability)" = pwm-backlight-v1; test "$driver_version" = "$1"; test "$driver_revision" = "$2"; valid_revision "$(field source_tree)"; valid_sha "$(field base_dtb_sha256)"; test "$expected_overlay_file" = "hyperpixel2r-kms-${2%${2#????????????}}.dtbo"; test "$module_file" = hyperpixel2r_kms.ko; test "$applied_dtb_file" = hyperpixel2r-kms-applied.dtb; test "$backlight_rule_file" = 70-planeradar-backlight.rules; for value in "$expected_module_sha256" "$expected_overlay_sha256" "$expected_applied_dtb_sha256" "$expected_backlight_rule_sha256"; do valid_sha "$value"; done; test "$expected_backlight_rule_sha256" = 579166d62b6444ea3b289b62d68a9c41ee3120262e2e2bca18967550e8423c11; regular "$driver_dir/$module_file" 644; digest "$driver_dir/$module_file" "$expected_module_sha256"; regular "$driver_dir/$expected_overlay_file" 644; digest "$driver_dir/$expected_overlay_file" "$expected_overlay_sha256"; regular "$driver_dir/$applied_dtb_file" 644; digest "$driver_dir/$applied_dtb_file" "$expected_applied_dtb_sha256"; regular "$driver_dir/$backlight_rule_file" 644; digest "$driver_dir/$backlight_rule_file" "$expected_backlight_rule_sha256"; installed_rule="/etc/udev/rules.d/$backlight_rule_file"; regular "$installed_rule" 644; digest "$installed_rule" "$expected_backlight_rule_sha256"; "#,
    r#"accepted_root=/var/lib/hyperpixel2r-kms; accepted=$accepted_root/accepted-state; accepted_stock=$accepted_root/accepted-stock-config.txt; accepted_prior_rule=$accepted_root/accepted-prior-backlight-rule; config=/boot/firmware/config.txt; regular "$accepted" 600; regular "$accepted_stock" 600; boot_regular "$config"; accepted_field() { awk -F= -v key="$1" '$1 == key { if (seen++) exit 2; value=$2 } END { if (!seen || value == "") exit 1; print value }' "$accepted"; }; accepted_schema=$(accepted_field schema_version); accepted_shape=$(awk -F= 'NF != 2 || $1 == "" || $2 == "" || seen[$1]++ { bad=1 } END { print NR ":" bad+0 }' "$accepted"); accepted_keys='schema_version driver_version source_revision kernel_release manifest_sha256 module_file module_sha256 overlay_file overlay_sha256 normal_config_sha256 stock_config_sha256 prior_dkms_inventory_sha256 backlight_rule_file backlight_rule_sha256 prior_backlight_rule_existed prior_backlight_rule_sha256'; case "$accepted_schema:$accepted_shape" in 3:16:0) ;; 4:22:0) accepted_keys="$accepted_keys normal_kernel_file normal_kernel_sha256 normal_initramfs_file normal_initramfs_sha256 base_dtb_sha256 vc4_overlay_sha256";; *) exit 1;; esac; for key in $accepted_keys; do test "$(awk -F= -v key="$key" '$1 == key { count++ } END { print count+0 }' "$accepted")" = 1; done; accepted_driver_manifest_sha256=$(accepted_field manifest_sha256); test "$(accepted_field driver_version)" = "$driver_version"; test "$(accepted_field source_revision)" = "$driver_revision"; test "$(accepted_field kernel_release)" = "$expected_kernel"; test "$(accepted_field module_file)" = "$module_file"; test "$(accepted_field module_sha256)" = "$expected_module_sha256"; test "$(accepted_field overlay_file)" = "$expected_overlay_file"; test "$(accepted_field overlay_sha256)" = "$expected_overlay_sha256"; test "$(accepted_field backlight_rule_file)" = "$backlight_rule_file"; test "$(accepted_field backlight_rule_sha256)" = "$expected_backlight_rule_sha256"; test "$accepted_driver_manifest_sha256" = "$driver_manifest_sha256"; for key in manifest_sha256 module_sha256 overlay_sha256 normal_config_sha256 stock_config_sha256 prior_dkms_inventory_sha256 backlight_rule_sha256; do valid_sha "$(accepted_field "$key")"; done; digest "$config" "$(accepted_field normal_config_sha256)"; digest "$accepted_stock" "$(accepted_field stock_config_sha256)"; marker=$driver_dir/dkms-prior-state; regular "$marker" 600; digest "$marker" "$(accepted_field prior_dkms_inventory_sha256)"; case "$(accepted_field prior_backlight_rule_existed)" in true) valid_sha "$(accepted_field prior_backlight_rule_sha256)"; regular "$accepted_prior_rule" 600; digest "$accepted_prior_rule" "$(accepted_field prior_backlight_rule_sha256)";; false) test "$(accepted_field prior_backlight_rule_sha256)" = none; absent "$accepted_prior_rule";; *) exit 1;; esac; prior_tryboot=$driver_dir/prior-tryboot.txt; live_tryboot=/boot/firmware/tryboot.txt; if test -L "$prior_tryboot" || test -L "$live_tryboot"; then exit 1; elif test -e "$prior_tryboot"; then regular "$prior_tryboot" 600; boot_regular "$live_tryboot"; digest "$live_tryboot" "$(sha256sum -- "$prior_tryboot" | awk '{print $1}')"; else absent "$prior_tryboot"; absent "$live_tryboot"; fi; normal_kernel_file=; normal_kernel_sha256=; normal_initramfs_file=; normal_initramfs_sha256=; base_dtb_sha256=; vc4_overlay_sha256=; if test "$accepted_schema" = 4; then normal_kernel_file=$(accepted_field normal_kernel_file); test "$normal_kernel_file" = kernel8.img; normal_kernel_sha256=$(accepted_field normal_kernel_sha256); normal_initramfs_file=$(accepted_field normal_initramfs_file); test "$normal_initramfs_file" = initramfs8; normal_initramfs_sha256=$(accepted_field normal_initramfs_sha256); base_dtb_sha256=$(accepted_field base_dtb_sha256); vc4_overlay_sha256=$(accepted_field vc4_overlay_sha256); for value in "$normal_kernel_sha256" "$normal_initramfs_sha256" "$base_dtb_sha256" "$vc4_overlay_sha256"; do valid_sha "$value"; done; normal_kernel=/boot/firmware/kernel8.img; normal_initramfs=/boot/firmware/initramfs8; active_base_dtb=/boot/firmware/bcm2710-rpi-zero-2-w.dtb; active_vc4_overlay=/boot/firmware/overlays/vc4-kms-v3d.dtbo; for file in "$normal_kernel" "$normal_initramfs" "$active_base_dtb" "$active_vc4_overlay"; do boot_regular "$file"; done; digest "$normal_kernel" "$normal_kernel_sha256"; digest "$normal_initramfs" "$normal_initramfs_sha256"; digest "$active_base_dtb" "$base_dtb_sha256"; digest "$active_vc4_overlay" "$vc4_overlay_sha256"; fi; "#,
    r#"running_kernel=$(uname -r); module_loaded=false; if awk '$1 == "hyperpixel2r_kms" { count++ } END { exit count != 1 }' /proc/modules; then module_loaded=true; fi; "#,
    r#"module_vermagic=$(/usr/sbin/modinfo -F vermagic hyperpixel2r_kms 2>/dev/null || printf unavailable); "#,
    r#"module_sha256=0000000000000000000000000000000000000000000000000000000000000000; module="/lib/modules/$expected_kernel/extra/$module_file"; if test ! -L "$module" && test -f "$module" && test "$(stat -c '%u:%g:%a' -- "$module")" = 0:0:644; then module_sha256=$(sha256sum -- "$module" | awk '{print $1}'); fi; "#,
    r#"overlay_sha256=0000000000000000000000000000000000000000000000000000000000000000; overlay="/boot/firmware/overlays/$expected_overlay_file"; if boot_regular "$overlay"; then overlay_sha256=$(sha256sum -- "$overlay" | awk '{print $1}'); fi; "#,
    r#"config=/boot/firmware/config.txt; boot_config_sha256=0000000000000000000000000000000000000000000000000000000000000000; overlay_file=unavailable; overlay_configured=false; if boot_regular "$config"; then boot_config_sha256=$(sha256sum -- "$config" | awk '{print $1}'); overlay_result=$(awk -v wanted="dtoverlay=$expected_overlay_file" '{ line=$0; sub(/^[[:space:]]+/, "", line); sub(/[[:space:]]+$/, "", line); if (line !~ /^dtoverlay=/) next; if (line == wanted) { count++; selected=line; next } if (line ~ /hyperpixel2r/) bad=1 } END { if (count == 1 && !bad) { sub(/^dtoverlay=/, "", selected); print "true:" selected } else print "false:unavailable" }' "$config"); overlay_configured=${overlay_result%%:*}; overlay_file=${overlay_result#*:}; fi; "#,
    r#"service_active=false; if systemctl is-active --quiet planeradar.service; then service_active=true; fi; service_restart_count=$(systemctl show planeradar.service --property=NRestarts --value); service_main_pid=$(systemctl show planeradar.service --property=MainPID --value); service_invocation=$(systemctl show planeradar.service --property=InvocationID --value); "#,
    r#"drm_device=unavailable; drm_mode=unavailable; renderer=unavailable; case "$service_main_pid:$service_invocation" in 0:*|*[!0-9]*:*|*:*[!0-9a-f]*) ;; *) card_count=0; for fd in /proc/"$service_main_pid"/fd/*; do target=$(readlink -- "$fd" 2>/dev/null || true); case "$target" in /dev/dri/card[0-9]*) if test "$drm_device" != unavailable && test "$drm_device" != "$target"; then card_count=2; break; fi; drm_device=$target; card_count=1;; esac; done; if test "$card_count" = 1 && test -c "$drm_device"; then card_name=${drm_device#/dev/dri/}; mode_file="/sys/class/drm/$card_name-DPI-1/modes"; drm_mode=$(sed -n '1p' "$mode_file" 2>/dev/null || true); test -n "$drm_mode" || drm_mode=unavailable; else drm_device=unavailable; fi; renderer=$(journalctl -b -u planeradar.service "_PID=$service_main_pid" "_SYSTEMD_INVOCATION_ID=$service_invocation" --no-pager -o cat 2>/dev/null | awk 'match($0, /render_driver=[^ ]+/) { value=substr($0, RSTART+14, RLENGTH-14); count++ } END { if (count == 1) print value }'); test -n "$renderer" || renderer=unavailable;; esac; "#,
    r#"touch_device=; touch_count=0; for name_file in /sys/class/input/event*/device/name; do test ! -L "$name_file" && test -f "$name_file" || continue; candidate=$(tr -d '\r\n' <"$name_file"); case "$candidate" in *HyperPixel*|*"generic ft5x06"*) touch_count=$((touch_count + 1)); touch_device=$candidate;; esac; done; test "$touch_count" -le 1; "#,
    r#"hostname=$(tr -d '\r\n' </etc/hostname); health_base64=; if health=$(curl --fail --silent --show-error --max-time 5 --max-filesize 4096 -H "Host: $hostname.local" http://127.0.0.1/healthz 2>/dev/null); then health_base64=$(printf %s "$health" | base64 -w0); fi; "#,
    r#"printf '{"schema_version":1,"os_id":"%s","os_version":"%s","architecture":"%s","application_version":"%s","application_revision":"%s","application_sha256":"%s","driver_version":"%s","driver_revision":"%s","driver_manifest_sha256":"%s","accepted_driver_manifest_sha256":"%s","expected_kernel":"%s","running_kernel":"%s","module_loaded":%s,"module_vermagic":"%s","expected_module_vermagic":"%s","module_sha256":"%s","expected_module_sha256":"%s","overlay_file":"%s","expected_overlay_file":"%s","overlay_sha256":"%s","expected_overlay_sha256":"%s","boot_config_sha256":"%s","normal_kernel_file":"%s","normal_kernel_sha256":"%s","normal_initramfs_file":"%s","normal_initramfs_sha256":"%s","base_dtb_sha256":"%s","vc4_overlay_sha256":"%s","overlay_configured":%s,"drm_device":"%s","drm_mode":"%s","renderer":"%s","touch_device":"%s","service_active":%s,"service_restart_count":%s,"health_base64":"%s","hostname":"%s"}' "#,
    r#""$os_id" "$os_version" "$architecture" "$application_version" "$application_revision" "$application_sha256" "$driver_version" "$driver_revision" "$driver_manifest_sha256" "$accepted_driver_manifest_sha256" "$expected_kernel" "$running_kernel" "$module_loaded" "$module_vermagic" "$expected_module_vermagic" "$module_sha256" "$expected_module_sha256" "$overlay_file" "$expected_overlay_file" "$overlay_sha256" "$expected_overlay_sha256" "$boot_config_sha256" "$normal_kernel_file" "$normal_kernel_sha256" "$normal_initramfs_file" "$normal_initramfs_sha256" "$base_dtb_sha256" "$vc4_overlay_sha256" "$overlay_configured" "$drm_device" "$drm_mode" "$renderer" "$touch_device" "$service_active" "$service_restart_count" "$health_base64" "$hostname""#,
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
    accepted_driver_manifest_sha256: String,
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
    normal_kernel_file: String,
    normal_kernel_sha256: String,
    normal_initramfs_file: String,
    normal_initramfs_sha256: String,
    base_dtb_sha256: String,
    vc4_overlay_sha256: String,
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
        installed_application: &ArtifactIdentity,
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
            &installed_application.version,
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
        let probe = self.diagnostic_probe(&persisted_driver, &expected_application)?;
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
            accepted_driver_manifest_sha256: probe.accepted_driver_manifest_sha256,
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
            normal_kernel_file: (!probe.normal_kernel_file.is_empty())
                .then_some(probe.normal_kernel_file),
            normal_kernel_sha256: (!probe.normal_kernel_sha256.is_empty())
                .then_some(probe.normal_kernel_sha256),
            normal_initramfs_file: (!probe.normal_initramfs_file.is_empty())
                .then_some(probe.normal_initramfs_file),
            normal_initramfs_sha256: (!probe.normal_initramfs_sha256.is_empty())
                .then_some(probe.normal_initramfs_sha256),
            base_dtb_sha256: (!probe.base_dtb_sha256.is_empty()).then_some(probe.base_dtb_sha256),
            vc4_overlay_sha256: (!probe.vc4_overlay_sha256.is_empty())
                .then_some(probe.vc4_overlay_sha256),
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
    if facts.installed_driver.sha256 != facts.accepted_driver_manifest_sha256
        || facts.persisted_driver_manifest_sha256 != facts.expected_driver.sha256
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
        &facts.accepted_driver_manifest_sha256,
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
        &facts.accepted_driver_manifest_sha256,
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
    let provenance = [
        facts.normal_kernel_file.as_deref(),
        facts.normal_kernel_sha256.as_deref(),
        facts.normal_initramfs_file.as_deref(),
        facts.normal_initramfs_sha256.as_deref(),
        facts.base_dtb_sha256.as_deref(),
        facts.vc4_overlay_sha256.as_deref(),
    ];
    if provenance.iter().any(Option::is_some) != provenance.iter().all(Option::is_some) {
        return Err(OperationError::MalformedFacts);
    }
    if let [
        Some(kernel_file),
        Some(kernel_sha),
        Some(initramfs_file),
        Some(initramfs_sha),
        Some(base_dtb_sha),
        Some(vc4_overlay_sha),
    ] = provenance
        && (kernel_file != "kernel8.img"
            || initramfs_file != "initramfs8"
            || ![kernel_sha, initramfs_sha, base_dtb_sha, vc4_overlay_sha]
                .into_iter()
                .all(|digest| is_lower_hex(digest, 64)))
    {
        return Err(OperationError::MalformedFacts);
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

pub const LIFECYCLE_SCHEMA_VERSION: u32 = 3;
pub const MAX_ACCEPTED_PAIRS: usize = 3;
pub const MANAGEMENT_HELPER_PROTOCOL: &str = "lifecycle-v3";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReleasePair {
    pub application: ArtifactIdentity,
    pub driver: ArtifactIdentity,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AcceptedPair {
    pub pair: ReleasePair,
    pub sequence: u64,
    pub owned_files: Vec<OwnedFile>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ManagementHelper {
    pub application: ArtifactIdentity,
    pub target_path: String,
    pub protocol: String,
}

impl Default for ManagementHelper {
    fn default() -> Self {
        Self {
            application: ArtifactIdentity {
                version: String::new(),
                source_commit: String::new(),
                sha256: String::new(),
            },
            target_path: String::new(),
            protocol: String::new(),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LifecyclePhase {
    Prepared,
    ApplicationStaged,
    DriverStaged,
    TrybootVerified,
    DriverCommitted,
    ExplicitNormalVerified,
    DriverNormalized,
    NormalizedBootVerified,
    /// Legacy pre-application phase retained for persisted lifecycle compatibility.
    NormalBootVerified,
    ApplicationActivated,
    ApplicationRestarted,
    PairAccepted,
    RecoveryDriverRestored,
    RecoveryApplicationRestored,
    RecoveryCandidateRetired,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LifecycleTransaction {
    pub prior: AcceptedPair,
    pub candidate: ReleasePair,
    #[serde(default)]
    pub management_helper: ManagementHelper,
    #[serde(default)]
    pub candidate_owned_files: Vec<OwnedFile>,
    #[serde(default)]
    pub restored_owned_files: Option<Vec<OwnedFile>>,
    pub phase: LifecyclePhase,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UninstallPhase {
    Prepared,
    ApplicationRemoved,
    DriverRemoved,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UninstallTransaction {
    pub accepted: AcceptedPair,
    pub purge_settings: bool,
    pub recovery_helper: OwnedFile,
    pub phase: UninstallPhase,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LifecycleState {
    schema_version: u32,
    hardware: TargetHardwareIdentity,
    accepted: Vec<AcceptedPair>,
    transaction: Option<LifecycleTransaction>,
    #[serde(default)]
    uninstall: Option<UninstallTransaction>,
}

impl LifecycleState {
    pub fn empty(hardware: TargetHardwareIdentity) -> Result<Self, LifecycleError> {
        Self::installed(hardware, Vec::new())
    }

    pub fn installed(
        hardware: TargetHardwareIdentity,
        accepted: Vec<AcceptedPair>,
    ) -> Result<Self, LifecycleError> {
        let state = Self {
            schema_version: LIFECYCLE_SCHEMA_VERSION,
            hardware,
            accepted,
            transaction: None,
            uninstall: None,
        };
        state.validate()?;
        Ok(state)
    }

    pub fn migrate_task14(state: &TargetInstallState) -> Result<Self, LifecycleError> {
        if state.schema_version != 1 || state.last_verified_phase != InstallPhase::Complete {
            return Err(LifecycleError::InvalidState);
        }
        let application = state
            .application
            .clone()
            .ok_or(LifecycleError::InvalidState)?;
        let driver = state.driver.clone().ok_or(LifecycleError::InvalidState)?;
        Self::installed(
            state.hardware.clone(),
            vec![AcceptedPair {
                pair: ReleasePair {
                    application,
                    driver,
                },
                sequence: 1,
                owned_files: state.owned_files.clone(),
            }],
        )
    }

    pub fn schema_version(&self) -> u32 {
        self.schema_version
    }

    pub fn hardware(&self) -> &TargetHardwareIdentity {
        &self.hardware
    }

    pub fn accepted(&self) -> &[AcceptedPair] {
        &self.accepted
    }

    pub fn uninstall_transaction(&self) -> Option<&UninstallTransaction> {
        self.uninstall.as_ref()
    }

    pub fn transaction(&self) -> Option<&LifecycleTransaction> {
        self.transaction.as_ref()
    }

    pub fn to_json(&self) -> Result<String, LifecycleError> {
        self.validate()?;
        serde_json::to_string(self).map_err(|_| LifecycleError::InvalidState)
    }

    pub fn from_json(contents: &[u8]) -> Result<Self, LifecycleError> {
        if contents.len() > MAX_TARGET_STATE_BYTES {
            return Err(LifecycleError::InvalidState);
        }
        let mut deserializer = serde_json::Deserializer::from_slice(contents);
        let mut state =
            Self::deserialize(&mut deserializer).map_err(|_| LifecycleError::InvalidState)?;
        deserializer
            .end()
            .map_err(|_| LifecycleError::InvalidState)?;
        if matches!(state.schema_version, 1 | 2) {
            state.schema_version = LIFECYCLE_SCHEMA_VERSION;
            if let Some(transaction) = state.transaction.as_mut() {
                if transaction.management_helper == ManagementHelper::default() {
                    transaction.management_helper =
                        management_helper(&transaction.candidate.application);
                }
                if transaction.candidate_owned_files.is_empty() {
                    transaction.candidate_owned_files =
                        candidate_owned_files(&transaction.candidate);
                }
            }
        }
        state.validate()?;
        Ok(state)
    }

    fn validate(&self) -> Result<(), LifecycleError> {
        if self.schema_version != LIFECYCLE_SCHEMA_VERSION
            || self.accepted.len() > MAX_ACCEPTED_PAIRS
            || self
                .accepted
                .windows(2)
                .any(|window| window[0].sequence <= window[1].sequence)
            || self.accepted.iter().any(|accepted| {
                accepted.sequence == 0
                    || accepted.owned_files.is_empty()
                    || !valid_release_pair(&accepted.pair)
                    || accepted
                        .owned_files
                        .iter()
                        .enumerate()
                        .any(|(index, file)| {
                            !valid_owned_file(file)
                                || accepted.owned_files[..index]
                                    .iter()
                                    .any(|prior| prior.target_path == file.target_path)
                        })
            })
            || self.accepted.iter().enumerate().any(|(index, accepted)| {
                self.accepted[index + 1..]
                    .iter()
                    .any(|other| other.pair == accepted.pair)
            })
            || self.uninstall.as_ref().is_some_and(|uninstall| {
                !valid_owned_file(&uninstall.recovery_helper)
                    || uninstall.recovery_helper.target_path
                        != format!(
                            "/var/lib/planeradar-installer/helpers/{}/planeradar",
                            uninstall.accepted.pair.application.sha256
                        )
                    || self.accepted.first() != Some(&uninstall.accepted)
                    || self.transaction.is_some()
            })
        {
            return Err(LifecycleError::InvalidState);
        }
        if let Some(transaction) = &self.transaction
            && (!valid_release_pair(&transaction.candidate)
                || !valid_management_helper(transaction)
                || transaction.candidate_owned_files
                    != candidate_owned_files(&transaction.candidate)
                || transaction
                    .restored_owned_files
                    .as_ref()
                    .is_some_and(|files| {
                        files.is_empty()
                            || files.iter().enumerate().any(|(index, file)| {
                                !valid_owned_file(file)
                                    || files[..index]
                                        .iter()
                                        .any(|prior| prior.target_path == file.target_path)
                            })
                    })
                || if transaction.phase == LifecyclePhase::PairAccepted {
                    !self.accepted.first().is_some_and(|current| {
                        current.pair == transaction.candidate
                            && self.accepted.contains(&transaction.prior)
                    })
                } else {
                    !self
                        .accepted
                        .first()
                        .is_some_and(|current| current == &transaction.prior)
                })
        {
            return Err(LifecycleError::InvalidState);
        }
        Ok(())
    }

    fn current(&self) -> Result<&AcceptedPair, LifecycleError> {
        self.accepted.first().ok_or(LifecycleError::NoAcceptedPair)
    }

    fn begin(
        &mut self,
        candidate: ReleasePair,
        management_helper: ManagementHelper,
    ) -> Result<(), LifecycleError> {
        self.validate()?;
        if self.uninstall.is_some() {
            return Err(LifecycleError::UninstallInProgress);
        }
        self.transaction = Some(LifecycleTransaction {
            prior: self.current()?.clone(),
            candidate_owned_files: candidate_owned_files(&candidate),
            candidate,
            management_helper,
            restored_owned_files: None,
            phase: LifecyclePhase::Prepared,
        });
        Ok(())
    }

    fn set_phase(&mut self, phase: LifecyclePhase) -> Result<(), LifecycleError> {
        self.transaction
            .as_mut()
            .ok_or(LifecycleError::InvalidState)?
            .phase = phase;
        Ok(())
    }

    fn accept_pending_driver_finalize(
        &mut self,
        pair: ReleasePair,
        owned_files: Vec<OwnedFile>,
    ) -> Result<(), LifecycleError> {
        if owned_files.is_empty() || owned_files.iter().any(|file| !valid_owned_file(file)) {
            return Err(LifecycleError::InvalidOwnership);
        }
        let next_sequence = self
            .accepted
            .iter()
            .map(|accepted| accepted.sequence)
            .max()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or(LifecycleError::InvalidState)?;
        self.accepted.retain(|accepted| accepted.pair != pair);
        self.accepted.insert(
            0,
            AcceptedPair {
                pair: pair.clone(),
                sequence: next_sequence,
                owned_files,
            },
        );
        self.accepted.truncate(MAX_ACCEPTED_PAIRS);
        let transaction = self
            .transaction
            .as_mut()
            .ok_or(LifecycleError::InvalidState)?;
        if transaction.candidate != pair {
            return Err(LifecycleError::InvalidState);
        }
        transaction.phase = LifecyclePhase::PairAccepted;
        self.validate()
    }

    fn finish_accept(&mut self) -> Result<(), LifecycleError> {
        if self
            .transaction
            .as_ref()
            .is_none_or(|transaction| transaction.phase != LifecyclePhase::PairAccepted)
        {
            return Err(LifecycleError::InvalidState);
        }
        self.transaction = None;
        self.validate()
    }

    fn begin_uninstall(
        &mut self,
        accepted: AcceptedPair,
        purge_settings: bool,
        recovery_helper: OwnedFile,
    ) -> Result<(), LifecycleError> {
        self.validate()?;
        if self.transaction.is_some() || self.uninstall.is_some() || self.current()? != &accepted {
            return Err(LifecycleError::InvalidState);
        }
        self.uninstall = Some(UninstallTransaction {
            accepted,
            purge_settings,
            recovery_helper,
            phase: UninstallPhase::Prepared,
        });
        self.validate()
    }

    fn set_uninstall_phase(&mut self, phase: UninstallPhase) -> Result<(), LifecycleError> {
        self.uninstall
            .as_mut()
            .ok_or(LifecycleError::InvalidState)?
            .phase = phase;
        self.validate()
    }
}

fn valid_release_pair(pair: &ReleasePair) -> bool {
    [&pair.application, &pair.driver]
        .into_iter()
        .all(|artifact| {
            semver::Version::parse(&artifact.version)
                .is_ok_and(|version| version.to_string() == artifact.version)
                && is_lower_hex(&artifact.source_commit, 40)
                && is_lower_hex(&artifact.sha256, 64)
        })
}

fn valid_owned_file(file: &OwnedFile) -> bool {
    file.target_path.starts_with('/')
        && file.target_path != "/"
        && !file.target_path.contains("//")
        && !file
            .target_path
            .split('/')
            .any(|component| component == "." || component == "..")
        && !file.target_path.bytes().any(|byte| byte.is_ascii_control())
        && is_lower_hex(&file.sha256, 64)
}

fn management_helper(application: &ArtifactIdentity) -> ManagementHelper {
    ManagementHelper {
        application: application.clone(),
        target_path: format!(
            "/var/lib/planeradar-installer/helpers/{}/planeradar",
            application.sha256
        ),
        protocol: MANAGEMENT_HELPER_PROTOCOL.into(),
    }
}

fn valid_management_helper(transaction: &LifecycleTransaction) -> bool {
    let helper = &transaction.management_helper;
    valid_release_artifact(&helper.application)
        && helper.protocol == MANAGEMENT_HELPER_PROTOCOL
        && helper.target_path
            == format!(
                "/var/lib/planeradar-installer/helpers/{}/planeradar",
                helper.application.sha256
            )
        && (helper.application == transaction.prior.pair.application
            || helper.application == transaction.candidate.application)
}

fn valid_release_artifact(artifact: &ArtifactIdentity) -> bool {
    semver::Version::parse(&artifact.version)
        .is_ok_and(|version| version.to_string() == artifact.version)
        && is_lower_hex(&artifact.source_commit, 40)
        && is_lower_hex(&artifact.sha256, 64)
}

fn candidate_owned_files(pair: &ReleasePair) -> Vec<OwnedFile> {
    vec![OwnedFile {
        target_path: format!(
            "/opt/planeradar/releases/{}/{}/planeradar",
            pair.application.version, pair.application.sha256
        ),
        sha256: pair.application.sha256.clone(),
    }]
}

pub trait LifecycleBackend {
    fn load_lifecycle_state(&self) -> Result<LifecycleState, LifecycleError>;
    fn save_lifecycle_state(&self, state: &LifecycleState) -> Result<(), LifecycleError>;
    fn resolve_release(
        &self,
        requested: Option<&semver::Version>,
    ) -> Result<ReleasePair, LifecycleError>;
    fn verify_historical_release(&self, expected: &ReleasePair) -> Result<(), LifecycleError>;
    fn prepare_management_helper(
        &self,
        pair: &ReleasePair,
    ) -> Result<ManagementHelper, LifecycleError> {
        Ok(management_helper(&pair.application))
    }
    fn retire_management_helper(&self, _helper: &ManagementHelper) -> Result<(), LifecycleError> {
        Ok(())
    }
    fn stage_application(&self, pair: &ReleasePair) -> Result<(), LifecycleError>;
    fn stage_driver(&self, pair: &ReleasePair) -> Result<(), LifecycleError>;
    fn tryboot_driver(&self, pair: &ReleasePair) -> Result<(), LifecycleError>;
    fn verify_tryboot_driver(&self, pair: &ReleasePair) -> Result<(), LifecycleError>;
    fn commit_driver(&self, pair: &ReleasePair) -> Result<(), LifecycleError>;
    fn reboot_normal(&self, pair: &ReleasePair) -> Result<(), LifecycleError>;
    fn verify_explicit_normal_driver(&self, pair: &ReleasePair) -> Result<(), LifecycleError>;
    fn normalize_driver(&self, pair: &ReleasePair) -> Result<(), LifecycleError>;
    fn verify_normalized_driver(&self, pair: &ReleasePair) -> Result<(), LifecycleError>;
    fn activate_application(&self, pair: &ReleasePair) -> Result<Vec<OwnedFile>, LifecycleError>;
    fn restart_application(&self) -> Result<(), LifecycleError>;
    fn verify_pair(&self, pair: &ReleasePair) -> Result<(), LifecycleError>;
    fn finalize_driver_acceptance(&self, pair: &ReleasePair) -> Result<(), LifecycleError>;
    fn restore_application(&self, prior: &AcceptedPair) -> Result<Vec<OwnedFile>, LifecycleError>;
    fn restore_driver(&self, prior: &AcceptedPair) -> Result<(), LifecycleError>;
    fn retire_candidate(&self, owned_files: &[OwnedFile]) -> Result<(), LifecycleError>;
    fn prepare_uninstall(&self, accepted: &AcceptedPair) -> Result<OwnedFile, LifecycleError>;
    fn uninstall_application(
        &self,
        owned_files: &[OwnedFile],
        purge_settings: bool,
    ) -> Result<(), LifecycleError>;
    fn uninstall_driver(&self, drivers: &[ArtifactIdentity]) -> Result<(), LifecycleError>;
    fn finalize_driver_uninstall(&self) -> Result<(), LifecycleError>;
    fn finalize_uninstall(&self, state: &LifecycleState) -> Result<(), LifecycleError>;
    fn retire_recovery_helper(
        &self,
        _application: &ArtifactIdentity,
    ) -> Result<(), LifecycleError> {
        Ok(())
    }
}

pub struct LifecycleManager<'a, B> {
    backend: &'a B,
}

impl<'a, B: LifecycleBackend> LifecycleManager<'a, B> {
    pub fn new(backend: &'a B) -> Self {
        Self { backend }
    }

    pub fn upgrade(
        &self,
        requested: Option<&semver::Version>,
    ) -> Result<LifecycleOutcome, LifecycleError> {
        let state = match self.load_recovered_state() {
            Ok(state) => state,
            Err(LifecycleError::ManagementHelperRequired) => {
                let pair = self.backend.resolve_release(requested)?;
                if !valid_release_pair(&pair) {
                    return Err(LifecycleError::ImmutableReleaseMismatch);
                }
                let helper = self.backend.prepare_management_helper(&pair)?;
                let state = self.load_recovered_state()?;
                return self.apply_pair(state, pair, helper);
            }
            Err(error) => return Err(error),
        };
        let pair = self.backend.resolve_release(requested)?;
        if !valid_release_pair(&pair) {
            return Err(LifecycleError::ImmutableReleaseMismatch);
        }
        let helper = self.backend.prepare_management_helper(&pair)?;
        self.apply_pair(state, pair, helper)
    }

    pub fn rollback(
        &self,
        requested: Option<&semver::Version>,
    ) -> Result<LifecycleOutcome, LifecycleError> {
        let state = self.load_recovered_state()?;
        let current = state.current()?;
        let candidate = match requested {
            None => {
                let candidate = state
                    .accepted
                    .get(1)
                    .cloned()
                    .ok_or(LifecycleError::NoPriorAcceptedPair)?;
                self.backend.verify_historical_release(&candidate.pair)?;
                candidate
            }
            Some(version) => {
                let matching = state
                    .accepted
                    .iter()
                    .skip(1)
                    .filter(|accepted| accepted.pair.application.version == version.to_string())
                    .collect::<Vec<_>>();
                if matching.is_empty() {
                    return Err(LifecycleError::RequestedVersionNotAccepted);
                }
                let [accepted] = matching.as_slice() else {
                    return Err(LifecycleError::ImmutableReleaseMismatch);
                };
                self.backend.verify_historical_release(&accepted.pair)?;
                (*accepted).clone()
            }
        };
        if candidate.pair == current.pair {
            return Err(LifecycleError::NoPriorAcceptedPair);
        }
        let helper = self.backend.prepare_management_helper(&current.pair)?;
        self.apply_pair(state, candidate.pair, helper)
    }

    pub fn uninstall(&self, purge_settings: bool) -> Result<LifecycleOutcome, LifecycleError> {
        let mut state = self.load_recovered_state()?;
        if state.uninstall.is_none() && state.accepted.is_empty() {
            return Ok(LifecycleOutcome::AlreadyUninstalled);
        }
        if state.uninstall.is_none() {
            let current = state.current()?.clone();
            let helper = self.backend.prepare_uninstall(&current)?;
            state.begin_uninstall(current, purge_settings, helper)?;
            self.backend.save_lifecycle_state(&state)?;
        }
        let uninstall = state
            .uninstall
            .clone()
            .ok_or(LifecycleError::InvalidState)?;
        if uninstall.purge_settings != purge_settings {
            return Err(LifecycleError::UninstallOptionsMismatch);
        }
        if uninstall.phase == UninstallPhase::Prepared {
            self.backend
                .uninstall_application(&uninstall.accepted.owned_files, uninstall.purge_settings)?;
            state.set_uninstall_phase(UninstallPhase::ApplicationRemoved)?;
            self.backend.save_lifecycle_state(&state)?;
        }
        if state
            .uninstall
            .as_ref()
            .is_some_and(|transaction| transaction.phase == UninstallPhase::ApplicationRemoved)
        {
            let mut drivers = Vec::new();
            for driver in state.accepted.iter().map(|accepted| &accepted.pair.driver) {
                if !drivers.contains(driver) {
                    drivers.push(driver.clone());
                }
            }
            self.backend.uninstall_driver(&drivers)?;
            state.set_uninstall_phase(UninstallPhase::DriverRemoved)?;
            self.backend.save_lifecycle_state(&state)?;
        }
        if state
            .uninstall
            .as_ref()
            .is_some_and(|transaction| transaction.phase == UninstallPhase::DriverRemoved)
        {
            self.backend.finalize_driver_uninstall()?;
        }
        self.backend.finalize_uninstall(&state)?;
        Ok(LifecycleOutcome::Uninstalled)
    }

    fn apply_pair(
        &self,
        mut state: LifecycleState,
        pair: ReleasePair,
        management_helper: ManagementHelper,
    ) -> Result<LifecycleOutcome, LifecycleError> {
        let prior = state.current()?.clone();
        if pair == prior.pair {
            let _ = self.backend.retire_recovery_helper(&pair.application);
            return Ok(LifecycleOutcome::AlreadyAccepted {
                version: semver::Version::parse(&pair.application.version)
                    .map_err(|_| LifecycleError::InvalidState)?,
            });
        }
        let driver_changed = pair.driver != prior.pair.driver;
        state.begin(pair.clone(), management_helper.clone())?;
        self.backend.save_lifecycle_state(&state)?;

        let result = self.apply_candidate(&mut state, &pair, driver_changed);
        let owned_files = match result {
            Ok(owned_files) => owned_files,
            Err(error) => {
                return if self.recover(&mut state, driver_changed).is_ok() {
                    Err(error)
                } else {
                    Err(LifecycleError::RecoveryFailed)
                };
            }
        };
        let mut accepted_state = state.clone();
        accepted_state.accept_pending_driver_finalize(pair.clone(), owned_files)?;
        if self.backend.save_lifecycle_state(&accepted_state).is_err() {
            return if self.recover(&mut state, driver_changed).is_ok() {
                Err(LifecycleError::Backend)
            } else {
                Err(LifecycleError::RecoveryFailed)
            };
        }
        state = accepted_state;
        if driver_changed {
            self.backend.finalize_driver_acceptance(&pair)?;
        }
        state.finish_accept()?;
        self.backend.save_lifecycle_state(&state)?;
        let _ = self.backend.retire_management_helper(&management_helper);
        Ok(LifecycleOutcome::Accepted {
            version: semver::Version::parse(&pair.application.version)
                .map_err(|_| LifecycleError::InvalidState)?,
            driver_changed,
        })
    }

    fn apply_candidate(
        &self,
        state: &mut LifecycleState,
        pair: &ReleasePair,
        driver_changed: bool,
    ) -> Result<Vec<OwnedFile>, LifecycleError> {
        self.backend.stage_application(pair)?;
        self.persist_phase(state, LifecyclePhase::ApplicationStaged)?;
        if driver_changed {
            self.backend.stage_driver(pair)?;
            self.persist_phase(state, LifecyclePhase::DriverStaged)?;
            self.backend.tryboot_driver(pair)?;
            self.backend.verify_tryboot_driver(pair)?;
            self.persist_phase(state, LifecyclePhase::TrybootVerified)?;
            self.backend.commit_driver(pair)?;
            self.persist_phase(state, LifecyclePhase::DriverCommitted)?;
            self.backend.reboot_normal(pair)?;
            self.backend.verify_explicit_normal_driver(pair)?;
            self.persist_phase(state, LifecyclePhase::ExplicitNormalVerified)?;
            self.backend.normalize_driver(pair)?;
            self.persist_phase(state, LifecyclePhase::DriverNormalized)?;
            self.backend.reboot_normal(pair)?;
            self.backend.verify_normalized_driver(pair)?;
            self.persist_phase(state, LifecyclePhase::NormalizedBootVerified)?;
        }
        let owned_files = self.backend.activate_application(pair)?;
        self.persist_phase(state, LifecyclePhase::ApplicationActivated)?;
        self.backend.restart_application()?;
        self.persist_phase(state, LifecyclePhase::ApplicationRestarted)?;
        self.backend.verify_pair(pair)?;
        Ok(owned_files)
    }

    fn persist_phase(
        &self,
        state: &mut LifecycleState,
        phase: LifecyclePhase,
    ) -> Result<(), LifecycleError> {
        state.set_phase(phase)?;
        self.backend.save_lifecycle_state(state)
    }

    fn load_recovered_state(&self) -> Result<LifecycleState, LifecycleError> {
        let mut state = self.backend.load_lifecycle_state()?;
        state.validate()?;
        if state.uninstall.is_some() {
            return Ok(state);
        }
        if let Some(transaction) = state.transaction.clone() {
            if transaction.phase == LifecyclePhase::PairAccepted {
                if transaction.prior.pair.driver != transaction.candidate.driver {
                    self.backend
                        .finalize_driver_acceptance(&transaction.candidate)?;
                }
                state.finish_accept()?;
                self.backend.save_lifecycle_state(&state)?;
                let _ = self
                    .backend
                    .retire_management_helper(&transaction.management_helper);
                return Ok(state);
            }
            let driver_changed = transaction.prior.pair.driver != transaction.candidate.driver;
            self.recover(&mut state, driver_changed)?;
        }
        Ok(state)
    }

    fn recover(
        &self,
        state: &mut LifecycleState,
        driver_changed: bool,
    ) -> Result<(), LifecycleError> {
        let transaction = state
            .transaction
            .clone()
            .ok_or(LifecycleError::InvalidState)?;
        let management_helper = transaction.management_helper.clone();
        let prior = transaction.prior;
        if state
            .transaction
            .as_ref()
            .is_some_and(|transaction| transaction.phase < LifecyclePhase::RecoveryDriverRestored)
        {
            if driver_changed {
                self.backend.restore_driver(&prior)?;
            }
            state.set_phase(LifecyclePhase::RecoveryDriverRestored)?;
            self.backend.save_lifecycle_state(state)?;
        }
        if state.transaction.as_ref().is_some_and(|transaction| {
            transaction.phase < LifecyclePhase::RecoveryApplicationRestored
        }) {
            let restored = self.backend.restore_application(&prior)?;
            let transaction = state
                .transaction
                .as_mut()
                .ok_or(LifecycleError::InvalidState)?;
            transaction.restored_owned_files = Some(restored);
            transaction.phase = LifecyclePhase::RecoveryApplicationRestored;
            self.backend.save_lifecycle_state(state)?;
        }
        self.backend.restart_application()?;
        self.backend.verify_pair(&prior.pair)?;
        if state
            .transaction
            .as_ref()
            .is_some_and(|transaction| transaction.phase < LifecyclePhase::RecoveryCandidateRetired)
        {
            let candidate = state
                .transaction
                .as_ref()
                .ok_or(LifecycleError::InvalidState)?
                .candidate_owned_files
                .clone();
            self.backend.retire_candidate(&candidate)?;
            state.set_phase(LifecyclePhase::RecoveryCandidateRetired)?;
            self.backend.save_lifecycle_state(state)?;
        }
        let mut restored = state
            .transaction
            .as_ref()
            .and_then(|transaction| transaction.restored_owned_files.clone())
            .ok_or(LifecycleError::InvalidState)?;
        let retired_paths = state
            .transaction
            .as_ref()
            .ok_or(LifecycleError::InvalidState)?
            .candidate_owned_files
            .iter()
            .map(|file| file.target_path.as_str())
            .collect::<Vec<_>>();
        restored.retain(|file| {
            !retired_paths
                .iter()
                .any(|retired| *retired == file.target_path)
        });
        if restored.is_empty() {
            return Err(LifecycleError::InvalidOwnership);
        }
        let prior_entry = state
            .accepted
            .iter_mut()
            .find(|accepted| accepted.pair == prior.pair)
            .ok_or(LifecycleError::InvalidState)?;
        prior_entry.owned_files = restored;
        state.transaction = None;
        self.backend.save_lifecycle_state(state)?;
        let _ = self.backend.retire_management_helper(&management_helper);
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LifecycleOutcome {
    Accepted {
        version: semver::Version,
        driver_changed: bool,
    },
    AlreadyAccepted {
        version: semver::Version,
    },
    Uninstalled,
    AlreadyUninstalled,
}

impl fmt::Display for LifecycleOutcome {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Accepted {
                version,
                driver_changed,
            } => write!(
                formatter,
                "Plane Radar {version} accepted ({})",
                if *driver_changed {
                    "application and driver"
                } else {
                    "application only"
                }
            ),
            Self::AlreadyAccepted { version } => {
                write!(formatter, "Plane Radar {version} is already accepted")
            }
            Self::Uninstalled => formatter.write_str("Plane Radar uninstalled"),
            Self::AlreadyUninstalled => formatter.write_str("Plane Radar is already uninstalled"),
        }
    }
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum LifecycleError {
    #[error("lifecycle state is invalid or inconsistent")]
    InvalidState,
    #[error("a verified current-protocol management helper is required")]
    ManagementHelperRequired,
    #[error("no accepted Plane Radar installation exists")]
    NoAcceptedPair,
    #[error("no prior accepted Plane Radar release exists")]
    NoPriorAcceptedPair,
    #[error("the requested rollback version was not accepted")]
    RequestedVersionNotAccepted,
    #[error("the requested rollback version is ambiguous")]
    AmbiguousAcceptedVersion,
    #[error("an uninstall transaction is already in progress")]
    UninstallInProgress,
    #[error("uninstall retry options do not match the durable transaction")]
    UninstallOptionsMismatch,
    #[error("the resolved release identity is not immutable and valid")]
    ImmutableReleaseMismatch,
    #[error("the target returned an invalid ownership manifest")]
    InvalidOwnership,
    #[error("the lifecycle backend operation failed")]
    Backend,
    #[error("the prior accepted pair could not be restored and verified")]
    RecoveryFailed,
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

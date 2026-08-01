use std::collections::BTreeSet;
use std::fmt;
use std::path::Path;
use std::time::Duration;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Deserialize;
use thiserror::Error;

use crate::target::{SshTarget, TargetIdentity};
use crate::transport::{CommandRunner, Invocation, RemoteCommand, Transport};

pub const APPLICATION_REPOSITORY: &str = "https://github.com/shayne/RPi-Plane-Radar";
pub const DRIVER_REPOSITORY: &str = "https://github.com/shayne/hyperpixel2r-kms";
pub const MIN_MACOS_MAJOR: u16 = 14;
pub const MAC_MIN_FREE_BYTES: u64 = 16 * 1024 * 1024 * 1024;
pub const HOST_PROBE_TIMEOUT: Duration = Duration::from_secs(30);
pub const MAX_HOST_OUTPUT_BYTES: usize = 16 * 1024;
pub const MAX_TARGET_FACTS_BYTES: usize = 32 * 1024;
pub const TARGET_ROOT_MIN_FREE_BYTES: u64 = 2 * 1024 * 1024 * 1024;
pub const TARGET_BOOT_MIN_FREE_BYTES: u64 = 128 * 1024 * 1024;
pub const MAX_CLOCK_SKEW_SECONDS: u64 = 5 * 60;
const MAX_FACT_FIELD_BYTES: usize = 128;
const MAX_REPORTED_FREE_BYTES: u64 = 1024 * 1024 * 1024 * 1024;
const MIN_SYSTEM_TIME_UNIX: u64 = 1_577_836_800;
const MAX_SYSTEM_TIME_UNIX: u64 = 4_102_444_800;

pub const TARGET_FACTS_SCRIPT: &str = concat!(
    r#"set -eu; model=$(tr -d '\000\r\n' </proc/device-tree/model); os_id=$(sed -n 's/^ID=//p' /etc/os-release | tr -d '"'); os_version=$(sed -n 's/^VERSION_ID=//p' /etc/os-release | tr -d '"'); "#,
    r#"architecture=$(dpkg --print-architecture); kernel_release=$(uname -r); kernel_vermagic=$(/usr/sbin/modinfo -F vermagic vc4); default_target=$(systemctl get-default); boot_config=/boot/firmware/config.txt; bool() { if "$@" >/dev/null 2>&1; then printf true; else printf false; fi; }; "#,
    r#"display_manager_active=$(bool systemctl is-active --quiet display-manager.service); boot_config_regular=$(bool test ! -L "$boot_config" && test -f "$boot_config"); tryboot_supported=$(bool test -f /boot/firmware/start4.elf); clock_synchronized=$(bool test "$(timedatectl show --property=NTPSynchronized --value)" = yes); system_time_unix=$(date +%s); "#,
    r#"repository_uri=$(apt-get indextargets --format '$(URI)' | head -n 1); repository_probe="${repository_uri}.xz"; repository_scheme=${repository_uri%%://*}; case "$repository_scheme" in http|https) repository_scheme_valid=true;; *) repository_scheme_valid=false;; esac; package_repository_reachable=$(if test "$repository_scheme_valid" = true && timeout 15 curl --fail --location --silent --show-error --range 0-0 --output /dev/null "$repository_probe"; then printf true; else printf false; fi); "#,
    r#"port_80_listeners=$(ss -H -ltnp 'sport = :80'); port_80_free=true; if test -n "$port_80_listeners"; then port_80_free=false; port_80_listener_count=$(printf '%s\n' "$port_80_listeners" | grep -c . || true); port_80_pid_count=$(printf '%s\n' "$port_80_listeners" | grep -o 'pid=[0-9][0-9]*' | grep -c . || true); port_80_listener_pid=$(printf '%s\n' "$port_80_listeners" | sed -n 's/.*pid=\([0-9][0-9]*\).*/\1/p'); service_main_pid=$(systemctl show --property=MainPID --value planeradar.service); service_fragment=$(systemctl show --property=FragmentPath --value planeradar.service); service_executable=; case "$service_main_pid" in ''|0|*[!0-9]*) ;; *) service_executable=$(readlink -f -- /proc/"$service_main_pid"/exe 2>/dev/null || true);; esac; if test "$port_80_listener_count" = 1 && test "$port_80_pid_count" = 1 && test "$port_80_listener_pid" = "$service_main_pid" && systemctl is-active --quiet planeradar.service && test "$service_fragment" = /etc/systemd/system/planeradar.service && test "$service_executable" = /opt/planeradar/bin/planeradar && test ! -L "$service_executable" && test -f "$service_executable" && test -x "$service_executable" && test "$(stat -c '%U:%G:%a:%h' "$service_executable")" = root:root:755:1; then port_80_free=true; fi; fi; root_available_bytes=$(df -B1 --output=avail / | tail -n 1 | tr -d ' '); boot_available_bytes=$(df -B1 --output=avail /boot/firmware | tail -n 1 | tr -d ' '); "#,
    r#"running_headers_release=; if test -r "/lib/modules/$kernel_release/build/include/config/kernel.release"; then running_headers_release=$(tr -d '\r\n' <"/lib/modules/$kernel_release/build/include/config/kernel.release"); fi; running_headers_available=$(bool test "$running_headers_release" = "$kernel_release"); "#,
    r#"kernel8_image=/boot/firmware/kernel8.img; boot_selected_kernel_match_count=0; boot_selected_kernel_release=; if test ! -L "$kernel8_image" && test -f "$kernel8_image" && test "$(stat -c '%U:%G' "$kernel8_image")" = root:root; then kernel8_sha=$(sha256sum -- "$kernel8_image" | awk '{print $1}'); for candidate_image in /boot/vmlinuz-*; do test ! -L "$candidate_image" && test -f "$candidate_image" || continue; test "$(stat -c '%U:%G' "$candidate_image")" = root:root || continue; candidate_release=${candidate_image#/boot/vmlinuz-}; case "$candidate_release" in ''|*[!A-Za-z0-9._+-]*) continue;; esac; test "$(sha256sum -- "$candidate_image" | awk '{print $1}')" = "$kernel8_sha" || continue; boot_selected_kernel_match_count=$((boot_selected_kernel_match_count + 1)); boot_selected_kernel_release=$candidate_release; done; fi; "#,
    r#"installed_kernel_header_pair_count=0; installed_kernel_release=; installed_headers_release=; for module_dir in /lib/modules/*; do test -d "$module_dir" || continue; candidate=${module_dir##*/}; candidate_headers=; if test -r "$module_dir/build/include/config/kernel.release"; then candidate_headers=$(tr -d '\r\n' <"$module_dir/build/include/config/kernel.release"); fi; if test "$candidate" = "$candidate_headers" && test "$candidate" = "$boot_selected_kernel_release"; then installed_kernel_header_pair_count=$((installed_kernel_header_pair_count + 1)); installed_kernel_release=$candidate; installed_headers_release=$candidate_headers; fi; done; "#,
    r#"boot_kernel_override_conflicting=$(if awk '{ line=$0; sub(/^[[:space:]]+/, "", line); if (line == "" || line ~ /^#/) next; split(line, pieces, "="); key=pieces[1]; gsub(/[[:space:]]/, "", key); if (key != "kernel") next; sub(/^[^=]*=/, "", line); sub(/^[[:space:]]+/, "", line); sub(/[[:space:]]+$/, "", line); if (line != "kernel8.img") exit 1 }' "$boot_config"; then printf false; else printf true; fi); "#,
    r#"overlay_lines=$(awk '{ line=$0; sub(/^[[:space:]]+/, "", line); sub(/[[:space:]]+$/, "", line); if (line ~ /^dtoverlay=/) print line }' "$boot_config"); external_hyperpixel_overlay_count=$(printf '%s\n' "$overlay_lines" | grep -Ec '^dtoverlay=hyperpixel2r-kms-[0-9a-f]{12}\.dtbo$' || true); hyperpixel_declaration_count=$(printf '%s\n' "$overlay_lines" | grep -Ec '^dtoverlay=(vc4-kms-dpi-hyperpixel2r|planeradar-hyperpixel2r-[0-9a-f]{12}|hyperpixel2r-kms-[0-9a-f]{12}\.dtbo)$' || true); replace_overlay=; if test "$hyperpixel_declaration_count" = 1; then replace_overlay=$(printf '%s\n' "$overlay_lines" | awk -F= '$0 ~ /^dtoverlay=(vc4-kms-dpi-hyperpixel2r|planeradar-hyperpixel2r-[0-9a-f]{12}|hyperpixel2r-kms-[0-9a-f]{12}\.dtbo)$/ { print $2 }'); fi; conflicting_overlay_lines=$(printf '%s\n' "$overlay_lines" | grep -E '^dtoverlay=(.*hyperpixel.*|vc4-kms-dpi.*|dpi[0-9].*)$' || true); unsafe_overlay_present=$(if printf '%s\n' "$conflicting_overlay_lines" | grep -Ev '^(|dtoverlay=vc4-kms-dpi-hyperpixel2r|dtoverlay=planeradar-hyperpixel2r-[0-9a-f]{12}|dtoverlay=hyperpixel2r-kms-[0-9a-f]{12}\.dtbo)$' | grep -q . || test "$hyperpixel_declaration_count" -gt 1; then printf true; else printf false; fi); "#,
    r#"hyperpixel_state_dir=/var/lib/hyperpixel2r-kms; hyperpixel_state_dir_safe=$(if test -L "$hyperpixel_state_dir"; then printf false; elif test -e "$hyperpixel_state_dir"; then if test ! -d "$hyperpixel_state_dir" || test "$(stat -c '%a:%U:%G' "$hyperpixel_state_dir")" != 755:root:root; then printf false; else printf true; fi; else printf true; fi); hyperpixel_transaction_active=false; if test "$hyperpixel_state_dir_safe" = false; then hyperpixel_transaction_active=true; elif test -L /var/lib/hyperpixel2r-kms/tryboot-state || test -e /var/lib/hyperpixel2r-kms/tryboot-state; then hyperpixel_transaction_active=true; else for transaction_path in /var/lib/hyperpixel2r-kms/.tryboot-state-hold.* /var/lib/hyperpixel2r-kms/.hp2r-transaction.*; do if test -L "$transaction_path" || test -e "$transaction_path"; then hyperpixel_transaction_active=true; break; fi; done; fi; legacy_checkpoint_active=$(bool systemctl is-active --quiet planeradar-hyperpixel-checkpoint.service); "#,
    r#"external_hyperpixel_module_count=$(awk '$1 == "hyperpixel2r_kms" { count++ } END { print count + 0 }' /proc/modules); external_hyperpixel_module_loaded=$(if test "$external_hyperpixel_module_count" = 1; then printf true; else printf false; fi); unexpected_hyperpixel_module_loaded=$(if awk '$1 ~ /hyperpixel/ && $1 != "hyperpixel2r_kms" { found=1 } END { exit !found }' /proc/modules; then printf true; else printf false; fi); "#,
    r#"external_hyperpixel_binding_count=0; generic_driver=/sys/bus/platform/drivers/hyperpixel2r-kms; if test ! -L "$generic_driver" && test -d "$generic_driver"; then for binding in "$generic_driver"/*; do test -L "$binding" || continue; resolved_binding=$(readlink -f -- "$binding") || continue; case "$resolved_binding" in /sys/devices/platform/*) ;; *) continue;; esac; compatible="$resolved_binding/of_node/compatible"; test ! -L "$compatible" && test -f "$compatible" || continue; if tr '\000' '\n' <"$compatible" | grep -Fxq shayne,hyperpixel2r-kms; then external_hyperpixel_binding_count=$((external_hyperpixel_binding_count + 1)); fi; done; fi; "#,
    r#"gpio_display_state_safe=$(if test "$hyperpixel_transaction_active" = false && test "$legacy_checkpoint_active" = false && test "$unexpected_hyperpixel_module_loaded" = false && { { test "$external_hyperpixel_overlay_count" = 0 && test "$external_hyperpixel_module_loaded" = false && test "$external_hyperpixel_binding_count" = 0; } || { test "$external_hyperpixel_overlay_count" = 1 && test "$external_hyperpixel_module_loaded" = true && test "$external_hyperpixel_binding_count" = 1; }; }; then printf true; else printf false; fi); "#,
    r#"printf '{"model":"%s","os_id":"%s","os_version":"%s","architecture":"%s","kernel_release":"%s","kernel_vermagic":"%s","default_target":"%s","display_manager_active":%s,"boot_config":"%s","boot_config_regular":%s,"tryboot_supported":%s,"clock_synchronized":%s,"system_time_unix":%s,"package_repository_reachable":%s,"port_80_free":%s,"root_available_bytes":%s,"boot_available_bytes":%s,"running_headers_available":%s,"running_headers_release":"%s","installed_kernel_header_pair_count":%s,"installed_kernel_release":"%s","installed_headers_release":"%s","boot_selected_kernel_match_count":%s,"boot_selected_kernel_release":"%s","boot_kernel_override_conflicting":%s,"unsafe_overlay_present":%s,"hyperpixel_declaration_count":%s,"replace_overlay":"%s","external_hyperpixel_overlay_count":%s,"external_hyperpixel_module_loaded":%s,"unexpected_hyperpixel_module_loaded":%s,"hyperpixel_state_dir_safe":%s,"hyperpixel_transaction_active":%s,"legacy_checkpoint_active":%s,"external_hyperpixel_binding_count":%s,"gpio_display_state_safe":%s}' "#,
    r#""$model" "$os_id" "$os_version" "$architecture" "$kernel_release" "$kernel_vermagic" "$default_target" "$display_manager_active" "$boot_config" "$boot_config_regular" "$tryboot_supported" "$clock_synchronized" "$system_time_unix" "$package_repository_reachable" "$port_80_free" "$root_available_bytes" "$boot_available_bytes" "$running_headers_available" "$running_headers_release" "$installed_kernel_header_pair_count" "$installed_kernel_release" "$installed_headers_release" "$boot_selected_kernel_match_count" "$boot_selected_kernel_release" "$boot_kernel_override_conflicting" "$unsafe_overlay_present" "$hyperpixel_declaration_count" "$replace_overlay" "$external_hyperpixel_overlay_count" "$external_hyperpixel_module_loaded" "$unexpected_hyperpixel_module_loaded" "$hyperpixel_state_dir_safe" "$hyperpixel_transaction_active" "$legacy_checkpoint_active" "$external_hyperpixel_binding_count" "$gpio_display_state_safe""#,
);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CheckId {
    HostOperatingSystem,
    HostArchitecture,
    HostMacosRelease,
    HostGit,
    HostMise,
    HostSsh,
    HostGh,
    HostGithubAuthentication,
    HostDocker,
    HostBuildx,
    HostApplicationRepository,
    HostDriverRepository,
    HostDiskSpace,
    TargetIdentity,
    TargetSudo,
    TargetModel,
    TargetOperatingSystem,
    TargetArchitecture,
    TargetGraphicalEnvironment,
    TargetBootConfig,
    TargetTryboot,
    TargetClock,
    TargetPackageRepository,
    TargetPort80,
    TargetOverlay,
    TargetRootSpace,
    TargetBootSpace,
    TargetRunningHeaders,
    TargetInstalledKernelHeaders,
    TargetGpioDisplayState,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FailureCode {
    Unsupported,
    UnsupportedHostOperatingSystem,
    UnsupportedHostArchitecture,
    UnsupportedMacosRelease,
    MissingTool,
    GithubAuthenticationUnavailable,
    BuildxUnavailable,
    InvalidDockerContext,
    RepositoryUnreachable,
    IdentityMismatch,
    ProbeUnavailable,
    SudoUnavailable,
    ModelMismatch,
    UnsupportedTargetOperatingSystem,
    UnsupportedTargetArchitecture,
    GraphicalEnvironmentActive,
    InvalidBootConfig,
    TrybootUnavailable,
    ClockUnsynchronized,
    ClockOutsideTolerance,
    PackageRepositoryUnavailable,
    PortOccupied,
    UnsafeOverlay,
    InsufficientSpace,
    RunningHeadersUnavailable,
    RunningHeadersMismatch,
    InstalledKernelHeadersMismatch,
    BootSelectedKernelUnproven,
    UnexpectedGpioDisplayState,
    MalformedFacts,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum HostTool {
    Git,
    Mise,
    Ssh,
    Gh,
    Docker,
}

#[derive(Clone, Eq, PartialEq)]
pub struct HostFacts {
    pub operating_system: String,
    pub architecture: String,
    pub macos_major: u16,
    pub available_tools: BTreeSet<HostTool>,
    pub github_authenticated: bool,
    pub buildx_available: bool,
    pub application_repository_reachable: bool,
    pub driver_repository_reachable: bool,
    pub available_disk_bytes: u64,
}

impl fmt::Debug for HostFacts {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HostFacts")
            .field("available_tool_count", &self.available_tools.len())
            .field("github_authenticated", &self.github_authenticated)
            .field("buildx_available", &self.buildx_available)
            .field(
                "application_repository_reachable",
                &self.application_repository_reachable,
            )
            .field(
                "driver_repository_reachable",
                &self.driver_repository_reachable,
            )
            .finish_non_exhaustive()
    }
}

pub fn evaluate_host(facts: &HostFacts) -> PreflightReport {
    let tool = |tool| {
        status(
            facts.available_tools.contains(&tool),
            FailureCode::MissingTool,
        )
    };
    PreflightReport::from_statuses([
        (
            CheckId::HostOperatingSystem,
            status(
                facts.operating_system == "Darwin",
                FailureCode::UnsupportedHostOperatingSystem,
            ),
        ),
        (
            CheckId::HostArchitecture,
            status(
                matches!(facts.architecture.as_str(), "arm64" | "x86_64"),
                FailureCode::UnsupportedHostArchitecture,
            ),
        ),
        (
            CheckId::HostMacosRelease,
            status(
                facts.macos_major >= MIN_MACOS_MAJOR,
                FailureCode::UnsupportedMacosRelease,
            ),
        ),
        (CheckId::HostGit, tool(HostTool::Git)),
        (CheckId::HostMise, tool(HostTool::Mise)),
        (CheckId::HostSsh, tool(HostTool::Ssh)),
        (CheckId::HostGh, tool(HostTool::Gh)),
        (
            CheckId::HostGithubAuthentication,
            status(
                facts.github_authenticated,
                FailureCode::GithubAuthenticationUnavailable,
            ),
        ),
        (CheckId::HostDocker, tool(HostTool::Docker)),
        (
            CheckId::HostBuildx,
            status(facts.buildx_available, FailureCode::BuildxUnavailable),
        ),
        (
            CheckId::HostApplicationRepository,
            status(
                facts.application_repository_reachable,
                FailureCode::RepositoryUnreachable,
            ),
        ),
        (
            CheckId::HostDriverRepository,
            status(
                facts.driver_repository_reachable,
                FailureCode::RepositoryUnreachable,
            ),
        ),
        (
            CheckId::HostDiskSpace,
            status(
                facts.available_disk_bytes >= MAC_MIN_FREE_BYTES,
                FailureCode::InsufficientSpace,
            ),
        ),
    ])
}

pub struct HostPreflight<'a, R> {
    runner: &'a R,
}

impl<'a, R: CommandRunner> HostPreflight<'a, R> {
    pub fn new(runner: &'a R) -> Self {
        Self { runner }
    }

    pub fn run(&self, repository_path: &Path, docker_context: Option<&str>) -> PreflightReport {
        let operating_system = self
            .run_text("uname", ["-s"])
            .and_then(parse_single_line)
            .unwrap_or_default();
        let architecture = self
            .run_text("uname", ["-m"])
            .and_then(parse_single_line)
            .unwrap_or_default();
        let macos_major = self
            .run_text("sw_vers", ["-productVersion"])
            .and_then(parse_single_line)
            .and_then(|version| version.split('.').next()?.parse().ok())
            .unwrap_or_default();

        let mut available_tools = BTreeSet::new();
        for (tool, program, argument) in [
            (HostTool::Git, "git", "--version"),
            (HostTool::Mise, "mise", "--version"),
            (HostTool::Ssh, "ssh", "-V"),
            (HostTool::Gh, "gh", "--version"),
        ] {
            if self.run_succeeded(program, [argument]) {
                available_tools.insert(tool);
            }
        }
        let github_authenticated = self.run_succeeded(
            "gh",
            ["auth", "status", "--active", "--hostname", "github.com"],
        );
        if self.run_succeeded("docker", ["--version"]) {
            available_tools.insert(HostTool::Docker);
        }

        let context_valid = docker_context.is_none_or(valid_docker_context);
        let buildx_available = if context_valid {
            let mut arguments = Vec::new();
            if let Some(context) = docker_context {
                arguments.extend(["--context", context]);
            }
            arguments.extend(["buildx", "inspect"]);
            self.run_succeeded("docker", arguments)
        } else {
            false
        };
        let application_repository_reachable = self.run_succeeded(
            "git",
            ["ls-remote", "--exit-code", APPLICATION_REPOSITORY, "HEAD"],
        );
        let driver_repository_reachable = self.run_succeeded(
            "git",
            ["ls-remote", "--exit-code", DRIVER_REPOSITORY, "HEAD"],
        );
        let available_disk_bytes = repository_path
            .to_str()
            .and_then(|path| self.run_text("df", ["-Pk", path]))
            .and_then(parse_df_available_bytes)
            .unwrap_or_default();
        let mut report = evaluate_host(&HostFacts {
            operating_system,
            architecture,
            macos_major,
            available_tools,
            github_authenticated,
            buildx_available,
            application_repository_reachable,
            driver_repository_reachable,
            available_disk_bytes,
        });
        if !context_valid
            && let Some(check) = report
                .checks
                .iter_mut()
                .find(|check| check.id == CheckId::HostBuildx)
        {
            check.status = CheckStatus::Failed(FailureCode::InvalidDockerContext);
        }
        report
    }

    fn run_succeeded<I, S>(&self, program: &str, arguments: I) -> bool
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.runner
            .run(
                Invocation::new(program, arguments.into_iter().map(Into::into).collect())
                    .with_timeout(HOST_PROBE_TIMEOUT),
            )
            .is_ok_and(|output| {
                output.status() == 0
                    && output.stdout().len() <= MAX_HOST_OUTPUT_BYTES
                    && output.stderr().len() <= MAX_HOST_OUTPUT_BYTES
            })
    }

    fn run_text<I, S>(&self, program: &str, arguments: I) -> Option<Vec<u8>>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let output = self
            .runner
            .run(
                Invocation::new(program, arguments.into_iter().map(Into::into).collect())
                    .with_timeout(HOST_PROBE_TIMEOUT),
            )
            .ok()?;
        (output.status() == 0
            && output.stdout().len() <= MAX_HOST_OUTPUT_BYTES
            && output.stderr().len() <= MAX_HOST_OUTPUT_BYTES)
            .then(|| output.stdout().to_vec())
    }
}

fn valid_docker_context(context: &str) -> bool {
    !context.is_empty()
        && context.len() <= 128
        && context.is_ascii()
        && context
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn parse_single_line(bytes: Vec<u8>) -> Option<String> {
    let text = std::str::from_utf8(&bytes).ok()?;
    let text = text.strip_suffix('\n').unwrap_or(text);
    if text.is_empty()
        || text.contains(['\r', '\n'])
        || text.bytes().any(|byte| byte.is_ascii_control())
    {
        return None;
    }
    Some(text.to_owned())
}

fn parse_df_available_bytes(bytes: Vec<u8>) -> Option<u64> {
    let text = std::str::from_utf8(&bytes).ok()?;
    let row = text.lines().rfind(|line| !line.trim().is_empty())?;
    let blocks = row.split_whitespace().nth(3)?.parse::<u64>().ok()?;
    blocks.checked_mul(1024)
}

pub trait UnixClock {
    fn now_unix_seconds(&self) -> u64;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SystemUnixClock;

impl UnixClock for SystemUnixClock {
    fn now_unix_seconds(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_secs())
    }
}

pub struct TargetPreflight<'a, T, C> {
    transport: &'a T,
    clock: C,
}

impl<'a, T: Transport, C: UnixClock> TargetPreflight<'a, T, C> {
    pub fn new(transport: &'a T, clock: C) -> Self {
        Self { transport, clock }
    }

    pub fn run(&self, target: &SshTarget, expected_identity: &TargetIdentity) -> PreflightReport {
        let observed = self
            .transport
            .probe(target)
            .ok()
            .map(|probe| probe.identity);
        if observed
            .as_ref()
            .is_none_or(|identity| !expected_identity.matches(identity))
        {
            return evaluate_target(
                expected_identity,
                observed.as_ref(),
                false,
                Err(TargetFactsError::InvalidJson),
                self.clock.now_unix_seconds(),
            );
        }

        let sudo_available = RemoteCommand::ordinary(["sudo", "-n", "true"])
            .ok()
            .and_then(|request| self.transport.run(target, request).ok())
            .is_some_and(|output| output.status() == 0)
            || RemoteCommand::interactive_sudo(["sudo", "true"])
                .ok()
                .and_then(|request| self.transport.run(target, request).ok())
                .is_some_and(|output| output.status() == 0);
        let facts = self.facts(target);
        evaluate_target(
            expected_identity,
            observed.as_ref(),
            sudo_available,
            facts.as_ref().map_err(|error| *error),
            self.clock.now_unix_seconds(),
        )
    }

    pub fn facts(&self, target: &SshTarget) -> Result<TargetFacts, TargetFactsError> {
        RemoteCommand::ordinary(["sudo", "-n", "sh", "-c", TARGET_FACTS_SCRIPT])
            .ok()
            .and_then(|request| self.transport.run(target, request).ok())
            .map_or(Err(TargetFactsError::InvalidJson), |output| {
                if output.status() == 0 {
                    TargetFacts::parse(output.stdout())
                } else {
                    Err(TargetFactsError::InvalidJson)
                }
            })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CheckStatus {
    Passed,
    Failed(FailureCode),
    RebootRequired,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CheckResult {
    id: CheckId,
    status: CheckStatus,
}

impl CheckResult {
    pub fn id(&self) -> CheckId {
        self.id
    }

    pub fn status(&self) -> CheckStatus {
        self.status
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PreflightDisposition {
    Ready,
    RebootRequired,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreflightReport {
    checks: Vec<CheckResult>,
}

impl PreflightReport {
    pub fn from_statuses(statuses: impl IntoIterator<Item = (CheckId, CheckStatus)>) -> Self {
        Self {
            checks: statuses
                .into_iter()
                .map(|(id, status)| CheckResult { id, status })
                .collect(),
        }
    }

    pub fn checks(&self) -> &[CheckResult] {
        &self.checks
    }

    pub fn require_success(&self) -> Result<PreflightDisposition, PreflightError> {
        let blocking = self
            .checks
            .iter()
            .filter_map(|check| matches!(check.status, CheckStatus::Failed(_)).then_some(check.id))
            .collect::<Vec<_>>();
        if !blocking.is_empty() {
            return Err(PreflightError::BlockingFailures(blocking));
        }
        Ok(
            if self
                .checks
                .iter()
                .any(|check| check.status == CheckStatus::RebootRequired)
            {
                PreflightDisposition::RebootRequired
            } else {
                PreflightDisposition::Ready
            },
        )
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum PreflightError {
    #[error("preflight found blocking failures")]
    BlockingFailures(Vec<CheckId>),
}

#[derive(Clone, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct TargetFacts {
    pub model: String,
    pub os_id: String,
    pub os_version: String,
    pub architecture: String,
    pub kernel_release: String,
    pub kernel_vermagic: String,
    pub default_target: String,
    pub display_manager_active: bool,
    pub boot_config: String,
    pub boot_config_regular: bool,
    pub tryboot_supported: bool,
    pub clock_synchronized: bool,
    pub system_time_unix: u64,
    pub package_repository_reachable: bool,
    pub port_80_free: bool,
    pub root_available_bytes: u64,
    pub boot_available_bytes: u64,
    pub running_headers_available: bool,
    pub running_headers_release: String,
    pub installed_kernel_header_pair_count: u8,
    pub installed_kernel_release: String,
    pub installed_headers_release: String,
    pub boot_selected_kernel_match_count: u8,
    pub boot_selected_kernel_release: String,
    pub boot_kernel_override_conflicting: bool,
    pub unsafe_overlay_present: bool,
    pub hyperpixel_declaration_count: u8,
    pub replace_overlay: String,
    pub external_hyperpixel_overlay_count: u8,
    pub external_hyperpixel_module_loaded: bool,
    pub unexpected_hyperpixel_module_loaded: bool,
    pub hyperpixel_state_dir_safe: bool,
    pub hyperpixel_transaction_active: bool,
    pub legacy_checkpoint_active: bool,
    pub external_hyperpixel_binding_count: u8,
    pub gpio_display_state_safe: bool,
}

impl fmt::Debug for TargetFacts {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TargetFacts")
            .field("field_count", &36)
            .finish_non_exhaustive()
    }
}

impl TargetFacts {
    pub fn parse(input: &[u8]) -> Result<Self, TargetFactsError> {
        if input.len() > MAX_TARGET_FACTS_BYTES {
            return Err(TargetFactsError::OutputTooLarge);
        }
        let facts: Self =
            serde_json::from_slice(input).map_err(|_| TargetFactsError::InvalidJson)?;
        facts.validate()?;
        Ok(facts)
    }

    fn validate(&self) -> Result<(), TargetFactsError> {
        for value in [
            &self.model,
            &self.os_id,
            &self.os_version,
            &self.architecture,
            &self.kernel_release,
            &self.kernel_vermagic,
            &self.default_target,
            &self.boot_config,
        ] {
            require_canonical_field(value, false)?;
        }
        require_canonical_field(
            &self.running_headers_release,
            !self.running_headers_available,
        )?;
        require_canonical_field(&self.replace_overlay, true)?;
        if !is_supported_hyperpixel_overlay(&self.replace_overlay) {
            return Err(TargetFactsError::NoncanonicalField);
        }
        require_kernel_release(&self.kernel_release)?;
        crate::driver::TargetProbe::new(self.kernel_release.clone(), self.kernel_vermagic.clone())
            .map_err(|_| TargetFactsError::NoncanonicalField)?;
        if !self.running_headers_release.is_empty() {
            require_kernel_release(&self.running_headers_release)?;
        }
        if !(MIN_SYSTEM_TIME_UNIX..=MAX_SYSTEM_TIME_UNIX).contains(&self.system_time_unix)
            || self.root_available_bytes > MAX_REPORTED_FREE_BYTES
            || self.boot_available_bytes > MAX_REPORTED_FREE_BYTES
            || self.installed_kernel_header_pair_count > 16
            || self.boot_selected_kernel_match_count > 16
            || self.hyperpixel_declaration_count > 16
            || self.external_hyperpixel_overlay_count > 16
            || self.external_hyperpixel_binding_count > 16
        {
            return Err(TargetFactsError::NoncanonicalField);
        }
        match self.installed_kernel_header_pair_count {
            0 if self.installed_kernel_release.is_empty()
                && self.installed_headers_release.is_empty() => {}
            0 => return Err(TargetFactsError::NoncanonicalField),
            _ if !self.installed_kernel_release.is_empty()
                && !self.installed_headers_release.is_empty() =>
            {
                require_canonical_field(&self.installed_kernel_release, false)?;
                require_canonical_field(&self.installed_headers_release, false)?;
                require_kernel_release(&self.installed_kernel_release)?;
                require_kernel_release(&self.installed_headers_release)?;
            }
            _ => return Err(TargetFactsError::NoncanonicalField),
        }
        match self.boot_selected_kernel_match_count {
            0 if self.boot_selected_kernel_release.is_empty() => {}
            0 => return Err(TargetFactsError::NoncanonicalField),
            _ if !self.boot_selected_kernel_release.is_empty() => {
                require_canonical_field(&self.boot_selected_kernel_release, false)?;
                require_kernel_release(&self.boot_selected_kernel_release)?;
            }
            _ => return Err(TargetFactsError::NoncanonicalField),
        }
        match self.hyperpixel_declaration_count {
            0 if self.replace_overlay.is_empty() => {}
            1 if !self.replace_overlay.is_empty() => {}
            count if count > 1 && self.replace_overlay.is_empty() => {}
            _ => return Err(TargetFactsError::NoncanonicalField),
        }
        if self.hyperpixel_declaration_count > 1 && !self.unsafe_overlay_present {
            return Err(TargetFactsError::NoncanonicalField);
        }
        if !self.unsafe_overlay_present {
            let expected_external = u8::from(
                self.replace_overlay
                    .strip_prefix("hyperpixel2r-kms-")
                    .and_then(|suffix| suffix.strip_suffix(".dtbo"))
                    .is_some(),
            );
            if self.external_hyperpixel_overlay_count != expected_external {
                return Err(TargetFactsError::NoncanonicalField);
            }
        }
        if self.gpio_display_state_safe != gpio_display_state_is_safe(self) {
            return Err(TargetFactsError::NoncanonicalField);
        }
        Ok(())
    }
}

pub(crate) fn is_supported_hyperpixel_overlay(value: &str) -> bool {
    if value.is_empty() || value == "vc4-kms-dpi-hyperpixel2r" {
        return true;
    }
    let revision = value.strip_prefix("planeradar-hyperpixel2r-").or_else(|| {
        value
            .strip_prefix("hyperpixel2r-kms-")
            .and_then(|suffix| suffix.strip_suffix(".dtbo"))
    });
    revision.is_some_and(|revision| {
        revision.len() == 12
            && revision
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    })
}

fn require_canonical_field(value: &str, allow_empty: bool) -> Result<(), TargetFactsError> {
    if (!allow_empty && value.is_empty())
        || value.len() > MAX_FACT_FIELD_BYTES
        || !value.is_ascii()
        || value
            .bytes()
            .any(|byte| byte.is_ascii_control() || (!byte.is_ascii_graphic() && byte != b' '))
    {
        return Err(TargetFactsError::NoncanonicalField);
    }
    Ok(())
}

fn require_kernel_release(value: &str) -> Result<(), TargetFactsError> {
    if value.is_empty()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'+' | b'-' | b'_'))
    {
        return Err(TargetFactsError::NoncanonicalField);
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum TargetFactsError {
    #[error("target facts output exceeds its bounded size")]
    OutputTooLarge,
    #[error("target facts are not one complete strict JSON value")]
    InvalidJson,
    #[error("target facts contain a noncanonical field")]
    NoncanonicalField,
}

pub fn evaluate_target(
    expected_identity: &TargetIdentity,
    observed_identity: Option<&TargetIdentity>,
    sudo_available: bool,
    facts: Result<&TargetFacts, TargetFactsError>,
    host_time_unix: u64,
) -> PreflightReport {
    let identity_status = match observed_identity {
        Some(observed) if expected_identity.matches(observed) => CheckStatus::Passed,
        Some(_) => CheckStatus::Failed(FailureCode::IdentityMismatch),
        None => CheckStatus::Failed(FailureCode::ProbeUnavailable),
    };
    let sudo_status = status(sudo_available, FailureCode::SudoUnavailable);
    let mut checks = vec![
        (CheckId::TargetIdentity, identity_status),
        (CheckId::TargetSudo, sudo_status),
    ];
    let Ok(facts) = facts else {
        checks.extend(
            TARGET_FACT_CHECK_IDS.map(|id| (id, CheckStatus::Failed(FailureCode::MalformedFacts))),
        );
        return PreflightReport::from_statuses(checks);
    };

    checks.extend([
        (
            CheckId::TargetModel,
            status(
                supported_model(&facts.model)
                    && observed_identity.is_some_and(|observed| observed.model == facts.model),
                FailureCode::ModelMismatch,
            ),
        ),
        (
            CheckId::TargetOperatingSystem,
            status(
                matches!(facts.os_id.as_str(), "debian" | "raspbian") && facts.os_version == "13",
                FailureCode::UnsupportedTargetOperatingSystem,
            ),
        ),
        (
            CheckId::TargetArchitecture,
            status(
                facts.architecture == "arm64",
                FailureCode::UnsupportedTargetArchitecture,
            ),
        ),
        (
            CheckId::TargetGraphicalEnvironment,
            status(
                facts.default_target == "multi-user.target" && !facts.display_manager_active,
                FailureCode::GraphicalEnvironmentActive,
            ),
        ),
        (
            CheckId::TargetBootConfig,
            status(
                facts.boot_config == "/boot/firmware/config.txt" && facts.boot_config_regular,
                FailureCode::InvalidBootConfig,
            ),
        ),
        (
            CheckId::TargetTryboot,
            status(facts.tryboot_supported, FailureCode::TrybootUnavailable),
        ),
        (
            CheckId::TargetClock,
            if !facts.clock_synchronized {
                CheckStatus::Failed(FailureCode::ClockUnsynchronized)
            } else {
                status(
                    facts.system_time_unix.abs_diff(host_time_unix) <= MAX_CLOCK_SKEW_SECONDS,
                    FailureCode::ClockOutsideTolerance,
                )
            },
        ),
        (
            CheckId::TargetPackageRepository,
            status(
                facts.package_repository_reachable,
                FailureCode::PackageRepositoryUnavailable,
            ),
        ),
        (
            CheckId::TargetPort80,
            status(facts.port_80_free, FailureCode::PortOccupied),
        ),
        (
            CheckId::TargetOverlay,
            status(!facts.unsafe_overlay_present, FailureCode::UnsafeOverlay),
        ),
        (
            CheckId::TargetRootSpace,
            status(
                facts.root_available_bytes >= TARGET_ROOT_MIN_FREE_BYTES,
                FailureCode::InsufficientSpace,
            ),
        ),
        (
            CheckId::TargetBootSpace,
            status(
                facts.boot_available_bytes >= TARGET_BOOT_MIN_FREE_BYTES,
                FailureCode::InsufficientSpace,
            ),
        ),
    ]);
    let installed_pair_matches = facts.installed_kernel_header_pair_count == 1
        && facts.installed_kernel_release == facts.installed_headers_release;
    let boot_selection_proven = facts.boot_selected_kernel_match_count == 1
        && !facts.boot_kernel_override_conflicting
        && facts.boot_selected_kernel_release == facts.installed_kernel_release;
    let running_matches =
        facts.running_headers_available && facts.running_headers_release == facts.kernel_release;
    let safe_alternate = installed_pair_matches
        && boot_selection_proven
        && facts.installed_kernel_release != facts.kernel_release;
    let running_status = if running_matches {
        CheckStatus::Passed
    } else if safe_alternate {
        CheckStatus::RebootRequired
    } else if facts.running_headers_available {
        CheckStatus::Failed(FailureCode::RunningHeadersMismatch)
    } else {
        CheckStatus::Failed(FailureCode::RunningHeadersUnavailable)
    };
    let installed_status = if !installed_pair_matches {
        CheckStatus::Failed(FailureCode::InstalledKernelHeadersMismatch)
    } else if !boot_selection_proven {
        CheckStatus::Failed(FailureCode::BootSelectedKernelUnproven)
    } else if facts.installed_kernel_release != facts.kernel_release {
        CheckStatus::RebootRequired
    } else {
        CheckStatus::Passed
    };
    checks.extend([
        (CheckId::TargetRunningHeaders, running_status),
        (CheckId::TargetInstalledKernelHeaders, installed_status),
        (
            CheckId::TargetGpioDisplayState,
            status(
                facts.gpio_display_state_safe && gpio_display_state_is_safe(facts),
                FailureCode::UnexpectedGpioDisplayState,
            ),
        ),
    ]);
    PreflightReport::from_statuses(checks)
}

fn gpio_display_state_is_safe(facts: &TargetFacts) -> bool {
    if facts.hyperpixel_transaction_active
        || !facts.hyperpixel_state_dir_safe
        || facts.legacy_checkpoint_active
        || facts.unexpected_hyperpixel_module_loaded
    {
        return false;
    }
    matches!(
        (
            facts.external_hyperpixel_overlay_count,
            facts.external_hyperpixel_module_loaded,
            facts.external_hyperpixel_binding_count,
        ),
        (0, false, 0) | (1, true, 1)
    )
}

fn supported_model(model: &str) -> bool {
    model == "Raspberry Pi Zero 2 W"
        || model
            .strip_prefix("Raspberry Pi Zero 2 W Rev ")
            .is_some_and(|revision| {
                !revision.is_empty()
                    && revision
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || byte == b'.')
            })
}

fn status(passed: bool, failure: FailureCode) -> CheckStatus {
    if passed {
        CheckStatus::Passed
    } else {
        CheckStatus::Failed(failure)
    }
}

const TARGET_FACT_CHECK_IDS: [CheckId; 15] = [
    CheckId::TargetModel,
    CheckId::TargetOperatingSystem,
    CheckId::TargetArchitecture,
    CheckId::TargetGraphicalEnvironment,
    CheckId::TargetBootConfig,
    CheckId::TargetTryboot,
    CheckId::TargetClock,
    CheckId::TargetPackageRepository,
    CheckId::TargetPort80,
    CheckId::TargetOverlay,
    CheckId::TargetRootSpace,
    CheckId::TargetBootSpace,
    CheckId::TargetRunningHeaders,
    CheckId::TargetInstalledKernelHeaders,
    CheckId::TargetGpioDisplayState,
];

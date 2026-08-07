use std::collections::VecDeque;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;
use std::sync::Mutex;

use planeradarctl::preflight::{
    APPLICATION_REPOSITORY, CheckId, CheckStatus, DRIVER_REPOSITORY, FailureCode,
    HOST_PROBE_TIMEOUT, HostFacts, HostPreflight, HostTool, MAC_MIN_FREE_BYTES, MIN_MACOS_MAJOR,
    PreflightDisposition, PreflightReport, TARGET_BOOT_MIN_FREE_BYTES, TARGET_FACTS_SCRIPT,
    TARGET_ROOT_MIN_FREE_BYTES, TargetFacts, TargetFactsError, TargetPreflight, UnixClock,
    evaluate_host, evaluate_target,
};
use planeradarctl::target::{SshTarget, TargetIdentity};
use planeradarctl::transport::{
    CommandOutput, CommandRunner, Invocation, Output, ReconnectPolicy, RemoteCommand, RunnerError,
    TargetProbe, Transport, TransportError,
};
use std::str::FromStr;

#[test]
fn report_rejects_a_blocking_check_and_preserves_stable_order() {
    let report = PreflightReport::from_statuses([
        (CheckId::HostOperatingSystem, CheckStatus::Passed),
        (
            CheckId::HostArchitecture,
            CheckStatus::Failed(planeradarctl::preflight::FailureCode::Unsupported),
        ),
    ]);

    assert_eq!(
        report
            .checks()
            .iter()
            .map(|check| check.id())
            .collect::<Vec<_>>(),
        [CheckId::HostOperatingSystem, CheckId::HostArchitecture]
    );
    assert!(report.require_success().is_err());

    let reboot = PreflightReport::from_statuses([(
        CheckId::TargetInstalledKernelHeaders,
        CheckStatus::RebootRequired,
    )]);
    assert_eq!(
        reboot.require_success().expect("valid reboot disposition"),
        PreflightDisposition::RebootRequired
    );
}

fn identity() -> TargetIdentity {
    TargetIdentity {
        host_key_sha256: "SHA256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".into(),
        model: "Raspberry Pi Zero 2 W Rev 1.0".into(),
        serial: "0123456789abcdef".into(),
    }
}

fn valid_target_json() -> Vec<u8> {
    br#"{
      "model":"Raspberry Pi Zero 2 W Rev 1.0",
      "os_id":"debian",
      "os_version":"13",
      "architecture":"arm64",
      "kernel_release":"6.18.34+rpt-rpi-v8",
      "kernel_vermagic":"6.18.34+rpt-rpi-v8 SMP preempt mod_unload modversions aarch64",
      "default_target":"multi-user.target",
      "display_manager_active":false,
      "boot_config":"/boot/firmware/config.txt",
      "boot_config_regular":true,
      "tryboot_supported":true,
      "clock_synchronized":true,
      "system_time_unix":1785196800,
      "package_repository_reachable":true,
      "port_80_free":true,
      "root_available_bytes":4294967296,
      "boot_available_bytes":268435456,
      "running_headers_available":true,
      "running_headers_release":"6.18.34+rpt-rpi-v8",
      "installed_kernel_header_pair_count":1,
      "installed_kernel_release":"6.18.34+rpt-rpi-v8",
      "installed_headers_release":"6.18.34+rpt-rpi-v8",
      "boot_selected_kernel_match_count":1,
      "boot_selected_kernel_release":"6.18.34+rpt-rpi-v8",
      "candidate_kernel_release":"6.18.34+rpt-rpi-v8",
      "candidate_kernel_vermagic":"6.18.34+rpt-rpi-v8 SMP preempt mod_unload modversions aarch64",
      "candidate_kernel_match_count":1,
      "boot_kernel_override_conflicting":false,
      "unsafe_overlay_present":false,
      "hyperpixel_declaration_count":0,
      "replace_overlay":"",
      "external_hyperpixel_overlay_count":0,
      "external_hyperpixel_module_loaded":false,
      "unexpected_hyperpixel_module_loaded":false,
      "hyperpixel_state_dir_safe":true,
      "hyperpixel_transaction_active":false,
      "legacy_checkpoint_active":false,
      "external_hyperpixel_binding_count":0,
      "gpio_display_state_safe":true
    }"#
    .to_vec()
}

fn parsed_target() -> TargetFacts {
    TargetFacts::parse(&valid_target_json()).expect("valid target facts")
}

fn target_status(report: &PreflightReport, id: CheckId) -> CheckStatus {
    report
        .checks()
        .iter()
        .find(|check| check.id() == id)
        .expect("target check")
        .status()
}

#[test]
fn target_facts_accept_one_complete_canonical_json_value() {
    let facts = parsed_target();
    assert_eq!(facts.os_id, "debian");
    assert_eq!(facts.system_time_unix, 1_785_196_800);
    assert_eq!(
        facts.kernel_vermagic,
        "6.18.34+rpt-rpi-v8 SMP preempt mod_unload modversions aarch64"
    );
    assert_eq!(facts.hyperpixel_declaration_count, 0);
    assert_eq!(facts.replace_overlay, "");
}

#[test]
fn target_facts_bind_the_exact_candidate_kernel_or_running_fallback() {
    let selected_fixture = String::from_utf8(valid_target_json()).expect("fixture utf8");
    let fallback_fixture = selected_fixture.replacen(
        "\"candidate_kernel_match_count\":1",
        "\"candidate_kernel_match_count\":0",
        1,
    );
    let fallback = TargetFacts::parse(fallback_fixture.as_bytes()).expect("running fallback");
    assert_eq!(fallback.candidate_kernel_match_count, 0);
    assert_eq!(fallback.candidate_kernel_release, fallback.kernel_release);
    assert_eq!(fallback.candidate_kernel_vermagic, fallback.kernel_vermagic);

    let selected = selected_fixture
        .replacen(
            "\"candidate_kernel_release\":\"6.18.34+rpt-rpi-v8\"",
            "\"candidate_kernel_release\":\"6.18.35+rpt-rpi-v8\"",
            1,
        )
        .replacen(
            "\"candidate_kernel_vermagic\":\"6.18.34+rpt-rpi-v8 SMP preempt mod_unload modversions aarch64\"",
            "\"candidate_kernel_vermagic\":\"6.18.35+rpt-rpi-v8 SMP preempt mod_unload modversions aarch64\"",
            1,
        );
    let selected = TargetFacts::parse(selected.as_bytes()).expect("one selected candidate");
    assert_eq!(selected.candidate_kernel_release, "6.18.35+rpt-rpi-v8");

    for malformed in [
        selected_fixture.replacen("\"candidate_kernel_match_count\":1", "\"candidate_kernel_match_count\":2", 1),
        selected_fixture.replacen(
            "\"candidate_kernel_release\":\"6.18.34+rpt-rpi-v8\"",
            "\"candidate_kernel_release\":\"6.18.35/rpt-rpi-v8\"",
            1,
        ),
        selected_fixture.replacen(
            "\"candidate_kernel_vermagic\":\"6.18.34+rpt-rpi-v8 SMP preempt mod_unload modversions aarch64\"",
            "\"candidate_kernel_vermagic\":\"6.18.35+rpt-rpi-v8 SMP preempt mod_unload modversions aarch64\"",
            1,
        ),
    ] {
        assert_eq!(
            TargetFacts::parse(malformed.as_bytes()),
            Err(TargetFactsError::NoncanonicalField)
        );
    }

    for fragment in [
        "candidate_kernel_release",
        "candidate_kernel_vermagic",
        "candidate_kernel_match_count",
        "readlink -f -- /boot/vmlinuz",
        "readlink -f -- /boot/initrd.img",
        "/usr/sbin/modinfo -k \"$candidate_kernel_release\" -F vermagic vc4",
    ] {
        assert!(
            TARGET_FACTS_SCRIPT.contains(fragment),
            "candidate discovery omitted {fragment}"
        );
    }
}

#[test]
fn target_facts_bind_the_exact_supported_overlay_that_driver_staging_must_replace() {
    let valid = String::from_utf8(valid_target_json()).expect("fixture utf8");
    for overlay in [
        "vc4-kms-dpi-hyperpixel2r",
        "planeradar-hyperpixel2r-eefaf3ae40fd",
        "hyperpixel2r-kms-224cc7ab7817.dtbo",
    ] {
        let mut existing = valid
            .replacen(
                "\"hyperpixel_declaration_count\":0",
                "\"hyperpixel_declaration_count\":1",
                1,
            )
            .replacen(
                "\"replace_overlay\":\"\"",
                &format!("\"replace_overlay\":\"{overlay}\""),
                1,
            );
        if overlay.starts_with("hyperpixel2r-kms-") {
            existing = existing
                .replacen(
                    "\"external_hyperpixel_overlay_count\":0",
                    "\"external_hyperpixel_overlay_count\":1",
                    1,
                )
                .replacen(
                    "\"external_hyperpixel_module_loaded\":false",
                    "\"external_hyperpixel_module_loaded\":true",
                    1,
                )
                .replacen(
                    "\"external_hyperpixel_binding_count\":0",
                    "\"external_hyperpixel_binding_count\":1",
                    1,
                );
        }
        let facts = TargetFacts::parse(existing.as_bytes()).expect("supported existing overlay");
        assert_eq!(facts.replace_overlay, overlay);
    }

    for hostile in [
        valid.replacen(
            "\"replace_overlay\":\"\"",
            "\"replace_overlay\":\"foreign-overlay\"",
            1,
        ),
        valid
            .replacen(
                "\"hyperpixel_declaration_count\":0",
                "\"hyperpixel_declaration_count\":1",
                1,
            )
            .replacen(
                "\"replace_overlay\":\"\"",
                "\"replace_overlay\":\"hyperpixel2r-kms-224cc7ab7817.dtbo,rotate=90\"",
                1,
            ),
        valid.replacen(
            "\"hyperpixel_declaration_count\":0",
            "\"hyperpixel_declaration_count\":1",
            1,
        ),
    ] {
        assert_eq!(
            TargetFacts::parse(hostile.as_bytes()),
            Err(TargetFactsError::NoncanonicalField)
        );
    }
}

#[test]
fn target_facts_require_exact_single_line_running_kernel_vermagic() {
    let valid = String::from_utf8(valid_target_json()).expect("fixture utf8");
    let expected =
        "\"kernel_vermagic\":\"6.18.34+rpt-rpi-v8 SMP preempt mod_unload modversions aarch64\",";
    for replacement in [
        "",
        "\"kernel_vermagic\":\"6.18.35+rpt-rpi-v8 SMP preempt mod_unload modversions aarch64\",",
        "\"kernel_vermagic\":\"6.18.34+rpt-rpi-v8\",",
        "\"kernel_vermagic\":\"6.18.34+rpt-rpi-v8 SMP\\npreempt\",",
        "\"kernel_vermagic\":\"6.18.34+rpt-rpi-v8 SMP\\u0000preempt\",",
    ] {
        let hostile = valid.replacen(expected, replacement, 1);
        assert!(
            TargetFacts::parse(hostile.as_bytes()).is_err(),
            "accepted replacement {replacement:?}"
        );
    }
}

#[test]
fn target_facts_script_uses_only_the_fixed_supported_vc4_vermagic_probe() {
    assert!(TARGET_FACTS_SCRIPT.contains("/usr/sbin/modinfo -F vermagic vc4"));
    assert!(!TARGET_FACTS_SCRIPT.contains("find "));
}

#[test]
fn target_facts_reject_unknown_duplicate_trailing_invalid_and_oversized_input() {
    let unknown = String::from_utf8(valid_target_json())
        .expect("fixture utf8")
        .replacen("\"model\":", "\"surprise\":true,\"model\":", 1);
    assert_eq!(
        TargetFacts::parse(unknown.as_bytes()),
        Err(TargetFactsError::InvalidJson)
    );

    let duplicate = String::from_utf8(valid_target_json())
        .expect("fixture utf8")
        .replacen("\"model\":", "\"model\":\"duplicate\",\"model\":", 1);
    assert_eq!(
        TargetFacts::parse(duplicate.as_bytes()),
        Err(TargetFactsError::InvalidJson)
    );

    let mut trailing = valid_target_json();
    trailing.extend_from_slice(b"\nsecond value");
    assert_eq!(
        TargetFacts::parse(&trailing),
        Err(TargetFactsError::InvalidJson)
    );

    let control = String::from_utf8(valid_target_json())
        .expect("fixture utf8")
        .replacen("debian", "deb\\u0000ian", 1);
    assert_eq!(
        TargetFacts::parse(control.as_bytes()),
        Err(TargetFactsError::NoncanonicalField)
    );

    let invalid_utf8 = [b"{\"model\":\"".as_slice(), &[0xff], b"\"}".as_slice()].concat();
    assert_eq!(
        TargetFacts::parse(&invalid_utf8),
        Err(TargetFactsError::InvalidJson)
    );

    assert_eq!(
        TargetFacts::parse(&vec![
            b' ';
            planeradarctl::preflight::MAX_TARGET_FACTS_BYTES + 1
        ]),
        Err(TargetFactsError::OutputTooLarge)
    );

    let unbounded = String::from_utf8(valid_target_json())
        .expect("fixture utf8")
        .replacen("debian", &"a".repeat(129), 1);
    assert_eq!(
        TargetFacts::parse(unbounded.as_bytes()),
        Err(TargetFactsError::NoncanonicalField)
    );
}

#[test]
fn successful_trixie_lite_target_is_ready_and_preserves_check_order() {
    let expected = identity();
    let report = evaluate_target(
        &expected,
        Some(&expected),
        true,
        Ok(&parsed_target()),
        1_785_196_800,
    );

    assert_eq!(
        report.require_success().expect("supported target"),
        PreflightDisposition::Ready
    );
    assert_eq!(
        report.checks().first().map(|check| check.id()),
        Some(CheckId::TargetIdentity)
    );
    assert_eq!(
        report.checks().last().map(|check| check.id()),
        Some(CheckId::TargetGpioDisplayState)
    );
}

#[test]
fn target_identity_sudo_model_and_platform_fail_with_typed_codes() {
    let expected = identity();
    let mut observed = expected.clone();
    observed.serial = "fedcba9876543210".into();
    let facts = parsed_target();
    let identity_report = evaluate_target(
        &expected,
        Some(&observed),
        true,
        Ok(&facts),
        facts.system_time_unix,
    );
    assert_eq!(
        target_status(&identity_report, CheckId::TargetIdentity),
        CheckStatus::Failed(FailureCode::IdentityMismatch)
    );

    let sudo_report = evaluate_target(
        &expected,
        Some(&expected),
        false,
        Ok(&facts),
        facts.system_time_unix,
    );
    assert_eq!(
        target_status(&sudo_report, CheckId::TargetSudo),
        CheckStatus::Failed(FailureCode::SudoUnavailable)
    );

    let mut wrong_model = facts.clone();
    wrong_model.model = "Raspberry Pi 4 Model B".into();
    let report = evaluate_target(
        &expected,
        Some(&expected),
        true,
        Ok(&wrong_model),
        facts.system_time_unix,
    );
    assert_eq!(
        target_status(&report, CheckId::TargetModel),
        CheckStatus::Failed(FailureCode::ModelMismatch)
    );
    let mut disagreeing_model = facts.clone();
    disagreeing_model.model = "Raspberry Pi Zero 2 W Rev 1.1".into();
    let report = evaluate_target(
        &expected,
        Some(&expected),
        true,
        Ok(&disagreeing_model),
        facts.system_time_unix,
    );
    assert_eq!(
        target_status(&report, CheckId::TargetModel),
        CheckStatus::Failed(FailureCode::ModelMismatch)
    );

    for (invalid, id, failure) in [
        (
            {
                let mut value = facts.clone();
                value.os_version = "12".into();
                value
            },
            CheckId::TargetOperatingSystem,
            FailureCode::UnsupportedTargetOperatingSystem,
        ),
        (
            {
                let mut value = facts.clone();
                value.architecture = "armhf".into();
                value
            },
            CheckId::TargetArchitecture,
            FailureCode::UnsupportedTargetArchitecture,
        ),
        (
            {
                let mut value = facts.clone();
                value.default_target = "graphical.target".into();
                value
            },
            CheckId::TargetGraphicalEnvironment,
            FailureCode::GraphicalEnvironmentActive,
        ),
        (
            {
                let mut value = facts.clone();
                value.display_manager_active = true;
                value
            },
            CheckId::TargetGraphicalEnvironment,
            FailureCode::GraphicalEnvironmentActive,
        ),
    ] {
        let report = evaluate_target(
            &expected,
            Some(&expected),
            true,
            Ok(&invalid),
            facts.system_time_unix,
        );
        assert_eq!(target_status(&report, id), CheckStatus::Failed(failure));
    }
}

#[test]
fn target_boot_time_repository_port_overlay_and_gpio_cases_are_explicit() {
    let expected = identity();
    let facts = parsed_target();
    let cases = [
        (
            {
                let mut value = facts.clone();
                value.boot_config = "/boot/config.txt".into();
                value
            },
            CheckId::TargetBootConfig,
            FailureCode::InvalidBootConfig,
        ),
        (
            {
                let mut value = facts.clone();
                value.boot_config_regular = false;
                value
            },
            CheckId::TargetBootConfig,
            FailureCode::InvalidBootConfig,
        ),
        (
            {
                let mut value = facts.clone();
                value.tryboot_supported = false;
                value
            },
            CheckId::TargetTryboot,
            FailureCode::TrybootUnavailable,
        ),
        (
            {
                let mut value = facts.clone();
                value.clock_synchronized = false;
                value
            },
            CheckId::TargetClock,
            FailureCode::ClockUnsynchronized,
        ),
        (
            {
                let mut value = facts.clone();
                value.package_repository_reachable = false;
                value
            },
            CheckId::TargetPackageRepository,
            FailureCode::PackageRepositoryUnavailable,
        ),
        (
            {
                let mut value = facts.clone();
                value.port_80_free = false;
                value
            },
            CheckId::TargetPort80,
            FailureCode::PortOccupied,
        ),
        (
            {
                let mut value = facts.clone();
                value.unsafe_overlay_present = true;
                value
            },
            CheckId::TargetOverlay,
            FailureCode::UnsafeOverlay,
        ),
        (
            {
                let mut value = facts.clone();
                value.gpio_display_state_safe = false;
                value
            },
            CheckId::TargetGpioDisplayState,
            FailureCode::UnexpectedGpioDisplayState,
        ),
    ];
    for (invalid, id, failure) in cases {
        let report = evaluate_target(
            &expected,
            Some(&expected),
            true,
            Ok(&invalid),
            facts.system_time_unix,
        );
        assert_eq!(target_status(&report, id), CheckStatus::Failed(failure));
    }

    let report = evaluate_target(
        &expected,
        Some(&expected),
        true,
        Ok(&facts),
        facts.system_time_unix + planeradarctl::preflight::MAX_CLOCK_SKEW_SECONDS + 1,
    );
    assert_eq!(
        target_status(&report, CheckId::TargetClock),
        CheckStatus::Failed(FailureCode::ClockOutsideTolerance)
    );
}

#[test]
fn target_gpio_state_accepts_only_pristine_or_coherent_external_driver() {
    let expected = identity();
    let pristine = parsed_target();
    assert_eq!(
        target_status(
            &evaluate_target(
                &expected,
                Some(&expected),
                true,
                Ok(&pristine),
                pristine.system_time_unix,
            ),
            CheckId::TargetGpioDisplayState,
        ),
        CheckStatus::Passed
    );

    let mut external = pristine.clone();
    external.hyperpixel_declaration_count = 1;
    external.replace_overlay = "hyperpixel2r-kms-224cc7ab7817.dtbo".into();
    external.external_hyperpixel_overlay_count = 1;
    external.external_hyperpixel_module_loaded = true;
    external.external_hyperpixel_binding_count = 1;
    assert_eq!(
        target_status(
            &evaluate_target(
                &expected,
                Some(&expected),
                true,
                Ok(&external),
                external.system_time_unix,
            ),
            CheckId::TargetGpioDisplayState,
        ),
        CheckStatus::Passed
    );

    for hostile in [
        {
            let mut value = pristine.clone();
            value.external_hyperpixel_overlay_count = 1;
            value
        },
        {
            let mut value = pristine.clone();
            value.external_hyperpixel_module_loaded = true;
            value.external_hyperpixel_binding_count = 1;
            value
        },
        {
            let mut value = external.clone();
            value.unexpected_hyperpixel_module_loaded = true;
            value
        },
        {
            let mut value = pristine.clone();
            value.hyperpixel_state_dir_safe = false;
            value
        },
        {
            let mut value = pristine.clone();
            value.hyperpixel_transaction_active = true;
            value
        },
        {
            let mut value = pristine.clone();
            value.legacy_checkpoint_active = true;
            value
        },
        {
            let mut value = external.clone();
            value.external_hyperpixel_binding_count = 0;
            value
        },
        {
            let mut value = external.clone();
            value.external_hyperpixel_binding_count = 2;
            value
        },
    ] {
        let report = evaluate_target(
            &expected,
            Some(&expected),
            true,
            Ok(&hostile),
            hostile.system_time_unix,
        );
        assert_eq!(
            target_status(&report, CheckId::TargetGpioDisplayState),
            CheckStatus::Failed(FailureCode::UnexpectedGpioDisplayState)
        );
    }

    for state in [
        "symlink",
        "non-directory",
        "mode 0777",
        "mode 0750",
        "foreign owner",
    ] {
        let mut hostile = pristine.clone();
        hostile.hyperpixel_state_dir_safe = false;
        let report = evaluate_target(
            &expected,
            Some(&expected),
            true,
            Ok(&hostile),
            hostile.system_time_unix,
        );
        assert_eq!(
            target_status(&report, CheckId::TargetGpioDisplayState),
            CheckStatus::Failed(FailureCode::UnexpectedGpioDisplayState),
            "{state}"
        );
    }

    for binding in [
        "regular fake entry",
        "unresolved symlink",
        "out-of-tree symlink",
        "symlinked compatible leaf",
        "missing compatible leaf",
    ] {
        let mut hostile = external.clone();
        hostile.external_hyperpixel_binding_count = 0;
        let report = evaluate_target(
            &expected,
            Some(&expected),
            true,
            Ok(&hostile),
            hostile.system_time_unix,
        );
        assert_eq!(
            target_status(&report, CheckId::TargetGpioDisplayState),
            CheckStatus::Failed(FailureCode::UnexpectedGpioDisplayState),
            "{binding}"
        );
    }
}

#[test]
fn target_external_hyperpixel_probe_matches_the_driver_lifecycle_contract() {
    for fragment in [
        r#"hyperpixel2r-kms-[0-9a-f]{12}\.dtbo"#,
        r#"hyperpixel_declaration_count"#,
        r#"replace_overlay"#,
        r#"hyperpixel_state_dir=/var/lib/hyperpixel2r-kms"#,
        r#"test -L "$hyperpixel_state_dir""#,
        r#"test ! -d "$hyperpixel_state_dir""#,
        r#"stat -c '%a:%U:%G' "$hyperpixel_state_dir""#,
        r#"755:root:root"#,
        r#"test -L /var/lib/hyperpixel2r-kms/tryboot-state"#,
        r#"test -e /var/lib/hyperpixel2r-kms/tryboot-state"#,
        r#"planeradar-hyperpixel-checkpoint.service"#,
        r#"$1 == "hyperpixel2r_kms""#,
        r#"/sys/bus/platform/drivers/hyperpixel2r-kms"#,
        r#"test -L "$binding""#,
        r#"readlink -f -- "$binding""#,
        r#"/sys/devices/platform/*"#,
        r#"test ! -L "$compatible" && test -f "$compatible""#,
        r#"shayne,hyperpixel2r-kms"#,
    ] {
        assert!(
            TARGET_FACTS_SCRIPT.contains(fragment),
            "missing external-driver contract: {fragment}"
        );
    }
    assert!(!TARGET_FACTS_SCRIPT.contains(r#"hyperpixel2r-kms-[0-9a-f]{12}\.dtbo(,"#));
    assert!(
        Command::new("/bin/sh")
            .args(["-n", "-c", TARGET_FACTS_SCRIPT])
            .status()
            .expect("parse target facts script")
            .success(),
        "fixed facts script must remain valid newline-free POSIX shell"
    );
}

#[test]
fn target_boot_config_probe_rejects_symlinks_before_accepting_regular_files() {
    assert!(TARGET_FACTS_SCRIPT.contains(
        r#"boot_config_regular=$(bool test ! -L "$boot_config" && test -f "$boot_config")"#
    ));

    let expected = identity();
    let regular = parsed_target();
    let accepted = evaluate_target(
        &expected,
        Some(&expected),
        true,
        Ok(&regular),
        regular.system_time_unix,
    );
    assert_eq!(
        target_status(&accepted, CheckId::TargetBootConfig),
        CheckStatus::Passed
    );

    let mut symlink = regular.clone();
    symlink.boot_config_regular = false;
    let rejected = evaluate_target(
        &expected,
        Some(&expected),
        true,
        Ok(&symlink),
        symlink.system_time_unix,
    );
    assert_eq!(
        target_status(&rejected, CheckId::TargetBootConfig),
        CheckStatus::Failed(FailureCode::InvalidBootConfig)
    );
}

#[test]
fn target_space_thresholds_accept_exact_boundary_and_reject_one_byte_less() {
    let expected = identity();
    let mut facts = parsed_target();
    facts.root_available_bytes = TARGET_ROOT_MIN_FREE_BYTES;
    facts.boot_available_bytes = TARGET_BOOT_MIN_FREE_BYTES;
    let exact = evaluate_target(
        &expected,
        Some(&expected),
        true,
        Ok(&facts),
        facts.system_time_unix,
    );
    assert_eq!(
        target_status(&exact, CheckId::TargetRootSpace),
        CheckStatus::Passed
    );
    assert_eq!(
        target_status(&exact, CheckId::TargetBootSpace),
        CheckStatus::Passed
    );

    facts.root_available_bytes -= 1;
    facts.boot_available_bytes -= 1;
    let insufficient = evaluate_target(
        &expected,
        Some(&expected),
        true,
        Ok(&facts),
        facts.system_time_unix,
    );
    assert_eq!(
        target_status(&insufficient, CheckId::TargetRootSpace),
        CheckStatus::Failed(FailureCode::InsufficientSpace)
    );
    assert_eq!(
        target_status(&insufficient, CheckId::TargetBootSpace),
        CheckStatus::Failed(FailureCode::InsufficientSpace)
    );
}

#[test]
fn target_header_relationship_is_ready_rebootable_or_blocking() {
    let expected = identity();
    let facts = parsed_target();

    let mut reboot = facts.clone();
    reboot.running_headers_available = false;
    reboot.running_headers_release = String::new();
    reboot.installed_kernel_release = "6.18.35+rpt-rpi-v8".into();
    reboot.installed_headers_release = "6.18.35+rpt-rpi-v8".into();
    reboot.boot_selected_kernel_release = "6.18.35+rpt-rpi-v8".into();
    reboot.candidate_kernel_release = "6.18.35+rpt-rpi-v8".into();
    reboot.candidate_kernel_vermagic =
        "6.18.35+rpt-rpi-v8 SMP preempt mod_unload modversions aarch64".into();
    let report = evaluate_target(
        &expected,
        Some(&expected),
        true,
        Ok(&reboot),
        facts.system_time_unix,
    );
    assert_eq!(
        report.require_success().expect("safe pending kernel"),
        PreflightDisposition::RebootRequired
    );

    for unproven in [
        {
            let mut value = reboot.clone();
            value.boot_selected_kernel_match_count = 0;
            value.boot_selected_kernel_release.clear();
            value.candidate_kernel_match_count = 2;
            value.candidate_kernel_release.clear();
            value.candidate_kernel_vermagic.clear();
            value
        },
        {
            let mut value = reboot.clone();
            value.boot_selected_kernel_match_count = 2;
            value.candidate_kernel_match_count = 2;
            value.candidate_kernel_release.clear();
            value.candidate_kernel_vermagic.clear();
            value
        },
        {
            let mut value = reboot.clone();
            value.boot_selected_kernel_release = "6.18.36+rpt-rpi-v8".into();
            value.candidate_kernel_match_count = 2;
            value.candidate_kernel_release.clear();
            value.candidate_kernel_vermagic.clear();
            value
        },
        {
            let mut value = reboot.clone();
            value.boot_kernel_override_conflicting = true;
            value
        },
    ] {
        let report = evaluate_target(
            &expected,
            Some(&expected),
            true,
            Ok(&unproven),
            facts.system_time_unix,
        );
        assert_eq!(
            target_status(&report, CheckId::TargetInstalledKernelHeaders),
            CheckStatus::Failed(FailureCode::BootSelectedKernelUnproven)
        );
        assert!(report.require_success().is_err());
    }

    let mut invalid = reboot.clone();
    invalid.installed_headers_release = "6.18.36+rpt-rpi-v8".into();
    invalid.candidate_kernel_match_count = 2;
    invalid.candidate_kernel_release.clear();
    invalid.candidate_kernel_vermagic.clear();
    let report = evaluate_target(
        &expected,
        Some(&expected),
        true,
        Ok(&invalid),
        facts.system_time_unix,
    );
    assert_eq!(
        target_status(&report, CheckId::TargetInstalledKernelHeaders),
        CheckStatus::Failed(FailureCode::BootSelectedKernelUnproven)
    );

    let mut ambiguous = facts.clone();
    ambiguous.installed_kernel_header_pair_count = 2;
    ambiguous.candidate_kernel_match_count = 2;
    ambiguous.candidate_kernel_release.clear();
    ambiguous.candidate_kernel_vermagic.clear();
    let report = evaluate_target(
        &expected,
        Some(&expected),
        true,
        Ok(&ambiguous),
        facts.system_time_unix,
    );
    assert_eq!(
        target_status(&report, CheckId::TargetInstalledKernelHeaders),
        CheckStatus::Failed(FailureCode::BootSelectedKernelUnproven)
    );

    let mut mismatched_running = facts.clone();
    mismatched_running.running_headers_release = "6.18.33+rpt-rpi-v8".into();
    let report = evaluate_target(
        &expected,
        Some(&expected),
        true,
        Ok(&mismatched_running),
        facts.system_time_unix,
    );
    assert_eq!(
        target_status(&report, CheckId::TargetRunningHeaders),
        CheckStatus::Failed(FailureCode::RunningHeadersMismatch)
    );

    let mut unavailable = facts.clone();
    unavailable.running_headers_available = false;
    unavailable.running_headers_release.clear();
    let report = evaluate_target(
        &expected,
        Some(&expected),
        true,
        Ok(&unavailable),
        facts.system_time_unix,
    );
    assert_eq!(
        target_status(&report, CheckId::TargetRunningHeaders),
        CheckStatus::Failed(FailureCode::RunningHeadersUnavailable)
    );
}

#[test]
fn target_facts_prove_the_conventional_candidate_is_the_running_kernel() {
    let mut facts = parsed_target();
    facts.kernel_release = "6.18.35+rpt-rpi-v8".into();
    facts.kernel_vermagic = "6.18.35+rpt-rpi-v8 SMP preempt mod_unload modversions aarch64".into();
    facts.candidate_kernel_release = facts.kernel_release.clone();
    facts.candidate_kernel_vermagic = facts.kernel_vermagic.clone();
    facts.candidate_kernel_match_count = 1;
    facts.boot_selected_kernel_release = facts.kernel_release.clone();
    facts.boot_selected_kernel_match_count = 1;
    facts.boot_kernel_override_conflicting = false;

    assert!(facts.conventional_candidate_is_running());

    for invalid in [
        {
            let mut value = facts.clone();
            value.kernel_release = "6.18.34+rpt-rpi-v8".into();
            value
        },
        {
            let mut value = facts.clone();
            value.boot_selected_kernel_release = "6.18.34+rpt-rpi-v8".into();
            value
        },
        {
            let mut value = facts.clone();
            value.boot_kernel_override_conflicting = true;
            value
        },
    ] {
        assert!(!invalid.conventional_candidate_is_running());
    }
}

#[test]
fn target_candidate_kernel_probe_uses_only_exact_owned_package_selectors() {
    for fragment in [
        r#"readlink -f -- /boot/vmlinuz"#,
        r#"readlink -f -- /boot/initrd.img"#,
        r#"test "$(stat -c '%U:%G' "$candidate_vmlinuz")" = root:root"#,
        r#"test "$(stat -c '%U:%G' "$candidate_initrd")" = root:root"#,
        r#"/lib/modules/$candidate_kernel_release/build/include/config/kernel.release"#,
        r#"/usr/sbin/modinfo -k "$candidate_kernel_release" -F vermagic vc4"#,
    ] {
        assert!(
            TARGET_FACTS_SCRIPT.contains(fragment),
            "missing boot-selection contract: {fragment}"
        );
    }
    assert!(!TARGET_FACTS_SCRIPT.contains("for candidate_image in"));
    assert!(!TARGET_FACTS_SCRIPT.contains("/lib/modules/*"));

    let expected = identity();
    let running_selected = parsed_target();
    assert_eq!(
        evaluate_target(
            &expected,
            Some(&expected),
            true,
            Ok(&running_selected),
            running_selected.system_time_unix,
        )
        .require_success()
        .expect("running selected kernel"),
        PreflightDisposition::Ready
    );

    let inconsistent = String::from_utf8(valid_target_json())
        .expect("fixture utf8")
        .replace(
            "\"candidate_kernel_match_count\":1",
            "\"candidate_kernel_match_count\":0",
        );
    let fallback = TargetFacts::parse(inconsistent.as_bytes()).expect("package candidate absent");
    assert_eq!(fallback.candidate_kernel_release, fallback.kernel_release);
}

#[test]
fn target_candidate_kernel_probe_emits_canonical_zero_selector_running_fallback() {
    let temporary = tempfile::tempdir().expect("private command shim");
    let readlink = temporary.path().join("readlink");
    std::fs::write(&readlink, b"#!/bin/sh\nexit 1\n").expect("write readlink shim");
    std::fs::set_permissions(&readlink, std::fs::Permissions::from_mode(0o700))
        .expect("readlink shim mode");

    let start = TARGET_FACTS_SCRIPT
        .find("candidate_kernel_match_count=0;")
        .expect("candidate probe start");
    let end = TARGET_FACTS_SCRIPT
        .find("boot_kernel_override_conflicting=")
        .expect("candidate probe end");
    let fragment = &TARGET_FACTS_SCRIPT[start..end];
    let program = [
        "set -eu; ",
        "kernel_release=6.18.34+rpt-rpi-v8; ",
        "kernel_vermagic='6.18.34+rpt-rpi-v8 SMP preempt mod_unload modversions aarch64'; ",
        fragment,
        r#"printf '%s\n' \
          "candidate_kernel_match_count=$candidate_kernel_match_count" \
          "candidate_kernel_release=$candidate_kernel_release" \
          "candidate_kernel_vermagic=$candidate_kernel_vermagic" \
          "installed_kernel_header_pair_count=$installed_kernel_header_pair_count" \
          "installed_kernel_release=$installed_kernel_release" \
          "installed_headers_release=$installed_headers_release" \
          "boot_selected_kernel_match_count=$boot_selected_kernel_match_count" \
          "boot_selected_kernel_release=$boot_selected_kernel_release";"#,
    ]
    .concat();
    let output = Command::new("/bin/sh")
        .args(["-c", &program])
        .env_clear()
        .env("PATH", temporary.path())
        .output()
        .expect("run isolated candidate probe");
    assert!(
        output.status.success(),
        "candidate probe failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("candidate probe utf8");
    let emitted = stdout
        .lines()
        .map(|line| line.split_once('=').expect("candidate probe field"))
        .collect::<std::collections::BTreeMap<_, _>>();

    assert_eq!(emitted["candidate_kernel_match_count"], "0");
    assert_eq!(emitted["candidate_kernel_release"], "6.18.34+rpt-rpi-v8");
    assert_eq!(
        emitted["candidate_kernel_vermagic"],
        "6.18.34+rpt-rpi-v8 SMP preempt mod_unload modversions aarch64"
    );
    assert_eq!(emitted["installed_kernel_header_pair_count"], "0");
    assert_eq!(emitted["installed_kernel_release"], "");
    assert_eq!(emitted["installed_headers_release"], "");
    assert_eq!(emitted["boot_selected_kernel_match_count"], "0");
    assert_eq!(emitted["boot_selected_kernel_release"], "");

    let mut fixture: serde_json::Value =
        serde_json::from_slice(&valid_target_json()).expect("target fixture");
    for field in [
        "candidate_kernel_release",
        "candidate_kernel_vermagic",
        "installed_kernel_release",
        "installed_headers_release",
        "boot_selected_kernel_release",
    ] {
        fixture[field] = serde_json::Value::String(emitted[field].to_owned());
    }
    for field in [
        "candidate_kernel_match_count",
        "installed_kernel_header_pair_count",
        "boot_selected_kernel_match_count",
    ] {
        fixture[field] = serde_json::Value::from(
            emitted[field]
                .parse::<u64>()
                .expect("candidate probe count"),
        );
    }
    let encoded = serde_json::to_vec(&fixture).expect("encoded target facts");
    let parsed = TargetFacts::parse(&encoded).expect("canonical running-header fallback");
    let expected = identity();
    let report = evaluate_target(
        &expected,
        Some(&expected),
        true,
        Ok(&parsed),
        parsed.system_time_unix,
    );
    assert_eq!(
        target_status(&report, CheckId::TargetRunningHeaders),
        CheckStatus::Passed
    );
    assert_eq!(
        target_status(&report, CheckId::TargetInstalledKernelHeaders),
        CheckStatus::Passed
    );
    assert_eq!(
        report.require_success().expect("running-header fallback"),
        PreflightDisposition::Ready
    );
}

#[test]
fn zero_installed_header_pairs_parse_and_fail_with_typed_check_codes() {
    let missing = String::from_utf8(valid_target_json())
        .expect("fixture utf8")
        .replace(
            "\"running_headers_available\":true",
            "\"running_headers_available\":false",
        )
        .replace(
            "\"running_headers_release\":\"6.18.34+rpt-rpi-v8\"",
            "\"running_headers_release\":\"\"",
        )
        .replace(
            "\"installed_kernel_header_pair_count\":1",
            "\"installed_kernel_header_pair_count\":0",
        )
        .replace(
            "\"installed_kernel_release\":\"6.18.34+rpt-rpi-v8\"",
            "\"installed_kernel_release\":\"\"",
        )
        .replace(
            "\"installed_headers_release\":\"6.18.34+rpt-rpi-v8\"",
            "\"installed_headers_release\":\"\"",
        );
    let mut parsed =
        TargetFacts::parse(missing.as_bytes()).expect("zero pair facts remain structured");
    parsed.candidate_kernel_match_count = 0;
    parsed.candidate_kernel_release = parsed.kernel_release.clone();
    parsed.candidate_kernel_vermagic = parsed.kernel_vermagic.clone();
    let expected = identity();
    let report = evaluate_target(
        &expected,
        Some(&expected),
        true,
        Ok(&parsed),
        parsed.system_time_unix,
    );
    assert_eq!(
        target_status(&report, CheckId::TargetRunningHeaders),
        CheckStatus::Failed(FailureCode::RunningHeadersUnavailable)
    );
    assert_eq!(
        target_status(&report, CheckId::TargetInstalledKernelHeaders),
        CheckStatus::Failed(FailureCode::InstalledKernelHeadersMismatch)
    );

    let inconsistent = missing.replace(
        "\"installed_kernel_release\":\"\"",
        "\"installed_kernel_release\":\"6.18.35+rpt-rpi-v8\"",
    );
    assert_eq!(
        TargetFacts::parse(inconsistent.as_bytes()),
        Err(TargetFactsError::NoncanonicalField)
    );
}

#[test]
fn raspbian_id_and_exact_clock_tolerance_are_supported() {
    let expected = identity();
    let mut facts = parsed_target();
    facts.os_id = "raspbian".into();
    let exact = evaluate_target(
        &expected,
        Some(&expected),
        true,
        Ok(&facts),
        facts.system_time_unix + planeradarctl::preflight::MAX_CLOCK_SKEW_SECONDS,
    );
    assert_eq!(
        target_status(&exact, CheckId::TargetOperatingSystem),
        CheckStatus::Passed
    );
    assert_eq!(
        target_status(&exact, CheckId::TargetClock),
        CheckStatus::Passed
    );

    let overflow_safe = evaluate_target(&expected, Some(&expected), true, Ok(&facts), u64::MAX);
    assert_eq!(
        target_status(&overflow_safe, CheckId::TargetClock),
        CheckStatus::Failed(FailureCode::ClockOutsideTolerance)
    );
}

#[test]
fn missing_identity_and_malformed_facts_fail_closed_without_losing_checks() {
    let expected = identity();
    let report = evaluate_target(
        &expected,
        None,
        true,
        Err(TargetFactsError::InvalidJson),
        1_785_196_800,
    );
    assert_eq!(
        target_status(&report, CheckId::TargetIdentity),
        CheckStatus::Failed(FailureCode::ProbeUnavailable)
    );
    assert_eq!(report.checks().len(), 17);
    assert_eq!(
        report
            .checks()
            .last()
            .map(|check| (check.id(), check.status())),
        Some((
            CheckId::TargetGpioDisplayState,
            CheckStatus::Failed(FailureCode::MalformedFacts)
        ))
    );
    assert!(report.require_success().is_err());
}

fn supported_host() -> HostFacts {
    HostFacts {
        operating_system: "Darwin".into(),
        architecture: "arm64".into(),
        macos_major: MIN_MACOS_MAJOR,
        available_tools: [
            HostTool::Git,
            HostTool::Mise,
            HostTool::Ssh,
            HostTool::Gh,
            HostTool::Docker,
        ]
        .into_iter()
        .collect(),
        github_authenticated: true,
        buildx_available: true,
        application_repository_reachable: true,
        driver_repository_reachable: true,
        available_disk_bytes: MAC_MIN_FREE_BYTES,
    }
}

fn host_status(report: &PreflightReport, id: CheckId) -> CheckStatus {
    report
        .checks()
        .iter()
        .find(|check| check.id() == id)
        .expect("host check")
        .status()
}

#[test]
fn supported_host_accepts_documented_release_and_disk_boundaries() {
    let report = evaluate_host(&supported_host());
    assert_eq!(
        report.require_success().expect("supported host"),
        PreflightDisposition::Ready
    );
    assert_eq!(
        report
            .checks()
            .iter()
            .map(|check| check.id())
            .collect::<Vec<_>>(),
        [
            CheckId::HostOperatingSystem,
            CheckId::HostArchitecture,
            CheckId::HostMacosRelease,
            CheckId::HostGit,
            CheckId::HostMise,
            CheckId::HostSsh,
            CheckId::HostGh,
            CheckId::HostGithubAuthentication,
            CheckId::HostDocker,
            CheckId::HostBuildx,
            CheckId::HostApplicationRepository,
            CheckId::HostDriverRepository,
            CheckId::HostDiskSpace,
        ]
    );

    let mut intel = supported_host();
    intel.architecture = "x86_64".into();
    intel.macos_major = MIN_MACOS_MAJOR + 10;
    assert!(evaluate_host(&intel).require_success().is_ok());
}

#[test]
fn unsupported_host_platform_and_each_missing_tool_are_explicit() {
    let cases = [
        (
            {
                let mut facts = supported_host();
                facts.operating_system = "Linux".into();
                facts
            },
            CheckId::HostOperatingSystem,
            FailureCode::UnsupportedHostOperatingSystem,
        ),
        (
            {
                let mut facts = supported_host();
                facts.architecture = "powerpc".into();
                facts
            },
            CheckId::HostArchitecture,
            FailureCode::UnsupportedHostArchitecture,
        ),
        (
            {
                let mut facts = supported_host();
                facts.macos_major = MIN_MACOS_MAJOR - 1;
                facts
            },
            CheckId::HostMacosRelease,
            FailureCode::UnsupportedMacosRelease,
        ),
    ];
    for (facts, id, failure) in cases {
        assert_eq!(
            host_status(&evaluate_host(&facts), id),
            CheckStatus::Failed(failure)
        );
    }

    for (tool, id) in [
        (HostTool::Git, CheckId::HostGit),
        (HostTool::Mise, CheckId::HostMise),
        (HostTool::Ssh, CheckId::HostSsh),
        (HostTool::Gh, CheckId::HostGh),
        (HostTool::Docker, CheckId::HostDocker),
    ] {
        let mut facts = supported_host();
        facts.available_tools.remove(&tool);
        assert_eq!(
            host_status(&evaluate_host(&facts), id),
            CheckStatus::Failed(FailureCode::MissingTool)
        );
    }
}

#[test]
fn host_buildx_repositories_and_disk_fail_independently() {
    for (facts, id, failure) in [
        (
            {
                let mut facts = supported_host();
                facts.github_authenticated = false;
                facts
            },
            CheckId::HostGithubAuthentication,
            FailureCode::GithubAuthenticationUnavailable,
        ),
        (
            {
                let mut facts = supported_host();
                facts.buildx_available = false;
                facts
            },
            CheckId::HostBuildx,
            FailureCode::BuildxUnavailable,
        ),
        (
            {
                let mut facts = supported_host();
                facts.application_repository_reachable = false;
                facts
            },
            CheckId::HostApplicationRepository,
            FailureCode::RepositoryUnreachable,
        ),
        (
            {
                let mut facts = supported_host();
                facts.driver_repository_reachable = false;
                facts
            },
            CheckId::HostDriverRepository,
            FailureCode::RepositoryUnreachable,
        ),
        (
            {
                let mut facts = supported_host();
                facts.available_disk_bytes = MAC_MIN_FREE_BYTES - 1;
                facts
            },
            CheckId::HostDiskSpace,
            FailureCode::InsufficientSpace,
        ),
    ] {
        assert_eq!(
            host_status(&evaluate_host(&facts), id),
            CheckStatus::Failed(failure)
        );
    }
}

struct RecordingHostRunner {
    outputs: Mutex<VecDeque<Result<CommandOutput, RunnerError>>>,
    invocations: Mutex<Vec<Invocation>>,
}

impl RecordingHostRunner {
    fn successful() -> Self {
        Self {
            outputs: Mutex::new(
                [
                    CommandOutput::success(b"Darwin\n".to_vec(), vec![]),
                    CommandOutput::success(b"arm64\n".to_vec(), vec![]),
                    CommandOutput::success(b"14.7.1\n".to_vec(), vec![]),
                    CommandOutput::success(b"git version 2\n".to_vec(), vec![]),
                    CommandOutput::success(b"mise 1\n".to_vec(), vec![]),
                    CommandOutput::success(vec![], b"OpenSSH_9\n".to_vec()),
                    CommandOutput::success(b"gh version 2\n".to_vec(), vec![]),
                    CommandOutput::success(vec![], b"authenticated secret account\n".to_vec()),
                    CommandOutput::success(b"Docker version 28\n".to_vec(), vec![]),
                    CommandOutput::success(b"builder\n".to_vec(), vec![]),
                    CommandOutput::success(b"deadbeef\tHEAD\n".to_vec(), vec![]),
                    CommandOutput::success(b"deadbeef\tHEAD\n".to_vec(), vec![]),
                    CommandOutput::success(
                        b"Filesystem 1024-blocks Used Available Capacity Mounted on\n/dev/disk 20000000 1 16777216 1% /\n"
                            .to_vec(),
                        vec![],
                    ),
                ]
                .into_iter()
                .map(Ok)
                .collect(),
            ),
            invocations: Mutex::new(Vec::new()),
        }
    }

    fn invocations(&self) -> Vec<Invocation> {
        self.invocations.lock().expect("invocation lock").clone()
    }
}

impl CommandRunner for RecordingHostRunner {
    fn run(&self, invocation: Invocation) -> Result<CommandOutput, RunnerError> {
        self.invocations
            .lock()
            .expect("invocation lock")
            .push(invocation);
        self.outputs
            .lock()
            .expect("output lock")
            .pop_front()
            .expect("host output")
    }
}

#[test]
fn host_adapter_uses_complete_typed_argument_vectors_without_a_shell() {
    let runner = RecordingHostRunner::successful();
    let preflight = HostPreflight::new(&runner);
    let report = preflight.run(Path::new("/Users/example/repo"), Some("orbstack"));
    assert!(report.require_success().is_ok(), "{report:?}");

    let invocations = runner.invocations();
    let vectors = invocations
        .iter()
        .map(|invocation| {
            (
                invocation.program().to_owned(),
                invocation.arguments().to_vec(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        vectors,
        [
            ("uname".into(), vec!["-s".into()]),
            ("uname".into(), vec!["-m".into()]),
            ("sw_vers".into(), vec!["-productVersion".into()]),
            ("git".into(), vec!["--version".into()]),
            ("mise".into(), vec!["--version".into()]),
            ("ssh".into(), vec!["-V".into()]),
            ("gh".into(), vec!["--version".into()]),
            (
                "gh".into(),
                vec![
                    "auth".into(),
                    "status".into(),
                    "--active".into(),
                    "--hostname".into(),
                    "github.com".into()
                ]
            ),
            ("docker".into(), vec!["--version".into()]),
            (
                "docker".into(),
                vec![
                    "--context".into(),
                    "orbstack".into(),
                    "buildx".into(),
                    "inspect".into()
                ]
            ),
            (
                "git".into(),
                vec![
                    "ls-remote".into(),
                    "--exit-code".into(),
                    APPLICATION_REPOSITORY.into(),
                    "HEAD".into()
                ]
            ),
            (
                "git".into(),
                vec![
                    "ls-remote".into(),
                    "--exit-code".into(),
                    DRIVER_REPOSITORY.into(),
                    "HEAD".into()
                ]
            ),
            (
                "df".into(),
                vec!["-Pk".into(), "/Users/example/repo".into()]
            ),
        ]
    );
    assert!(
        vectors
            .iter()
            .all(|(program, _)| !matches!(program.as_str(), "sh" | "bash" | "zsh"))
    );
    assert!(
        invocations
            .iter()
            .all(|invocation| invocation.timeout() == Some(HOST_PROBE_TIMEOUT))
    );
}

#[test]
fn invalid_docker_context_is_failed_without_exposing_it_or_running_buildx() {
    let runner = RecordingHostRunner::successful();
    let report = HostPreflight::new(&runner).run(Path::new("/repo"), Some("secret\ncontext"));
    assert_eq!(
        host_status(&report, CheckId::HostBuildx),
        CheckStatus::Failed(FailureCode::InvalidDockerContext)
    );
    let rendered = format!("{report:?}");
    assert!(!rendered.contains("secret"));
    assert!(
        runner
            .invocations()
            .iter()
            .all(|invocation| !invocation.arguments().iter().any(|arg| arg == "buildx"))
    );
}

#[test]
fn host_adapter_maps_runner_and_nonzero_tool_failures_without_panicking() {
    let runner = RecordingHostRunner::successful();
    {
        let mut outputs = runner.outputs.lock().expect("output lock");
        outputs[3] = Err(RunnerError::Failed);
        outputs[4] = Ok(CommandOutput::new(7, b"mise secret".to_vec(), vec![]));
    }
    let report = HostPreflight::new(&runner).run(Path::new("/repo"), None);
    assert_eq!(
        host_status(&report, CheckId::HostGit),
        CheckStatus::Failed(FailureCode::MissingTool)
    );
    assert_eq!(
        host_status(&report, CheckId::HostMise),
        CheckStatus::Failed(FailureCode::MissingTool)
    );
    assert!(!format!("{report:?}").contains("mise secret"));
}

#[test]
fn host_adapter_fails_closed_for_nonzero_or_malformed_df_and_oversized_output() {
    for output in [
        CommandOutput::new(1, b"secret path".to_vec(), vec![]),
        CommandOutput::success(b"not a df table".to_vec(), vec![]),
    ] {
        let runner = RecordingHostRunner::successful();
        runner.outputs.lock().expect("output lock")[12] = Ok(output);
        let report = HostPreflight::new(&runner).run(Path::new("/Users/secret/repo"), None);
        assert_eq!(
            host_status(&report, CheckId::HostDiskSpace),
            CheckStatus::Failed(FailureCode::InsufficientSpace)
        );
        assert!(!format!("{report:?}").contains("secret"));
        assert!(!format!("{:?}", runner.invocations()[12]).contains("/Users/secret/repo"));
    }

    let runner = RecordingHostRunner::successful();
    runner.outputs.lock().expect("output lock")[3] = Ok(CommandOutput::success(
        vec![b'x'; planeradarctl::preflight::MAX_HOST_OUTPUT_BYTES + 1],
        vec![],
    ));
    let report = HostPreflight::new(&runner).run(Path::new("/repo"), None);
    assert_eq!(
        host_status(&report, CheckId::HostGit),
        CheckStatus::Failed(FailureCode::MissingTool)
    );
}

#[test]
fn host_adapter_maps_buildx_and_each_fixed_repository_nonzero_status() {
    let runner = RecordingHostRunner::successful();
    {
        let mut outputs = runner.outputs.lock().expect("output lock");
        outputs[9] = Ok(CommandOutput::new(1, vec![], b"context secret".to_vec()));
        outputs[10] = Ok(CommandOutput::new(2, b"app response".to_vec(), vec![]));
        outputs[11] = Ok(CommandOutput::new(3, b"driver response".to_vec(), vec![]));
    }
    let report = HostPreflight::new(&runner).run(Path::new("/repo"), Some("orbstack"));
    assert_eq!(
        host_status(&report, CheckId::HostBuildx),
        CheckStatus::Failed(FailureCode::BuildxUnavailable)
    );
    assert_eq!(
        host_status(&report, CheckId::HostApplicationRepository),
        CheckStatus::Failed(FailureCode::RepositoryUnreachable)
    );
    assert_eq!(
        host_status(&report, CheckId::HostDriverRepository),
        CheckStatus::Failed(FailureCode::RepositoryUnreachable)
    );
    let rendered = format!("{report:?}");
    for sensitive in [
        "orbstack",
        "context secret",
        "app response",
        "driver response",
    ] {
        assert!(!rendered.contains(sensitive));
    }
}

#[test]
fn host_adapter_blocks_nonzero_and_runner_error_github_auth_without_exposing_output() {
    for output in [
        Ok(CommandOutput::new(
            1,
            b"secret github account".to_vec(),
            b"secret token detail".to_vec(),
        )),
        Err(RunnerError::Failed),
    ] {
        let runner = RecordingHostRunner::successful();
        runner.outputs.lock().expect("output lock")[7] = output;
        let report = HostPreflight::new(&runner).run(Path::new("/repo"), None);

        assert_eq!(host_status(&report, CheckId::HostGh), CheckStatus::Passed);
        assert_eq!(
            host_status(&report, CheckId::HostGithubAuthentication),
            CheckStatus::Failed(FailureCode::GithubAuthenticationUnavailable)
        );
        let rendered = format!("{report:?}");
        assert!(!rendered.contains("secret github account"));
        assert!(!rendered.contains("secret token detail"));
    }
}

#[test]
fn facts_debug_views_do_not_expose_collected_values() {
    let mut host = supported_host();
    host.operating_system = "sensitive-host-value".into();
    let target = parsed_target();
    let host_debug = format!("{host:?}");
    let target_debug = format!("{target:?}");

    assert!(!host_debug.contains("sensitive-host-value"));
    assert!(!target_debug.contains("Raspberry Pi"));
    assert!(!target_debug.contains("6.18.34"));
}

#[derive(Clone, Copy)]
struct FixedUnixClock(u64);

impl UnixClock for FixedUnixClock {
    fn now_unix_seconds(&self) -> u64 {
        self.0
    }
}

struct RecordingTransport {
    identity: TargetIdentity,
    requests: Mutex<Vec<RemoteCommand>>,
    outputs: Mutex<VecDeque<Result<Output, TransportError>>>,
}

impl RecordingTransport {
    fn supported() -> Self {
        Self::with_outputs([
            Ok(CommandOutput::success(vec![], vec![])),
            Ok(CommandOutput::success(valid_target_json(), vec![])),
        ])
    }

    fn with_outputs(outputs: impl IntoIterator<Item = Result<Output, TransportError>>) -> Self {
        Self {
            identity: identity(),
            requests: Mutex::new(Vec::new()),
            outputs: Mutex::new(outputs.into_iter().collect()),
        }
    }
}

impl Transport for RecordingTransport {
    fn probe(&self, _target: &SshTarget) -> Result<TargetProbe, TransportError> {
        Ok(TargetProbe {
            identity: self.identity.clone(),
        })
    }

    fn run(&self, _target: &SshTarget, request: RemoteCommand) -> Result<Output, TransportError> {
        self.requests.lock().expect("request lock").push(request);
        self.outputs
            .lock()
            .expect("output lock")
            .pop_front()
            .expect("target output")
    }

    fn copy_to(
        &self,
        _target: &SshTarget,
        _local: &Path,
        _remote: &Path,
    ) -> Result<(), TransportError> {
        panic!("preflight must not copy")
    }

    fn copy_from(
        &self,
        _target: &SshTarget,
        _remote: &Path,
        _local: &Path,
    ) -> Result<(), TransportError> {
        panic!("preflight must not copy")
    }

    fn wait_for_reboot(
        &self,
        _identity: &TargetIdentity,
        _addresses: &[SshTarget],
        _policy: ReconnectPolicy,
    ) -> Result<SshTarget, TransportError> {
        panic!("preflight must not reboot")
    }
}

#[test]
fn target_adapter_uses_noninteractive_sudo_when_it_is_already_available() {
    let transport = RecordingTransport::supported();
    let target = SshTarget::from_str("pi@raspberrypi.local").expect("target");
    let report = TargetPreflight::new(&transport, FixedUnixClock(parsed_target().system_time_unix))
        .run(&target, &identity());
    assert!(report.require_success().is_ok(), "{report:?}");

    let requests = transport.requests.lock().expect("request lock");
    assert_eq!(requests.len(), 2);
    assert!(!requests[0].is_interactive_sudo());
    assert_eq!(requests[0].arguments(), ["sudo", "-n", "true"]);
    assert!(!requests[1].is_interactive_sudo());
    assert_eq!(
        requests[1].arguments(),
        ["sudo", "-n", "sh", "-c", TARGET_FACTS_SCRIPT]
    );
    assert!(
        requests
            .iter()
            .flat_map(RemoteCommand::arguments)
            .all(
                |argument| !argument.to_ascii_lowercase().contains("password")
                    && !argument.to_ascii_lowercase().contains("token")
            )
    );
}

#[test]
fn target_adapter_falls_back_to_interactive_sudo_validation() {
    let transport = RecordingTransport::with_outputs([
        Err(TransportError::CommandFailed),
        Ok(CommandOutput::success(vec![], vec![])),
        Ok(CommandOutput::success(valid_target_json(), vec![])),
    ]);
    let target = SshTarget::from_str("pi@raspberrypi.local").expect("target");
    let report = TargetPreflight::new(&transport, FixedUnixClock(parsed_target().system_time_unix))
        .run(&target, &identity());
    assert!(report.require_success().is_ok(), "{report:?}");

    let requests = transport.requests.lock().expect("request lock");
    assert_eq!(requests.len(), 3);
    assert!(!requests[0].is_interactive_sudo());
    assert_eq!(requests[0].arguments(), ["sudo", "-n", "true"]);
    assert!(requests[1].is_interactive_sudo());
    assert_eq!(requests[1].arguments(), ["sudo", "true"]);
    assert!(!requests[2].is_interactive_sudo());
    assert_eq!(
        requests[2].arguments(),
        ["sudo", "-n", "sh", "-c", TARGET_FACTS_SCRIPT]
    );
}

#[test]
fn target_facts_probe_accepts_safe_existing_pi_state_without_weakening_conflict_checks() {
    for required in [
        r#"repository_probe="${repository_uri}.xz""#,
        r#"FragmentPath --value planeradar.service"#,
        r#"/proc/"$service_main_pid"/exe"#,
        r#"port_80_pid_count"#,
        r#"test "$port_80_pid_count" = 1"#,
        r#"root:root:755:1"#,
        r#"candidate_kernel_release=$candidate_vmlinuz_release"#,
        r#"grep -E '^dtoverlay=(.*hyperpixel.*|vc4-kms-dpi.*|dpi[0-9].*)$'"#,
    ] {
        assert!(
            TARGET_FACTS_SCRIPT.contains(required),
            "missing supported existing-state preflight proof: {required}"
        );
    }
    assert!(
        !TARGET_FACTS_SCRIPT.contains("curl --fail --head"),
        "apt reachability must probe an actual compressed index instead of an unserved uncompressed HEAD target"
    );
    assert!(
        !TARGET_FACTS_SCRIPT
            .contains(r#"grep -Ev '^(|dtoverlay=vc4-kms-v3d|dtoverlay=vc4-kms-dpi-hyperpixel2r"#),
        "unrelated Raspberry Pi overlays must not be treated as HyperPixel conflicts"
    );
}

#[test]
fn target_adapter_stops_before_sudo_and_facts_when_identity_is_wrong() {
    let mut transport = RecordingTransport::supported();
    transport.identity.serial = "fedcba9876543210".into();
    let target = SshTarget::from_str("pi@raspberrypi.local").expect("target");
    let report = TargetPreflight::new(&transport, FixedUnixClock(parsed_target().system_time_unix))
        .run(&target, &identity());

    assert_eq!(
        target_status(&report, CheckId::TargetIdentity),
        CheckStatus::Failed(FailureCode::IdentityMismatch)
    );
    assert!(transport.requests.lock().expect("request lock").is_empty());
}

#[test]
fn target_adapter_rejects_nonzero_sudo_even_with_plausible_output() {
    let transport = RecordingTransport::with_outputs([
        Ok(CommandOutput::new(
            1,
            b"{\"sudo\":true}".to_vec(),
            b"password prompt".to_vec(),
        )),
        Ok(CommandOutput::new(
            1,
            b"{\"sudo\":true}".to_vec(),
            b"password prompt".to_vec(),
        )),
        Ok(CommandOutput::success(valid_target_json(), vec![])),
    ]);
    let target = SshTarget::from_str("pi@raspberrypi.local").expect("target");
    let now = parsed_target().system_time_unix;
    let report = TargetPreflight::new(&transport, FixedUnixClock(now)).run(&target, &identity());

    assert_eq!(
        target_status(&report, CheckId::TargetSudo),
        CheckStatus::Failed(FailureCode::SudoUnavailable)
    );
    assert!(!format!("{report:?}").contains("password prompt"));
}

#[test]
fn target_adapter_rejects_nonzero_facts_even_when_stdout_is_valid_json() {
    let transport = RecordingTransport::with_outputs([
        Ok(CommandOutput::success(vec![], vec![])),
        Ok(CommandOutput::new(
            2,
            valid_target_json(),
            b"secret target".to_vec(),
        )),
    ]);
    let target = SshTarget::from_str("pi@raspberrypi.local").expect("target");
    let now = parsed_target().system_time_unix;
    let report = TargetPreflight::new(&transport, FixedUnixClock(now)).run(&target, &identity());

    assert_eq!(
        target_status(&report, CheckId::TargetModel),
        CheckStatus::Failed(FailureCode::MalformedFacts)
    );
    assert!(!format!("{report:?}").contains("secret target"));
}

use std::fs;

use clap::Parser;
use planeradarctl::{
    DriverLock,
    cli::{Cli, Command, DriverCommand},
    config::{DEFAULT_HOSTNAME, Environment, InstallConfig},
};

const LOCK: &str = include_str!("../../../driver.lock.toml");

#[test]
fn cli_target_wins_over_environment_and_default() {
    let cli = Cli::try_parse_from([
        "planeradarctl",
        "install",
        "alice@radar.local",
        "--hostname",
        "hangar",
    ])
    .expect("parse install command");
    let environment = Environment {
        target: Some("pi@raspberrypi.local".into()),
        hostname: Some("planeradar".into()),
        docker_context: Some("orbstack".into()),
    };

    let config = InstallConfig::resolve(cli, environment).expect("resolve config");

    assert_eq!(config.target.as_deref(), Some("alice@radar.local"));
    assert_eq!(config.hostname, "hangar");
    assert_eq!(config.docker_context.as_deref(), Some("orbstack"));
}

#[test]
fn absent_dotenv_and_no_cli_overrides_leave_target_promptable() {
    let temporary_directory = tempfile::tempdir().expect("temporary directory");
    let environment = Environment::from_dotenv_path(&temporary_directory.path().join(".env"))
        .expect("missing dotenv is allowed");
    let cli = Cli::try_parse_from(["planeradarctl", "install"])
        .expect("parse install command without overrides");

    let config = InstallConfig::resolve(cli, environment).expect("resolve config");

    assert_eq!(config.target, None);
    assert_eq!(config.hostname, DEFAULT_HOSTNAME);
    assert_eq!(config.docker_context, None);
}

#[test]
fn dotenv_rejects_values_outside_the_documented_non_secret_contract() {
    let cases = [
        ("unknown key", b"PLANERADAR_TOKEN=secret\n".as_slice()),
        ("password", b"PLANERADAR_PASSWORD=secret\n".as_slice()),
        ("malformed line", b"PLANERADAR_PI_TARGET\n".as_slice()),
        (
            "duplicate key",
            b"PLANERADAR_HOSTNAME=planeradar\nPLANERADAR_HOSTNAME=hangar\n".as_slice(),
        ),
        ("empty target", b"PLANERADAR_PI_TARGET=\n".as_slice()),
        ("empty hostname", b"PLANERADAR_HOSTNAME=\n".as_slice()),
        ("non unicode", b"PLANERADAR_HOSTNAME=\xff\n".as_slice()),
    ];

    for (name, contents) in cases {
        let file = tempfile::NamedTempFile::new().expect("temporary dotenv");
        fs::write(file.path(), contents).expect("write dotenv");
        assert!(
            Environment::from_dotenv_path(file.path()).is_err(),
            "{name} must be rejected"
        );
    }
}

#[test]
fn dotenv_values_override_generic_defaults() {
    let file = tempfile::NamedTempFile::new().expect("temporary dotenv");
    fs::write(
        file.path(),
        "PLANERADAR_PI_TARGET=pi@radar.local\nPLANERADAR_HOSTNAME=hangar\nPLANERADAR_DOCKER_CONTEXT=orbstack\n",
    )
    .expect("write dotenv");
    let cli = Cli::try_parse_from(["planeradarctl", "install"])
        .expect("parse install command without overrides");

    let config = InstallConfig::resolve(
        cli,
        Environment::from_dotenv_path(file.path()).expect("load dotenv"),
    )
    .expect("resolve config");

    assert_eq!(config.target.as_deref(), Some("pi@radar.local"));
    assert_eq!(config.hostname, "hangar");
    assert_eq!(config.docker_context.as_deref(), Some("orbstack"));
}

#[test]
fn mutually_exclusive_release_inputs_are_rejected() {
    let cli = Cli::try_parse_from([
        "planeradarctl",
        "upgrade",
        "--version",
        "0.1.0",
        "--release-dir",
        "/tmp/release",
    ])
    .expect("parse upgrade command");

    assert!(InstallConfig::resolve(cli, Environment::default()).is_err());
}

#[test]
fn command_surface_accepts_all_public_command_names() {
    for command in [
        "install",
        "upgrade",
        "status",
        "doctor",
        "screenshot",
        "rollback",
        "uninstall",
    ] {
        Cli::try_parse_from(["planeradarctl", command]).expect(command);
    }
}

#[test]
fn purge_settings_is_an_uninstall_only_option() {
    for command in ["install", "upgrade", "rollback"] {
        assert!(
            Cli::try_parse_from(["planeradarctl", command, "--purge-settings"]).is_err(),
            "{command} must reject uninstall-only settings purge"
        );
    }
    let uninstall = Cli::try_parse_from(["planeradarctl", "uninstall", "--purge-settings"])
        .expect("uninstall purge");
    let config =
        InstallConfig::resolve(uninstall, Environment::default()).expect("uninstall config");
    assert!(config.purge_settings);
}

#[test]
fn diagnostic_and_screenshot_options_are_explicit() {
    let doctor = Cli::try_parse_from(["planeradarctl", "doctor", "pi@planeradar.local", "--json"])
        .expect("doctor options");
    assert!(matches!(
        doctor.command,
        Command::Doctor(options)
            if options.target.as_deref() == Some("pi@planeradar.local") && options.json
    ));

    let screenshot = Cli::try_parse_from([
        "planeradarctl",
        "screenshot",
        "pi@planeradar.local",
        "--output",
        "radar.png",
    ])
    .expect("screenshot options");
    assert!(matches!(
        screenshot.command,
        Command::Screenshot(options)
            if options.target.as_deref() == Some("pi@planeradar.local")
                && options.output == std::path::Path::new("radar.png")
    ));
}

#[test]
fn maintainer_driver_commands_parse_exact_sync_and_update_forms() {
    let sync = Cli::try_parse_from(["planeradarctl", "driver", "sync"]).expect("driver sync");
    assert!(matches!(
        sync.command,
        Command::Driver {
            command: DriverCommand::Sync
        }
    ));

    let update = Cli::try_parse_from(["planeradarctl", "driver", "update", "0.1.0-rc.15"])
        .expect("driver update");
    assert!(matches!(
        update.command,
        Command::Driver {
            command: DriverCommand::Update { version }
        } if version == "0.1.0-rc.15"
    ));
}

#[test]
fn driver_lock_matches_the_next_release_candidate_gate() {
    let lock = DriverLock::parse(LOCK).expect("parse driver lock");

    assert_eq!(
        lock.repository,
        "https://github.com/shayne/hyperpixel2r-kms"
    );
    assert_eq!(lock.version.to_string(), "0.1.0-rc.15");
    assert_eq!(lock.commit, "ab3f88c7f106df9fbfd70afa43bab1b24ca6dd8d");
    assert_eq!(
        lock.manifest_sha256,
        "77a6efdd0afdb8cffce7737b7244f9cc902aca4623769272cd5b9dcd485d85b0"
    );
}

#[test]
fn driver_lock_rejects_invalid_or_ambiguous_identity_fields() {
    let lock = DriverLock::parse(LOCK).expect("parse current driver lock");
    let version = lock.version.to_string();
    let replace_current = |current: &str, replacement: &str| {
        let mutated = LOCK.replace(current, replacement);
        assert_ne!(mutated, LOCK, "invalid-lock mutation must change the lock");
        mutated
    };
    let cases = [
        (
            "shortened commit",
            replace_current(&lock.commit, &lock.commit[..39]),
        ),
        (
            "uppercase commit",
            replace_current(&lock.commit, &lock.commit.to_uppercase()),
        ),
        (
            "non https repository",
            replace_current(
                "https://github.com/shayne/hyperpixel2r-kms",
                "http://github.com/shayne/hyperpixel2r-kms",
            ),
        ),
        (
            "wrong repository",
            replace_current(
                "https://github.com/shayne/hyperpixel2r-kms",
                "https://github.com/example/driver",
            ),
        ),
        (
            "non semantic version",
            replace_current(&version, &format!("v{version}")),
        ),
        (
            "uppercase manifest digest",
            replace_current(&lock.manifest_sha256, &lock.manifest_sha256.to_uppercase()),
        ),
        (
            "missing lifecycle protocol",
            LOCK.replace("lifecycle_protocol = \"accepted-driver-v1\"\n", ""),
        ),
        (
            "wrong lifecycle protocol",
            replace_current("accepted-driver-v1", "unsupported-driver-v1"),
        ),
        ("unknown field", format!("{LOCK}unexpected = \"value\"\n")),
        (
            "duplicate field",
            format!("{LOCK}version = \"{version}\"\n"),
        ),
        ("trailing non toml", format!("{LOCK}this is not TOML")),
    ];

    for (name, contents) in cases {
        assert!(
            DriverLock::parse(&contents).is_err(),
            "{name} must be rejected"
        );
    }
}

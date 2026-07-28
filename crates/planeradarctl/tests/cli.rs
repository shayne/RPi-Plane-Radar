use std::fs;

use clap::Parser;
use planeradarctl::{
    DriverLock,
    cli::Cli,
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
fn driver_lock_matches_the_published_release_candidate() {
    let lock = DriverLock::parse(LOCK).expect("parse driver lock");

    assert_eq!(
        lock.repository,
        "https://github.com/shayne/hyperpixel2r-kms"
    );
    assert_eq!(lock.version.to_string(), "0.1.0-rc.4");
    assert_eq!(lock.commit, "6826419b4f3ab01c2e1ce9a3ef870186ae2cc3b8");
    assert_eq!(
        lock.manifest_sha256,
        "93f413aac135b44585703a03717d5aa2e9ae6b2b2d4b178d193d4758dfdedee7"
    );
}

#[test]
fn driver_lock_rejects_invalid_or_ambiguous_identity_fields() {
    let cases = [
        (
            "shortened commit",
            LOCK.replace(
                "6826419b4f3ab01c2e1ce9a3ef870186ae2cc3b8",
                "6826419b4f3ab01c2e1ce9a3ef870186ae2cc3",
            ),
        ),
        (
            "uppercase commit",
            LOCK.replace(
                "6826419b4f3ab01c2e1ce9a3ef870186ae2cc3b8",
                "6826419B4F3AB01C2E1CE9A3EF870186AE2CC3B8",
            ),
        ),
        (
            "non https repository",
            LOCK.replace(
                "https://github.com/shayne/hyperpixel2r-kms",
                "http://github.com/shayne/hyperpixel2r-kms",
            ),
        ),
        (
            "wrong repository",
            LOCK.replace(
                "https://github.com/shayne/hyperpixel2r-kms",
                "https://github.com/example/driver",
            ),
        ),
        (
            "non semantic version",
            LOCK.replace("0.1.0-rc.4", "v0.1.0-rc.4"),
        ),
        (
            "uppercase manifest digest",
            LOCK.replace(
                "93f413aac135b44585703a03717d5aa2e9ae6b2b2d4b178d193d4758dfdedee7",
                "93F413AAC135B44585703A03717D5AA2E9AE6B2B2D4B178D193D4758DFDEDEE7",
            ),
        ),
        ("unknown field", format!("{LOCK}unexpected = \"value\"\n")),
        (
            "duplicate field",
            format!("{LOCK}version = \"0.1.0-rc.4\"\n"),
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

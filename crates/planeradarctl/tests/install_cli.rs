use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;

fn write_executable(path: &std::path::Path, contents: &str) {
    fs::write(path, contents).expect("write executable");
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("make executable");
}

#[test]
fn public_install_dispatch_rejects_an_invalid_target_instead_of_exiting_successfully() {
    let temporary = tempfile::tempdir().expect("temporary working directory");
    let output = Command::new(env!("CARGO_BIN_EXE_planeradarctl"))
        .current_dir(temporary.path())
        .args([
            "install",
            "not-a-user-at-host",
            "--release-dir",
            "/tmp/release",
            "--non-interactive",
        ])
        .output()
        .expect("run planeradarctl");

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).expect("UTF-8 stderr");
    assert!(stderr.contains("SSH target"));
    assert!(!stderr.contains("/Users/"));
}

#[test]
fn public_install_dispatch_requires_a_target_without_prompting_in_noninteractive_mode() {
    let temporary = tempfile::tempdir().expect("temporary working directory");
    let output = Command::new(env!("CARGO_BIN_EXE_planeradarctl"))
        .current_dir(temporary.path())
        .args(["install", "--non-interactive"])
        .output()
        .expect("run planeradarctl");

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).expect("UTF-8 stderr");
    assert!(stderr.contains("target is required"));
    assert!(!stderr.contains("SSH target (user@host):"));
    assert!(
        output.stdout.is_empty(),
        "non-interactive mode wrote a prompt"
    );
}

#[test]
fn public_install_dispatch_never_prompts_when_stdin_is_not_a_terminal() {
    let temporary = tempfile::tempdir().expect("temporary working directory");
    let output = Command::new(env!("CARGO_BIN_EXE_planeradarctl"))
        .current_dir(temporary.path())
        .arg("install")
        .output()
        .expect("run planeradarctl");

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).expect("UTF-8 stderr");
    assert!(stderr.contains("target is required"));
    assert!(!stderr.contains("SSH target (user@host):"));
    assert!(
        output.stdout.is_empty(),
        "redirected stdin received a prompt"
    );
}

#[test]
fn selector_free_public_install_resolves_stable_before_downloading_exact_assets() {
    let temporary = tempfile::tempdir().expect("temporary working directory");
    let bin = temporary.path().join("bin");
    fs::create_dir(&bin).expect("fake command directory");
    let record = temporary.path().join("gh.log");
    write_executable(
        &bin.join("gh"),
        r#"#!/bin/sh
set -eu
printf '%s\n' "$*" >>"$PLANERADAR_GH_RECORD"
case "$1 $2" in
  "release view")
    printf '%s\n' '{"tagName":"v1.2.3","isDraft":false,"isPrerelease":false}'
    ;;
  "release download")
    exit 73
    ;;
  *)
    exit 74
    ;;
esac
"#,
    );

    let output = Command::new(env!("CARGO_BIN_EXE_planeradarctl"))
        .current_dir(temporary.path())
        .env("HOME", temporary.path())
        .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
        .env("PLANERADAR_GH_RECORD", &record)
        .args(["install", "pi@raspberrypi.local", "--non-interactive"])
        .output()
        .expect("run planeradarctl");

    assert!(
        !output.status.success(),
        "fixture download must stop install"
    );
    let stderr = String::from_utf8(output.stderr).expect("UTF-8 stderr");
    assert!(
        stderr.contains("release source failed"),
        "selector-free install did not pass stable resolution: {stderr}"
    );
    assert!(!stderr.contains("exact --version"));
    let invocations = fs::read_to_string(record).expect("gh invocation record");
    let mut lines = invocations.lines();
    assert_eq!(
        lines.next(),
        Some("release view -R shayne/RPi-Plane-Radar --json tagName,isDraft,isPrerelease")
    );
    assert!(
        lines
            .next()
            .is_some_and(|line| line.starts_with("release download v1.2.3 ")),
        "stable tag was not forwarded to exact asset download: {invocations}"
    );
}

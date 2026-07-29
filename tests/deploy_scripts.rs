use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

const PUBLIC_TARGET: &str = "pi@raspberrypi.local";

fn is_device_tree_node_identifier(token: &str) -> bool {
    let Some((name, suffix)) = token.split_once('@') else {
        return false;
    };
    matches!(name, "fragment" | "touchscreen" | "v3d")
        && !suffix.is_empty()
        && suffix
            .chars()
            .all(|character| character.is_ascii_hexdigit())
}

fn hard_coded_ssh_targets(source: &str) -> impl Iterator<Item = &str> {
    source
        .split(|character: char| {
            !character.is_ascii_alphanumeric() && !matches!(character, '.' | '_' | '-' | '@')
        })
        .map(|token| token.trim_matches('-'))
        .filter(|token| {
            let Some((user, host)) = token.split_once('@') else {
                return false;
            };
            token.matches('@').count() == 1
                && !user.is_empty()
                && !host.is_empty()
                && !is_device_tree_node_identifier(token)
        })
}

#[test]
fn pi_application_scripts_share_the_public_target_override() {
    for relative in ["scripts/deploy-pi.sh", "scripts/smoke-pi.sh"] {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(relative);
        let source = fs::read_to_string(&path).expect("deployment script must be readable");
        assert!(
            source.contains(r#"target="${1:-${PLANERADAR_PI_TARGET:-pi@raspberrypi.local}}""#),
            "{relative} must honor PLANERADAR_PI_TARGET"
        );
    }
}

#[test]
fn ssh_target_scanner_rejects_nonempty_users_and_hosts() {
    assert_eq!(
        hard_coded_ssh_targets(
            r#"target="1@192.0.2.1" target="9@host.example" target="8@localhost"
if ssh 2@192.0.2.2 true; then
sudo ssh 3@host.example true
remote=4@localhost
endpoint=5@example
printf %s 6@192.0.2.6
# 7@comment.example"#
        )
        .collect::<Vec<_>>(),
        [
            "1@192.0.2.1",
            "9@host.example",
            "8@localhost",
            "2@192.0.2.2",
            "3@host.example",
            "4@localhost",
            "5@example",
            "6@192.0.2.6",
            "7@comment.example",
        ]
    );
}

#[test]
fn ssh_target_scanner_ignores_device_tree_node_identifiers() {
    assert!(
        hard_coded_ssh_targets(
            "fragment@0\ntouchscreen@15\nv3d_status=\"/proc/device-tree/soc/v3d@7ec00000/status\""
        )
        .next()
        .is_none()
    );
}

#[test]
fn shell_scripts_contain_only_the_documented_public_ssh_target() {
    let scripts = Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts");
    for entry in fs::read_dir(scripts).expect("scripts directory must be readable") {
        let path = entry.expect("script entry").path();
        if path.extension().is_none_or(|extension| extension != "sh") {
            continue;
        }
        let source = fs::read_to_string(&path).expect("script must be readable");
        for token in hard_coded_ssh_targets(&source) {
            assert_eq!(
                token,
                PUBLIC_TARGET,
                "{} contains a non-public SSH target: {token}",
                path.display()
            );
        }
    }
}

#[test]
fn smoke_uses_only_typed_mise_control_tasks_and_private_doctor_json() {
    let source =
        fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/smoke-pi.sh"))
            .expect("smoke script");
    for command in [
        r#"mise run status -- "$target""#,
        r#"mise run doctor -- "$target" --json"#,
        r#"mise run screenshot -- "$target" --output dist/smoke-radar.png"#,
    ] {
        assert!(source.contains(command), "missing typed command: {command}");
    }
    assert!(!source.contains("\nssh "));
    assert!(!source.contains("| ssh "));
    assert!(
        source.contains("doctor_json=") && source.contains(">\"$doctor_json\""),
        "doctor JSON must be redirected to a private file"
    );
}

#[test]
fn smoke_forwards_one_target_in_order_without_printing_doctor_facts() {
    let temporary = tempfile::tempdir().expect("smoke script fixture");
    let root = temporary.path();
    fs::create_dir_all(root.join("scripts")).expect("scripts directory");
    fs::create_dir_all(root.join("dist/release")).expect("release directory");
    fs::create_dir_all(root.join("bin")).expect("fake bin");
    fs::create_dir_all(root.join("tmp")).expect("private temp parent");
    fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/smoke-pi.sh"),
        root.join("scripts/smoke-pi.sh"),
    )
    .expect("copy smoke script");
    fs::set_permissions(
        root.join("scripts/smoke-pi.sh"),
        fs::Permissions::from_mode(0o755),
    )
    .expect("smoke script mode");

    let png = root.join("fixture.png");
    let file = fs::File::create(&png).expect("PNG fixture");
    let mut encoder = png::Encoder::new(file, 480, 480);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().expect("PNG header");
    writer
        .write_image_data(&vec![0; 480 * 480 * 4])
        .expect("PNG pixels");

    let fake_mise = root.join("bin/mise");
    fs::write(
        &fake_mise,
        r#"#!/bin/sh
set -eu
printf '%s\n' "$*" >>"$FAKE_LOG"
case "$1:$2" in
  run:status)
    printf '%s\n' 'Plane Radar healthy'
    ;;
  run:doctor)
    printf '%s\n' '{"healthy":true,"target_serial":"SERIAL-MUST-STAY-PRIVATE"}'
    ;;
  run:screenshot)
    shift 2
    while test "$#" -gt 0; do
      if test "$1" = --output; then
        cp "$FAKE_PNG" "$2"
        exit 0
      fi
      shift
    done
    exit 64
    ;;
  run:smoke-verify)
    shift 2
    doctor=
    while test "$#" -gt 0; do
      if test "$1" = --doctor-json; then
        doctor=$2
        break
      fi
      shift
    done
    test -n "$doctor"
    test -f "$doctor"
    grep -q SERIAL-MUST-STAY-PRIVATE "$doctor"
    test -f dist/smoke-radar.png
    printf '%s\n' 'Plane Radar smoke verified'
    ;;
  *)
    exit 64
    ;;
esac
"#,
    )
    .expect("fake mise");
    fs::set_permissions(&fake_mise, fs::Permissions::from_mode(0o755)).expect("fake mise mode");
    let log = root.join("mise.log");
    let output = Command::new(root.join("scripts/smoke-pi.sh"))
        .arg("pi@test.invalid")
        .current_dir(root)
        .env(
            "PATH",
            format!("{}:/usr/bin:/bin", root.join("bin").display()),
        )
        .env("TMPDIR", root.join("tmp"))
        .env("FAKE_LOG", &log)
        .env("FAKE_PNG", &png)
        .output()
        .expect("run smoke script");
    assert!(
        output.status.success(),
        "smoke failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("smoke stdout");
    assert!(!stdout.contains("SERIAL-MUST-STAY-PRIVATE"));
    assert!(!String::from_utf8_lossy(&output.stderr).contains("SERIAL-MUST-STAY-PRIVATE"));
    let invocations = fs::read_to_string(log).expect("mise invocation log");
    let lines = invocations.lines().collect::<Vec<_>>();
    assert_eq!(lines.len(), 4);
    assert_eq!(lines[0], "run status -- pi@test.invalid");
    assert_eq!(lines[1], "run doctor -- pi@test.invalid --json");
    assert_eq!(
        lines[2],
        "run screenshot -- pi@test.invalid --output dist/smoke-radar.png"
    );
    assert!(lines[3].starts_with("run smoke-verify -- --release-dir dist/release "));
    assert!(root.join("dist/smoke-radar.png").is_file());
}

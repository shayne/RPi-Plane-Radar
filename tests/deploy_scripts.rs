use std::fs;
use std::path::Path;

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

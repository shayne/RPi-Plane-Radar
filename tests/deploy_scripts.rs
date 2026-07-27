use std::fs;
use std::path::Path;

const PUBLIC_TARGET: &str = "pi@raspberrypi.local";

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
fn shell_scripts_contain_only_the_documented_public_ssh_target() {
    let scripts = Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts");
    for entry in fs::read_dir(scripts).expect("scripts directory must be readable") {
        let path = entry.expect("script entry").path();
        if path.extension().is_none_or(|extension| extension != "sh") {
            continue;
        }
        let source = fs::read_to_string(&path).expect("script must be readable");
        for token in source.split(|character: char| {
            !character.is_ascii_alphanumeric() && !matches!(character, '.' | '_' | '-' | '@')
        }) {
            let token = token.trim_matches('-');
            let Some((user, host)) = token.split_once('@') else {
                continue;
            };
            if token.matches('@').count() != 1
                || user.is_empty()
                || !user
                    .chars()
                    .next()
                    .is_some_and(|character| character.is_ascii_alphabetic())
                || !host.contains('.')
                || !host
                    .chars()
                    .next()
                    .is_some_and(|character| character.is_ascii_alphabetic())
            {
                continue;
            }
            assert_eq!(
                token,
                PUBLIC_TARGET,
                "{} contains a non-public SSH target: {token}",
                path.display()
            );
        }
    }
}

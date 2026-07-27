use std::fs;
use std::path::Path;

#[test]
fn pi_application_scripts_share_the_public_target_override() {
    for relative in ["scripts/deploy-pi.sh", "scripts/smoke-pi.sh"] {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(relative);
        let source = fs::read_to_string(&path).expect("deployment script must be readable");
        assert!(
            source.contains(r#"target="${PLANERADAR_PI_TARGET:-shayne@planeradar.local}""#),
            "{relative} must honor PLANERADAR_PI_TARGET"
        );
        assert!(
            !source.contains(r#"target="shayne@planeradar.local""#),
            "{relative} must not hard-code the maintainer's SSH target"
        );
    }
}

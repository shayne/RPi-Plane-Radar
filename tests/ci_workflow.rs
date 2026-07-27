use std::fs;
use std::path::Path;

#[test]
fn ci_installs_required_rust_components_before_verification() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(".github/workflows/ci.yml");
    let workflow = fs::read_to_string(path).expect("CI workflow must be readable");

    let component_step = workflow
        .find("rustup component add clippy rustfmt")
        .expect("CI must install clippy and rustfmt explicitly");
    let verify_step = workflow
        .find("mise run verify")
        .expect("CI must run the complete verification task");

    assert!(
        component_step < verify_step,
        "CI must install Rust components before verification"
    );
}

use std::process::Command;

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
}

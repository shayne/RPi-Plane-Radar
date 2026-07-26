use std::process::Command;

#[test]
fn version_reports_name_and_revision() {
    let output = Command::new(env!("CARGO_BIN_EXE_planeradar"))
        .arg("version")
        .output()
        .expect("run planeradar");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("utf8");
    assert!(stdout.starts_with("planeradar "));
    assert!(stdout.contains("development") || stdout.contains('('));
}

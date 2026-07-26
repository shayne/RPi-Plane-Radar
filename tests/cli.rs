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

#[test]
fn radar_demo_requires_a_bounded_seconds_argument() {
    let output = Command::new(env!("CARGO_BIN_EXE_planeradar"))
        .args(["demo", "radar", "--seconds", "not-a-number"])
        .output()
        .expect("run planeradar");

    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("invalid value"));
}

#[test]
fn render_fixtures_writes_three_pngs_only_to_the_explicit_output() {
    let output_directory = tempfile::tempdir().expect("temporary directory");
    let output = Command::new(env!("CARGO_BIN_EXE_planeradar"))
        .args([
            "render-fixtures",
            "--output",
            output_directory.path().to_str().expect("UTF-8 path"),
        ])
        .output()
        .expect("run planeradar");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut names: Vec<_> = std::fs::read_dir(output_directory.path())
        .expect("fixture output directory")
        .map(|entry| {
            entry
                .expect("fixture entry")
                .file_name()
                .into_string()
                .expect("UTF-8 fixture name")
        })
        .collect();
    names.sort();
    assert_eq!(
        names,
        ["radar-empty.png", "radar-stale.png", "radar-traffic.png"]
    );
}

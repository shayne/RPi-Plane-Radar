use std::fs;
use std::os::unix::fs::PermissionsExt;

use planeradar::install::{
    InstallError, edit_boot_config, edit_boot_config_from_source, ensure_overlay,
};

const DECLARATION: &str = "dtoverlay=vc4-kms-dpi-hyperpixel2r";

#[test]
fn adds_one_overlay_under_all() {
    let source = "[all]\ndtoverlay=vc4-kms-v3d\n";
    let (updated, changed) = ensure_overlay(source, DECLARATION);
    assert!(changed);
    assert_eq!(updated.matches(DECLARATION).count(), 1);
    assert_eq!(
        updated,
        "[all]\ndtoverlay=vc4-kms-dpi-hyperpixel2r\ndtoverlay=vc4-kms-v3d\n"
    );
}

#[test]
fn second_edit_is_identical() {
    let source = "[all]\ndtoverlay=vc4-kms-dpi-hyperpixel2r\n";
    assert_eq!(
        ensure_overlay(source, DECLARATION),
        (source.to_owned(), false)
    );
}

#[test]
fn commented_declaration_does_not_count_as_active() {
    let source = "[all]\n# dtoverlay=vc4-kms-dpi-hyperpixel2r\n";
    assert_eq!(
        ensure_overlay(source, DECLARATION),
        (
            "[all]\ndtoverlay=vc4-kms-dpi-hyperpixel2r\n# dtoverlay=vc4-kms-dpi-hyperpixel2r\n"
                .to_owned(),
            true,
        )
    );
}

#[test]
fn duplicate_active_declarations_are_collapsed_under_last_all() {
    let source = concat!(
        "dtoverlay=vc4-kms-dpi-hyperpixel2r\n",
        "[pi4]\n",
        "dtoverlay=vc4-kms-dpi-hyperpixel2r\n",
        "[all]\n",
        "dtparam=i2c_arm=off\n",
        "[all]\n",
        "dtoverlay=vc4-kms-v3d\n",
        "dtoverlay=vc4-kms-dpi-hyperpixel2r\n",
    );
    let (updated, changed) = ensure_overlay(source, DECLARATION);
    assert!(changed);
    assert_eq!(updated.matches(DECLARATION).count(), 1);
    assert_eq!(
        updated,
        concat!(
            "[pi4]\n",
            "[all]\n",
            "dtparam=i2c_arm=off\n",
            "[all]\n",
            "dtoverlay=vc4-kms-dpi-hyperpixel2r\n",
            "dtoverlay=vc4-kms-v3d\n",
        )
    );
}

#[test]
fn preserves_crlf_newlines() {
    let source = "[all]\r\ndtoverlay=vc4-kms-v3d\r\n";
    assert_eq!(
        ensure_overlay(source, DECLARATION),
        (
            concat!(
                "[all]\r\n",
                "dtoverlay=vc4-kms-dpi-hyperpixel2r\r\n",
                "dtoverlay=vc4-kms-v3d\r\n",
            )
            .to_owned(),
            true,
        )
    );
}

#[test]
fn preserves_unrelated_mixed_newline_bytes() {
    let source = "[all]\r\ndtparam=audio=on\ndtoverlay=vc4-kms-v3d\r\n";
    assert_eq!(
        ensure_overlay(source, DECLARATION),
        (
            concat!(
                "[all]\r\n",
                "dtoverlay=vc4-kms-dpi-hyperpixel2r\r\n",
                "dtparam=audio=on\n",
                "dtoverlay=vc4-kms-v3d\r\n",
            )
            .to_owned(),
            true,
        )
    );
}

#[test]
fn preserves_missing_final_newline() {
    let source = "[all]\ndtoverlay=vc4-kms-v3d";
    assert_eq!(
        ensure_overlay(source, DECLARATION),
        (
            concat!(
                "[all]\n",
                "dtoverlay=vc4-kms-dpi-hyperpixel2r\n",
                "dtoverlay=vc4-kms-v3d",
            )
            .to_owned(),
            true,
        )
    );
}

#[test]
fn appends_all_section_when_missing() {
    let source = "[pi4]\ndtoverlay=vc4-kms-v3d\n";
    assert_eq!(
        ensure_overlay(source, DECLARATION),
        (
            concat!(
                "[pi4]\n",
                "dtoverlay=vc4-kms-v3d\n",
                "[all]\n",
                "dtoverlay=vc4-kms-dpi-hyperpixel2r\n",
            )
            .to_owned(),
            true,
        )
    );
}

#[test]
fn edit_creates_one_backup_and_preserves_file_mode() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let path = directory.path().join("config.txt");
    let original = "[all]\ndtoverlay=vc4-kms-v3d\n";
    fs::write(&path, original).expect("write fixture");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).expect("set fixture mode");

    assert!(edit_boot_config(&path, DECLARATION).expect("first edit"));
    assert_eq!(
        fs::read_to_string(directory.path().join("config.txt.planeradar-backup"))
            .expect("read backup"),
        original
    );
    assert_eq!(
        fs::metadata(&path)
            .expect("updated metadata")
            .permissions()
            .mode()
            & 0o777,
        0o640
    );
    assert!(!edit_boot_config(&path, DECLARATION).expect("idempotent edit"));
}

#[test]
fn approved_preview_rejects_a_concurrent_boot_config_change() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let path = directory.path().join("config.txt");
    let approved = "[all]\ndtoverlay=vc4-kms-v3d\n";
    let concurrent = "[all]\ndtoverlay=vc4-kms-v3d\n# concurrent edit\n";
    fs::write(&path, approved).expect("write approved fixture");
    fs::write(&path, concurrent).expect("write concurrent fixture");

    let error = edit_boot_config_from_source(&path, approved, DECLARATION)
        .expect_err("concurrent edit must be rejected");
    assert!(matches!(error, InstallError::SourceChanged(changed) if changed == path));
    assert_eq!(
        fs::read_to_string(&path).expect("read rejected fixture"),
        concurrent
    );
    assert!(
        !directory
            .path()
            .join("config.txt.planeradar-backup")
            .exists()
    );
}

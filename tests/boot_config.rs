use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Stdio};

use planeradar::install::{
    BootConfigEditor, DisplaySelection, InstallError, commit_display_config, edit_boot_config,
    edit_boot_config_from_source, ensure_overlay, rollback_display_config,
    select_hyperpixel_overlay, stage_tryboot_config, stage_tryboot_config_if_source_matches,
    validate_boot_config,
};
use sha2::{Digest, Sha256};

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
fn removed_declaration_remains_the_newline_style_source() {
    let source = "dtoverlay=vc4-kms-dpi-hyperpixel2r\r\n[all]";
    assert_eq!(
        ensure_overlay(source, DECLARATION),
        (
            "[all]\r\ndtoverlay=vc4-kms-dpi-hyperpixel2r".to_owned(),
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

#[test]
fn cooperating_editors_serialize_preview_and_stale_source_check() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let path = directory.path().join("config.txt");
    let approved = "[all]\ndtoverlay=vc4-kms-v3d\n";
    fs::write(&path, approved).expect("write fixture");

    let first = BootConfigEditor::acquire(&path).expect("acquire first editor");
    let preview = first.read_source().expect("read locked preview");

    assert!(
        BootConfigEditor::try_acquire(&path)
            .expect("try second editor")
            .is_none(),
        "second cooperating editor must not acquire the preview lock"
    );
    assert_eq!(
        fs::read_to_string(&path).expect("read locked fixture"),
        approved,
        "second editor must not modify while lock acquisition is denied"
    );

    assert!(
        first
            .edit_from_source(&preview, DECLARATION)
            .expect("first locked edit")
    );
    drop(first);

    let second = BootConfigEditor::try_acquire(&path)
        .expect("try released editor")
        .expect("second editor acquires after release");
    let error = second
        .edit_from_source(&preview, "dtoverlay=planeradar-second-editor")
        .expect_err("second editor must observe its stale source");
    assert!(matches!(error, InstallError::SourceChanged(changed) if changed == path));
    drop(second);

    assert!(
        directory.path().join("config.txt.planeradar-lock").exists(),
        "the sibling lock file remains persistent"
    );
}

#[test]
fn configure_display_holds_preview_lock_until_confirmation() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let path = directory.path().join("config.txt");
    let source = "[all]\ndtoverlay=vc4-kms-v3d\n";
    fs::write(&path, source).expect("write fixture");

    let mut child = Command::new(env!("CARGO_BIN_EXE_planeradar"))
        .args(["configure-display", "--boot-config"])
        .arg(&path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn configure-display");
    let mut stdout = child.stdout.take().expect("capture stdout");
    let mut output = Vec::new();
    let prompt = b"Apply these changes? [y/N] ";
    while !output.ends_with(prompt) {
        let mut byte = [0_u8; 1];
        stdout.read_exact(&mut byte).expect("read preview output");
        output.push(byte[0]);
    }

    let second_acquired = BootConfigEditor::try_acquire(&path)
        .expect("try second editor")
        .is_some();

    child
        .stdin
        .take()
        .expect("open child stdin")
        .write_all(b"n\n")
        .expect("cancel configure-display");
    stdout
        .read_to_end(&mut output)
        .expect("read command output");
    assert!(child.wait().expect("wait for configure-display").success());

    assert!(
        !second_acquired,
        "configure-display must hold the preview lock while awaiting confirmation"
    );
    assert_eq!(
        fs::read_to_string(&path).expect("read cancelled fixture"),
        source
    );
}

#[test]
fn stock_selection_removes_every_active_custom_declaration_and_owned_parameter() {
    let source = concat!(
        "dtoverlay=planeradar-hyperpixel2r-111111111111\n",
        "dtparam=rotate=90\n",
        "[pi4]\n",
        " dtoverlay=planeradar-hyperpixel2r-222222222222 \n",
        " dtparam=touchscreen-inverted-x \n",
        "dtparam=audio=on\n",
        "[all]\n",
        "dtoverlay=vc4-kms-v3d\n",
    );
    assert_eq!(
        select_hyperpixel_overlay(source, DisplaySelection::Stock)
            .expect("stock config")
            .0,
        concat!(
            "[pi4]\n",
            "dtparam=audio=on\n",
            "[all]\n",
            "dtoverlay=vc4-kms-dpi-hyperpixel2r\n",
            "dtoverlay=vc4-kms-v3d\n",
        )
    );
}

#[test]
fn candidate_selection_removes_stock_and_older_custom_declarations() {
    let source = concat!(
        "dtoverlay=planeradar-hyperpixel2r-111111111111\n",
        "dtparam=rotate=180\n",
        "[all]\n",
        "dtoverlay=vc4-kms-dpi-hyperpixel2r\n",
    );
    assert_eq!(
        select_hyperpixel_overlay(
            source,
            DisplaySelection::Candidate {
                overlay: "planeradar-hyperpixel2r-0123456789ab",
                parameters: &["touchscreen-swapped-x-y", "touchscreen-inverted-x",],
            },
        )
        .expect("candidate config")
        .0,
        concat!(
            "[all]\n",
            "dtoverlay=planeradar-hyperpixel2r-0123456789ab\n",
            "dtparam=touchscreen-swapped-x-y\n",
            "dtparam=touchscreen-inverted-x\n",
        ),
    );
}

#[test]
fn declaration_comments_remain_byte_identical() {
    let source = concat!(
        "# dtoverlay=vc4-kms-dpi-hyperpixel2r\r\n",
        "[all]\r\n",
        "#dtoverlay=planeradar-hyperpixel2r-111111111111\r\n",
    );
    let selected = select_hyperpixel_overlay(
        source,
        DisplaySelection::Candidate {
            overlay: "planeradar-hyperpixel2r-0123456789ab",
            parameters: &[],
        },
    )
    .expect("candidate config")
    .0;
    assert!(selected.contains("# dtoverlay=vc4-kms-dpi-hyperpixel2r\r\n"));
    assert!(selected.contains("#dtoverlay=planeradar-hyperpixel2r-111111111111\r\n"));
}

#[test]
fn candidate_selection_is_idempotent() {
    let selection = DisplaySelection::Candidate {
        overlay: "planeradar-hyperpixel2r-0123456789ab",
        parameters: &["rotate=270", "touchscreen-inverted-y"],
    };
    let (selected, changed) =
        select_hyperpixel_overlay("[all]\n", selection).expect("first selection");
    assert!(changed);
    assert_eq!(
        select_hyperpixel_overlay(&selected, selection).expect("second selection"),
        (selected, false)
    );
}

#[test]
fn selection_preserves_crlf_and_missing_final_newline() {
    let source = "[all]\r\ndtoverlay=vc4-kms-dpi-hyperpixel2r";
    assert_eq!(
        select_hyperpixel_overlay(
            source,
            DisplaySelection::Candidate {
                overlay: "planeradar-hyperpixel2r-0123456789ab",
                parameters: &["rotate=90"],
            },
        )
        .expect("candidate config")
        .0,
        concat!(
            "[all]\r\n",
            "dtoverlay=planeradar-hyperpixel2r-0123456789ab\r\n",
            "dtparam=rotate=90",
        )
    );
}

#[test]
fn candidate_overlay_name_rejects_unversioned_or_unsafe_values() {
    for overlay in [
        "vc4-kms-dpi-hyperpixel2r",
        "planeradar-hyperpixel2r-0123456789ab/escape",
        "planeradar-hyperpixel2r-0123456789ab escape",
        "planeradar-hyperpixel2r-0123456789ab,rotate=90",
        "planeradar-hyperpixel2r-0123456789ag",
        "planeradar-hyperpixel2r-0123456789ABC",
    ] {
        assert!(
            select_hyperpixel_overlay(
                "[all]\n",
                DisplaySelection::Candidate {
                    overlay,
                    parameters: &[],
                },
            )
            .is_err(),
            "unsafe overlay was accepted: {overlay}"
        );
    }
}

#[test]
fn parameters_accept_only_the_overlay_contract() {
    for parameter in [
        "rotate=0",
        "rotate=90",
        "rotate=180",
        "rotate=270",
        "touchscreen-inverted-x",
        "touchscreen-inverted-y",
        "touchscreen-swapped-x-y",
    ] {
        select_hyperpixel_overlay(
            "[all]\n",
            DisplaySelection::Candidate {
                overlay: "planeradar-hyperpixel2r-0123456789ab",
                parameters: &[parameter],
            },
        )
        .unwrap_or_else(|error| panic!("supported parameter {parameter} was rejected: {error}"));
    }

    for parameter in [
        "rotate",
        "rotate=45",
        "touchscreen-inverted-x=1",
        "audio=on",
        "touchscreen-inverted-y\ninitramfs evil",
    ] {
        assert!(
            select_hyperpixel_overlay(
                "[all]\n",
                DisplaySelection::Candidate {
                    overlay: "planeradar-hyperpixel2r-0123456789ab",
                    parameters: &[parameter],
                },
            )
            .is_err(),
            "unsupported parameter was accepted: {parameter:?}"
        );
    }

    assert!(
        select_hyperpixel_overlay(
            "[all]\n",
            DisplaySelection::Candidate {
                overlay: "planeradar-hyperpixel2r-0123456789ab",
                parameters: &["rotate=90", "rotate=90"],
            },
        )
        .is_err(),
        "duplicate parameters must not create duplicate config entries"
    );
}

#[test]
fn each_candidate_parameter_is_emitted_on_its_own_line() {
    let selected = select_hyperpixel_overlay(
        "[all]\n",
        DisplaySelection::Candidate {
            overlay: "planeradar-hyperpixel2r-0123456789ab",
            parameters: &["rotate=180", "touchscreen-swapped-x-y"],
        },
    )
    .expect("candidate config")
    .0;
    assert_eq!(
        selected,
        concat!(
            "[all]\n",
            "dtoverlay=planeradar-hyperpixel2r-0123456789ab\n",
            "dtparam=rotate=180\n",
            "dtparam=touchscreen-swapped-x-y\n",
        )
    );
}

#[test]
fn boot_config_line_limit_accepts_98_bytes_and_rejects_99() {
    let valid = format!("{}\n", "x".repeat(98));
    validate_boot_config(&valid).expect("98-byte line");

    let invalid = format!("{}\n", "x".repeat(99));
    let error = validate_boot_config(&invalid).expect_err("99-byte line");
    assert!(matches!(
        error,
        InstallError::BootLineTooLong { line: 1, bytes: 99 }
    ));
}

#[test]
fn stage_writes_only_tryboot_and_preserves_normal_config_bytes() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let normal = directory.path().join("config.txt");
    let tryboot = directory.path().join("tryboot.txt");
    let source = "[all]\r\ndtoverlay=vc4-kms-dpi-hyperpixel2r";
    fs::write(&normal, source).expect("normal fixture");

    assert!(
        stage_tryboot_config(
            &normal,
            &tryboot,
            DisplaySelection::Candidate {
                overlay: "planeradar-hyperpixel2r-0123456789ab",
                parameters: &["rotate=90"],
            },
        )
        .expect("stage tryboot")
    );

    assert_eq!(fs::read(&normal).expect("normal bytes"), source.as_bytes());
    assert_eq!(
        fs::read_to_string(&tryboot).expect("tryboot config"),
        concat!(
            "[all]\r\n",
            "dtoverlay=planeradar-hyperpixel2r-0123456789ab\r\n",
            "dtparam=rotate=90",
        )
    );
    assert_eq!(
        fs::metadata(&tryboot)
            .expect("tryboot metadata")
            .permissions()
            .mode()
            & 0o777,
        0o644
    );
    assert!(
        !normal
            .with_file_name("config.txt.planeradar-backup")
            .exists()
    );
    assert!(
        !stage_tryboot_config(
            &normal,
            &tryboot,
            DisplaySelection::Candidate {
                overlay: "planeradar-hyperpixel2r-0123456789ab",
                parameters: &["rotate=90"],
            },
        )
        .expect("idempotent stage")
    );
}

#[test]
fn checked_stage_rejects_normal_config_drift_before_publishing_tryboot() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let normal = directory.path().join("config.txt");
    let tryboot = directory.path().join("tryboot.txt");
    let original = "[all]\ndtoverlay=vc4-kms-dpi-hyperpixel2r\n";
    let prior_tryboot = "[all]\n# prior candidate\n";
    fs::write(&normal, original).expect("normal fixture");
    fs::write(&tryboot, prior_tryboot).expect("prior tryboot");
    let expected = format!("{:x}", Sha256::digest(original.as_bytes()));

    let writer = BootConfigEditor::acquire(&normal).expect("cooperating writer");
    let source = writer.read_source().expect("writer source");
    assert!(
        writer
            .edit_from_source(&source, "dtoverlay=cooperating-writer")
            .expect("writer commit")
    );
    drop(writer);

    let error = stage_tryboot_config_if_source_matches(
        &normal,
        &tryboot,
        &expected,
        DisplaySelection::Candidate {
            overlay: "planeradar-hyperpixel2r-0123456789ab",
            parameters: &[],
        },
    )
    .expect_err("stale expected normal-config digest");
    assert!(matches!(error, InstallError::SourceChanged(changed) if changed == normal));
    assert_eq!(
        fs::read_to_string(&tryboot).expect("preserved tryboot"),
        prior_tryboot
    );
}

#[test]
fn stage_rejects_the_normal_config_as_the_tryboot_destination() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let normal = directory.path().join("config.txt");
    let original = "[all]\ndtoverlay=vc4-kms-dpi-hyperpixel2r\n";
    fs::write(&normal, original).expect("normal fixture");

    assert!(
        stage_tryboot_config(
            &normal,
            &normal,
            DisplaySelection::Candidate {
                overlay: "planeradar-hyperpixel2r-0123456789ab",
                parameters: &[],
            },
        )
        .is_err()
    );
    assert_eq!(
        fs::read_to_string(&normal).expect("normal config"),
        original
    );
}

#[test]
fn commit_preserves_one_backup_and_atomically_selects_candidate() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let normal = directory.path().join("config.txt");
    let original = "[all]\ndtoverlay=vc4-kms-dpi-hyperpixel2r\n";
    fs::write(&normal, original).expect("normal fixture");
    fs::set_permissions(&normal, fs::Permissions::from_mode(0o640)).expect("normal mode");

    assert!(
        commit_display_config(
            &normal,
            DisplaySelection::Candidate {
                overlay: "planeradar-hyperpixel2r-0123456789ab",
                parameters: &["touchscreen-inverted-x"],
            },
        )
        .expect("first commit")
    );
    assert!(
        commit_display_config(
            &normal,
            DisplaySelection::Candidate {
                overlay: "planeradar-hyperpixel2r-fedcba987654",
                parameters: &[],
            },
        )
        .expect("second commit")
    );

    assert_eq!(
        fs::read_to_string(directory.path().join("config.txt.planeradar-backup"))
            .expect("one preserved backup"),
        original
    );
    assert_eq!(
        fs::read_to_string(&normal).expect("committed config"),
        "[all]\ndtoverlay=planeradar-hyperpixel2r-fedcba987654\n"
    );
    assert_eq!(
        fs::metadata(&normal)
            .expect("normal metadata")
            .permissions()
            .mode()
            & 0o777,
        0o640
    );
}

#[test]
fn rollback_atomically_returns_normal_config_to_stock() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let normal = directory.path().join("config.txt");
    fs::write(
        &normal,
        concat!(
            "[all]\n",
            "dtoverlay=planeradar-hyperpixel2r-0123456789ab\n",
            "dtparam=touchscreen-inverted-y\n",
        ),
    )
    .expect("candidate fixture");

    assert!(rollback_display_config(&normal).expect("rollback"));
    assert_eq!(
        fs::read_to_string(&normal).expect("rolled back config"),
        "[all]\ndtoverlay=vc4-kms-dpi-hyperpixel2r\n"
    );
    assert!(!rollback_display_config(&normal).expect("idempotent rollback"));
}

#[test]
fn noninteractive_display_commands_report_their_outcome() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let normal = directory.path().join("config.txt");
    let tryboot = directory.path().join("tryboot.txt");
    fs::write(&normal, "[all]\ndtoverlay=vc4-kms-dpi-hyperpixel2r\n").expect("normal fixture");

    let staged = Command::new(env!("CARGO_BIN_EXE_planeradar"))
        .args(["stage-display", "--boot-config"])
        .arg(&normal)
        .args(["--tryboot-config"])
        .arg(&tryboot)
        .args([
            "--overlay",
            "planeradar-hyperpixel2r-0123456789ab",
            "--parameter",
            "touchscreen-swapped-x-y",
        ])
        .output()
        .expect("stage command");
    assert!(
        staged.status.success(),
        "stage stderr: {}",
        String::from_utf8_lossy(&staged.stderr)
    );
    assert_eq!(
        String::from_utf8(staged.stdout).expect("stage stdout"),
        format!("staged {}\n", tryboot.display())
    );

    let committed = Command::new(env!("CARGO_BIN_EXE_planeradar"))
        .args(["commit-display", "--boot-config"])
        .arg(&normal)
        .args(["--overlay", "planeradar-hyperpixel2r-0123456789ab"])
        .output()
        .expect("commit command");
    assert!(committed.status.success());
    assert_eq!(committed.stdout, b"changed\n");

    let rolled_back = Command::new(env!("CARGO_BIN_EXE_planeradar"))
        .args(["rollback-display", "--boot-config"])
        .arg(&normal)
        .output()
        .expect("rollback command");
    assert!(rolled_back.status.success());
    assert_eq!(rolled_back.stdout, b"changed\n");
}

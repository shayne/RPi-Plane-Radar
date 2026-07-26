use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::Path;
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

fn write_executable(path: &Path, contents: &str) {
    fs::write(path, contents).expect("write executable fixture");
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("make fixture executable");
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[test]
fn staging_script_publishes_a_complete_idempotent_sandbox_bundle_without_rebooting() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    let fixture = tempfile::tempdir().expect("temporary directory");
    let driver = fixture.path().join("driver");
    let target_manifest = fixture.path().join("target.txt");
    let app = fixture.path().join("app");
    let bin = fixture.path().join("bin");
    let root = fixture.path().join("root");
    let log = fixture.path().join("commands.log");
    fs::create_dir_all(&driver).expect("driver fixture");
    fs::create_dir_all(&app).expect("app fixture");
    fs::create_dir_all(&bin).expect("bin fixture");
    fs::create_dir_all(root.join("boot/firmware/overlays")).expect("boot fixture");
    fs::write(
        root.join("boot/firmware/config.txt"),
        "[all]\ndtoverlay=vc4-kms-dpi-hyperpixel2r\n",
    )
    .expect("normal config fixture");

    let revision = String::from_utf8(
        Command::new("git")
            .args(["rev-parse", "HEAD"])
            .output()
            .expect("read revision")
            .stdout,
    )
    .expect("revision UTF-8")
    .trim()
    .to_owned();
    let source_tree = String::from_utf8(
        Command::new("git")
            .args(["rev-parse", "HEAD^{tree}"])
            .output()
            .expect("read source tree")
            .stdout,
    )
    .expect("source tree UTF-8")
    .trim()
    .to_owned();
    let release = "6.18.34+rpt-rpi-v8";
    let overlay_file = format!("planeradar-hyperpixel2r-{}.dtbo", &revision[..12]);
    let module = b"fixture arm64 module";
    let overlay = b"fixture validated overlay";
    fs::write(driver.join("planeradar_hyperpixel2r.ko"), module).expect("module fixture");
    fs::write(driver.join(&overlay_file), overlay).expect("overlay fixture");
    fs::write(
        driver.join("planeradar-hyperpixel2r-applied.dtb"),
        b"fixture applied dtb",
    )
    .expect("applied fixture");
    fs::write(
        driver.join("module.sha256"),
        format!("{}  planeradar_hyperpixel2r.ko\n", sha256_hex(module)),
    )
    .expect("module checksum fixture");
    fs::write(driver.join("module.file.txt"), "ARM aarch64\n").expect("file fixture");
    fs::write(
        driver.join("module.modinfo.txt"),
        format!("license: GPL\nvermagic: {release}\n"),
    )
    .expect("modinfo fixture");
    fs::write(driver.join("module.readelf.txt"), "Machine: AArch64\n").expect("readelf fixture");
    let host_helpers = [
        ("host-fixdep", b"fixture native fixdep".as_slice()),
        ("host-modpost", b"fixture native modpost".as_slice()),
        ("host-genksyms", b"fixture native genksyms".as_slice()),
    ];
    for (name, bytes) in host_helpers {
        fs::write(driver.join(name), bytes).expect("host-helper evidence fixture");
    }
    let source_deb_sha = "3".repeat(64);
    let base_dtb_sha = "4".repeat(64);
    fs::write(
        &target_manifest,
        format!(
            "kernel_release\t{release}\n\
             kernel_arch\taarch64\n\
             kernel_source_package\tlinux\n\
             kernel_source_version\t1:6.18.34-1+rpt1\n\
             kernel_source_deb_package\tlinux-source-6.18\n\
             kernel_source_deb_sha256\t{source_deb_sha}\n\
             base_dtb_sha256\t{base_dtb_sha}\n"
        ),
    )
    .expect("target provenance fixture");
    fs::write(
        driver.join("manifest.txt"),
        format!(
            concat!(
                "source_revision\t{revision}\n",
                "source_tree\t{source_tree}\n",
                "source_dirty\tfalse\n",
                "kernel_release\t{release}\n",
                "kernel_arch\taarch64\n",
                "build_image\tplaneradar-kernel-builder:test\n",
                "build_command\tmake test modules\n",
                "build_host_arch\taarch64\n",
                "kernel_source_package\tlinux\n",
                "kernel_source_version\t1:6.18.34-1+rpt1\n",
                "kernel_source_deb_package\tlinux-source-6.18\n",
                "kernel_source_deb_sha256\t{source_deb_sha}\n",
                "host_fixdep_sha256\t{host_fixdep_sha}\n",
                "host_modpost_sha256\t{host_modpost_sha}\n",
                "host_genksyms_sha256\t{host_genksyms_sha}\n",
                "base_dtb_sha256\t{base_dtb_sha}\n",
                "overlay_file\t{overlay_file}\n",
                "overlay_sha256\t{overlay_sha}\n",
                "overlay_applied_dtb\tplaneradar-hyperpixel2r-applied.dtb\n",
                "module_file\tplaneradar_hyperpixel2r.ko\n",
                "module_sha256\t{module_sha}\n",
                "module_vermagic\t{release} SMP aarch64\n",
                "module_license\tGPL\n",
            ),
            revision = revision,
            source_tree = source_tree,
            release = release,
            overlay_file = overlay_file,
            source_deb_sha = source_deb_sha,
            host_fixdep_sha =
                sha256_hex(&fs::read(driver.join("host-fixdep")).expect("fixdep fixture")),
            host_modpost_sha =
                sha256_hex(&fs::read(driver.join("host-modpost")).expect("modpost fixture")),
            host_genksyms_sha =
                sha256_hex(&fs::read(driver.join("host-genksyms")).expect("genksyms fixture")),
            base_dtb_sha = base_dtb_sha,
            overlay_sha = sha256_hex(overlay),
            module_sha = sha256_hex(module),
        ),
    )
    .expect("manifest fixture");

    let host_binary = repository.join("target/debug/planeradar");
    let app_wrapper = format!(
        "#!/usr/bin/env bash\n\
         if test \"${{1-}}\" = version; then printf 'planeradar 0.1.0 ({revision})\\n'; exit; fi\n\
         if test \"${{1-}}\" = stage-display && test -n \"${{PLANERADAR_TEST_FAIL_STAGE:-}}\"; then\n\
           '{}' \"$@\"\n\
           while test \"$#\" -gt 0; do\n\
             if test \"$1\" = --tryboot-config; then shift; printf '%099d\\n' 0 >> \"$1\"; exit; fi\n\
             shift\n\
           done\n\
           exit 64\n\
         fi\n\
         exec '{}' \"$@\"\n",
        host_binary.display(),
        host_binary.display(),
    );
    write_executable(&app.join("planeradar"), &app_wrapper);
    let app_bytes = fs::read(app.join("planeradar")).expect("app bytes");
    fs::write(app.join("planeradar.revision"), format!("{revision}\n")).expect("app revision");
    fs::write(app.join("planeradar.tree"), format!("{source_tree}\n")).expect("app source tree");
    fs::write(
        app.join("planeradar.sha256"),
        format!("{}  planeradar\n", sha256_hex(&app_bytes)),
    )
    .expect("app checksum");
    fs::write(app.join("planeradar.readelf.txt"), "Machine: AArch64\n").expect("app readelf");

    write_executable(
        &bin.join("but"),
        "#!/usr/bin/env bash\nprintf 'zz [uncommitted] (no changes)\\n'\n",
    );
    let real_git = String::from_utf8(
        Command::new("which")
            .arg("git")
            .output()
            .expect("find git")
            .stdout,
    )
    .expect("git path")
    .trim()
    .to_owned();
    write_executable(
        &bin.join("git"),
        &format!(
            "#!/usr/bin/env bash\nif test \"${{1-}}\" = status; then exit 0; fi\nexec '{real_git}' \"$@\"\n"
        ),
    );
    write_executable(
        &bin.join("ssh"),
        r#"#!/usr/bin/env bash
set -euo pipefail
while test "${1-}" = -o; do shift 2; done
shift
if test "${1-}" = uname && test "${2-}" = -r; then
  printf '%s\n' "$PLANERADAR_TEST_RELEASE"
  exit
fi
if test "${1-}" = mktemp && test "${2-}" = -d; then
  stage=/tmp/planeradar-hyperpixel-stage.fixture
  mkdir -p "$PLANERADAR_TEST_ROOT$stage"
  printf '%s\n' "$stage"
  exit
fi
if test "${1-}" = rm && test "${2-}" = -rf; then
  shift 3
  rm -rf "$PLANERADAR_TEST_ROOT$1"
  exit
fi
if test "${1-}" = bash; then
  PLANERADAR_INSTALL_ROOT="$PLANERADAR_TEST_ROOT" \
    PLANERADAR_TEST_LOG="$PLANERADAR_TEST_LOG" \
    bash "${@:2}"
  exit
fi
printf 'unexpected ssh command: %s\n' "$*" >&2
exit 64
"#,
    );
    write_executable(
        &bin.join("scp"),
        r#"#!/usr/bin/env bash
set -euo pipefail
while test "${1-}" = -o; do shift 2; done
while [[ "${1-}" == -* ]]; do shift; done
source="$1"
destination="${2#*:}"
cp -Rp "${source%/.}/." "$PLANERADAR_TEST_ROOT$destination"
"#,
    );
    write_executable(
        &bin.join("sudo"),
        r#"#!/usr/bin/env bash
set -euo pipefail
if test "${1-}" = chown; then exit; fi
if test "${1-}" = -u; then shift 2; fi
exec "$@"
"#,
    );
    write_executable(
        &bin.join("apt-get"),
        "#!/usr/bin/env bash\nprintf 'apt-get %s\\n' \"$*\" >> \"$PLANERADAR_TEST_LOG\"\n",
    );
    write_executable(
        &bin.join("depmod"),
        "#!/usr/bin/env bash\nprintf 'depmod %s\\n' \"$*\" >> \"$PLANERADAR_TEST_LOG\"\n",
    );
    write_executable(
        &bin.join("stat"),
        r#"#!/usr/bin/env bash
if test "${1-}" = -c; then
  case "$2" in
    %a) /usr/bin/stat -f '%Lp' "$3" ;;
    %U:%G) printf 'root:root\n' ;;
    *) exit 64 ;;
  esac
else
  exec /usr/bin/stat "$@"
fi
"#,
    );
    write_executable(
        &bin.join("dkms"),
        r#"#!/usr/bin/env bash
set -euo pipefail
printf 'dkms %s\n' "$*" >> "$PLANERADAR_TEST_LOG"
marker="$PLANERADAR_INSTALL_ROOT/var/lib/dkms/planeradar-hyperpixel2r/0.1.0/registered"
if test "${1-}" = status; then
  if test -f "$marker"; then
    printf 'planeradar-hyperpixel2r/0.1.0: added\n'
  fi
  exit
fi
if test "${1-}" = add; then mkdir -p "$(dirname "$marker")"; : > "$marker"; exit; fi
exit 64
"#,
    );

    let original_path = std::env::var("PATH").expect("PATH");
    let fixture_path = format!("{}:{original_path}", bin.display());
    let run_stage = || {
        Command::new(repository.join("scripts/stage-hyperpixel-tryboot.sh"))
            .arg("--parameter")
            .arg("rotate=90")
            .env("PATH", &fixture_path)
            .env("PLANERADAR_DRIVER_ARTIFACT_DIR", &driver)
            .env("PLANERADAR_KERNEL_TARGET_MANIFEST", &target_manifest)
            .env("PLANERADAR_APP_ARTIFACT_DIR", &app)
            .env("PLANERADAR_TEST_ROOT", &root)
            .env("PLANERADAR_TEST_LOG", &log)
            .env("PLANERADAR_TEST_RELEASE", release)
            .output()
            .expect("run staging script")
    };

    for attempt in 1..=2 {
        let output = run_stage();
        assert!(
            output.status.success(),
            "staging attempt {attempt} failed:\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("sudo reboot '0 tryboot'"));
    }

    assert_eq!(
        fs::read_to_string(root.join("boot/firmware/config.txt")).expect("normal config"),
        "[all]\ndtoverlay=vc4-kms-dpi-hyperpixel2r\n"
    );
    assert_eq!(
        fs::read_to_string(root.join("boot/firmware/tryboot.txt")).expect("tryboot config"),
        format!(
            "[all]\ndtoverlay={}\ndtparam=rotate=90\n",
            overlay_file.trim_end_matches(".dtbo")
        )
    );
    let artifact_dir = root
        .join("usr/lib/planeradar/hyperpixel")
        .join(&revision)
        .join(release);
    for name in [
        "manifest.txt",
        "planeradar",
        "planeradar.revision",
        "planeradar.tree",
        "planeradar.sha256",
        "planeradar_hyperpixel2r.ko",
        "host-fixdep",
        "host-modpost",
        "host-genksyms",
        overlay_file.as_str(),
        "display-parameters.txt",
    ] {
        assert!(artifact_dir.join(name).is_file(), "missing staged {name}");
    }
    assert_eq!(
        fs::read(root.join(format!(
            "lib/modules/{release}/extra/planeradar_hyperpixel2r.ko"
        )))
        .expect("installed module"),
        module
    );
    assert_eq!(
        fs::read(root.join("boot/firmware/overlays").join(&overlay_file))
            .expect("installed overlay"),
        overlay
    );
    assert!(
        root.join("usr/src/planeradar-hyperpixel2r-0.1.0/dkms.conf")
            .is_file()
    );
    let commands = fs::read_to_string(&log).expect("command log");
    assert!(commands.contains("apt-get install"));
    assert!(commands.contains("dkms add"));
    assert_eq!(commands.matches("dkms add").count(), 1);
    assert!(!commands.contains("dkms build"));
    assert!(!commands.contains("dkms install"));
    assert!(commands.contains(&format!("depmod -a {release}")));
    assert!(!commands.contains("reboot"));
    let tryboot_before_failed_stage =
        fs::read(root.join("boot/firmware/tryboot.txt")).expect("staged tryboot bytes");
    let failed_stage = Command::new(repository.join("scripts/stage-hyperpixel-tryboot.sh"))
        .arg("--parameter")
        .arg("rotate=90")
        .env("PATH", &fixture_path)
        .env("PLANERADAR_DRIVER_ARTIFACT_DIR", &driver)
        .env("PLANERADAR_KERNEL_TARGET_MANIFEST", &target_manifest)
        .env("PLANERADAR_APP_ARTIFACT_DIR", &app)
        .env("PLANERADAR_TEST_ROOT", &root)
        .env("PLANERADAR_TEST_LOG", &log)
        .env("PLANERADAR_TEST_RELEASE", release)
        .env("PLANERADAR_TEST_FAIL_STAGE", "1")
        .output()
        .expect("run deliberately failed staging script");
    assert!(
        !failed_stage.status.success(),
        "a post-write tryboot validation failure was accepted"
    );
    assert_eq!(
        fs::read(root.join("boot/firmware/tryboot.txt")).expect("restored tryboot bytes"),
        tryboot_before_failed_stage,
        "failed staging must restore the previously valid tryboot config"
    );

    let run_operator = |script: &str| {
        Command::new(repository.join("scripts").join(script))
            .env("PATH", &fixture_path)
            .env("PLANERADAR_DRIVER_ARTIFACT_DIR", &driver)
            .env("PLANERADAR_TEST_ROOT", &root)
            .env("PLANERADAR_TEST_LOG", &log)
            .env("PLANERADAR_TEST_RELEASE", release)
            .output()
            .unwrap_or_else(|error| panic!("run {script}: {error}"))
    };
    fs::create_dir_all(root.join("proc/device-tree/chosen/bootloader")).expect("tryboot fixture");
    fs::write(
        root.join("proc/device-tree/chosen/bootloader/tryboot"),
        [0, 0, 0, 1],
    )
    .expect("tryboot flag");
    let bound_device = root.join("sys/devices/platform/planeradar-hyperpixel2r");
    fs::create_dir_all(&bound_device).expect("platform device fixture");
    let platform_driver = root.join("sys/bus/platform/drivers/planeradar-hyperpixel2r");
    fs::create_dir_all(&platform_driver).expect("platform driver fixture");
    symlink(
        &bound_device,
        platform_driver.join("planeradar-hyperpixel2r.0"),
    )
    .expect("bound platform fixture");
    fs::create_dir_all(root.join("sys/class/drm/card0-DPI-1")).expect("DRM fixture");
    fs::write(root.join("sys/class/drm/card0-DPI-1/status"), "connected\n")
        .expect("connector status");
    fs::write(root.join("sys/class/drm/card0-DPI-1/modes"), "480x480\n").expect("connector mode");
    let touch_input = bound_device.join("i2c-11/11-0015/input/input0");
    fs::create_dir_all(&touch_input).expect("touch input fixture");
    fs::write(touch_input.join("name"), "EDT FT5406\n").expect("input name");
    let class_event = root.join("sys/class/input/event0");
    fs::create_dir_all(&class_event).expect("input class fixture");
    symlink(&touch_input, class_event.join("device")).expect("input device fixture");
    write_executable(
        &bin.join("uname"),
        &format!(
            "#!/usr/bin/env bash\ncase \"$1\" in -m) echo aarch64;; -r) echo '{release}';; *) exit 64;; esac\n"
        ),
    );
    write_executable(
        &bin.join("lsmod"),
        "#!/usr/bin/env bash\nprintf '%s\\n' 'Module Size Used_by' 'planeradar_hyperpixel2r 1 0' 'i2c_algo_bit 1 1' 'edt_ft5x06 1 0' 'vc4 1 0' 'v3d 1 0'\n",
    );
    write_executable(
        &bin.join("evtest"),
        r#"#!/usr/bin/env bash
set -euo pipefail
test "${1-}" != --info
test "$#" -eq 1
test "$(basename "$1")" = event0
printf '%s\n' \
  'Input driver version is 1.0.1' \
  'Input device ID: bus 0x18 vendor 0x0 product 0x0 version 0x0' \
  'Input device name: "EDT FT5406"' \
  'Supported events:' \
  '  Event type 0 (EV_SYN)' \
  '  Event type 1 (EV_KEY)' \
  '    Event code 330 (BTN_TOUCH)' \
  '  Event type 3 (EV_ABS)' \
  '    Event code 53 (ABS_MT_POSITION_X)' \
  '      Value      0' \
  '      Min        0' \
  '      Max      479' \
  '    Event code 54 (ABS_MT_POSITION_Y)' \
  '      Value      0' \
  '      Min        0' \
  '      Max      480' \
  'Properties:' \
  'Testing ... (interrupt to exit)'
"#,
    );
    write_executable(
        &bin.join("stdbuf"),
        r#"#!/usr/bin/env bash
set -euo pipefail
test "${1-}" = -oL
test "${2-}" = -eL
shift 2
exec "$@"
"#,
    );
    write_executable(
        &bin.join("timeout"),
        r#"#!/usr/bin/env bash
set -euo pipefail
if test "${1-}" = --signal=TERM &&
   test "${2-}" = --kill-after=1 &&
   test "${3-}" = 10 &&
   test "${4-}" = bash
then
  shift 3
  exec "$@"
fi
if test "${1-}" = 10; then
  shift
  exec "$@"
fi
test "${1-}" = --signal=INT
test "${2-}" = --kill-after=1
test "${3-}" = 2
shift 3
"$@"
exit 124
"#,
    );
    write_executable(
        &bin.join("systemd-run"),
        r#"#!/usr/bin/env bash
set -euo pipefail
printf 'systemd-run %s\n' "$*" >> "$PLANERADAR_TEST_LOG"
test "${1-}" = --unit=planeradar-hyperpixel-checkpoint
test "${2-}" = --collect
test "${3-}" = --uid=shayne
test "${4-}" = --property=StateDirectory=planeradar
test "${5-}" = --property=StateDirectoryMode=0750
test "${6-}" = --property=AmbientCapabilities=CAP_NET_BIND_SERVICE
state_dir="$PLANERADAR_TEST_ROOT/var/lib/planeradar"
mkdir -p "$state_dir"
chmod 0750 "$state_dir"
"#,
    );
    write_executable(
        &bin.join("systemctl"),
        r#"#!/usr/bin/env bash
set -euo pipefail
if test "${1-}" = show; then printf '4242\n'; exit; fi
if test "${1-}" = --failed; then exit; fi
if test "${1-}" = stop; then printf 'systemctl stop %s\n' "${2-}" >> "$PLANERADAR_TEST_LOG"; exit; fi
exit 64
"#,
    );
    write_executable(
        &bin.join("curl"),
        &format!(
            "#!/usr/bin/env bash\n\
             set -euo pipefail\n\
             test \"$#\" -eq 6\n\
             test \"$1\" = --fail\n\
             test \"$2\" = --silent\n\
             test \"$3\" = --show-error\n\
             test \"$4\" = --header\n\
             test \"$5\" = 'Host: planeradar.local'\n\
             test \"$6\" = http://127.0.0.1/healthz\n\
             printf '{{\"revision\":\"{revision}\"}}\\n'\n"
        ),
    );
    write_executable(
        &bin.join("journalctl"),
        r#"#!/usr/bin/env bash
case "$*" in
  "-b -n 0 --show-cursor --no-pager")
    printf '%s\n' '-- cursor: fixture-cursor'
    ;;
  *"--after-cursor=fixture-cursor"*)
    printf '%s\n' \
      'Jul 27 01:34:28 planeradar planeradar[2798]: [2026-07-27T05:34:28Z INFO  planeradar::display] SDL display ready: video_driver=KMSDRM render_driver=opengles2'
    ;;
  "-b --no-pager")
    printf '%s\n' 'clean boot log'
    ;;
  *) exit 64 ;;
esac
"#,
    );
    write_executable(
        &bin.join("kill"),
        r#"#!/usr/bin/env bash
printf 'kill %s\n' "$*" >> "$PLANERADAR_TEST_LOG"
cp "$PLANERADAR_TEST_VALID_PNG" \
  "$PLANERADAR_TEST_ROOT/var/lib/planeradar/debug.png"
"#,
    );
    write_executable(
        &bin.join("pngcheck"),
        "#!/usr/bin/env bash\ncmp -s \"$2\" \"$PLANERADAR_TEST_VALID_PNG\"\n",
    );

    let verified = Command::new(repository.join("scripts/verify-hyperpixel-boot.sh"))
        .arg("--expect-tryboot")
        .env("PATH", &fixture_path)
        .env("PLANERADAR_DRIVER_ARTIFACT_DIR", &driver)
        .env("PLANERADAR_TEST_ROOT", &root)
        .env("PLANERADAR_TEST_LOG", &log)
        .env("PLANERADAR_TEST_RELEASE", release)
        .env(
            "PLANERADAR_TEST_VALID_PNG",
            repository.join("tests/goldens/radar-empty.png"),
        )
        .output()
        .expect("run verification script");
    assert!(
        verified.status.success(),
        "verification failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&verified.stdout),
        String::from_utf8_lossy(&verified.stderr)
    );

    let committed = run_operator("commit-hyperpixel-boot.sh");
    assert!(
        committed.status.success(),
        "commit failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&committed.stdout),
        String::from_utf8_lossy(&committed.stderr)
    );
    assert!(String::from_utf8_lossy(&committed.stdout).contains("sudo reboot"));
    assert_eq!(
        fs::read_to_string(root.join("boot/firmware/config.txt")).expect("committed normal config"),
        format!(
            "[all]\ndtoverlay={}\ndtparam=rotate=90\n",
            overlay_file.trim_end_matches(".dtbo")
        )
    );
    assert_eq!(
        fs::read_to_string(root.join("boot/firmware/config.txt.planeradar-backup"))
            .expect("preserved normal backup"),
        "[all]\ndtoverlay=vc4-kms-dpi-hyperpixel2r\n"
    );

    let rolled_back = run_operator("rollback-hyperpixel-boot.sh");
    assert!(
        rolled_back.status.success(),
        "rollback failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&rolled_back.stdout),
        String::from_utf8_lossy(&rolled_back.stderr)
    );
    assert!(String::from_utf8_lossy(&rolled_back.stdout).contains("sudo reboot"));
    assert_eq!(
        fs::read_to_string(root.join("boot/firmware/config.txt"))
            .expect("rolled back normal config"),
        "[all]\ndtoverlay=vc4-kms-dpi-hyperpixel2r\n"
    );
    assert!(
        artifact_dir.is_dir(),
        "rollback must preserve versioned artifacts"
    );

    let normal_before_unsafe_manifest =
        fs::read(root.join("boot/firmware/config.txt")).expect("normal config snapshot");
    let tryboot_before_unsafe_manifest =
        fs::read(root.join("boot/firmware/tryboot.txt")).expect("tryboot snapshot");
    let valid_manifest = fs::read_to_string(driver.join("manifest.txt")).expect("valid manifest");
    fs::write(
        driver.join("manifest.txt"),
        valid_manifest.replace(
            &format!("overlay_file\t{overlay_file}"),
            "overlay_file\t../../escape.dtbo",
        ),
    )
    .expect("unsafe manifest fixture");
    let unsafe_stage = run_stage();
    assert!(
        !unsafe_stage.status.success(),
        "path-traversing overlay manifest was accepted"
    );
    assert_eq!(
        fs::read(root.join("boot/firmware/config.txt")).expect("normal after rejection"),
        normal_before_unsafe_manifest
    );
    assert_eq!(
        fs::read(root.join("boot/firmware/tryboot.txt")).expect("tryboot after rejection"),
        tryboot_before_unsafe_manifest
    );
}

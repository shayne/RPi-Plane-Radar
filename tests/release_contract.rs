use std::{
    collections::BTreeSet,
    fs::{self, File},
    io::{Read, Write},
    os::fd::{AsRawFd, FromRawFd, RawFd},
    os::unix::fs::{PermissionsExt, symlink},
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use serde_json::Value;
use sha2::{Digest, Sha256};

const RELEASE_ASSETS: [&str; 7] = [
    "SHA256SUMS",
    "SBOM.spdx.json",
    "install.sh",
    "planeradar-aarch64-linux-gnu.tar.zst",
    "planeradarctl-aarch64-apple-darwin.tar.zst",
    "planeradarctl-x86_64-apple-darwin.tar.zst",
    "release-manifest.json",
];

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(path: impl AsRef<Path>) -> String {
    fs::read_to_string(root().join(path)).expect("required release file")
}

fn write_executable(path: &Path, contents: &str) {
    fs::write(path, contents).expect("write executable fixture");
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("make fixture executable");
}

fn sha256(path: &Path) -> String {
    format!(
        "{:x}",
        Sha256::digest(fs::read(path).expect("read digest subject"))
    )
}

fn driver_lock_field(name: &str) -> String {
    read("driver.lock.toml")
        .lines()
        .find_map(|line| {
            let (key, value) = line.split_once('=')?;
            (key.trim() == name).then(|| value.trim().trim_matches('"').to_owned())
        })
        .unwrap_or_else(|| panic!("driver lock field {name}"))
}

fn packaged_macho_architecture(path: &Path) -> std::process::Output {
    let package = read("scripts/package-release.sh");
    let start = package
        .find("macho_architecture() {")
        .expect("portable Mach-O architecture helper");
    let end = package[start..]
        .find("\n}\n")
        .map(|offset| start + offset + 3)
        .expect("complete Mach-O architecture helper");
    let probe = format!(
        "set -eu\n{}\nmacho_architecture \"$1\"\n",
        &package[start..end]
    );
    Command::new("/bin/bash")
        .args(["-c", &probe, "planeradar-macho-probe"])
        .arg(path)
        .output()
        .expect("execute portable Mach-O helper")
}

#[test]
fn release_entrypoints_and_mise_task_are_checked_in() {
    for path in [
        "scripts/package-release.sh",
        "scripts/install.sh",
        "scripts/stable_release.py",
        "scripts/validate-release-metadata.sh",
        "scripts/validate-stable-release-tag.sh",
        "release/validator-requirements.in",
        "release/validator-requirements.txt",
        ".config/nextest.toml",
        ".github/workflows/release.yml",
        ".github/workflows/stable-draft.yml",
        ".github/workflows/stable-promote.yml",
    ] {
        let metadata = fs::metadata(root().join(path)).expect(path);
        assert!(metadata.is_file(), "{path} must be a regular file");
    }

    let mise = read("mise.toml");
    assert!(mise.contains("[tasks.package-release]"));
    assert!(mise.contains("./scripts/package-release.sh"));
    assert!(mise.contains("[tasks.validate-release-metadata]"));
    assert!(mise.contains("./scripts/validate-release-metadata.sh"));
    assert!(mise.contains("[tasks.validate-stable-release-tag]"));
    assert!(mise.contains("./scripts/validate-stable-release-tag.sh"));
    assert!(
        mise.contains("CARGO_TARGET_AARCH64_APPLE_DARWIN_LINKER = \"/usr/bin/clang\"")
            && mise.contains("CARGO_TARGET_X86_64_APPLE_DARWIN_LINKER = \"/usr/bin/clang\""),
        "both native macOS release targets must bypass non-Apple cc shims"
    );
}

#[test]
fn stable_release_state_machine_is_hostile_state_tested() {
    let output = Command::new("python3")
        .arg(root().join("tests/stable_release_contract.py"))
        .output()
        .expect("run stable release state-machine contract");
    assert!(
        output.status.success(),
        "stable release state-machine contract failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn stable_workflows_promote_the_exact_accepted_draft_without_rebuilding() {
    let draft = read(".github/workflows/stable-draft.yml");
    let promote = read(".github/workflows/stable-promote.yml");

    for required in [
        "Create unpublished stable app draft",
        "refs/heads/main",
        "mise run verify",
        "mise run package-release",
        "diff -r",
        "Attest every stable draft subject",
        "subject-path: dist/release/SHA256SUMS",
        "Create unpublished stable draft without a tag",
        "scripts/stable_release.py draft",
    ] {
        assert!(
            draft.contains(required),
            "stable draft workflow is missing {required:?}"
        );
    }
    for required in [
        "Promote accepted stable app draft",
        "source_commit",
        "release_id",
        "asset_fingerprint",
        "scripts/stable_release.py verify",
        "gh attestation verify",
        "gh attestation verify \"$downloads/SHA256SUMS\"",
        "scripts/stable_release.py publish",
        "scripts/stable_release.py confirm",
        "gh release verify \"$TAG\"",
        "gh release verify-asset \"$TAG\"",
    ] {
        assert!(
            promote.contains(required),
            "stable promotion workflow is missing {required:?}"
        );
    }
    assert!(
        !promote.contains("cargo build")
            && !promote.contains("mise run build-pi")
            && !promote.contains("mise run package-release")
            && !promote.contains("actions/upload-artifact"),
        "stable promotion must verify and publish the accepted draft without rebuilding or reuploading"
    );
    assert!(
        !promote.contains("--signer-repo"),
        "gh attestation verification must not combine the mutually exclusive --signer-repo and --signer-workflow policies"
    );
    for workflow in [&draft, &promote] {
        for line in workflow
            .lines()
            .filter(|line| line.trim().starts_with("uses:"))
        {
            let revision = line
                .split_once('@')
                .map(|(_, revision)| revision.split_whitespace().next().unwrap_or(""))
                .expect("stable action pin");
            assert_eq!(
                revision.len(),
                40,
                "stable third-party action must use a full commit SHA: {line}"
            );
            assert!(revision.bytes().all(|byte| byte.is_ascii_hexdigit()));
        }
    }
}

#[test]
fn stable_draft_installs_pinned_tools_before_parallel_source_verification() {
    let draft = read(".github/workflows/stable-draft.yml");
    let install = draft
        .find("mise install")
        .expect("stable draft must install the pinned mise environment");
    let verify = draft
        .find("mise run verify")
        .expect("stable draft must verify the exact source");

    assert!(
        install < verify,
        "stable draft must finish installing pinned tools before parallel verification"
    );
}

#[test]
fn authoritative_verification_covers_every_workspace_package_and_target() {
    let mise = read("mise.toml");
    let nextest = read(".config/nextest.toml");
    assert!(
        mise.contains(r#"run = "cargo nextest run --workspace --all-features""#,),
        "the authoritative test task must include every workspace package"
    );
    assert!(
        mise.contains(
            r#"run = "cargo clippy --workspace --all-targets --all-features -- -D warnings""#,
        ),
        "the authoritative lint task must include every workspace package and target"
    );
    assert!(
        nextest.contains("bootstrap-processes = { max-threads = 1 }")
            && nextest.contains(
                "binary(=release_contract) | binary(=bootstrap) | binary(=bootstrap_pty)",
            )
            && nextest.contains(r#"test-group = "bootstrap-processes""#)
            && nextest.contains(r#"threads-required = "num-test-threads""#),
        "the process and PTY release binaries must run serially with the global nextest pool reserved"
    );
}

#[test]
fn release_contract_declares_the_exact_public_asset_set() {
    let package = read("scripts/package-release.sh");
    for asset in RELEASE_ASSETS {
        assert!(
            package.contains(asset),
            "packager must name required asset {asset}"
        );
    }
    assert!(
        package.contains("reject_unexpected_release_files"),
        "packager must reject undeclared release files"
    );
}

#[test]
fn release_manifest_schema_carries_complete_source_driver_and_artifact_identity() {
    let schema: Value =
        serde_json::from_str(&read("release/release-manifest.schema.json")).expect("schema JSON");
    let required = schema["required"]
        .as_array()
        .expect("top-level required")
        .iter()
        .filter_map(Value::as_str)
        .collect::<BTreeSet<_>>();
    for field in [
        "schema_version",
        "version",
        "source_commit",
        "source_tree",
        "source_timestamp",
        "source_date_epoch",
        "repository",
        "workflow",
        "supported",
        "required_target_packages",
        "minimum_control_version",
        "driver",
        "artifacts",
    ] {
        assert!(required.contains(field), "manifest requires {field}");
    }

    let driver_required = schema["properties"]["driver"]["required"]
        .as_array()
        .expect("driver required")
        .iter()
        .filter_map(Value::as_str)
        .collect::<BTreeSet<_>>();
    assert_eq!(
        driver_required,
        BTreeSet::from([
            "commit",
            "lifecycle_protocol",
            "manifest_sha256",
            "repository",
            "version",
        ])
    );

    let artifact_required = schema["$defs"]["artifact"]["required"]
        .as_array()
        .expect("artifact required")
        .iter()
        .filter_map(Value::as_str)
        .collect::<BTreeSet<_>>();
    assert_eq!(
        artifact_required,
        BTreeSet::from([
            "architecture",
            "kind",
            "platform",
            "runnable",
            "sha256",
            "size",
        ])
    );

    assert!(
        schema["properties"]["artifacts"]["properties"]
            ["planeradar-aarch64-linux-gnu.tar.zst"]["allOf"][1]["properties"]["size"]
            .is_null(),
        "the application keeps the general 128 MiB artifact cap"
    );
    for control in [
        "planeradarctl-aarch64-apple-darwin.tar.zst",
        "planeradarctl-x86_64-apple-darwin.tar.zst",
    ] {
        assert_eq!(
            schema["properties"]["artifacts"]["properties"][control]["allOf"][1]["properties"]["size"]
                ["maximum"],
            16 * 1024 * 1024,
            "both control archives use the bootstrap's narrow compressed-size cap"
        );
    }
}

#[test]
fn packager_has_deterministic_source_and_archive_guards() {
    let package = read("scripts/package-release.sh");
    for required in [
        "git diff-index --quiet",
        "git merge-base --is-ancestor",
        "SOURCE_DATE_EPOCH",
        "--sort=name",
        "--owner=0",
        "--group=0",
        "--numeric-owner",
        "--env \"SOURCE_DATE_EPOCH=$SOURCE_DATE_EPOCH\"",
        "--mtime=\"@$SOURCE_DATE_EPOCH\"",
        "-o pipefail",
        "ARCHIVE_IMAGE=\"rust:1.97.1-trixie\"@\"sha256:",
        "snapshot.debian.org/archive/debian/20260701T000000Z",
        "./scripts/validate-release-metadata.sh",
        "PLANERADAR_WORKFLOW_REF",
        "PLANERADAR_WORKFLOW_COMMIT",
        "selected source does not match current checkout",
        "source changed while packaging",
        "version does not match Cargo.toml",
        "ELF 64-bit LSB",
        "Mach-O 64-bit",
        "arm64",
        "x86_64",
    ] {
        assert!(
            package.contains(required),
            "missing deterministic packaging guard {required:?}"
        );
    }
    assert!(
        !package.contains("--pax-option"),
        "GNU archive creation must not use POSIX-only pax options"
    );

    let dockerfile = read("packaging/Dockerfile.build");
    assert!(
        dockerfile.starts_with(
            "FROM rust:1.97.1-trixie@sha256:1bcff4befb740599103a2c7cb51058e14479b2e35e3a34a3f0dc4ede09927488"
        ),
        "the Linux builder must pin the OCI base by digest"
    );
    assert!(
        dockerfile.contains("snapshot.debian.org/archive/debian/20260701T000000Z"),
        "the Linux builder must use an immutable Debian snapshot"
    );
    assert!(
        !dockerfile.contains("deb.debian.org") && !dockerfile.contains("security.debian.org"),
        "the build must not consult mutable Debian repositories"
    );
}

#[test]
fn packager_portably_identifies_only_thin_supported_macho_inputs() {
    fn segment_command(file_size: u64, section_count: u32) -> Vec<u8> {
        let mut command = Vec::new();
        command.extend_from_slice(&0x19_u32.to_le_bytes());
        command.extend_from_slice(&72_u32.to_le_bytes());
        command.extend_from_slice(&[0; 16]);
        command.extend_from_slice(&0_u64.to_le_bytes());
        command.extend_from_slice(&file_size.to_le_bytes());
        command.extend_from_slice(&0_u64.to_le_bytes());
        command.extend_from_slice(&file_size.to_le_bytes());
        command.extend_from_slice(&5_u32.to_le_bytes());
        command.extend_from_slice(&5_u32.to_le_bytes());
        command.extend_from_slice(&section_count.to_le_bytes());
        command.extend_from_slice(&0_u32.to_le_bytes());
        assert_eq!(command.len(), 72);
        command
    }

    fn thin_macho(
        cpu_type: u32,
        file_type: u32,
        command_count: u32,
        command_bytes: u32,
        commands: &[u8],
    ) -> Vec<u8> {
        let mut bytes = Vec::new();
        for value in [
            0xfeed_facf,
            cpu_type,
            0,
            file_type,
            command_count,
            command_bytes,
            0,
            0,
        ] {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        bytes.extend_from_slice(commands);
        bytes
    }

    let valid_segment = segment_command(32 + 72, 0);
    let valid_arm64 = thin_macho(0x0100_000c, 2, 1, 72, &valid_segment);
    let valid_x86_64 = thin_macho(0x0100_0007, 2, 1, 72, &valid_segment);
    let temporary = tempfile::tempdir().expect("temporary Mach-O fixtures");

    for (name, bytes, expected) in [
        ("arm64", valid_arm64.clone(), "arm64"),
        ("x86_64", valid_x86_64.clone(), "x86_64"),
    ] {
        let path = temporary.path().join(name);
        fs::write(&path, bytes).expect("valid thin Mach-O fixture");
        let output = packaged_macho_architecture(&path);
        assert!(
            output.status.success(),
            "portable Mach-O helper rejected {name}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), expected);
    }

    let mut undersized_command = Vec::new();
    undersized_command.extend_from_slice(&0x19_u32.to_le_bytes());
    undersized_command.extend_from_slice(&4_u32.to_le_bytes());
    let mut trailing_command_bytes = valid_segment.clone();
    trailing_command_bytes.extend_from_slice(&[0; 8]);
    let uuid_command = [
        0x1b_u32.to_le_bytes().as_slice(),
        8_u32.to_le_bytes().as_slice(),
    ]
    .concat();
    let unsupported_cpu = thin_macho(0x0100_0012, 2, 1, 72, &valid_segment);
    let not_executable = thin_macho(0x0100_000c, 6, 1, 72, &valid_segment);
    let malformed_fixtures = [
        ("truncated-header", valid_arm64[..8].to_vec()),
        ("universal", b"\xca\xfe\xba\xbe\0\0\0\x02".to_vec()),
        ("unsupported-cpu", unsupported_cpu),
        ("not-executable", not_executable),
        ("zero-commands", thin_macho(0x0100_000c, 2, 0, 0, &[])),
        (
            "too-many-commands",
            thin_macho(0x0100_000c, 2, 4097, 72, &valid_segment),
        ),
        (
            "truncated-table",
            thin_macho(0x0100_000c, 2, 1, 72, &valid_segment[..8]),
        ),
        (
            "undersized-command",
            thin_macho(0x0100_000c, 2, 1, 8, &undersized_command),
        ),
        (
            "trailing-command-bytes",
            thin_macho(0x0100_000c, 2, 1, 80, &trailing_command_bytes),
        ),
        (
            "missing-segment",
            thin_macho(0x0100_000c, 2, 1, 8, &uuid_command),
        ),
        (
            "unbacked-segment",
            thin_macho(0x0100_000c, 2, 1, 72, &segment_command(1024 * 1024, 0)),
        ),
        (
            "truncated-sections",
            thin_macho(0x0100_000c, 2, 1, 72, &segment_command(32 + 72, 1)),
        ),
    ];
    for (name, bytes) in malformed_fixtures {
        let path = temporary.path().join(name);
        fs::write(&path, bytes).expect("malformed Mach-O fixture");
        let output = packaged_macho_architecture(&path);
        assert!(
            !output.status.success(),
            "portable Mach-O helper accepted malformed fixture {name}: {}",
            String::from_utf8_lossy(&output.stdout)
        );
    }
}

#[test]
fn release_packaging_verifiers_run_on_native_docker_hosts() {
    for (path, next_job) in [
        (".github/workflows/release.yml", "release"),
        (".github/workflows/stable-draft.yml", "draft"),
    ] {
        let workflow = read(path);
        let start = workflow
            .find("  verify-release:\n")
            .unwrap_or_else(|| panic!("{path} verify-release job"));
        let end_marker = format!("\n  {next_job}:\n");
        let end = workflow[start..]
            .find(&end_marker)
            .map(|offset| start + offset)
            .unwrap_or_else(|| panic!("{path} job after verify-release"));
        let verifier = &workflow[start..end];
        assert!(
            verifier.contains("runs-on: ubuntu-24.04-arm"),
            "{path} must package on a native Docker host"
        );
        assert!(
            verifier.contains("Install packaging dependencies")
                && verifier.contains("file libdigest-sha-perl libsdl2-dev pkg-config zstd"),
            "{path} must provision the non-mise packaging dependencies"
        );
        assert!(
            !verifier.contains("runs-on: macos"),
            "{path} must not require unsupported nested virtualization"
        );
    }
}

#[test]
fn release_provenance_is_the_dispatch_oidc_identity_end_to_end() {
    let workflow = read(".github/workflows/release.yml");
    assert!(
        workflow.contains("test \"$source_commit\" = \"$GITHUB_SHA\""),
        "the selected source must equal the workflow invocation commit"
    );
    assert!(workflow.contains("PLANERADAR_WORKFLOW_REF: ${{ github.ref }}"));
    assert!(workflow.contains("PLANERADAR_WORKFLOW_COMMIT: ${{ github.sha }}"));

    let package = read("scripts/package-release.sh");
    assert!(package.contains("workflow_ref = os.environ[\"PLANERADAR_WORKFLOW_REF\"]"));
    assert!(package.contains("workflow_commit = os.environ[\"PLANERADAR_WORKFLOW_COMMIT\"]"));
    assert!(
        package.contains("[[ \"$workflow_commit\" == \"$source_commit\" ]]"),
        "the packager must refuse a source different from the signing workflow commit"
    );

    let schema: Value =
        serde_json::from_str(&read("release/release-manifest.schema.json")).expect("schema JSON");
    let workflow_ref = schema["properties"]["workflow"]["properties"]["ref"]["pattern"]
        .as_str()
        .expect("workflow ref pattern");
    assert!(workflow_ref.contains("refs/(heads|tags)"));
}

#[test]
fn release_metadata_validation_is_official_hash_locked_and_mandatory() {
    let validator = read("scripts/validate-release-metadata.sh");
    assert!(validator.contains("--require-hashes"));
    assert!(validator.contains("--only-binary=:all:"));
    assert!(validator.contains("pyspdxtools"));
    assert!(validator.contains("check-jsonschema"));

    let requirements = read("release/validator-requirements.txt");
    assert!(requirements.contains("spdx-tools==0.8.3"));
    let requirement_lines = requirements.lines().collect::<Vec<_>>();
    for (index, line) in requirement_lines.iter().enumerate() {
        if line.starts_with(|character: char| character.is_ascii_alphanumeric())
            && line.contains("==")
        {
            assert!(
                line.contains("--hash=sha256:")
                    || requirement_lines
                        .get(index + 1)
                        .is_some_and(|next| next.contains("--hash=sha256:")),
                "locked requirement has no hash: {line}"
            );
        }
    }

    let package = read("scripts/package-release.sh");
    let validation = package
        .find("./scripts/validate-release-metadata.sh")
        .expect("official metadata validation");
    let checksums = package
        .find("done | LC_ALL=C sort -k2 >SHA256SUMS")
        .expect("checksum generation");
    assert!(
        validation < checksums,
        "invalid metadata must fail before packaging completes"
    );
}

#[test]
fn installer_is_a_verified_mac_bootstrap_with_typed_forwarding() {
    let installer = read("scripts/install.sh");
    for required in [
        "Darwin",
        "arm64",
        "x86_64",
        "command -v gh",
        "gh release view",
        "gh release verify",
        "gh release download",
        "gh attestation verify",
        "--signer-workflow",
        "--source-ref",
        "--source-digest",
        "release-manifest.json",
        "SHA256SUMS",
        "SBOM.spdx.json",
        "MAX_CONTROL_ARCHIVE_BYTES",
        "MAX_CONTROL_MEMBER_BYTES",
        "MAX_EXPANDED_ARCHIVE_BYTES",
        "ulimit -f",
        "df -Pk",
        "planeradarctl install",
        "control-bootstrap.continue",
        "--__planeradar-foreground-tty-v1",
        "foreground_control_terminal",
        "--__planeradar-restore-tty-v1",
        "restore_control_terminal",
        "await_control_completion",
        "control_retire_pending=1",
        "retire_completed_control_group",
        "control_reap_pid=$control_pid control_reap_pending=1 control_retire_pending=0 control_group_owned=0 control_pid=\"\"",
    ] {
        assert!(
            installer.contains(required),
            "bootstrap is missing {required:?}"
        );
    }
    let retirement_window_start = installer
        .find("{\n  retire_completed_control_group || retire_status=$?")
        .expect("control retirement must begin inside a stderr-suppressed compound command");
    let retirement_window_end = installer[retirement_window_start..]
        .find("} 2>/dev/null")
        .map(|offset| retirement_window_start + offset)
        .expect("control retirement suppression must have a bounded end");
    let retirement_window = &installer[retirement_window_start..retirement_window_end];
    assert!(
        retirement_window.contains(
            "control_reap_pid=$control_pid control_reap_pending=1 control_retire_pending=0 control_group_owned=0 control_pid=\"\""
        ) && retirement_window.contains("wait_retired_control || reap_status=$?"),
        "the suppressed retirement window must span group termination, authority transfer, and reap"
    );
    assert!(
        !installer.contains("ssh "),
        "bootstrap must delegate Pi mutation to planeradarctl"
    );
    assert!(
        !installer.contains("eval "),
        "bootstrap must never evaluate target/options as shell source"
    );
    assert!(
        !installer.contains("kill -0 \"$control_pid\""),
        "bootstrap must not probe a control PID after its ownership anchor can be reaped"
    );
    let native_control = read("crates/planeradarctl/src/main.rs");
    for required in [
        "tcgetpgrp",
        "tcsetpgrp",
        "Signal::SIGTTOU",
        "relay_terminal_signal",
        "foreground_internal_terminal",
        "restore_internal_terminal",
        "supervise_internal_control",
        "complete {worker_status}",
    ] {
        assert!(
            native_control.contains(required),
            "native terminal protocol is missing {required:?}"
        );
    }
}

#[test]
fn installer_converts_bytes_to_darwin_file_blocks_with_ceiling() {
    let installer = read("scripts/install.sh");
    let start = installer
        .find("darwin_file_blocks() {")
        .expect("central Darwin file-block helper");
    let end = installer[start..]
        .find("\n}\n")
        .map(|offset| start + offset + 3)
        .expect("complete Darwin file-block helper");
    let helper = &installer[start..end];
    let probe = format!(
        "set -eu\n{helper}\nfor bytes in 1 511 512 513 1024 65536 16777216; do darwin_file_blocks \"$bytes\"; done\n"
    );
    let output = Command::new("/bin/bash")
        .args(["-c", &probe])
        .output()
        .expect("execute Darwin file-block helper");
    assert!(
        output.status.success(),
        "Darwin file-block helper failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "1\n1\n1\n2\n2\n128\n32768\n"
    );
}

#[test]
fn ordinary_ci_is_read_only_and_covers_the_supported_build_matrix() {
    let ci = read(".github/workflows/ci.yml");
    assert!(ci.contains("permissions:\n  contents: read"));
    assert!(ci.contains("ubuntu-24.04-arm"));
    assert!(ci.contains("macos-15\n"));
    assert!(ci.contains("macos-15-intel"));
    assert!(ci.contains("mise run verify"));
    assert!(
        ci.contains("mise exec -- cargo test --test release_contract -- --test-threads=1"),
        "ordinary CI must serialize the explicit release-contract rerun"
    );
    assert!(ci.contains("README"));
}

#[test]
fn release_workflow_pins_actions_and_defers_all_write_authority_to_release_job() {
    let workflow = read(".github/workflows/release.yml");
    assert!(workflow.contains("workflow_dispatch:"));
    assert!(workflow.contains("contents: read"));
    assert!(workflow.contains("contents: write"));
    assert!(workflow.contains("id-token: write"));
    assert!(workflow.contains("attestations: write"));
    assert!(workflow.contains("draft: true"));
    assert!(workflow.contains("prerelease: true"));
    assert!(
        workflow.contains(r#"[[ "$version" =~ ^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)-rc\.([1-9][0-9]*)$ ]]"#),
        "the candidate workflow must reject stable-looking and unnumbered tags"
    );
    assert!(
        workflow.contains(
            r#"test "$(jq -r .workflow.path dist/release/release-manifest.json)" = ".github/workflows/release.yml""#
        ),
        "the candidate workflow must prove the packaged signer path before tagging"
    );
    assert!(
        workflow.contains("subject-path: dist/release/SHA256SUMS"),
        "the checksum manifest is a public asset and needs its own provenance attestation"
    );
    assert!(
        workflow.contains(
            "PLANERADAR_RELEASE_FIXTURE_DIR=\"$PWD/dist/release\" mise exec -- cargo test --test release_contract -- --test-threads=1"
        ),
        "release verification must serialize the real assembled-archive inspector"
    );

    let verify = workflow
        .find("Verify complete release")
        .expect("verification");
    let tag = workflow.find("Create annotated tag").expect("tag creation");
    let attest = workflow
        .find("Attest release subjects")
        .expect("attestations");
    let release = workflow
        .find("Create draft prerelease")
        .expect("draft release");
    let publish = workflow
        .find("Publish the verified prerelease")
        .expect("prerelease publication");
    assert!(verify < tag && tag < attest && attest < release && release < publish);
    assert!(
        workflow.contains("gh release edit \"$TAG\" --draft=false --prerelease"),
        "the successful release workflow must make the verified candidate installable"
    );

    for line in workflow
        .lines()
        .filter(|line| line.trim().starts_with("uses:"))
    {
        let revision = line
            .split_once('@')
            .map(|(_, revision)| revision.split_whitespace().next().unwrap_or(""))
            .expect("action pin");
        assert_eq!(
            revision.len(),
            40,
            "third-party action must use a full commit SHA: {line}"
        );
        assert!(revision.bytes().all(|byte| byte.is_ascii_hexdigit()));
    }
    assert!(
        workflow.contains("softprops/action-gh-release@3d0d9888cb7fd7b750713d6e236d1fcb99157228"),
        "softprops/action-gh-release must use the dereferenced v3.0.2 commit"
    );
}

#[test]
fn packager_rejects_a_dirty_release_source_before_building() {
    let temporary = clean_clone();
    fs::write(temporary.path().join("dirty"), b"not committed").expect("dirty fixture");
    let bin = temporary.path().join("fixture-bin");
    fs::create_dir(&bin).expect("fixture command directory");
    symlink("/bin/bash", bin.join("bash")).expect("fixture bash");
    symlink("/usr/bin/git", bin.join("git")).expect("fixture git");

    let output = package_release_fixture_command()
        .arg("0.1.0-rc.1")
        .current_dir(temporary.path())
        .env("PATH", &bin)
        .env("PLANERADAR_PACKAGE_NO_BUILD", "1")
        .output()
        .expect("run packager");
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("clean tracked source"),
        "unexpected error: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn package_release_fixture_command() -> Command {
    let mut command = Command::new(root().join("scripts/package-release.sh"));
    command
        .env_remove("PLANERADAR_WORKFLOW_REF")
        .env_remove("PLANERADAR_WORKFLOW_COMMIT");
    command
}

#[test]
fn packager_fixture_commands_clear_outer_workflow_identity() {
    let command = package_release_fixture_command();
    for key in ["PLANERADAR_WORKFLOW_REF", "PLANERADAR_WORKFLOW_COMMIT"] {
        assert!(
            command
                .get_envs()
                .any(|(name, value)| name == key && value.is_none()),
            "fixture command must clear inherited {key}"
        );
    }
}

fn clean_clone_from(source: &Path) -> tempfile::TempDir {
    let temporary = tempfile::tempdir().expect("temporary repository");
    let clone = Command::new("git")
        .args(["clone", "--quiet"])
        .arg(source)
        .arg(".")
        .current_dir(temporary.path())
        .output()
        .expect("clone fixture repository");
    assert!(
        clone.status.success(),
        "fixture clone failed: {}",
        String::from_utf8_lossy(&clone.stderr)
    );
    let symbolic_head = Command::new("git")
        .args(["symbolic-ref", "-q", "HEAD"])
        .current_dir(temporary.path())
        .output()
        .expect("inspect fixture repository HEAD");
    if !symbolic_head.status.success() {
        let status = Command::new("git")
            .args(["switch", "--create", "fixture-source", "--quiet"])
            .current_dir(temporary.path())
            .status()
            .expect("attach fixture repository HEAD");
        assert!(status.success());
    }
    temporary
}

fn clean_clone() -> tempfile::TempDir {
    clean_clone_from(&root())
}

#[test]
fn packager_fixture_clone_has_an_attached_head_when_its_source_is_detached() {
    let detached_source = clean_clone();
    let source_ref = run_fixture_git(detached_source.path(), &["symbolic-ref", "-q", "HEAD"]);
    let source_branch = source_ref
        .strip_prefix("refs/heads/")
        .expect("fixture source branch");
    run_fixture_git(detached_source.path(), &["switch", "--detach", "--quiet"]);
    run_fixture_git(detached_source.path(), &["branch", "-D", source_branch]);

    let clone = clean_clone_from(detached_source.path());
    let symbolic_head = Command::new("git")
        .args(["symbolic-ref", "-q", "HEAD"])
        .current_dir(clone.path())
        .output()
        .expect("inspect cloned fixture HEAD");
    assert!(
        symbolic_head.status.success(),
        "release fixtures must start from an attached branch even when CI is detached"
    );
}

#[test]
fn packager_rejects_unreachable_source_version_mismatch_and_mislabeled_inputs() {
    let unreachable = clean_clone();
    let output = package_release_fixture_command()
        .arg("0.1.0-rc.1")
        .current_dir(unreachable.path())
        .env("PLANERADAR_SOURCE_REF", "f".repeat(40))
        .env("PLANERADAR_PACKAGE_SKIP_BUILDS", "1")
        .output()
        .expect("unreachable source fixture");
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("reachable commit"));

    let mismatched = clean_clone();
    let output = package_release_fixture_command()
        .arg("9.9.9")
        .current_dir(mismatched.path())
        .env("PLANERADAR_PACKAGE_SKIP_BUILDS", "1")
        .output()
        .expect("version mismatch fixture");
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("Cargo.toml"));

    let mislabeled = clean_clone();
    fs::create_dir(mislabeled.path().join("dist")).expect("ignored dist");
    let bin = mislabeled.path().join("dist/fixture-bin");
    fs::create_dir(&bin).expect("fixture command directory");
    write_executable(&bin.join("lipo"), "#!/bin/sh\nexit 1\n");
    let fake = mislabeled.path().join("dist/fake-aarch64");
    write_executable(&fake, "#!/bin/sh\nexit 0\n");
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let output = package_release_fixture_command()
        .arg("0.1.0-rc.1")
        .current_dir(mislabeled.path())
        .env("PATH", path)
        .env("PLANERADAR_PACKAGE_SKIP_BUILDS", "1")
        .env("PLANERADAR_APP_BINARY", &fake)
        .env("PLANERADAR_CTL_ARM64_BINARY", &fake)
        .env("PLANERADAR_CTL_X86_64_BINARY", &fake)
        .output()
        .expect("mislabeled input fixture");
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("ELF 64-bit LSB"));
}

#[test]
fn packager_rejects_an_ancestor_that_is_not_the_checked_out_source() {
    let checked_out = clean_clone();
    let output = package_release_fixture_command()
        .arg("0.1.0-rc.1")
        .current_dir(checked_out.path())
        .env("PLANERADAR_SOURCE_REF", "HEAD^")
        .env("PLANERADAR_PACKAGE_SKIP_BUILDS", "1")
        .output()
        .expect("mismatched selected source fixture");
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("selected source does not match current checkout"),
        "unexpected error: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[derive(Clone, Copy)]
enum ProvenanceIdentity {
    Default,
    ExplicitCurrent,
    ExplicitMismatchedRef,
}

struct PackagerProvenanceOutcome {
    success: bool,
    stderr: String,
    head: String,
    manifest: Option<Value>,
}

fn run_fixture_git(directory: &Path, arguments: &[&str]) -> String {
    let output = Command::new("git")
        .args(arguments)
        .current_dir(directory)
        .output()
        .expect("run fixture git");
    assert!(
        output.status.success(),
        "fixture git {:?} failed: {}",
        arguments,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

fn packager_provenance_fixture(
    branch: &str,
    detached: bool,
    identity: ProvenanceIdentity,
) -> PackagerProvenanceOutcome {
    let temporary = clean_clone();
    let directory = temporary.path();
    let current_ref = run_fixture_git(directory, &["symbolic-ref", "-q", "HEAD"]);
    let wanted_ref = format!("refs/heads/{branch}");
    if current_ref != wanted_ref {
        run_fixture_git(directory, &["branch", "-M", branch]);
    }

    write_executable(
        &directory.join("scripts/validate-release-metadata.sh"),
        "#!/bin/sh\nexit 0\n",
    );
    run_fixture_git(directory, &["add", "scripts/validate-release-metadata.sh"]);
    run_fixture_git(
        directory,
        &[
            "-c",
            "user.name=Plane Radar Test",
            "-c",
            "user.email=planeradar-test@example.invalid",
            "commit",
            "--quiet",
            "-m",
            "test: prepare provenance fixture",
        ],
    );
    let head = run_fixture_git(directory, &["rev-parse", "HEAD"]);
    if matches!(identity, ProvenanceIdentity::ExplicitMismatchedRef) {
        run_fixture_git(directory, &["branch", "mismatched-provenance", "HEAD^"]);
    }
    if detached && matches!(identity, ProvenanceIdentity::ExplicitCurrent) {
        let remote_ref = format!("refs/remotes/origin/{branch}");
        run_fixture_git(directory, &["update-ref", &remote_ref, &head]);
    }
    if detached {
        run_fixture_git(directory, &["switch", "--detach", "--quiet"]);
    }
    if detached && matches!(identity, ProvenanceIdentity::ExplicitCurrent) {
        run_fixture_git(directory, &["update-ref", "-d", &wanted_ref]);
    }

    let dist = directory.join("dist");
    let bin = dist.join("fixture-bin");
    fs::create_dir_all(&bin).expect("fixture command directory");
    let app = dist.join("app-input");
    let control_arm64 = dist.join("control-arm64-input");
    let control_x86_64 = dist.join("control-x86_64-input");
    write_executable(&app, "#!/bin/sh\nexit 0\n");
    for (input, cpu_type) in [
        (&control_arm64, 0x0100_000c_u32),
        (&control_x86_64, 0x0100_0007_u32),
    ] {
        let mut bytes = Vec::new();
        for value in [0xfeed_facf, cpu_type, 0, 2, 1, 72, 0, 0] {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        bytes.extend_from_slice(&0x19_u32.to_le_bytes());
        bytes.extend_from_slice(&72_u32.to_le_bytes());
        bytes.extend_from_slice(&[0; 16]);
        bytes.extend_from_slice(&0_u64.to_le_bytes());
        bytes.extend_from_slice(&104_u64.to_le_bytes());
        bytes.extend_from_slice(&0_u64.to_le_bytes());
        bytes.extend_from_slice(&104_u64.to_le_bytes());
        bytes.extend_from_slice(&5_u32.to_le_bytes());
        bytes.extend_from_slice(&5_u32.to_le_bytes());
        bytes.extend_from_slice(&0_u32.to_le_bytes());
        bytes.extend_from_slice(&0_u32.to_le_bytes());
        assert_eq!(bytes.len(), 104);
        fs::write(input, bytes).expect("thin Mach-O input fixture");
        fs::set_permissions(input, fs::Permissions::from_mode(0o755))
            .expect("executable Mach-O input fixture");
    }
    write_executable(
        &bin.join("file"),
        r#"#!/bin/sh
case "$*" in
  *app-input*) echo 'ELF 64-bit LSB executable, ARM aarch64' ;;
  *control-arm64-input*) echo 'Mach-O 64-bit executable arm64' ;;
  *control-x86_64-input*) echo 'Mach-O 64-bit executable x86_64' ;;
  *) exit 1 ;;
esac
"#,
    );
    write_executable(
        &bin.join("docker"),
        r#"#!/bin/bash
set -euo pipefail
input=
output=
arguments=("$@")
for ((index = 0; index < ${#arguments[@]}; index++)); do
  if [[ "${arguments[index]}" == "--volume" ]]; then
    volume="${arguments[index + 1]}"
    case "$volume" in
      *:/input:ro) input="${volume%:/input:ro}" ;;
      *:/output) output="${volume%:/output}" ;;
    esac
  fi
done
member="${arguments[${#arguments[@]} - 2]}"
archive="${arguments[${#arguments[@]} - 1]}"
tar -cf - -C "$input" "$member" |
  zstd -19 -T1 --no-progress -o "$output/$archive"
"#,
    );

    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let mut command = package_release_fixture_command();
    command
        .arg("0.1.0-rc.1")
        .current_dir(directory)
        .env("PATH", path)
        .env("PLANERADAR_PACKAGE_SKIP_BUILDS", "1")
        .env("PLANERADAR_APP_BINARY", &app)
        .env("PLANERADAR_CTL_ARM64_BINARY", &control_arm64)
        .env("PLANERADAR_CTL_X86_64_BINARY", &control_x86_64);
    match identity {
        ProvenanceIdentity::Default => {}
        ProvenanceIdentity::ExplicitCurrent => {
            command
                .env("PLANERADAR_WORKFLOW_REF", &wanted_ref)
                .env("PLANERADAR_WORKFLOW_COMMIT", &head);
        }
        ProvenanceIdentity::ExplicitMismatchedRef => {
            command
                .env(
                    "PLANERADAR_WORKFLOW_REF",
                    "refs/heads/mismatched-provenance",
                )
                .env("PLANERADAR_WORKFLOW_COMMIT", &head);
        }
    }
    let output = command.output().expect("run provenance package fixture");
    let manifest_path = directory.join("dist/release/release-manifest.json");
    let manifest = manifest_path.exists().then(|| {
        serde_json::from_slice(&fs::read(manifest_path).expect("provenance manifest"))
            .expect("provenance manifest JSON")
    });
    PackagerProvenanceOutcome {
        success: output.status.success(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        head,
        manifest,
    }
}

fn assert_packaged_workflow_identity(outcome: &PackagerProvenanceOutcome, expected_ref: &str) {
    assert!(
        outcome.success,
        "package fixture failed: {}",
        outcome.stderr
    );
    let manifest = outcome.manifest.as_ref().expect("packaged manifest");
    assert_eq!(manifest["source_commit"], outcome.head);
    assert_eq!(manifest["workflow"]["ref"], expected_ref);
    assert_eq!(manifest["workflow"]["commit"], outcome.head);
}

#[test]
fn packager_defaults_a_normal_clone_to_its_attainable_symbolic_head() {
    let outcome = packager_provenance_fixture("main", false, ProvenanceIdentity::Default);
    assert_packaged_workflow_identity(&outcome, "refs/heads/main");
}

#[test]
fn packager_defaults_a_non_main_clone_to_its_attainable_symbolic_head() {
    let outcome =
        packager_provenance_fixture("release-candidate", false, ProvenanceIdentity::Default);
    assert_packaged_workflow_identity(&outcome, "refs/heads/release-candidate");
}

#[test]
fn packager_accepts_an_attainable_gitbutler_workspace_identity() {
    let outcome =
        packager_provenance_fixture("gitbutler/workspace", false, ProvenanceIdentity::Default);
    assert_packaged_workflow_identity(&outcome, "refs/heads/gitbutler/workspace");
}

#[test]
fn packager_requires_explicit_attainable_identity_for_detached_head() {
    let outcome = packager_provenance_fixture("main", true, ProvenanceIdentity::Default);
    assert!(!outcome.success);
    assert!(outcome.manifest.is_none());
    assert!(
        outcome.stderr.contains(
            "detached HEAD requires PLANERADAR_WORKFLOW_REF and PLANERADAR_WORKFLOW_COMMIT"
        ),
        "unexpected detached-HEAD error: {}",
        outcome.stderr
    );
}

#[test]
fn packager_supports_public_workflow_identity_from_detached_checkout() {
    let outcome = packager_provenance_fixture("main", true, ProvenanceIdentity::ExplicitCurrent);
    assert_packaged_workflow_identity(&outcome, "refs/heads/main");
}

#[test]
fn packager_rejects_a_workflow_ref_that_does_not_resolve_to_its_commit() {
    let outcome =
        packager_provenance_fixture("main", false, ProvenanceIdentity::ExplicitMismatchedRef);
    assert!(!outcome.success);
    assert!(outcome.manifest.is_none());
    assert!(
        outcome
            .stderr
            .contains("workflow ref does not resolve to workflow commit"),
        "unexpected mismatched workflow identity error: {}",
        outcome.stderr
    );
}

#[test]
fn bootstrap_rejects_hostile_input_and_missing_prerequisites_before_mutation() {
    let script = root().join("scripts/install.sh");
    let hostile = Command::new("/bin/bash")
        .arg(&script)
        .arg("pi@radar.local;touch /tmp/owned")
        .output()
        .expect("run hostile target fixture");
    assert!(!hostile.status.success());
    assert!(String::from_utf8_lossy(&hostile.stderr).contains("safe OpenSSH"));

    let temporary = tempfile::tempdir().expect("temporary");
    write_executable(
        &temporary.path().join("uname"),
        "#!/bin/sh\nif test \"$1\" = -s; then echo Linux; else echo arm64; fi\n",
    );
    let unsupported = Command::new("/bin/bash")
        .arg(&script)
        .arg("pi@radar.local")
        .env(
            "PATH",
            format!("{}:/usr/bin:/bin", temporary.path().display()),
        )
        .output()
        .expect("run unsupported host fixture");
    assert!(!unsupported.status.success());
    assert!(String::from_utf8_lossy(&unsupported.stderr).contains("requires macOS"));

    write_executable(
        &temporary.path().join("uname"),
        "#!/bin/sh\nif test \"$1\" = -s; then echo Darwin; else echo arm64; fi\n",
    );
    let missing_gh = Command::new("/bin/bash")
        .arg(&script)
        .arg("pi@radar.local")
        .env(
            "PATH",
            format!("{}:/usr/bin:/bin", temporary.path().display()),
        )
        .output()
        .expect("run missing gh fixture");
    assert!(!missing_gh.status.success());
    assert!(String::from_utf8_lossy(&missing_gh.stderr).contains("required command"));
}

#[derive(Clone, Copy)]
enum BootstrapScenario {
    Success,
    AcceptedDeclaredBoundaries,
    ControlFailure,
    InsufficientTemporarySpace,
    OversizedInstallerMetadata,
    OversizedManifestMetadata,
    OversizedChecksumsMetadata,
    OversizedSbomMetadata,
    SignalHup,
    SignalInt,
    SignalTerm,
    ControlSignalHup,
    ControlSignalInt,
    ControlSignalTerm,
    ControlStartupSignalHup,
    ControlStartupSignalInt,
    ControlStartupSignalTerm,
    ControlBarrierPsFailure,
    ControlBarrierParentMismatch,
    ControlBarrierGroupMismatch,
    ControlBarrierStateMismatch,
    ControlBarrierChildExit,
    ControlSignalWithUnrelatedProcess,
    PtySuccess,
    PtySlowSuccess,
    PtyForegroundHandoff,
    PtyMalformedCompletion,
    PtyControlFailure,
    PtyCompletionDescendantSuccess,
    PtyCompletionDescendantFailure,
    PtyCompletionDescendantSignal,
    PtyControlSignalHup,
    PtyControlSignalInt,
    PtyControlSignalTerm,
    PtyTerminalInterrupt,
    PtyBarrierFailure,
    PtyPostReapSignalHup,
    PtyPostReapSignalInt,
    PtyPostReapSignalTerm,
    PtyPostClearSignalTerm,
    PtyTransientCompletionSnapshotFailure,
    PtyPermanentCompletionSnapshotFailure,
    RepeatedSignalHupThenTerm,
    ExtraArchiveMember,
    DuplicateArchiveMember,
    SymlinkArchiveMember,
    SpecialArchiveMember,
    OversizedArchiveMember,
    OversizedCompressedArchive,
    DecompressionBomb,
    TruncatedArchive,
    FailedAttestation,
    WrongWorkflowAttestation,
    FailedReleaseVerification,
    WrongReleaseVerificationTag,
    WrongReleaseVerificationRepository,
    MutableTag,
    MismatchedTag,
    BadManifest,
    BadChecksum,
    WrongArchitecture,
    IncompleteDownload,
    FailedReleaseLookup,
}

#[derive(Debug)]
struct BootstrapBoundarySizes {
    installer: u64,
    manifest: u64,
    checksums: u64,
    sbom: u64,
    control_archive: u64,
    expanded_archive: u64,
    control_member: u64,
}

struct BootstrapOutcome {
    success: bool,
    status_code: Option<i32>,
    stdout: String,
    argv: String,
    attestations: String,
    release_verification: String,
    downloads: String,
    metadata_parses: String,
    private_residue: Vec<String>,
    stderr: String,
    boundary_sizes: Option<BootstrapBoundarySizes>,
    control_completion: bool,
    control_processes_recorded: Vec<u32>,
    control_processes_alive: Vec<u32>,
    cancellation_latency: Option<Duration>,
    unrelated_process_survived: Option<bool>,
    original_terminal_pgid: Option<u32>,
    restored_terminal_pgid: Option<u32>,
    post_reap_restore_marker_survived: bool,
    stale_control_action_recorded: bool,
    completion_group_was_stopped: bool,
    initial_continue_was_intercepted: bool,
    initial_stopped_state_was_observed: bool,
    initial_continue_followed_observation: bool,
    initial_continue_handshake_timed_out: bool,
    terminal_handoff_observed: bool,
    control_terminal_trace: String,
    completion_snapshot_fault_was_injected: bool,
}

fn open_release_pty() -> (File, File) {
    let mut master: RawFd = -1;
    let mut slave: RawFd = -1;
    // SAFETY: openpty initializes both descriptors on success; ownership is
    // transferred immediately to File.
    let result = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    assert_eq!(
        result,
        0,
        "release PTY creation failed: {}",
        std::io::Error::last_os_error()
    );
    // SAFETY: successful openpty returned two distinct owned descriptors.
    unsafe { (File::from_raw_fd(master), File::from_raw_fd(slave)) }
}

fn read_release_pty(master: &mut File) -> Vec<u8> {
    // SAFETY: fcntl operates on this live PTY master descriptor.
    let flags = unsafe { libc::fcntl(master.as_raw_fd(), libc::F_GETFL) };
    assert!(flags >= 0, "read release PTY flags");
    // SAFETY: the descriptor and flags were validated above.
    assert_eq!(
        unsafe { libc::fcntl(master.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) },
        0,
        "make release PTY nonblocking"
    );
    let mut output = Vec::new();
    let _ = master.read_to_end(&mut output);
    output
}

fn recorded_pid(path: &Path) -> Option<u32> {
    fs::read_to_string(path).ok()?.trim().parse().ok()
}

fn kill_recorded_group(path: &Path) {
    if let Some(pid) = recorded_pid(path) {
        let _ = Command::new("/bin/kill")
            .args(["-KILL", &format!("-{pid}")])
            .status();
    }
}

fn stop_pty_wrapper(process: &mut Child, installer_pid_record: &Path, control_pid_record: &Path) {
    kill_recorded_group(control_pid_record);
    if let Some(installer_pid) = recorded_pid(installer_pid_record) {
        let _ = Command::new("/bin/kill")
            .args(["-KILL", &installer_pid.to_string()])
            .status();
    }
    let _ = process.kill();
    let deadline = Instant::now() + Duration::from_secs(2);
    while process.try_wait().ok().flatten().is_none() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
}

fn bootstrap_fixture_outcome(scenario: BootstrapScenario) -> BootstrapOutcome {
    let temporary = tempfile::tempdir().expect("temporary bootstrap fixture");
    let fixture = temporary.path().join("release");
    let bin = temporary.path().join("bin");
    let payload = temporary.path().join("payload");
    let private_root = temporary.path().join("private");
    fs::create_dir_all(&fixture).expect("release fixture");
    fs::create_dir_all(&bin).expect("fixture bin");
    fs::create_dir_all(&payload).expect("payload fixture");
    fs::create_dir_all(&private_root).expect("private fixture root");
    let argv_record = temporary.path().join("argv");
    let attestation_record = temporary.path().join("attestations");
    let release_verification_record = temporary.path().join("release-verification");
    let download_record = temporary.path().join("downloads");
    let metadata_parse_record = temporary.path().join("metadata-parses");
    let control_ready_record = temporary.path().join("control-ready");
    let control_completion_record = temporary.path().join("control-completion");
    let control_pid_record = temporary.path().join("control-pid");
    let control_descendant_pid_record = temporary.path().join("control-descendant-pid");
    let control_grandchild_pid_record = temporary.path().join("control-grandchild-pid");
    let startup_debug_env = temporary.path().join("startup-debug-env");
    let startup_signal_record = temporary.path().join("startup-signal");
    let barrier_entry_record = temporary.path().join("barrier-entry");
    let installer_pid_record = temporary.path().join("installer-pid");
    let pty_result_record = temporary.path().join("pty-result");
    let inner_bash_env = temporary.path().join("inner-bash-env");
    let post_reap_signal_record = temporary.path().join("post-reap-signal");
    let stale_control_action_record = temporary.path().join("stale-control-action");
    let completion_group_stopped_record = temporary.path().join("completion-group-stopped");
    let initial_continue_intercept_record = temporary.path().join("initial-continue-intercept");
    let initial_stopped_observation_record = temporary.path().join("initial-stopped-observation");
    let initial_continue_ack_record = temporary.path().join("initial-continue-ack");
    let initial_continue_timeout_record = temporary.path().join("initial-continue-timeout");
    let terminal_handoff_record = temporary.path().join("terminal-handoff");
    let control_terminal_trace_record = temporary.path().join("control-terminal-trace");
    let completion_published_record = temporary.path().join("completion-published");
    let completion_snapshot_fault_record = temporary.path().join("completion-snapshot-fault");
    let pty_scenario = matches!(
        scenario,
        BootstrapScenario::PtySuccess
            | BootstrapScenario::PtySlowSuccess
            | BootstrapScenario::PtyForegroundHandoff
            | BootstrapScenario::PtyMalformedCompletion
            | BootstrapScenario::PtyControlFailure
            | BootstrapScenario::PtyCompletionDescendantSuccess
            | BootstrapScenario::PtyCompletionDescendantFailure
            | BootstrapScenario::PtyCompletionDescendantSignal
            | BootstrapScenario::PtyControlSignalHup
            | BootstrapScenario::PtyControlSignalInt
            | BootstrapScenario::PtyControlSignalTerm
            | BootstrapScenario::PtyTerminalInterrupt
            | BootstrapScenario::PtyBarrierFailure
            | BootstrapScenario::PtyPostReapSignalHup
            | BootstrapScenario::PtyPostReapSignalInt
            | BootstrapScenario::PtyPostReapSignalTerm
            | BootstrapScenario::PtyPostClearSignalTerm
            | BootstrapScenario::PtyTransientCompletionSnapshotFailure
            | BootstrapScenario::PtyPermanentCompletionSnapshotFailure
    );

    let control_mode = if matches!(
        scenario,
        BootstrapScenario::ControlStartupSignalHup
            | BootstrapScenario::ControlStartupSignalInt
            | BootstrapScenario::ControlStartupSignalTerm
            | BootstrapScenario::ControlBarrierPsFailure
            | BootstrapScenario::ControlBarrierParentMismatch
            | BootstrapScenario::ControlBarrierGroupMismatch
            | BootstrapScenario::ControlBarrierStateMismatch
            | BootstrapScenario::PtyBarrierFailure
    ) {
        "startup"
    } else if matches!(scenario, BootstrapScenario::ControlBarrierChildExit) {
        "barrier-exit"
    } else if matches!(
        scenario,
        BootstrapScenario::ControlSignalHup
            | BootstrapScenario::ControlSignalInt
            | BootstrapScenario::ControlSignalTerm
            | BootstrapScenario::ControlSignalWithUnrelatedProcess
            | BootstrapScenario::PtyControlSignalHup
            | BootstrapScenario::PtyControlSignalInt
            | BootstrapScenario::PtyControlSignalTerm
            | BootstrapScenario::PtyTerminalInterrupt
    ) {
        "active"
    } else if matches!(
        scenario,
        BootstrapScenario::PtyCompletionDescendantSuccess
            | BootstrapScenario::PtyCompletionDescendantFailure
            | BootstrapScenario::PtyCompletionDescendantSignal
    ) {
        "completion-descendant"
    } else {
        "normal"
    };
    let control_program = format!(
        r#"#!/usr/bin/env python3
import errno
import os
import signal
import subprocess
import sys
import time

mode = "{control_mode}"
arguments = sys.argv[1:]
saved_terminal = None

def block_sigttou():
    signal.pthread_sigmask(signal.SIG_BLOCK, {{signal.SIGTTOU}})

def restore_terminal():
    if saved_terminal is None:
        return
    original_pgid, control_pgid = saved_terminal
    block_sigttou()
    foreground = os.tcgetpgrp(0)
    if foreground == original_pgid:
        return
    if foreground != control_pgid:
        raise RuntimeError("unrelated terminal foreground process group")
    os.tcsetpgrp(0, original_pgid)

def relay_terminal_signal(received, _frame):
    os.kill(os.getppid(), received)

if arguments and arguments[0] == "--__planeradar-restore-tty-v1":
    marker = arguments[1]
    with open(marker, "r", encoding="utf-8") as ready:
        fields = ready.readline().strip().split(" ")
    if fields == ["ready", "none"]:
        sys.exit(0)
    if len(fields) != 4 or fields[:2] != ["ready", "tty"]:
        sys.exit(74)
    saved_terminal = (int(fields[2]), int(fields[3]))
    if os.getpgid(os.getppid()) != saved_terminal[0] or os.getpgrp() != saved_terminal[0]:
        sys.exit(75)
    restore_terminal()
    sys.exit(0)

if arguments and arguments[0] == "--__planeradar-foreground-tty-v1":
    marker = arguments[1]
    with open(marker, "r", encoding="utf-8") as ready:
        fields = ready.readline().strip().split(" ")
    if fields == ["ready", "none"]:
        with open(os.environ["PLANERADAR_TERMINAL_HANDOFF_RECORD"], "w", encoding="utf-8") as record:
            record.write("none\n")
        sys.exit(0)
    if len(fields) != 4 or fields[:2] != ["ready", "tty"]:
        sys.exit(74)
    saved_terminal = (int(fields[2]), int(fields[3]))
    if os.getpgid(os.getppid()) != saved_terminal[0] or os.getpgrp() != saved_terminal[0]:
        sys.exit(75)
    block_sigttou()
    foreground = os.tcgetpgrp(0)
    if foreground not in saved_terminal:
        sys.exit(76)
    if foreground != saved_terminal[1]:
        os.tcsetpgrp(0, saved_terminal[1])
    if os.tcgetpgrp(0) != saved_terminal[1]:
        sys.exit(77)
    with open(os.environ["PLANERADAR_TERMINAL_HANDOFF_RECORD"], "w", encoding="utf-8") as record:
        record.write("foreground\n")
    if os.environ.get("PLANERADAR_FORCE_RECLAIM_AFTER_PARENT_HANDOFF", "0") == "1":
        os.tcsetpgrp(0, saved_terminal[0])
        with open(os.environ["PLANERADAR_CONTROL_TERMINAL_TRACE_RECORD"], "a", encoding="utf-8") as record:
            record.write(f"reclaimer {{os.getpgrp()}} {{os.tcgetpgrp(0)}}\n")
    sys.exit(0)

if arguments and arguments[0] == "--__planeradar-bootstrap-v1":
    with open(os.environ["PLANERADAR_BARRIER_ENTRY_RECORD"], "w", encoding="utf-8") as record:
        record.write("entered\n")
    if mode == "barrier-exit":
        sys.exit(73)
    marker = arguments[1]
    continue_marker = arguments[2]
    arguments = arguments[3:]
    try:
        original_pgid = os.tcgetpgrp(0)
    except OSError as error:
        if error.errno != errno.ENOTTY:
            raise
        original_pgid = None
    if original_pgid is not None:
        parent_pgid = os.getpgid(os.getppid())
        if original_pgid != parent_pgid or os.getpgrp() != parent_pgid:
            block_sigttou()
            sys.exit(76)
    os.setpgid(0, 0)
    control_pgid = os.getpgrp()
    if original_pgid is not None:
        saved_terminal = (original_pgid, control_pgid)
    ready = open(marker, "w+", encoding="utf-8")
    if saved_terminal is None:
        ready.write("ready none\n")
    else:
        ready.write(f"ready tty {{saved_terminal[0]}} {{saved_terminal[1]}}\n")
    ready.flush()
    os.fsync(ready.fileno())
    os.kill(os.getpid(), signal.SIGSTOP)
    continue_deadline = time.monotonic() + 3
    while True:
        try:
            with open(continue_marker, "rb") as acknowledgement:
                continue_contents = acknowledgement.read(10)
        except OSError:
            continue_contents = b""
        if continue_contents == b"continue\n":
            break
        if not b"continue\n".startswith(continue_contents):
            sys.exit(78)
        if time.monotonic() >= continue_deadline:
            sys.exit(78)
        time.sleep(0.005)
    if (
        os.environ.get("PLANERADAR_REQUIRE_TERMINAL_HANDOFF", "0") == "1"
        and not os.path.exists(os.environ["PLANERADAR_TERMINAL_HANDOFF_RECORD"])
    ):
        sys.exit(78)
    if saved_terminal is not None:
        block_sigttou()
        foreground = os.tcgetpgrp(0)
        if foreground not in saved_terminal:
            sys.exit(77)
        if foreground != saved_terminal[1]:
            os.tcsetpgrp(0, saved_terminal[1])
        if os.tcgetpgrp(0) != saved_terminal[1]:
            sys.exit(77)
        with open(os.environ["PLANERADAR_CONTROL_TERMINAL_TRACE_RECORD"], "a", encoding="utf-8") as record:
            record.write(f"supervisor {{os.getpgrp()}} {{os.tcgetpgrp(0)}}\n")
        if os.environ.get("PLANERADAR_CONTROL_DEFAULT_SIGINT", "0") == "1":
            signal.signal(signal.SIGHUP, relay_terminal_signal)
            signal.signal(signal.SIGINT, relay_terminal_signal)
            signal.signal(signal.SIGTERM, relay_terminal_signal)
    worker_environment = os.environ.copy()
    worker_environment["PLANERADAR_FIXTURE_CONTROL_WORKER"] = "1"
    worker = subprocess.Popen(
        [sys.executable, sys.argv[0], *arguments],
        env=worker_environment,
    )
    worker_status = worker.wait()
    if worker_status < 0:
        worker_status = 128 - worker_status
    restore_terminal()
    completion = (
        os.environ.get("PLANERADAR_CONTROL_COMPLETION_LINE")
        or f"complete {{worker_status}}"
    )
    ready.write(f"{{completion}}\n")
    ready.flush()
    os.fsync(ready.fileno())
    with open(os.environ["PLANERADAR_COMPLETION_PUBLISHED_RECORD"], "w", encoding="utf-8") as record:
        record.write("published\n")
    os.killpg(os.getpgrp(), signal.SIGSTOP)
    os._exit(worker_status)

try:
    if mode == "startup":
        time.sleep(20)
        with open(os.environ["PLANERADAR_CONTROL_COMPLETION_RECORD"], "w", encoding="utf-8") as record:
            record.write("delayed startup mutation\n")
        sys.exit(0)

    try:
        worker_foreground_pgid = os.tcgetpgrp(0)
    except OSError:
        worker_foreground_pgid = -1
    with open(os.environ["PLANERADAR_CONTROL_TERMINAL_TRACE_RECORD"], "a", encoding="utf-8") as record:
        record.write(f"worker {{os.getpgrp()}} {{worker_foreground_pgid}}\n")
    control_input = sys.stdin.readline().rstrip("\n")
    print(f"control stdin={{control_input}}", flush=True)
    print("control stdout", flush=True)
    print("control stderr", file=sys.stderr, flush=True)
    time.sleep(float(os.environ.get("PLANERADAR_CONTROL_DELAY_SECONDS", "0")))
    with open(os.environ["PLANERADAR_ARGV_RECORD"], "w", encoding="utf-8") as record:
        record.write("\n".join(arguments) + "\n")

    if mode == "active":
        with open(os.environ["PLANERADAR_CONTROL_PID_RECORD"], "w", encoding="utf-8") as record:
            record.write(f"{{os.getpid()}}\n")
        descendant = subprocess.Popen(
            ["/bin/sh", "-c", r'''
/bin/sleep 20 &
grandchild=$!
printf '%s\n' "$grandchild" >"$PLANERADAR_CONTROL_GRANDCHILD_PID_RECORD"
wait "$grandchild"
printf 'descendant mutation\n' >"$PLANERADAR_CONTROL_COMPLETION_RECORD"
''']
        )
        with open(os.environ["PLANERADAR_CONTROL_DESCENDANT_PID_RECORD"], "w", encoding="utf-8") as record:
            record.write(f"{{descendant.pid}}\n")
        while not os.path.exists(os.environ["PLANERADAR_CONTROL_GRANDCHILD_PID_RECORD"]):
            time.sleep(0.005)
        with open(os.environ["PLANERADAR_CONTROL_READY_RECORD"], "w", encoding="utf-8") as record:
            record.write("ready\n")
        descendant.wait()
        with open(os.environ["PLANERADAR_CONTROL_COMPLETION_RECORD"], "a", encoding="utf-8") as record:
            record.write("control mutation\n")
        sys.exit(0)

    if mode == "completion-descendant":
        with open(os.environ["PLANERADAR_CONTROL_PID_RECORD"], "w", encoding="utf-8") as record:
            record.write(f"{{os.getpid()}}\n")
        descendant_program = r'''
import os
import signal
import subprocess
import sys
for received in (signal.SIGHUP, signal.SIGINT, signal.SIGTERM):
    signal.signal(received, signal.SIG_IGN)
grandchild_program = r"""
import os
import signal
import time
for received in (signal.SIGHUP, signal.SIGINT, signal.SIGTERM):
    signal.signal(received, signal.SIG_IGN)
time.sleep(20)
with open(os.environ["PLANERADAR_CONTROL_COMPLETION_RECORD"], "w", encoding="utf-8") as record:
    record.write("descendant mutation\n")
"""
grandchild = subprocess.Popen([sys.executable, "-c", grandchild_program])
with open(os.environ["PLANERADAR_CONTROL_GRANDCHILD_PID_RECORD"], "w", encoding="utf-8") as record:
    record.write(f"{{grandchild.pid}}\n")
grandchild.wait()
'''
        descendant = subprocess.Popen(
            [sys.executable, "-c", descendant_program]
        )
        with open(os.environ["PLANERADAR_CONTROL_DESCENDANT_PID_RECORD"], "w", encoding="utf-8") as record:
            record.write(f"{{descendant.pid}}\n")
        while not os.path.exists(os.environ["PLANERADAR_CONTROL_GRANDCHILD_PID_RECORD"]):
            time.sleep(0.005)
        if os.environ.get("PLANERADAR_CONTROL_WORKER_SIGNAL") == "TERM":
            os.kill(os.getpid(), signal.SIGTERM)
        sys.exit(int(os.environ.get("PLANERADAR_CONTROL_EXIT_STATUS", "0")))

    sys.exit(int(os.environ.get("PLANERADAR_CONTROL_EXIT_STATUS", "0")))
finally:
    if os.environ.get("PLANERADAR_FIXTURE_CONTROL_WORKER", "0") != "1":
        restore_terminal()
"#
    )
    .into_bytes();
    if matches!(scenario, BootstrapScenario::AcceptedDeclaredBoundaries) {
        let target_size = 16 * 1024 * 1024;
        let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
        let mut state = 0x8f34_a2d1_u32;
        let mut contents = Vec::with_capacity(target_size);
        contents.extend_from_slice(&control_program);
        while contents.len() < target_size {
            contents.push(b'#');
            for _ in 0..78 {
                if contents.len() == target_size {
                    break;
                }
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                contents.push(alphabet[(state as usize) % alphabet.len()]);
            }
            if contents.len() < target_size {
                contents.push(b'\n');
            }
        }
        fs::write(payload.join("planeradarctl"), contents).expect("boundary control program");
        fs::set_permissions(
            payload.join("planeradarctl"),
            fs::Permissions::from_mode(0o755),
        )
        .expect("boundary control executable");
    } else {
        fs::write(payload.join("planeradarctl"), control_program).expect("control fixture");
        fs::set_permissions(
            payload.join("planeradarctl"),
            fs::Permissions::from_mode(0o755),
        )
        .expect("control executable");
    }
    if matches!(scenario, BootstrapScenario::ExtraArchiveMember) {
        fs::write(payload.join("extra"), b"hostile").expect("extra member");
    }
    if matches!(scenario, BootstrapScenario::SymlinkArchiveMember) {
        fs::remove_file(payload.join("planeradarctl")).expect("replace control with symlink");
        symlink("target", payload.join("planeradarctl")).expect("symlink member");
    }
    if matches!(scenario, BootstrapScenario::SpecialArchiveMember) {
        fs::remove_file(payload.join("planeradarctl")).expect("replace control with fifo");
        assert!(
            Command::new("mkfifo")
                .arg(payload.join("planeradarctl"))
                .status()
                .expect("mkfifo")
                .success()
        );
    }
    if matches!(
        scenario,
        BootstrapScenario::OversizedArchiveMember | BootstrapScenario::DecompressionBomb
    ) {
        let oversized = fs::OpenOptions::new()
            .write(true)
            .open(payload.join("planeradarctl"))
            .expect("open oversized member");
        oversized
            .set_len(
                if matches!(scenario, BootstrapScenario::DecompressionBomb) {
                    40 * 1024 * 1024
                } else {
                    17 * 1024 * 1024
                },
            )
            .expect("size sparse member");
    }
    let archive = fixture.join("planeradarctl-aarch64-apple-darwin.tar.zst");
    let tar_program = if Path::new("/opt/homebrew/bin/gtar").exists() {
        "/opt/homebrew/bin/gtar"
    } else {
        "tar"
    };
    let mut archive_command = format!(
        "{tar_program} --sort=name --format=gnu --owner=0 --group=0 --numeric-owner --mode=0755 --mtime=@0 -cf - -C \"$PLANERADAR_PAYLOAD\" planeradarctl"
    );
    if matches!(scenario, BootstrapScenario::ExtraArchiveMember) {
        archive_command.push_str(" extra");
    }
    if matches!(scenario, BootstrapScenario::DuplicateArchiveMember) {
        archive_command.push_str(" planeradarctl");
    }
    archive_command.push_str(" | zstd -19 -T1 --no-progress -o \"$PLANERADAR_ARCHIVE\"");
    let status = Command::new("/bin/bash")
        .args(["-c", &archive_command])
        .env("PLANERADAR_PAYLOAD", &payload)
        .env("PLANERADAR_ARCHIVE", &archive)
        .status()
        .expect("create bootstrap archive");
    assert!(status.success());
    if matches!(scenario, BootstrapScenario::OversizedCompressedArchive) {
        let mut state = 0x6d2b_79f5_u32;
        let mut incompressible = vec![0_u8; 17 * 1024 * 1024];
        for byte in &mut incompressible {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            *byte = state as u8;
        }
        fs::OpenOptions::new()
            .append(true)
            .open(&archive)
            .expect("open archive for oversized compressed tail")
            .write_all(&incompressible)
            .expect("append oversized compressed tail");
    }
    if matches!(scenario, BootstrapScenario::TruncatedArchive) {
        let bytes = fs::read(&archive).expect("read archive for truncation");
        fs::write(&archive, &bytes[..bytes.len() / 2]).expect("truncate archive");
    }
    let expanded_archive_size = if matches!(scenario, BootstrapScenario::AcceptedDeclaredBoundaries)
    {
        let expanded = Command::new("zstd")
            .args(["-dc"])
            .arg(&archive)
            .output()
            .expect("expand boundary archive");
        assert!(expanded.status.success());
        expanded.stdout.len() as u64
    } else {
        0
    };

    fs::copy(
        root().join("scripts/install.sh"),
        fixture.join("install.sh"),
    )
    .expect("copy installer fixture");
    if matches!(scenario, BootstrapScenario::OversizedInstallerMetadata) {
        fs::OpenOptions::new()
            .write(true)
            .open(fixture.join("install.sh"))
            .expect("oversized installer fixture")
            .set_len(64 * 1024 + 1)
            .expect("resize installer fixture");
    }
    if matches!(scenario, BootstrapScenario::AcceptedDeclaredBoundaries) {
        let installer = fixture.join("install.sh");
        let current = fs::metadata(&installer).expect("installer metadata").len() as usize;
        assert!(current < 40 * 1024);
        fs::OpenOptions::new()
            .append(true)
            .open(installer)
            .expect("pad installer fixture")
            .write_all(&vec![b' '; 40 * 1024 - current])
            .expect("boundary installer fixture");
    }
    let archive_digest = sha256(&archive);
    let archive_size = fs::metadata(&archive).expect("archive metadata").len();
    let manifest_commit = if matches!(scenario, BootstrapScenario::BadManifest) {
        "cccccccccccccccccccccccccccccccccccccccc"
    } else {
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
    };
    fs::write(
        fixture.join("release-manifest.json"),
        format!(
            r#"{{"version":"0.1.0","source_commit":"{manifest_commit}","workflow":{{"path":".github/workflows/stable-draft.yml","ref":"refs/heads/main","commit":"{manifest_commit}"}},"artifacts":{{"planeradarctl-aarch64-apple-darwin.tar.zst":{{"kind":"control","platform":"apple-darwin","architecture":"aarch64","size":{archive_size},"sha256":"{archive_digest}"}}}}}}"#
        ),
    )
    .expect("manifest fixture");
    if matches!(scenario, BootstrapScenario::OversizedManifestMetadata) {
        fs::OpenOptions::new()
            .write(true)
            .open(fixture.join("release-manifest.json"))
            .expect("oversized manifest fixture")
            .set_len(64 * 1024 + 1)
            .expect("resize manifest fixture");
    }
    if matches!(scenario, BootstrapScenario::AcceptedDeclaredBoundaries) {
        let manifest = fixture.join("release-manifest.json");
        let current = fs::metadata(&manifest).expect("manifest metadata").len() as usize;
        fs::OpenOptions::new()
            .append(true)
            .open(manifest)
            .expect("pad manifest fixture")
            .write_all(&vec![b' '; 40 * 1024 - current])
            .expect("boundary manifest fixture");
    }
    fs::write(
        fixture.join("SBOM.spdx.json"),
        r#"{"spdxVersion":"SPDX-2.3"}"#,
    )
    .expect("SBOM fixture");
    if matches!(scenario, BootstrapScenario::OversizedSbomMetadata) {
        fs::OpenOptions::new()
            .write(true)
            .open(fixture.join("SBOM.spdx.json"))
            .expect("oversized SBOM fixture")
            .set_len(1024 * 1024 + 1)
            .expect("resize SBOM fixture");
    }
    if matches!(scenario, BootstrapScenario::AcceptedDeclaredBoundaries) {
        let sbom = fixture.join("SBOM.spdx.json");
        let current = fs::metadata(&sbom).expect("SBOM metadata").len() as usize;
        fs::OpenOptions::new()
            .append(true)
            .open(sbom)
            .expect("pad SBOM fixture")
            .write_all(&vec![b' '; 600 * 1024 - current])
            .expect("boundary SBOM fixture");
    }
    let subjects = [
        "planeradar-aarch64-linux-gnu.tar.zst",
        "planeradarctl-aarch64-apple-darwin.tar.zst",
        "planeradarctl-x86_64-apple-darwin.tar.zst",
        "install.sh",
        "release-manifest.json",
        "SBOM.spdx.json",
    ];
    let checksums = subjects
        .iter()
        .map(|name| {
            let path = fixture.join(name);
            let digest = if matches!(scenario, BootstrapScenario::BadChecksum)
                && *name == "planeradarctl-aarch64-apple-darwin.tar.zst"
            {
                "0".repeat(64)
            } else if path.exists() {
                sha256(&path)
            } else {
                "0".repeat(64)
            };
            format!("{digest}  {name}\n")
        })
        .collect::<String>();
    fs::write(fixture.join("SHA256SUMS"), checksums).expect("checksums fixture");
    if matches!(scenario, BootstrapScenario::OversizedChecksumsMetadata) {
        fs::OpenOptions::new()
            .write(true)
            .open(fixture.join("SHA256SUMS"))
            .expect("oversized checksums fixture")
            .set_len(16 * 1024 + 1)
            .expect("resize checksums fixture");
    }
    if matches!(scenario, BootstrapScenario::AcceptedDeclaredBoundaries) {
        let checksums = fixture.join("SHA256SUMS");
        let current = fs::metadata(&checksums).expect("checksums metadata").len() as usize;
        fs::OpenOptions::new()
            .append(true)
            .open(checksums)
            .expect("pad checksums fixture")
            .write_all(&vec![b' '; 10 * 1024 - current])
            .expect("boundary checksums fixture");
    }
    let boundary_sizes =
        matches!(scenario, BootstrapScenario::AcceptedDeclaredBoundaries).then(|| {
            BootstrapBoundarySizes {
                installer: fs::metadata(fixture.join("install.sh"))
                    .expect("boundary installer metadata")
                    .len(),
                manifest: fs::metadata(fixture.join("release-manifest.json"))
                    .expect("boundary manifest metadata")
                    .len(),
                checksums: fs::metadata(fixture.join("SHA256SUMS"))
                    .expect("boundary checksums metadata")
                    .len(),
                sbom: fs::metadata(fixture.join("SBOM.spdx.json"))
                    .expect("boundary SBOM metadata")
                    .len(),
                control_archive: fs::metadata(&archive)
                    .expect("boundary archive metadata")
                    .len(),
                expanded_archive: expanded_archive_size,
                control_member: fs::metadata(payload.join("planeradarctl"))
                    .expect("boundary control metadata")
                    .len(),
            }
        });

    write_executable(
        &bin.join("uname"),
        "#!/bin/sh\nif test \"$1\" = -s; then echo Darwin; else echo arm64; fi\n",
    );
    write_executable(
        &bin.join("lipo"),
        if matches!(scenario, BootstrapScenario::WrongArchitecture) {
            "#!/bin/sh\necho x86_64\n"
        } else {
            "#!/bin/sh\necho arm64\n"
        },
    );
    write_executable(
        &bin.join("stat"),
        "#!/bin/sh\ntest \"$1\" = -f\nshift 2\nwc -c <\"$1\" | tr -d ' '\n",
    );
    write_executable(
        &bin.join("ps"),
        r#"#!/bin/sh
case "$*" in
  *"ppid= -o pgid= -o state="*)
    if test -f "$PLANERADAR_COMPLETION_PUBLISHED_RECORD"; then
      case "${PLANERADAR_COMPLETION_PS_SCENARIO:-}" in
        transient)
          attempts=0
          if test -f "$PLANERADAR_COMPLETION_SNAPSHOT_FAULT_RECORD"; then
            attempts="$(cat "$PLANERADAR_COMPLETION_SNAPSHOT_FAULT_RECORD")"
          fi
          attempts=$((attempts + 1))
          printf '%s\n' "$attempts" >"$PLANERADAR_COMPLETION_SNAPSHOT_FAULT_RECORD"
          test "$attempts" -gt 3 || exit 71
          ;;
        permanent)
          printf 'permanent\n' >"$PLANERADAR_COMPLETION_SNAPSHOT_FAULT_RECORD"
          exit 71
          ;;
      esac
    fi
    case "${PLANERADAR_BARRIER_PS_SCENARIO:-}" in
      failure) exit 71 ;;
      parent)
        eval "pid=\${$#}"
        printf '%s %s T\n' "$((PPID + 1))" "$pid"
        exit 0
        ;;
      group)
        printf '%s 1 T\n' "$PPID"
        exit 0
        ;;
      state)
        eval "pid=\${$#}"
        printf '%s %s R\n' "$PPID" "$pid"
        exit 0
        ;;
    esac
    output="$(/bin/ps "$@")"
    status=$?
    if test "$status" -eq 0 &&
       test "${PLANERADAR_DELAY_INITIAL_CONTINUE:-0}" = 1 &&
       test -f "$PLANERADAR_INITIAL_CONTINUE_INTERCEPT_RECORD" &&
       test ! -f "$PLANERADAR_INITIAL_CONTINUE_ACK_RECORD" &&
       test ! -f "$PLANERADAR_INITIAL_CONTINUE_TIMEOUT_RECORD"; then
      set -- $output
      if test "$#" -eq 3; then
        case "$3" in
          T*) printf 'stopped\n' >"$PLANERADAR_INITIAL_STOPPED_OBSERVATION_RECORD" ;;
        esac
      fi
    fi
    printf '%s\n' "$output"
    exit "$status"
    ;;
esac
exec /bin/ps "$@"
"#,
    );
    write_executable(
        &bin.join("df"),
        r#"#!/bin/sh
if test "${PLANERADAR_INSUFFICIENT_TEMP_SPACE:-0}" = 1; then
  printf '%s\n' \
    'Filesystem 1024-blocks Used Available Capacity Mounted on' \
    'fixture 100 99 1 99% /fixture'
else
  exec /bin/df "$@"
fi
"#,
    );
    write_executable(
        &bin.join("awk"),
        r#"#!/bin/sh
case "$*" in
  *SHA256SUMS*|*release-manifest.json*|*SBOM.spdx.json*|*install.sh*)
    printf '%s\n' "$*" >>"$PLANERADAR_METADATA_PARSE_RECORD"
    ;;
esac
exec /usr/bin/awk "$@"
"#,
    );
    write_executable(
        &bin.join("rm"),
        r#"#!/bin/sh
if test "${PLANERADAR_REPEAT_SIGNAL_DURING_CLEANUP:-0}" = 1; then
  case "$*" in
    *planeradar-bootstrap.*)
      kill -TERM "$PPID"
      /bin/sleep 0.1
      kill -0 "$PPID" 2>/dev/null || exit 0
      ;;
  esac
fi
exec /bin/rm "$@"
"#,
    );
    write_executable(
        &bin.join("plutil"),
        r#"#!/usr/bin/env python3
import json
import os
import re
import sys
with open(os.environ["PLANERADAR_METADATA_PARSE_RECORD"], "a") as record:
    record.write("plutil " + " ".join(sys.argv[1:]) + "\n")
key = sys.argv[2]
with open(sys.argv[4], "rb") as source:
    value = json.load(source)
for component in re.split(r"(?<!\\)\.", key):
    value = value[component.replace(r"\.", ".")]
if isinstance(value, bool):
    print(str(value).lower())
else:
    print(value)
"#,
    );
    write_executable(
        &bin.join("gh"),
        r#"#!/bin/sh
set -eu
case "$1 $2" in
  "release view")
    test "${PLANERADAR_FAIL_RELEASE_LOOKUP:-0}" != 1
    printf '%s\n' "{\"tagName\":\"${PLANERADAR_RELEASE_TAG:-v0.1.0}\",\"isDraft\":false,\"isPrerelease\":false}"
    ;;
	  "release download")
	    name=
	    destination=
    while test "$#" -gt 0; do
      case "$1" in
        --pattern) name=$2; shift 2 ;;
        --dir) destination=$2; shift 2 ;;
        *) shift ;;
	      esac
	    done
	    printf '%s\n' "$name" >>"$PLANERADAR_DOWNLOAD_RECORD"
	    if test -n "${PLANERADAR_SIGNAL_ON_DOWNLOAD:-}"; then
	      candidate=$PPID
	      installer_pid=
	      while test "$candidate" -gt 1; do
	        command_line="$(/bin/ps -o command= -p "$candidate")"
	        case "$command_line" in
	          *scripts/install.sh*) installer_pid=$candidate ;;
	        esac
	        candidate="$(/bin/ps -o ppid= -p "$candidate" | tr -d '[:space:]')"
	      done
	      test -n "$installer_pid"
	      kill "-$PLANERADAR_SIGNAL_ON_DOWNLOAD" "$installer_pid"
	      exit 0
	    fi
	    test "$name" != "${PLANERADAR_INCOMPLETE_ASSET:-}" || exit 1
	    cp "$PLANERADAR_FIXTURE_RELEASE/$name" "$destination/$name"
    ;;
  "release verify")
    printf '%s\n' "$*" >"$PLANERADAR_RELEASE_VERIFICATION_RECORD"
    test "${PLANERADAR_FAIL_RELEASE_VERIFICATION:-0}" != 1
    test "$3" = "${PLANERADAR_EXPECTED_RELEASE_VERIFY_TAG:-v0.1.0}"
    test "$4" = -R
    test "$5" = "${PLANERADAR_EXPECTED_RELEASE_VERIFY_REPOSITORY:-shayne/RPi-Plane-Radar}"
    ;;
  "attestation verify")
    printf '%s\n' "$*" >>"$PLANERADAR_ATTESTATION_RECORD"
    test "${PLANERADAR_FAIL_ATTESTATION:-0}" != 1
    ;;
  "api repos/shayne/RPi-Plane-Radar/git/ref/tags/"*)
    case "$*" in
      *".object.type"*) echo "${PLANERADAR_REF_TYPE:-tag}" ;;
      *".object.sha"*) echo bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb ;;
      *) exit 1 ;;
    esac
    ;;
  "api repos/shayne/RPi-Plane-Radar/git/tags/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
    case "$*" in
      *".object.type"*) echo commit ;;
      *".object.sha"*) echo aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa ;;
      *) exit 1 ;;
    esac
    ;;
  *) exit 90 ;;
esac
"#,
    );

    let fixture_path = if Path::new("/opt/homebrew/bin/zstd").exists() {
        format!("{}:/opt/homebrew/bin:/usr/bin:/bin", bin.display())
    } else {
        format!("{}:/usr/bin:/bin", bin.display())
    };
    let mut unrelated_process = matches!(
        scenario,
        BootstrapScenario::ControlSignalWithUnrelatedProcess
            | BootstrapScenario::ControlBarrierPsFailure
            | BootstrapScenario::ControlBarrierParentMismatch
            | BootstrapScenario::ControlBarrierGroupMismatch
            | BootstrapScenario::ControlBarrierStateMismatch
            | BootstrapScenario::ControlBarrierChildExit
            | BootstrapScenario::PtyBarrierFailure
            | BootstrapScenario::PtyPostReapSignalHup
            | BootstrapScenario::PtyPostReapSignalInt
            | BootstrapScenario::PtyPostReapSignalTerm
            | BootstrapScenario::PtyPostClearSignalTerm
            | BootstrapScenario::PtyCompletionDescendantSuccess
            | BootstrapScenario::PtyCompletionDescendantFailure
            | BootstrapScenario::PtyCompletionDescendantSignal
    )
    .then(|| {
        Command::new("/bin/sleep")
            .arg("30")
            .spawn()
            .expect("unrelated sentinel process")
    });
    let startup_signal = match scenario {
        BootstrapScenario::ControlStartupSignalHup => Some("HUP"),
        BootstrapScenario::ControlStartupSignalInt => Some("INT"),
        BootstrapScenario::ControlStartupSignalTerm => Some("TERM"),
        _ => None,
    };
    if let Some(signal) = startup_signal {
        fs::write(
            &startup_debug_env,
            format!(
                r#"__planeradar_before_control_pid_publication() {{
  if [[ "$BASH_COMMAND" == 'control_pid=$!' ]]; then
    trap - DEBUG
    set +e
    printf 'signal\n' >"$PLANERADAR_STARTUP_SIGNAL_RECORD"
    kill -{signal} "$$"
    /bin/sleep 0.5
    set -e
  fi
  return 0
}}
trap '__planeradar_before_control_pid_publication' DEBUG
"#
            ),
        )
        .expect("startup signal DEBUG trap");
    }
    let post_reap_signal = match scenario {
        BootstrapScenario::PtyPostReapSignalHup => Some("HUP"),
        BootstrapScenario::PtyPostReapSignalInt => Some("INT"),
        BootstrapScenario::PtyPostReapSignalTerm => Some("TERM"),
        _ => None,
    };
    if let Some(signal) = post_reap_signal {
        fs::write(
            &inner_bash_env,
            format!(
                r#"__planeradar_retire_group_actions=0
__planeradar_initial_continue_delayed=0
kill() {{
  if [[ "$*" == "-CONT -- -${{control_pid:-0}}" &&
        "${{control_group_owned:-0}}" -eq 1 &&
        "${{control_retire_pending:-0}}" -eq 0 &&
        $__planeradar_initial_continue_delayed -eq 0 ]]; then
    __planeradar_initial_continue_delayed=1
    printf 'intercepted\n' >"$PLANERADAR_INITIAL_CONTINUE_INTERCEPT_RECORD"
    (
      __planeradar_resume_observed=0
      for ((__planeradar_resume_wait=0; __planeradar_resume_wait<3000; __planeradar_resume_wait++)); do
        if [[ -f "$PLANERADAR_INITIAL_STOPPED_OBSERVATION_RECORD" ]]; then
          __planeradar_resume_observed=1
          break
        fi
        /bin/sleep 0.01
      done
      if [[ $__planeradar_resume_observed -eq 1 ]]; then
        printf 'continued\n' >"$PLANERADAR_INITIAL_CONTINUE_ACK_RECORD"
        builtin kill "$@"
      else
        printf 'timeout\n' >"$PLANERADAR_INITIAL_CONTINUE_TIMEOUT_RECORD"
      fi
    ) &
    return 0
  fi
  case "$*" in
    *"-- -${{control_pid:-0}}"*)
      if [[ "${{control_retire_pending:-0}}" -eq 1 ]]; then
        __planeradar_retire_group_actions=$((__planeradar_retire_group_actions + 1))
        if [[ $__planeradar_retire_group_actions -gt 2 ]]; then
          printf '%s\n' "$*" >>"$PLANERADAR_STALE_CONTROL_ACTION_RECORD"
        fi
      fi
      ;;
  esac
  case "$*" in
    *"-${{control_reap_pid:-0}}"*|*"-0 ${{control_reap_pid:-0}}"*)
      if [[ "${{control_group_owned:-0}}" -eq 0 ]]; then
        printf '%s\n' "$*" >>"$PLANERADAR_STALE_CONTROL_ACTION_RECORD"
      fi
      ;;
  esac
  builtin kill "$@"
}}
__planeradar_signal_at_completion_handoff() {{
  if [[ "$BASH_COMMAND" == 'control_reap_pid=$control_pid control_reap_pending=1 control_retire_pending=0 control_group_owned=0 control_pid=""' ]]; then
    trap - DEBUG
    kill -{signal} "$$"
    [[ -f "$control_barrier" ]] &&
      printf 'marker survived\n' >"$PLANERADAR_POST_REAP_SIGNAL_RECORD"
  fi
  return 0
}}
trap '__planeradar_signal_at_completion_handoff' DEBUG
"#
            ),
        )
        .expect("completion-handoff signal DEBUG trap");
    } else if matches!(scenario, BootstrapScenario::PtyPostClearSignalTerm) {
        fs::write(
            &inner_bash_env,
            r#"kill() {
  case "$*" in
    *"-${control_reap_pid:-0}"*|*"-0 ${control_reap_pid:-0}"*)
      if [[ "${control_group_owned:-0}" -eq 0 ]]; then
        printf '%s\n' "$*" >>"$PLANERADAR_STALE_CONTROL_ACTION_RECORD"
      fi
      ;;
  esac
  builtin kill "$@"
}
__planeradar_signal_after_ownership_clear() {
  if [[ "$BASH_COMMAND" == "restore_control_terminal" ]]; then
    trap - DEBUG
    kill -TERM "$$"
    [[ -f "$control_barrier" ]] &&
      printf 'marker survived\n' >"$PLANERADAR_POST_REAP_SIGNAL_RECORD"
  fi
  return 0
}
trap '__planeradar_signal_after_ownership_clear' DEBUG
"#,
        )
        .expect("post-clear signal DEBUG trap");
    } else if matches!(
        scenario,
        BootstrapScenario::PtyCompletionDescendantSuccess
            | BootstrapScenario::PtyCompletionDescendantFailure
            | BootstrapScenario::PtyCompletionDescendantSignal
    ) {
        fs::write(
            &inner_bash_env,
            r#"kill() {
  if [[ "$*" == "-KILL -- -$control_pid" ]]; then
    descendant_pid="$(<"$PLANERADAR_CONTROL_DESCENDANT_PID_RECORD")"
    grandchild_pid="$(<"$PLANERADAR_CONTROL_GRANDCHILD_PID_RECORD")"
    root_state="$(/bin/ps -o state= -p "$control_pid" | /usr/bin/tr -d '[:space:]')"
    descendant_state="$(/bin/ps -o state= -p "$descendant_pid" | /usr/bin/tr -d '[:space:]')"
    grandchild_state="$(/bin/ps -o state= -p "$grandchild_pid" | /usr/bin/tr -d '[:space:]')"
    if [[ "$root_state" == T* && "$descendant_state" == T* &&
          "$grandchild_state" == T* ]]; then
      printf 'stopped\n' >"$PLANERADAR_COMPLETION_GROUP_STOPPED_RECORD"
    fi
  fi
  builtin kill "$@"
}
"#,
        )
        .expect("completion group-stop observation");
    } else if pty_scenario {
        fs::write(&inner_bash_env, b"").expect("empty PTY inner BASH_ENV");
    }
    let mut command = Command::new("/bin/bash");
    if pty_scenario {
        command
            .args([
                "-c",
                r#"set +e
installer=$1
shift
original_pgid="$(/bin/ps -o pgid= -p "$$" | /usr/bin/tr -d '[:space:]')"
/bin/bash -c \
  'printf "%s\n" "$$" >"$PLANERADAR_INSTALLER_PID_RECORD"; exec /usr/bin/env BASH_ENV="$PLANERADAR_INNER_BASH_ENV" /bin/bash "$@"' \
  planeradar-installer "$installer" "$@"
installer_status=$?
restored_tpgid="$(/bin/ps -o tpgid= -p "$$" | /usr/bin/tr -d '[:space:]')"
if [[ "${PLANERADAR_DELAY_PTY_RESULT_WRITE:-0}" == "1" ]]; then
  : >"$PLANERADAR_PTY_RESULT_RECORD"
  /bin/sleep 0.1
fi
printf '%s %s %s\n' "$original_pgid" "$installer_status" "$restored_tpgid" \
  >"$PLANERADAR_PTY_RESULT_RECORD"
"#,
                "planeradar-pty-wrapper",
            ])
            .arg(root().join("scripts/install.sh"))
            .args(["--hostname", "hangar", "pi@radar.local"]);
    } else {
        command.arg(root().join("scripts/install.sh")).args([
            "--hostname",
            "hangar",
            "--non-interactive",
            "pi@radar.local",
        ]);
    }
    command
        .env("PATH", fixture_path)
        .env("PLANERADAR_FIXTURE_RELEASE", &fixture)
        .env("TMPDIR", &private_root)
        .env("PLANERADAR_INSTALLER_PID_RECORD", &installer_pid_record)
        .env("PLANERADAR_INNER_BASH_ENV", &inner_bash_env)
        .env(
            "PLANERADAR_POST_REAP_SIGNAL_RECORD",
            &post_reap_signal_record,
        )
        .env(
            "PLANERADAR_STALE_CONTROL_ACTION_RECORD",
            &stale_control_action_record,
        )
        .env(
            "PLANERADAR_COMPLETION_GROUP_STOPPED_RECORD",
            &completion_group_stopped_record,
        )
        .env(
            "PLANERADAR_INITIAL_CONTINUE_INTERCEPT_RECORD",
            &initial_continue_intercept_record,
        )
        .env(
            "PLANERADAR_INITIAL_STOPPED_OBSERVATION_RECORD",
            &initial_stopped_observation_record,
        )
        .env(
            "PLANERADAR_INITIAL_CONTINUE_ACK_RECORD",
            &initial_continue_ack_record,
        )
        .env(
            "PLANERADAR_INITIAL_CONTINUE_TIMEOUT_RECORD",
            &initial_continue_timeout_record,
        )
        .env(
            "PLANERADAR_TERMINAL_HANDOFF_RECORD",
            &terminal_handoff_record,
        )
        .env(
            "PLANERADAR_CONTROL_TERMINAL_TRACE_RECORD",
            &control_terminal_trace_record,
        )
        .env(
            "PLANERADAR_COMPLETION_PUBLISHED_RECORD",
            &completion_published_record,
        )
        .env(
            "PLANERADAR_COMPLETION_SNAPSHOT_FAULT_RECORD",
            &completion_snapshot_fault_record,
        )
        .env(
            "PLANERADAR_COMPLETION_PS_SCENARIO",
            match scenario {
                BootstrapScenario::PtyTransientCompletionSnapshotFailure => "transient",
                BootstrapScenario::PtyPermanentCompletionSnapshotFailure => "permanent",
                _ => "",
            },
        )
        .env(
            "PLANERADAR_REQUIRE_TERMINAL_HANDOFF",
            if matches!(scenario, BootstrapScenario::PtyForegroundHandoff) {
                "1"
            } else {
                "0"
            },
        )
        .env(
            "PLANERADAR_FORCE_RECLAIM_AFTER_PARENT_HANDOFF",
            if matches!(scenario, BootstrapScenario::PtyForegroundHandoff) {
                "1"
            } else {
                "0"
            },
        )
        .env(
            "PLANERADAR_DELAY_INITIAL_CONTINUE",
            if post_reap_signal.is_some() { "1" } else { "0" },
        )
        .env("PLANERADAR_PTY_RESULT_RECORD", &pty_result_record)
        .env(
            "PLANERADAR_DELAY_PTY_RESULT_WRITE",
            if matches!(scenario, BootstrapScenario::PtyMalformedCompletion) {
                "1"
            } else {
                "0"
            },
        )
        .env("PLANERADAR_ARGV_RECORD", &argv_record)
        .env("PLANERADAR_DOWNLOAD_RECORD", &download_record)
        .env("PLANERADAR_METADATA_PARSE_RECORD", &metadata_parse_record)
        .env("PLANERADAR_CONTROL_READY_RECORD", &control_ready_record)
        .env(
            "PLANERADAR_CONTROL_COMPLETION_RECORD",
            &control_completion_record,
        )
        .env("PLANERADAR_CONTROL_PID_RECORD", &control_pid_record)
        .env(
            "PLANERADAR_CONTROL_DESCENDANT_PID_RECORD",
            &control_descendant_pid_record,
        )
        .env(
            "PLANERADAR_CONTROL_GRANDCHILD_PID_RECORD",
            &control_grandchild_pid_record,
        )
        .env("PLANERADAR_STARTUP_SIGNAL_RECORD", &startup_signal_record)
        .env("PLANERADAR_BARRIER_ENTRY_RECORD", &barrier_entry_record)
        .env(
            "PLANERADAR_BARRIER_PS_SCENARIO",
            match scenario {
                BootstrapScenario::ControlBarrierPsFailure => "failure",
                BootstrapScenario::ControlBarrierParentMismatch => "parent",
                BootstrapScenario::ControlBarrierGroupMismatch => "group",
                BootstrapScenario::ControlBarrierStateMismatch
                | BootstrapScenario::PtyBarrierFailure => "state",
                _ => "",
            },
        )
        .env(
            "PLANERADAR_CONTROL_EXIT_STATUS",
            if matches!(
                scenario,
                BootstrapScenario::ControlFailure
                    | BootstrapScenario::PtyControlFailure
                    | BootstrapScenario::PtyCompletionDescendantFailure
            ) {
                "37"
            } else {
                "0"
            },
        )
        .env(
            "PLANERADAR_CONTROL_DELAY_SECONDS",
            if matches!(scenario, BootstrapScenario::PtySlowSuccess) {
                "5"
            } else {
                "0"
            },
        )
        .env(
            "PLANERADAR_CONTROL_COMPLETION_LINE",
            if matches!(scenario, BootstrapScenario::PtyMalformedCompletion) {
                "malformed completion"
            } else {
                ""
            },
        )
        .env(
            "PLANERADAR_REPEAT_SIGNAL_DURING_CLEANUP",
            if matches!(scenario, BootstrapScenario::RepeatedSignalHupThenTerm) {
                "1"
            } else {
                "0"
            },
        )
        .env("PLANERADAR_ATTESTATION_RECORD", &attestation_record)
        .env(
            "PLANERADAR_RELEASE_VERIFICATION_RECORD",
            &release_verification_record,
        )
        .env(
            "PLANERADAR_FAIL_ATTESTATION",
            if matches!(
                scenario,
                BootstrapScenario::FailedAttestation | BootstrapScenario::WrongWorkflowAttestation
            ) {
                "1"
            } else {
                "0"
            },
        )
        .env(
            "PLANERADAR_RELEASE_TAG",
            if matches!(scenario, BootstrapScenario::MismatchedTag) {
                "v0.1.1"
            } else {
                "v0.1.0"
            },
        )
        .env(
            "PLANERADAR_REF_TYPE",
            if matches!(scenario, BootstrapScenario::MutableTag) {
                "blob"
            } else {
                "tag"
            },
        )
        .env(
            "PLANERADAR_INCOMPLETE_ASSET",
            if matches!(scenario, BootstrapScenario::IncompleteDownload) {
                "SBOM.spdx.json"
            } else {
                ""
            },
        )
        .env(
            "PLANERADAR_FAIL_RELEASE_LOOKUP",
            if matches!(scenario, BootstrapScenario::FailedReleaseLookup) {
                "1"
            } else {
                "0"
            },
        )
        .env(
            "PLANERADAR_FAIL_RELEASE_VERIFICATION",
            if matches!(scenario, BootstrapScenario::FailedReleaseVerification) {
                "1"
            } else {
                "0"
            },
        )
        .env(
            "PLANERADAR_INSUFFICIENT_TEMP_SPACE",
            if matches!(scenario, BootstrapScenario::InsufficientTemporarySpace) {
                "1"
            } else {
                "0"
            },
        )
        .env(
            "PLANERADAR_SIGNAL_ON_DOWNLOAD",
            match scenario {
                BootstrapScenario::SignalHup | BootstrapScenario::RepeatedSignalHupThenTerm => {
                    "HUP"
                }
                BootstrapScenario::SignalInt => "INT",
                BootstrapScenario::SignalTerm => "TERM",
                _ => "",
            },
        )
        .env(
            "PLANERADAR_CONTROL_DEFAULT_SIGINT",
            if matches!(scenario, BootstrapScenario::PtyTerminalInterrupt) {
                "1"
            } else {
                "0"
            },
        )
        .env(
            "PLANERADAR_CONTROL_WORKER_SIGNAL",
            if matches!(scenario, BootstrapScenario::PtyCompletionDescendantSignal) {
                "TERM"
            } else {
                ""
            },
        )
        .env(
            "PLANERADAR_EXPECTED_RELEASE_VERIFY_TAG",
            if matches!(scenario, BootstrapScenario::WrongReleaseVerificationTag) {
                "v9.9.9"
            } else {
                "v0.1.0"
            },
        )
        .env(
            "PLANERADAR_EXPECTED_RELEASE_VERIFY_REPOSITORY",
            if matches!(
                scenario,
                BootstrapScenario::WrongReleaseVerificationRepository
            ) {
                "someone/else"
            } else {
                "shayne/RPi-Plane-Radar"
            },
        );
    if startup_signal.is_some() {
        command.env("BASH_ENV", &startup_debug_env);
    }
    let mut pty_master = None;
    let mut process = if pty_scenario {
        let (master, slave) = open_release_pty();
        command
            .stdin(Stdio::from(slave.try_clone().expect("PTY stdin")))
            .stdout(Stdio::from(slave.try_clone().expect("PTY stdout")))
            .stderr(Stdio::from(slave.try_clone().expect("PTY stderr")));
        // SAFETY: this closure uses only async-signal-safe libc calls between
        // fork and exec; fd 0 is already connected to the PTY slave.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::ioctl(0, libc::TIOCSCTTY as _, 0) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::tcsetpgrp(0, libc::getpgrp()) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let process = command.spawn().expect("start PTY bootstrap fixture");
        drop(slave);
        pty_master = Some(master);
        process
    } else {
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("start bootstrap fixture")
    };
    if let Some(master) = pty_master.as_mut() {
        master
            .write_all(b"interactive input\n")
            .expect("write bootstrap PTY stdin");
    } else {
        process
            .stdin
            .take()
            .expect("bootstrap stdin")
            .write_all(b"interactive input\n")
            .expect("write bootstrap stdin");
    }
    let control_signal = match scenario {
        BootstrapScenario::ControlSignalHup | BootstrapScenario::PtyControlSignalHup => Some("HUP"),
        BootstrapScenario::ControlSignalInt | BootstrapScenario::PtyControlSignalInt => Some("INT"),
        BootstrapScenario::ControlSignalTerm | BootstrapScenario::PtyControlSignalTerm => {
            Some("TERM")
        }
        BootstrapScenario::ControlSignalWithUnrelatedProcess => Some("HUP"),
        _ => None,
    };
    let cancellation_started = if let Some(signal) = control_signal {
        let deadline = Instant::now() + Duration::from_secs(60);
        while !control_ready_record.exists() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(
            control_ready_record.exists(),
            "long-running control did not become ready"
        );
        let cancellation_started = Instant::now();
        let signal_pid = if pty_scenario {
            let deadline = Instant::now() + Duration::from_secs(60);
            while !installer_pid_record.exists() && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(10));
            }
            recorded_pid(&installer_pid_record).unwrap_or_else(|| {
                panic!(
                    "recorded PTY installer PID; output={}",
                    String::from_utf8_lossy(&read_release_pty(
                        pty_master.as_mut().expect("PTY diagnostic")
                    ))
                )
            })
        } else {
            process.id()
        };
        let status = Command::new("/bin/kill")
            .args([format!("-{signal}"), signal_pid.to_string()])
            .status()
            .expect("signal only the installer process");
        assert!(status.success());
        Some(cancellation_started)
    } else if matches!(scenario, BootstrapScenario::PtyTerminalInterrupt) {
        let deadline = Instant::now() + Duration::from_secs(60);
        while !control_ready_record.exists() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(
            control_ready_record.exists(),
            "PTY control did not become ready for terminal interruption"
        );
        let cancellation_started = Instant::now();
        pty_master
            .as_mut()
            .expect("PTY master for terminal interruption")
            .write_all(&[3])
            .expect("write terminal interrupt");
        Some(cancellation_started)
    } else if startup_signal.is_some() {
        let deadline = Instant::now() + Duration::from_secs(60);
        while !startup_signal_record.exists() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(
            startup_signal_record.exists(),
            "startup fixture did not deliver its pre-publication signal"
        );
        Some(Instant::now())
    } else if matches!(
        scenario,
        BootstrapScenario::ControlBarrierPsFailure
            | BootstrapScenario::ControlBarrierParentMismatch
            | BootstrapScenario::ControlBarrierGroupMismatch
            | BootstrapScenario::ControlBarrierStateMismatch
            | BootstrapScenario::ControlBarrierChildExit
            | BootstrapScenario::PtyBarrierFailure
    ) {
        let deadline = Instant::now() + Duration::from_secs(60);
        while !barrier_entry_record.exists() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(
            barrier_entry_record.exists(),
            "control did not enter its native bootstrap barrier"
        );
        Some(Instant::now())
    } else {
        None
    };
    let (
        output_success,
        output_status_code,
        output_stdout,
        output_stderr,
        original_terminal_pgid,
        restored_terminal_pgid,
    ) = if pty_scenario {
        let deadline = Instant::now() + Duration::from_secs(60);
        let result = loop {
            let result = fs::read_to_string(&pty_result_record).unwrap_or_default();
            if result.ends_with('\n')
                && result.lines().count() == 1
                && result.split_whitespace().count() == 3
            {
                break result;
            }
            if Instant::now() >= deadline {
                let output = read_release_pty(pty_master.as_mut().expect("PTY timeout output"));
                stop_pty_wrapper(&mut process, &installer_pid_record, &control_pid_record);
                panic!(
                    "PTY bootstrap fixture published no complete result: {result:?}; output={}",
                    String::from_utf8_lossy(&output)
                );
            }
            thread::sleep(Duration::from_millis(10));
        };
        let fields = result.split_whitespace().collect::<Vec<_>>();
        assert_eq!(fields.len(), 3, "unexpected PTY result: {result:?}");
        let original = fields[0].parse::<u32>().expect("original PTY PGID");
        let status = fields[1].parse::<i32>().expect("PTY installer status");
        let restored = fields[2].parse::<u32>().expect("restored PTY PGID");
        if process.try_wait().expect("inspect PTY wrapper").is_none() {
            let _ = process.kill();
        }
        (
            status == 0,
            Some(status),
            read_release_pty(pty_master.as_mut().expect("PTY output")),
            Vec::new(),
            Some(original),
            Some(restored),
        )
    } else {
        let output = process.wait_with_output().expect("run bootstrap fixture");
        (
            output.status.success(),
            output.status.code(),
            output.stdout,
            output.stderr,
            None,
            None,
        )
    };
    let cancellation_latency = cancellation_started.map(|started| started.elapsed());
    let unrelated_process_survived = unrelated_process.as_mut().map(|process| {
        process
            .try_wait()
            .expect("inspect unrelated sentinel process")
            .is_none()
    });
    if let Some(process) = unrelated_process.as_mut() {
        if process
            .try_wait()
            .expect("reinspect unrelated sentinel process")
            .is_none()
        {
            process.kill().expect("stop unrelated sentinel process");
        }
        process.wait().expect("reap unrelated sentinel process");
    }
    if !output_success {
        eprintln!(
            "bootstrap fixture stderr: {}",
            String::from_utf8_lossy(&output_stderr)
        );
    }
    let argv = fs::read_to_string(argv_record).unwrap_or_default();
    let attestations = fs::read_to_string(attestation_record).unwrap_or_default();
    let release_verification = fs::read_to_string(release_verification_record).unwrap_or_default();
    let downloads = fs::read_to_string(download_record).unwrap_or_default();
    let metadata_parses = fs::read_to_string(metadata_parse_record).unwrap_or_default();
    let control_processes_recorded = [
        &control_pid_record,
        &control_descendant_pid_record,
        &control_grandchild_pid_record,
    ]
    .into_iter()
    .filter_map(|record| fs::read_to_string(record).ok())
    .filter_map(|pid| pid.trim().parse::<u32>().ok())
    .collect::<Vec<_>>();
    let control_processes_alive = control_processes_recorded
        .iter()
        .copied()
        .filter(|pid| {
            Command::new("/bin/kill")
                .args(["-0", &pid.to_string()])
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|status| status.success())
        })
        .collect::<Vec<_>>();
    if let Some(pid) = control_processes_alive.first() {
        let pgid = Command::new("/bin/ps")
            .args(["-o", "pgid=", "-p", &pid.to_string()])
            .output()
            .ok()
            .filter(|output| output.status.success())
            .and_then(|output| String::from_utf8(output.stdout).ok())
            .and_then(|value| value.trim().parse::<u32>().ok());
        if let Some(pgid) = pgid.filter(|pgid| *pgid > 1) {
            let _ = Command::new("/bin/kill")
                .args(["-KILL", &format!("-{pgid}")])
                .status();
        }
    }
    let private_residue = fs::read_dir(&private_root)
        .expect("private fixture contents")
        .map(|entry| {
            entry
                .expect("private fixture entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect::<Vec<_>>();
    BootstrapOutcome {
        success: output_success,
        status_code: output_status_code,
        stdout: String::from_utf8_lossy(&output_stdout).into_owned(),
        argv,
        attestations,
        release_verification,
        downloads,
        metadata_parses,
        private_residue,
        stderr: String::from_utf8_lossy(&output_stderr).into_owned(),
        boundary_sizes,
        control_completion: control_completion_record.exists(),
        control_processes_recorded,
        control_processes_alive,
        cancellation_latency,
        unrelated_process_survived,
        original_terminal_pgid,
        restored_terminal_pgid,
        post_reap_restore_marker_survived: post_reap_signal_record.exists(),
        stale_control_action_recorded: stale_control_action_record.exists(),
        completion_group_was_stopped: completion_group_stopped_record.exists(),
        initial_continue_was_intercepted: initial_continue_intercept_record.exists(),
        initial_stopped_state_was_observed: initial_stopped_observation_record.exists(),
        initial_continue_followed_observation: initial_continue_ack_record.exists(),
        initial_continue_handshake_timed_out: initial_continue_timeout_record.exists(),
        terminal_handoff_observed: terminal_handoff_record.exists(),
        control_terminal_trace: fs::read_to_string(control_terminal_trace_record)
            .unwrap_or_default(),
        completion_snapshot_fault_was_injected: completion_snapshot_fault_record.exists(),
    }
}

fn bootstrap_fixture(scenario: BootstrapScenario) -> (bool, String, String, String) {
    let outcome = bootstrap_fixture_outcome(scenario);
    (
        outcome.success,
        outcome.argv,
        outcome.attestations,
        outcome.release_verification,
    )
}

#[test]
fn bootstrap_verifies_both_subjects_and_forwards_exact_typed_argv_offline() {
    let outcome = bootstrap_fixture_outcome(BootstrapScenario::Success);
    assert!(outcome.success);
    assert_eq!(outcome.status_code, Some(0));
    assert_eq!(
        outcome.stdout,
        "control stdin=interactive input\ncontrol stdout\n"
    );
    assert_eq!(outcome.stderr, "control stderr\n");
    assert!(
        outcome.private_residue.is_empty(),
        "private bootstrap state remained after successful control execution: {:?}",
        outcome.private_residue
    );
    assert_eq!(
        outcome
            .argv
            .lines()
            .filter(|line| !line.contains("/release"))
            .collect::<Vec<_>>(),
        [
            "install",
            "pi@radar.local",
            "--version",
            "0.1.0",
            "--hostname",
            "hangar",
            "--non-interactive",
        ]
    );
    assert_eq!(outcome.attestations.lines().count(), 2);
    assert!(outcome.attestations.contains("install.sh"));
    assert!(
        outcome
            .attestations
            .contains("planeradarctl-aarch64-apple-darwin.tar.zst")
    );
    assert!(outcome.attestations.lines().all(|line| {
        line.contains("--signer-workflow")
            && line.contains("--source-ref refs/heads/main")
            && line.contains("--source-digest aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
            && line.contains("--deny-self-hosted-runners")
    }));
    assert_eq!(
        outcome.release_verification.trim(),
        "release verify v0.1.0 -R shayne/RPi-Plane-Radar"
    );
}

#[test]
fn bootstrap_accepts_every_declared_boundary_above_the_old_darwin_half_limit() {
    let outcome = bootstrap_fixture_outcome(BootstrapScenario::AcceptedDeclaredBoundaries);
    let sizes = outcome
        .boundary_sizes
        .as_ref()
        .expect("accepted-boundary fixture sizes");
    for (name, actual, old_half_limit, declared_maximum) in [
        ("installer metadata", sizes.installer, 32 * 1024, 64 * 1024),
        ("manifest metadata", sizes.manifest, 32 * 1024, 64 * 1024),
        ("checksums metadata", sizes.checksums, 8 * 1024, 16 * 1024),
        ("SBOM metadata", sizes.sbom, 512 * 1024, 1024 * 1024),
        (
            "control archive",
            sizes.control_archive,
            8 * 1024 * 1024,
            16 * 1024 * 1024,
        ),
        (
            "expanded archive",
            sizes.expanded_archive,
            16 * 1024 * 1024,
            32 * 1024 * 1024,
        ),
        (
            "control member",
            sizes.control_member,
            8 * 1024 * 1024,
            16 * 1024 * 1024,
        ),
    ] {
        assert!(
            actual > old_half_limit,
            "{name} fixture must exceed the old effective limit: {actual} <= {old_half_limit}"
        );
        assert!(
            actual <= declared_maximum,
            "{name} fixture must remain within its declared maximum: {actual} > {declared_maximum}"
        );
    }
    assert!(
        outcome.success,
        "declared-boundary bootstrap failed: {}",
        outcome.stderr
    );
    assert_eq!(outcome.status_code, Some(0));
    assert_eq!(
        outcome.stdout,
        "control stdin=interactive input\ncontrol stdout\n"
    );
    assert_eq!(outcome.stderr, "control stderr\n");
    assert!(
        outcome.private_residue.is_empty(),
        "private bootstrap state remained after declared-boundary success: {:?}",
        outcome.private_residue
    );
}

#[test]
fn bootstrap_preserves_failing_control_status_and_streams_then_cleans_private_state() {
    let outcome = bootstrap_fixture_outcome(BootstrapScenario::ControlFailure);
    assert!(!outcome.success);
    assert_eq!(outcome.status_code, Some(37));
    assert_eq!(
        outcome.stdout,
        "control stdin=interactive input\ncontrol stdout\n"
    );
    assert_eq!(outcome.stderr, "control stderr\n");
    assert_eq!(
        outcome
            .argv
            .lines()
            .filter(|line| !line.contains("/release"))
            .collect::<Vec<_>>(),
        [
            "install",
            "pi@radar.local",
            "--version",
            "0.1.0",
            "--hostname",
            "hangar",
            "--non-interactive",
        ]
    );
    assert!(
        outcome.private_residue.is_empty(),
        "private bootstrap state remained after failing control execution: {:?}",
        outcome.private_residue
    );
}

#[test]
fn bootstrap_rejects_failed_attestation_and_hostile_archive_offline() {
    let (attestation_success, attestation_argv, _, _) =
        bootstrap_fixture(BootstrapScenario::FailedAttestation);
    assert!(!attestation_success);
    assert!(attestation_argv.is_empty());

    let (archive_success, archive_argv, _, _) =
        bootstrap_fixture(BootstrapScenario::ExtraArchiveMember);
    assert!(!archive_success);
    assert!(archive_argv.is_empty());
}

#[test]
fn bootstrap_rejects_release_identity_integrity_and_download_failures_offline() {
    for scenario in [
        BootstrapScenario::MutableTag,
        BootstrapScenario::MismatchedTag,
        BootstrapScenario::BadManifest,
        BootstrapScenario::BadChecksum,
        BootstrapScenario::IncompleteDownload,
        BootstrapScenario::FailedReleaseLookup,
    ] {
        let (success, argv, _, _) = bootstrap_fixture(scenario);
        assert!(!success);
        assert!(argv.is_empty());
    }
}

#[test]
fn bootstrap_rejects_wrong_workflow_attestation_and_macho_architecture_offline() {
    for scenario in [
        BootstrapScenario::WrongWorkflowAttestation,
        BootstrapScenario::WrongArchitecture,
    ] {
        let (success, argv, _, _) = bootstrap_fixture(scenario);
        assert!(!success);
        assert!(argv.is_empty());
    }
}

#[test]
fn bootstrap_rejects_release_verification_failures_and_wrong_identity_offline() {
    for scenario in [
        BootstrapScenario::FailedReleaseVerification,
        BootstrapScenario::WrongReleaseVerificationTag,
        BootstrapScenario::WrongReleaseVerificationRepository,
    ] {
        let (success, argv, _, release_verification) = bootstrap_fixture(scenario);
        assert!(!success);
        assert!(argv.is_empty());
        assert!(!release_verification.is_empty());
    }
}

#[test]
fn bootstrap_rejects_every_bounded_archive_attack_before_execution() {
    for scenario in [
        BootstrapScenario::DuplicateArchiveMember,
        BootstrapScenario::SymlinkArchiveMember,
        BootstrapScenario::SpecialArchiveMember,
        BootstrapScenario::OversizedArchiveMember,
        BootstrapScenario::OversizedCompressedArchive,
        BootstrapScenario::DecompressionBomb,
        BootstrapScenario::TruncatedArchive,
    ] {
        let (success, argv, _, _) = bootstrap_fixture(scenario);
        assert!(!success, "hostile archive scenario was accepted");
        assert!(argv.is_empty());
    }
}

#[test]
fn bootstrap_preflights_total_private_space_before_downloading_any_asset() {
    let outcome = bootstrap_fixture_outcome(BootstrapScenario::InsufficientTemporarySpace);
    assert!(!outcome.success);
    assert!(outcome.argv.is_empty());
    assert!(outcome.downloads.is_empty());
    assert!(outcome.metadata_parses.is_empty());
    assert!(outcome.private_residue.is_empty());
}

#[test]
fn bootstrap_rejects_each_oversized_metadata_asset_before_parsing_or_execution() {
    for scenario in [
        BootstrapScenario::OversizedInstallerMetadata,
        BootstrapScenario::OversizedManifestMetadata,
        BootstrapScenario::OversizedChecksumsMetadata,
        BootstrapScenario::OversizedSbomMetadata,
    ] {
        let outcome = bootstrap_fixture_outcome(scenario);
        assert!(!outcome.success, "oversized metadata was accepted");
        assert!(
            outcome.argv.is_empty(),
            "control executed after oversized metadata"
        );
        assert!(
            outcome.metadata_parses.is_empty(),
            "metadata parser ran before all sizes were accepted: {}",
            outcome.metadata_parses
        );
        assert!(
            outcome.private_residue.is_empty(),
            "private bootstrap state remained after oversized metadata: {:?}",
            outcome.private_residue
        );
    }
}

#[test]
fn bootstrap_signals_cancel_with_conventional_status_and_remove_private_state() {
    for (scenario, expected_status) in [
        (BootstrapScenario::SignalHup, 129),
        (BootstrapScenario::SignalInt, 130),
        (BootstrapScenario::SignalTerm, 143),
    ] {
        let outcome = bootstrap_fixture_outcome(scenario);
        assert!(!outcome.success);
        assert_eq!(
            outcome.status_code,
            Some(expected_status),
            "unexpected signal status; stderr: {}",
            outcome.stderr
        );
        assert!(
            outcome.argv.is_empty(),
            "control executed after cancellation"
        );
        assert_eq!(
            outcome.downloads, "install.sh\n",
            "installer continued downloading after cancellation"
        );
        assert!(
            outcome.private_residue.is_empty(),
            "private bootstrap state remained after cancellation: {:?}",
            outcome.private_residue
        );
    }
}

fn assert_active_control_tree_cancellation(scenario: BootstrapScenario, expected_status: i32) {
    let outcome = bootstrap_fixture_outcome(scenario);
    assert!(!outcome.success);
    assert_eq!(
        outcome.status_code,
        Some(expected_status),
        "unexpected active-control signal status; stderr: {}",
        outcome.stderr
    );
    assert_eq!(
        outcome
            .argv
            .lines()
            .filter(|line| !line.contains("/release"))
            .collect::<Vec<_>>(),
        [
            "install",
            "pi@radar.local",
            "--version",
            "0.1.0",
            "--hostname",
            "hangar",
            "--non-interactive",
        ]
    );
    assert_eq!(
        outcome.stdout,
        "control stdin=interactive input\ncontrol stdout\n"
    );
    assert_eq!(outcome.stderr, "control stderr\n");
    assert!(
        !outcome.control_completion
            && outcome
                .cancellation_latency
                .is_some_and(|latency| latency < Duration::from_secs(5)),
        "control completion={} cancellation latency={:?}",
        outcome.control_completion,
        outcome.cancellation_latency
    );
    assert_eq!(
        outcome.control_processes_recorded.len(),
        3,
        "fixture did not record the control, child, and deepest grandchild"
    );
    assert!(
        outcome.control_processes_alive.is_empty(),
        "control descendants survived installer cancellation: {:?}",
        outcome.control_processes_alive
    );
    assert!(
        outcome.private_residue.is_empty(),
        "private bootstrap state remained after active-control cancellation: {:?}",
        outcome.private_residue
    );
}

#[test]
fn bootstrap_hup_cancels_the_active_control_tree_before_remote_mutation() {
    assert_active_control_tree_cancellation(BootstrapScenario::ControlSignalHup, 129);
}

#[test]
fn bootstrap_int_cancels_the_active_control_tree_before_remote_mutation() {
    assert_active_control_tree_cancellation(BootstrapScenario::ControlSignalInt, 130);
}

#[test]
fn bootstrap_term_cancels_the_active_control_tree_before_remote_mutation() {
    assert_active_control_tree_cancellation(BootstrapScenario::ControlSignalTerm, 143);
}

fn assert_startup_control_cancellation(scenario: BootstrapScenario, status_code: i32) {
    let outcome = bootstrap_fixture_outcome(scenario);
    assert_eq!(
        outcome.status_code,
        Some(status_code),
        "startup cancellation stderr: {}",
        outcome.stderr
    );
    assert!(
        !outcome.control_completion,
        "control mutated after a signal delivered before PID publication"
    );
    assert!(
        outcome
            .cancellation_latency
            .is_some_and(|latency| latency < Duration::from_secs(8)),
        "startup cancellation was not prompt: {:?}",
        outcome.cancellation_latency
    );
    assert!(
        outcome.control_processes_alive.is_empty(),
        "startup control survived cancellation: {:?}",
        outcome.control_processes_alive
    );
    assert!(
        outcome.private_residue.is_empty(),
        "private bootstrap state remained after startup cancellation: {:?}",
        outcome.private_residue
    );
}

#[test]
fn bootstrap_hup_before_control_pid_publication_prevents_immediate_mutation() {
    assert_startup_control_cancellation(BootstrapScenario::ControlStartupSignalHup, 129);
}

#[test]
fn bootstrap_int_before_control_pid_publication_prevents_immediate_mutation() {
    assert_startup_control_cancellation(BootstrapScenario::ControlStartupSignalInt, 130);
}

#[test]
fn bootstrap_term_before_control_pid_publication_prevents_immediate_mutation() {
    assert_startup_control_cancellation(BootstrapScenario::ControlStartupSignalTerm, 143);
}

#[test]
fn bootstrap_barrier_validation_failures_never_continue_or_mutate_control() {
    for scenario in [
        BootstrapScenario::ControlBarrierPsFailure,
        BootstrapScenario::ControlBarrierParentMismatch,
        BootstrapScenario::ControlBarrierGroupMismatch,
        BootstrapScenario::ControlBarrierStateMismatch,
        BootstrapScenario::ControlBarrierChildExit,
    ] {
        let outcome = bootstrap_fixture_outcome(scenario);
        assert!(
            !outcome.success,
            "invalid native bootstrap barrier was accepted"
        );
        assert!(
            !outcome.control_completion,
            "control mutated after native barrier validation failed"
        );
        assert_eq!(
            outcome.unrelated_process_survived,
            Some(true),
            "barrier failure signaled an unrelated process"
        );
        assert!(
            outcome.control_processes_alive.is_empty(),
            "barrier-failed control survived: {:?}",
            outcome.control_processes_alive
        );
        assert!(
            outcome.private_residue.is_empty(),
            "private state remained after barrier failure: {:?}",
            outcome.private_residue
        );
        assert!(
            outcome
                .cancellation_latency
                .is_some_and(|latency| latency < Duration::from_secs(5)),
            "barrier failure did not exit promptly: {:?}",
            outcome.cancellation_latency
        );
    }
}

#[test]
fn bootstrap_uses_owned_process_groups_without_recursive_process_discovery() {
    let installer = fs::read_to_string(root().join("scripts/install.sh")).expect("installer");
    assert!(
        !installer.contains("pgrep"),
        "bootstrap must not discover mutable process topology with pgrep"
    );
}

#[test]
fn bootstrap_process_group_cancellation_does_not_signal_an_unrelated_process() {
    let outcome = bootstrap_fixture_outcome(BootstrapScenario::ControlSignalWithUnrelatedProcess);
    assert_eq!(outcome.status_code, Some(129));
    assert_eq!(outcome.unrelated_process_survived, Some(true));
    assert!(
        !outcome.control_completion,
        "a partially contained control tree reached its mutation marker"
    );
    assert!(
        outcome.control_processes_alive.is_empty(),
        "partially contained control descendants survived: {:?}",
        outcome.control_processes_alive
    );
    assert!(
        outcome.private_residue.is_empty(),
        "private state remained after partial-containment failure: {:?}",
        outcome.private_residue
    );
}

fn assert_pty_foreground_restored(outcome: &BootstrapOutcome) {
    assert_eq!(
        outcome.original_terminal_pgid, outcome.restored_terminal_pgid,
        "controlling terminal foreground PGID was not restored"
    );
    assert!(
        outcome.original_terminal_pgid.is_some(),
        "PTY fixture did not record a foreground process group"
    );
    assert!(
        !outcome.stdout.contains("Stopped"),
        "PTY control was stopped by terminal job control: {}",
        outcome.stdout
    );
    assert!(
        outcome.private_residue.is_empty(),
        "private PTY bootstrap state remained: {:?}",
        outcome.private_residue
    );
}

#[test]
fn bootstrap_real_pty_preserves_interactive_io_statuses_and_foreground_ownership() {
    for (scenario, expected_status) in [
        (BootstrapScenario::PtySuccess, 0),
        (BootstrapScenario::PtyControlFailure, 37),
    ] {
        let outcome = bootstrap_fixture_outcome(scenario);
        assert_eq!(
            outcome.status_code,
            Some(expected_status),
            "unexpected completion-handoff status; stdout={} stderr={}",
            outcome.stdout,
            outcome.stderr
        );
        assert_eq!(outcome.success, expected_status == 0);
        assert!(
            outcome.stdout.contains("control stdin=interactive input"),
            "interactive PTY input was not read: {}",
            outcome.stdout
        );
        assert!(
            outcome.stdout.contains("control stdout") && outcome.stdout.contains("control stderr"),
            "control streams were not visible through the PTY: {}",
            outcome.stdout
        );
        assert!(
            !outcome.argv.contains("--non-interactive"),
            "PTY fixture unexpectedly disabled interaction"
        );
        assert!(outcome.control_processes_alive.is_empty());
        assert_pty_foreground_restored(&outcome);
    }
}

#[test]
fn bootstrap_real_pty_allows_verified_control_to_run_longer_than_startup_timeout() {
    let outcome = bootstrap_fixture_outcome(BootstrapScenario::PtySlowSuccess);
    assert_eq!(
        outcome.status_code,
        Some(0),
        "long-running verified control was mistaken for a failed completion barrier; stdout={} stderr={}",
        outcome.stdout,
        outcome.stderr
    );
    assert!(outcome.success);
    assert!(outcome.control_processes_alive.is_empty());
    assert_pty_foreground_restored(&outcome);
}

#[test]
fn bootstrap_real_pty_hands_off_before_continue_and_recovers_delayed_foreground_reclaim() {
    let outcome = bootstrap_fixture_outcome(BootstrapScenario::PtyForegroundHandoff);
    assert!(
        outcome.terminal_handoff_observed,
        "installer did not authenticate the parent-side terminal handoff"
    );
    assert_eq!(
        outcome.status_code,
        Some(0),
        "control was continued before its parent-side terminal handoff; stdout={} stderr={} terminal_trace={}",
        outcome.stdout,
        outcome.stderr,
        outcome.control_terminal_trace
    );
    assert!(outcome.success);
    assert!(outcome.control_processes_alive.is_empty());
    let trace = outcome
        .control_terminal_trace
        .lines()
        .map(|line| line.split_whitespace().collect::<Vec<_>>())
        .collect::<Vec<_>>();
    assert_eq!(trace.len(), 3, "unexpected terminal trace: {trace:?}");
    assert_eq!(trace[0][0], "reclaimer");
    assert_eq!(trace[0][1], trace[0][2]);
    assert_eq!(
        trace[0][2].parse::<u32>().ok(),
        outcome.original_terminal_pgid,
        "fixture did not restore the original foreground group"
    );
    assert_eq!(trace[1][0], "supervisor");
    assert_eq!(trace[1][1], trace[1][2]);
    assert_eq!(trace[2][0], "worker");
    assert_eq!(trace[2][1], trace[1][1]);
    assert_eq!(trace[2][2], trace[1][2]);
    assert_pty_foreground_restored(&outcome);
}

#[test]
fn bootstrap_real_pty_rejects_a_stopped_control_with_malformed_completion() {
    let outcome = bootstrap_fixture_outcome(BootstrapScenario::PtyMalformedCompletion);
    assert_eq!(
        outcome.status_code,
        Some(1),
        "malformed completion did not fail closed; stdout={} stderr={}",
        outcome.stdout,
        outcome.stderr
    );
    assert!(
        outcome
            .stdout
            .contains("verified control completion barrier failed")
    );
    assert!(outcome.control_processes_alive.is_empty());
    assert_pty_foreground_restored(&outcome);
}

#[test]
fn bootstrap_real_pty_completion_retires_worker_descendants_before_clearing_group_authority() {
    for (scenario, expected_status) in [
        (BootstrapScenario::PtyCompletionDescendantSuccess, 0),
        (BootstrapScenario::PtyCompletionDescendantFailure, 37),
        (BootstrapScenario::PtyCompletionDescendantSignal, 143),
    ] {
        let outcome = bootstrap_fixture_outcome(scenario);
        assert_eq!(
            outcome.status_code,
            Some(expected_status),
            "unexpected descendant-completion status; stdout={} stderr={}",
            outcome.stdout,
            outcome.stderr
        );
        assert_eq!(outcome.success, expected_status == 0);
        assert_eq!(
            outcome.control_processes_recorded.len(),
            3,
            "completion descendant PID records were incomplete; stdout={} stderr={}",
            outcome.stdout,
            outcome.stderr
        );
        assert!(
            outcome.completion_group_was_stopped,
            "completion retirement did not observe the whole owned group stopped"
        );
        assert!(
            outcome.control_processes_alive.is_empty(),
            "completion orphaned control descendants: {:?}",
            outcome.control_processes_alive
        );
        assert!(
            !outcome.control_completion,
            "an orphaned completion descendant reached its delayed mutation"
        );
        assert_eq!(outcome.unrelated_process_survived, Some(true));
        assert_pty_foreground_restored(&outcome);
    }
}

#[test]
fn bootstrap_real_pty_explicit_signals_cancel_the_deepest_control_tree_and_restore_foreground() {
    for (scenario, expected_status) in [
        (BootstrapScenario::PtyControlSignalHup, 129),
        (BootstrapScenario::PtyControlSignalInt, 130),
        (BootstrapScenario::PtyControlSignalTerm, 143),
    ] {
        let outcome = bootstrap_fixture_outcome(scenario);
        assert_eq!(outcome.status_code, Some(expected_status));
        assert_eq!(outcome.control_processes_recorded.len(), 3);
        assert!(
            outcome.control_processes_alive.is_empty(),
            "PTY cancellation left control processes alive: {:?}",
            outcome.control_processes_alive
        );
        assert!(!outcome.control_completion);
        assert!(
            outcome
                .cancellation_latency
                .is_some_and(|latency| latency < Duration::from_secs(5)),
            "PTY cancellation was not prompt: {:?}",
            outcome.cancellation_latency
        );
        assert_pty_foreground_restored(&outcome);
    }
}

#[test]
fn bootstrap_real_pty_terminal_interrupt_restores_foreground_after_control_group_death() {
    let outcome = bootstrap_fixture_outcome(BootstrapScenario::PtyTerminalInterrupt);
    assert_eq!(outcome.status_code, Some(130));
    assert_eq!(outcome.control_processes_recorded.len(), 3);
    assert!(outcome.control_processes_alive.is_empty());
    assert!(!outcome.control_completion);
    assert!(
        outcome
            .cancellation_latency
            .is_some_and(|latency| latency < Duration::from_secs(5))
    );
    assert_pty_foreground_restored(&outcome);
}

#[test]
fn bootstrap_real_pty_post_reap_signals_restore_foreground_before_conventional_exit() {
    for (scenario, expected_status) in [
        (BootstrapScenario::PtyPostReapSignalHup, 129),
        (BootstrapScenario::PtyPostReapSignalInt, 130),
        (BootstrapScenario::PtyPostReapSignalTerm, 143),
    ] {
        let outcome = bootstrap_fixture_outcome(scenario);
        assert_eq!(
            outcome.status_code,
            Some(expected_status),
            "unexpected completion-handoff status; stdout={} stderr={}",
            outcome.stdout,
            outcome.stderr
        );
        assert!(
            outcome.post_reap_restore_marker_survived,
            "signal did not land after authenticated completion became observable"
        );
        assert!(
            outcome.initial_continue_was_intercepted,
            "fixture did not intercept the initial control-group continuation"
        );
        assert!(
            outcome.initial_stopped_state_was_observed,
            "installer did not observe the supervisor stopped before continuation"
        );
        assert!(
            outcome.initial_continue_followed_observation,
            "fixture did not continue the supervisor after observing it stopped"
        );
        assert!(
            !outcome.initial_continue_handshake_timed_out,
            "fixture timed out before observing the stopped supervisor"
        );
        assert_eq!(outcome.unrelated_process_survived, Some(true));
        assert!(
            !outcome.stale_control_action_recorded,
            "retired control identity was used for a stale signal or liveness probe"
        );
        assert!(outcome.control_processes_alive.is_empty());
        assert_pty_foreground_restored(&outcome);
    }
}

#[test]
fn bootstrap_real_pty_retries_transient_authenticated_completion_snapshot_failures() {
    let outcome =
        bootstrap_fixture_outcome(BootstrapScenario::PtyTransientCompletionSnapshotFailure);
    assert_eq!(
        outcome.status_code,
        Some(0),
        "transient completion sampling failed; stdout={} stderr={} terminal_trace={}",
        outcome.stdout,
        outcome.stderr,
        outcome.control_terminal_trace
    );
    assert!(
        outcome.completion_snapshot_fault_was_injected,
        "fixture did not inject the authenticated completion sampling fault"
    );
    assert!(outcome.control_processes_alive.is_empty());
    assert!(outcome.private_residue.is_empty());
    assert_pty_foreground_restored(&outcome);
}

#[test]
fn bootstrap_real_pty_permanent_authenticated_completion_snapshot_failure_fails_closed() {
    let outcome =
        bootstrap_fixture_outcome(BootstrapScenario::PtyPermanentCompletionSnapshotFailure);
    assert_eq!(outcome.status_code, Some(1));
    assert!(
        outcome.completion_snapshot_fault_was_injected,
        "fixture did not inject the permanent completion sampling fault"
    );
    assert!(outcome.control_processes_alive.is_empty());
    assert!(outcome.private_residue.is_empty());
    assert_pty_foreground_restored(&outcome);
}

#[test]
fn bootstrap_real_pty_post_clear_signal_only_latches_status_before_supervisor_reap() {
    let outcome = bootstrap_fixture_outcome(BootstrapScenario::PtyPostClearSignalTerm);
    assert_eq!(
        outcome.status_code,
        Some(143),
        "unexpected post-clear status; stdout={} stderr={} terminal_trace={}",
        outcome.stdout,
        outcome.stderr,
        outcome.control_terminal_trace
    );
    assert!(
        outcome.post_reap_restore_marker_survived,
        "post-clear signal did not observe the authenticated marker"
    );
    assert_eq!(outcome.unrelated_process_survived, Some(true));
    assert!(
        !outcome.stale_control_action_recorded,
        "post-clear trap signaled or probed the retired control identity"
    );
    assert!(outcome.control_processes_alive.is_empty());
    assert_pty_foreground_restored(&outcome);
}

#[test]
fn bootstrap_real_pty_barrier_failure_never_hands_off_or_leaves_terminal_or_process_state() {
    let outcome = bootstrap_fixture_outcome(BootstrapScenario::PtyBarrierFailure);
    assert_eq!(outcome.status_code, Some(1));
    assert!(!outcome.control_completion);
    assert!(outcome.control_processes_alive.is_empty());
    assert_eq!(outcome.unrelated_process_survived, Some(true));
    assert_pty_foreground_restored(&outcome);
}

#[test]
fn bootstrap_repeated_signal_cleanup_keeps_first_status_and_removes_private_state() {
    let outcome = bootstrap_fixture_outcome(BootstrapScenario::RepeatedSignalHupThenTerm);
    assert!(!outcome.success);
    assert_eq!(
        outcome.status_code,
        Some(129),
        "the first signal must determine status; stderr: {}",
        outcome.stderr
    );
    assert!(
        outcome.argv.is_empty(),
        "control executed after repeated-signal cancellation"
    );
    assert_eq!(
        outcome.downloads, "install.sh\n",
        "installer continued downloading after repeated-signal cancellation"
    );
    assert!(
        outcome.private_residue.is_empty(),
        "private bootstrap state remained after repeated-signal cancellation: {:?}",
        outcome.private_residue
    );
}

#[test]
fn assembled_release_fixture_is_schema_bound_normalized_and_arch_correct() {
    let Some(directory) = std::env::var_os("PLANERADAR_RELEASE_FIXTURE_DIR").map(PathBuf::from)
    else {
        eprintln!(
            "PLANERADAR_RELEASE_FIXTURE_DIR unset; full package is verified by the release task"
        );
        return;
    };
    let actual = fs::read_dir(&directory)
        .expect("release directory")
        .map(|entry| {
            entry
                .expect("release entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(
        actual,
        RELEASE_ASSETS.into_iter().map(str::to_owned).collect()
    );

    let manifest: Value = serde_json::from_slice(
        &fs::read(directory.join("release-manifest.json")).expect("manifest"),
    )
    .expect("manifest JSON");
    let schema: Value =
        serde_json::from_str(&read("release/release-manifest.schema.json")).expect("schema");
    let validator = jsonschema::options()
        .with_draft(jsonschema::Draft::Draft202012)
        .build(&schema)
        .expect("schema compiles");
    assert!(validator.is_valid(&manifest), "packaged manifest validates");
    for field in [
        "repository",
        "version",
        "commit",
        "manifest_sha256",
        "lifecycle_protocol",
    ] {
        assert_eq!(
            manifest["driver"][field],
            driver_lock_field(field),
            "assembled manifest driver field {field} must match driver.lock.toml"
        );
    }
    let source_epoch = manifest["source_date_epoch"]
        .as_u64()
        .expect("source epoch");

    let sbom: Value =
        serde_json::from_slice(&fs::read(directory.join("SBOM.spdx.json")).expect("SBOM"))
            .expect("SPDX JSON");
    assert_eq!(sbom["spdxVersion"], "SPDX-2.3");
    assert_eq!(sbom["dataLicense"], "CC0-1.0");
    assert!(
        sbom["packages"]
            .as_array()
            .is_some_and(|packages| packages.len() > 10)
    );
    let files = sbom["files"].as_array().expect("SPDX files");
    assert_eq!(files.len(), 3);
    for file in files {
        let algorithms = file["checksums"]
            .as_array()
            .expect("SPDX file checksums")
            .iter()
            .filter_map(|checksum| checksum["algorithm"].as_str())
            .collect::<BTreeSet<_>>();
        assert_eq!(algorithms, BTreeSet::from(["SHA1", "SHA256"]));
        assert!(
            file["fileName"].as_str().is_some_and(
                |name| name.ends_with("/planeradar") || name.ends_with("/planeradarctl")
            ),
            "SPDX must describe packaged executable inputs"
        );
    }

    for (archive, member) in [
        ("planeradar-aarch64-linux-gnu.tar.zst", "planeradar"),
        (
            "planeradarctl-aarch64-apple-darwin.tar.zst",
            "planeradarctl",
        ),
        ("planeradarctl-x86_64-apple-darwin.tar.zst", "planeradarctl"),
    ] {
        let listing = Command::new("tar")
            .args(["-tf", archive])
            .current_dir(&directory)
            .output()
            .expect("archive listing");
        assert!(listing.status.success());
        assert_eq!(String::from_utf8_lossy(&listing.stdout).trim(), member);

        let extraction = tempfile::tempdir().expect("archive metadata extraction");
        let status = Command::new("tar")
            .args(["-xf", archive, "-C"])
            .arg(extraction.path())
            .current_dir(&directory)
            .status()
            .expect("extract normalized metadata");
        assert!(status.success());
        let modified = fs::metadata(extraction.path().join(member))
            .expect("extracted metadata")
            .modified()
            .expect("archive member modification time")
            .duration_since(std::time::UNIX_EPOCH)
            .expect("archive member is after Unix epoch")
            .as_secs();
        assert_eq!(
            modified, source_epoch,
            "archive member timestamp comes from SOURCE_DATE_EPOCH"
        );
    }

    for (archive, architecture) in [
        ("planeradarctl-aarch64-apple-darwin.tar.zst", "arm64"),
        ("planeradarctl-x86_64-apple-darwin.tar.zst", "x86_64"),
    ] {
        let extraction = tempfile::NamedTempFile::new().expect("control extraction");
        let output = Command::new("tar")
            .args(["-xOf", archive, "planeradarctl"])
            .current_dir(&directory)
            .output()
            .expect("extract control");
        assert!(output.status.success());
        fs::write(extraction.path(), output.stdout).expect("extracted control");
        let probe = packaged_macho_architecture(extraction.path());
        assert!(
            probe.status.success(),
            "portable Mach-O verification failed: {}",
            String::from_utf8_lossy(&probe.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&probe.stdout).trim(), architecture);
    }
}

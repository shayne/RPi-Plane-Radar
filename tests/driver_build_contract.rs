use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use sha2::{Digest, Sha256};
use tempfile::TempDir;

fn run_common_script(arguments: &[&str]) -> Output {
    let mut command = Command::new("bash");
    command
        .arg("-c")
        .arg(
            r#"
set -euo pipefail
source scripts/hyperpixel-build-common.sh
"$@"
"#,
        )
        .arg("driver-build-contract");
    command.args(arguments);
    command.output().expect("common script command must run")
}

fn make_executable(path: &Path) {
    let mut permissions = fs::metadata(path)
        .expect("fixture executable metadata must exist")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("fixture must be executable");
}

fn write_minimal_elf64(path: &Path, machine: u16, executable: bool) {
    let mut elf = vec![0_u8; 64];
    elf[..16].copy_from_slice(&[0x7f, b'E', b'L', b'F', 2, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    elf[16..18].copy_from_slice(&2_u16.to_le_bytes());
    elf[18..20].copy_from_slice(&machine.to_le_bytes());
    elf[20..24].copy_from_slice(&1_u32.to_le_bytes());
    elf[52..54].copy_from_slice(&64_u16.to_le_bytes());
    elf[54..56].copy_from_slice(&56_u16.to_le_bytes());
    elf[58..60].copy_from_slice(&64_u16.to_le_bytes());
    fs::write(path, elf).expect("minimal ELF fixture must be written");
    if executable {
        make_executable(path);
    }
}

const KERNEL_BUILDER_IMAGE: &str = "planeradar-kernel-builder:debian-trixie-gcc14";

fn linux_fixture_path(fixture_dir: &Path, host_path: &Path, use_docker: bool) -> PathBuf {
    if use_docker {
        Path::new("/fixtures").join(
            host_path
                .strip_prefix(fixture_dir)
                .expect("fixture path must remain under the mounted directory"),
        )
    } else {
        host_path.to_path_buf()
    }
}

fn run_linux_fixture_command(
    fixture_dir: &Path,
    use_docker: bool,
    script: &str,
    arguments: &[String],
) -> Output {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut command;
    if use_docker {
        command = Command::new("docker");
        command
            .args(["run", "--rm"])
            .arg("--volume")
            .arg(format!("{}:/repo:ro", repository.display()))
            .arg("--volume")
            .arg(format!("{}:/fixtures:rw", fixture_dir.display()))
            .args(["--workdir", "/repo", KERNEL_BUILDER_IMAGE, "bash", "-c"])
            .arg(script)
            .arg("driver-build-contract");
    } else {
        command = Command::new("bash");
        command
            .arg("-c")
            .arg(script)
            .arg("driver-build-contract")
            .current_dir(repository);
    }
    command
        .args(arguments)
        .output()
        .expect("Linux fixture command must run")
}

#[test]
fn release_paths_are_validated_and_contained_by_the_real_helper() {
    let parent = TempDir::new().expect("temporary parent must be created");
    let parent_path = parent
        .path()
        .to_str()
        .expect("temporary path must be UTF-8");
    let valid = run_common_script(&["hp2r_release_path", parent_path, "6.18.34+rpt-rpi-v8"]);
    assert!(
        valid.status.success(),
        "valid release rejected: {}",
        String::from_utf8_lossy(&valid.stderr)
    );
    assert_eq!(
        String::from_utf8(valid.stdout)
            .expect("path output must be UTF-8")
            .trim(),
        fs::canonicalize(parent.path())
            .expect("temporary parent must canonicalize")
            .join("6.18.34+rpt-rpi-v8")
            .display()
            .to_string()
    );

    fs::create_dir(parent.path().join("existing-release"))
        .expect("existing release directory must be created");
    let existing = run_common_script(&["hp2r_release_path", parent_path, "existing-release"]);
    assert!(
        existing.status.success(),
        "existing in-parent release rejected: {}",
        String::from_utf8_lossy(&existing.stderr)
    );

    for invalid in ["", ".", "..", "../escape", "nested/release"] {
        let output = run_common_script(&["hp2r_release_path", parent_path, invalid]);
        assert!(
            !output.status.success(),
            "unsafe release was accepted: {invalid}"
        );
    }
}

#[test]
fn release_path_rejects_a_symlink_that_resolves_outside_the_fixed_parent() {
    let temporary = TempDir::new().expect("temporary directory must be created");
    let parent = temporary.path().join("parent");
    let outside = temporary.path().join("outside");
    fs::create_dir(&parent).expect("parent directory must be created");
    fs::create_dir(&outside).expect("outside directory must be created");
    symlink(&outside, parent.join("6.18.34+rpt-rpi-v8")).expect("release symlink must be created");

    let output = run_common_script(&[
        "hp2r_release_path",
        parent.to_str().expect("parent path must be UTF-8"),
        "6.18.34+rpt-rpi-v8",
    ]);
    assert!(
        !output.status.success(),
        "outside-resolving release symlink was accepted as {}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("symlinked kernel release destination"),
        "symlink rejection was not explicit: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn checksum_helper_accepts_only_the_fresh_file_digest() {
    let temporary = TempDir::new().expect("temporary directory must be created");
    let file = temporary.path().join("base.dtb");
    fs::write(&file, b"transferred DTB bytes").expect("fixture must be written");
    let expected = format!("{:x}", Sha256::digest(b"transferred DTB bytes"));

    let valid = run_common_script(&[
        "hp2r_verify_sha256",
        file.to_str().expect("fixture path must be UTF-8"),
        &expected,
    ]);
    assert!(
        valid.status.success(),
        "fresh checksum rejected: {}",
        String::from_utf8_lossy(&valid.stderr)
    );

    let invalid = run_common_script(&[
        "hp2r_verify_sha256",
        file.to_str().expect("fixture path must be UTF-8"),
        &"0".repeat(64),
    ]);
    assert!(!invalid.status.success(), "stale checksum was accepted");
}

#[test]
fn source_identity_rejects_dirty_stale_and_mismatched_overlay_provenance() {
    let revision = "1".repeat(40);
    let tree = "2".repeat(40);
    let valid_overlay = format!("planeradar-hyperpixel2r-{}.dtbo", &revision[..12]);
    let cases = [
        (
            "true",
            revision.as_str(),
            tree.as_str(),
            valid_overlay.as_str(),
            "driver artifacts require clean tracked source",
        ),
        (
            "false",
            "0000000000000000000000000000000000000000",
            tree.as_str(),
            "planeradar-hyperpixel2r-000000000000.dtbo",
            "source revision does not match checked source",
        ),
        (
            "false",
            revision.as_str(),
            "0000000000000000000000000000000000000000",
            valid_overlay.as_str(),
            "source tree does not match checked source",
        ),
        (
            "false",
            revision.as_str(),
            tree.as_str(),
            "planeradar-hyperpixel2r-000000000000.dtbo",
            "overlay filename does not match source revision",
        ),
    ];
    for (dirty, source_revision, source_tree, overlay_file, expected_error) in cases {
        let output = run_common_script(&[
            "hp2r_validate_source_identity",
            dirty,
            source_revision,
            source_tree,
            overlay_file,
            &revision,
            &tree,
        ]);
        assert!(!output.status.success(), "invalid provenance was accepted");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(expected_error),
            "provenance failed for the wrong reason: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn app_build_uses_the_clean_workspace_head_identity_that_the_driver_manifest_records() {
    let temporary = TempDir::new().expect("temporary directory must be created");
    let fake_bin = temporary.path().join("bin");
    fs::create_dir(&fake_bin).expect("fake bin directory must be created");
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
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    let synthesized_revision = "1".repeat(40);
    let stack_revision = "2".repeat(40);
    let source_tree = "3".repeat(40);
    fs::create_dir_all(temporary.path().join("dist/hyperpixel/test"))
        .expect("driver artifact directory must be created");
    fs::write(
        temporary.path().join("dist/hyperpixel/test/manifest.txt"),
        format!("source_revision\t{synthesized_revision}\nsource_tree\t{source_tree}\n"),
    )
    .expect("driver manifest must be written");

    fs::write(
        fake_bin.join("git"),
        format!(
            r#"#!/usr/bin/env bash
set -euo pipefail
case "$*" in
  "status --porcelain") exit 0 ;;
  "diff-index --quiet HEAD --") exit 0 ;;
  "rev-parse HEAD") printf '%s\n' '{synthesized_revision}' ;;
  "rev-parse HEAD^{{tree}}") printf '%s\n' '{source_tree}' ;;
  "rev-parse --verify rpi-port^{{commit}}") printf '%s\n' '{stack_revision}' ;;
  "archive --format=tar HEAD") exec '{real_git}' -C '{repository}' archive --format=tar HEAD ;;
  *) printf 'unexpected git command: %s\n' "$*" >&2; exit 64 ;;
esac
"#,
            real_git = real_git,
            repository = repository.display(),
        ),
    )
    .expect("fake git must be written");
    fs::write(
        fake_bin.join("docker"),
        r#"#!/usr/bin/env bash
set -euo pipefail
case "${1-}" in
  info) exit 0 ;;
  buildx)
    printf '%s\n' "$*" > docker-arguments.txt
    mkdir -p dist
    printf app > dist/planeradar
    printf 'Machine: AArch64\n' > dist/planeradar.readelf.txt
    ;;
  *) exit 64 ;;
esac
"#,
    )
    .expect("fake docker must be written");
    fs::write(
        fake_bin.join("file"),
        "#!/usr/bin/env bash\nprintf 'dist/planeradar: ELF 64-bit ARM aarch64\\n'\n",
    )
    .expect("fake file must be written");
    for executable in ["git", "docker", "file"] {
        let path = fake_bin.join(executable);
        let mut permissions = fs::metadata(&path)
            .expect("fake executable metadata must exist")
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions).expect("fake executable must be executable");
    }

    let output = Command::new("bash")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/build-pi.sh"))
        .current_dir(temporary.path())
        .env(
            "PATH",
            format!(
                "{}:{}",
                fake_bin.display(),
                std::env::var("PATH").expect("PATH must be set")
            ),
        )
        .output()
        .expect("app build script must run");
    assert!(
        output.status.success(),
        "app build failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let manifest = fs::read_to_string(temporary.path().join("dist/hyperpixel/test/manifest.txt"))
        .expect("driver manifest must remain readable");
    let driver_revision = manifest
        .lines()
        .find_map(|line| line.strip_prefix("source_revision\t"))
        .expect("driver revision");
    let driver_tree = manifest
        .lines()
        .find_map(|line| line.strip_prefix("source_tree\t"))
        .expect("driver tree");
    assert_eq!(
        fs::read_to_string(temporary.path().join("dist/planeradar.revision"))
            .expect("app revision")
            .trim(),
        driver_revision
    );
    assert_eq!(
        fs::read_to_string(temporary.path().join("dist/planeradar.tree"))
            .expect("app source tree")
            .trim(),
        driver_tree
    );
    let docker_arguments =
        fs::read_to_string(temporary.path().join("docker-arguments.txt")).expect("docker args");
    assert!(docker_arguments.contains(&format!(
        "--build-arg PLANERADAR_REVISION={synthesized_revision}"
    )));
    assert!(!docker_arguments.contains(&stack_revision));
}

#[test]
fn app_build_context_excludes_untracked_build_affecting_inputs() {
    let temporary = TempDir::new().expect("temporary directory must be created");
    let fake_bin = temporary.path().join("bin");
    fs::create_dir(&fake_bin).expect("fake bin directory must be created");
    fs::create_dir_all(temporary.path().join("packaging"))
        .expect("packaging directory must be created");
    fs::write(
        temporary.path().join("packaging/Dockerfile.build"),
        "FROM scratch\n",
    )
    .expect("tracked Dockerfile must be written");
    fs::write(temporary.path().join("tracked.txt"), "tracked\n")
        .expect("tracked sentinel must be written");

    let run_git = |arguments: &[&str]| {
        let output = Command::new("git")
            .args(arguments)
            .current_dir(temporary.path())
            .output()
            .expect("git command must run");
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            arguments,
            String::from_utf8_lossy(&output.stderr)
        );
    };
    run_git(&["init", "-q"]);
    run_git(&["config", "user.email", "fixture@example.invalid"]);
    run_git(&["config", "user.name", "Fixture"]);
    run_git(&["add", "packaging/Dockerfile.build", "tracked.txt"]);
    run_git(&["commit", "-qm", "fixture"]);

    fs::create_dir(temporary.path().join(".cargo"))
        .expect("untracked cargo directory must be created");
    fs::write(
        temporary.path().join(".cargo/config.toml"),
        "[build]\nrustflags = [\"--cfg\", \"untracked_input\"]\n",
    )
    .expect("untracked cargo config must be written");
    fs::write(
        temporary.path().join("build.rs"),
        "fn main() { println!(\"cargo:rustc-cfg=untracked_input\"); }\n",
    )
    .expect("untracked build script must be written");

    fs::write(
        fake_bin.join("docker"),
        r#"#!/usr/bin/env bash
set -euo pipefail
case "${1-}" in
  info) exit 0 ;;
  buildx)
    context="${@: -1}"
    test "$context" != "."
    test -f "$context/tracked.txt"
    test ! -e "$context/.cargo/config.toml"
    test ! -e "$context/build.rs"
    printf '%s\n' "$context" > docker-context.txt
    mkdir -p dist
    printf app > dist/planeradar
    printf 'Machine: AArch64\n' > dist/planeradar.readelf.txt
    ;;
  *) exit 64 ;;
esac
"#,
    )
    .expect("fake docker must be written");
    fs::write(
        fake_bin.join("file"),
        "#!/usr/bin/env bash\nprintf 'dist/planeradar: ELF 64-bit ARM aarch64\\n'\n",
    )
    .expect("fake file must be written");
    for executable in ["docker", "file"] {
        let path = fake_bin.join(executable);
        let mut permissions = fs::metadata(&path)
            .expect("fake executable metadata must exist")
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions).expect("fake executable must be executable");
    }

    let output = Command::new("bash")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/build-pi.sh"))
        .current_dir(temporary.path())
        .env(
            "PATH",
            format!(
                "{}:{}",
                fake_bin.display(),
                std::env::var("PATH").expect("PATH must be set")
            ),
        )
        .output()
        .expect("app build script must run");
    assert!(
        output.status.success(),
        "untracked inputs reached the app build context:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        temporary.path().join("docker-context.txt").is_file(),
        "Docker never received an isolated build context"
    );
}

#[test]
fn build_rejects_dirty_tracked_source_before_compilation() {
    let temporary = TempDir::new().expect("temporary directory must be created");
    let release = "6.18.34+rpt-rpi-v8";
    let target_dir = temporary.path().join("dist/kernel-target").join(release);
    let target_root = target_dir.join("root");
    for directory in [
        target_root.join("headers"),
        target_root.join("common/include"),
        target_root.join("usr/src"),
        target_root.join("kbuild"),
        target_root.join("boot"),
    ] {
        fs::create_dir_all(directory).expect("target directory must be created");
    }
    fs::write(
        target_root.join("headers/.config"),
        concat!(
            "CONFIG_DRM_PANEL=y\n",
            "CONFIG_I2C_ALGOBIT=m\n",
            "CONFIG_TOUCHSCREEN_EDT_FT5X06=m\n",
            "CONFIG_OF_OVERLAY=y\n",
            "CONFIG_DRM_VC4=m\n",
            "CONFIG_DRM_V3D=m\n",
        ),
    )
    .expect("target config must be written");
    fs::write(target_root.join("headers/Module.symvers"), "")
        .expect("Module.symvers must be written");
    fs::write(target_root.join("boot/base.dtb"), "base").expect("base DTB must be written");
    fs::write(
        target_dir.join("target.txt"),
        format!(
            "kernel_release\t{release}\n\
             kernel_arch\taarch64\n\
             header_path\t/headers\n\
             common_header_path\t/common\n\
             kbuild_path\t/kbuild\n\
             base_dtb_path\t/boot/base.dtb\n\
             base_dtb_sha256\t{}\n",
            "0".repeat(64)
        ),
    )
    .expect("target manifest must be written");
    fs::create_dir(temporary.path().join("kernel")).expect("kernel source must be created");
    fs::write(temporary.path().join("kernel/source.c"), "dirty source")
        .expect("kernel source must be written");

    let fake_bin = temporary.path().join("bin");
    fs::create_dir(&fake_bin).expect("fake bin directory must be created");
    fs::write(
        fake_bin.join("ssh"),
        format!("#!/usr/bin/env bash\nprintf '{release}\\n'\n"),
    )
    .expect("fake ssh must be written");
    fs::write(
        fake_bin.join("git"),
        r#"#!/usr/bin/env bash
set -eu
case "${1-} ${2-}" in
  "rev-parse HEAD") printf '%040d\n' 1 ;;
  "rev-parse HEAD^{tree}") printf '%040d\n' 2 ;;
  "diff-index --quiet") exit 1 ;;
  "cat-file -e") exit 0 ;;
  "status --porcelain") printf ' M kernel/source.c\n' ;;
  *) exit 2 ;;
esac
"#,
    )
    .expect("fake git must be written");
    fs::write(
        fake_bin.join("docker"),
        r#"#!/usr/bin/env bash
set -eu
case "${1-}" in
  info|buildx) exit 0 ;;
  run)
    shift
    build=
    while test "$#" -gt 0; do
      if test "$1" = --volume; then
        case "$2" in
          *:/build) build="${2%:/build}" ;;
        esac
        shift 2
      else
        shift
      fi
    done
    test -n "$build"
    printf module > "$build/kernel/planeradar_hyperpixel2r.ko"
    printf file > "$build/kernel/module.file.txt"
    printf readelf > "$build/kernel/module.readelf.txt"
    printf 'vermagic: fake\nlicense: GPL\n' > "$build/kernel/module.modinfo.txt"
    printf 'hash  planeradar_hyperpixel2r.ko\n' > "$build/kernel/module.sha256"
    printf overlay > "$build/out/planeradar-hyperpixel2r-000000000000.dtbo"
    printf applied > "$build/out/planeradar-hyperpixel2r-applied.dtb"
    ;;
  *) exit 2 ;;
esac
"#,
    )
    .expect("fake docker must be written");
    for executable in ["ssh", "git", "docker"] {
        let path = fake_bin.join(executable);
        let mut permissions = fs::metadata(&path)
            .expect("fake executable metadata must exist")
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions).expect("fake executable must be executable");
    }

    let output = Command::new("bash")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/build-hyperpixel-driver.sh"))
        .current_dir(temporary.path())
        .env(
            "PATH",
            format!(
                "{}:{}",
                fake_bin.display(),
                std::env::var("PATH").expect("PATH must be set")
            ),
        )
        .output()
        .expect("build script must run");
    assert!(
        !output.status.success(),
        "dirty tracked source was compiled"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("tracked source is dirty"),
        "dirty build failed for the wrong reason: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn checker_rejects_dot_release_before_constructing_an_artifact_path() {
    let temporary = TempDir::new().expect("temporary directory must be created");
    let fake_bin = temporary.path().join("bin");
    fs::create_dir(&fake_bin).expect("fake bin directory must be created");
    let fake_ssh = fake_bin.join("ssh");
    fs::write(&fake_ssh, "#!/usr/bin/env bash\nprintf '..\\n'\n")
        .expect("fake ssh must be written");
    let mut permissions = fs::metadata(&fake_ssh)
        .expect("fake ssh metadata must exist")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&fake_ssh, permissions).expect("fake ssh must be executable");

    let output = Command::new("bash")
        .arg("scripts/check-hyperpixel-artifacts.sh")
        .env(
            "PATH",
            format!(
                "{}:{}",
                fake_bin.display(),
                std::env::var("PATH").expect("PATH must be set")
            ),
        )
        .output()
        .expect("checker must run");

    assert!(!output.status.success(), "dot release was accepted");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("unsafe kernel release returned by target"),
        "checker reached an unsafe artifact path instead: {stderr}"
    );
    assert!(!stderr.contains("hyperpixel/../"));
}

#[test]
fn checker_rejects_an_artifact_release_symlink_before_reading_through_it() {
    let temporary = TempDir::new().expect("temporary directory must be created");
    let artifact_parent = temporary.path().join("dist/hyperpixel");
    let outside = temporary.path().join("outside");
    fs::create_dir_all(&artifact_parent).expect("artifact parent must be created");
    fs::create_dir(&outside).expect("outside directory must be created");
    symlink(&outside, artifact_parent.join("6.18.34+rpt-rpi-v8"))
        .expect("artifact symlink must be created");

    let fake_bin = temporary.path().join("bin");
    fs::create_dir(&fake_bin).expect("fake bin directory must be created");
    let fake_ssh = fake_bin.join("ssh");
    fs::write(
        &fake_ssh,
        "#!/usr/bin/env bash\nprintf '6.18.34+rpt-rpi-v8\\n'\n",
    )
    .expect("fake ssh must be written");
    let mut permissions = fs::metadata(&fake_ssh)
        .expect("fake ssh metadata must exist")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&fake_ssh, permissions).expect("fake ssh must be executable");

    let output = Command::new("bash")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/check-hyperpixel-artifacts.sh"))
        .current_dir(temporary.path())
        .env(
            "PATH",
            format!(
                "{}:{}",
                fake_bin.display(),
                std::env::var("PATH").expect("PATH must be set")
            ),
        )
        .output()
        .expect("checker must run");

    assert!(!output.status.success(), "artifact symlink was accepted");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("symlinked kernel release destination"),
        "checker read through the symlink instead: {stderr}"
    );
    assert!(!stderr.contains("missing driver artifact"));
}

#[test]
fn build_rejects_an_artifact_release_symlink_before_removing_through_it() {
    let temporary = TempDir::new().expect("temporary directory must be created");
    let release = "6.18.34+rpt-rpi-v8";
    let target_dir = temporary.path().join("dist/kernel-target").join(release);
    let target_root = target_dir.join("root");
    fs::create_dir_all(target_root.join("headers")).expect("headers must be created");
    fs::create_dir_all(target_root.join("usr/src")).expect("usr/src must be created");
    fs::create_dir_all(target_root.join("kbuild")).expect("kbuild must be created");
    fs::write(
        target_root.join("headers/.config"),
        concat!(
            "CONFIG_DRM_PANEL=y\n",
            "CONFIG_I2C_ALGOBIT=m\n",
            "CONFIG_TOUCHSCREEN_EDT_FT5X06=m\n",
            "CONFIG_OF_OVERLAY=y\n",
            "CONFIG_DRM_VC4=m\n",
            "CONFIG_DRM_V3D=m\n",
        ),
    )
    .expect("target config must be written");
    fs::write(target_root.join("headers/Module.symvers"), "")
        .expect("Module.symvers must be written");
    fs::write(
        target_dir.join("target.txt"),
        format!(
            "kernel_release\t{release}\n\
             kernel_arch\taarch64\n\
             header_path\t/headers\n\
             kbuild_path\t/kbuild\n\
             base_dtb_sha256\t{}\n",
            "0".repeat(64)
        ),
    )
    .expect("target manifest must be written");
    fs::create_dir(temporary.path().join("kernel")).expect("kernel source must be created");

    let artifact_parent = temporary.path().join("dist/hyperpixel");
    let outside = temporary.path().join("outside");
    fs::create_dir_all(&artifact_parent).expect("artifact parent must be created");
    fs::create_dir(&outside).expect("outside directory must be created");
    let sentinel = outside.join("planeradar_hyperpixel2r.ko");
    fs::write(&sentinel, "must survive").expect("outside sentinel must be written");
    symlink(&outside, artifact_parent.join(release)).expect("artifact symlink must be created");

    let fake_bin = temporary.path().join("bin");
    fs::create_dir(&fake_bin).expect("fake bin directory must be created");
    let fake_ssh = fake_bin.join("ssh");
    fs::write(
        &fake_ssh,
        format!("#!/usr/bin/env bash\nprintf '{release}\\n'\n"),
    )
    .expect("fake ssh must be written");
    let fake_docker = fake_bin.join("docker");
    fs::write(&fake_docker, "#!/usr/bin/env bash\nexit 0\n").expect("fake docker must be written");
    let fake_git = fake_bin.join("git");
    fs::write(
        &fake_git,
        r#"#!/usr/bin/env bash
set -eu
case "${1-} ${2-}" in
  "diff-index --quiet"|"cat-file -e") exit 0 ;;
  "rev-parse HEAD") printf '%040d\n' 1 ;;
  "rev-parse HEAD^{tree}") printf '%040d\n' 2 ;;
  *) exit 2 ;;
esac
"#,
    )
    .expect("fake git must be written");
    for executable in [&fake_ssh, &fake_docker, &fake_git] {
        let mut permissions = fs::metadata(executable)
            .expect("fake executable metadata must exist")
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(executable, permissions).expect("fake executable must be executable");
    }

    let output = Command::new("bash")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/build-hyperpixel-driver.sh"))
        .current_dir(temporary.path())
        .env(
            "PATH",
            format!(
                "{}:{}",
                fake_bin.display(),
                std::env::var("PATH").expect("PATH must be set")
            ),
        )
        .output()
        .expect("build script must run");

    assert!(!output.status.success(), "artifact symlink was accepted");
    assert!(
        sentinel.exists(),
        "build removed an artifact through the outside-resolving symlink"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("symlinked kernel release destination"),
        "build reached the symlinked destination instead: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn export_rejects_a_transferred_dtb_that_does_not_match_live_metadata() {
    let temporary = TempDir::new().expect("temporary directory must be created");
    let fake_root = temporary.path().join("target-root");
    let header = fake_root.join("usr/src/linux-headers-test");
    let common = fake_root.join("usr/src/linux-headers-common");
    let kbuild = fake_root.join("usr/lib/linux-kbuild-test");
    let boot = fake_root.join("boot/firmware");
    for directory in [&header, &common, &kbuild, &boot] {
        fs::create_dir_all(directory).expect("fake target directory must be created");
    }
    fs::write(header.join(".config"), "CONFIG_DRM_PANEL=y\n").expect("config must be written");
    fs::write(header.join("Module.symvers"), "").expect("symvers must be written");
    fs::write(boot.join("bcm2710-rpi-zero-2-w.dtb"), b"transferred bytes")
        .expect("DTB must be written");

    let fake_bin = temporary.path().join("bin");
    fs::create_dir(&fake_bin).expect("fake bin directory must be created");
    let fake_ssh = fake_bin.join("ssh");
    fs::write(
        &fake_ssh,
        r#"#!/usr/bin/env bash
set -euo pipefail
if test "${2:-}" = bash; then
  printf 'kernel_release\ttest\n'
  printf 'kernel_arch\taarch64\n'
  printf 'header_path\t/usr/src/linux-headers-test\n'
  printf 'common_header_path\t/usr/src/linux-headers-common\n'
  printf 'kbuild_path\t/usr/lib/linux-kbuild-test\n'
  printf 'kernel_source_package\tlinux\n'
  printf 'kernel_source_version\t1:6.18.34-1+rpt1\n'
  printf 'kernel_source_deb_package\tlinux-source-6.18\n'
  printf 'kernel_source_deb_filename\tpool/main/l/linux/linux-source.deb\n'
  printf 'kernel_source_deb_sha256\t%s\n' '1111111111111111111111111111111111111111111111111111111111111111'
  printf 'base_dtb_path\t/boot/firmware/bcm2710-rpi-zero-2-w.dtb\n'
  printf 'base_dtb_sha256\t%s\n' '0000000000000000000000000000000000000000000000000000000000000000'
else
  /usr/bin/tar -C "$FAKE_TARGET_ROOT" -cf - \
    usr/src/linux-headers-test \
    usr/src/linux-headers-common \
    usr/lib/linux-kbuild-test \
    boot/firmware/bcm2710-rpi-zero-2-w.dtb
fi
"#,
    )
    .expect("fake ssh must be written");
    let mut permissions = fs::metadata(&fake_ssh)
        .expect("fake ssh metadata must exist")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&fake_ssh, permissions).expect("fake ssh must be executable");

    let output = Command::new("bash")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/export-pi-kernel-build.sh"))
        .current_dir(temporary.path())
        .env("PLANERADAR_PI_TARGET", "fake-target")
        .env("FAKE_TARGET_ROOT", &fake_root)
        .env(
            "PATH",
            format!(
                "{}:{}",
                fake_bin.display(),
                std::env::var("PATH").expect("PATH must be set")
            ),
        )
        .output()
        .expect("export script must run");

    assert!(
        !output.status.success(),
        "export published a DTB with a mismatched checksum"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("base DTB checksum"),
        "export failed for the wrong reason: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn host_tools_are_rebuilt_from_matching_source_instead_of_reusing_target_binaries() {
    let temporary = TempDir::new().expect("temporary directory must be created");
    let use_docker = !cfg!(target_os = "linux");
    if use_docker {
        let available = Command::new("docker")
            .args(["image", "inspect", KERNEL_BUILDER_IMAGE])
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false);
        if !available {
            eprintln!(
                "skipping macOS source-derived ELF preparation: build {KERNEL_BUILDER_IMAGE} first"
            );
            return;
        }
    }

    let architecture = run_linux_fixture_command(temporary.path(), use_docker, "uname -m", &[]);
    assert!(
        architecture.status.success(),
        "native architecture probe failed"
    );
    let (expected_machine, wrong_machine, wrong_machine_name) =
        match String::from_utf8_lossy(&architecture.stdout).trim() {
            "arm64" | "aarch64" => ("AArch64", 62, "Advanced Micro Devices X86-64"),
            "x86_64" | "amd64" => ("Advanced Micro Devices X86-64", 183, "AArch64"),
            other => panic!("unsupported Linux fixture architecture: {other}"),
        };

    let source_deb = temporary.path().join("linux-source.deb");
    let source_bytes = b"exact source package";
    fs::write(&source_deb, source_bytes).expect("source package fixture must be written");
    let source_sha256 = format!("{:x}", Sha256::digest(source_bytes));
    let target_config = temporary.path().join("target.config");
    fs::write(&target_config, "CONFIG_MODVERSIONS=y\n")
        .expect("target config fixture must be written");
    let exported_kbuild = temporary.path().join("exported-kbuild");
    for helper in [
        "scripts/basic/fixdep",
        "scripts/mod/modpost",
        "scripts/genksyms/genksyms",
    ] {
        let path = exported_kbuild.join(helper);
        fs::create_dir_all(path.parent().expect("helper must have a parent"))
            .expect("exported helper parent must be created");
        write_minimal_elf64(&path, wrong_machine, true);
    }

    let fake_bin = temporary.path().join("bin");
    fs::create_dir(&fake_bin).expect("fake bin directory must be created");
    fs::write(
        fake_bin.join("dpkg-deb"),
        r#"#!/usr/bin/env bash
set -euo pipefail
case "$1" in
  -f)
    case "$3" in
      Package) printf 'linux-source-6.18\n' ;;
      Version) printf '1:6.18.34-1+rpt1\n' ;;
      Architecture) printf 'all\n' ;;
      *) exit 64 ;;
    esac
    ;;
  -x)
    mkdir -p "$3/usr/src"
    printf archive > "$3/usr/src/linux-source-6.18.tar.xz"
    ;;
  *) exit 64 ;;
esac
"#,
    )
    .expect("fake dpkg-deb must be written");
    fs::write(
        fake_bin.join("tar"),
        r#"#!/usr/bin/env bash
set -euo pipefail
destination=
while test "$#" -gt 0; do
  if test "$1" = -C; then
    destination="$2"
    shift 2
  else
    shift
  fi
done
test -n "$destination"
mkdir -p "$destination"
printf source > "$destination/Makefile"
cat > "$destination/fixture-fixdep.c" <<'SOURCE'
int main(void) { return 1; }
SOURCE
cat > "$destination/fixture-helper.c" <<'SOURCE'
int main(void) { return 0; }
SOURCE
"#,
    )
    .expect("fake tar must be written");
    fs::write(
        fake_bin.join("make"),
        r#"#!/usr/bin/env bash
set -euo pipefail
output=
source_root=
arguments=("$@")
for ((index = 0; index < ${#arguments[@]}; index++)); do
  case "${arguments[index]}" in
    -C) source_root="${arguments[index + 1]}" ;;
    O=*) output="${arguments[index]#O=}" ;;
  esac
done
test -n "$output" -a -n "$source_root"
case " $* " in
  *" prepare "*)
    mkdir -p \
      "$output/scripts/basic" \
      "$output/scripts/mod" \
      "$output/scripts/genksyms"
    "$HOSTCC" "$source_root/fixture-fixdep.c" \
      -o "$output/scripts/basic/fixdep"
    "$HOSTCC" "$source_root/fixture-helper.c" \
      -o "$output/scripts/mod/modpost"
    "$HOSTCC" "$source_root/fixture-helper.c" \
      -o "$output/scripts/genksyms/genksyms"
    case "${PLANERADAR_TEST_HELPER_MODE:-valid}" in
      valid) ;;
      wrong-architecture)
        cp "$PLANERADAR_TEST_WRONG_ARCH" "$output/scripts/basic/fixdep"
        ;;
      nonexecutable)
        chmod 0644 "$output/scripts/basic/fixdep"
        ;;
      missing-loader)
        "$HOSTCC" "$source_root/fixture-fixdep.c" \
          -Wl,--dynamic-linker=/planeradar/missing-loader \
          -o "$output/scripts/basic/fixdep"
        ;;
      *) exit 64 ;;
    esac
    ;;
esac
"#,
    )
    .expect("fake make must be written");
    for executable in ["dpkg-deb", "tar", "make"] {
        make_executable(&fake_bin.join(executable));
    }

    let output_kbuild = temporary.path().join("output-kbuild");
    let work_dir = temporary.path().join("host-build");
    let prepare = |mode: &str, output_path: &Path, work_path: &Path| {
        let runner_path = |path: &Path| {
            linux_fixture_path(temporary.path(), path, use_docker)
                .display()
                .to_string()
        };
        run_linux_fixture_command(
            temporary.path(),
            use_docker,
            r#"
set -euo pipefail
export PLANERADAR_TEST_HELPER_MODE="$1"
export PATH="$2:$PATH"
export PLANERADAR_TEST_WRONG_ARCH="$3"
export HOSTCC=cc
exec scripts/prepare-kbuild-host-tools.sh \
  "$4" "$5" "$6" "$7" "$8" "$9" "${10}" "${11}"
"#,
            &[
                mode.to_owned(),
                runner_path(&fake_bin),
                runner_path(&exported_kbuild.join("scripts/basic/fixdep")),
                runner_path(&source_deb),
                source_sha256.clone(),
                "linux-source-6.18".to_owned(),
                "1:6.18.34-1+rpt1".to_owned(),
                runner_path(&target_config),
                runner_path(&exported_kbuild),
                runner_path(output_path),
                runner_path(work_path),
            ],
        )
    };

    let output = prepare("valid", &output_kbuild, &work_dir);
    assert!(
        output.status.success(),
        "host-tool preparation failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let exported_header = run_linux_fixture_command(
        temporary.path(),
        use_docker,
        "readelf -h \"$1\"",
        &[linux_fixture_path(
            temporary.path(),
            &exported_kbuild.join("scripts/basic/fixdep"),
            use_docker,
        )
        .display()
        .to_string()],
    );
    assert!(
        exported_header.status.success()
            && String::from_utf8_lossy(&exported_header.stdout).contains(wrong_machine_name),
        "exported target helper is not an actual wrong-architecture ELF"
    );
    let rebuilt_validation = run_linux_fixture_command(
        temporary.path(),
        use_docker,
        r#"
set -euo pipefail
source scripts/hyperpixel-build-common.sh
hp2r_validate_host_helper "$1" "$2" 1
"#,
        &[
            linux_fixture_path(
                temporary.path(),
                &output_kbuild.join("scripts/basic/fixdep"),
                use_docker,
            )
            .display()
            .to_string(),
            expected_machine.to_owned(),
        ],
    );
    assert!(
        rebuilt_validation.status.success(),
        "fresh source-derived host helper was not runnable: {}",
        String::from_utf8_lossy(&rebuilt_validation.stderr)
    );
    assert_ne!(
        fs::read(exported_kbuild.join("scripts/basic/fixdep")).expect("target helper bytes"),
        fs::read(output_kbuild.join("scripts/basic/fixdep")).expect("rebuilt helper bytes"),
        "output reused the target-native helper instead of the fresh source-derived ELF"
    );

    let stale_attempt = prepare("valid", &output_kbuild, &work_dir);
    assert!(
        !stale_attempt.status.success(),
        "pre-existing host-tool output was reused"
    );
    assert!(
        String::from_utf8_lossy(&stale_attempt.stderr).contains("must not already exist"),
        "stale output failed for the wrong reason: {}",
        String::from_utf8_lossy(&stale_attempt.stderr)
    );

    for (index, (mode, expected_error)) in [
        ("wrong-architecture", "wrong architecture"),
        ("nonexecutable", "not executable"),
        ("missing-loader", "not executable in the build container"),
    ]
    .into_iter()
    .enumerate()
    {
        let invalid_output = prepare(
            mode,
            &temporary.path().join(format!("invalid-output-{index}")),
            &temporary.path().join(format!("invalid-work-{index}")),
        );
        assert!(
            !invalid_output.status.success(),
            "preparation accepted {mode} host helper output"
        );
        assert!(
            String::from_utf8_lossy(&invalid_output.stderr).contains(expected_error),
            "{mode} preparation failed for the wrong reason: {}",
            String::from_utf8_lossy(&invalid_output.stderr)
        );
    }
}

#[test]
fn helper_validation_uses_real_elf_architecture_permissions_and_loader_behavior() {
    let temporary = TempDir::new().expect("temporary directory must be created");
    let valid = temporary.path().join("valid-fixdep");
    let missing_loader = temporary.path().join("missing-loader-fixdep");
    let source = temporary.path().join("fixdep.c");
    fs::write(&source, "int main(void) { return 1; }\n")
        .expect("native helper source fixture must be written");

    let use_docker = !cfg!(target_os = "linux");
    if use_docker {
        let available = Command::new("docker")
            .args(["image", "inspect", KERNEL_BUILDER_IMAGE])
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false);
        if !available {
            eprintln!("skipping macOS real-ELF validation: build {KERNEL_BUILDER_IMAGE} first");
            return;
        }
    }

    let source_in_runner = linux_fixture_path(temporary.path(), &source, use_docker);
    let valid_in_runner = linux_fixture_path(temporary.path(), &valid, use_docker);
    let missing_loader_in_runner =
        linux_fixture_path(temporary.path(), &missing_loader, use_docker);
    let compile = run_linux_fixture_command(
        temporary.path(),
        use_docker,
        r#"
set -euo pipefail
cc "$1" -o "$2"
cc "$1" -Wl,--dynamic-linker=/planeradar/missing-loader -o "$3"
"#,
        &[
            source_in_runner.display().to_string(),
            valid_in_runner.display().to_string(),
            missing_loader_in_runner.display().to_string(),
        ],
    );
    assert!(
        compile.status.success(),
        "native ELF fixture compilation failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let architecture = run_linux_fixture_command(temporary.path(), use_docker, "uname -m", &[]);
    assert!(
        architecture.status.success(),
        "native architecture probe failed"
    );
    let (expected_machine, wrong_machine, wrong_machine_name) =
        match String::from_utf8_lossy(&architecture.stdout).trim() {
            "arm64" | "aarch64" => ("AArch64", 62, "Advanced Micro Devices X86-64"),
            "x86_64" | "amd64" => ("Advanced Micro Devices X86-64", 183, "AArch64"),
            other => panic!("unsupported Linux fixture architecture: {other}"),
        };

    let wrong_arch = temporary.path().join("wrong-architecture-fixdep");
    write_minimal_elf64(&wrong_arch, wrong_machine, true);
    let nonexecutable = temporary.path().join("nonexecutable-fixdep");
    fs::copy(&valid, &nonexecutable).expect("copy non-executable native ELF fixture");
    fs::set_permissions(&nonexecutable, fs::Permissions::from_mode(0o644))
        .expect("remove native ELF executable permission");
    let malformed = temporary.path().join("malformed-fixdep");
    fs::write(&malformed, b"\x7fELF\x02\x01\x01").expect("malformed ELF fixture must be written");
    make_executable(&malformed);

    let wrong_in_runner = linux_fixture_path(temporary.path(), &wrong_arch, use_docker);
    let wrong_header = run_linux_fixture_command(
        temporary.path(),
        use_docker,
        "readelf -h \"$1\"",
        &[wrong_in_runner.display().to_string()],
    );
    assert!(
        wrong_header.status.success()
            && String::from_utf8_lossy(&wrong_header.stdout).contains(wrong_machine_name),
        "wrong-architecture fixture is not a valid inspected ELF:\n{}",
        String::from_utf8_lossy(&wrong_header.stderr)
    );
    let missing_loader_header = run_linux_fixture_command(
        temporary.path(),
        use_docker,
        "readelf -l \"$1\"",
        &[missing_loader_in_runner.display().to_string()],
    );
    assert!(
        missing_loader_header.status.success()
            && String::from_utf8_lossy(&missing_loader_header.stdout)
                .contains("/planeradar/missing-loader"),
        "missing-loader fixture does not contain the intended ELF interpreter"
    );

    let validate = |fixture: &Path, expected_status: &str| {
        let fixture_in_runner = linux_fixture_path(temporary.path(), fixture, use_docker);
        run_linux_fixture_command(
            temporary.path(),
            use_docker,
            r#"
set -euo pipefail
source scripts/hyperpixel-build-common.sh
hp2r_validate_host_helper "$1" "$2" "$3"
"#,
            &[
                fixture_in_runner.display().to_string(),
                expected_machine.to_owned(),
                expected_status.to_owned(),
            ],
        )
    };

    let valid_output = validate(&valid, "1");
    assert!(
        valid_output.status.success(),
        "valid helper rejected: {}",
        String::from_utf8_lossy(&valid_output.stderr)
    );

    let wrong_arch_output = validate(&wrong_arch, "1");
    assert!(
        !wrong_arch_output.status.success(),
        "wrong architecture was accepted"
    );
    assert!(
        String::from_utf8_lossy(&wrong_arch_output.stderr).contains("wrong architecture"),
        "wrong architecture failed for the wrong reason: {}",
        String::from_utf8_lossy(&wrong_arch_output.stderr)
    );

    let nonexecutable_output = validate(&nonexecutable, "1");
    assert!(
        !nonexecutable_output.status.success(),
        "nonexecutable helper was accepted"
    );
    assert!(
        String::from_utf8_lossy(&nonexecutable_output.stderr).contains("not executable"),
        "nonexecutable helper failed for the wrong reason: {}",
        String::from_utf8_lossy(&nonexecutable_output.stderr)
    );

    let missing_loader_output = validate(&missing_loader, "1");
    assert!(
        !missing_loader_output.status.success(),
        "native ELF with a missing loader was accepted"
    );
    assert!(
        String::from_utf8_lossy(&missing_loader_output.stderr)
            .contains("not executable in the build container"),
        "missing-loader ELF failed for the wrong reason: {}",
        String::from_utf8_lossy(&missing_loader_output.stderr)
    );

    let malformed_output = validate(&malformed, "1");
    assert!(
        !malformed_output.status.success(),
        "malformed ELF helper was accepted"
    );
}

#[test]
fn artifact_provenance_gate_rejects_missing_tampered_and_unbound_host_evidence() {
    let temporary = TempDir::new().expect("temporary directory must be created");
    let artifact_dir = temporary.path().join("artifacts");
    fs::create_dir(&artifact_dir).expect("artifact directory must be created");
    let helper_bytes = [
        ("host-fixdep", b"fixdep".as_slice()),
        ("host-modpost", b"modpost".as_slice()),
        ("host-genksyms", b"genksyms".as_slice()),
    ];
    for (name, bytes) in helper_bytes {
        fs::write(artifact_dir.join(name), bytes).expect("host helper evidence must be written");
    }
    let checksum = |name: &str| {
        format!(
            "{:x}",
            Sha256::digest(fs::read(artifact_dir.join(name)).expect("helper evidence"))
        )
    };
    let release = "6.18.34+rpt-rpi-v8";
    let source_sha = "1".repeat(64);
    let base_sha = "2".repeat(64);
    let manifest = temporary.path().join("manifest.txt");
    fs::write(
        &manifest,
        format!(
            "source_revision\t{}\n\
             source_tree\t{}\n\
             source_dirty\tfalse\n\
             kernel_release\t{release}\n\
             kernel_arch\taarch64\n\
             build_image\tplaneradar-kernel-builder:debian-trixie-gcc14\n\
             build_command\tmake\n\
             build_host_arch\tx86_64\n\
             kernel_source_package\tlinux\n\
             kernel_source_version\t1:6.18.34-1+rpt1\n\
             kernel_source_deb_package\tlinux-source-6.18\n\
             kernel_source_deb_sha256\t{source_sha}\n\
             host_fixdep_sha256\t{}\n\
             host_modpost_sha256\t{}\n\
             host_genksyms_sha256\t{}\n\
             base_dtb_sha256\t{base_sha}\n\
             overlay_file\tplaneradar-hyperpixel2r-000000000000.dtbo\n\
             overlay_sha256\t{}\n\
             overlay_applied_dtb\tplaneradar-hyperpixel2r-applied.dtb\n\
             module_file\tplaneradar_hyperpixel2r.ko\n\
             module_sha256\t{}\n\
             module_vermagic\t{release} SMP aarch64\n\
             module_license\tGPL\n",
            "0".repeat(40),
            "0".repeat(40),
            checksum("host-fixdep"),
            checksum("host-modpost"),
            checksum("host-genksyms"),
            "3".repeat(64),
            "4".repeat(64),
        ),
    )
    .expect("manifest must be written");
    let target = temporary.path().join("target.txt");
    fs::write(
        &target,
        format!(
            "kernel_release\t{release}\n\
             kernel_arch\taarch64\n\
             kernel_source_package\tlinux\n\
             kernel_source_version\t1:6.18.34-1+rpt1\n\
             kernel_source_deb_package\tlinux-source-6.18\n\
             kernel_source_deb_sha256\t{source_sha}\n\
             base_dtb_sha256\t{base_sha}\n"
        ),
    )
    .expect("target manifest must be written");

    let run = |manifest_path: &Path, artifact_path: &Path| {
        run_common_script(&[
            "hp2r_validate_artifact_provenance",
            manifest_path.to_str().expect("manifest path must be UTF-8"),
            target.to_str().expect("target path must be UTF-8"),
            artifact_path.to_str().expect("artifact path must be UTF-8"),
        ])
    };
    let valid = run(&manifest, &artifact_dir);
    assert!(
        valid.status.success(),
        "valid provenance rejected: {}",
        String::from_utf8_lossy(&valid.stderr)
    );

    let original = fs::read_to_string(&manifest).expect("manifest must be readable");
    for (name, contents, expected_error) in [
        (
            "missing",
            original
                .lines()
                .filter(|line| !line.starts_with("host_fixdep_sha256\t"))
                .collect::<Vec<_>>()
                .join("\n"),
            "manifest schema",
        ),
        (
            "tampered-source",
            original.replace(
                &format!("kernel_source_deb_sha256\t{source_sha}"),
                &format!("kernel_source_deb_sha256\t{}", "9".repeat(64)),
            ),
            "does not match target",
        ),
        (
            "duplicate",
            format!(
                "{original}host_fixdep_sha256\t{}\n",
                checksum("host-fixdep")
            ),
            "manifest schema",
        ),
    ] {
        let path = temporary.path().join(format!("{name}.txt"));
        fs::write(&path, contents).expect("invalid manifest must be written");
        let output = run(&path, &artifact_dir);
        assert!(!output.status.success(), "invalid provenance was accepted");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(expected_error),
            "invalid provenance failed for the wrong reason: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fs::write(artifact_dir.join("host-modpost"), b"tampered")
        .expect("host evidence must be tampered");
    let tampered_helper = run(&manifest, &artifact_dir);
    assert!(
        !tampered_helper.status.success(),
        "tampered host helper evidence was accepted"
    );
    assert!(
        String::from_utf8_lossy(&tampered_helper.stderr).contains("host helper checksum"),
        "tampered helper failed for the wrong reason: {}",
        String::from_utf8_lossy(&tampered_helper.stderr)
    );
}

#[test]
fn host_tool_preparation_fails_closed_on_source_or_compiler_mismatch() {
    let temporary = TempDir::new().expect("temporary directory must be created");
    let source_deb = temporary.path().join("linux-source.deb");
    let source_bytes = b"exact source package";
    fs::write(&source_deb, source_bytes).expect("source package fixture must be written");
    let source_sha256 = format!("{:x}", Sha256::digest(source_bytes));
    let target_config = temporary.path().join("target.config");
    fs::write(&target_config, "CONFIG_MODVERSIONS=y\n")
        .expect("target config fixture must be written");
    let exported_kbuild = temporary.path().join("exported-kbuild");
    fs::create_dir_all(&exported_kbuild).expect("exported kbuild fixture must be created");

    let fake_bin = temporary.path().join("bin");
    fs::create_dir(&fake_bin).expect("fake bin directory must be created");
    fs::write(
        fake_bin.join("dpkg-deb"),
        r#"#!/usr/bin/env bash
set -euo pipefail
case "$3" in
  Package) printf 'linux-source-6.18\n' ;;
  Version) printf '1:6.18.34-1+rpt1\n' ;;
  Architecture) printf 'all\n' ;;
  *) exit 64 ;;
esac
"#,
    )
    .expect("fake dpkg-deb must be written");
    make_executable(&fake_bin.join("dpkg-deb"));
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/prepare-kbuild-host-tools.sh");
    let cases = [
        (
            "1:6.18.33-1+rpt1",
            "fixture-host-cc",
            "source package version does not match",
        ),
        (
            "1:6.18.34-1+rpt1",
            "missing-host-compiler",
            "missing host C compiler",
        ),
    ];
    for (index, (expected_version, hostcc, expected_error)) in cases.into_iter().enumerate() {
        let output = Command::new("bash")
            .arg(&script)
            .arg(&source_deb)
            .arg(&source_sha256)
            .arg("linux-source-6.18")
            .arg(expected_version)
            .arg(&target_config)
            .arg(&exported_kbuild)
            .arg(temporary.path().join(format!("output-{index}")))
            .arg(temporary.path().join(format!("work-{index}")))
            .env("HOSTCC", hostcc)
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    fake_bin.display(),
                    std::env::var("PATH").expect("PATH must be set")
                ),
            )
            .output()
            .expect("failure case must run");
        assert!(
            !output.status.success(),
            "invalid host-tool input was accepted"
        );
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(expected_error),
            "failure case reported the wrong error: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

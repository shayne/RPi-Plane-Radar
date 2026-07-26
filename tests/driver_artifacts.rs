use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use sha2::{Digest, Sha256};
use tempfile::TempDir;

const REQUIRED_MANIFEST_KEYS: &[&str] = &[
    "source_revision",
    "source_tree",
    "source_dirty",
    "kernel_release",
    "kernel_arch",
    "build_image",
    "build_command",
    "build_host_arch",
    "kernel_source_package",
    "kernel_source_version",
    "kernel_source_deb_package",
    "kernel_source_deb_sha256",
    "host_fixdep_sha256",
    "host_modpost_sha256",
    "host_genksyms_sha256",
    "base_dtb_sha256",
    "overlay_file",
    "overlay_sha256",
    "overlay_applied_dtb",
    "module_file",
    "module_sha256",
    "module_vermagic",
    "module_license",
];

const HOST_HELPERS: &[(&str, &str)] = &[
    ("host-fixdep", "host_fixdep_sha256"),
    ("host-modpost", "host_modpost_sha256"),
    ("host-genksyms", "host_genksyms_sha256"),
];

fn parse_rows(text: &str, label: &str) -> Result<BTreeMap<String, String>, String> {
    let mut rows = BTreeMap::new();
    for (line_number, line) in text.lines().enumerate() {
        let (key, value) = line
            .split_once('\t')
            .ok_or_else(|| format!("{label} line {} is not tab-separated", line_number + 1))?;
        if key.is_empty() || value.is_empty() || value.contains('\t') {
            return Err(format!(
                "{label} line {} must contain exactly one non-empty key/value pair",
                line_number + 1
            ));
        }
        if rows.insert(key.to_owned(), value.to_owned()).is_some() {
            return Err(format!("{label} key is duplicated: {key}"));
        }
    }
    Ok(rows)
}

fn parse_artifact_manifest(text: &str) -> Result<BTreeMap<String, String>, String> {
    let manifest = parse_rows(text, "manifest")?;
    if manifest.len() != REQUIRED_MANIFEST_KEYS.len() {
        return Err(format!(
            "manifest has {} keys, expected {}",
            manifest.len(),
            REQUIRED_MANIFEST_KEYS.len()
        ));
    }
    for key in REQUIRED_MANIFEST_KEYS {
        if !manifest.contains_key(*key) {
            return Err(format!("manifest key is missing: {key}"));
        }
    }
    if let Some(key) = manifest
        .keys()
        .find(|key| !REQUIRED_MANIFEST_KEYS.contains(&key.as_str()))
    {
        return Err(format!("manifest key is not allowed: {key}"));
    }
    Ok(manifest)
}

fn is_lower_hex_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn validate_build_provenance(
    manifest: &BTreeMap<String, String>,
    target: &BTreeMap<String, String>,
) -> Result<(), String> {
    let host_arch = manifest
        .get("build_host_arch")
        .ok_or_else(|| "build_host_arch is missing".to_owned())?;
    if !matches!(host_arch.as_str(), "aarch64" | "arm64" | "x86_64" | "amd64") {
        return Err(format!("unsupported build_host_arch: {host_arch}"));
    }
    for (manifest_key, target_key) in [
        ("kernel_release", "kernel_release"),
        ("kernel_arch", "kernel_arch"),
        ("kernel_source_package", "kernel_source_package"),
        ("kernel_source_version", "kernel_source_version"),
        ("kernel_source_deb_package", "kernel_source_deb_package"),
        ("kernel_source_deb_sha256", "kernel_source_deb_sha256"),
        ("base_dtb_sha256", "base_dtb_sha256"),
    ] {
        let manifest_value = manifest
            .get(manifest_key)
            .ok_or_else(|| format!("{manifest_key} is missing"))?;
        let target_value = target
            .get(target_key)
            .ok_or_else(|| format!("target {target_key} is missing"))?;
        if manifest_value != target_value {
            return Err(format!("{manifest_key} does not match target {target_key}"));
        }
    }
    if manifest.get("kernel_source_package").map(String::as_str) != Some("linux") {
        return Err("kernel_source_package must be linux".to_owned());
    }
    let source_version = manifest
        .get("kernel_source_version")
        .ok_or_else(|| "kernel_source_version is missing".to_owned())?;
    if source_version.is_empty()
        || !source_version
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b".+:~_-".contains(&byte))
    {
        return Err("kernel_source_version has an invalid format".to_owned());
    }
    let source_deb_package = manifest
        .get("kernel_source_deb_package")
        .ok_or_else(|| "kernel_source_deb_package is missing".to_owned())?;
    let series = source_deb_package
        .strip_prefix("linux-source-")
        .ok_or_else(|| "kernel_source_deb_package has an invalid format".to_owned())?;
    let mut components = series.split('.');
    if components
        .next()
        .is_none_or(|part| part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()))
        || components
            .next()
            .is_none_or(|part| part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()))
        || components.next().is_some()
    {
        return Err("kernel_source_deb_package has an invalid format".to_owned());
    }
    for key in [
        "kernel_source_deb_sha256",
        "host_fixdep_sha256",
        "host_modpost_sha256",
        "host_genksyms_sha256",
    ] {
        if !manifest
            .get(key)
            .is_some_and(|value| is_lower_hex_sha256(value))
        {
            return Err(format!("{key} must be lowercase SHA-256"));
        }
    }
    if manifest.get("kernel_arch").map(String::as_str) != Some("aarch64") {
        return Err("kernel_arch must remain aarch64".to_owned());
    }
    let release = manifest
        .get("kernel_release")
        .ok_or_else(|| "kernel_release is missing".to_owned())?;
    if !manifest
        .get("module_vermagic")
        .is_some_and(|vermagic| vermagic.starts_with(release))
    {
        return Err("module_vermagic must begin with kernel_release".to_owned());
    }
    Ok(())
}

fn checked_applied_dtb_path(artifact_dir: &Path, value: &str) -> Result<PathBuf, String> {
    if value != "planeradar-hyperpixel2r-applied.dtb" {
        return Err("overlay_applied_dtb must be planeradar-hyperpixel2r-applied.dtb".to_owned());
    }
    Ok(artifact_dir.join(value))
}

fn validate_source_provenance(
    source_dirty: &str,
    source_revision: &str,
    source_tree: &str,
    overlay_revision: &str,
    expected_revision: &str,
    expected_tree: &str,
) -> Result<(), String> {
    let is_object_id = |value: &str| {
        value.len() == 40
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    };
    if source_dirty != "false" {
        return Err("source_dirty must be false".to_owned());
    }
    if !is_object_id(source_revision) || !is_object_id(expected_revision) {
        return Err("source revisions must be lowercase hexadecimal Git object IDs".to_owned());
    }
    if !is_object_id(source_tree) || !is_object_id(expected_tree) {
        return Err("source trees must be lowercase hexadecimal Git object IDs".to_owned());
    }
    if source_revision != expected_revision {
        return Err("source_revision must match checked HEAD".to_owned());
    }
    if source_tree != expected_tree {
        return Err("source_tree must match checked HEAD tree".to_owned());
    }
    if overlay_revision != &source_revision[..12] {
        return Err("overlay filename revision must match source_revision".to_owned());
    }
    Ok(())
}

fn run_overlay_validator(overlay: &Path) -> Output {
    Command::new("bash")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/validate-hyperpixel-overlay.sh"))
        .arg(overlay)
        .output()
        .expect("overlay validator must run")
}

fn compile_overlay_source(
    artifact_dir: &Path,
    source: &str,
    fixture_name: &str,
) -> (TempDir, PathBuf) {
    let release = artifact_dir
        .file_name()
        .and_then(|name| name.to_str())
        .expect("artifact directory must end in the kernel release");
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    let target_dir = repository.join("dist/kernel-target").join(release);
    let target_manifest = fs::read_to_string(target_dir.join("target.txt"))
        .expect("target manifest must be readable");
    let common_header_path = target_manifest
        .lines()
        .find_map(|line| line.strip_prefix("common_header_path\t"))
        .expect("target manifest must name common headers");
    let temporary = TempDir::new().expect("temporary directory must be created");
    let source_path = temporary.path().join(format!("{fixture_name}.dts"));
    fs::write(&source_path, source).expect("mutated source must be written");
    let fixture_mount = format!("{}:/fixture", temporary.path().display());
    let target_mount = format!("{}:/target-root:ro", target_dir.join("root").display());
    let compile = Command::new("docker")
        .args([
            "run",
            "--rm",
            "--volume",
            &fixture_mount,
            "--volume",
            &target_mount,
            "planeradar-kernel-builder:debian-trixie-gcc14",
            "sh",
            "-eu",
            "-c",
            r#"
aarch64-linux-gnu-gcc-14 \
  -E -nostdinc -undef -D__DTS__ -x assembler-with-cpp \
  -I"/target-root$1/include" \
  "/fixture/$2.dts" \
  -o "/fixture/$2.preprocessed.dts"
dtc -@ -I dts -O dtb \
  -o "/fixture/$2.dtbo" \
  "/fixture/$2.preprocessed.dts"
"#,
            "sh",
            common_header_path,
            fixture_name,
        ])
        .output()
        .expect("mutated overlay compiler must run");
    assert!(
        compile.status.success(),
        "mutated overlay must compile: {}",
        String::from_utf8_lossy(&compile.stderr)
    );
    assert!(
        compile.stderr.is_empty(),
        "mutated overlay must compile warning-free: {}",
        String::from_utf8_lossy(&compile.stderr)
    );
    let overlay_path = temporary.path().join(format!("{fixture_name}.dtbo"));
    (temporary, overlay_path)
}

#[test]
fn driver_artifact_bundle_matches_its_manifest() {
    let Ok(artifact_dir) = env::var("PLANERADAR_DRIVER_ARTIFACT_DIR") else {
        println!("PLANERADAR_DRIVER_ARTIFACT_DIR is unset; skipping driver artifact test");
        return;
    };
    let artifact_dir = Path::new(&artifact_dir)
        .canonicalize()
        .expect("driver artifact directory must resolve to an absolute path");
    let module_path = artifact_dir.join("planeradar_hyperpixel2r.ko");
    let manifest_path = artifact_dir.join("manifest.txt");
    let checksum_path = artifact_dir.join("module.sha256");
    let modinfo_path = artifact_dir.join("module.modinfo.txt");
    let host_helper_paths: Vec<_> = HOST_HELPERS
        .iter()
        .map(|(file, _)| artifact_dir.join(file))
        .collect();

    for path in [&module_path, &manifest_path, &checksum_path, &modinfo_path]
        .into_iter()
        .chain(host_helper_paths.iter())
    {
        assert!(
            path.is_file(),
            "required artifact is missing: {}",
            path.display()
        );
    }

    let manifest_text =
        fs::read_to_string(&manifest_path).expect("manifest.txt must be valid UTF-8");
    let manifest =
        parse_artifact_manifest(&manifest_text).expect("manifest must have the exact schema");

    assert_eq!(manifest["kernel_arch"], "aarch64");
    assert_eq!(manifest["module_license"], "GPL");
    assert_eq!(
        manifest["build_image"],
        "planeradar-kernel-builder:debian-trixie-gcc14"
    );
    assert_eq!(
        manifest["build_command"],
        format!(
            "make -C /usr/src/linux-headers-{} M=/build/kernel ARCH=arm64 \
             CROSS_COMPILE=aarch64-linux-gnu- W=1 modules",
            manifest["kernel_release"]
        )
    );
    assert_eq!(
        manifest["module_file"], "planeradar_hyperpixel2r.ko",
        "module_file must name the required module"
    );

    let overlay_file = &manifest["overlay_file"];
    let overlay_revision = overlay_file
        .strip_prefix("planeradar-hyperpixel2r-")
        .and_then(|name| name.strip_suffix(".dtbo"))
        .expect("overlay_file must match planeradar-hyperpixel2r-[0-9a-f]{12}.dtbo");
    assert!(
        overlay_revision.len() == 12
            && overlay_revision
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "overlay_file must match planeradar-hyperpixel2r-[0-9a-f]{{12}}.dtbo"
    );
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    let target_manifest_text = fs::read_to_string(
        repository
            .join("dist/kernel-target")
            .join(&manifest["kernel_release"])
            .join("target.txt"),
    )
    .expect("target manifest must be readable");
    let target =
        parse_rows(&target_manifest_text, "target manifest").expect("target manifest must parse");
    validate_build_provenance(&manifest, &target)
        .expect("build provenance must match the live target export");
    assert!(
        Command::new("git")
            .args(["diff-index", "--quiet", "HEAD", "--"])
            .current_dir(repository)
            .status()
            .expect("git diff-index must run")
            .success(),
        "tracked source must be clean when validating artifacts"
    );
    let checked_revision = String::from_utf8(
        Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(repository)
            .output()
            .expect("git rev-parse HEAD must run")
            .stdout,
    )
    .expect("checked source revision must be UTF-8");
    let checked_tree = String::from_utf8(
        Command::new("git")
            .args(["rev-parse", "HEAD^{tree}"])
            .current_dir(repository)
            .output()
            .expect("git rev-parse HEAD tree must run")
            .stdout,
    )
    .expect("checked source tree must be UTF-8");
    validate_source_provenance(
        &manifest["source_dirty"],
        &manifest["source_revision"],
        &manifest["source_tree"],
        overlay_revision,
        checked_revision.trim(),
        checked_tree.trim(),
    )
    .expect("artifact source provenance must match the clean checked source");

    let overlay_path = artifact_dir.join(overlay_file);
    let applied_dtb_path =
        checked_applied_dtb_path(&artifact_dir, &manifest["overlay_applied_dtb"])
            .expect("overlay_applied_dtb must be the required contained basename");
    for path in [&overlay_path, &applied_dtb_path] {
        assert!(
            path.is_file(),
            "required artifact is missing: {}",
            path.display()
        );
    }

    let overlay = fs::read(&overlay_path).expect("overlay must be readable");
    let actual_overlay_checksum = format!("{:x}", Sha256::digest(overlay));
    assert_eq!(manifest["overlay_sha256"], actual_overlay_checksum);

    let module = fs::read(&module_path).expect("module must be readable");
    let actual_checksum = format!("{:x}", Sha256::digest(module));
    assert_eq!(manifest["module_sha256"], actual_checksum);

    for ((_, manifest_key), helper_path) in HOST_HELPERS.iter().zip(&host_helper_paths) {
        let helper = fs::read(helper_path).expect("host helper must be readable");
        assert_eq!(
            manifest[*manifest_key],
            format!("{:x}", Sha256::digest(helper)),
            "{manifest_key} must match the bundled host helper"
        );
    }
    let helper_mount = format!("{}:/artifacts:ro", artifact_dir.display());
    let helper_check = Command::new("docker")
        .args([
            "run",
            "--rm",
            "--volume",
            &helper_mount,
            "planeradar-kernel-builder:debian-trixie-gcc14",
            "sh",
            "-eu",
            "-c",
            r#"
case "$1" in
  arm64|aarch64) expected_machine=AArch64 ;;
  amd64|x86_64) expected_machine="Advanced Micro Devices X86-64" ;;
  *) exit 64 ;;
esac
for specification in \
  host-fixdep:1 \
  host-modpost:0 \
  host-genksyms:0
do
  helper="${specification%:*}"
  expected_status="${specification#*:}"
  machine="$(
    readelf -h "/artifacts/$helper" |
      awk -F ": *" '$1 ~ /^[[:space:]]*Machine$/ { print $2; exit }'
  )"
  test "$machine" = "$expected_machine"
  set +e
  "/artifacts/$helper" </dev/null >/dev/null 2>&1
  status="$?"
  set -e
  test "$status" -eq "$expected_status"
done
"#,
            "sh",
            &manifest["build_host_arch"],
        ])
        .output()
        .expect("native host-helper validation container must run");
    assert!(
        helper_check.status.success(),
        "host helpers must match and execute on the native image: {}",
        String::from_utf8_lossy(&helper_check.stderr)
    );

    let checksum_text =
        fs::read_to_string(&checksum_path).expect("module.sha256 must be valid UTF-8");
    let checksum_fields: Vec<_> = checksum_text.split_whitespace().collect();
    assert_eq!(
        checksum_fields,
        [actual_checksum.as_str(), "planeradar_hyperpixel2r.ko"],
        "module.sha256 must be a sha256sum record for the module"
    );
}

fn provenance_fixture(host_arch: &str) -> (BTreeMap<String, String>, BTreeMap<String, String>) {
    let release = "6.18.34+rpt-rpi-v8";
    let checksum = "1".repeat(64);
    let mut manifest = BTreeMap::new();
    for key in REQUIRED_MANIFEST_KEYS {
        manifest.insert((*key).to_owned(), "value".to_owned());
    }
    for (key, value) in [
        ("kernel_release", release),
        ("kernel_arch", "aarch64"),
        (
            "module_vermagic",
            "6.18.34+rpt-rpi-v8 SMP preempt mod_unload modversions aarch64",
        ),
        ("build_host_arch", host_arch),
        ("kernel_source_package", "linux"),
        ("kernel_source_version", "1:6.18.34-1+rpt1"),
        ("kernel_source_deb_package", "linux-source-6.18"),
        ("kernel_source_deb_sha256", checksum.as_str()),
        ("host_fixdep_sha256", checksum.as_str()),
        ("host_modpost_sha256", checksum.as_str()),
        ("host_genksyms_sha256", checksum.as_str()),
        ("base_dtb_sha256", checksum.as_str()),
    ] {
        manifest.insert(key.to_owned(), value.to_owned());
    }
    let mut target = BTreeMap::new();
    for key in [
        "kernel_release",
        "kernel_arch",
        "kernel_source_package",
        "kernel_source_version",
        "kernel_source_deb_package",
        "kernel_source_deb_sha256",
        "base_dtb_sha256",
    ] {
        target.insert(key.to_owned(), manifest[key].clone());
    }
    (manifest, target)
}

#[test]
fn manifest_schema_and_build_provenance_reject_removal_tampering_and_extra_rows() {
    let (manifest, target) = provenance_fixture("aarch64");
    validate_build_provenance(&manifest, &target).expect("valid provenance");

    let exact_text = REQUIRED_MANIFEST_KEYS
        .iter()
        .map(|key| format!("{key}\t{}", manifest[*key]))
        .collect::<Vec<_>>()
        .join("\n");
    parse_artifact_manifest(&exact_text).expect("exact manifest schema");
    for invalid in [
        exact_text
            .lines()
            .filter(|line| !line.starts_with("host_fixdep_sha256\t"))
            .collect::<Vec<_>>()
            .join("\n"),
        format!("{exact_text}\nunknown\tvalue"),
        format!("{exact_text}\nhost_fixdep_sha256\t{}", "2".repeat(64)),
    ] {
        assert!(
            parse_artifact_manifest(&invalid).is_err(),
            "invalid manifest cardinality was accepted"
        );
    }

    for (key, value) in [
        ("kernel_source_version", "1:6.18.33-1+rpt1"),
        ("kernel_source_deb_sha256", "2"),
        ("build_host_arch", "riscv64"),
        ("host_modpost_sha256", "2"),
    ] {
        let mut tampered = manifest.clone();
        tampered.insert(key.to_owned(), value.to_owned());
        assert!(
            validate_build_provenance(&tampered, &target).is_err(),
            "tampered provenance was accepted: {key}"
        );
    }
}

#[test]
fn native_x86_host_provenance_keeps_the_target_module_aarch64() {
    let (manifest, target) = provenance_fixture("x86_64");
    validate_build_provenance(&manifest, &target)
        .expect("native x86 host and AArch64 target are distinct valid architectures");
    assert_eq!(manifest["kernel_arch"], "aarch64");
    assert!(manifest["module_vermagic"].ends_with("aarch64"));

    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    for path in [
        "scripts/check-hyperpixel-artifacts.sh",
        "scripts/validate-hyperpixel-overlay.sh",
    ] {
        let source = fs::read_to_string(repository.join(path)).expect("source must be readable");
        assert!(
            !source.contains("--platform linux/arm64")
                && !source.contains("\"--platform\",")
                && !source.contains("\"linux/arm64\","),
            "{path} must use the native loaded image without arm64 emulation"
        );
    }
}

#[test]
fn source_provenance_requires_clean_current_revision_tree_and_overlay_name() {
    let revision = "1".repeat(40);
    let tree = "2".repeat(40);
    for (dirty, source_revision, source_tree, overlay_revision) in [
        ("true", revision.as_str(), tree.as_str(), &revision[..12]),
        (
            "false",
            "0000000000000000000000000000000000000000",
            tree.as_str(),
            "000000000000",
        ),
        (
            "false",
            revision.as_str(),
            "0000000000000000000000000000000000000000",
            &revision[..12],
        ),
        ("false", revision.as_str(), tree.as_str(), "000000000000"),
    ] {
        assert!(
            validate_source_provenance(
                dirty,
                source_revision,
                source_tree,
                overlay_revision,
                &revision,
                &tree,
            )
            .is_err(),
            "invalid source provenance was accepted: dirty={dirty}, \
             revision={source_revision}, tree={source_tree}, \
             overlay_revision={overlay_revision}"
        );
    }
}

#[test]
fn applied_dtb_name_rejects_absolute_and_traversal_paths() {
    let artifact_dir = Path::new("/artifacts");
    for invalid in [
        "/tmp/planeradar-hyperpixel2r-applied.dtb",
        "../planeradar-hyperpixel2r-applied.dtb",
        "nested/planeradar-hyperpixel2r-applied.dtb",
    ] {
        assert!(
            checked_applied_dtb_path(artifact_dir, invalid).is_err(),
            "escaping applied DTB path was accepted: {invalid}"
        );
    }
}

#[test]
fn overlay_validator_rejects_mutated_rotate_bytecode() {
    let Ok(artifact_dir) = env::var("PLANERADAR_DRIVER_ARTIFACT_DIR") else {
        println!("PLANERADAR_DRIVER_ARTIFACT_DIR is unset; skipping overlay mutation test");
        return;
    };
    let manifest = fs::read_to_string(Path::new(&artifact_dir).join("manifest.txt"))
        .expect("manifest fixture must be readable");
    let overlay_file = manifest
        .lines()
        .find_map(|line| line.strip_prefix("overlay_file\t"))
        .expect("manifest must name its overlay");
    let mut overlay =
        fs::read(Path::new(&artifact_dir).join(overlay_file)).expect("overlay must be readable");
    let needle = b"rotation:0\0";
    let matches: Vec<_> = overlay
        .windows(needle.len())
        .enumerate()
        .filter_map(|(index, bytes)| (bytes == needle).then_some(index))
        .collect();
    assert_eq!(matches.len(), 1, "overlay must contain one rotate bytecode");
    overlay[matches[0] + needle.len() - 2] = b'4';

    let temporary = TempDir::new().expect("temporary directory must be created");
    let mutated = temporary.path().join("mutated-rotate.dtbo");
    fs::write(&mutated, overlay).expect("mutated overlay must be written");
    let output = run_overlay_validator(&mutated);
    assert!(
        !output.status.success(),
        "mutated rotate bytecode was accepted"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("invalid rotate override encoding"),
        "rotate mutation failed for the wrong reason: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn overlay_validator_rejects_mutated_touchscreen_modifiers_and_target() {
    let Ok(artifact_dir) = env::var("PLANERADAR_DRIVER_ARTIFACT_DIR") else {
        println!("PLANERADAR_DRIVER_ARTIFACT_DIR is unset; skipping overlay mutation test");
        return;
    };
    let manifest = fs::read_to_string(Path::new(&artifact_dir).join("manifest.txt"))
        .expect("manifest fixture must be readable");
    let overlay_file = manifest
        .lines()
        .find_map(|line| line.strip_prefix("overlay_file\t"))
        .expect("manifest must name its overlay");
    let original =
        fs::read(Path::new(&artifact_dir).join(overlay_file)).expect("overlay must be readable");
    let temporary = TempDir::new().expect("temporary directory must be created");

    for (property, expected_error) in [
        (
            b"touchscreen-inverted-x?\0".as_slice(),
            "invalid touchscreen-inverted-x override encoding",
        ),
        (
            b"touchscreen-inverted-y?\0".as_slice(),
            "invalid touchscreen-inverted-y override encoding",
        ),
        (
            b"touchscreen-swapped-x-y?\0".as_slice(),
            "invalid touchscreen-swapped-x-y override encoding",
        ),
    ] {
        let matches: Vec<_> = original
            .windows(property.len())
            .enumerate()
            .filter_map(|(index, bytes)| (bytes == property).then_some(index))
            .collect();
        assert_eq!(matches.len(), 1, "override property must occur once");
        let mut mutated = original.clone();
        mutated[matches[0] + property.len() - 2] = b'!';
        let mutated_path = temporary.path().join(format!(
            "mutated-{}.dtbo",
            String::from_utf8_lossy(&property[..property.len() - 2])
        ));
        fs::write(&mutated_path, mutated).expect("mutated overlay must be written");
        let output = run_overlay_validator(&mutated_path);
        assert!(
            !output.status.success(),
            "mutated touchscreen modifier was accepted"
        );
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(expected_error),
            "touchscreen mutation failed for the wrong reason: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let target_needle = b"\0\0\0\x04touchscreen-inverted-x?\0";
    let matches: Vec<_> = original
        .windows(target_needle.len())
        .enumerate()
        .filter_map(|(index, bytes)| (bytes == target_needle).then_some(index))
        .collect();
    assert_eq!(matches.len(), 1, "touch target bytecode must occur once");
    let mut wrong_target = original;
    wrong_target[matches[0] + 3] = 3;
    let wrong_target_path = temporary.path().join("mutated-touch-target.dtbo");
    fs::write(&wrong_target_path, wrong_target).expect("mutated overlay must be written");
    let output = run_overlay_validator(&wrong_target_path);
    assert!(
        !output.status.success(),
        "mutated touchscreen target was accepted"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("invalid touchscreen-inverted-x override encoding"),
        "touch target mutation failed for the wrong reason: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn overlay_validator_rejects_vc4_and_v3d_fragments_even_when_they_are_noops() {
    let Ok(artifact_dir) = env::var("PLANERADAR_DRIVER_ARTIFACT_DIR") else {
        println!("PLANERADAR_DRIVER_ARTIFACT_DIR is unset; skipping overlay mutation test");
        return;
    };
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    let source = fs::read_to_string(repository.join("kernel/planeradar-hyperpixel2r-overlay.dts"))
        .expect("overlay source must be readable");
    let forbidden_fragments = r#"
	fragment@2 {
		target = <&vc4>;
		__overlay__ {
			status = "disabled";
		};
	};

	fragment@3 {
		target = <&v3d>;
		__overlay__ {
			status = "disabled";
		};
	};

"#;
    let mutated_source = source.replacen(
        "\n\t__overrides__ {",
        &format!("\n{forbidden_fragments}\t__overrides__ {{"),
        1,
    );
    assert_ne!(
        mutated_source, source,
        "mutation insertion point must exist"
    );

    let (_temporary, overlay_path) = compile_overlay_source(
        Path::new(&artifact_dir),
        &mutated_source,
        "forbidden-fragments",
    );
    let output = run_overlay_validator(&overlay_path);
    assert!(
        !output.status.success(),
        "VC4/V3D no-op fragments were accepted"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("compiled overlay root shape is invalid"),
        "forbidden-fragment mutation failed for the wrong reason: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn overlay_validator_rejects_nested_accelerator_payload_in_the_root_fragment() {
    let Ok(artifact_dir) = env::var("PLANERADAR_DRIVER_ARTIFACT_DIR") else {
        println!("PLANERADAR_DRIVER_ARTIFACT_DIR is unset; skipping overlay mutation test");
        return;
    };
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    let source = fs::read_to_string(repository.join("kernel/planeradar-hyperpixel2r-overlay.dts"))
        .expect("overlay source must be readable");
    for (node_name, fixture_name) in [("gpu", "nested-gpu"), ("v3d", "nested-v3d")] {
        let injected = format!(
            r#"		__overlay__ {{
			soc {{
				{node_name} {{
					status = "disabled";
				}};
			}};
"#
        );
        let mutated_source = source.replacen("\t\t__overlay__ {\n", &injected, 1);
        assert_ne!(
            mutated_source, source,
            "root-fragment mutation insertion point must exist"
        );
        let (_temporary, overlay_path) =
            compile_overlay_source(Path::new(&artifact_dir), &mutated_source, fixture_name);
        let output = run_overlay_validator(&overlay_path);
        assert!(
            !output.status.success(),
            "nested root-fragment accelerator payload was accepted: {node_name}"
        );
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .contains("root fragment overlay shape is invalid"),
            "nested {node_name} mutation failed for the wrong reason: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn overlay_validator_rejects_the_wrong_root_compatible() {
    let Ok(artifact_dir) = env::var("PLANERADAR_DRIVER_ARTIFACT_DIR") else {
        println!("PLANERADAR_DRIVER_ARTIFACT_DIR is unset; skipping overlay mutation test");
        return;
    };
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    let source = fs::read_to_string(repository.join("kernel/planeradar-hyperpixel2r-overlay.dts"))
        .expect("overlay source must be readable");
    let mutated_source = source.replacen(
        "compatible = \"brcm,bcm2835\";",
        "compatible = \"brcm,bcm2712\";",
        1,
    );
    assert_ne!(
        mutated_source, source,
        "root-compatible mutation point must exist"
    );
    let (_temporary, overlay_path) = compile_overlay_source(
        Path::new(&artifact_dir),
        &mutated_source,
        "wrong-root-compatible",
    );
    let output = run_overlay_validator(&overlay_path);
    assert!(
        !output.status.success(),
        "wrong root compatible was accepted"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("root compatible is invalid"),
        "root-compatible mutation failed for the wrong reason: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

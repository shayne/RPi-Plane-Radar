use std::fs;
use std::io::{BufWriter, Write};
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use sha2::{Digest, Sha256};
use tempfile::TempDir;

fn write_executable(path: &Path, contents: &str) {
    fs::write(path, contents).expect("write executable fixture");
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("make fixture executable");
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn mode(path: &Path) -> u32 {
    fs::symlink_metadata(path)
        .unwrap_or_else(|error| panic!("metadata for {}: {error}", path.display()))
        .permissions()
        .mode()
        & 0o777
}

fn write_valid_png(path: &Path) {
    let file = fs::File::create(path).expect("create valid PNG fixture");
    let mut encoder = png::Encoder::new(BufWriter::new(file), 480, 480);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().expect("write PNG header");
    writer
        .write_image_data(&vec![0; 480 * 480 * 4])
        .expect("write PNG pixels");
}

struct ScriptFixture {
    _temporary: TempDir,
    repository: PathBuf,
    driver: PathBuf,
    target_manifest: PathBuf,
    app: PathBuf,
    root: PathBuf,
    log: PathBuf,
    ownership: PathBuf,
    fixture_path: String,
    revision: String,
    source_tree: String,
    release: &'static str,
    overlay_file: String,
    module: Vec<u8>,
    overlay: Vec<u8>,
    valid_png: PathBuf,
}

impl ScriptFixture {
    fn new() -> Self {
        let repository = Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf();
        let temporary = tempfile::tempdir().expect("temporary directory");
        let driver = temporary.path().join("driver");
        let target_manifest = temporary.path().join("target.txt");
        let app = temporary.path().join("app");
        let bin = temporary.path().join("bin");
        let root = temporary.path().join("root");
        let log = temporary.path().join("commands.log");
        let ownership = temporary.path().join("ownership.tsv");
        fs::write(&ownership, "").expect("ownership model");
        for directory in [&driver, &app, &bin, &root.join("boot/firmware/overlays")] {
            fs::create_dir_all(directory)
                .unwrap_or_else(|error| panic!("create {}: {error}", directory.display()));
        }
        fs::write(
            root.join("boot/firmware/config.txt"),
            "[all]\ndtoverlay=vc4-kms-dpi-hyperpixel2r\n",
        )
        .expect("normal config fixture");

        let revision = String::from_utf8(
            Command::new("git")
                .args(["rev-parse", "HEAD"])
                .current_dir(&repository)
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
                .current_dir(&repository)
                .output()
                .expect("read source tree")
                .stdout,
        )
        .expect("source tree UTF-8")
        .trim()
        .to_owned();
        let release = "6.18.34+rpt-rpi-v8";
        let overlay_file = format!("planeradar-hyperpixel2r-{}.dtbo", &revision[..12]);
        let module = b"fixture arm64 module".to_vec();
        let overlay = b"fixture validated overlay".to_vec();
        fs::write(driver.join("planeradar_hyperpixel2r.ko"), &module).expect("module fixture");
        fs::write(driver.join(&overlay_file), &overlay).expect("overlay fixture");
        fs::write(
            driver.join("planeradar-hyperpixel2r-applied.dtb"),
            b"fixture applied dtb",
        )
        .expect("applied fixture");
        fs::write(
            driver.join("module.sha256"),
            format!("{}  planeradar_hyperpixel2r.ko\n", sha256_hex(&module)),
        )
        .expect("module checksum fixture");
        fs::write(driver.join("module.file.txt"), "ARM aarch64\n").expect("file fixture");
        fs::write(
            driver.join("module.modinfo.txt"),
            format!("license: GPL\nvermagic: {release}\n"),
        )
        .expect("modinfo fixture");
        fs::write(driver.join("module.readelf.txt"), "Machine: AArch64\n")
            .expect("readelf fixture");
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
                concat!(
                    "kernel_release\t{release}\n",
                    "kernel_arch\taarch64\n",
                    "header_path\t/usr/src/linux-headers-{release}\n",
                    "common_header_path\t/usr/src/linux-headers-6.18.34+rpt-common-rpi\n",
                    "kbuild_path\t/usr/lib/linux-kbuild-6.18.34+rpt\n",
                    "kernel_source_package\tlinux\n",
                    "kernel_source_version\t1:6.18.34-1+rpt1\n",
                    "kernel_source_deb_package\tlinux-source-6.18\n",
                    "kernel_source_deb_filename\tpool/main/l/linux/linux-source-6.18_fixture_all.deb\n",
                    "kernel_source_deb_sha256\t{source_deb_sha}\n",
                    "kernel_source_deb\tkernel-source.deb\n",
                    "base_dtb_path\t/boot/firmware/bcm2710-rpi-zero-2-w.dtb\n",
                    "base_dtb_sha256\t{base_dtb_sha}\n",
                ),
                release = release,
                source_deb_sha = source_deb_sha,
                base_dtb_sha = base_dtb_sha,
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
                overlay_sha = sha256_hex(&overlay),
                module_sha = sha256_hex(&module),
            ),
        )
        .expect("manifest fixture");

        let host_binary = repository.join("target/debug/planeradar");
        write_executable(
            &app.join("planeradar"),
            &format!(
                "#!/usr/bin/env bash\n\
                 if test \"${{1-}}\" = version; then printf 'planeradar 0.1.0 ({revision})\\n'; exit; fi\n\
                 if test \"${{1-}}\" = stage-display && test -n \"${{PLANERADAR_TEST_MUTATE_CONFIG_AT_FINAL_BOUNDARY:-}}\"; then\n\
                   checked=false\n\
                   boot_config=''\n\
                   arguments=(\"$@\")\n\
                   for ((index = 0; index < ${{#arguments[@]}}; index++)); do\n\
                     test \"${{arguments[index]}}\" != --expected-boot-config-sha256 || checked=true\n\
                     if test \"${{arguments[index]}}\" = --boot-config; then boot_config=\"${{arguments[index + 1]}}\"; fi\n\
                   done\n\
                   if \"$checked\"; then\n\
                     printf 'y\\n' | '{}' configure-display --boot-config \"$boot_config\" \\\n\
                       --declaration dtoverlay=cooperating-writer >/dev/null 2>&1\n\
                   fi\n\
                 fi\n\
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
                host_binary.display(),
            ),
        );
        let app_bytes = fs::read(app.join("planeradar")).expect("app bytes");
        fs::write(app.join("planeradar.revision"), format!("{revision}\n")).expect("app revision");
        fs::write(app.join("planeradar.tree"), format!("{source_tree}\n"))
            .expect("app source tree");
        fs::write(
            app.join("planeradar.sha256"),
            format!("{}  planeradar\n", sha256_hex(&app_bytes)),
        )
        .expect("app checksum");
        fs::write(app.join("planeradar.readelf.txt"), "Machine: AArch64\n").expect("app readelf");

        let valid_png = temporary.path().join("valid-480x480.png");
        write_valid_png(&valid_png);

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
        let real_timeout = String::from_utf8(
            Command::new("which")
                .arg("timeout")
                .output()
                .expect("find timeout")
                .stdout,
        )
        .expect("timeout path")
        .trim()
        .to_owned();
        write_executable(
            &bin.join("git"),
            &format!(
                "#!/usr/bin/env bash\n\
                 if test \"${{1-}}\" = status; then exit 0; fi\n\
                 if test \"$*\" = 'rev-parse HEAD'; then printf '%s\\n' \"$PLANERADAR_TEST_REVISION\"; exit; fi\n\
                 if test \"$*\" = 'rev-parse HEAD^{{tree}}'; then printf '%s\\n' \"$PLANERADAR_TEST_SOURCE_TREE\"; exit; fi\n\
                 exec '{real_git}' \"$@\"\n"
            ),
        );
        let real_sha256sum = String::from_utf8(
            Command::new("which")
                .arg("sha256sum")
                .output()
                .expect("find sha256sum")
                .stdout,
        )
        .expect("sha256sum path")
        .trim()
        .to_owned();
        write_executable(
            &bin.join("sha256sum"),
            &format!(
                r#"#!/usr/bin/env bash
set -euo pipefail
normal_config="$PLANERADAR_TEST_ROOT/boot/firmware/config.txt"
if test -n "${{PLANERADAR_TEST_MUTATE_CONFIG_AT_FINAL_BOUNDARY:-}}" &&
   test "$#" -eq 1 && test "$1" = "$normal_config"
then
  mkdir -p "$PLANERADAR_TEST_ROOT/tmp"
  count_file="$PLANERADAR_TEST_ROOT/tmp/normal-sha-count"
  count=0
  test ! -f "$count_file" || count="$(cat "$count_file")"
  count=$((count + 1))
  printf '%s\n' "$count" > "$count_file"
  digest="$('{real_sha256sum}' "$@")"
  if test "$count" -eq 4 &&
     test ! -e "$PLANERADAR_TEST_ROOT/tmp/final-boundary-mutated"
  then
    : > "$PLANERADAR_TEST_ROOT/tmp/final-boundary-mutated"
    printf 'y\n' | "$PLANERADAR_TEST_HOST_BINARY" configure-display \
      --boot-config "$normal_config" \
      --declaration dtoverlay=cooperating-writer >/dev/null 2>&1
  fi
  printf '%s\n' "$digest"
  exit
fi
exec '{real_sha256sum}' "$@"
"#
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
  chmod 0700 "$PLANERADAR_TEST_ROOT$stage"
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
printf 'scp %s %s\n' "$source" "$destination" >> "$PLANERADAR_TEST_LOG"
cp -Rp "${source%/.}/." "$PLANERADAR_TEST_ROOT$destination"
if test -n "${PLANERADAR_TEST_DISTINCT_DKMS_SOURCE:-}"; then
  printf '%s\n' "$PLANERADAR_TEST_DISTINCT_DKMS_SOURCE" \
    >> "$PLANERADAR_TEST_ROOT$destination/dkms-source/planeradar_hyperpixel2r_main.c"
fi
if test -n "${PLANERADAR_TEST_RESHAPE_DKMS_SOURCE:-}"; then
  rm -f "$PLANERADAR_TEST_ROOT$destination/dkms-source/README.md"
  printf '%s\n' '/* revision B only */' \
    > "$PLANERADAR_TEST_ROOT$destination/dkms-source/planeradar_hyperpixel2r_revision_b.c"
fi
"#,
        );
        write_executable(
            &bin.join("sudo"),
            r#"#!/usr/bin/env bash
set -euo pipefail
if test "${1-}" = test && test "${2-}" = -c; then
  expected="$PLANERADAR_TEST_ROOT/dev/dri/renderD128"
  test "$#" -eq 3
  test "$3" = "$expected"
  if test -n "${PLANERADAR_TEST_RENDER_IS_CHARACTER:-}"; then
    test -e "$3"
    test ! -L "$3"
    exit
  fi
  exec /usr/bin/test -c "$3"
fi
if test "${1-}" = install && test "${2-}" = -d; then
  "$@"
  target="${@: -1}"
  printf 'root:root\t%s\n' "$target" >> "$PLANERADAR_TEST_OWNERSHIP"
  exit
fi
if test "${1-}" = chown; then
  printf 'sudo chown %s\n' "${*:2}" >> "$PLANERADAR_TEST_LOG"
  shift
  recursive=false
  if test "${1-}" = -R; then
    recursive=true
    shift
  fi
  owner="$1"
  shift
  for target in "$@"; do
    if "$recursive"; then
      while IFS= read -r -d '' entry; do
        printf '%s\t%s\n' "$owner" "$entry" >> "$PLANERADAR_TEST_OWNERSHIP"
      done < <(find "$target" -print0)
    else
      printf '%s\t%s\n' "$owner" "$target" >> "$PLANERADAR_TEST_OWNERSHIP"
    fi
  done
  exit
fi
if test "${1-}" = -u; then
  printf 'sudo -u %s\n' "${*:2}" >> "$PLANERADAR_TEST_LOG"
  shift 2
fi
exec "$@"
"#,
        );
        let real_mv = String::from_utf8(
            Command::new("which")
                .arg("mv")
                .output()
                .expect("find mv")
                .stdout,
        )
        .expect("mv path")
        .trim()
        .to_owned();
        write_executable(
            &bin.join("mv"),
            &format!(
                r#"#!/usr/bin/env bash
set -euo pipefail
source_path="${{@: -2:1}}"
destination="${{@: -1}}"
if test -d "$destination"; then
  final_path="$destination/$(basename "$source_path")"
else
  final_path="$destination"
fi
'{real_mv}' "$@"
if test -f "$PLANERADAR_TEST_OWNERSHIP"; then
  temporary="$PLANERADAR_TEST_OWNERSHIP.tmp.$$"
  awk -F '\t' -v OFS='\t' -v old="$source_path" -v new="$final_path" '
    {{
      path = $2
      if (path == old) path = new
      else if (index(path, old "/") == 1) path = new substr(path, length(old) + 1)
      print $1, path
    }}
  ' "$PLANERADAR_TEST_OWNERSHIP" > "$temporary"
  '{real_mv}' "$temporary" "$PLANERADAR_TEST_OWNERSHIP"
fi
"#
            ),
        );
        let real_diff = String::from_utf8(
            Command::new("which")
                .arg("diff")
                .output()
                .expect("find diff")
                .stdout,
        )
        .expect("diff path")
        .trim()
        .to_owned();
        write_executable(
            &bin.join("diff"),
            &format!(
                r#"#!/usr/bin/env bash
set -euo pipefail
if test -n "${{PLANERADAR_TEST_GUARD_UNSAFE_DIFF:-}}"; then
  left="${{@: -2:1}}"
  right="${{@: -1}}"
  for tree in "$left" "$right"; do
    if test -n "$(find "$tree" -type l -print -quit)" ||
       test -n "$(find "$tree" ! -type d ! -type f -print -quit)"
    then
      cat "$PLANERADAR_TEST_OUTSIDE_SENTINEL" >/dev/null
      mkdir -p "$PLANERADAR_TEST_ROOT/tmp"
      : > "$PLANERADAR_TEST_ROOT/tmp/unsafe-diff-invoked"
      exit 86
    fi
  done
fi
exec '{real_diff}' "$@"
"#
            ),
        );
        write_executable(
            &bin.join("apt-get"),
            r#"#!/usr/bin/env bash
set -euo pipefail
printf 'apt-get %s\n' "$*" >> "$PLANERADAR_TEST_LOG"
if test -n "${PLANERADAR_TEST_MUTATE_CONFIG_ON_APT:-}" &&
   test ! -e "$PLANERADAR_TEST_ROOT/tmp/apt-mutated"
then
  mkdir -p "$PLANERADAR_TEST_ROOT/tmp"
  : > "$PLANERADAR_TEST_ROOT/tmp/apt-mutated"
  printf '# concurrent mutation\n' >> "$PLANERADAR_TEST_ROOT/boot/firmware/config.txt"
fi
"#,
        );
        write_executable(
            &bin.join("depmod"),
            "#!/usr/bin/env bash\nprintf 'depmod %s\\n' \"$*\" >> \"$PLANERADAR_TEST_LOG\"\n",
        );
        write_executable(
            &bin.join("stat"),
            r#"#!/usr/bin/env bash
set -euo pipefail
if test "${1-}" = -c; then
  case "$2" in
    %a)
      module="$PLANERADAR_TEST_ROOT/lib/modules/$PLANERADAR_TEST_RELEASE/extra/planeradar_hyperpixel2r.ko"
      if test -n "${PLANERADAR_TEST_MODULE_FILE_MODE:-}" && test "$3" = "$module"
      then
        printf '%s\n' "$PLANERADAR_TEST_MODULE_FILE_MODE"
        exit
      fi
      if test -n "${PLANERADAR_TEST_BOOT_FILE_MODE:-}" &&
         [[ "$3" == "$PLANERADAR_TEST_ROOT/boot/firmware/"* ]]
      then
        printf '%s\n' "$PLANERADAR_TEST_BOOT_FILE_MODE"
        exit
      fi
      if /usr/bin/stat -f '%Lp' "$3" >/dev/null 2>&1; then
        /usr/bin/stat -f '%Lp' "$3"
      else
        /usr/bin/stat -c '%a' "$3"
      fi
      ;;
    %U:%G)
      owner="$(
        awk -F '\t' -v path="$3" '$2 == path { owner = $1 } END { print owner }' \
          "$PLANERADAR_TEST_OWNERSHIP"
      )"
      printf '%s\n' "${owner:-shayne:staff}"
      ;;
    *) printf 'unsupported stat format: %s\n' "$2" >&2; exit 64 ;;
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
marker="$PLANERADAR_TEST_ROOT/var/lib/dkms/planeradar-hyperpixel2r/0.1.0/registered"
if test "${1-}" = status; then
  if test -n "${PLANERADAR_TEST_DKMS_STATUS+x}"; then
    printf '%s\n' "$PLANERADAR_TEST_DKMS_STATUS"
    exit "${PLANERADAR_TEST_DKMS_STATUS_EXIT:-0}"
  fi
  if test -f "$marker"; then
    printf 'planeradar-hyperpixel2r/0.1.0: added\n'
  fi
  exit 0
fi
if test "${1-}" = add; then
  mkdir -p "$(dirname "$marker")"
  : > "$marker"
  exit
fi
if test "${1-}" = remove; then
  test "$*" = "remove -m planeradar-hyperpixel2r -v 0.1.0 --all"
  rm -f "$marker"
  exit
fi
exit 64
"#,
        );
        write_executable(
            &bin.join("uname"),
            &format!(
                "#!/usr/bin/env bash\n\
                 case \"${{1-}}\" in -m) echo aarch64;; -r) echo '{release}';; *) exit 64;; esac\n"
            ),
        );
        write_executable(
            &bin.join("lsmod"),
            r#"#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' \
  'Module Size Used_by' \
  'planeradar_hyperpixel2r 1 0' \
  'i2c_algo_bit 1 1' \
  'edt_ft5x06 1 0'
test -n "${PLANERADAR_TEST_OMIT_VC4_MODULE:-}" || printf '%s\n' 'vc4 1 0'
test -n "${PLANERADAR_TEST_OMIT_V3D_MODULE:-}" || printf '%s\n' 'v3d 1 0'
"#,
        );
        write_executable(
            &bin.join("evtest"),
            r#"#!/usr/bin/env bash
set -euo pipefail
printf 'evtest %s\n' "$*" >> "$PLANERADAR_TEST_LOG"
if test "${1-}" = --info; then
  printf '%s\n' "evtest: unrecognized option '--info'" >&2
  exit 1
fi
test "$#" -eq 1
device="$(basename "$1")"
expected="${PLANERADAR_TEST_EXPECT_EVENT:-event0}"
if test "$device" != "$expected"; then
  printf 'evtest: unexpected touch device: %s (expected %s)\n' "$device" "$expected" >&2
  exit 66
fi
mode="${PLANERADAR_TEST_EVTEST_MODE:-valid}"
if test "$mode" = command-error; then
  printf 'evtest: cannot open %s: fixture device error\n' "$1" >&2
  exit 70
fi
if test "$mode" = no-device; then
  printf 'evtest: cannot open %s: No such file or directory\n' "$1" >&2
  exit 1
fi
device_line='Input device name: "EDT FT5406"'
case "$mode" in
  wrong-device) device_line='Input device name: "unrelated touchscreen"' ;;
  prefixed-device) device_line='stale: Input device name: "EDT FT5406"' ;;
  suffixed-device) device_line='Input device name: "EDT FT5406" stale' ;;
esac
printf '%s\n' \
  'Input driver version is 1.0.1' \
  'Input device ID: bus 0x18 vendor 0x0 product 0x0 version 0x0' \
  "$device_line" \
  'Supported events:' \
  '  Event type 0 (EV_SYN)' \
  '  Event type 1 (EV_KEY)' \
  '    Event code 330 (BTN_TOUCH)' \
  '  Event type 3 (EV_ABS)'
case "$mode" in
  valid|wrong-device|prefixed-device|suffixed-device)
    printf '%s\n' \
      '    Event code 53 (ABS_MT_POSITION_X)' \
      '      Value      0' \
      '      Min        0' \
      '      Max      479' \
      '    Event code 54 (ABS_MT_POSITION_Y)' \
      '      Value      0' \
      '      Min        0' \
      '      Max      480'
    ;;
  missing-axes)
    printf '%s\n' \
      '    Event code 53 (ABS_MT_POSITION_X)' \
      '      Value      0' \
      '      Min        0' \
      '      Max      479'
    ;;
  legacy-axes)
    printf '%s\n' \
      '    Event code 0 (ABS_X)' \
      '      Value      0' \
      '      Min        0' \
      '      Max      479' \
      '    Event code 1 (ABS_Y)' \
      '      Value      0' \
      '      Min        0' \
      '      Max      480'
    ;;
  invalid-maxima)
    printf '%s\n' \
      '    Event code 53 (ABS_MT_POSITION_X)' \
      '      Value      0' \
      '      Min        0' \
      '      Max     4095' \
      '    Event code 54 (ABS_MT_POSITION_Y)' \
      '      Value      0' \
      '      Min        0' \
      '      Max     4095'
    ;;
  *) printf 'unknown evtest fixture mode: %s\n' "$mode" >&2; exit 64 ;;
esac
printf '%s\n' 'Properties:' 'Testing ... (interrupt to exit)'
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
            &format!(
                r#"#!/usr/bin/env bash
set -euo pipefail
if test "${{1-}}" = --signal=TERM &&
   test "${{2-}}" = --kill-after=1 &&
   test "${{3-}}" = 10 &&
   test "${{4-}}" = bash
then
  shift 3
  exec '{real_timeout}' --signal=TERM --kill-after=0.1 \
    "${{PLANERADAR_TEST_SDL_TIMEOUT_SECONDS:-0.65}}" "$@"
fi
if test "${{1-}}" = 10; then
  shift
  exec "$@"
fi
test "${{1-}}" = --signal=INT
test "${{2-}}" = --kill-after=1
test "${{3-}}" = 2
shift 3
set +e
"$@"
status=$?
set -e
test "$status" -eq 0 || exit "$status"
exit "${{PLANERADAR_TEST_EVTEST_TIMEOUT_STATUS:-124}}"
"#
            ),
        );
        write_executable(
            &bin.join("systemd-run"),
            r#"#!/usr/bin/env bash
set -euo pipefail
printf 'systemd-run %s\n' "$*" >> "$PLANERADAR_TEST_LOG"
test -z "${PLANERADAR_TEST_SYSTEMD_RUN_FAIL:-}" || exit 70
arguments=("$@")

require_arg() {
  local index="$1"
  local expected="$2"
  local actual="${arguments[index - 1]-}"
  if test "$actual" != "$expected"; then
    printf 'systemd-run argument %s must be %s, got %s\n' \
      "$index" "$expected" "${actual:-<missing>}" >&2
    exit 64
  fi
}

require_arg 1 --unit=planeradar-hyperpixel-checkpoint
require_arg 2 --collect
require_arg 3 --uid=shayne
require_arg 4 --property=StateDirectory=planeradar
require_arg 5 --property=StateDirectoryMode=0750
test "$#" -eq 19 || {
  printf 'systemd-run must receive exactly 19 arguments, got %s\n' "$#" >&2
  exit 64
}
require_arg 6 --property=AmbientCapabilities=CAP_NET_BIND_SERVICE
require_arg 7 --setenv=SDL_VIDEODRIVER=kmsdrm
require_arg 8 --setenv=SDL_RENDER_DRIVER=opengles2
require_arg 9 --setenv=RUST_LOG=info
require_arg 10 \
  "$PLANERADAR_TEST_ROOT/usr/lib/planeradar/hyperpixel/$PLANERADAR_TEST_REVISION/$PLANERADAR_TEST_RELEASE/planeradar"
require_arg 11 run
require_arg 12 --settings
require_arg 13 "$PLANERADAR_TEST_ROOT/var/lib/planeradar/settings.json"
require_arg 14 --geocode-cache
require_arg 15 "$PLANERADAR_TEST_ROOT/var/lib/planeradar/geocode-cache.json"
require_arg 16 --debug-frame
require_arg 17 "$PLANERADAR_TEST_ROOT/var/lib/planeradar/debug.png"
require_arg 18 --http
require_arg 19 0.0.0.0:80

state_dir="$PLANERADAR_TEST_ROOT/var/lib/planeradar"
case "${PLANERADAR_TEST_STATE_DIRECTORY_MODE:-ready}" in
  ready)
    mkdir -p "$state_dir"
    chmod 0750 "$state_dir"
    printf 'shayne:shayne\t%s\n' "$state_dir" >> "$PLANERADAR_TEST_OWNERSHIP"
    ;;
  missing) ;;
  wrong)
    wrong_dir="$PLANERADAR_TEST_ROOT/var/lib/not-planeradar"
    mkdir -p "$wrong_dir"
    chmod 0750 "$wrong_dir"
    printf 'shayne:shayne\t%s\n' "$wrong_dir" >> "$PLANERADAR_TEST_OWNERSHIP"
    ;;
  unwritable)
    mkdir -p "$state_dir"
    chmod 0550 "$state_dir"
    printf 'root:root\t%s\n' "$state_dir" >> "$PLANERADAR_TEST_OWNERSHIP"
    ;;
  *)
    printf 'unknown state-directory fixture mode: %s\n' \
      "$PLANERADAR_TEST_STATE_DIRECTORY_MODE" >&2
    exit 64
    ;;
esac
mkdir -p "$PLANERADAR_TEST_ROOT/run"
: > "$PLANERADAR_TEST_ROOT/run/planeradar-hyperpixel-checkpoint.active"
"#,
        );
        write_executable(
            &bin.join("systemctl"),
            r#"#!/usr/bin/env bash
set -euo pipefail
marker="$PLANERADAR_TEST_ROOT/run/planeradar-hyperpixel-checkpoint.active"
case "${1-}" in
  show)
    test "$*" = "show planeradar-hyperpixel-checkpoint.service --property=MainPID --value"
    test -f "$marker"
    printf '4242\n'
    ;;
  --failed)
    test "$*" = "--failed --no-legend --plain"
    ;;
  stop)
    test "$*" = "stop planeradar-hyperpixel-checkpoint.service"
    test -f "$marker"
    printf 'systemctl stop %s\n' "${2-}" >> "$PLANERADAR_TEST_LOG"
    rm -f "$marker"
    ;;
  *) exit 64 ;;
esac
"#,
        );
        write_executable(
            &bin.join("curl"),
            r#"#!/usr/bin/env bash
set -euo pipefail
printf 'curl %s\n' "$*" >> "$PLANERADAR_TEST_LOG"
if test "$#" -ne 6 ||
   test "$1" != --fail ||
   test "$2" != --silent ||
   test "$3" != --show-error ||
   test "$4" != --header ||
   test "$5" != 'Host: planeradar.local' ||
   test "$6" != http://127.0.0.1/healthz
then
  printf '%s\n' 'curl: (22) The requested URL returned error: 403' >&2
  exit 22
fi
attempt_file="$PLANERADAR_TEST_ROOT/run/health-attempts"
attempt=0
test ! -f "$attempt_file" || read -r attempt < "$attempt_file"
attempt=$((attempt + 1))
printf '%s\n' "$attempt" > "$attempt_file"
case "${PLANERADAR_TEST_HEALTH_MODE:-ready}" in
  ready) ;;
  delayed)
    test "$attempt" -gt "${PLANERADAR_TEST_HEALTH_DELAY_ATTEMPTS:-2}" || exit 7
    ;;
  forbidden)
    printf '%s\n' 'curl: (22) The requested URL returned error: 403' >&2
    exit 22
    ;;
  unavailable) exit 7 ;;
  *) exit 64 ;;
esac
printf '{"revision":"%s"}\n' "${PLANERADAR_TEST_HEALTH_REVISION:-$PLANERADAR_TEST_REVISION}"
"#,
        );
        write_executable(
            &bin.join("journalctl"),
            r#"#!/usr/bin/env bash
set -euo pipefail
printf 'journalctl %s\n' "$*" >> "$PLANERADAR_TEST_LOG"
case "$*" in
  "-b -n 0 --show-cursor --no-pager")
    printf '%s\n' '-- cursor: fixture-cursor'
    ;;
  "-b -k --no-pager")
    printf '%s\n' "${PLANERADAR_TEST_VC4_V3D_LOG:-clean kernel boot log}"
    ;;
  *"--after-cursor=fixture-cursor"*|*"--after-cursor fixture-cursor"*)
    attempt_file="$PLANERADAR_TEST_ROOT/run/sdl-journal-attempts"
    attempt=0
    test ! -f "$attempt_file" || read -r attempt < "$attempt_file"
    attempt=$((attempt + 1))
    printf '%s\n' "$attempt" > "$attempt_file"
    if test "$attempt" -gt 20; then
      printf '%s\n' 'fixture guard: SDL readiness polling exceeded 20 snapshots' >&2
      exit 75
    fi
    mode="${PLANERADAR_TEST_SDL_LOG_MODE:-uppercase}"
    test -z "${PLANERADAR_TEST_STALE_LOG_ONLY:-}" || mode=stale
    case "$mode" in
      uppercase)
        printf '%s\n' \
          'Jul 27 01:34:28 planeradar planeradar[2798]: [2026-07-27T05:34:28Z INFO  planeradar::display] SDL display ready: video_driver=KMSDRM render_driver=opengles2'
        ;;
      lowercase)
        printf '%s\n' \
          'Jul 27 01:34:28 planeradar planeradar[2798]: [2026-07-27T05:34:28Z INFO  planeradar::display] SDL display ready: video_driver=kmsdrm render_driver=opengles2'
        ;;
      delayed)
        if test "$attempt" -gt "${PLANERADAR_TEST_SDL_DELAY_ATTEMPTS:-1}"; then
          printf '%s\n' \
            'Jul 27 01:34:29 planeradar planeradar[2798]: [2026-07-27T05:34:29Z INFO  planeradar::display] SDL display ready: video_driver=KMSDRM render_driver=opengles2'
        else
          printf '%s\n' 'new invocation has not initialized SDL yet'
        fi
        ;;
      late-after-deadline)
        if test "$attempt" -gt 8; then
          printf '%s\n' \
            'Jul 27 01:34:39 planeradar planeradar[2798]: [2026-07-27T05:34:39Z INFO  planeradar::display] SDL display ready: video_driver=KMSDRM render_driver=opengles2'
        else
          printf '%s\n' 'new invocation will initialize SDL after the readiness deadline'
        fi
        ;;
      never)
        printf '%s\n' 'new invocation never initialized SDL'
        ;;
      prefixed)
        printf '%s\n' \
          'Jul 27 01:34:28 planeradar planeradar[2798]: [2026-07-27T05:34:28Z INFO  planeradar::display] stale: SDL display ready: video_driver=kmsdrm render_driver=opengles2'
        ;;
      suffixed)
        printf '%s\n' \
          'Jul 27 01:34:28 planeradar planeradar[2798]: [2026-07-27T05:34:28Z INFO  planeradar::display] SDL display ready: video_driver=kmsdrm render_driver=opengles2 stale'
        ;;
      wrong-video-driver)
        printf '%s\n' \
          'Jul 27 01:34:28 planeradar planeradar[2798]: [2026-07-27T05:34:28Z INFO  planeradar::display] SDL display ready: video_driver=kmsdrm-extra render_driver=opengles2'
        ;;
      mixed-case-video-driver)
        printf '%s\n' \
          'Jul 27 01:34:28 planeradar planeradar[2798]: [2026-07-27T05:34:28Z INFO  planeradar::display] SDL display ready: video_driver=KmsDrm render_driver=opengles2'
        ;;
      wrong-renderer)
        printf '%s\n' \
          'Jul 27 01:34:28 planeradar planeradar[2798]: [2026-07-27T05:34:28Z INFO  planeradar::display] SDL display ready: video_driver=KMSDRM render_driver=software'
        ;;
      stale) printf '%s\n' 'new invocation has no SDL readiness line' ;;
      *) printf 'unknown SDL log fixture mode: %s\n' "$mode" >&2; exit 64 ;;
    esac
    ;;
  *"-u planeradar-hyperpixel-checkpoint.service"*)
    printf '%s\n' \
      'Jul 27 01:34:27 planeradar planeradar[1964]: [2026-07-27T05:34:27Z INFO  planeradar::display] SDL display ready: video_driver=KMSDRM render_driver=opengles2'
    ;;
  "-b --no-pager")
    printf '%s\n' 'clean boot log'
    ;;
  *) printf 'unexpected journalctl command: %s\n' "$*" >&2; exit 64 ;;
esac
"#,
        );
        write_executable(
            &bin.join("kill"),
            r#"#!/usr/bin/env bash
set -euo pipefail
printf 'kill %s\n' "$*" >> "$PLANERADAR_TEST_LOG"
state_dir="$PLANERADAR_TEST_ROOT/var/lib/planeradar"
test -d "$state_dir" || {
  printf 'transient Plane Radar state directory is missing: %s\n' "$state_dir" >&2
  exit 73
}
state_owner="$(
  awk -F '\t' -v path="$state_dir" \
    '$2 == path { owner = $1 } END { print owner }' \
    "$PLANERADAR_TEST_OWNERSHIP"
)"
test "$state_owner" = shayne:shayne || {
  printf 'transient Plane Radar state directory is not owned by shayne:shayne: %s\n' \
    "${state_owner:-<unrecorded>}" >&2
  exit 73
}
state_mode="$(stat -c %a "$state_dir")"
test "$state_mode" = 750 || {
  printf 'transient Plane Radar state directory is not mode 0750: %s\n' \
    "$state_mode" >&2
  exit 73
}
if test -z "${PLANERADAR_TEST_SKIP_SIGNAL_FRAME:-}"; then
  printf 'app-frame-write shayne %s\n' \
    "$PLANERADAR_TEST_ROOT/var/lib/planeradar/debug.png" \
    >> "$PLANERADAR_TEST_LOG"
  cp "${PLANERADAR_TEST_PNG_SOURCE:-$PLANERADAR_TEST_VALID_PNG}" \
    "$PLANERADAR_TEST_ROOT/var/lib/planeradar/debug.png"
fi
"#,
        );
        write_executable(
            &bin.join("pngcheck"),
            r#"#!/usr/bin/env bash
set -euo pipefail
printf 'pngcheck %s\n' "$*" >> "$PLANERADAR_TEST_LOG"
test "${1-}" = -q
cmp -s "$2" "$PLANERADAR_TEST_VALID_PNG"
"#,
        );

        let original_path = std::env::var("PATH").expect("PATH");
        let fixture_path = format!("{}:{original_path}", bin.display());
        Self {
            _temporary: temporary,
            repository,
            driver,
            target_manifest,
            app,
            root,
            log,
            ownership,
            fixture_path,
            revision,
            source_tree,
            release,
            overlay_file,
            module,
            overlay,
            valid_png,
        }
    }

    fn command(&self, script: &str) -> Command {
        let mut command = Command::new(self.repository.join("scripts").join(script));
        command
            .env("PATH", &self.fixture_path)
            .env("PLANERADAR_DRIVER_ARTIFACT_DIR", &self.driver)
            .env("PLANERADAR_KERNEL_TARGET_MANIFEST", &self.target_manifest)
            .env("PLANERADAR_APP_ARTIFACT_DIR", &self.app)
            .env("PLANERADAR_TEST_ROOT", &self.root)
            .env("PLANERADAR_TEST_LOG", &self.log)
            .env("PLANERADAR_TEST_OWNERSHIP", &self.ownership)
            .env(
                "PLANERADAR_TEST_HOST_BINARY",
                self.repository.join("target/debug/planeradar"),
            )
            .env("PLANERADAR_TEST_RELEASE", self.release)
            .env("PLANERADAR_TEST_REVISION", &self.revision)
            .env("PLANERADAR_TEST_SOURCE_TREE", &self.source_tree)
            .env("PLANERADAR_TEST_VALID_PNG", &self.valid_png);
        command
    }

    fn stage(&self, environment: &[(&str, &str)]) -> Output {
        let mut command = self.command("stage-hyperpixel-tryboot.sh");
        command.args(["--parameter", "rotate=90"]);
        for (key, value) in environment {
            command.env(key, value);
        }
        command.output().expect("run staging script")
    }

    fn operator(&self, script: &str, environment: &[(&str, &str)]) -> Output {
        let mut command = self.command(script);
        for (key, value) in environment {
            command.env(key, value);
        }
        command
            .output()
            .unwrap_or_else(|error| panic!("run {script}: {error}"))
    }

    fn verify(&self, environment: &[(&str, &str)]) -> Output {
        let mut command = self.command("verify-hyperpixel-boot.sh");
        command.arg("--expect-tryboot");
        for (key, value) in environment {
            command.env(key, value);
        }
        command.output().expect("run verification script")
    }

    fn artifact_dir(&self) -> PathBuf {
        self.root
            .join("usr/lib/planeradar/hyperpixel")
            .join(&self.revision)
            .join(self.release)
    }

    fn retarget_revision(&mut self, revision: &str, source_tree: &str) {
        assert_eq!(revision.len(), 40, "fixture revision length");
        assert_eq!(source_tree.len(), 40, "fixture source tree length");
        let prior_revision = self.revision.clone();
        let prior_source_tree = self.source_tree.clone();
        let prior_overlay = self.overlay_file.clone();
        let overlay_file = format!("planeradar-hyperpixel2r-{}.dtbo", &revision[..12]);

        fs::rename(
            self.driver.join(&prior_overlay),
            self.driver.join(&overlay_file),
        )
        .expect("retarget overlay fixture");
        let manifest =
            fs::read_to_string(self.driver.join("manifest.txt")).expect("read manifest fixture");
        fs::write(
            self.driver.join("manifest.txt"),
            manifest
                .replace(
                    &format!("source_revision\t{prior_revision}"),
                    &format!("source_revision\t{revision}"),
                )
                .replace(
                    &format!("source_tree\t{prior_source_tree}"),
                    &format!("source_tree\t{source_tree}"),
                )
                .replace(
                    &format!("overlay_file\t{prior_overlay}"),
                    &format!("overlay_file\t{overlay_file}"),
                ),
        )
        .expect("retarget manifest fixture");
        fs::write(
            self.app.join("planeradar.revision"),
            format!("{revision}\n"),
        )
        .expect("retarget app revision");
        fs::write(self.app.join("planeradar.tree"), format!("{source_tree}\n"))
            .expect("retarget app source tree");

        self.revision = revision.to_owned();
        self.source_tree = source_tree.to_owned();
        self.overlay_file = overlay_file;
    }

    fn commands(&self) -> String {
        fs::read_to_string(&self.log).unwrap_or_default()
    }

    fn dkms_registration_marker(&self) -> PathBuf {
        self.root
            .join("var/lib/dkms/planeradar-hyperpixel2r/0.1.0/registered")
    }

    fn set_owner(&self, path: &Path, owner: &str) {
        let mut ownership = fs::OpenOptions::new()
            .append(true)
            .open(&self.ownership)
            .expect("open ownership model");
        writeln!(ownership, "{owner}\t{}", path.display()).expect("update ownership model");
    }

    fn owner(&self, path: &Path) -> String {
        fs::read_to_string(&self.ownership)
            .expect("read ownership model")
            .lines()
            .filter_map(|line| line.split_once('\t'))
            .filter(|(_, recorded_path)| *recorded_path == path.to_string_lossy())
            .map(|(owner, _)| owner.to_owned())
            .next_back()
            .unwrap_or_else(|| "shayne:staff".to_owned())
    }

    fn assert_tree_root_owned(&self, path: &Path) {
        assert_eq!(self.owner(path), "root:root", "owner of {}", path.display());
        if path.is_dir() {
            for entry in fs::read_dir(path).expect("read owned tree") {
                self.assert_tree_root_owned(&entry.expect("owned tree entry").path());
            }
        }
    }

    fn write_tryboot_flag(&self) {
        let flag = self.root.join("proc/device-tree/chosen/bootloader/tryboot");
        fs::create_dir_all(flag.parent().expect("tryboot flag parent"))
            .expect("tryboot flag directory");
        fs::write(flag, [0, 0, 0, 1]).expect("tryboot flag");
    }

    fn install_hardware_fixture(&self, touch_is_child: bool) {
        self.write_tryboot_flag();
        let bound = self
            .root
            .join("sys/devices/platform/planeradar-hyperpixel2r");
        fs::create_dir_all(&bound).expect("bound platform device");
        let driver = self
            .root
            .join("sys/bus/platform/drivers/planeradar-hyperpixel2r");
        fs::create_dir_all(&driver).expect("platform driver directory");
        symlink(&bound, driver.join("planeradar-hyperpixel2r.0")).expect("bound platform symlink");

        let connector = self.root.join("sys/class/drm/card0-DPI-1");
        fs::create_dir_all(&connector).expect("DRM connector fixture");
        fs::write(connector.join("status"), "connected\n").expect("connector status");
        fs::write(connector.join("modes"), "480x480\n").expect("connector mode");

        let input = if touch_is_child {
            bound.join("i2c-11/11-0015/input/input0")
        } else {
            self.root
                .join("sys/devices/platform/unrelated/i2c-4/4-0015/input/input0")
        };
        fs::create_dir_all(&input).expect("input device fixture");
        fs::write(input.join("name"), "EDT FT5406\n").expect("input name");
        let class_event = self.root.join("sys/class/input/event0");
        fs::create_dir_all(&class_event).expect("input class fixture");
        symlink(&input, class_event.join("device")).expect("input device symlink");
    }

    fn install_integrated_v3d_fixture(&self, status: &str, render_node: bool) {
        let compatible = self.root.join("proc/device-tree/compatible");
        fs::create_dir_all(compatible.parent().expect("compatible parent"))
            .expect("device-tree root");
        fs::write(compatible, b"raspberrypi,model-zero-2-w\0brcm,bcm2837\0")
            .expect("Zero 2 W compatible fixture");

        self.write_v3d_status("7ec00000", status);

        if render_node {
            let render = self.root.join("dev/dri/renderD128");
            fs::create_dir_all(render.parent().expect("render parent"))
                .expect("render-node directory");
            fs::write(render, b"fixture render node").expect("render-node fixture");
        }
    }

    fn write_v3d_status(&self, address: &str, status: &str) {
        let v3d = self
            .root
            .join("proc/device-tree/soc")
            .join(format!("v3d@{address}"));
        fs::create_dir_all(&v3d).expect("V3D device-tree node");
        fs::write(v3d.join("status"), format!("{status}\0")).expect("V3D status fixture");
    }

    fn set_normal_crlf_line(&self, bytes: usize, candidate: bool) {
        let declaration = if candidate {
            format!("dtoverlay={}", self.overlay_file.trim_end_matches(".dtbo"))
        } else {
            "dtoverlay=vc4-kms-dpi-hyperpixel2r".to_owned()
        };
        fs::write(
            self.root.join("boot/firmware/config.txt"),
            format!("[all]\r\n{}\r\n{declaration}\r\n", "x".repeat(bytes)),
        )
        .expect("CRLF boot config fixture");
    }
}

#[test]
fn published_bundle_is_root_owned_read_only_and_traversable_by_the_runtime_user() {
    let fixture = ScriptFixture::new();
    let output = fixture.stage(&[]);
    assert!(
        output.status.success(),
        "stage failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let artifact = fixture.artifact_dir();
    assert_eq!(mode(&artifact), 0o755, "published release directory");
    assert_eq!(mode(&artifact.join("dkms-source")), 0o755);
    assert_eq!(mode(&artifact.join("planeradar")), 0o755);
    for file in [
        "manifest.txt",
        "planeradar.revision",
        "planeradar.tree",
        "planeradar.sha256",
        "display-parameters.txt",
        "planeradar_hyperpixel2r.ko",
        "host-fixdep",
        "host-modpost",
        "host-genksyms",
        fixture.overlay_file.as_str(),
    ] {
        assert_eq!(mode(&artifact.join(file)), 0o644, "published {file}");
    }
    assert_eq!(
        fs::read(fixture.root.join(format!(
            "lib/modules/{}/extra/planeradar_hyperpixel2r.ko",
            fixture.release
        )))
        .expect("installed module"),
        fixture.module
    );
    assert_eq!(
        fs::read(
            fixture
                .root
                .join("boot/firmware/overlays")
                .join(&fixture.overlay_file)
        )
        .expect("installed overlay"),
        fixture.overlay
    );
    assert_eq!(
        mode(&fixture.root.join(format!(
            "lib/modules/{}/extra/planeradar_hyperpixel2r.ko",
            fixture.release
        ))),
        0o644
    );
    assert_eq!(
        mode(
            &fixture
                .root
                .join("boot/firmware/overlays")
                .join(&fixture.overlay_file)
        ),
        0o644
    );
    let commands = fixture.commands();
    assert!(
        commands.contains("sudo chown -R root:root"),
        "publication never established root ownership:\n{commands}"
    );
    fixture.assert_tree_root_owned(&artifact);
    fixture.assert_tree_root_owned(&fixture.root.join("usr/src/planeradar-hyperpixel2r-0.1.0"));
}

#[test]
fn staging_accepts_fat_reported_mode_only_for_the_boot_overlay() {
    let fixture = ScriptFixture::new();
    let output = fixture.stage(&[("PLANERADAR_TEST_BOOT_FILE_MODE", "755")]);
    assert!(
        output.status.success(),
        "stage rejected the FAT boot-file mode:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read(
            fixture
                .root
                .join("boot/firmware/overlays")
                .join(&fixture.overlay_file)
        )
        .expect("installed FAT-mode overlay"),
        fixture.overlay
    );
    assert_eq!(
        mode(&fixture.root.join(format!(
            "lib/modules/{}/extra/planeradar_hyperpixel2r.ko",
            fixture.release
        ))),
        0o644,
        "module mode must remain strict"
    );

    let executable_module_fixture = ScriptFixture::new();
    let executable_module_output =
        executable_module_fixture.stage(&[("PLANERADAR_TEST_MODULE_FILE_MODE", "755")]);
    assert!(
        !executable_module_output.status.success(),
        "stage accepted a module reported as mode 0755"
    );
    assert!(
        !executable_module_fixture
            .root
            .join("boot/firmware/overlays")
            .join(&executable_module_fixture.overlay_file)
            .exists(),
        "module mode failure must occur before the boot overlay is installed"
    );
    assert!(
        !executable_module_fixture
            .root
            .join("boot/firmware/tryboot.txt")
            .exists(),
        "module mode failure must occur before tryboot.txt"
    );

    let writable_fixture = ScriptFixture::new();
    let writable_output = writable_fixture.stage(&[("PLANERADAR_TEST_BOOT_FILE_MODE", "775")]);
    assert!(
        !writable_output.status.success(),
        "stage accepted a group-writable boot-overlay mode"
    );
    assert!(
        !writable_fixture
            .root
            .join("boot/firmware/tryboot.txt")
            .exists(),
        "failed mode validation must not publish tryboot.txt"
    );
}

#[test]
fn staging_rejects_missing_tampered_extra_or_unbound_driver_evidence_before_transfer() {
    let mut accepted = Vec::new();
    for case in [
        "missing-helper",
        "tampered-helper",
        "extra-helper",
        "missing-provenance",
        "tampered-source-identity",
        "unsupported-host-arch",
    ] {
        let fixture = ScriptFixture::new();
        match case {
            "missing-helper" => {
                fs::remove_file(fixture.driver.join("host-fixdep"))
                    .expect("remove required helper evidence");
            }
            "tampered-helper" => {
                fs::write(
                    fixture.driver.join("host-modpost"),
                    b"tampered helper bytes",
                )
                .expect("tamper helper evidence");
            }
            "extra-helper" => {
                fs::write(fixture.driver.join("host-unexpected"), b"unexpected helper")
                    .expect("add unexpected helper evidence");
            }
            "missing-provenance" => {
                let manifest = fs::read_to_string(fixture.driver.join("manifest.txt"))
                    .expect("read manifest fixture");
                let without_source_checksum = manifest
                    .lines()
                    .filter(|line| !line.starts_with("kernel_source_deb_sha256\t"))
                    .collect::<Vec<_>>()
                    .join("\n");
                fs::write(
                    fixture.driver.join("manifest.txt"),
                    format!("{without_source_checksum}\n"),
                )
                .expect("remove required provenance row");
            }
            "tampered-source-identity" => {
                let manifest = fs::read_to_string(fixture.driver.join("manifest.txt"))
                    .expect("read manifest fixture");
                fs::write(
                    fixture.driver.join("manifest.txt"),
                    manifest.replace(
                        &format!("kernel_source_deb_sha256\t{}", "3".repeat(64)),
                        &format!("kernel_source_deb_sha256\t{}", "9".repeat(64)),
                    ),
                )
                .expect("tamper source identity");
            }
            "unsupported-host-arch" => {
                let manifest = fs::read_to_string(fixture.driver.join("manifest.txt"))
                    .expect("read manifest fixture");
                fs::write(
                    fixture.driver.join("manifest.txt"),
                    manifest.replace("build_host_arch\taarch64", "build_host_arch\tmips64"),
                )
                .expect("tamper build host architecture");
            }
            _ => unreachable!(),
        }

        let output = fixture.stage(&[]);
        if output.status.success() {
            accepted.push(case);
        }
        assert!(
            !fixture.commands().contains("scp "),
            "{case} reached transfer before local artifact validation"
        );
    }
    assert!(
        accepted.is_empty(),
        "invalid driver evidence was accepted: {accepted:?}"
    );
}

#[test]
fn repeated_stage_rejects_nested_package_and_dkms_ownership_drift() {
    for relative in [
        "usr/lib/package/planeradar",
        "usr/lib/package/manifest.txt",
        "usr/src/planeradar-hyperpixel2r-0.1.0/dkms.conf",
    ] {
        let fixture = ScriptFixture::new();
        let first = fixture.stage(&[]);
        assert!(
            first.status.success(),
            "initial stage for {relative} failed: {}",
            String::from_utf8_lossy(&first.stderr)
        );
        let drifted = match relative.strip_prefix("usr/lib/package/") {
            Some(package_path) => fixture.artifact_dir().join(package_path),
            None => fixture.root.join(relative),
        };
        fixture.set_owner(&drifted, "shayne:staff");

        let repeated = fixture.stage(&[]);
        assert!(
            !repeated.status.success(),
            "reused tree accepted nested ownership drift at {}",
            drifted.display()
        );
        assert_eq!(
            fixture.owner(&drifted),
            "shayne:staff",
            "repeat mutated the drifted object instead of rejecting it"
        );
    }
}

#[test]
fn reused_trees_reject_nested_symlinks_and_special_files_before_content_diff() {
    for (tree, kind) in [
        ("package", "symlink"),
        ("package", "socket"),
        ("dkms", "symlink"),
        ("dkms", "socket"),
    ] {
        let fixture = ScriptFixture::new();
        let first = fixture.stage(&[]);
        assert!(
            first.status.success(),
            "initial stage for {tree} {kind} failed: {}",
            String::from_utf8_lossy(&first.stderr)
        );

        let target = if tree == "package" {
            fixture.artifact_dir().join("manifest.txt")
        } else {
            fixture
                .root
                .join("usr/src/planeradar-hyperpixel2r-0.1.0/dkms.conf")
        };
        fs::remove_file(&target).expect("remove regular entry before type drift");
        let outside = fixture.root.join("outside-sentinel");
        let outside_bytes = b"outside content must never be traversed\n";
        fs::write(&outside, outside_bytes).expect("outside sentinel");
        if kind == "symlink" {
            symlink(&outside, &target).expect("nested outside symlink");
        } else {
            let status = Command::new("mkfifo")
                .arg(&target)
                .status()
                .expect("create nested FIFO");
            assert!(status.success(), "mkfifo failed");
        }

        let outside_path = outside.to_str().expect("outside path");
        let repeated = fixture.stage(&[
            ("PLANERADAR_TEST_GUARD_UNSAFE_DIFF", "1"),
            ("PLANERADAR_TEST_OUTSIDE_SENTINEL", outside_path),
        ]);
        assert!(
            !repeated.status.success(),
            "reused {tree} accepted nested {kind}"
        );
        assert!(
            !fixture.root.join("tmp/unsafe-diff-invoked").exists(),
            "content diff was invoked before rejecting {tree} {kind}"
        );
        assert_eq!(
            fs::read(&outside).expect("outside sentinel after rejection"),
            outside_bytes
        );
    }
}

#[test]
fn dkms_empty_then_real_source_only_status_registers_exactly_once_across_repeated_stages() {
    let fixture = ScriptFixture::new();
    for attempt in 1..=2 {
        let output = fixture.stage(&[]);
        assert!(
            output.status.success(),
            "stage {attempt} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let commands = fixture.commands();
    assert_eq!(
        commands
            .matches("dkms add -m planeradar-hyperpixel2r -v 0.1.0")
            .count(),
        1,
        "DKMS must be added once after an exit-zero empty status:\n{commands}"
    );
    assert_eq!(
        commands
            .matches("dkms status -m planeradar-hyperpixel2r -v 0.1.0")
            .count(),
        2
    );
}

#[test]
fn dkms_nonzero_status_discards_valid_looking_stdout_and_registers() {
    let fixture = ScriptFixture::new();
    let output = fixture.stage(&[
        (
            "PLANERADAR_TEST_DKMS_STATUS",
            "planeradar-hyperpixel2r/0.1.0: added",
        ),
        ("PLANERADAR_TEST_DKMS_STATUS_EXIT", "69"),
    ]);
    assert!(
        output.status.success(),
        "stage rejected unreadable DKMS status: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let commands = fixture.commands();
    assert_eq!(
        commands
            .matches("dkms add -m planeradar-hyperpixel2r -v 0.1.0")
            .count(),
        1,
        "valid-looking stdout from failed status suppressed registration:\n{commands}"
    );
    assert!(
        fixture.dkms_registration_marker().is_file(),
        "stage completed without a registered fixed-version DKMS source"
    );
}

#[test]
fn staging_distinct_owned_revision_upgrades_the_fixed_version_dkms_source() {
    let mut fixture = ScriptFixture::new();
    let first = fixture.stage(&[]);
    assert!(
        first.status.success(),
        "revision A stage failed: {}",
        String::from_utf8_lossy(&first.stderr)
    );
    let revision_a_source = fixture
        .artifact_dir()
        .join("dkms-source/planeradar_hyperpixel2r_main.c");
    let revision_a_bytes = fs::read(&revision_a_source).expect("revision A source");

    fixture.retarget_revision(&"b".repeat(40), &"c".repeat(40));
    let second = fixture.stage(&[(
        "PLANERADAR_TEST_DISTINCT_DKMS_SOURCE",
        "/* committed revision B */",
    )]);
    assert!(
        second.status.success(),
        "legitimate revision A to B source upgrade failed: {}",
        String::from_utf8_lossy(&second.stderr)
    );

    let revision_b_source = fixture
        .artifact_dir()
        .join("dkms-source/planeradar_hyperpixel2r_main.c");
    let revision_b_bytes = fs::read(&revision_b_source).expect("revision B source");
    assert_ne!(
        revision_a_bytes, revision_b_bytes,
        "the two staged revisions did not carry distinct source"
    );
    let installed_source = fixture
        .root
        .join("usr/src/planeradar-hyperpixel2r-0.1.0/planeradar_hyperpixel2r_main.c");
    assert_eq!(
        fs::read(&installed_source).expect("upgraded fixed-version source"),
        revision_b_bytes,
        "fixed-version DKMS source did not advance to revision B"
    );
    fixture.assert_tree_root_owned(&fixture.root.join("usr/src/planeradar-hyperpixel2r-0.1.0"));
    assert!(
        fixture.dkms_registration_marker().is_file(),
        "revision B was not registered after the source upgrade"
    );

    let commands = fixture.commands();
    assert_eq!(
        commands
            .matches("dkms remove -m planeradar-hyperpixel2r -v 0.1.0 --all")
            .count(),
        1,
        "registered revision A was not removed exactly once:\n{commands}"
    );
    assert_eq!(
        commands
            .matches("dkms add -m planeradar-hyperpixel2r -v 0.1.0")
            .count(),
        2,
        "revision A and revision B were not each registered exactly once:\n{commands}"
    );
    let dkms_commands: Vec<_> = commands
        .lines()
        .filter(|line| line.starts_with("dkms "))
        .collect();
    assert_eq!(
        dkms_commands,
        [
            "dkms status -m planeradar-hyperpixel2r -v 0.1.0",
            "dkms add -m planeradar-hyperpixel2r -v 0.1.0",
            "dkms status -m planeradar-hyperpixel2r -v 0.1.0",
            "dkms remove -m planeradar-hyperpixel2r -v 0.1.0 --all",
            "dkms add -m planeradar-hyperpixel2r -v 0.1.0",
        ],
        "source upgrade did not unregister A before registering B"
    );
}

#[test]
fn staging_distinct_revision_allows_safe_dkms_source_shape_change() {
    let mut fixture = ScriptFixture::new();
    let first = fixture.stage(&[]);
    assert!(
        first.status.success(),
        "revision A stage failed: {}",
        String::from_utf8_lossy(&first.stderr)
    );

    fixture.retarget_revision(&"b".repeat(40), &"c".repeat(40));
    let second = fixture.stage(&[
        (
            "PLANERADAR_TEST_DISTINCT_DKMS_SOURCE",
            "/* committed revision B */",
        ),
        ("PLANERADAR_TEST_RESHAPE_DKMS_SOURCE", "1"),
    ]);
    assert!(
        second.status.success(),
        "safe revision B source-shape change failed: {}",
        String::from_utf8_lossy(&second.stderr)
    );

    let revision_b_source = fixture.artifact_dir().join("dkms-source");
    let installed_source = fixture.root.join("usr/src/planeradar-hyperpixel2r-0.1.0");
    assert_eq!(
        fs::read(installed_source.join("planeradar_hyperpixel2r_main.c"))
            .expect("installed revision B main source"),
        fs::read(revision_b_source.join("planeradar_hyperpixel2r_main.c"))
            .expect("revision B main source")
    );
    assert_eq!(
        fs::read(installed_source.join("planeradar_hyperpixel2r_revision_b.c"))
            .expect("installed revision B-only source"),
        b"/* revision B only */\n"
    );
    assert!(
        !installed_source.join("README.md").exists(),
        "revision A-only source entry survived the revision B replacement"
    );
    assert!(
        fixture.dkms_registration_marker().is_file(),
        "reshaped revision B source was not registered"
    );
}

#[test]
fn staging_distinct_revision_rejects_writable_installed_revision_root_before_unregistering() {
    let mut fixture = ScriptFixture::new();
    assert!(fixture.stage(&[]).status.success());

    let revision_dir = fixture
        .artifact_dir()
        .parent()
        .expect("installed revision directory")
        .to_path_buf();
    fs::set_permissions(&revision_dir, fs::Permissions::from_mode(0o775))
        .expect("make installed revision root writable");

    let installed_source = fixture
        .root
        .join("usr/src/planeradar-hyperpixel2r-0.1.0/planeradar_hyperpixel2r_main.c");
    let revision_a_bytes = fs::read(&installed_source).expect("revision A installed source");

    fixture.retarget_revision(&"b".repeat(40), &"c".repeat(40));
    let second = fixture.stage(&[(
        "PLANERADAR_TEST_DISTINCT_DKMS_SOURCE",
        "/* committed revision B */",
    )]);
    assert!(
        !second.status.success(),
        "writable installed revision root was trusted for source upgrade"
    );
    assert_eq!(
        fs::read(&installed_source).expect("rejected installed source"),
        revision_a_bytes,
        "rejected revision-root drift changed the fixed-version source"
    );
    assert_eq!(
        fixture
            .commands()
            .matches("dkms remove -m planeradar-hyperpixel2r -v 0.1.0 --all")
            .count(),
        0,
        "unsafe revision root triggered unregister"
    );
}

#[test]
fn staging_distinct_revision_rejects_unproven_dkms_source_before_unregistering() {
    let mut fixture = ScriptFixture::new();
    assert!(fixture.stage(&[]).status.success());
    let installed_source = fixture
        .root
        .join("usr/src/planeradar-hyperpixel2r-0.1.0/planeradar_hyperpixel2r_main.c");
    let mut drifted = fs::read(&installed_source).expect("installed source");
    drifted.extend_from_slice(b"\n/* arbitrary local mutation */\n");
    fs::write(&installed_source, &drifted).expect("mutate installed source");

    fixture.retarget_revision(&"b".repeat(40), &"c".repeat(40));
    let second = fixture.stage(&[(
        "PLANERADAR_TEST_DISTINCT_DKMS_SOURCE",
        "/* committed revision B */",
    )]);
    assert!(
        !second.status.success(),
        "arbitrarily mutated fixed-version source was accepted"
    );
    assert_eq!(
        fs::read(&installed_source).expect("rejected installed source"),
        drifted,
        "rejected source drift was modified"
    );
    assert!(
        !fixture
            .commands()
            .contains("dkms remove -m planeradar-hyperpixel2r -v 0.1.0 --all"),
        "unproven source was unregistered before rejection"
    );
}

#[test]
fn source_upgrade_rejects_mixed_dkms_status_before_unregistering() {
    let mut fixture = ScriptFixture::new();
    assert!(fixture.stage(&[]).status.success());
    fixture.retarget_revision(&"b".repeat(40), &"c".repeat(40));

    let second = fixture.stage(&[
        (
            "PLANERADAR_TEST_DISTINCT_DKMS_SOURCE",
            "/* committed revision B */",
        ),
        (
            "PLANERADAR_TEST_DKMS_STATUS",
            "planeradar-hyperpixel2r/0.1.0: added\nplaneradar-hyperpixel2r/0.1.0: broken",
        ),
    ]);
    assert!(
        !second.status.success(),
        "mixed exact and malformed DKMS status was accepted for source upgrade"
    );
    assert!(
        String::from_utf8_lossy(&second.stderr)
            .contains("refusing DKMS source upgrade with unrecognized status"),
        "source upgrade did not report the malformed DKMS status: {}",
        String::from_utf8_lossy(&second.stderr)
    );
    assert!(
        !fixture
            .commands()
            .contains("dkms remove -m planeradar-hyperpixel2r -v 0.1.0 --all"),
        "malformed DKMS status triggered unregister"
    );
}

#[test]
fn failed_stage_rolls_back_the_prior_dkms_source_and_registration() {
    let mut fixture = ScriptFixture::new();
    assert!(fixture.stage(&[]).status.success());
    let installed_source = fixture
        .root
        .join("usr/src/planeradar-hyperpixel2r-0.1.0/planeradar_hyperpixel2r_main.c");
    let revision_a_bytes = fs::read(&installed_source).expect("revision A installed source");

    fixture.retarget_revision(&"b".repeat(40), &"c".repeat(40));
    let failed = fixture.stage(&[
        (
            "PLANERADAR_TEST_DISTINCT_DKMS_SOURCE",
            "/* committed revision B */",
        ),
        ("PLANERADAR_TEST_FAIL_STAGE", "1"),
    ]);
    assert!(
        !failed.status.success(),
        "forced stage failure was accepted"
    );
    assert_eq!(
        fs::read(&installed_source).expect("rolled-back installed source"),
        revision_a_bytes,
        "failed revision B stage did not restore revision A source"
    );
    assert!(
        fixture.dkms_registration_marker().is_file(),
        "rollback did not restore revision A's DKMS registration"
    );

    let commands = fixture.commands();
    assert_eq!(
        commands
            .matches("dkms remove -m planeradar-hyperpixel2r -v 0.1.0 --all")
            .count(),
        2,
        "upgrade and rollback must each unregister once:\n{commands}"
    );
    assert_eq!(
        commands
            .matches("dkms add -m planeradar-hyperpixel2r -v 0.1.0")
            .count(),
        3,
        "revision A, revision B, and restored revision A must each register:\n{commands}"
    );
    let dkms_commands: Vec<_> = commands
        .lines()
        .filter(|line| line.starts_with("dkms "))
        .collect();
    assert_eq!(
        dkms_commands,
        [
            "dkms status -m planeradar-hyperpixel2r -v 0.1.0",
            "dkms add -m planeradar-hyperpixel2r -v 0.1.0",
            "dkms status -m planeradar-hyperpixel2r -v 0.1.0",
            "dkms remove -m planeradar-hyperpixel2r -v 0.1.0 --all",
            "dkms add -m planeradar-hyperpixel2r -v 0.1.0",
            "dkms remove -m planeradar-hyperpixel2r -v 0.1.0 --all",
            "dkms add -m planeradar-hyperpixel2r -v 0.1.0",
        ],
        "rollback did not replace A with B and then restore A in order"
    );
}

#[test]
fn dkms_exact_source_only_and_built_statuses_do_not_reregister() {
    for status in [
        "planeradar-hyperpixel2r/0.1.0: added",
        "planeradar-hyperpixel2r/0.1.0, 6.18.34+rpt-rpi-v8, aarch64: installed",
        "planeradar-hyperpixel2r/0.1.0, 6.12.47+rpt-rpi-v8, aarch64: built",
    ] {
        let fixture = ScriptFixture::new();
        for attempt in 1..=2 {
            let output = fixture.stage(&[("PLANERADAR_TEST_DKMS_STATUS", status)]);
            assert!(
                output.status.success(),
                "stage {attempt} rejected legitimate DKMS status {status}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        assert_eq!(
            fixture
                .commands()
                .matches("dkms add -m planeradar-hyperpixel2r -v 0.1.0")
                .count(),
            0,
            "exact DKMS membership was re-added for {status}"
        );
    }
}

#[test]
fn dkms_status_rejects_malformed_records_disallowed_statuses_and_substring_collisions() {
    for status in [
        "planeradar-hyperpixel2r/0.1.0: removed",
        "planeradar-hyperpixel2r/0.1.0: broken",
        "planeradar-hyperpixel2r/0.1.0: added trailing-data",
        "planeradar-hyperpixel2r/0.1.0, garbage",
        "planeradar-hyperpixel2r/0.1.0, 6.18.34+rpt-rpi-v8, aarch64: removed",
        "planeradar-hyperpixel2r/0.1.0, 6.18.34+rpt-rpi-v8, aarch64: installed trailing-data",
        "planeradar-hyperpixel2r/0.1.0, unsafe/release, aarch64: installed",
        "planeradar-hyperpixel2r/0.1.0, 6.18.34+rpt-rpi-v8, x86_64: installed",
        concat!(
            "planeradar-hyperpixel2r/0.1.0: added\n",
            "planeradar-hyperpixel2r/0.1.0: broken",
        ),
        concat!(
            "planeradar-hyperpixel2r-helper/0.1.0: added\n",
            "planeradar-hyperpixel2r/0.1.00: added\n",
            "other-planeradar-hyperpixel2r/0.1.0, 6.18.34+rpt-rpi-v8, aarch64: installed",
        ),
    ] {
        let fixture = ScriptFixture::new();
        let output = fixture.stage(&[("PLANERADAR_TEST_DKMS_STATUS", status)]);
        assert!(
            output.status.success(),
            "rejected DKMS status caused staging failure for {status}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            fixture
                .commands()
                .matches("dkms add -m planeradar-hyperpixel2r -v 0.1.0")
                .count(),
            1,
            "malformed or disallowed DKMS record satisfied membership: {status}"
        );
    }
}

#[test]
fn normal_config_is_snapshotted_before_package_mutation_and_concurrent_change_fails_closed() {
    let fixture = ScriptFixture::new();
    let tryboot = fixture.root.join("boot/firmware/tryboot.txt");
    let prior_tryboot = b"[all]\n# prior candidate\n";
    fs::write(&tryboot, prior_tryboot).expect("prior tryboot");
    fs::set_permissions(&tryboot, fs::Permissions::from_mode(0o600)).expect("prior tryboot mode");

    let output = fixture.stage(&[("PLANERADAR_TEST_MUTATE_CONFIG_ON_APT", "1")]);
    assert!(
        !output.status.success(),
        "stage accepted a normal config changed after its initial snapshot"
    );
    assert!(
        fs::read_to_string(fixture.root.join("boot/firmware/config.txt"))
            .expect("mutated normal config")
            .ends_with("# concurrent mutation\n"),
        "the script must not overwrite a concurrent normal-config edit"
    );
    assert_eq!(
        fs::read(&tryboot).expect("preserved tryboot"),
        prior_tryboot
    );
    assert_eq!(mode(&tryboot), 0o600);
    assert!(
        !fixture.artifact_dir().exists(),
        "publication must not begin after the early config change"
    );
}

#[test]
fn final_stage_boundary_rejects_a_cooperating_normal_config_writer_and_restores_tryboot() {
    let prior = ScriptFixture::new();
    let prior_tryboot = prior.root.join("boot/firmware/tryboot.txt");
    let prior_bytes = b"[all]\n# prior candidate\n";
    fs::write(&prior_tryboot, prior_bytes).expect("prior tryboot");
    fs::set_permissions(&prior_tryboot, fs::Permissions::from_mode(0o600))
        .expect("prior tryboot mode");

    let prior_output = prior.stage(&[("PLANERADAR_TEST_MUTATE_CONFIG_AT_FINAL_BOUNDARY", "1")]);
    assert!(
        !prior_output.status.success(),
        "stage reported success after a cooperating writer changed normal config"
    );
    assert!(
        !String::from_utf8_lossy(&prior_output.stdout).contains("sudo reboot '0 tryboot'"),
        "failed stage printed the tryboot reboot command"
    );
    assert!(
        fs::read_to_string(prior.root.join("boot/firmware/config.txt"))
            .expect("cooperatively changed normal config")
            .contains("dtoverlay=cooperating-writer"),
        "the cooperating writer did not execute"
    );
    assert_eq!(
        fs::read(&prior_tryboot).expect("restored prior tryboot"),
        prior_bytes
    );
    assert_eq!(mode(&prior_tryboot), 0o600);

    let new = ScriptFixture::new();
    let new_output = new.stage(&[("PLANERADAR_TEST_MUTATE_CONFIG_AT_FINAL_BOUNDARY", "1")]);
    assert!(
        !new_output.status.success(),
        "new tryboot stage reported success after final-boundary config drift"
    );
    assert!(
        !new.root.join("boot/firmware/tryboot.txt").exists(),
        "failed stage retained a newly created tryboot file"
    );
}

#[test]
fn atomic_install_rejects_directory_leaves_for_module_and_overlay() {
    for destination in ["module", "overlay"] {
        let fixture = ScriptFixture::new();
        let leaf = if destination == "module" {
            fixture.root.join(format!(
                "lib/modules/{}/extra/planeradar_hyperpixel2r.ko",
                fixture.release
            ))
        } else {
            fixture
                .root
                .join("boot/firmware/overlays")
                .join(&fixture.overlay_file)
        };
        fs::create_dir_all(&leaf).expect("directory-leaf fixture");
        let output = fixture.stage(&[]);
        assert!(
            !output.status.success(),
            "{destination} directory leaf was accepted"
        );
        assert!(leaf.is_dir(), "{destination} directory leaf was replaced");
        assert_eq!(
            fs::read_dir(&leaf).expect("directory leaf").count(),
            0,
            "{destination} temporary file was moved inside the directory leaf"
        );
    }
}

#[test]
fn verification_requires_a_fresh_complete_decodable_480x480_png() {
    let valid = ScriptFixture::new();
    assert!(valid.stage(&[]).status.success());
    valid.install_hardware_fixture(true);
    let output = valid.verify(&[]);
    assert!(
        output.status.success(),
        "valid PNG verification failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let truncated = ScriptFixture::new();
    assert!(truncated.stage(&[]).status.success());
    truncated.install_hardware_fixture(true);
    let truncated_path = truncated.root.join("truncated.png");
    let valid_bytes = fs::read(&truncated.valid_png).expect("valid PNG bytes");
    fs::write(&truncated_path, &valid_bytes[..24]).expect("truncated PNG");
    let truncated_output = truncated.verify(&[(
        "PLANERADAR_TEST_PNG_SOURCE",
        truncated_path.to_str().expect("truncated PNG path"),
    )]);
    assert!(
        !truncated_output.status.success(),
        "24-byte pseudo-PNG passed verification"
    );

    let corrupt = ScriptFixture::new();
    assert!(corrupt.stage(&[]).status.success());
    corrupt.install_hardware_fixture(true);
    let corrupt_path = corrupt.root.join("corrupt.png");
    let mut corrupt_bytes = fs::read(&corrupt.valid_png).expect("valid PNG bytes");
    let middle = corrupt_bytes.len() / 2;
    corrupt_bytes[middle] ^= 0x01;
    fs::write(&corrupt_path, corrupt_bytes).expect("corrupt PNG");
    let corrupt_output = corrupt.verify(&[(
        "PLANERADAR_TEST_PNG_SOURCE",
        corrupt_path.to_str().expect("corrupt PNG path"),
    )]);
    assert!(
        !corrupt_output.status.success(),
        "CRC-corrupt PNG passed verification"
    );

    let stale = ScriptFixture::new();
    assert!(stale.stage(&[]).status.success());
    stale.install_hardware_fixture(true);
    fs::create_dir_all(stale.root.join("var/lib/planeradar")).expect("stale state directory");
    fs::copy(
        &stale.valid_png,
        stale.root.join("var/lib/planeradar/debug.png"),
    )
    .expect("stale debug PNG");
    let stale_output = stale.verify(&[("PLANERADAR_TEST_SKIP_SIGNAL_FRAME", "1")]);
    assert!(
        !stale_output.status.success(),
        "a stale pre-launch PNG satisfied the fresh-frame check"
    );
    assert!(
        !stale.root.join("var/lib/planeradar/debug.png").exists(),
        "the stale pre-launch PNG was not removed"
    );
}

#[test]
fn verification_rejects_sdl_evidence_that_only_predates_the_new_launch() {
    let fixture = ScriptFixture::new();
    assert!(fixture.stage(&[]).status.success());
    fixture.install_hardware_fixture(true);
    let output = fixture.verify(&[("PLANERADAR_TEST_STALE_LOG_ONLY", "1")]);
    assert!(
        !output.status.success(),
        "stale boot-wide SDL readiness evidence was accepted"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains(
            "timed out waiting up to 10 seconds for current invocation exact KMSDRM/opengles2 readiness"
        ),
        "stale-only evidence did not take the bounded readiness-timeout path:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let commands = fixture.commands();
    assert!(
        commands.contains("systemctl stop planeradar-hyperpixel-checkpoint.service"),
        "stale-only readiness timeout bypassed service cleanup"
    );
    assert!(
        !commands.contains("kill -USR1"),
        "stale-only evidence reached debug-frame signaling"
    );
}

#[test]
fn verification_waits_for_delayed_current_invocation_sdl_readiness_before_signaling() {
    let fixture = ScriptFixture::new();
    assert!(fixture.stage(&[]).status.success());
    fixture.install_hardware_fixture(true);

    let output = fixture.verify(&[
        ("PLANERADAR_TEST_SDL_LOG_MODE", "delayed"),
        ("PLANERADAR_TEST_SDL_DELAY_ATTEMPTS", "1"),
    ]);

    assert!(
        output.status.success(),
        "the verifier cleaned up before delayed SDL readiness arrived:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(fixture.root.join("run/sdl-journal-attempts"))
            .expect("SDL journal attempt count"),
        "2\n",
        "the verifier did not require one empty post-cursor snapshot before readiness"
    );
    let commands = fixture.commands();
    let readiness_snapshot = commands
        .match_indices("--after-cursor=fixture-cursor")
        .nth(1)
        .expect("second post-cursor SDL journal snapshot")
        .0;
    let debug_signal = commands.find("kill -USR1").expect("debug-frame signal");
    assert!(
        readiness_snapshot < debug_signal,
        "SIGUSR1 was sent before the snapshot containing exact SDL readiness"
    );
}

#[test]
fn verification_times_out_when_current_invocation_sdl_readiness_never_beats_the_deadline() {
    for (mode, description) in [
        ("never", "missing readiness line"),
        (
            "late-after-deadline",
            "exact readiness line arriving after the deadline",
        ),
    ] {
        let fixture = ScriptFixture::new();
        assert!(fixture.stage(&[]).status.success());
        fixture.install_hardware_fixture(true);

        let output = fixture.verify(&[("PLANERADAR_TEST_SDL_LOG_MODE", mode)]);

        assert!(!output.status.success(), "{description} was accepted");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(
                "timed out waiting up to 10 seconds for current invocation exact KMSDRM/opengles2 readiness"
            ),
            "{description} did not produce the distinct readiness-timeout diagnostic:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let commands = fixture.commands();
        assert!(
            commands.contains("systemctl stop planeradar-hyperpixel-checkpoint.service"),
            "{description} bypassed service cleanup"
        );
        assert!(
            !commands.contains("kill -USR1"),
            "{description} reached debug-frame signaling"
        );
    }
}

#[test]
fn verification_accepts_only_exact_kmsdrm_opengles2_current_invocation_messages() {
    let lowercase = ScriptFixture::new();
    assert!(lowercase.stage(&[]).status.success());
    lowercase.install_hardware_fixture(true);
    let lowercase_output = lowercase.verify(&[("PLANERADAR_TEST_SDL_LOG_MODE", "lowercase")]);
    assert!(
        lowercase_output.status.success(),
        "exact lowercase kmsdrm readiness was rejected:\n{}",
        String::from_utf8_lossy(&lowercase_output.stderr)
    );
    assert_eq!(
        fs::read_to_string(lowercase.root.join("run/sdl-journal-attempts"))
            .expect("immediate SDL journal attempt count"),
        "1\n",
        "an immediate exact SDL readiness line required an unnecessary retry"
    );
    let lowercase_commands = lowercase.commands();
    assert!(
        lowercase_commands
            .find("--after-cursor=fixture-cursor")
            .expect("immediate post-cursor SDL journal snapshot")
            < lowercase_commands
                .find("kill -USR1")
                .expect("immediate debug-frame signal"),
        "SIGUSR1 was sent before immediate exact SDL readiness"
    );

    for (mode, description) in [
        ("prefixed", "prefixed readiness message"),
        ("suffixed", "suffixed readiness message"),
        ("wrong-video-driver", "near-match video driver"),
        ("mixed-case-video-driver", "unlisted video-driver spelling"),
        ("wrong-renderer", "wrong render driver"),
    ] {
        let fixture = ScriptFixture::new();
        assert!(fixture.stage(&[]).status.success());
        fixture.install_hardware_fixture(true);
        let output = fixture.verify(&[("PLANERADAR_TEST_SDL_LOG_MODE", mode)]);
        assert!(!output.status.success(), "{description} was accepted");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(
                "timed out waiting up to 10 seconds for current invocation exact KMSDRM/opengles2 readiness"
            ),
            "{description} did not produce the bounded readiness-timeout diagnostic:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let commands = fixture.commands();
        assert!(
            commands.contains("systemctl stop planeradar-hyperpixel-checkpoint.service"),
            "{description} bypassed service cleanup"
        );
        assert!(
            !commands.contains("kill -USR1"),
            "{description} reached debug-frame signaling"
        );
    }
}

#[test]
fn verification_requires_touch_to_descend_from_the_bound_custom_platform_device() {
    let unrelated = ScriptFixture::new();
    assert!(unrelated.stage(&[]).status.success());
    unrelated.install_hardware_fixture(false);
    let unrelated_output = unrelated.verify(&[]);
    assert!(
        !unrelated_output.status.success(),
        "an unrelated EDT device satisfied the HyperPixel touch check"
    );

    let child = ScriptFixture::new();
    assert!(child.stage(&[]).status.success());
    child.install_hardware_fixture(true);
    let child_output = child.verify(&[]);
    assert!(
        child_output.status.success(),
        "correctly parented touch device was rejected: {}",
        String::from_utf8_lossy(&child_output.stderr)
    );
}

#[test]
fn verification_uses_bounded_capture_mode_for_old_evtest_touch_capabilities() {
    let valid_timeout = ScriptFixture::new();
    assert!(valid_timeout.stage(&[]).status.success());
    valid_timeout.install_hardware_fixture(true);
    let valid_output = valid_timeout.verify(&[]);
    assert!(
        valid_output.status.success(),
        "valid old-evtest capability output followed by timeout was rejected:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&valid_output.stdout),
        String::from_utf8_lossy(&valid_output.stderr)
    );

    for (mode, diagnostic) in [
        (
            "missing-axes",
            "touch input capabilities are missing ABS_MT_POSITION_X or ABS_MT_POSITION_Y",
        ),
        (
            "invalid-maxima",
            "touch input axes do not report 479 or 480 maxima",
        ),
        (
            "command-error",
            "touch capability probe failed before its expected timeout",
        ),
        (
            "no-device",
            "touch event device became unavailable before capability probing",
        ),
        (
            "wrong-device",
            "touch capability output does not identify the ancestry-validated device",
        ),
        (
            "prefixed-device",
            "touch capability output does not identify the ancestry-validated device",
        ),
        (
            "suffixed-device",
            "touch capability output does not identify the ancestry-validated device",
        ),
        (
            "legacy-axes",
            "touch input capabilities are missing ABS_MT_POSITION_X or ABS_MT_POSITION_Y",
        ),
    ] {
        let fixture = ScriptFixture::new();
        assert!(fixture.stage(&[]).status.success());
        fixture.install_hardware_fixture(true);
        let output = fixture.verify(&[("PLANERADAR_TEST_EVTEST_MODE", mode)]);
        assert!(
            !output.status.success(),
            "{mode} touch capability evidence was accepted"
        );
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(diagnostic),
            "{mode} did not emit the actionable diagnostic:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            !fixture.commands().contains("systemd-run"),
            "{mode} launched the transient app after failed touch evidence"
        );
    }
}

#[test]
fn verification_accepts_only_proven_zero2_vc4_owned_v3d_without_a_v3d_module() {
    const VC4_V3D_BINDING: &str = "vc4-drm soc:gpu: bound 3fc00000.v3d (ops vc4_v3d_ops [vc4])";

    let integrated = ScriptFixture::new();
    assert!(integrated.stage(&[]).status.success());
    integrated.install_hardware_fixture(true);
    integrated.install_integrated_v3d_fixture("okay", true);
    let integrated_output = integrated.verify(&[
        ("PLANERADAR_TEST_OMIT_V3D_MODULE", "1"),
        ("PLANERADAR_TEST_RENDER_IS_CHARACTER", "1"),
        ("PLANERADAR_TEST_VC4_V3D_LOG", VC4_V3D_BINDING),
    ]);
    assert!(
        integrated_output.status.success(),
        "verified Zero 2 W VC4-owned V3D was rejected: {}",
        String::from_utf8_lossy(&integrated_output.stderr)
    );

    let no_render = ScriptFixture::new();
    assert!(no_render.stage(&[]).status.success());
    no_render.install_hardware_fixture(true);
    no_render.install_integrated_v3d_fixture("okay", false);
    assert!(
        !no_render
            .verify(&[
                ("PLANERADAR_TEST_OMIT_V3D_MODULE", "1"),
                ("PLANERADAR_TEST_VC4_V3D_LOG", VC4_V3D_BINDING),
            ])
            .status
            .success(),
        "integrated V3D without a render node was accepted"
    );

    let disabled = ScriptFixture::new();
    assert!(disabled.stage(&[]).status.success());
    disabled.install_hardware_fixture(true);
    disabled.install_integrated_v3d_fixture("disabled", true);
    assert!(
        !disabled
            .verify(&[
                ("PLANERADAR_TEST_OMIT_V3D_MODULE", "1"),
                ("PLANERADAR_TEST_VC4_V3D_LOG", VC4_V3D_BINDING),
            ])
            .status
            .success(),
        "disabled integrated V3D was accepted"
    );

    let unbound = ScriptFixture::new();
    assert!(unbound.stage(&[]).status.success());
    unbound.install_hardware_fixture(true);
    unbound.install_integrated_v3d_fixture("okay", true);
    assert!(
        !unbound
            .verify(&[
                ("PLANERADAR_TEST_OMIT_V3D_MODULE", "1"),
                ("PLANERADAR_TEST_RENDER_IS_CHARACTER", "1"),
            ])
            .status
            .success(),
        "enabled but unbound integrated V3D was accepted"
    );

    let wrong_owner = ScriptFixture::new();
    assert!(wrong_owner.stage(&[]).status.success());
    wrong_owner.install_hardware_fixture(true);
    wrong_owner.install_integrated_v3d_fixture("okay", true);
    assert!(
        !wrong_owner
            .verify(&[
                ("PLANERADAR_TEST_OMIT_V3D_MODULE", "1"),
                ("PLANERADAR_TEST_RENDER_IS_CHARACTER", "1"),
                (
                    "PLANERADAR_TEST_VC4_V3D_LOG",
                    "v3d 3fc00000.v3d: bound (ops vc4_v3d_ops [v3d])",
                ),
            ])
            .status
            .success(),
        "V3D binding not owned by VC4 was accepted"
    );

    let missing_vc4 = ScriptFixture::new();
    assert!(missing_vc4.stage(&[]).status.success());
    missing_vc4.install_hardware_fixture(true);
    missing_vc4.install_integrated_v3d_fixture("okay", true);
    assert!(
        !missing_vc4
            .verify(&[
                ("PLANERADAR_TEST_OMIT_V3D_MODULE", "1"),
                ("PLANERADAR_TEST_OMIT_VC4_MODULE", "1"),
                ("PLANERADAR_TEST_VC4_V3D_LOG", VC4_V3D_BINDING),
            ])
            .status
            .success(),
        "integrated V3D without a loaded VC4 module was accepted"
    );

    let neither = ScriptFixture::new();
    assert!(neither.stage(&[]).status.success());
    neither.install_hardware_fixture(true);
    assert!(
        !neither
            .verify(&[("PLANERADAR_TEST_OMIT_V3D_MODULE", "1")])
            .status
            .success(),
        "verification accepted neither a V3D module nor integrated VC4 evidence"
    );
}

#[test]
fn verification_requires_the_exact_zero2_bcm2837_v3d_device_tree_node() {
    const VC4_V3D_BINDING: &str = "vc4-drm soc:gpu: bound 3fc00000.v3d (ops vc4_v3d_ops [vc4])";

    let unrelated_enabled = ScriptFixture::new();
    assert!(unrelated_enabled.stage(&[]).status.success());
    unrelated_enabled.install_hardware_fixture(true);
    unrelated_enabled.install_integrated_v3d_fixture("disabled", true);
    unrelated_enabled.write_v3d_status("deadbeef", "okay");
    let unrelated_output = unrelated_enabled.verify(&[
        ("PLANERADAR_TEST_OMIT_V3D_MODULE", "1"),
        ("PLANERADAR_TEST_RENDER_IS_CHARACTER", "1"),
        ("PLANERADAR_TEST_VC4_V3D_LOG", VC4_V3D_BINDING),
    ]);
    assert!(
        !unrelated_output.status.success(),
        "an unrelated enabled V3D node overrode the disabled exact BCM2837 node"
    );

    let malformed_status = ScriptFixture::new();
    assert!(malformed_status.stage(&[]).status.success());
    malformed_status.install_hardware_fixture(true);
    malformed_status.install_integrated_v3d_fixture("o\0kay", true);
    let malformed_status_output = malformed_status.verify(&[
        ("PLANERADAR_TEST_OMIT_V3D_MODULE", "1"),
        ("PLANERADAR_TEST_RENDER_IS_CHARACTER", "1"),
        ("PLANERADAR_TEST_VC4_V3D_LOG", VC4_V3D_BINDING),
    ]);
    assert!(
        !malformed_status_output.status.success(),
        "an embedded NUL was stripped to manufacture an enabled V3D status"
    );

    let wrong_board = ScriptFixture::new();
    assert!(wrong_board.stage(&[]).status.success());
    wrong_board.install_hardware_fixture(true);
    wrong_board.install_integrated_v3d_fixture("okay", true);
    fs::write(
        wrong_board.root.join("proc/device-tree/compatible"),
        b"raspberrypi,4-model-b\0brcm,bcm2711\0",
    )
    .expect("wrong-board compatible fixture");
    let wrong_board_output = wrong_board.verify(&[
        ("PLANERADAR_TEST_OMIT_V3D_MODULE", "1"),
        ("PLANERADAR_TEST_RENDER_IS_CHARACTER", "1"),
        ("PLANERADAR_TEST_VC4_V3D_LOG", VC4_V3D_BINDING),
    ]);
    assert!(
        !wrong_board_output.status.success(),
        "integrated V3D evidence from the wrong board was accepted"
    );
}

#[test]
fn verification_requires_a_non_symlink_character_render_node_for_integrated_v3d() {
    const VC4_V3D_BINDING: &str = "vc4-drm soc:gpu: bound 3fc00000.v3d (ops vc4_v3d_ops [vc4])";

    let simulated_character = ScriptFixture::new();
    assert!(simulated_character.stage(&[]).status.success());
    simulated_character.install_hardware_fixture(true);
    simulated_character.install_integrated_v3d_fixture("okay", true);
    let simulated_character_output = simulated_character.verify(&[
        ("PLANERADAR_TEST_OMIT_V3D_MODULE", "1"),
        ("PLANERADAR_TEST_RENDER_IS_CHARACTER", "1"),
        ("PLANERADAR_TEST_VC4_V3D_LOG", VC4_V3D_BINDING),
    ]);
    assert!(
        simulated_character_output.status.success(),
        "simulated non-symlink character render node was rejected: {}",
        String::from_utf8_lossy(&simulated_character_output.stderr)
    );

    let regular = ScriptFixture::new();
    assert!(regular.stage(&[]).status.success());
    regular.install_hardware_fixture(true);
    regular.install_integrated_v3d_fixture("okay", true);
    let regular_output = regular.verify(&[
        ("PLANERADAR_TEST_OMIT_V3D_MODULE", "1"),
        ("PLANERADAR_TEST_VC4_V3D_LOG", VC4_V3D_BINDING),
    ]);
    assert!(
        !regular_output.status.success(),
        "a regular file satisfied the character render-node requirement"
    );

    let linked = ScriptFixture::new();
    assert!(linked.stage(&[]).status.success());
    linked.install_hardware_fixture(true);
    linked.install_integrated_v3d_fixture("okay", false);
    let linked_target = linked.root.join("dev/dri/unrelated-render-device");
    fs::create_dir_all(linked_target.parent().expect("linked render parent"))
        .expect("linked render directory");
    fs::write(&linked_target, b"unrelated device").expect("linked render target");
    symlink(&linked_target, linked.root.join("dev/dri/renderD128")).expect("render-node symlink");
    let linked_output = linked.verify(&[
        ("PLANERADAR_TEST_OMIT_V3D_MODULE", "1"),
        ("PLANERADAR_TEST_RENDER_IS_CHARACTER", "1"),
        ("PLANERADAR_TEST_VC4_V3D_LOG", VC4_V3D_BINDING),
    ]);
    assert!(
        !linked_output.status.success(),
        "a symlink satisfied the character render-node requirement"
    );

    let missing = ScriptFixture::new();
    assert!(missing.stage(&[]).status.success());
    missing.install_hardware_fixture(true);
    missing.install_integrated_v3d_fixture("okay", false);
    let missing_output = missing.verify(&[
        ("PLANERADAR_TEST_OMIT_V3D_MODULE", "1"),
        ("PLANERADAR_TEST_RENDER_IS_CHARACTER", "1"),
        ("PLANERADAR_TEST_VC4_V3D_LOG", VC4_V3D_BINDING),
    ]);
    assert!(
        !missing_output.status.success(),
        "a missing render node satisfied the character-device requirement"
    );
}

#[test]
fn checksum_sidecars_reject_extra_absolute_and_traversal_entries_before_transfer() {
    let mut accepted = Vec::new();
    for case in ["extra", "absolute", "traversal"] {
        let fixture = ScriptFixture::new();
        match case {
            "extra" => {
                let manifest =
                    fs::read(fixture.driver.join("manifest.txt")).expect("manifest bytes");
                let mut checksum = fs::read_to_string(fixture.app.join("planeradar.sha256"))
                    .expect("app checksum");
                checksum.push_str(&format!("{}  manifest.txt\n", sha256_hex(&manifest)));
                fs::write(fixture.app.join("planeradar.sha256"), checksum)
                    .expect("extra checksum entry");
            }
            "absolute" => {
                let mut checksum = fs::read_to_string(fixture.driver.join("module.sha256"))
                    .expect("module checksum");
                checksum.push_str(&format!(
                    "{}  {}\n",
                    sha256_hex(&fixture.module),
                    fixture.driver.join("planeradar_hyperpixel2r.ko").display()
                ));
                fs::write(fixture.driver.join("module.sha256"), checksum)
                    .expect("absolute checksum entry");
            }
            "traversal" => {
                let mut checksum = fs::read_to_string(fixture.driver.join("module.sha256"))
                    .expect("module checksum");
                checksum.push_str(&format!(
                    "{}  ../driver/planeradar_hyperpixel2r.ko\n",
                    sha256_hex(&fixture.module)
                ));
                fs::write(fixture.driver.join("module.sha256"), checksum)
                    .expect("traversal checksum entry");
            }
            _ => unreachable!(),
        }
        if fixture.stage(&[]).status.success() {
            accepted.push(case);
        }
    }
    assert!(
        accepted.is_empty(),
        "unsafe checksum sidecars were accepted: {accepted:?}"
    );
}

#[test]
fn shell_line_limits_count_crlf_like_the_rust_boot_config_validator() {
    let mut failures = Vec::new();

    let stage_98 = ScriptFixture::new();
    stage_98.set_normal_crlf_line(98, false);
    if !stage_98.stage(&[]).status.success() {
        failures.push("stage rejected 98-byte CRLF line");
    }

    let stage_99 = ScriptFixture::new();
    stage_99.set_normal_crlf_line(99, false);
    if stage_99.stage(&[]).status.success() {
        failures.push("stage accepted 99-byte CRLF line");
    }

    let commit_98 = ScriptFixture::new();
    assert!(commit_98.stage(&[]).status.success());
    commit_98.set_normal_crlf_line(98, false);
    if !commit_98
        .operator("commit-hyperpixel-boot.sh", &[])
        .status
        .success()
    {
        failures.push("commit rejected 98-byte CRLF line");
    }

    let commit_99 = ScriptFixture::new();
    assert!(commit_99.stage(&[]).status.success());
    commit_99.set_normal_crlf_line(99, false);
    if commit_99
        .operator("commit-hyperpixel-boot.sh", &[])
        .status
        .success()
    {
        failures.push("commit accepted 99-byte CRLF line");
    }

    let rollback_98 = ScriptFixture::new();
    assert!(rollback_98.stage(&[]).status.success());
    rollback_98.set_normal_crlf_line(98, true);
    if !rollback_98
        .operator("rollback-hyperpixel-boot.sh", &[])
        .status
        .success()
    {
        failures.push("rollback rejected 98-byte CRLF line");
    }

    let rollback_99 = ScriptFixture::new();
    assert!(rollback_99.stage(&[]).status.success());
    rollback_99.set_normal_crlf_line(99, true);
    if rollback_99
        .operator("rollback-hyperpixel-boot.sh", &[])
        .status
        .success()
    {
        failures.push("rollback accepted 99-byte CRLF line");
    }

    assert!(failures.is_empty(), "{}", failures.join("; "));
}

#[test]
fn runtime_fixture_enforces_allowlisted_health_readiness_and_cleanup_contracts() {
    let fixture = ScriptFixture::new();
    assert!(fixture.stage(&[]).status.success());
    fixture.install_hardware_fixture(true);
    let state_dir = fixture.root.join("var/lib/planeradar");
    assert!(
        !state_dir.exists(),
        "runtime fixture must begin without a pre-created state directory"
    );
    let verified = fixture.verify(&[]);
    assert!(
        verified.status.success(),
        "positive runtime verification failed: {}",
        String::from_utf8_lossy(&verified.stderr)
    );
    let expected_launch = format!(
        concat!(
            "systemd-run --unit=planeradar-hyperpixel-checkpoint --collect --uid=shayne ",
            "--property=StateDirectory=planeradar ",
            "--property=StateDirectoryMode=0750 ",
            "--property=AmbientCapabilities=CAP_NET_BIND_SERVICE ",
            "--setenv=SDL_VIDEODRIVER=kmsdrm --setenv=SDL_RENDER_DRIVER=opengles2 ",
            "--setenv=RUST_LOG=info {}/planeradar run ",
            "--settings {}/var/lib/planeradar/settings.json ",
            "--geocode-cache {}/var/lib/planeradar/geocode-cache.json ",
            "--debug-frame {}/var/lib/planeradar/debug.png --http 0.0.0.0:80"
        ),
        fixture.artifact_dir().display(),
        fixture.root.display(),
        fixture.root.display(),
        fixture.root.display(),
    );
    let commands = fixture.commands();
    assert!(
        commands.contains(&expected_launch),
        "wrong launch:\n{commands}"
    );
    assert!(
        commands.contains(
            "curl --fail --silent --show-error --header Host: planeradar.local http://127.0.0.1/healthz"
        ),
        "health probe did not use loopback transport with the fixed allowlisted Host:\n{commands}"
    );
    assert!(
        commands.contains("systemctl stop planeradar-hyperpixel-checkpoint.service"),
        "transient service was not stopped:\n{commands}"
    );
    assert!(
        commands.contains("pngcheck -q"),
        "complete PNG decoder was not invoked:\n{commands}"
    );
    assert!(
        commands.contains(&format!(
            "app-frame-write shayne {}",
            state_dir.join("debug.png").display()
        )),
        "the app user never wrote a fresh frame into systemd-managed state:\n{commands}"
    );
    assert!(state_dir.is_dir(), "cleanup removed persistent state");
    assert_eq!(mode(&state_dir), 0o750, "persistent state mode");
    assert_eq!(
        fixture.owner(&state_dir),
        "shayne:shayne",
        "persistent state owner"
    );
    assert!(
        state_dir.join("debug.png").is_file(),
        "cleanup removed the verified debug frame"
    );
    assert!(
        !fixture
            .root
            .join("run/planeradar-hyperpixel-checkpoint.active")
            .exists(),
        "transient service remained active"
    );

    let delayed = ScriptFixture::new();
    assert!(delayed.stage(&[]).status.success());
    delayed.install_hardware_fixture(true);
    let delayed_output = delayed.verify(&[
        ("PLANERADAR_TEST_HEALTH_MODE", "delayed"),
        ("PLANERADAR_TEST_HEALTH_DELAY_ATTEMPTS", "2"),
    ]);
    assert!(
        delayed_output.status.success(),
        "health that became ready within the bounded retry window was rejected:\n{}",
        String::from_utf8_lossy(&delayed_output.stderr)
    );
    assert_eq!(
        fs::read_to_string(delayed.root.join("run/health-attempts"))
            .expect("delayed health attempt count"),
        "3\n",
        "health readiness was not retried exactly through the delayed success"
    );
    assert!(
        delayed
            .commands()
            .contains("systemctl stop planeradar-hyperpixel-checkpoint.service"),
        "delayed health success bypassed service cleanup"
    );

    for (mode, description) in [
        ("forbidden", "HTTP 403 health response"),
        ("unavailable", "health endpoint that never became ready"),
    ] {
        let failed = ScriptFixture::new();
        assert!(failed.stage(&[]).status.success());
        failed.install_hardware_fixture(true);
        let output = failed.verify(&[("PLANERADAR_TEST_HEALTH_MODE", mode)]);
        assert!(!output.status.success(), "{description} was accepted");
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .contains("Plane Radar health endpoint did not become ready"),
            "{description} did not emit the readiness diagnostic:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            failed
                .commands()
                .contains("systemctl stop planeradar-hyperpixel-checkpoint.service"),
            "{description} bypassed service cleanup"
        );
        assert!(
            !failed
                .root
                .join("run/planeradar-hyperpixel-checkpoint.active")
                .exists(),
            "{description} left the transient service active"
        );
    }

    for (state_mode, description) in [
        ("missing", "missing state directory"),
        ("wrong", "wrong state directory"),
        ("unwritable", "unwritable state directory"),
    ] {
        let failed = ScriptFixture::new();
        assert!(failed.stage(&[]).status.success());
        failed.install_hardware_fixture(true);
        let output = failed.verify(&[("PLANERADAR_TEST_STATE_DIRECTORY_MODE", state_mode)]);
        assert!(!output.status.success(), "{description} was accepted");
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .contains("transient Plane Radar state directory"),
            "{description} did not emit the state-directory diagnostic:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            failed
                .commands()
                .contains("systemctl stop planeradar-hyperpixel-checkpoint.service"),
            "{description} bypassed service cleanup"
        );
        assert!(
            !failed
                .root
                .join("run/planeradar-hyperpixel-checkpoint.active")
                .exists(),
            "{description} left the transient service active"
        );
        assert!(
            !failed.root.join("var/lib/planeradar/debug.png").exists(),
            "{description} produced a frame"
        );
    }

    let wrong_revision_fixture = ScriptFixture::new();
    assert!(wrong_revision_fixture.stage(&[]).status.success());
    wrong_revision_fixture.install_hardware_fixture(true);
    let wrong_revision = "f".repeat(40);
    let output =
        wrong_revision_fixture.verify(&[("PLANERADAR_TEST_HEALTH_REVISION", &wrong_revision)]);
    assert!(
        !output.status.success(),
        "wrong runtime health revision was accepted"
    );
    assert!(
        wrong_revision_fixture
            .commands()
            .contains("systemctl stop planeradar-hyperpixel-checkpoint.service"),
        "runtime failure bypassed service cleanup"
    );
    assert!(
        !wrong_revision_fixture
            .root
            .join("run/planeradar-hyperpixel-checkpoint.active")
            .exists(),
        "wrong runtime health revision left the transient service active"
    );
}

#[test]
fn staging_failure_cleanup_preserves_prior_mode_removes_new_tryboot_and_rejects_stale_package() {
    let prior = ScriptFixture::new();
    let tryboot = prior.root.join("boot/firmware/tryboot.txt");
    let prior_bytes = b"[all]\n# prior tryboot\n";
    fs::write(&tryboot, prior_bytes).expect("prior tryboot");
    fs::set_permissions(&tryboot, fs::Permissions::from_mode(0o600)).expect("prior tryboot mode");
    let failed = prior.stage(&[("PLANERADAR_TEST_FAIL_STAGE", "1")]);
    assert!(!failed.status.success());
    assert_eq!(
        fs::read(&tryboot).expect("restored prior tryboot"),
        prior_bytes
    );
    assert_eq!(mode(&tryboot), 0o600);

    let new = ScriptFixture::new();
    let failed = new.stage(&[("PLANERADAR_TEST_FAIL_STAGE", "1")]);
    assert!(!failed.status.success());
    assert!(
        !new.root.join("boot/firmware/tryboot.txt").exists(),
        "failed stage left a new tryboot candidate"
    );

    let stale = ScriptFixture::new();
    assert!(stale.stage(&[]).status.success());
    fs::write(stale.artifact_dir().join("manifest.txt"), "stale\n")
        .expect("stale package mutation");
    let repeated = stale.stage(&[]);
    assert!(
        !repeated.status.success(),
        "stale versioned package was accepted on repeat"
    );

    let writable = ScriptFixture::new();
    assert!(writable.stage(&[]).status.success());
    fs::set_permissions(
        writable.artifact_dir().join("manifest.txt"),
        fs::Permissions::from_mode(0o666),
    )
    .expect("writable package mode");
    fs::set_permissions(
        writable
            .root
            .join("usr/src/planeradar-hyperpixel2r-0.1.0/dkms.conf"),
        fs::Permissions::from_mode(0o666),
    )
    .expect("writable DKMS mode");
    let repeated = writable.stage(&[]);
    assert!(
        !repeated.status.success(),
        "writable reused package and DKMS source were accepted"
    );
}

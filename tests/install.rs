use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use clap::Parser;
use planeradar::cli::{Cli, Command as CliCommand};
use planeradar::install::{
    CommandRunner, InstallError, InstallOptions, InstallResult, Installer, PLANERADAR_SERVICE,
    read_installer_state_json, read_optional_installer_state_json, write_installer_state_json,
};
use sha2::{Digest, Sha256};

const ACCEPTED_CUSTOM_OVERLAY: &str = "planeradar-hyperpixel2r-eefaf3ae40fd";
const EXPECTED_PACKAGES: &[&str] = &[
    "libsdl2-2.0-0",
    "libegl1",
    "libgles2",
    "libgl1-mesa-dri",
    "ca-certificates",
    "avahi-daemon",
    "dkms",
    "kmod",
    "device-tree-compiler",
    "linux-headers-rpi-v8",
    "build-essential",
    "evtest",
    "pngcheck",
];

#[derive(Default)]
struct RecordingRunner {
    commands: Mutex<Vec<(String, Vec<String>)>>,
    passwd: Mutex<Option<PathBuf>>,
}

impl RecordingRunner {
    fn for_root(root: &Path) -> Self {
        Self {
            commands: Mutex::new(Vec::new()),
            passwd: Mutex::new(Some(root.join("etc/passwd"))),
        }
    }

    fn commands(&self) -> Vec<(String, Vec<String>)> {
        self.commands.lock().expect("commands lock").clone()
    }
}

impl CommandRunner for RecordingRunner {
    fn run(&self, program: &str, args: &[&str]) -> Result<(), InstallError> {
        self.commands.lock().expect("commands lock").push((
            program.to_owned(),
            args.iter().map(|arg| (*arg).to_owned()).collect(),
        ));
        if program == "useradd"
            && let Some(passwd) = self.passwd.lock().expect("passwd lock").as_ref()
        {
            let mut contents = fs::read_to_string(passwd).expect("read fake passwd");
            contents.push_str(
                "planeradar:x:991:991:Plane Radar:/var/lib/planeradar:/usr/sbin/nologin\n",
            );
            fs::write(passwd, contents).expect("record fake user creation");
        }
        Ok(())
    }
}

struct Fixture {
    _directory: tempfile::TempDir,
    root: PathBuf,
    artifact: PathBuf,
    checksum: PathBuf,
    revision: PathBuf,
    boot_config: PathBuf,
}

impl Fixture {
    fn new(boot_source: &str) -> Self {
        let directory = tempfile::tempdir().expect("temporary directory");
        let root = directory.path().join("root");
        for relative in [
            "etc",
            "proc/device-tree",
            "proc/sys/kernel",
            "boot/firmware",
            "lib/modules",
        ] {
            fs::create_dir_all(root.join(relative)).expect("fixture directory");
        }
        fs::write(
            root.join("etc/os-release"),
            "ID=debian\nVERSION_ID=\"13\"\nPRETTY_NAME=\"Raspberry Pi OS (Trixie)\"\n",
        )
        .expect("os-release fixture");
        fs::write(root.join("etc/passwd"), "root:x:0:0:root:/root:/bin/bash\n")
            .expect("passwd fixture");
        fs::write(
            root.join("proc/device-tree/model"),
            b"Raspberry Pi Zero 2 W Rev 1.0\0",
        )
        .expect("model fixture");
        fs::write(
            root.join("proc/sys/kernel/osrelease"),
            "6.18.34+rpt-rpi-v8\n",
        )
        .expect("running kernel fixture");
        let running_header =
            root.join("lib/modules/6.18.34+rpt-rpi-v8/build/include/config/kernel.release");
        fs::create_dir_all(running_header.parent().expect("header parent"))
            .expect("running header directory");
        fs::write(&running_header, "6.18.34+rpt-rpi-v8\n").expect("running header fixture");
        let boot_config = root.join("boot/firmware/config.txt");
        fs::write(&boot_config, boot_source).expect("boot fixture");

        let artifact = directory.path().join("planeradar");
        let mut elf = vec![0_u8; 64];
        elf[..4].copy_from_slice(b"\x7fELF");
        elf[4] = 2;
        elf[5] = 1;
        elf[18] = 0xb7;
        fs::write(&artifact, &elf).expect("artifact fixture");
        let artifact_sha256 = format!("{:x}", Sha256::digest(&elf));
        let checksum = directory.path().join("planeradar.sha256");
        fs::write(&checksum, format!("{artifact_sha256}  planeradar\n")).expect("checksum fixture");
        let revision = directory.path().join("planeradar.revision");
        fs::write(&revision, concat!(env!("PLANERADAR_REVISION"), "\n")).expect("revision fixture");

        Self {
            _directory: directory,
            root,
            artifact,
            checksum,
            revision,
            boot_config,
        }
    }

    fn options(&self, reboot: bool) -> InstallOptions {
        InstallOptions {
            root: self.root.clone(),
            boot_config: self.boot_config.clone(),
            artifact: self.artifact.clone(),
            checksum_file: self.checksum.clone(),
            revision_file: self.revision.clone(),
            reboot,
        }
    }

    fn set_kernel_state(&self, running: &str, installed_pairs: &[(&str, &str)]) {
        fs::write(
            self.root.join("proc/sys/kernel/osrelease"),
            format!("{running}\n"),
        )
        .expect("running kernel fixture");
        let modules = self.root.join("lib/modules");
        fs::remove_dir_all(&modules).expect("remove module fixtures");
        fs::create_dir(&modules).expect("module fixture root");
        for (kernel, headers) in installed_pairs {
            let release = modules
                .join(kernel)
                .join("build/include/config/kernel.release");
            fs::create_dir_all(release.parent().expect("header parent"))
                .expect("header fixture directory");
            fs::write(release, format!("{headers}\n")).expect("header release fixture");
        }
    }

    fn select_boot_kernel(&self, release: &str, bytes: &[u8]) {
        fs::write(
            self.root.join("boot").join(format!("vmlinuz-{release}")),
            bytes,
        )
        .expect("selected vmlinuz fixture");
        fs::write(self.root.join("boot/firmware/kernel8.img"), bytes).expect("kernel8 fixture");
    }

    fn add_boot_kernel(&self, release: &str, bytes: &[u8]) {
        fs::write(
            self.root.join("boot").join(format!("vmlinuz-{release}")),
            bytes,
        )
        .expect("additional vmlinuz fixture");
    }
}

fn mode(path: impl AsRef<Path>) -> u32 {
    fs::metadata(path).expect("metadata").permissions().mode() & 0o777
}

#[test]
fn installer_declares_every_graphics_runtime_package() {
    let fixture = Fixture::new("[all]\n");
    let runner = RecordingRunner::for_root(&fixture.root);
    Installer::new(&runner)
        .install(&fixture.options(false))
        .expect("install");
    let install = runner
        .commands()
        .into_iter()
        .find(|(program, args)| {
            program == "apt-get" && args.first().is_some_and(|value| value == "install")
        })
        .expect("apt install");
    for package in EXPECTED_PACKAGES {
        assert!(
            install.1.iter().any(|value| value == package),
            "missing {package}"
        );
    }
}

#[test]
fn installer_verifies_then_installs_once_and_is_idempotent() {
    let original_boot = "[all]\ndtparam=audio=on\n";
    let fixture = Fixture::new(original_boot);
    let runner = RecordingRunner::for_root(&fixture.root);
    let installer = Installer::new(&runner);

    let first = installer
        .install(&fixture.options(false))
        .expect("first install");
    assert!(first.files_changed);
    assert!(first.boot_config_changed);
    assert!(first.reboot_required);

    let installed = fixture.root.join("opt/planeradar");
    assert_eq!(
        fs::read(installed.join("bin/planeradar")).expect("installed binary"),
        fs::read(&fixture.artifact).expect("source binary")
    );
    assert_eq!(mode(installed.join("bin/planeradar")), 0o755);
    assert_eq!(
        fs::read_to_string(installed.join("REVISION")).expect("installed revision"),
        concat!(env!("PLANERADAR_REVISION"), "\n")
    );
    assert_eq!(mode(installed.join("REVISION")), 0o644);
    assert_eq!(
        fs::read_to_string(installed.join("SHA256")).expect("installed checksum"),
        fs::read_to_string(&fixture.checksum).expect("source checksum")
    );
    assert_eq!(mode(installed.join("SHA256")), 0o644);
    assert_eq!(mode(fixture.root.join("var/lib/planeradar")), 0o750);
    assert_eq!(
        fs::read_to_string(fixture.root.join("etc/systemd/system/planeradar.service"))
            .expect("installed service"),
        PLANERADAR_SERVICE
    );
    let boot = fs::read_to_string(&fixture.boot_config).expect("updated boot config");
    assert_eq!(
        boot.matches("dtoverlay=vc4-kms-dpi-hyperpixel2r").count(),
        1
    );
    assert_eq!(
        fs::read_to_string(
            fixture
                .root
                .join("boot/firmware/config.txt.planeradar-backup")
        )
        .expect("boot backup"),
        original_boot
    );

    let first_commands = runner.commands();
    assert!(first_commands.contains(&("apt-get".into(), vec!["update".into()])));
    assert!(first_commands.contains(&(
        "apt-get".into(),
        vec![
            "install".into(),
            "--yes".into(),
            "--no-install-recommends".into(),
            "libsdl2-2.0-0".into(),
            "libegl1".into(),
            "libgles2".into(),
            "libgl1-mesa-dri".into(),
            "ca-certificates".into(),
            "avahi-daemon".into(),
            "dkms".into(),
            "kmod".into(),
            "device-tree-compiler".into(),
            "linux-headers-rpi-v8".into(),
            "build-essential".into(),
            "evtest".into(),
            "pngcheck".into(),
        ],
    )));
    assert!(!first_commands.iter().any(|(program, args)| {
        program == "apt-get"
            && args
                .iter()
                .any(|argument| matches!(argument.as_str(), "full-upgrade" | "dist-upgrade"))
    }));
    assert!(first_commands.iter().any(|(program, args)| {
        program == "useradd" && args.last().map(String::as_str) == Some("planeradar")
    }));
    assert!(first_commands.contains(&(
        "usermod".into(),
        vec![
            "--append".into(),
            "--groups".into(),
            "video,render,input".into(),
            "planeradar".into(),
        ],
    )));
    assert!(first_commands.contains(&("systemctl".into(), vec!["daemon-reload".into()],)));
    assert!(first_commands.contains(&(
        "systemctl".into(),
        vec!["enable".into(), "planeradar.service".into()],
    )));
    assert!(first_commands.contains(&(
        "systemctl".into(),
        vec!["restart".into(), "planeradar.service".into()],
    )));
    assert!(
        !first_commands
            .iter()
            .any(|(program, args)| program == "systemctl" && args.as_slice() == ["reboot"])
    );

    let second = installer
        .install(&fixture.options(false))
        .expect("second install");
    assert!(!second.files_changed);
    assert!(!second.boot_config_changed);
    assert!(!second.reboot_required);
}

#[test]
fn installer_preserves_an_accepted_custom_overlay_and_requests_no_reboot() {
    let source = format!("[all]\ndtoverlay={ACCEPTED_CUSTOM_OVERLAY}\n");
    let fixture = Fixture::new(&source);
    let runner = RecordingRunner::for_root(&fixture.root);
    let result = Installer::new(&runner)
        .install(&fixture.options(true))
        .expect("install over accepted custom display");

    assert!(!result.boot_config_changed);
    assert!(!result.reboot_required);
    assert_eq!(
        fs::read_to_string(&fixture.boot_config).expect("preserved boot config"),
        source
    );
    assert!(
        !runner
            .commands()
            .iter()
            .any(|(program, args)| program == "systemctl" && args.as_slice() == ["reboot"])
    );
}

#[test]
fn installer_preserves_inline_calibration_and_existing_private_settings() {
    let source = "[all]\ndtoverlay=vc4-kms-dpi-hyperpixel2r,rotate=180\n";
    let fixture = Fixture::new(source);
    let state = fixture.root.join("var/lib/planeradar");
    fs::create_dir_all(&state).expect("existing state directory");
    fs::set_permissions(&state, fs::Permissions::from_mode(0o750)).expect("private state mode");
    fs::write(state.join("settings.json"), b"private settings\n").expect("existing settings");
    let runner = RecordingRunner::for_root(&fixture.root);

    let result = Installer::new(&runner)
        .install(&fixture.options(false))
        .expect("install over inline calibration");

    assert!(!result.boot_config_changed);
    assert!(!result.reboot_required);
    assert_eq!(
        fs::read_to_string(&fixture.boot_config).expect("preserved inline config"),
        source
    );
    assert_eq!(
        fs::read(state.join("settings.json")).expect("preserved settings"),
        b"private settings\n"
    );
}

#[test]
fn installer_accepts_debians_relative_os_release_symlink() {
    let fixture = Fixture::new("[all]\n");
    let canonical_os_release = fixture.root.join("usr/lib/os-release");
    fs::create_dir_all(canonical_os_release.parent().expect("os-release parent"))
        .expect("canonical os-release directory");
    fs::rename(fixture.root.join("etc/os-release"), &canonical_os_release)
        .expect("move canonical os-release");
    symlink("../usr/lib/os-release", fixture.root.join("etc/os-release"))
        .expect("Debian os-release symlink");
    let runner = RecordingRunner::for_root(&fixture.root);

    let result = Installer::new(&runner)
        .install(&fixture.options(false))
        .expect("install with Debian os-release symlink");

    assert!(result.files_changed);
}

#[test]
fn reboot_runs_only_when_requested_and_boot_configuration_changed() {
    let fixture = Fixture::new("[all]\n");
    let runner = RecordingRunner::for_root(&fixture.root);
    let result = Installer::new(&runner)
        .install(&fixture.options(true))
        .expect("install and reboot");

    assert!(result.boot_config_changed);
    assert!(result.reboot_required);
    assert!(
        runner
            .commands()
            .contains(&("systemctl".into(), vec!["reboot".into()],))
    );
}

#[test]
fn matching_running_kernel_headers_do_not_add_a_reboot() {
    let fixture = Fixture::new("[all]\ndtoverlay=planeradar-hyperpixel2r-eefaf3ae40fd\n");
    let runner = RecordingRunner::for_root(&fixture.root);
    let result = Installer::new(&runner)
        .install(&fixture.options(true))
        .expect("install with matching headers");

    assert!(!result.boot_config_changed);
    assert!(!result.reboot_required);
    assert!(
        !runner
            .commands()
            .iter()
            .any(|(program, args)| program == "systemctl" && args.as_slice() == ["reboot"])
    );
}

#[test]
fn alternate_installed_kernel_header_pair_requests_reboot_only_when_explicit() {
    for reboot in [false, true] {
        let fixture = Fixture::new("[all]\ndtoverlay=planeradar-hyperpixel2r-eefaf3ae40fd\n");
        fixture.set_kernel_state(
            "6.18.34+rpt-rpi-v8",
            &[("6.18.35+rpt-rpi-v8", "6.18.35+rpt-rpi-v8")],
        );
        fixture.select_boot_kernel("6.18.35+rpt-rpi-v8", b"selected kernel image");
        let runner = RecordingRunner::for_root(&fixture.root);
        let result = Installer::new(&runner)
            .install(&fixture.options(reboot))
            .expect("install with pending kernel");

        assert!(!result.boot_config_changed);
        assert!(result.reboot_required);
        assert_eq!(
            runner
                .commands()
                .iter()
                .filter(|(program, args)| {
                    program == "systemctl" && args.as_slice() == ["reboot"]
                })
                .count(),
            usize::from(reboot)
        );
    }
}

#[test]
fn alternate_headers_without_selected_boot_image_do_not_request_reboot() {
    let fixture = Fixture::new("[all]\ndtoverlay=planeradar-hyperpixel2r-eefaf3ae40fd\n");
    fixture.set_kernel_state(
        "6.18.34+rpt-rpi-v8",
        &[("6.18.35+rpt-rpi-v8", "6.18.35+rpt-rpi-v8")],
    );
    let runner = RecordingRunner::for_root(&fixture.root);

    let result = Installer::new(&runner)
        .install(&fixture.options(true))
        .expect("install with stale alternate headers");

    assert!(!result.reboot_required);
    assert!(
        !runner
            .commands()
            .iter()
            .any(|(program, args)| program == "systemctl" && args.as_slice() == ["reboot"])
    );
}

#[test]
fn mismatched_symlinked_ambiguous_or_overridden_boot_images_do_not_request_reboot() {
    for case in ["mismatch", "symlink", "ambiguous", "override"] {
        let boot = if case == "override" {
            "[all]\ndtoverlay=planeradar-hyperpixel2r-eefaf3ae40fd\nkernel=custom.img\n"
        } else {
            "[all]\ndtoverlay=planeradar-hyperpixel2r-eefaf3ae40fd\n"
        };
        let fixture = Fixture::new(boot);
        fixture.set_kernel_state(
            "6.18.34+rpt-rpi-v8",
            &[("6.18.35+rpt-rpi-v8", "6.18.35+rpt-rpi-v8")],
        );
        fixture.add_boot_kernel("6.18.35+rpt-rpi-v8", b"selected kernel image");
        match case {
            "mismatch" => {
                fs::write(
                    fixture.root.join("boot/firmware/kernel8.img"),
                    b"different kernel image",
                )
                .expect("mismatched kernel8 fixture");
            }
            "symlink" => {
                symlink(
                    "../vmlinuz-6.18.35+rpt-rpi-v8",
                    fixture.root.join("boot/firmware/kernel8.img"),
                )
                .expect("symlinked kernel8 fixture");
            }
            "ambiguous" => {
                fs::write(
                    fixture.root.join("boot/firmware/kernel8.img"),
                    b"selected kernel image",
                )
                .expect("kernel8 fixture");
                fixture.add_boot_kernel("6.18.36+rpt-rpi-v8", b"selected kernel image");
            }
            "override" => {
                fs::write(
                    fixture.root.join("boot/firmware/kernel8.img"),
                    b"selected kernel image",
                )
                .expect("kernel8 fixture");
            }
            _ => unreachable!(),
        }
        let runner = RecordingRunner::for_root(&fixture.root);

        let result = Installer::new(&runner)
            .install(&fixture.options(true))
            .unwrap_or_else(|error| panic!("{case} fixture install: {error}"));

        assert!(!result.reboot_required, "{case}");
        assert!(
            !runner
                .commands()
                .iter()
                .any(|(program, args)| program == "systemctl" && args.as_slice() == ["reboot"])
        );
    }
}

#[test]
fn direct_installer_rejects_debian_12_before_commands() {
    let fixture = Fixture::new("[all]\n");
    fs::write(
        fixture.root.join("etc/os-release"),
        "ID=debian\nVERSION_ID=\"12\"\n",
    )
    .expect("bookworm fixture");
    let runner = RecordingRunner::for_root(&fixture.root);
    let error = Installer::new(&runner)
        .install(&fixture.options(false))
        .expect_err("Bookworm is unsupported");

    assert!(matches!(error, InstallError::UnsupportedOperatingSystem(_)));
    assert!(runner.commands().is_empty());
}

#[test]
fn invalid_artifact_revision_or_platform_fails_before_writes_or_commands() {
    let fixture = Fixture::new("[all]\n");
    fs::write(&fixture.revision, "wrong-revision\n").expect("wrong revision");
    let runner = RecordingRunner::for_root(&fixture.root);

    Installer::new(&runner)
        .install(&fixture.options(false))
        .expect_err("revision mismatch must fail");

    assert!(runner.commands().is_empty());
    assert!(!fixture.root.join("opt/planeradar").exists());
    assert_eq!(
        fs::read_to_string(&fixture.boot_config).expect("unchanged boot"),
        "[all]\n"
    );
}

#[test]
fn checksum_architecture_platform_and_ambiguous_display_fail_before_mutation() {
    let checksum_fixture = Fixture::new("[all]\n");
    fs::write(
        &checksum_fixture.checksum,
        format!("{}  planeradar\n", "0".repeat(64)),
    )
    .expect("wrong checksum");
    assert_preflight_rejected(&checksum_fixture);

    let architecture_fixture = Fixture::new("[all]\n");
    let mut wrong_architecture =
        fs::read(&architecture_fixture.artifact).expect("artifact fixture");
    wrong_architecture[18] = 0x3e;
    fs::write(&architecture_fixture.artifact, wrong_architecture).expect("wrong architecture");
    assert_preflight_rejected(&architecture_fixture);

    let platform_fixture = Fixture::new("[all]\n");
    fs::write(
        platform_fixture.root.join("proc/device-tree/model"),
        b"Raspberry Pi 4 Model B\0",
    )
    .expect("wrong model");
    assert_preflight_rejected(&platform_fixture);

    let display_fixture = Fixture::new(concat!(
        "[all]\n",
        "dtoverlay=vc4-kms-dpi-hyperpixel2r\n",
        "dtoverlay=planeradar-hyperpixel2r-eefaf3ae40fd\n",
    ));
    assert_preflight_rejected(&display_fixture);
}

fn assert_preflight_rejected(fixture: &Fixture) {
    let boot_before = fs::read(&fixture.boot_config).expect("boot before rejection");
    let runner = RecordingRunner::for_root(&fixture.root);
    Installer::new(&runner)
        .install(&fixture.options(false))
        .expect_err("preflight must reject fixture");
    assert!(runner.commands().is_empty());
    assert!(!fixture.root.join("opt/planeradar").exists());
    assert_eq!(
        fs::read(&fixture.boot_config).expect("boot after rejection"),
        boot_before
    );
}

#[test]
fn service_unit_has_the_exact_hardening_and_device_contract() {
    for directive in [
        "[Unit]",
        "After=network-online.target",
        "Wants=network-online.target",
        "[Service]",
        "User=planeradar",
        "SupplementaryGroups=video render input",
        "WorkingDirectory=/opt/planeradar",
        "ExecStart=/opt/planeradar/bin/planeradar run",
        "Restart=on-failure",
        "RestartSec=3",
        "Environment=SDL_VIDEODRIVER=kmsdrm",
        "AmbientCapabilities=CAP_NET_BIND_SERVICE",
        "CapabilityBoundingSet=CAP_NET_BIND_SERVICE",
        "NoNewPrivileges=true",
        "ProtectSystem=strict",
        "ProtectHome=true",
        "PrivateTmp=true",
        "PrivateDevices=false",
        "DevicePolicy=closed",
        "DeviceAllow=char-drm rw",
        "DeviceAllow=char-input r",
        "RestrictAddressFamilies=AF_UNIX AF_INET AF_INET6 AF_NETLINK",
        "ReadWritePaths=/var/lib/planeradar",
        "StateDirectory=planeradar",
        "StateDirectoryMode=0750",
        "UMask=0027",
        "[Install]",
        "WantedBy=multi-user.target",
    ] {
        assert!(
            PLANERADAR_SERVICE.lines().any(|line| line == directive),
            "service is missing exact directive: {directive}"
        );
    }
    for invalid_directive in [
        "DeviceAllow=/dev/dri/card* rw",
        "DeviceAllow=/dev/dri/renderD* rw",
        "DeviceAllow=/dev/input/event* r",
    ] {
        assert!(
            !PLANERADAR_SERVICE
                .lines()
                .any(|line| line == invalid_directive),
            "service must not use unsupported device-path glob: {invalid_directive}"
        );
    }
}

#[test]
fn install_cli_requires_the_three_verified_artifact_sidecars() {
    let cli = Cli::try_parse_from([
        "planeradar",
        "install",
        "--artifact",
        "/tmp/planeradar",
        "--checksum-file",
        "/tmp/planeradar.sha256",
        "--revision-file",
        "/tmp/planeradar.revision",
    ])
    .expect("parse install command");

    assert!(matches!(
        cli.command,
        CliCommand::Install {
            artifact,
            checksum_file,
            revision_file,
            reboot: false,
            ..
        } if artifact == Path::new("/tmp/planeradar")
            && checksum_file == Path::new("/tmp/planeradar.sha256")
            && revision_file == Path::new("/tmp/planeradar.revision")
    ));
}

#[test]
fn install_cli_accepts_an_explicit_machine_output_flag() {
    let cli = Cli::try_parse_from([
        "planeradar",
        "install",
        "--artifact",
        "/tmp/planeradar",
        "--checksum-file",
        "/tmp/planeradar.sha256",
        "--revision-file",
        "/tmp/planeradar.revision",
        "--json",
    ])
    .expect("parse install command");

    assert!(matches!(
        cli.command,
        CliCommand::Install { json: true, .. }
    ));
}

#[test]
fn target_install_result_machine_json_has_the_exact_public_schema() {
    let result = InstallResult {
        files_changed: true,
        boot_config_changed: false,
        reboot_required: false,
        revision: "0123456789abcdef0123456789abcdef01234567".into(),
        sha256: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".into(),
    };

    assert_eq!(
        result.to_json().expect("machine JSON"),
        r#"{"schema_version":1,"files_changed":true,"boot_config_changed":false,"reboot_required":false,"revision":"0123456789abcdef0123456789abcdef01234567","sha256":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"}"#
    );
}

#[test]
fn target_install_result_refuses_invalid_machine_identity_fields() {
    for result in [
        InstallResult {
            revision: "short".into(),
            sha256: "2".repeat(64),
            files_changed: false,
            boot_config_changed: false,
            reboot_required: false,
        },
        InstallResult {
            revision: "1".repeat(40),
            sha256: "UPPER".repeat(13),
            files_changed: false,
            boot_config_changed: false,
            reboot_required: false,
        },
    ] {
        assert!(result.to_json().is_err());
    }
}

#[test]
fn target_installer_state_helper_round_trips_strict_json_atomically() {
    let directory = tempfile::tempdir().expect("temporary state directory");
    let path = directory
        .path()
        .join("planeradar")
        .join("installer")
        .join("state.json");
    let json = r#"{"schema_version":1,"hardware":{"model":"Raspberry Pi Zero 2 W Rev 1.0","serial":"0123456789abcdef"},"application":{"version":"1.2.3","source_commit":"1111111111111111111111111111111111111111","sha256":"2222222222222222222222222222222222222222222222222222222222222222"},"driver":null,"owned_files":[],"last_verified_phase":"application_acquired"}"#;

    write_installer_state_json(&path, json.as_bytes()).expect("write state");

    assert_eq!(read_installer_state_json(&path).expect("read state"), json);
    assert_eq!(mode(&path), 0o600);
    assert_eq!(mode(path.parent().expect("installer parent")), 0o700);
}

#[test]
fn target_installer_state_helper_reports_absent_state_as_exact_null() {
    let directory = tempfile::tempdir().expect("temporary state directory");
    let path = directory.path().join("missing").join("state.json");

    assert_eq!(
        read_optional_installer_state_json(&path).expect("optional state"),
        "null"
    );
}

#[test]
fn target_installer_state_helper_rejects_hostile_json_and_unsafe_final_paths() {
    let directory = tempfile::tempdir().expect("temporary state directory");
    let path = directory.path().join("state.json");
    let valid = r#"{"schema_version":1,"hardware":{"model":"Raspberry Pi Zero 2 W","serial":"0123456789abcdef"},"application":null,"driver":null,"owned_files":[],"last_verified_phase":"discovered"}"#;
    let hostile = [
        "{}",
        r#"{"schema_version":2,"hardware":{"model":"Raspberry Pi Zero 2 W","serial":"0123456789abcdef"},"application":null,"driver":null,"owned_files":[],"last_verified_phase":"discovered"}"#,
        r#"{"schema_version":1,"hardware":{"model":"Raspberry Pi Zero 2 W","serial":"0123456789abcdef"},"application":null,"driver":null,"owned_files":[],"last_verified_phase":"unknown"}"#,
        &format!("{valid}\n{{}}"),
        &format!("{valid}\npartial"),
    ];
    for input in hostile {
        assert!(write_installer_state_json(&path, input.as_bytes()).is_err());
        assert!(!path.exists());
    }

    let outside = directory.path().join("outside");
    fs::write(&outside, b"sentinel").expect("outside sentinel");
    symlink(&outside, &path).expect("state symlink");
    assert!(write_installer_state_json(&path, valid.as_bytes()).is_err());
    assert_eq!(fs::read(&outside).expect("outside unchanged"), b"sentinel");
}

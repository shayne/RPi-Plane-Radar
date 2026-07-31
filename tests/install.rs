use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

use clap::Parser;
use planeradar::cli::{Cli, Command as CliCommand};
use planeradar::install::{
    ApplicationReleaseIdentity, CommandRunner, InstallError, InstallOptions, InstallResult,
    InstalledFile, Installer, MachineOutputCommandRunner, PLANERADAR_SERVICE,
    activate_application_release, application_release_ownership_json,
    parse_application_ownership_json, read_installer_state_json, read_lifecycle_state_json,
    read_optional_installer_state_json, retire_application_artifacts, uninstall_owned_installation,
    write_installer_state_json, write_lifecycle_state_json,
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
fn installer_never_selects_a_kernel_or_header_meta_package() {
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

    assert!(
        install.1.iter().all(|package| {
            !package.starts_with("linux-image") && !package.starts_with("linux-headers")
        }),
        "application installation may not select a new kernel or kernel-header meta package: {:?}",
        install.1
    );
}

#[test]
fn machine_output_runner_reserves_stdout_for_the_result_document() {
    MachineOutputCommandRunner
        .run("sh", &["-c", "test -c /dev/fd/1 && printf package-noise"])
        .expect("machine-output child stdout is /dev/null");
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
    assert_eq!(
        first
            .owned_files
            .iter()
            .map(|file| file.target_path.as_str())
            .collect::<Vec<_>>(),
        [
            "/opt/planeradar/bin/planeradar",
            "/opt/planeradar/REVISION",
            "/opt/planeradar/SHA256",
            "/etc/systemd/system/planeradar.service",
            "/var/lib/planeradar/settings.json",
            "/var/lib/planeradar-installer/settings-owned-v1",
        ]
    );
    for owned in &first.owned_files {
        let installed_path = fixture.root.join(owned.target_path.trim_start_matches('/'));
        assert_eq!(
            owned.sha256,
            format!(
                "{:x}",
                Sha256::digest(fs::read(installed_path).expect("read owned file"))
            )
        );
    }
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
    assert!(!first_commands.iter().any(|(_, args)| {
        args.iter()
            .any(|argument| argument.contains("planeradar-installer"))
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
fn installer_preserves_the_versioned_external_driver_overlay_and_requests_no_reboot() {
    let source = "[all]\ndtoverlay=hyperpixel2r-kms-224cc7ab7817.dtbo\n";
    let fixture = Fixture::new(source);
    let runner = RecordingRunner::for_root(&fixture.root);
    let result = Installer::new(&runner)
        .install(&fixture.options(true))
        .expect("install over the accepted external driver display");

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
        owned_files: vec![planeradar::install::InstalledFile {
            target_path: "/opt/planeradar/bin/planeradar".into(),
            sha256: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".into(),
        }],
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
            owned_files: vec![],
        },
        InstallResult {
            revision: "1".repeat(40),
            sha256: "UPPER".repeat(13),
            files_changed: false,
            boot_config_changed: false,
            reboot_required: false,
            owned_files: vec![],
        },
    ] {
        assert!(result.to_json().is_err());
    }
}

#[test]
fn installer_ownership_is_a_separate_exact_internal_contract() {
    let directory = tempfile::tempdir().expect("temporary root");
    for (relative, contents) in [
        ("opt/planeradar/bin/planeradar", b"binary".as_slice()),
        ("opt/planeradar/REVISION", b"revision".as_slice()),
        ("opt/planeradar/SHA256", b"checksum".as_slice()),
        (
            "etc/systemd/system/planeradar.service",
            b"service".as_slice(),
        ),
    ] {
        let path = directory.path().join(relative);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("create parent");
        std::fs::write(path, contents).expect("write owned file");
    }

    let encoded =
        planeradar::install::installer_ownership_json(directory.path()).expect("ownership JSON");
    let value: serde_json::Value = serde_json::from_str(&encoded).expect("parse ownership JSON");
    assert_eq!(value["schema_version"], 1);
    let files = value["owned_files"].as_array().expect("owned files");
    assert_eq!(files.len(), 4);
    assert_eq!(files[0]["target_path"], "/opt/planeradar/bin/planeradar");
    assert_eq!(
        value
            .as_object()
            .expect("object")
            .keys()
            .collect::<Vec<_>>(),
        ["owned_files", "schema_version"]
    );
}

#[test]
fn target_installer_state_helper_round_trips_strict_json_atomically() {
    let directory = tempfile::tempdir().expect("temporary state directory");
    let path = directory
        .path()
        .canonicalize()
        .expect("canonical temporary directory")
        .join("planeradar-installer")
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
    let path = directory
        .path()
        .canonicalize()
        .expect("canonical temporary directory")
        .join("missing")
        .join("state.json");

    assert_eq!(
        read_optional_installer_state_json(&path).expect("optional state"),
        "null"
    );
}

#[test]
fn target_installer_state_helper_rejects_hostile_json_and_unsafe_final_paths() {
    let directory = tempfile::tempdir().expect("temporary state directory");
    let root = directory
        .path()
        .canonicalize()
        .expect("canonical temporary directory");
    let private = root.join("planeradar-installer");
    fs::create_dir(&private).expect("private state directory");
    fs::set_permissions(&private, fs::Permissions::from_mode(0o700)).expect("private permissions");
    let path = private.join("state.json");
    let valid = r#"{"schema_version":1,"hardware":{"model":"Raspberry Pi Zero 2 W","serial":"0123456789abcdef"},"application":null,"driver":null,"owned_files":[],"last_verified_phase":"discovered"}"#;
    let hostile = [
        "{}",
        r#"{"schema_version":2,"hardware":{"model":"Raspberry Pi Zero 2 W","serial":"0123456789abcdef"},"application":null,"driver":null,"owned_files":[],"last_verified_phase":"discovered"}"#,
        r#"{"schema_version":1,"hardware":{"model":"Raspberry Pi Zero 2 W","serial":"0123456789abcdef"},"application":null,"driver":null,"owned_files":[],"last_verified_phase":"unknown"}"#,
        r#"{"schema_version":1,"hardware":{"model":"Raspberry Pi Zero 2 W","serial":"0123456789abcdef"},"application":null,"driver":null,"owned_files":[],"last_verified_phase":"complete"}"#,
        r#"{"schema_version":1,"hardware":{"model":"Raspberry Pi Zero 2 W","serial":"0123456789abcdef"},"application":{"version":"1.2.3","source_commit":"1111111111111111111111111111111111111111","sha256":"2222222222222222222222222222222222222222222222222222222222222222"},"driver":null,"owned_files":[],"last_verified_phase":"discovered"}"#,
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

#[test]
fn target_installer_state_rejects_non_private_or_symlinked_state_directories() {
    let directory = tempfile::tempdir().expect("temporary state directory");
    let root = directory
        .path()
        .canonicalize()
        .expect("canonical temporary directory");
    let valid = r#"{"schema_version":1,"hardware":{"model":"Raspberry Pi Zero 2 W","serial":"0123456789abcdef"},"application":null,"driver":null,"owned_files":[],"last_verified_phase":"discovered"}"#;

    let permissive = root.join("permissive");
    fs::create_dir(&permissive).expect("permissive directory");
    fs::set_permissions(&permissive, fs::Permissions::from_mode(0o755))
        .expect("permissive permissions");
    assert!(write_installer_state_json(&permissive.join("state.json"), valid.as_bytes()).is_err());

    let outside = root.join("outside-state");
    fs::create_dir(&outside).expect("outside directory");
    fs::set_permissions(&outside, fs::Permissions::from_mode(0o700)).expect("outside permissions");
    let linked = root.join("linked-state");
    symlink(&outside, &linked).expect("linked state directory");
    assert!(write_installer_state_json(&linked.join("state.json"), valid.as_bytes()).is_err());
    assert!(!outside.join("state.json").exists());
}

#[test]
fn application_upgrade_stages_content_addressed_bytes_and_atomically_switches_the_live_binary() {
    let fixture = Fixture::new("[all]\n");
    let runner = RecordingRunner::for_root(&fixture.root);
    let installed = Installer::new(&runner)
        .install(&fixture.options(false))
        .expect("initial install");
    let settings = fixture.root.join("var/lib/planeradar/settings.json");
    fs::write(&settings, b"{\"range_km\":10}").expect("settings");
    let old_settings = fs::read(&settings).expect("old settings");
    let mut next_bytes = fs::read(&fixture.artifact).expect("artifact");
    next_bytes.push(42);
    let next = fixture._directory.path().join("planeradar-next");
    fs::write(&next, &next_bytes).expect("next artifact");
    let identity = ApplicationReleaseIdentity {
        version: "1.1.0".into(),
        revision: "b".repeat(40),
        sha256: format!("{:x}", Sha256::digest(&next_bytes)),
    };

    let owned =
        activate_application_release(&fixture.root, &next, &identity, &installed.owned_files)
            .expect("activate release");

    assert_eq!(
        fs::read(fixture.root.join("opt/planeradar/bin/planeradar")).expect("live binary"),
        next_bytes
    );
    assert_eq!(
        fs::read(
            fixture
                .root
                .join("opt/planeradar/releases/1.1.0")
                .join(&identity.sha256)
                .join("planeradar")
        )
        .expect("versioned binary"),
        fs::read(&next).unwrap()
    );
    assert_eq!(fs::read(&settings).unwrap(), old_settings);
    assert_eq!(owned.len(), 7);
    assert_eq!(
        owned.last().unwrap().target_path,
        format!(
            "/opt/planeradar/releases/1.1.0/{}/planeradar",
            identity.sha256
        )
    );
}

#[test]
fn application_rollback_reuses_an_existing_release_without_duplicate_ownership() {
    let fixture = Fixture::new("[all]\n");
    let runner = RecordingRunner::for_root(&fixture.root);
    let installed = Installer::new(&runner)
        .install(&fixture.options(false))
        .expect("initial install");
    let original_bytes = fs::read(&fixture.artifact).expect("original artifact");
    let original = ApplicationReleaseIdentity {
        version: "1.0.0".into(),
        revision: "a".repeat(40),
        sha256: format!("{:x}", Sha256::digest(&original_bytes)),
    };
    let first_owned = activate_application_release(
        &fixture.root,
        &fixture.artifact,
        &original,
        &installed.owned_files,
    )
    .expect("stage original release");

    let mut next_bytes = original_bytes.clone();
    next_bytes.push(42);
    let next_path = fixture._directory.path().join("planeradar-next");
    fs::write(&next_path, &next_bytes).expect("next artifact");
    let next = ApplicationReleaseIdentity {
        version: "1.1.0".into(),
        revision: "b".repeat(40),
        sha256: format!("{:x}", Sha256::digest(&next_bytes)),
    };
    let next_owned = activate_application_release(&fixture.root, &next_path, &next, &first_owned)
        .expect("activate next release");

    let rolled_back =
        activate_application_release(&fixture.root, &fixture.artifact, &original, &next_owned)
            .expect("rollback to retained release");

    application_release_ownership_json(&rolled_back).expect("prospective ownership remains valid");
    let selected_path = format!(
        "/opt/planeradar/releases/{}/{}/planeradar",
        original.version, original.sha256
    );
    assert_eq!(
        rolled_back
            .iter()
            .filter(|file| file.target_path == selected_path)
            .count(),
        1
    );
    assert_eq!(
        fs::read(fixture.root.join("opt/planeradar/bin/planeradar")).unwrap(),
        original_bytes
    );
}

#[test]
fn application_activation_retires_only_release_artifacts_outside_bounded_history() {
    let fixture = Fixture::new("[all]\n");
    let runner = RecordingRunner::for_root(&fixture.root);
    let installed = Installer::new(&runner)
        .install(&fixture.options(false))
        .expect("initial install");
    let base = fs::read(&fixture.artifact).expect("base artifact");
    let mut owned = installed.owned_files;
    let mut releases = Vec::new();
    for index in 0..4u8 {
        let mut bytes = base.clone();
        bytes.push(index);
        let artifact = fixture
            ._directory
            .path()
            .join(format!("planeradar-{index}"));
        fs::write(&artifact, &bytes).expect("release artifact");
        let identity = ApplicationReleaseIdentity {
            version: format!("1.{index}.0"),
            revision: char::from(b'a' + index).to_string().repeat(40),
            sha256: format!("{:x}", Sha256::digest(&bytes)),
        };
        let path = fixture
            .root
            .join("opt/planeradar/releases")
            .join(&identity.version)
            .join(&identity.sha256)
            .join("planeradar");
        owned = activate_application_release(&fixture.root, &artifact, &identity, &owned)
            .expect("activate bounded release");
        releases.push(path);
    }

    assert_eq!(
        owned
            .iter()
            .filter(|file| file.target_path.starts_with("/opt/planeradar/releases/"))
            .count(),
        3
    );
    assert!(!releases[0].exists());
    assert!(releases[1..].iter().all(|path| path.exists()));
}

#[test]
fn exact_manifest_uninstall_preserves_settings_unrelated_files_and_boot_lines_and_is_idempotent() {
    let fixture = Fixture::new("[all]\ndtparam=audio=on\n");
    let runner = RecordingRunner::for_root(&fixture.root);
    let installed = Installer::new(&runner)
        .install(&fixture.options(false))
        .expect("initial install");
    let settings = fixture.root.join("var/lib/planeradar/settings.json");
    fs::write(&settings, b"{\"latitude\":40}").expect("settings");
    let unrelated = fixture.root.join("opt/planeradar/user-note");
    fs::write(&unrelated, b"mine").expect("unrelated file");
    let boot_before = fs::read(&fixture.boot_config).expect("boot before");

    uninstall_owned_installation(&fixture.root, &installed.owned_files, false, &runner)
        .expect("uninstall");
    uninstall_owned_installation(&fixture.root, &installed.owned_files, false, &runner)
        .expect("idempotent repeat");

    for owned in &installed.owned_files {
        if matches!(
            owned.target_path.as_str(),
            "/var/lib/planeradar/settings.json" | "/var/lib/planeradar-installer/settings-owned-v1"
        ) {
            continue;
        }
        assert!(
            !fixture
                .root
                .join(owned.target_path.trim_start_matches('/'))
                .exists()
        );
    }
    assert_eq!(fs::read(&settings).unwrap(), b"{\"latitude\":40}");
    assert_eq!(fs::read(&unrelated).unwrap(), b"mine");
    assert_eq!(fs::read(&fixture.boot_config).unwrap(), boot_before);
    assert!(runner.commands().iter().any(|(program, arguments)| {
        program == "systemctl" && arguments == &["disable", "--now", "planeradar.service"]
    }));
}

#[test]
fn purge_settings_requires_and_removes_only_an_exact_owned_settings_record() {
    let fixture = Fixture::new("[all]\n");
    let runner = RecordingRunner::for_root(&fixture.root);
    let installed = Installer::new(&runner)
        .install(&fixture.options(false))
        .expect("initial install");
    let settings = fixture.root.join("var/lib/planeradar/settings.json");
    assert!(
        installed
            .owned_files
            .iter()
            .any(|file| file.target_path == "/var/lib/planeradar/settings.json"),
        "a settings file created by the installer must be recorded"
    );
    uninstall_owned_installation(&fixture.root, &installed.owned_files, true, &runner)
        .expect("purge installer-created settings");
    assert!(!settings.exists());
}

#[test]
fn production_ownership_rediscovers_mutable_installer_settings_through_immutable_marker() {
    let fixture = Fixture::new("[all]\n");
    let runner = RecordingRunner::for_root(&fixture.root);
    Installer::new(&runner)
        .install(&fixture.options(false))
        .expect("initial install");
    let settings = fixture.root.join("var/lib/planeradar/settings.json");
    fs::write(&settings, b"{\"user\":\"changed after install\"}\n").expect("mutable settings");

    let encoded =
        planeradar::install::installer_ownership_json(&fixture.root).expect("production ownership");
    let owned = parse_application_ownership_json(encoded.as_bytes()).expect("typed ownership");
    assert!(
        owned
            .iter()
            .any(|file| { file.target_path == "/var/lib/planeradar/settings.json" })
    );
    assert!(
        owned
            .iter()
            .any(|file| { file.target_path == "/var/lib/planeradar-installer/settings-owned-v1" })
    );

    uninstall_owned_installation(&fixture.root, &owned, true, &runner)
        .expect("purge mutable installer settings");
    assert!(!settings.exists());
    assert!(
        !fixture
            .root
            .join("var/lib/planeradar-installer/settings-owned-v1")
            .exists()
    );
}

#[test]
fn settings_purge_rejects_missing_or_mutated_immutable_ownership_marker() {
    for hostile in ["missing", "mutated", "hardlink"] {
        let fixture = Fixture::new("[all]\n");
        let runner = RecordingRunner::for_root(&fixture.root);
        let installed = Installer::new(&runner)
            .install(&fixture.options(false))
            .expect("initial install");
        let marker = fixture
            .root
            .join("var/lib/planeradar-installer/settings-owned-v1");
        match hostile {
            "missing" => fs::remove_file(&marker).expect("remove marker"),
            "mutated" => fs::write(&marker, b"forged\n").expect("mutate marker"),
            "hardlink" => {
                fs::hard_link(&marker, marker.with_extension("link")).expect("hardlink marker");
            }
            _ => unreachable!(),
        }
        assert!(
            uninstall_owned_installation(&fixture.root, &installed.owned_files, true, &runner)
                .is_err()
        );
        assert!(
            fixture
                .root
                .join("var/lib/planeradar/settings.json")
                .exists()
        );
    }
}

#[test]
fn default_uninstall_preserves_modified_installer_created_settings() {
    let fixture = Fixture::new("[all]\n");
    let runner = RecordingRunner::for_root(&fixture.root);
    let installed = Installer::new(&runner)
        .install(&fixture.options(false))
        .expect("initial install");
    let settings = fixture.root.join("var/lib/planeradar/settings.json");
    fs::write(&settings, b"{\"user\":\"changed\"}\n").expect("modified settings");

    uninstall_owned_installation(&fixture.root, &installed.owned_files, false, &runner)
        .expect("default uninstall");

    assert_eq!(fs::read(&settings).unwrap(), b"{\"user\":\"changed\"}\n");
}

#[test]
fn preexisting_settings_are_preserved_and_cannot_be_purged_as_installer_owned() {
    let fixture = Fixture::new("[all]\n");
    fs::create_dir_all(fixture.root.join("var/lib/planeradar")).expect("settings directory");
    let settings = fixture.root.join("var/lib/planeradar/settings.json");
    fs::write(&settings, b"{\"user\":\"mine\"}\n").expect("preexisting settings");
    let runner = RecordingRunner::for_root(&fixture.root);
    let installed = Installer::new(&runner)
        .install(&fixture.options(false))
        .expect("install preserving settings");

    assert!(
        !installed
            .owned_files
            .iter()
            .any(|file| file.target_path == "/var/lib/planeradar/settings.json")
    );
    assert!(matches!(
        uninstall_owned_installation(&fixture.root, &installed.owned_files, true, &runner),
        Err(InstallError::SettingsNotOwned)
    ));
    assert_eq!(fs::read(&settings).unwrap(), b"{\"user\":\"mine\"}\n");
}

#[test]
fn uninstall_fails_closed_before_deletion_on_drift_symlinks_or_hardlinks() {
    for hostile in ["drift", "symlink", "hardlink"] {
        let fixture = Fixture::new("[all]\n");
        let runner = RecordingRunner::for_root(&fixture.root);
        let installed = Installer::new(&runner)
            .install(&fixture.options(false))
            .expect("initial install");
        let binary = fixture.root.join("opt/planeradar/bin/planeradar");
        match hostile {
            "drift" => fs::write(&binary, b"changed").expect("drift"),
            "symlink" => {
                let outside = fixture._directory.path().join("outside");
                fs::write(&outside, b"outside").expect("outside");
                fs::remove_file(&binary).expect("remove binary");
                symlink(&outside, &binary).expect("binary symlink");
            }
            "hardlink" => {
                fs::hard_link(
                    &binary,
                    fixture._directory.path().join("second-binary-link"),
                )
                .expect("hard link");
            }
            _ => unreachable!(),
        }

        assert!(
            uninstall_owned_installation(&fixture.root, &installed.owned_files, false, &runner)
                .is_err(),
            "{hostile}"
        );
        assert!(
            fixture.root.join("opt/planeradar/REVISION").exists(),
            "{hostile}: no earlier verified file may be deleted"
        );
        assert!(
            !runner.commands().iter().any(|(program, arguments)| {
                program == "systemctl" && arguments == &["disable", "--now", "planeradar.service"]
            }),
            "{hostile}: service mutation must follow complete preflight"
        );
    }
}

#[test]
fn target_lifecycle_state_round_trips_strict_bounded_json_atomically() {
    let directory = tempfile::tempdir().expect("temporary state directory");
    let state_directory = directory
        .path()
        .canonicalize()
        .expect("canonical temporary directory")
        .join("lifecycle");
    fs::create_dir(&state_directory).expect("state directory");
    fs::set_permissions(&state_directory, fs::Permissions::from_mode(0o700))
        .expect("private state directory");
    let path = state_directory.join("state.json");
    let json = r#"{"schema_version":1,"hardware":{"model":"Raspberry Pi Zero 2 W","serial":"0123456789abcdef"},"accepted":[{"pair":{"application":{"version":"1.0.0","source_commit":"1111111111111111111111111111111111111111","sha256":"2222222222222222222222222222222222222222222222222222222222222222"},"driver":{"version":"0.1.0","source_commit":"3333333333333333333333333333333333333333","sha256":"4444444444444444444444444444444444444444444444444444444444444444"}},"sequence":1,"owned_files":[{"target_path":"/opt/planeradar/bin/planeradar","sha256":"2222222222222222222222222222222222222222222222222222222222222222"}]}],"transaction":null}"#;

    write_lifecycle_state_json(&path, json.as_bytes()).expect("write lifecycle");
    let migrated = json
        .replace("\"schema_version\":1", "\"schema_version\":3")
        .replace(
            "\"transaction\":null",
            "\"transaction\":null,\"uninstall\":null",
        );
    assert_eq!(read_lifecycle_state_json(&path).unwrap(), migrated);
    assert_eq!(mode(&path), 0o600);

    for hostile in [
        json.replace("\"schema_version\":1", "\"schema_version\":4"),
        json.replace("\"sequence\":1", "\"sequence\":0"),
        format!("{json}\n{{}}"),
    ] {
        assert!(write_lifecycle_state_json(&path, hostile.as_bytes()).is_err());
        assert_eq!(read_lifecycle_state_json(&path).unwrap(), migrated);
    }
}

#[test]
fn target_lifecycle_state_requires_the_exact_current_protocol_management_helper() {
    let directory = tempfile::tempdir().expect("temporary state directory");
    let state_directory = directory
        .path()
        .canonicalize()
        .expect("canonical temporary directory")
        .join("lifecycle-helper");
    fs::create_dir(&state_directory).expect("state directory");
    fs::set_permissions(&state_directory, fs::Permissions::from_mode(0o700))
        .expect("private state directory");
    let path = state_directory.join("state.json");
    let prior_application = serde_json::json!({
        "version": "1.0.0",
        "source_commit": "1".repeat(40),
        "sha256": "1".repeat(64)
    });
    let candidate_application = serde_json::json!({
        "version": "2.0.0",
        "source_commit": "2".repeat(40),
        "sha256": "2".repeat(64)
    });
    let prior_driver = serde_json::json!({
        "version": "0.1.0",
        "source_commit": "a".repeat(40),
        "sha256": "a".repeat(64)
    });
    let candidate_driver = serde_json::json!({
        "version": "0.1.0",
        "source_commit": "b".repeat(40),
        "sha256": "b".repeat(64)
    });
    let prior = serde_json::json!({
        "pair": {
            "application": prior_application,
            "driver": prior_driver
        },
        "sequence": 1,
        "owned_files": [{
            "target_path": "/opt/planeradar/bin/planeradar",
            "sha256": "1".repeat(64)
        }]
    });
    let helper_path = format!(
        "/var/lib/planeradar-installer/helpers/{}/planeradar",
        "2".repeat(64)
    );
    let state = serde_json::json!({
        "schema_version": 3,
        "hardware": {
            "model": "Raspberry Pi Zero 2 W",
            "serial": "0123456789abcdef"
        },
        "accepted": [prior.clone()],
        "transaction": {
            "prior": prior,
            "candidate": {
                "application": candidate_application.clone(),
                "driver": candidate_driver
            },
            "management_helper": {
                "application": candidate_application,
                "target_path": helper_path,
                "protocol": "lifecycle-v3"
            },
            "candidate_owned_files": [{
                "target_path": format!(
                    "/opt/planeradar/releases/2.0.0/{}/planeradar",
                    "2".repeat(64)
                ),
                "sha256": "2".repeat(64)
            }],
            "restored_owned_files": null,
            "phase": "prepared"
        },
        "uninstall": null
    });
    let encoded = serde_json::to_vec(&state).expect("lifecycle JSON");

    write_lifecycle_state_json(&path, &encoded).expect("write lifecycle helper state");
    let returned: serde_json::Value = serde_json::from_str(
        &read_lifecycle_state_json(&path).expect("read lifecycle helper state"),
    )
    .expect("returned lifecycle JSON");
    assert_eq!(
        returned["transaction"]["management_helper"]["target_path"],
        helper_path
    );
    assert_eq!(
        returned["transaction"]["management_helper"]["protocol"],
        "lifecycle-v3"
    );

    let mut wrong_protocol = state.clone();
    wrong_protocol["transaction"]["management_helper"]["protocol"] = "lifecycle-v2".into();
    let mut wrong_path = state.clone();
    wrong_path["transaction"]["management_helper"]["target_path"] =
        "/opt/planeradar/bin/planeradar".into();
    let mut wrong_identity = state;
    wrong_identity["transaction"]["management_helper"]["application"]["sha256"] =
        "9".repeat(64).into();

    for hostile in [wrong_protocol, wrong_path, wrong_identity] {
        let hostile = serde_json::to_vec(&hostile).expect("hostile lifecycle JSON");
        assert!(write_lifecycle_state_json(&path, &hostile).is_err());
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(
                &read_lifecycle_state_json(&path).expect("preserved lifecycle helper state"),
            )
            .expect("preserved lifecycle helper JSON"),
            returned
        );
    }
}

#[test]
fn target_lifecycle_state_rejects_wrong_mode_and_hardlinks() {
    let directory = tempfile::tempdir().expect("temporary state directory");
    let state_directory = directory
        .path()
        .canonicalize()
        .expect("canonical temporary directory")
        .join("lifecycle");
    fs::create_dir(&state_directory).expect("state directory");
    fs::set_permissions(&state_directory, fs::Permissions::from_mode(0o700))
        .expect("private state directory");
    let path = state_directory.join("state.json");
    let json = r#"{"schema_version":1,"hardware":{"model":"Raspberry Pi Zero 2 W","serial":"0123456789abcdef"},"accepted":[{"pair":{"application":{"version":"1.0.0","source_commit":"1111111111111111111111111111111111111111","sha256":"2222222222222222222222222222222222222222222222222222222222222222"},"driver":{"version":"0.1.0","source_commit":"3333333333333333333333333333333333333333","sha256":"4444444444444444444444444444444444444444444444444444444444444444"}},"sequence":1,"owned_files":[{"target_path":"/opt/planeradar/bin/planeradar","sha256":"2222222222222222222222222222222222222222222222222222222222222222"}]}],"transaction":null}"#;

    write_lifecycle_state_json(&path, json.as_bytes()).expect("write lifecycle");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).expect("wrong mode");
    assert!(read_lifecycle_state_json(&path).is_err());
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("restore mode");
    fs::hard_link(&path, state_directory.join("state-link.json")).expect("hardlink lifecycle");
    assert!(read_lifecycle_state_json(&path).is_err());
}

#[test]
fn target_application_ownership_protocol_round_trips_exactly() {
    let owned = vec![InstalledFile {
        target_path: "/opt/planeradar/bin/planeradar".into(),
        sha256: "a".repeat(64),
    }];
    let json = application_release_ownership_json(&owned).expect("ownership JSON");
    assert_eq!(
        parse_application_ownership_json(json.as_bytes()).expect("parse ownership"),
        owned
    );
    assert!(parse_application_ownership_json(format!("{json}\n{{}}").as_bytes()).is_err());
}

#[test]
fn target_lifecycle_cli_exposes_only_typed_state_activation_and_uninstall_arguments() {
    let protocol =
        Cli::try_parse_from(["planeradar", "lifecycle-protocol"]).expect("lifecycle protocol");
    assert!(matches!(protocol.command, CliCommand::LifecycleProtocol));

    let state =
        Cli::try_parse_from(["planeradar", "lifecycle-state", "read"]).expect("lifecycle state");
    assert!(matches!(state.command, CliCommand::LifecycleState { .. }));

    let activate = Cli::try_parse_from([
        "planeradar",
        "lifecycle-activate",
        "--artifact",
        "/opt/planeradar/releases/1.0.0/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/planeradar",
        "--version",
        "1.0.0",
        "--revision",
        "1111111111111111111111111111111111111111",
        "--sha256",
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "--owned-json",
        "{\"schema_version\":1,\"owned_files\":[]}",
    ])
    .expect("lifecycle activate");
    assert!(matches!(
        activate.command,
        CliCommand::LifecycleActivate { version, .. } if version == "1.0.0"
    ));

    let uninstall = Cli::try_parse_from([
        "planeradar",
        "lifecycle-uninstall",
        "--owned-json",
        "{\"schema_version\":1,\"owned_files\":[]}",
        "--purge-settings",
    ])
    .expect("lifecycle uninstall");
    assert!(matches!(
        uninstall.command,
        CliCommand::LifecycleUninstall {
            purge_settings: true,
            ..
        }
    ));

    let retire = Cli::try_parse_from([
        "planeradar",
        "lifecycle-retire",
        "--owned-json",
        "{\"schema_version\":1,\"owned_files\":[]}",
    ])
    .expect("lifecycle retire");
    assert!(matches!(retire.command, CliCommand::LifecycleRetire { .. }));
}

#[test]
fn target_lifecycle_protocol_reports_the_exact_management_contract() {
    let output = Command::new(env!("CARGO_BIN_EXE_planeradar"))
        .arg("lifecycle-protocol")
        .output()
        .expect("run target lifecycle protocol");

    assert!(output.status.success());
    assert_eq!(output.stdout, b"lifecycle-v3\n");
    assert!(output.stderr.is_empty());
}

#[test]
fn failed_candidate_retirement_is_exact_idempotent_and_never_accepts_live_paths() {
    let fixture = Fixture::new("[all]\n");
    let digest = format!("{:x}", Sha256::digest(b"candidate"));
    let release = fixture
        .root
        .join("opt/planeradar/releases/1.2.3")
        .join(&digest)
        .join("planeradar");
    let helper = fixture
        .root
        .join("var/lib/planeradar-installer/helpers")
        .join(&digest)
        .join("planeradar");
    for path in [&release, &helper] {
        fs::create_dir_all(path.parent().unwrap()).expect("candidate parent");
        fs::write(path, b"candidate").expect("candidate artifact");
    }
    let owned = vec![
        InstalledFile {
            target_path: format!("/opt/planeradar/releases/1.2.3/{digest}/planeradar"),
            sha256: digest.clone(),
        },
        InstalledFile {
            target_path: format!("/var/lib/planeradar-installer/helpers/{digest}/planeradar"),
            sha256: digest.clone(),
        },
    ];

    retire_application_artifacts(&fixture.root, &owned).expect("retire candidate");
    retire_application_artifacts(&fixture.root, &owned).expect("idempotent retire");
    assert!(!release.exists());
    assert!(!helper.exists());
    assert!(
        retire_application_artifacts(
            &fixture.root,
            &[InstalledFile {
                target_path: "/opt/planeradar/bin/planeradar".into(),
                sha256: "a".repeat(64),
            }]
        )
        .is_err()
    );
}

#[test]
fn interrupted_mixed_application_switch_restores_only_transaction_proven_bytes() {
    let fixture = Fixture::new("[all]\n");
    let runner = RecordingRunner::for_root(&fixture.root);
    let installed = Installer::new(&runner)
        .install(&fixture.options(false))
        .expect("initial install");
    let prior_bytes = fs::read(&fixture.artifact).expect("prior artifact");
    let prior_sha = format!("{:x}", Sha256::digest(&prior_bytes));
    let prior_revision = "a".repeat(40);
    let revision_bytes = format!("{prior_revision}\n");
    fs::write(
        fixture.root.join("opt/planeradar/REVISION"),
        revision_bytes.as_bytes(),
    )
    .expect("valid prior revision");
    let mut prior_owned = installed.owned_files.clone();
    prior_owned
        .iter_mut()
        .find(|file| file.target_path == "/opt/planeradar/REVISION")
        .expect("revision ownership")
        .sha256 = format!("{:x}", Sha256::digest(revision_bytes.as_bytes()));
    let prior_identity = ApplicationReleaseIdentity {
        version: "1.0.0".into(),
        revision: prior_revision.clone(),
        sha256: prior_sha.clone(),
    };
    let mut candidate_bytes = prior_bytes.clone();
    candidate_bytes.push(42);
    let candidate_sha = format!("{:x}", Sha256::digest(&candidate_bytes));
    let lifecycle_path = fixture
        .root
        .canonicalize()
        .expect("canonical fixture root")
        .join("var/lib/planeradar-installer/lifecycle.json");
    fs::create_dir_all(lifecycle_path.parent().expect("lifecycle parent"))
        .expect("lifecycle state directory");
    fs::set_permissions(
        lifecycle_path.parent().expect("lifecycle parent"),
        fs::Permissions::from_mode(0o700),
    )
    .expect("private lifecycle state directory");
    let accepted_owned = serde_json::to_value(&prior_owned).unwrap();
    let state = serde_json::json!({
        "schema_version": 1,
        "hardware": {
            "model": "Raspberry Pi Zero 2 W",
            "serial": "0123456789abcdef"
        },
        "accepted": [{
            "pair": {
                "application": {
                    "version": "1.0.0",
                    "source_commit": prior_revision,
                    "sha256": prior_sha
                },
                "driver": {
                    "version": "0.1.0",
                    "source_commit": "3".repeat(40),
                    "sha256": "4".repeat(64)
                }
            },
            "sequence": 1,
            "owned_files": accepted_owned
        }],
        "transaction": {
            "prior": {
                "pair": {
                    "application": {
                        "version": "1.0.0",
                        "source_commit": prior_identity.revision,
                        "sha256": prior_identity.sha256
                    },
                    "driver": {
                        "version": "0.1.0",
                        "source_commit": "3".repeat(40),
                        "sha256": "4".repeat(64)
                    }
                },
                "sequence": 1,
                "owned_files": prior_owned.clone()
            },
            "candidate": {
                "application": {
                    "version": "2.0.0",
                    "source_commit": "b".repeat(40),
                    "sha256": candidate_sha
                },
                "driver": {
                    "version": "0.1.0",
                    "source_commit": "3".repeat(40),
                    "sha256": "4".repeat(64)
                }
            },
            "phase": "application_activated"
        }
    });
    let encoded_state = serde_json::to_string(&state).unwrap();
    write_lifecycle_state_json(&lifecycle_path, encoded_state.as_bytes())
        .unwrap_or_else(|error| panic!("transaction state {error}: {encoded_state}"));
    fs::write(
        fixture.root.join("opt/planeradar/bin/planeradar"),
        candidate_bytes,
    )
    .expect("interrupted live switch");

    activate_application_release(
        &fixture.root,
        &fixture.artifact,
        &prior_identity,
        &prior_owned,
    )
    .expect("transaction-proven recovery");

    assert_eq!(
        fs::read(fixture.root.join("opt/planeradar/bin/planeradar")).unwrap(),
        prior_bytes
    );
}

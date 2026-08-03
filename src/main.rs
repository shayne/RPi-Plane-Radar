#![forbid(unsafe_code)]

use std::io::{self, Write};
use std::thread;
use std::time::Duration;

use clap::Parser;
use planeradar::app::PlaneRadarApp;
use planeradar::backlight::{Backlight, NoopBacklight, SysfsBacklight};
use planeradar::capture::{
    CapturePaths, capture_metadata, capture_snapshot_protocol, metadata_json, parse_metadata_json,
    parse_snapshot_protocol,
};
use planeradar::cli::{Cli, Command, DemoCommand, InstallerStateCommand, version_line};
use planeradar::display::{DisplayConfig, run_display, run_probe};
use planeradar::install::{
    ApplicationReleaseIdentity, BootConfigEditor, DisplaySelection, InstallOptions, Installer,
    MachineOutputCommandRunner, SystemCommandRunner, activate_application_release,
    application_release_ownership_json, commit_display_config, ensure_overlay,
    installer_ownership_json, parse_application_ownership_json, read_installer_state_json,
    read_lifecycle_state_json, read_optional_installer_state_json,
    read_optional_lifecycle_state_json, retire_application_artifacts, rollback_display_config,
    stage_tryboot_config, stage_tryboot_config_if_source_matches, uninstall_owned_installation,
    write_installer_state_json, write_lifecycle_state_json,
};
use planeradar::logging;
use planeradar::render::FontAsset;
use planeradar::render::radar::RadarRenderer;
use planeradar::render::radar::{run_radar_demo, write_fixtures};
use planeradar::render::setup::SetupRenderer;
use planeradar::render::setup::{run_setup_demo, write_fixtures as write_setup_fixtures};
use planeradar::runtime::{RuntimeConfig, RuntimeCoordinator};

const BACKLIGHT_CLASS_DEVICE: &str = "/sys/class/backlight/planeradar-backlight";

fn main() {
    if let Err(error) = logging::init() {
        eprintln!("planeradar: logger initialization failed: {error}");
        std::process::exit(1);
    }
    if let Err(error) = run() {
        eprintln!("planeradar: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    match Cli::parse().command {
        Command::Run {
            headless,
            settings,
            geocode_cache,
            http,
            local_url,
            nominatim_url,
            debug_frame,
        } => {
            let local_url = match local_url {
                Some(url) => planeradar::network::local_url_override(&url)?,
                None => {
                    let hostname = std::fs::read_to_string("/etc/hostname")?;
                    planeradar::network::local_url(hostname.trim())?
                }
            };
            let mut runtime_config = RuntimeConfig::default();
            (
                runtime_config.settings_path,
                runtime_config.geocode_cache_path,
                runtime_config.http_address,
                runtime_config.local_url,
                runtime_config.nominatim_url,
            ) = (settings, geocode_cache, http, local_url, nominatim_url);
            let handle = RuntimeCoordinator::start(runtime_config)?;
            if headless {
                while !handle.stop_requested() {
                    thread::sleep(Duration::from_millis(50));
                }
                handle.shutdown()?;
            } else {
                let font = FontAsset::embedded()?;
                let backlight: Box<dyn Backlight> =
                    match SysfsBacklight::open(std::path::PathBuf::from(BACKLIGHT_CLASS_DEVICE)) {
                        Ok(backlight) => Box::new(backlight),
                        Err(_) => {
                            log::warn!("display backlight is unavailable");
                            Box::new(NoopBacklight)
                        }
                    };
                let mut app = PlaneRadarApp::new(
                    handle,
                    RadarRenderer::new(font.clone()),
                    SetupRenderer::new(font),
                    backlight,
                );
                app.install_debug_signal(debug_frame)?;
                let display_result = run_display(DisplayConfig::default(), &mut app);
                let shutdown_result = app.shutdown();
                display_result?;
                shutdown_result?;
            }
        }
        Command::Version => println!("{}", version_line()),
        Command::Probe => run_probe()?,
        Command::Demo {
            command: DemoCommand::Radar { seconds },
        } => run_radar_demo(seconds)?,
        Command::Demo {
            command: DemoCommand::Setup { ip_url, seconds },
        } => run_setup_demo(ip_url.as_deref(), seconds)?,
        Command::RenderFixtures { output } => {
            write_fixtures(&output)?;
            write_setup_fixtures(&output)?;
        }
        Command::Install {
            artifact,
            checksum_file,
            revision_file,
            reboot,
            json,
        } => {
            let options = InstallOptions {
                root: "/".into(),
                boot_config: "/boot/firmware/config.txt".into(),
                artifact,
                checksum_file,
                revision_file,
                reboot,
            };
            let result = if json {
                Installer::new(&MachineOutputCommandRunner).install(&options)?
            } else {
                Installer::new(&SystemCommandRunner).install(&options)?
            };
            if json {
                println!("{}", result.to_json()?);
            } else {
                println!("files_changed={}", result.files_changed);
                println!("boot_config_changed={}", result.boot_config_changed);
                println!("reboot_required={}", result.reboot_required);
            }
        }
        Command::InstallerState {
            command: InstallerStateCommand::Read,
        } => {
            println!(
                "{}",
                read_optional_installer_state_json(std::path::Path::new(
                    "/var/lib/planeradar-installer/state.json",
                ))?
            );
        }
        Command::InstallerState {
            command: InstallerStateCommand::Write { json },
        } => {
            write_installer_state_json(
                std::path::Path::new("/var/lib/planeradar-installer/state.json"),
                json.as_bytes(),
            )?;
            println!(
                "{}",
                read_installer_state_json(std::path::Path::new(
                    "/var/lib/planeradar-installer/state.json",
                ))?
            );
        }
        Command::LifecycleProtocol => {
            println!("lifecycle-v3");
        }
        Command::LifecycleState {
            command: InstallerStateCommand::Read,
        } => {
            println!(
                "{}",
                read_optional_lifecycle_state_json(std::path::Path::new(
                    "/var/lib/planeradar-installer/lifecycle.json",
                ))?
            );
        }
        Command::LifecycleState {
            command: InstallerStateCommand::Write { json },
        } => {
            let path = std::path::Path::new("/var/lib/planeradar-installer/lifecycle.json");
            write_lifecycle_state_json(path, json.as_bytes())?;
            println!("{}", read_lifecycle_state_json(path)?);
        }
        Command::LifecycleActivate {
            artifact,
            version,
            revision,
            sha256,
            owned_json,
        } => {
            let current = parse_application_ownership_json(owned_json.as_bytes())?;
            let owned = activate_application_release(
                std::path::Path::new("/"),
                &artifact,
                &ApplicationReleaseIdentity {
                    version,
                    revision,
                    sha256,
                },
                &current,
            )?;
            println!("{}", application_release_ownership_json(&owned)?);
        }
        Command::LifecycleUninstall {
            owned_json,
            purge_settings,
        } => {
            let owned = parse_application_ownership_json(owned_json.as_bytes())?;
            uninstall_owned_installation(
                std::path::Path::new("/"),
                &owned,
                purge_settings,
                &SystemCommandRunner,
            )?;
            println!("uninstalled");
        }
        Command::LifecycleRetire { owned_json } => {
            let owned = parse_application_ownership_json(owned_json.as_bytes())?;
            retire_application_artifacts(std::path::Path::new("/"), &owned)?;
            println!("retired");
        }
        Command::InstallerOwnership => {
            println!("{}", installer_ownership_json(std::path::Path::new("/"))?);
        }
        Command::CaptureMetadata => {
            let metadata = capture_metadata(CapturePaths::production().debug_frame())?;
            println!(
                "{}",
                metadata
                    .as_ref()
                    .map(metadata_json)
                    .transpose()?
                    .unwrap_or_else(|| "null".to_owned())
            );
        }
        Command::CaptureSnapshot { before, timeout_ms } => {
            let before = match before.as_str() {
                "none" => None,
                value => Some(parse_metadata_json(value)?),
            };
            let protocol = capture_snapshot_protocol(
                &CapturePaths::production(),
                before.as_ref(),
                Duration::from_millis(timeout_ms),
            )?;
            let snapshot = parse_snapshot_protocol(&protocol)?;
            if snapshot.published.uid != 0
                || snapshot.published.gid != 0
                || snapshot.rechecked.uid != 0
                || snapshot.rechecked.gid != 0
            {
                return Err("privileged capture is not root-owned".into());
            }
            io::stdout().write_all(&protocol)?;
            io::stdout().flush()?;
        }
        Command::ConfigureDisplay {
            boot_config,
            declaration,
        } => {
            let editor = BootConfigEditor::acquire(&boot_config)?;
            let source = editor.read_source()?;
            let (updated, changed) = ensure_overlay(&source, &declaration);
            if !changed {
                println!("unchanged");
                return Ok(());
            }

            println!("--- {}", boot_config.display());
            println!("+++ {}", boot_config.display());
            print_block('-', &source);
            print_block('+', &updated);
            print!("Apply these changes? [y/N] ");
            io::stdout().flush()?;
            let mut response = String::new();
            io::stdin().read_line(&mut response)?;
            if !matches!(response.trim(), "y" | "Y" | "yes" | "YES") {
                println!("cancelled");
                return Ok(());
            }

            if editor.edit_from_source(&source, &declaration)? {
                println!("changed");
            } else {
                println!("unchanged");
            }
        }
        Command::StageDisplay {
            boot_config,
            tryboot_config,
            expected_boot_config_sha256,
            overlay,
            parameters,
        } => {
            let parameters: Vec<_> = parameters.iter().map(String::as_str).collect();
            let selection = DisplaySelection::Candidate {
                overlay: &overlay,
                parameters: &parameters,
            };
            if let Some(expected) = expected_boot_config_sha256 {
                stage_tryboot_config_if_source_matches(
                    &boot_config,
                    &tryboot_config,
                    &expected,
                    selection,
                )?;
            } else {
                stage_tryboot_config(&boot_config, &tryboot_config, selection)?;
            }
            println!("staged {}", tryboot_config.display());
        }
        Command::CommitDisplay {
            boot_config,
            overlay,
            parameters,
        } => {
            let parameters: Vec<_> = parameters.iter().map(String::as_str).collect();
            let changed = commit_display_config(
                &boot_config,
                DisplaySelection::Candidate {
                    overlay: &overlay,
                    parameters: &parameters,
                },
            )?;
            println!("{}", if changed { "changed" } else { "unchanged" });
        }
        Command::RollbackDisplay { boot_config } => {
            let changed = rollback_display_config(&boot_config)?;
            println!("{}", if changed { "changed" } else { "unchanged" });
        }
    }
    Ok(())
}

fn print_block(prefix: char, contents: &str) {
    for line in contents.lines() {
        println!("{prefix}{line}");
    }
    if contents.is_empty() {
        println!("{prefix}");
    }
}

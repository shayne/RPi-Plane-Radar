use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

#[derive(Clone, Debug, Parser)]
#[command(name = "planeradarctl")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Clone, Debug, Subcommand)]
pub enum Command {
    Install(MutatingOptions),
    Upgrade(MutatingOptions),
    Status(TargetOptions),
    Doctor(DoctorOptions),
    Screenshot(ScreenshotOptions),
    Rollback(MutatingOptions),
    Uninstall(UninstallOptions),
    Driver {
        #[command(subcommand)]
        command: DriverCommand,
    },
}

impl Command {
    pub fn is_mutating(&self) -> bool {
        matches!(
            self,
            Self::Install(_) | Self::Upgrade(_) | Self::Rollback(_) | Self::Uninstall(_)
        )
    }
}

#[derive(Clone, Debug, Subcommand)]
pub enum DriverCommand {
    Sync,
    Update { version: String },
}

#[derive(Clone, Debug, Args)]
pub struct MutatingOptions {
    #[arg(value_name = "target")]
    pub target: Option<String>,
    #[arg(long)]
    pub hostname: Option<String>,
    #[arg(long)]
    pub version: Option<String>,
    #[arg(long)]
    pub release_dir: Option<PathBuf>,
    #[arg(long)]
    pub docker_context: Option<String>,
    #[arg(long)]
    pub non_interactive: bool,
}

#[derive(Clone, Debug, Args)]
pub struct UninstallOptions {
    #[arg(value_name = "target")]
    pub target: Option<String>,
    #[arg(long)]
    pub non_interactive: bool,
    #[arg(long)]
    pub purge_settings: bool,
}

#[derive(Clone, Debug, Args)]
pub struct TargetOptions {
    #[arg(value_name = "target")]
    pub target: Option<String>,
}

#[derive(Clone, Debug, Args)]
pub struct DoctorOptions {
    #[arg(value_name = "target")]
    pub target: Option<String>,
    #[arg(long)]
    pub json: bool,
}

#[derive(Clone, Debug, Args)]
pub struct ScreenshotOptions {
    #[arg(value_name = "target")]
    pub target: Option<String>,
    #[arg(long, default_value = "planeradar-radar.png")]
    pub output: PathBuf,
}

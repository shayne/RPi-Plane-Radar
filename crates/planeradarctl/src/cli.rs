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
    Doctor(TargetOptions),
    Screenshot(TargetOptions),
    Rollback(MutatingOptions),
    Uninstall(MutatingOptions),
    Driver {
        #[command(subcommand)]
        command: DriverCommand,
    },
}

impl Command {
    pub fn into_mutating_options(self) -> Option<MutatingOptions> {
        match self {
            Self::Install(options)
            | Self::Upgrade(options)
            | Self::Rollback(options)
            | Self::Uninstall(options) => Some(options),
            Self::Status(_) | Self::Doctor(_) | Self::Screenshot(_) | Self::Driver { .. } => None,
        }
    }

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
pub struct TargetOptions {
    #[arg(value_name = "target")]
    pub target: Option<String>,
}

use clap::{Parser, Subcommand};
use std::path::PathBuf;

use crate::install::DEFAULT_HYPERPIXEL_DECLARATION;

#[derive(Debug, Parser)]
#[command(name = "planeradar")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    Version,
    Probe,
    Demo {
        #[command(subcommand)]
        command: DemoCommand,
    },
    RenderFixtures {
        #[arg(long)]
        output: PathBuf,
    },
    ConfigureDisplay {
        #[arg(long, default_value = "/boot/firmware/config.txt")]
        boot_config: PathBuf,
        #[arg(long, default_value = DEFAULT_HYPERPIXEL_DECLARATION)]
        declaration: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum DemoCommand {
    Radar {
        #[arg(long)]
        seconds: u64,
    },
    Setup {
        #[arg(long)]
        ip_url: Option<String>,
        #[arg(long)]
        seconds: u64,
    },
}

pub fn version_line() -> String {
    format!(
        "planeradar {} ({})",
        env!("CARGO_PKG_VERSION"),
        env!("PLANERADAR_REVISION")
    )
}

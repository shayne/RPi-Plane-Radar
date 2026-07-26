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
    ConfigureDisplay {
        #[arg(long, default_value = "/boot/firmware/config.txt")]
        boot_config: PathBuf,
        #[arg(long, default_value = DEFAULT_HYPERPIXEL_DECLARATION)]
        declaration: String,
    },
}

pub fn version_line() -> String {
    format!(
        "planeradar {} ({})",
        env!("CARGO_PKG_VERSION"),
        env!("PLANERADAR_REVISION")
    )
}

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "planeradar")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    Version,
}

pub fn version_line() -> String {
    format!(
        "planeradar {} ({})",
        env!("CARGO_PKG_VERSION"),
        env!("PLANERADAR_REVISION")
    )
}

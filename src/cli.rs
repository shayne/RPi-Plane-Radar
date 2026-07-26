use clap::{Parser, Subcommand};
use std::net::SocketAddr;
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
    Run {
        #[arg(long)]
        headless: bool,
        #[arg(
            long,
            env = "PLANERADAR_SETTINGS",
            default_value = "/var/lib/planeradar/settings.json"
        )]
        settings: PathBuf,
        #[arg(
            long,
            env = "PLANERADAR_GEOCODE_CACHE",
            default_value = "/var/lib/planeradar/geocode-cache.json"
        )]
        geocode_cache: PathBuf,
        #[arg(long, env = "PLANERADAR_HTTP", default_value = "0.0.0.0:80")]
        http: SocketAddr,
        #[arg(
            long,
            env = "PLANERADAR_LOCAL_URL",
            default_value = "http://planeradar.local"
        )]
        local_url: String,
        #[arg(
            long,
            env = "PLANERADAR_NOMINATIM_URL",
            default_value = "https://nominatim.openstreetmap.org/search"
        )]
        nominatim_url: String,
        #[arg(
            long,
            env = "PLANERADAR_DEBUG_FRAME",
            default_value = "/var/lib/planeradar/debug.png"
        )]
        debug_frame: PathBuf,
    },
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

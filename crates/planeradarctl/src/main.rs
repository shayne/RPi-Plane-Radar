use std::path::Path;
use std::process::ExitCode;

use clap::Parser;
use planeradarctl::{cli::Cli, config::Environment, config::InstallConfig};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("planeradarctl: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    let environment = Environment::from_dotenv_path(Path::new(".env"))?;
    if cli.command.is_mutating() {
        let _config = InstallConfig::resolve(cli, environment)?;
    }
    Ok(())
}

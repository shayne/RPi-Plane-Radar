use std::process::ExitCode;
use std::{fs, path::Path};

use clap::Parser;
use planeradarctl::{
    DriverLock,
    cli::{Cli, Command, DriverCommand},
    config::{Environment, InstallConfig},
    driver::{DriverManager, GhDriverReleaseSource, GhDriverReleaseVerifier},
};
use semver::Version;

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
    if let Command::Driver { command } = cli.command.clone() {
        return run_driver(command);
    }
    let environment = Environment::from_dotenv_path(Path::new(".env"))?;
    if cli.command.is_mutating() {
        let _config = InstallConfig::resolve(cli, environment)?;
    }
    Ok(())
}

fn run_driver(command: DriverCommand) -> Result<(), Box<dyn std::error::Error>> {
    let repository = std::env::current_dir()?;
    let cache = repository.join(".cache/driver");
    let manager = DriverManager::new(
        GhDriverReleaseSource::system(),
        GhDriverReleaseVerifier::system(),
        cache,
    );
    match command {
        DriverCommand::Sync => {
            let lock =
                DriverLock::parse(&fs::read_to_string(repository.join("driver.lock.toml"))?)?;
            let synced = manager.sync(&lock)?;
            println!("Synced locked HyperPixel driver {}", synced.lock().version);
        }
        DriverCommand::Update { version } => {
            let version = Version::parse(&version)?;
            let lock = manager.update(&repository.join("driver.lock.toml"), &version)?;
            println!("Updated HyperPixel driver lock to {}", lock.version);
        }
    }
    Ok(())
}

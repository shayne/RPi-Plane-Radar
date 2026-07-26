#![forbid(unsafe_code)]

use std::io::{self, Write};

use clap::Parser;
use planeradar::cli::{Cli, Command, DemoCommand, version_line};
use planeradar::display::run_probe;
use planeradar::install::{BootConfigEditor, ensure_overlay};
use planeradar::logging;
use planeradar::render::radar::{run_radar_demo, write_fixtures};
use planeradar::render::setup::{run_setup_demo, write_fixtures as write_setup_fixtures};

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

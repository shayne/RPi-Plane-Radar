#![forbid(unsafe_code)]

use clap::Parser;
use planeradar::cli::{Cli, Command, version_line};

fn main() {
    match Cli::parse().command {
        Command::Version => println!("{}", version_line()),
    }
}

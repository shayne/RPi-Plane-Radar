#![forbid(unsafe_code)]

use std::fs::File;
use std::io::{BufReader, BufWriter};
use std::path::PathBuf;

use planeradar::airports::{AirportError, build_dataset, write_dataset};
use thiserror::Error;

#[derive(Debug, Error)]
enum BuildAirportsError {
    #[error("usage: build-airports <airports.csv> <runways.csv> <output.json.gz>")]
    Usage,
    #[error("failed to open {path}: {source}")]
    Open {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("airport dataset failed: {0}")]
    Dataset(#[from] AirportError),
}

fn main() -> Result<(), BuildAirportsError> {
    let mut arguments = std::env::args_os().skip(1);
    let airports_path = arguments
        .next()
        .map(PathBuf::from)
        .ok_or(BuildAirportsError::Usage)?;
    let runways_path = arguments
        .next()
        .map(PathBuf::from)
        .ok_or(BuildAirportsError::Usage)?;
    let output_path = arguments
        .next()
        .map(PathBuf::from)
        .ok_or(BuildAirportsError::Usage)?;
    if arguments.next().is_some() {
        return Err(BuildAirportsError::Usage);
    }

    let airports = File::open(&airports_path).map_err(|source| BuildAirportsError::Open {
        path: airports_path,
        source,
    })?;
    let runways = File::open(&runways_path).map_err(|source| BuildAirportsError::Open {
        path: runways_path,
        source,
    })?;
    let dataset = build_dataset(BufReader::new(airports), BufReader::new(runways))?;
    let airport_count = dataset.airports.len();
    let runway_count = dataset
        .airports
        .iter()
        .map(|airport| airport.runways.len())
        .sum::<usize>();
    let output = File::create(&output_path).map_err(|source| BuildAirportsError::Open {
        path: output_path,
        source,
    })?;
    write_dataset(&dataset, BufWriter::new(output))?;
    println!("wrote {airport_count} airports and {runway_count} runways");
    Ok(())
}

use std::collections::BTreeMap;
use std::io::{Read, Write};

use csv::{Reader, StringRecord};
use flate2::Compression;
use flate2::GzBuilder;
use flate2::bufread::GzDecoder;
use flate2::write::GzEncoder;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::geometry::offset_km;
use crate::model::{Airport, GeoPoint, Location, Runway};

const SCHEMA_VERSION: u32 = 1;
const SOURCE: &str = "OurAirports";
const MAX_DECOMPRESSED_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AirportDataset {
    pub schema_version: u32,
    pub source: String,
    pub airports: Vec<DatasetAirport>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DatasetAirport {
    pub ident: String,
    pub latitude: f64,
    pub longitude: f64,
    pub runways: Vec<DatasetRunway>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DatasetRunway {
    pub le: [f64; 2],
    pub he: [f64; 2],
}

#[derive(Debug, Error)]
pub enum AirportError {
    #[error("airport CSV failed: {0}")]
    Csv(#[from] csv::Error),
    #[error("{dataset} CSV is missing required header {missing}")]
    CsvSchema {
        dataset: &'static str,
        missing: &'static str,
    },
    #[error("duplicate large-airport identifier {0}")]
    DuplicateAirport(String),
    #[error("dataset I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("dataset JSON failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("unsupported airport dataset schema {0}")]
    UnsupportedSchema(u32),
    #[error("unexpected airport dataset source {0}")]
    UnexpectedSource(String),
    #[error("invalid coordinate at {field}")]
    InvalidCoordinate { field: String },
    #[error("decompressed airport dataset exceeds its safety limit")]
    DatasetTooLarge,
}

pub fn build_dataset(
    airports: impl Read,
    runways: impl Read,
) -> Result<AirportDataset, AirportError> {
    let mut airport_reader = Reader::from_reader(airports);
    let airport_headers = airport_reader.headers()?.clone();
    let airport_columns = RequiredColumns::new(
        "airports",
        &airport_headers,
        &["ident", "type", "latitude_deg", "longitude_deg"],
    )?;
    let mut selected = BTreeMap::new();

    for record in airport_reader.records() {
        let record = record?;
        if airport_columns.value(&record, "type").trim() != "large_airport" {
            continue;
        }
        let ident = airport_columns.value(&record, "ident").trim();
        let Some(latitude) = parse_coordinate(airport_columns.value(&record, "latitude_deg"), true)
        else {
            continue;
        };
        let Some(longitude) =
            parse_coordinate(airport_columns.value(&record, "longitude_deg"), false)
        else {
            continue;
        };
        if ident.is_empty() {
            continue;
        }

        let airport = DatasetAirport {
            ident: ident.to_owned(),
            latitude: round_seven(latitude),
            longitude: round_seven(longitude),
            runways: Vec::new(),
        };
        if selected.insert(airport.ident.clone(), airport).is_some() {
            return Err(AirportError::DuplicateAirport(ident.to_owned()));
        }
    }

    let mut runway_reader = Reader::from_reader(runways);
    let runway_headers = runway_reader.headers()?.clone();
    let runway_columns = RequiredColumns::new(
        "runways",
        &runway_headers,
        &[
            "airport_ident",
            "length_ft",
            "closed",
            "le_ident",
            "le_latitude_deg",
            "le_longitude_deg",
            "he_ident",
            "he_latitude_deg",
            "he_longitude_deg",
        ],
    )?;

    for record in runway_reader.records() {
        let record = record?;
        let ident = runway_columns.value(&record, "airport_ident").trim();
        let Some(airport) = selected.get_mut(ident) else {
            continue;
        };
        if runway_columns.value(&record, "closed").trim() != "0" {
            continue;
        }
        let Ok(length_feet) = runway_columns
            .value(&record, "length_ft")
            .trim()
            .parse::<u32>()
        else {
            continue;
        };
        if length_feet == 0 || is_helipad(&runway_columns, &record, length_feet) {
            continue;
        }
        let Some(le) = parse_endpoint(&runway_columns, &record, "le") else {
            continue;
        };
        let Some(he) = parse_endpoint(&runway_columns, &record, "he") else {
            continue;
        };
        airport.runways.push(DatasetRunway { le, he });
    }

    let mut airports: Vec<_> = selected.into_values().collect();
    for airport in &mut airports {
        airport.runways.sort_by(compare_runways);
    }

    Ok(AirportDataset {
        schema_version: SCHEMA_VERSION,
        source: SOURCE.to_owned(),
        airports,
    })
}

pub fn write_dataset(dataset: &AirportDataset, writer: impl Write) -> Result<(), AirportError> {
    validate_dataset(dataset)?;
    let mut normalized = dataset.clone();
    normalized
        .airports
        .sort_by(|left, right| left.ident.cmp(&right.ident));
    for airport in &mut normalized.airports {
        airport.runways.sort_by(compare_runways);
    }

    let mut encoder: GzEncoder<_> = GzBuilder::new()
        .mtime(0)
        .write(writer, Compression::default());
    serde_json::to_writer(&mut encoder, &normalized)?;
    encoder.finish()?;
    Ok(())
}

pub fn read_dataset(reader: impl Read) -> Result<Vec<Airport>, AirportError> {
    let decoder = GzDecoder::new(std::io::BufReader::new(reader));
    let mut limited = decoder.take(MAX_DECOMPRESSED_BYTES + 1);
    let mut json = Vec::new();
    limited.read_to_end(&mut json)?;
    if json.len() as u64 > MAX_DECOMPRESSED_BYTES {
        return Err(AirportError::DatasetTooLarge);
    }

    let dataset: AirportDataset = serde_json::from_slice(&json)?;
    validate_dataset(&dataset)?;
    Ok(dataset
        .airports
        .into_iter()
        .map(|airport| Airport {
            ident: airport.ident,
            location: GeoPoint {
                latitude: airport.latitude,
                longitude: airport.longitude,
            },
            runways: airport
                .runways
                .into_iter()
                .map(|runway| Runway {
                    low_end: GeoPoint {
                        latitude: runway.le[0],
                        longitude: runway.le[1],
                    },
                    high_end: GeoPoint {
                        latitude: runway.he[0],
                        longitude: runway.he[1],
                    },
                })
                .collect(),
        })
        .collect())
}

pub fn load_embedded() -> Result<Vec<Airport>, AirportError> {
    read_dataset(include_bytes!("assets/large_airports.json.gz").as_slice())
}

pub fn airports_within<'a>(
    airports: &'a [Airport],
    origin: &Location,
    radius_km: f64,
    max: usize,
) -> Vec<&'a Airport> {
    if max == 0
        || !radius_km.is_finite()
        || radius_km <= 0.0
        || !valid_latitude(origin.latitude)
        || !valid_longitude(origin.longitude)
    {
        return Vec::new();
    }

    let mut matches: Vec<_> = airports
        .iter()
        .filter_map(|airport| {
            if !valid_latitude(airport.location.latitude)
                || !valid_longitude(airport.location.longitude)
            {
                return None;
            }
            let offset = offset_km(
                origin,
                airport.location.latitude,
                airport.location.longitude,
            );
            let distance = f64::hypot(offset.east, offset.north);
            (distance.is_finite() && distance <= radius_km).then_some((distance, airport))
        })
        .collect();
    matches.sort_by(|(left_distance, left), (right_distance, right)| {
        left_distance
            .total_cmp(right_distance)
            .then_with(|| left.ident.cmp(&right.ident))
    });
    matches
        .into_iter()
        .take(max)
        .map(|(_, airport)| airport)
        .collect()
}

struct RequiredColumns {
    dataset: &'static str,
    columns: BTreeMap<&'static str, usize>,
}

impl RequiredColumns {
    fn new(
        dataset: &'static str,
        headers: &StringRecord,
        required: &[&'static str],
    ) -> Result<Self, AirportError> {
        let mut columns = BTreeMap::new();
        for &name in required {
            let Some(index) = headers.iter().position(|header| header == name) else {
                return Err(AirportError::CsvSchema {
                    dataset,
                    missing: name,
                });
            };
            columns.insert(name, index);
        }
        Ok(Self { dataset, columns })
    }

    fn value<'a>(&self, record: &'a StringRecord, name: &'static str) -> &'a str {
        let index = self.columns[&name];
        record
            .get(index)
            .unwrap_or_else(|| panic!("validated {} CSV record width", self.dataset))
    }
}

fn parse_endpoint(
    columns: &RequiredColumns,
    record: &StringRecord,
    prefix: &'static str,
) -> Option<[f64; 2]> {
    let (latitude_name, longitude_name) = match prefix {
        "le" => ("le_latitude_deg", "le_longitude_deg"),
        "he" => ("he_latitude_deg", "he_longitude_deg"),
        _ => return None,
    };
    let latitude = parse_coordinate(columns.value(record, latitude_name), true)?;
    let longitude = parse_coordinate(columns.value(record, longitude_name), false)?;
    Some([round_seven(latitude), round_seven(longitude)])
}

fn is_helipad(columns: &RequiredColumns, record: &StringRecord, length_feet: u32) -> bool {
    let low_is_helipad = is_helipad_designator(columns.value(record, "le_ident"));
    let high_is_helipad = is_helipad_designator(columns.value(record, "he_ident"));
    (low_is_helipad && high_is_helipad)
        || ((low_is_helipad || high_is_helipad) && length_feet < 2_500)
}

fn is_helipad_designator(value: &str) -> bool {
    let value = value.trim().to_ascii_uppercase();
    let Some(rest) = value.strip_prefix('H') else {
        return false;
    };
    rest.is_empty()
        || rest.starts_with('-')
        || rest.starts_with('_')
        || rest.chars().all(|character| character.is_ascii_digit())
}

fn parse_coordinate(value: &str, latitude: bool) -> Option<f64> {
    let parsed = value.trim().parse::<f64>().ok()?;
    let valid = if latitude {
        valid_latitude(parsed)
    } else {
        valid_longitude(parsed)
    };
    valid.then_some(parsed)
}

fn valid_latitude(value: f64) -> bool {
    value.is_finite() && (-90.0..=90.0).contains(&value)
}

fn valid_longitude(value: f64) -> bool {
    value.is_finite() && (-180.0..=180.0).contains(&value)
}

fn round_seven(value: f64) -> f64 {
    let rounded = (value * 10_000_000.0).round() / 10_000_000.0;
    if rounded == 0.0 { 0.0 } else { rounded }
}

fn compare_runways(left: &DatasetRunway, right: &DatasetRunway) -> std::cmp::Ordering {
    left.le
        .iter()
        .chain(left.he.iter())
        .zip(right.le.iter().chain(right.he.iter()))
        .find_map(|(left, right)| {
            let ordering = left.total_cmp(right);
            (ordering != std::cmp::Ordering::Equal).then_some(ordering)
        })
        .unwrap_or(std::cmp::Ordering::Equal)
}

fn validate_dataset(dataset: &AirportDataset) -> Result<(), AirportError> {
    if dataset.schema_version != SCHEMA_VERSION {
        return Err(AirportError::UnsupportedSchema(dataset.schema_version));
    }
    if dataset.source != SOURCE {
        return Err(AirportError::UnexpectedSource(dataset.source.clone()));
    }
    for airport in &dataset.airports {
        if airport.ident.trim().is_empty() {
            return Err(AirportError::InvalidCoordinate {
                field: "airport ident".to_owned(),
            });
        }
        validate_point(
            airport.latitude,
            airport.longitude,
            format!("airport {}", airport.ident),
        )?;
        for (index, runway) in airport.runways.iter().enumerate() {
            validate_point(
                runway.le[0],
                runway.le[1],
                format!("airport {} runway {index} low end", airport.ident),
            )?;
            validate_point(
                runway.he[0],
                runway.he[1],
                format!("airport {} runway {index} high end", airport.ident),
            )?;
        }
    }
    Ok(())
}

fn validate_point(latitude: f64, longitude: f64, field: String) -> Result<(), AirportError> {
    if !valid_latitude(latitude) || !valid_longitude(longitude) {
        return Err(AirportError::InvalidCoordinate { field });
    }
    Ok(())
}

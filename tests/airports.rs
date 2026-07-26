use std::io::{Read, Write};
use std::process::Command;

use flate2::Compression;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use planeradar::airports::{
    AirportError, airports_within, build_dataset, load_embedded, read_dataset, write_dataset,
};
use planeradar::model::{Airport, GeoPoint, Location};

const AIRPORTS_FIXTURE: &[u8] = include_bytes!("fixtures/ourairports/airports.csv");
const RUNWAYS_FIXTURE: &[u8] = include_bytes!("fixtures/ourairports/runways.csv");

fn build_fixture() -> planeradar::airports::AirportDataset {
    build_dataset(AIRPORTS_FIXTURE, RUNWAYS_FIXTURE).expect("dataset")
}

fn airport(ident: &str, latitude: f64, longitude: f64) -> Airport {
    Airport {
        ident: ident.to_owned(),
        location: GeoPoint {
            latitude,
            longitude,
        },
        runways: Vec::new(),
    }
}

fn gzip_json(json: &[u8]) -> Vec<u8> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(json).expect("gzip json");
    encoder.finish().expect("finish gzip")
}

#[test]
fn generator_filters_invalid_closed_incomplete_and_helipad_rows() {
    let dataset = build_fixture();

    assert_eq!(dataset.schema_version, 1);
    assert_eq!(dataset.source, "OurAirports");
    assert_eq!(dataset.airports.len(), 1);
    assert_eq!(dataset.airports[0].ident, "KJFK");
    assert_eq!(dataset.airports[0].latitude, 40.6394474);
    assert_eq!(dataset.airports[0].longitude, -73.7793174);
    assert_eq!(dataset.airports[0].runways.len(), 2);
    assert_eq!(dataset.airports[0].runways[0].le, [40.6400000, -73.8000000]);
    assert_eq!(dataset.airports[0].runways[1].le, [40.6486594, -73.7918704]);
}

#[test]
fn generator_sorts_airports_by_identifier_regardless_of_csv_order() {
    let airports = b"id,ident,type,name,latitude_deg,longitude_deg\n\
        1,ZZZZ,large_airport,Zulu,1,2\n\
        2,AAAA,large_airport,Alpha,3,4\n";
    let runways = b"id,airport_ref,airport_ident,length_ft,closed,le_ident,le_latitude_deg,le_longitude_deg,he_ident,he_latitude_deg,he_longitude_deg\n";

    let dataset = build_dataset(airports.as_slice(), runways.as_slice()).expect("dataset");

    assert_eq!(
        dataset
            .airports
            .iter()
            .map(|airport| airport.ident.as_str())
            .collect::<Vec<_>>(),
        ["AAAA", "ZZZZ"]
    );
}

#[test]
fn writer_is_byte_deterministic_and_sets_gzip_mtime_to_zero() {
    let dataset = build_fixture();
    let mut first = Vec::new();
    let mut second = Vec::new();

    write_dataset(&dataset, &mut first).expect("first write");
    write_dataset(&dataset, &mut second).expect("second write");

    assert_eq!(first, second);
    assert_eq!(&first[4..8], &[0, 0, 0, 0]);
    let mut decoded = String::new();
    GzDecoder::new(first.as_slice())
        .read_to_string(&mut decoded)
        .expect("decode");
    assert!(
        decoded.starts_with(
            r#"{"schema_version":1,"source":"OurAirports","airports":[{"ident":"KJFK""#
        )
    );
}

#[test]
fn generator_rejects_missing_required_headers() {
    let airports = b"id,ident,type,name,latitude_deg\n1,KJFK,large_airport,JFK,40.6\n";
    let error = build_dataset(airports.as_slice(), RUNWAYS_FIXTURE).expect_err("header error");

    assert!(matches!(error, AirportError::CsvSchema { .. }));
}

#[test]
fn generator_rejects_structurally_malformed_csv_instead_of_silently_skipping_it() {
    let malformed = b"id,ident,type,name,latitude_deg,longitude_deg\n\
        1,KJFK,large_airport,\"unterminated,40.6,-73.7\n";
    let error = build_dataset(malformed.as_slice(), RUNWAYS_FIXTURE).expect_err("CSV error");

    assert!(matches!(error, AirportError::Csv(_)));
}

#[test]
fn generator_skips_rows_with_invalid_types_nonfinite_or_out_of_bounds_coordinates() {
    let airports = b"id,ident,type,name,latitude_deg,longitude_deg\n\
        1,GOOD,large_airport,Good,1,2\n\
        2,NAN1,large_airport,NaN,NaN,2\n\
        3,INF1,large_airport,Infinity,inf,2\n\
        4,LAT1,large_airport,Latitude,91,2\n\
        5,LON1,large_airport,Longitude,1,-181\n";
    let runways = b"id,airport_ref,airport_ident,length_ft,closed,le_ident,le_latitude_deg,le_longitude_deg,he_ident,he_latitude_deg,he_longitude_deg\n";

    let dataset = build_dataset(airports.as_slice(), runways.as_slice()).expect("dataset");

    assert_eq!(dataset.airports.len(), 1);
    assert_eq!(dataset.airports[0].ident, "GOOD");
}

#[test]
fn loader_requires_the_versioned_source_schema_and_coordinate_bounds() {
    for json in [
        br#"{"schema_version":2,"source":"OurAirports","airports":[]}"#.as_slice(),
        br#"{"schema_version":1,"source":"Other","airports":[]}"#.as_slice(),
        br#"{"schema_version":"1","source":"OurAirports","airports":[]}"#.as_slice(),
        br#"{"schema_version":1,"source":"OurAirports","airports":[{"ident":"BAD","latitude":91,"longitude":0,"runways":[]}]}"#.as_slice(),
        br#"{"schema_version":1,"source":"OurAirports","airports":[{"ident":"BAD","latitude":0,"longitude":0,"runways":[{"le":[0,181],"he":[0,0]}]}]}"#.as_slice(),
    ] {
        let error = read_dataset(gzip_json(json).as_slice()).expect_err("invalid dataset");
        assert!(
            matches!(
                error,
                AirportError::UnsupportedSchema(_)
                    | AirportError::UnexpectedSource(_)
                    | AirportError::Json(_)
                    | AirportError::InvalidCoordinate { .. }
            ),
            "unexpected error: {error:?}"
        );
    }
}

#[test]
fn loader_converts_the_compact_schema_to_domain_models() {
    let dataset = build_fixture();
    let mut encoded = Vec::new();
    write_dataset(&dataset, &mut encoded).expect("write");

    let airports = read_dataset(encoded.as_slice()).expect("read");

    assert_eq!(airports.len(), 1);
    assert_eq!(airports[0].ident, "KJFK");
    assert_eq!(airports[0].location.latitude, 40.6394474);
    assert_eq!(airports[0].runways.len(), 2);
    assert_eq!(airports[0].runways[0].low_end.longitude, -73.8);
}

#[test]
fn embedded_production_dataset_is_valid_and_nonempty() {
    let airports = load_embedded().expect("embedded dataset");

    assert!(!airports.is_empty());
    assert!(airports.iter().any(|airport| !airport.runways.is_empty()));
}

#[test]
fn loader_rejects_decompressed_payloads_over_the_safety_limit() {
    let oversized = vec![b' '; 8 * 1024 * 1024 + 1];
    let error = read_dataset(gzip_json(&oversized).as_slice()).expect_err("size error");

    assert!(matches!(error, AirportError::DatasetTooLarge));
}

#[test]
fn radius_filter_rejects_invalid_radius_and_honours_zero_maximum() {
    let airports = [airport("HERE", 0.0, 0.0)];
    let origin = Location {
        latitude: 0.0,
        longitude: 0.0,
        label: String::new(),
    };

    for radius in [0.0, -1.0, f64::NAN, f64::INFINITY] {
        assert!(airports_within(&airports, &origin, radius, 1).is_empty());
    }
    assert!(airports_within(&airports, &origin, 1.0, 0).is_empty());
}

#[test]
fn radius_filter_orders_by_distance_then_identifier_deterministically() {
    let airports = [
        airport("BETA", 0.0, 0.01),
        airport("FAR", 0.0, 0.1),
        airport("ALPHA", 0.0, -0.01),
        airport("HERE", 0.0, 0.0),
    ];
    let origin = Location {
        latitude: 0.0,
        longitude: 0.0,
        label: String::new(),
    };

    let nearby = airports_within(&airports, &origin, 2.0, 3);

    assert_eq!(
        nearby
            .iter()
            .map(|airport| airport.ident.as_str())
            .collect::<Vec<_>>(),
        ["HERE", "ALPHA", "BETA"]
    );
}

#[test]
fn radius_filter_rejects_nonfinite_origins_and_skips_invalid_airports() {
    let airports = [
        airport("VALID", 0.0, 0.0),
        airport("INVALID", f64::NAN, 0.0),
    ];
    let invalid_origin = Location {
        latitude: f64::INFINITY,
        longitude: 0.0,
        label: String::new(),
    };
    let valid_origin = Location {
        latitude: 0.0,
        longitude: 0.0,
        label: String::new(),
    };

    assert!(airports_within(&airports, &invalid_origin, 1.0, 10).is_empty());
    assert_eq!(
        airports_within(&airports, &valid_origin, 1.0, 10)
            .iter()
            .map(|airport| airport.ident.as_str())
            .collect::<Vec<_>>(),
        ["VALID"]
    );
}

#[test]
fn build_airports_binary_writes_a_loadable_dataset_and_reports_counts() {
    let directory = tempfile::tempdir().expect("tempdir");
    let output_path = directory.path().join("airports.json.gz");
    let output = Command::new(env!("CARGO_BIN_EXE_build-airports"))
        .arg("tests/fixtures/ourairports/airports.csv")
        .arg("tests/fixtures/ourairports/runways.csv")
        .arg(&output_path)
        .output()
        .expect("run build-airports");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).expect("UTF-8"),
        "wrote 1 airports and 2 runways\n"
    );
    let airports =
        read_dataset(std::fs::File::open(output_path).expect("open output")).expect("load output");
    assert_eq!(airports.len(), 1);
    assert_eq!(airports[0].runways.len(), 2);
}

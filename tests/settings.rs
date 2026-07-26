use std::fs;
use std::os::unix::fs::PermissionsExt;

use planeradar::model::{Location, RadarSettings, Units};
use planeradar::settings::{SettingsStore, validate_settings};
use serde_json::{Value, json};

fn configured_settings() -> RadarSettings {
    RadarSettings {
        schema_version: 1,
        location: Some(Location {
            latitude: 40.7128,
            longitude: -74.0060,
            label: "New York, NY".to_owned(),
        }),
        units: Units::Kilometres,
        show_runways: true,
        range_index: 1,
    }
}

fn valid_json() -> Value {
    json!({
        "schema_version": 1,
        "location": {
            "latitude": 40.7128,
            "longitude": -74.0060,
            "label": "New York, NY"
        },
        "units": "km",
        "show_runways": true,
        "range_index": 1
    })
}

#[test]
fn defaults_require_location_setup() {
    let settings = RadarSettings::default();

    assert_eq!(settings.schema_version, 1);
    assert_eq!(settings.location, None);
    assert_eq!(settings.units, Units::Kilometres);
    assert!(settings.show_runways);
    assert_eq!(settings.range_index, 1);
}

#[test]
fn missing_settings_file_loads_defaults() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let store = SettingsStore::new(directory.path().join("settings.json"));

    assert_eq!(
        store.load().expect("missing settings load"),
        RadarSettings::default()
    );
}

#[test]
fn settings_round_trip_atomically() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let store = SettingsStore::new(directory.path().join("settings.json"));
    let expected = configured_settings();

    store.save(&expected).expect("save");

    assert_eq!(store.load().expect("load"), expected);
}

#[test]
fn save_writes_deterministic_pretty_json() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let path = directory.path().join("settings.json");
    let store = SettingsStore::new(path.clone());

    store.save(&configured_settings()).expect("save");

    assert_eq!(
        fs::read_to_string(path).expect("saved JSON"),
        concat!(
            "{\n",
            "  \"schema_version\": 1,\n",
            "  \"location\": {\n",
            "    \"latitude\": 40.7128,\n",
            "    \"longitude\": -74.006,\n",
            "    \"label\": \"New York, NY\"\n",
            "  },\n",
            "  \"units\": \"km\",\n",
            "  \"show_runways\": true,\n",
            "  \"range_index\": 1\n",
            "}\n",
        )
    );
}

#[test]
fn validation_rejects_each_invalid_document_shape() {
    let mut out_of_range_latitude = valid_json();
    out_of_range_latitude["location"]["latitude"] = json!(90.000_001);

    let mut out_of_range_longitude = valid_json();
    out_of_range_longitude["location"]["longitude"] = json!(-180.000_001);

    let mut non_numeric_latitude = valid_json();
    non_numeric_latitude["location"]["latitude"] = json!("north");

    let mut unsupported_schema = valid_json();
    unsupported_schema["schema_version"] = json!(2);

    let mut unsupported_units = valid_json();
    unsupported_units["units"] = json!("meters");

    let mut out_of_range_index = valid_json();
    out_of_range_index["range_index"] = json!(4);

    let mut unknown_top_level_key = valid_json();
    unknown_top_level_key["unexpected"] = json!(true);

    let mut missing_required_field = valid_json();
    missing_required_field
        .as_object_mut()
        .expect("valid JSON object")
        .remove("show_runways");

    for (name, value) in [
        ("latitude outside ±90", out_of_range_latitude),
        ("longitude outside ±180", out_of_range_longitude),
        ("non-number coordinate", non_numeric_latitude),
        ("unsupported schema", unsupported_schema),
        ("unsupported units", unsupported_units),
        ("range index outside 0-3", out_of_range_index),
        ("unknown top-level key", unknown_top_level_key),
        ("missing required field", missing_required_field),
    ] {
        assert!(validate_settings(value).is_err(), "{name} must be rejected");
    }
}

#[test]
fn save_rejects_programmatic_non_finite_coordinates() {
    for (name, coordinate) in [
        ("NaN", f64::NAN),
        ("positive infinity", f64::INFINITY),
        ("negative infinity", f64::NEG_INFINITY),
    ] {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("settings.json");
        let store = SettingsStore::new(path.clone());
        let mut invalid = configured_settings();
        invalid
            .location
            .as_mut()
            .expect("configured location")
            .latitude = coordinate;

        assert!(store.save(&invalid).is_err(), "{name} must be rejected");
        assert!(!path.exists(), "{name} must not create settings");
    }
}

#[test]
fn invalid_existing_file_returns_error_without_replacement() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let path = directory.path().join("settings.json");
    let malformed = b"{ not valid JSON";
    fs::write(&path, malformed).expect("write malformed fixture");
    let store = SettingsStore::new(path.clone());

    assert!(store.load().is_err());
    assert_eq!(fs::read(&path).expect("read malformed fixture"), malformed);
}

#[test]
fn malformed_existing_file_returns_error() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let path = directory.path().join("settings.json");
    fs::write(&path, "{\n").expect("write malformed JSON");

    assert!(SettingsStore::new(path).load().is_err());
}

#[test]
fn save_creates_missing_parent_with_private_group_readable_mode() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let parent = directory.path().join("state").join("nested");
    let store = SettingsStore::new(parent.join("settings.json"));

    store.save(&configured_settings()).expect("save");

    assert_eq!(
        fs::metadata(parent)
            .expect("settings parent metadata")
            .permissions()
            .mode()
            & 0o777,
        0o750
    );
}

#[test]
fn rename_failure_preserves_directory_destination_and_existing_valid_settings() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let valid_path = directory.path().join("previous-settings.json");
    let valid_store = SettingsStore::new(valid_path.clone());
    let expected = configured_settings();
    valid_store.save(&expected).expect("save valid settings");
    let valid_bytes = fs::read(&valid_path).expect("read valid settings");

    let destination = directory.path().join("settings.json");
    fs::create_dir(&destination).expect("directory destination");
    fs::write(destination.join("keep"), "do not replace").expect("directory fixture");
    let blocked_store = SettingsStore::new(destination.clone());

    assert!(blocked_store.save(&configured_settings()).is_err());
    assert!(
        destination.is_dir(),
        "failed rename must retain destination directory"
    );
    assert_eq!(
        fs::read(destination.join("keep")).expect("destination fixture"),
        b"do not replace"
    );
    assert_eq!(
        fs::read(&valid_path).expect("read valid settings after failure"),
        valid_bytes
    );
    assert_eq!(valid_store.load().expect("load valid settings"), expected);
}

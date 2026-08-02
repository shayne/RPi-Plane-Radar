use std::fs;
use std::os::unix::fs::PermissionsExt;

use planeradar::model::{
    FooterSettings, Location, RadarSettings, SETTINGS_SCHEMA_VERSION, TimeZone, Units,
};
use planeradar::settings::{SettingsStore, validate_settings};
use serde_json::{Value, json};

fn configured_settings() -> RadarSettings {
    RadarSettings {
        location: Some(Location {
            latitude: 40.7128,
            longitude: -74.0060,
            label: "New York, NY".to_owned(),
        }),
        units: Units::Kilometres,
        show_runways: true,
        range_index: 1,
        ..RadarSettings::default()
    }
}

fn valid_json() -> Value {
    json!({
        "schema_version": 2,
        "location": {
            "latitude": 40.7128,
            "longitude": -74.0060,
            "label": "New York, NY"
        },
        "units": "km",
        "show_runways": true,
        "range_index": 1,
        "show_callsign": true,
        "show_route": false,
        "show_expanded_model": false,
        "radar_text_scale_percent": 100,
        "minimum_altitude_feet": null,
        "maximum_altitude_feet": null,
        "footer": {
            "show_condition": false,
            "show_temperature": false,
            "show_humidity": false,
            "show_time": false,
            "show_date": false,
            "temperature_unit": "celsius",
            "time_zone": "radar_local",
            "clock_format": "twenty_four"
        }
    })
}

fn assert_compatibility_defaults(settings: &RadarSettings) {
    assert_eq!(settings.schema_version, SETTINGS_SCHEMA_VERSION);
    assert!(settings.show_callsign);
    assert!(!settings.show_route);
    assert!(!settings.show_expanded_model);
    assert_eq!(settings.radar_text_scale_percent, 100);
    assert_eq!(settings.minimum_altitude_feet, None);
    assert_eq!(settings.maximum_altitude_feet, None);
    assert_eq!(settings.footer, FooterSettings::default());
}

#[test]
fn defaults_require_location_setup() {
    let settings = RadarSettings::default();

    assert_compatibility_defaults(&settings);
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
            "  \"schema_version\": 2,\n",
            "  \"location\": {\n",
            "    \"latitude\": 40.7128,\n",
            "    \"longitude\": -74.006,\n",
            "    \"label\": \"New York, NY\"\n",
            "  },\n",
            "  \"units\": \"km\",\n",
            "  \"show_runways\": true,\n",
            "  \"range_index\": 1,\n",
            "  \"show_callsign\": true,\n",
            "  \"show_route\": false,\n",
            "  \"show_expanded_model\": false,\n",
            "  \"radar_text_scale_percent\": 100,\n",
            "  \"minimum_altitude_feet\": null,\n",
            "  \"maximum_altitude_feet\": null,\n",
            "  \"footer\": {\n",
            "    \"show_condition\": false,\n",
            "    \"show_temperature\": false,\n",
            "    \"show_humidity\": false,\n",
            "    \"show_time\": false,\n",
            "    \"show_date\": false,\n",
            "    \"temperature_unit\": \"celsius\",\n",
            "    \"time_zone\": \"radar_local\",\n",
            "    \"clock_format\": \"twenty_four\"\n",
            "  }\n",
            "}\n",
        )
    );
}

#[test]
fn version_one_document_migrates_to_compatibility_defaults() {
    let migrated =
        validate_settings(serde_json::from_str(include_str!("fixtures/settings/v1.json")).unwrap())
            .expect("v1 migration");

    assert_eq!(migrated.units, Units::Miles);
    assert!(!migrated.show_runways);
    assert_eq!(migrated.range_index, 3);
    assert_compatibility_defaults(&migrated);
}

#[test]
fn loading_version_one_does_not_write_until_the_next_mutation() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let path = directory.path().join("settings.json");
    let legacy = include_bytes!("fixtures/settings/v1.json");
    fs::write(&path, legacy).expect("write v1 fixture");
    let store = SettingsStore::new(path.clone());

    let migrated = store.load().expect("load v1 settings");

    assert_eq!(fs::read(&path).expect("unchanged v1 bytes"), legacy);
    assert_compatibility_defaults(&migrated);

    store.save(&migrated).expect("save migrated settings");
    let persisted_value: Value =
        serde_json::from_slice(&fs::read(&path).expect("persisted v2 bytes")).expect("v2 JSON");
    assert_eq!(persisted_value["schema_version"], SETTINGS_SCHEMA_VERSION);
    let persisted = validate_settings(persisted_value).expect("persisted v2 settings");
    assert_compatibility_defaults(&persisted);
}

#[test]
fn footer_visibility_reports_any_enabled_item() {
    let mut footer = FooterSettings::default();
    assert!(!footer.any_visible());

    footer.show_time = true;
    assert!(footer.any_visible());
}

#[test]
fn footer_environment_need_honors_radar_local_and_zulu_time() {
    let mut footer = FooterSettings::default();
    assert!(!footer.needs_environment());

    footer.show_time = true;
    assert!(footer.needs_environment());

    footer.time_zone = TimeZone::Zulu;
    assert!(!footer.needs_environment());

    footer.show_condition = true;
    assert!(footer.needs_environment());
}

#[test]
fn altitude_filter_is_active_when_either_bound_is_present() {
    let mut settings = RadarSettings::default();
    assert!(!settings.altitude_filter_active());
    settings.minimum_altitude_feet = Some(0);
    assert!(settings.altitude_filter_active());
    settings.minimum_altitude_feet = None;
    settings.maximum_altitude_feet = Some(10_000);
    assert!(settings.altitude_filter_active());
}

#[test]
fn validation_rejects_each_invalid_document_shape() {
    let mut out_of_range_latitude = valid_json();
    out_of_range_latitude["location"]["latitude"] = json!(90.000_001);

    let mut out_of_range_longitude = valid_json();
    out_of_range_longitude["location"]["longitude"] = json!(-180.000_001);

    let mut non_numeric_latitude = valid_json();
    non_numeric_latitude["location"]["latitude"] = json!("north");

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
        ("unsupported units", unsupported_units),
        ("range index outside 0-3", out_of_range_index),
        ("unknown top-level key", unknown_top_level_key),
        ("missing required field", missing_required_field),
    ] {
        assert!(validate_settings(value).is_err(), "{name} must be rejected");
    }
}

#[test]
fn validation_rejects_invalid_v2_display_options_and_schema_versions() {
    let mut cases = Vec::new();
    for scale in [79, 81, 140] {
        let mut value = valid_json();
        value["radar_text_scale_percent"] = json!(scale);
        cases.push((format!("text scale {scale}"), value));
    }
    for schema_version in [0, 3] {
        let mut value = valid_json();
        value["schema_version"] = json!(schema_version);
        cases.push((format!("schema version {schema_version}"), value));
    }

    let mut minimum_too_low = valid_json();
    minimum_too_low["minimum_altitude_feet"] = json!(-2001);
    cases.push(("minimum below -2000".to_owned(), minimum_too_low));

    let mut maximum_too_high = valid_json();
    maximum_too_high["maximum_altitude_feet"] = json!(100001);
    cases.push(("maximum above 100000".to_owned(), maximum_too_high));

    let mut reversed_bounds = valid_json();
    reversed_bounds["minimum_altitude_feet"] = json!(45_001);
    reversed_bounds["maximum_altitude_feet"] = json!(45_000);
    cases.push(("minimum above maximum".to_owned(), reversed_bounds));

    let mut unknown_footer_field = valid_json();
    unknown_footer_field["footer"]["unexpected"] = json!(true);
    cases.push(("unknown footer field".to_owned(), unknown_footer_field));

    for (name, value) in cases {
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

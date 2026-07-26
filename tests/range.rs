use planeradar::model::Units;
use planeradar::range::{RANGE_RING3_KM, format_range_label, next_range_index, range_preset};

#[test]
fn cycles_all_upstream_ranges() {
    assert_eq!([0, 1, 2, 3].map(next_range_index), [1, 2, 3, 0]);
    assert_eq!(RANGE_RING3_KM, [5.0, 10.0, 15.0, 25.0]);
    assert_eq!(range_preset(1).expect("range").ring3_km, 10.0);
    assert!((range_preset(1).expect("range").outer_km - 13.333_333).abs() < 1e-6);
}

#[test]
fn formats_ring_three_labels_in_selected_units() {
    let preset = range_preset(1).expect("ten kilometre range");

    assert_eq!(format_range_label(preset, Units::Kilometres), "10km");
    assert_eq!(format_range_label(preset, Units::Miles), "6mi");
}

#[test]
fn converts_all_upstream_ring_labels_to_nearest_mile() {
    let labels = [0, 1, 2, 3].map(|index| {
        format_range_label(range_preset(index).expect("upstream range"), Units::Miles)
    });

    assert_eq!(labels, ["3mi", "6mi", "9mi", "16mi"]);
}

#[test]
fn rejects_unknown_range_index() {
    assert!(range_preset(4).is_err());
}

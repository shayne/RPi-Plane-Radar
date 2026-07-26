use planeradar::geometry::{offset_km, project_to_radar, rim_point};
use planeradar::model::Location;

const OUTER_KM: f64 = 10.0;
const GRID_RADIUS_PX: f64 = 214.0;
const AIRCRAFT_SAFE_RADIUS_PX: f64 = 190.0;

fn location(latitude: f64, longitude: f64) -> Location {
    Location {
        latitude,
        longitude,
        label: String::new(),
    }
}

fn latitude_delta_for(kilometres: f64) -> f64 {
    kilometres / 6_371.008_8 * 180.0 / std::f64::consts::PI
}

fn longitude_delta_for(kilometres: f64, mean_latitude_degrees: f64) -> f64 {
    kilometres / (6_371.008_8 * mean_latitude_degrees.to_radians().cos()) * 180.0
        / std::f64::consts::PI
}

#[test]
fn projects_north_up_and_east_right_at_new_york() {
    let origin = location(40.7128, -74.0060);
    let north = project_to_radar(
        &origin,
        origin.latitude + latitude_delta_for(5.0),
        origin.longitude,
        OUTER_KM,
        GRID_RADIUS_PX,
        AIRCRAFT_SAFE_RADIUS_PX,
    )
    .expect("north projection");
    let east = project_to_radar(
        &origin,
        origin.latitude,
        origin.longitude + longitude_delta_for(5.0, origin.latitude),
        OUTER_KM,
        GRID_RADIUS_PX,
        AIRCRAFT_SAFE_RADIUS_PX,
    )
    .expect("east projection");

    assert!(north.y < 240, "north must move up the screen");
    assert_eq!(north.x, 240);
    assert!(east.x > 240, "east must move right on the screen");
    assert_eq!(east.y, 240);
    assert!((pixel_radius(north.x, north.y) - pixel_radius(east.x, east.y)).abs() <= 0.5);
}

#[test]
fn projects_equal_physical_distances_equally_at_seventy_degrees_latitude() {
    let origin = location(70.0, 20.0);
    let north = project_to_radar(
        &origin,
        origin.latitude + latitude_delta_for(5.0),
        origin.longitude,
        OUTER_KM,
        GRID_RADIUS_PX,
        AIRCRAFT_SAFE_RADIUS_PX,
    )
    .expect("north projection");
    let east = project_to_radar(
        &origin,
        origin.latitude,
        origin.longitude + longitude_delta_for(5.0, origin.latitude),
        OUTER_KM,
        GRID_RADIUS_PX,
        AIRCRAFT_SAFE_RADIUS_PX,
    )
    .expect("east projection");

    assert!((pixel_radius(north.x, north.y) - pixel_radius(east.x, east.y)).abs() <= 0.5);
}

#[test]
fn east_west_offset_uses_cosine_of_mean_latitude() {
    let origin = location(70.0, 20.0);
    let target_latitude = 70.2_f64;
    let target_longitude = origin.longitude + 0.1_f64;
    let offset = offset_km(&origin, target_latitude, target_longitude);
    let expected_east = 6_371.008_8
        * 0.1_f64.to_radians()
        * ((origin.latitude.to_radians() + target_latitude.to_radians()) / 2.0).cos();

    assert!((offset.east - expected_east).abs() < 1e-12);
}

#[test]
fn places_cardinal_bearings_on_the_requested_rim() {
    assert_eq!(rim_point(0.0, 1.0, 238.0), (240, 2));
    assert_eq!(rim_point(1.0, 0.0, 238.0), (478, 240));
    assert_eq!(rim_point(0.0, -1.0, 238.0), (240, 478));
    assert_eq!(rim_point(-1.0, 0.0, 238.0), (2, 240));
}

#[test]
fn rejects_nonpositive_or_nonfinite_rim_radius() {
    for radius_px in [0.0, -1.0, f64::NAN, f64::INFINITY] {
        assert_eq!(rim_point(1.0, 0.0, radius_px), (240, 240));
    }
}

#[test]
fn aircraft_safe_radius_controls_inside_ring_without_changing_projection_scale() {
    let origin = location(40.7128, -74.0060);
    let target_latitude = origin.latitude + latitude_delta_for(9.0);
    let inside = project_to_radar(
        &origin,
        target_latitude,
        origin.longitude,
        OUTER_KM,
        GRID_RADIUS_PX,
        200.0,
    )
    .expect("inside projection");
    let outside = project_to_radar(
        &origin,
        target_latitude,
        origin.longitude,
        OUTER_KM,
        GRID_RADIUS_PX,
        190.0,
    )
    .expect("outside projection");

    assert_eq!((inside.x, inside.y), (outside.x, outside.y));
    assert!(inside.inside_ring);
    assert!(!outside.inside_ring);
}

#[test]
fn rejects_nonpositive_or_nonfinite_projection_scale_inputs() {
    let origin = location(40.7128, -74.0060);

    for (outer_km, grid_radius_px, aircraft_safe_radius_px) in [
        (0.0, GRID_RADIUS_PX, AIRCRAFT_SAFE_RADIUS_PX),
        (-1.0, GRID_RADIUS_PX, AIRCRAFT_SAFE_RADIUS_PX),
        (f64::NAN, GRID_RADIUS_PX, AIRCRAFT_SAFE_RADIUS_PX),
        (OUTER_KM, 0.0, AIRCRAFT_SAFE_RADIUS_PX),
        (OUTER_KM, -1.0, AIRCRAFT_SAFE_RADIUS_PX),
        (OUTER_KM, f64::INFINITY, AIRCRAFT_SAFE_RADIUS_PX),
        (OUTER_KM, GRID_RADIUS_PX, 0.0),
        (OUTER_KM, GRID_RADIUS_PX, -1.0),
        (OUTER_KM, GRID_RADIUS_PX, f64::NEG_INFINITY),
    ] {
        assert!(
            project_to_radar(
                &origin,
                origin.latitude,
                origin.longitude,
                outer_km,
                grid_radius_px,
                aircraft_safe_radius_px,
            )
            .is_err(),
            "invalid inputs ({outer_km}, {grid_radius_px}, {aircraft_safe_radius_px}) must fail"
        );
    }
}

fn pixel_radius(x: i32, y: i32) -> f64 {
    f64::hypot(f64::from(x - 240), f64::from(y - 240))
}

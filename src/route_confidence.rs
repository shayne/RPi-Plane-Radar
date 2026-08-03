//! Position-aware confidence for static ADSBDB route candidates.

use std::f64::consts::{PI, TAU};

use crate::geometry::EARTH_RADIUS_KM;
use crate::model::{Aircraft, GeoPoint};

const CORRIDOR_FRACTION: f64 = 0.20;
const MIN_CORRIDOR_KM: f64 = 200.0;
const MAX_CORRIDOR_KM: f64 = 500.0;
const MIN_SEGMENT_KM: f64 = 1.0;
const ANTIPODAL_EPSILON_RADIANS: f64 = 0.000_001;

#[derive(Clone, Debug, PartialEq)]
pub struct RouteCandidate {
    label: String,
    points: Box<[GeoPoint]>,
}

impl RouteCandidate {
    pub fn new(label: String, points: Vec<GeoPoint>) -> Option<Self> {
        if label.is_empty()
            || !(2..=3).contains(&points.len())
            || !points.iter().all(valid_point)
            || points.windows(2).any(|segment| {
                let Some(angle) = angular_distance(&segment[0], &segment[1]) else {
                    return true;
                };
                angle * EARTH_RADIUS_KM < MIN_SEGMENT_KM
                    || (PI - angle).abs() <= ANTIPODAL_EPSILON_RADIANS
            })
        {
            return None;
        }
        Some(Self {
            label,
            points: points.into_boxed_slice(),
        })
    }

    pub fn label_for(&self, aircraft: &Aircraft) -> Option<&str> {
        let live = GeoPoint {
            latitude: aircraft.latitude,
            longitude: aircraft.longitude,
        };
        if !valid_point(&live) {
            return None;
        }
        self.points
            .windows(2)
            .any(|segment| {
                let Some(length_radians) = angular_distance(&segment[0], &segment[1]) else {
                    return false;
                };
                let corridor = corridor_width_km(length_radians * EARTH_RADIUS_KM);
                distance_to_segment_km(&live, &segment[0], &segment[1])
                    .is_some_and(|distance| distance <= corridor)
            })
            .then_some(self.label.as_str())
    }
}

fn valid_point(point: &GeoPoint) -> bool {
    point.latitude.is_finite()
        && point.longitude.is_finite()
        && (-90.0..=90.0).contains(&point.latitude)
        && (-180.0..=180.0).contains(&point.longitude)
}

fn angular_distance(a: &GeoPoint, b: &GeoPoint) -> Option<f64> {
    let lat_a = a.latitude.to_radians();
    let lat_b = b.latitude.to_radians();
    let delta_lat = lat_b - lat_a;
    let delta_lon = normalize_radians(b.longitude.to_radians() - a.longitude.to_radians());
    let haversine = (delta_lat / 2.0).sin().powi(2)
        + lat_a.cos() * lat_b.cos() * (delta_lon / 2.0).sin().powi(2);
    let angle = 2.0 * haversine.clamp(0.0, 1.0).sqrt().asin();
    angle.is_finite().then_some(angle)
}

fn initial_bearing(a: &GeoPoint, b: &GeoPoint) -> Option<f64> {
    let lat_a = a.latitude.to_radians();
    let lat_b = b.latitude.to_radians();
    let delta_lon = normalize_radians(b.longitude.to_radians() - a.longitude.to_radians());
    let y = delta_lon.sin() * lat_b.cos();
    let x = lat_a.cos() * lat_b.sin() - lat_a.sin() * lat_b.cos() * delta_lon.cos();
    let bearing = y.atan2(x);
    bearing.is_finite().then_some(bearing)
}

fn distance_to_segment_km(point: &GeoPoint, start: &GeoPoint, end: &GeoPoint) -> Option<f64> {
    let segment_angle = angular_distance(start, end)?;
    let point_angle = angular_distance(start, point)?;
    let segment_bearing = initial_bearing(start, end)?;
    let point_bearing = initial_bearing(start, point)?;
    let bearing_delta = point_bearing - segment_bearing;
    let cross_track = (point_angle.sin() * bearing_delta.sin())
        .clamp(-1.0, 1.0)
        .asin();
    let along_track = (point_angle.sin() * bearing_delta.cos()).atan2(point_angle.cos());
    let distance = if (0.0..=segment_angle).contains(&along_track) {
        cross_track.abs() * EARTH_RADIUS_KM
    } else {
        angular_distance(point, start)?.min(angular_distance(point, end)?) * EARTH_RADIUS_KM
    };
    distance.is_finite().then_some(distance)
}

fn normalize_radians(value: f64) -> f64 {
    (value + PI).rem_euclid(TAU) - PI
}

fn corridor_width_km(segment_length_km: f64) -> f64 {
    (segment_length_km * CORRIDOR_FRACTION).clamp(MIN_CORRIDOR_KM, MAX_CORRIDOR_KM)
}

#[cfg(test)]
mod tests {
    use super::corridor_width_km;

    #[test]
    fn corridor_width_uses_the_exact_minimum_and_maximum() {
        assert_eq!(corridor_width_km(500.0), 200.0);
        assert_eq!(corridor_width_km(1_500.0), 300.0);
        assert_eq!(corridor_width_km(5_000.0), 500.0);
    }
}

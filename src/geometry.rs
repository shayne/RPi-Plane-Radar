use thiserror::Error;

use crate::model::Location;

pub const EARTH_RADIUS_KM: f64 = 6_371.008_8;
const RADAR_CENTER_PX: f64 = 240.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OffsetKm {
    pub east: f64,
    pub north: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ProjectedPoint {
    pub x: i32,
    pub y: i32,
    pub inside_ring: bool,
    pub offset: OffsetKm,
}

#[derive(Debug, Error, PartialEq)]
pub enum GeometryError {
    #[error("coordinates must be finite")]
    NonFiniteCoordinates,
    #[error("outer distance must be positive and finite")]
    InvalidOuterDistance,
    #[error("grid radius must be positive and finite")]
    InvalidGridRadius,
    #[error("aircraft-safe radius must be positive and finite")]
    InvalidAircraftSafeRadius,
}

pub fn offset_km(origin: &Location, latitude: f64, longitude: f64) -> OffsetKm {
    let lat0 = origin.latitude.to_radians();
    let lat1 = latitude.to_radians();
    let dlon = normalize_longitude_delta_degrees(longitude - origin.longitude).to_radians();
    OffsetKm {
        east: EARTH_RADIUS_KM * dlon * ((lat0 + lat1) / 2.0).cos(),
        north: EARTH_RADIUS_KM * (lat1 - lat0),
    }
}

/// Normalizes to [-180, 180); an exact 180-degree separation is westbound.
fn normalize_longitude_delta_degrees(delta: f64) -> f64 {
    (delta + 180.0).rem_euclid(360.0) - 180.0
}

pub fn project_to_radar(
    origin: &Location,
    latitude: f64,
    longitude: f64,
    outer_km: f64,
    grid_radius_px: f64,
    aircraft_safe_radius_px: f64,
) -> Result<ProjectedPoint, GeometryError> {
    validate_projection_inputs(
        origin,
        latitude,
        longitude,
        outer_km,
        grid_radius_px,
        aircraft_safe_radius_px,
    )?;

    let offset = offset_km(origin, latitude, longitude);
    let pixels_per_kilometre = grid_radius_px / outer_km;
    let x = RADAR_CENTER_PX + offset.east * pixels_per_kilometre;
    let y = RADAR_CENTER_PX - offset.north * pixels_per_kilometre;
    let distance_px = f64::hypot(x - RADAR_CENTER_PX, y - RADAR_CENTER_PX);

    Ok(ProjectedPoint {
        x: x.round() as i32,
        y: y.round() as i32,
        inside_ring: distance_px <= aircraft_safe_radius_px,
        offset,
    })
}

pub fn rim_point(dx_km: f64, dy_km: f64, radius_px: f64) -> (i32, i32) {
    let distance_km = f64::hypot(dx_km, dy_km);
    if !distance_km.is_finite() || distance_km == 0.0 || !radius_px.is_finite() || radius_px <= 0.0
    {
        return (RADAR_CENTER_PX as i32, RADAR_CENTER_PX as i32);
    }

    (
        (RADAR_CENTER_PX + dx_km / distance_km * radius_px).round() as i32,
        (RADAR_CENTER_PX - dy_km / distance_km * radius_px).round() as i32,
    )
}

fn validate_projection_inputs(
    origin: &Location,
    latitude: f64,
    longitude: f64,
    outer_km: f64,
    grid_radius_px: f64,
    aircraft_safe_radius_px: f64,
) -> Result<(), GeometryError> {
    if !origin.latitude.is_finite()
        || !origin.longitude.is_finite()
        || !latitude.is_finite()
        || !longitude.is_finite()
    {
        return Err(GeometryError::NonFiniteCoordinates);
    }
    if !outer_km.is_finite() || outer_km <= 0.0 {
        return Err(GeometryError::InvalidOuterDistance);
    }
    if !grid_radius_px.is_finite() || grid_radius_px <= 0.0 {
        return Err(GeometryError::InvalidGridRadius);
    }
    if !aircraft_safe_radius_px.is_finite() || aircraft_safe_radius_px <= 0.0 {
        return Err(GeometryError::InvalidAircraftSafeRadius);
    }
    Ok(())
}

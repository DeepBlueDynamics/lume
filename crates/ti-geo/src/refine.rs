use crate::validate_point;
use ti_contracts::{Error, Result};

/// Oracle sphere radius, in nautical miles.
pub const EARTH_RADIUS_NM: f64 = 3440.065;

/// Longitude bounds wrap across the antimeridian when min > max.
/// Latitude and longitude endpoints are inclusive.
#[derive(Debug, Clone, Copy)]
pub struct Bbox {
    pub lat_min: f64,
    pub lon_min: f64,
    pub lat_max: f64,
    pub lon_max: f64,
}
impl Bbox {
    pub fn new(lat_min: f64, lon_min: f64, lat_max: f64, lon_max: f64) -> Result<Self> {
        validate_point(lat_min, lon_min)?;
        validate_point(lat_max, lon_max)?;
        if lat_min > lat_max {
            return Err(Error::InvalidInput("bbox latitude bounds are reversed".into()));
        }
        Ok(Self { lat_min, lon_min, lat_max, lon_max })
    }
    pub fn contains(&self, lat: f64, lon: f64) -> bool {
        if validate_point(lat, lon).is_err() || lat < self.lat_min || lat > self.lat_max {
            return false;
        }
        let inside = |lon| if self.lon_min <= self.lon_max {
            lon >= self.lon_min && lon <= self.lon_max
        } else {
            lon >= self.lon_min || lon <= self.lon_max
        };
        inside(lon) || (lon.abs() == 180.0 && inside(-lon))
    }
    pub fn longitude_spans(&self) -> Vec<(f64, f64)> {
        if self.lon_min <= self.lon_max {
            vec![(self.lon_min, self.lon_max)]
        } else {
            vec![(self.lon_min, 180.0), (-180.0, self.lon_max)]
        }
    }
    pub fn area_km2(&self) -> f64 {
        let longitude = self.longitude_spans().iter().map(|(lo, hi)| hi - lo).sum::<f64>().to_radians();
        (EARTH_RADIUS_NM * 1.852).powi(2) * longitude
            * (self.lat_max.to_radians().sin() - self.lat_min.to_radians().sin()).abs()
    }
}

/// Stable at the antimeridian and near antipodal points.
pub fn haversine_nm(lat: f64, lon: f64, other_lat: f64, other_lon: f64) -> Result<f64> {
    validate_point(lat, lon)?;
    validate_point(other_lat, other_lon)?;
    let delta_lat = (other_lat - lat).to_radians();
    let delta_lon = (other_lon - lon).to_radians();
    let a = (delta_lat / 2.0).sin().powi(2)
        + lat.to_radians().cos() * other_lat.to_radians().cos() * (delta_lon / 2.0).sin().powi(2);
    Ok(2.0 * EARTH_RADIUS_NM * a.clamp(0.0, 1.0).sqrt().asin())
}

pub fn within_nm(lat: f64, lon: f64, center_lat: f64, center_lon: f64, radius_nm: f64) -> Result<bool> {
    validate_radius(radius_nm)?;
    Ok(haversine_nm(lat, lon, center_lat, center_lon)? <= radius_nm)
}

pub(crate) fn validate_radius(radius: f64) -> Result<()> {
    if !radius.is_finite() || radius < 0.0 {
        return Err(Error::InvalidInput("radius requires a finite nonnegative nautical-mile value".into()));
    }
    Ok(())
}

/// Conservative spherical bounding box, including all longitudes when a cap
/// reaches either pole. Used for bitmap candidates, followed by haversine.
pub fn radius_bbox(lat: f64, lon: f64, radius_nm: f64) -> Result<Bbox> {
    validate_point(lat, lon)?;
    validate_radius(radius_nm)?;
    let angle = (radius_nm / EARTH_RADIUS_NM).min(std::f64::consts::PI);
    // Expand a tiny amount for floating-point trig roundoff at boundaries.
    let delta = angle.to_degrees() + 1e-9;
    let lat_min = (lat - delta).max(-90.0);
    let lat_max = (lat + delta).min(90.0);
    if lat_min == -90.0 || lat_max == 90.0 {
        return Bbox::new(lat_min, -180.0, lat_max, 180.0);
    }
    let span = (angle.sin() / lat.to_radians().cos()).clamp(-1.0, 1.0).asin().abs().to_degrees() + 1e-9;
    let wrap = |value: f64| (value + 180.0).rem_euclid(360.0) - 180.0;
    Bbox::new(lat_min, wrap(lon - span), lat_max, wrap(lon + span))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn wrapped_bbox_and_radius_edges() {
        let bbox = Bbox::new(-1.0, 179.0, 1.0, -179.0).unwrap();
        assert!(bbox.contains(0.0, 180.0));
        assert!(bbox.contains(0.0, -180.0));
        assert!(!bbox.contains(0.0, 0.0));
        assert!(Bbox::new(-1.0, -180.0, 1.0, -179.0).unwrap().contains(0.0, 180.0));
        assert!(radius_bbox(89.9, 0.0, 20.0).unwrap().contains(89.99, 179.0));
        assert_eq!(haversine_nm(0.0, 0.0, 0.0, 0.0).unwrap(), 0.0);
        assert!((haversine_nm(0.0, 179.9, 0.0, -179.9).unwrap() - 12.008092).abs() < 1e-5);
        assert!(within_nm(0.0, 0.0, 0.0, 0.0, 0.0).unwrap());
        assert!(within_nm(0.0, 0.0, 0.0, 0.0, -1.0).is_err());
        assert!(Bbox::new(1.0, 0.0, -1.0, 0.0).is_err());
    }
}

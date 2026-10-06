//! H3 indices at the three resolutions persisted by telemetry ingest.
mod cover;
pub use cover::{bbox_cover, bbox_cover_at_resolution, radius_cover, resolution_for_area};
mod refine;
pub use refine::{haversine_nm, radius_bbox, within_nm, Bbox, EARTH_RADIUS_NM};

use h3o::{LatLng, Resolution};
use ti_contracts::{Error, Result};

/// Return resolution 5, 7 and 9 cells for a valid WGS84 coordinate.
pub fn cells_for(lat: f64, lon: f64) -> Result<[u64; 3]> {
    validate_point(lat, lon)?;
    let point = LatLng::new(lat, lon).map_err(|e| Error::InvalidInput(e.to_string()))?;
    Ok([Resolution::Five, Resolution::Seven, Resolution::Nine]
        .map(|resolution| u64::from(point.to_cell(resolution))))
}

fn validate_point(lat: f64, lon: f64) -> Result<()> {
    if !lat.is_finite() || !lon.is_finite() || !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
        return Err(Error::InvalidInput("coordinate requires finite latitude [-90,90] and longitude [-180,180]".into()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cells_are_valid_and_have_requested_resolutions() {
        for (lat, lon) in [(36.0, -122.0), (90.0, 180.0), (-90.0, -180.0)] {
            for (cell, resolution) in cells_for(lat, lon).unwrap().into_iter().zip([5, 7, 9]) {
                let cell = h3o::CellIndex::try_from(cell).unwrap();
                assert_eq!(u8::from(cell.resolution()), resolution);
            }
        }
        for (lat, lon) in [(91.0, 0.0), (0.0, 181.0), (f64::NAN, 0.0)] {
            assert!(cells_for(lat, lon).is_err());
        }
    }
}

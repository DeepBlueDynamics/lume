//! Conservative Covers tiling. Rectangle pieces avoid ambiguous >180° arcs.
use crate::{radius_bbox, Bbox};
use geo::{LineString, Polygon};
use h3o::{
    geom::{ContainmentMode, TilerBuilder},
    Resolution,
};
use std::collections::BTreeSet;
use ti_contracts::{Error, Result};

/// Choose among the three ingest resolutions to keep ordinary covers bounded.
pub fn resolution_for_area(area_km2: f64) -> u8 {
    if area_km2 <= 25.0 {
        9
    } else if area_km2 <= 2500.0 {
        7
    } else {
        5
    }
}
pub fn bbox_cover(lat_min: f64, lon_min: f64, lat_max: f64, lon_max: f64) -> Result<Vec<u64>> {
    let bbox = Bbox::new(lat_min, lon_min, lat_max, lon_max)?;
    bbox_cover_at_resolution(bbox, resolution_for_area(bbox.area_km2()))
}
pub fn radius_cover(lat: f64, lon: f64, radius_nm: f64) -> Result<Vec<u64>> {
    let bbox = radius_bbox(lat, lon, radius_nm)?;
    bbox_cover_at_resolution(bbox, resolution_for_area(bbox.area_km2()))
}
/// Explicit resolution is useful for measuring spatial false positives.
/// Tiny padding handles zero-area rectangles and fixed-point reconstruction.
pub fn bbox_cover_at_resolution(bbox: Bbox, resolution: u8) -> Result<Vec<u64>> {
    let resolution = match resolution {
        5 => Resolution::Five,
        7 => Resolution::Seven,
        9 => Resolution::Nine,
        _ => {
            return Err(Error::InvalidInput(
                "cover resolution must be 5, 7 or 9".into(),
            ))
        }
    };
    let south = (bbox.lat_min - 1e-7).max(-90.0);
    let north = (bbox.lat_max + 1e-7).min(90.0);
    let mut spans = Vec::new();
    for (west, east) in bbox.longitude_spans() {
        // Pad across the dateline as well as along it.
        if west - 1e-7 < -180.0 {
            spans.push((180.0 - 1e-7, 180.0));
        }
        if east + 1e-7 > 180.0 {
            spans.push((-180.0, -180.0 + 1e-7));
        }
        spans.push(((west - 1e-7).max(-180.0), (east + 1e-7).min(180.0)));
    }
    let mut cells = BTreeSet::new();
    for (west, east) in spans {
        // All pieces are at most 90° wide, including full-longitude polar caps.
        let pieces = ((east - west) / 90.0).ceil().max(1.0) as usize;
        for i in 0..pieces {
            let lo = west + (east - west) * i as f64 / pieces as f64;
            let hi = west + (east - west) * (i + 1) as f64 / pieces as f64;
            let polygon = Polygon::new(
                LineString::from(vec![
                    (lo, south),
                    (hi, south),
                    (hi, north),
                    (lo, north),
                    (lo, south),
                ]),
                vec![],
            );
            let mut tiler = TilerBuilder::new(resolution)
                .containment_mode(ContainmentMode::Covers)
                .disable_transmeridian_heuristic()
                .build();
            tiler
                .add(polygon)
                .map_err(|e| Error::InvalidInput(e.to_string()))?;
            cells.extend(tiler.into_coverage().map(u64::from));
        }
    }
    Ok(cells.into_iter().collect())
}

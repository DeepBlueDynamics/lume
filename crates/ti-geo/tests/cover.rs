use proptest::prelude::*;
use ti_geo::{bbox_cover, bbox_cover_at_resolution, cells_for, haversine_nm, radius_bbox, radius_cover, Bbox};

fn hit(cells:&[u64],lat:f64,lon:f64) -> bool {
    cells_for(lat,lon).unwrap().iter().any(|c| cells.binary_search(c).is_ok())
}
proptest! {
    #![proptest_config(ProptestConfig::with_cases(300))]
    #[test]
    fn bbox_never_misses(lat in -89.99f64..89.99, lon in -180.0f64..180.0, delta in 0.00000001f64..0.05, a in 0.0f64..1.0, b in 0.0f64..1.0) {
        let south = (lat-delta).max(-90.0);
        let north = (lat+delta).min(90.0);
        let wrap = |v:f64| (v+180.0).rem_euclid(360.0)-180.0;
        let west = wrap(lon-delta);
        let east = wrap(lon+delta);
        let p_lat = south+(north-south)*a;
        let p_lon = wrap(lon-delta+2.0*delta*b);
        let cover = bbox_cover(south,west,north,east).unwrap();
        prop_assert!(hit(&cover,p_lat,p_lon), "bbox {:?} point {p_lat},{p_lon}", (south,west,north,east));
    }
    #[test]
    fn radius_never_misses(lat in -89.99f64..89.99, lon in -180.0f64..180.0, radius in 0.001f64..3.0, bearing in 0.0f64..std::f64::consts::TAU, fraction in 0.0f64..1.0) {
        // Independent spherical destination calculation.
        let angular = radius*fraction/3440.065;
        let phi = lat.to_radians();
        let lambda = lon.to_radians();
        let dest_phi = (phi.sin()*angular.cos()+phi.cos()*angular.sin()*bearing.cos()).asin();
        let dest_lambda = lambda+(bearing.sin()*angular.sin()*phi.cos()).atan2(angular.cos()-phi.sin()*dest_phi.sin());
        let dest_lat = dest_phi.to_degrees();
        let dest_lon = (dest_lambda.to_degrees()+180.0).rem_euclid(360.0)-180.0;
        prop_assert!(haversine_nm(lat,lon,dest_lat,dest_lon).unwrap() <= radius+1e-8);
        prop_assert!(radius_bbox(lat,lon,radius).unwrap().contains(dest_lat,dest_lon));
        let cover = radius_cover(lat,lon,radius).unwrap();
        prop_assert!(hit(&cover,dest_lat,dest_lon), "radius {lat},{lon},{radius} point {dest_lat},{dest_lon}");
    }
}
#[test]
fn poles_dateline_degenerate_and_boundary() {
    for b in [
        Bbox::new(89.9,-180.0,90.0,180.0).unwrap(),
        Bbox::new(-90.0,-180.0,-89.9,180.0).unwrap(),
        Bbox::new(-1.0,179.9,1.0,-179.9).unwrap(),
        Bbox::new(36.0,-122.0,36.0,-122.0).unwrap(),
        Bbox::new(0.0,-180.0,0.0,-180.0).unwrap(),
    ] {
        let cover = bbox_cover(b.lat_min,b.lon_min,b.lat_max,b.lon_max).unwrap();
        for (lat,lon) in [(b.lat_min,b.lon_min),(b.lat_max,b.lon_max)] {
            assert!(hit(&cover,lat,lon),"{b:?}, {lat},{lon}");
        }
    }
    for lat in [-90.0,90.0] {
        let cover = radius_cover(lat,180.0,1.0).unwrap();
        for lon in [-180.0,-90.0,0.0,90.0,180.0] {
            assert!(hit(&cover,lat,lon));
        }
    }
}
#[test]
fn res9_false_positive_bucket_rate() {
    // One coordinate per synthetic bucket, a uniform 301x301 local grid.
    let bbox = Bbox::new(35.99,-122.02,36.03,-121.98).unwrap();
    let cover = bbox_cover_at_resolution(bbox,9).unwrap();
    let mut candidates=0;
    let mut false_positives=0;
    let mut exact=0;
    for i in 0..301 {
        for j in 0..301 {
            let lat=35.97+0.08*i as f64/300.0;
            let lon=-122.04+0.08*j as f64/300.0;
            let inside=bbox.contains(lat,lon);
            let covered=hit(&cover,lat,lon);
            if inside { exact+=1; assert!(covered); }
            if covered { candidates+=1; if !inside { false_positives+=1; } }
        }
    }
    let rate=false_positives as f64/candidates as f64;
    println!("res9 bbox: {exact} exact buckets, {candidates} candidate buckets, {false_positives} false positives, rate={:.4}%",rate*100.0);
    assert!(rate<=0.30);
}

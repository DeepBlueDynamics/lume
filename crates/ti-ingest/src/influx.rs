//! InfluxDB backfill mapper for HaLOS Marine (signalk-to-influxdb2 point model).
//!
//! Enforces:
//! - Measurement is the Signal K path
//! - Tags include `context` and `source`
//! - Field `value` holds the scalar value (or `lat`/`lon` for positions)
//! - Nanosecond timestamp converted to epoch seconds (with repo-fit §10 note on insert-time timestamps)

use crate::normalize::{NormalizedPoint, NormalizedValue};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq)]
pub enum InfluxValue {
    Float(f64),
    Integer(i64),
    String(String),
    Boolean(bool),
}

#[derive(Debug, Clone, PartialEq)]
pub struct InfluxPoint {
    pub measurement: String,
    pub tags: BTreeMap<String, String>,
    pub fields: BTreeMap<String, InfluxValue>,
    pub timestamp_ns: i64,
}

/// Map an InfluxDB point to NormalizedPoints for the TI ingest pipeline.
pub fn map_influx_point(point: InfluxPoint, self_urn: &str) -> Vec<NormalizedPoint> {
    let ts = point.timestamp_ns / 1_000_000_000;
    let context = point
        .tags
        .get("context")
        .cloned()
        .unwrap_or_else(|| self_urn.to_string());
    let source = point
        .tags
        .get("source")
        .cloned()
        .unwrap_or_else(|| "influxdb".to_string());

    let mut out = Vec::new();

    if point.measurement == "navigation.position" {
        let lat = point.fields.get("lat").and_then(|v| match v {
            InfluxValue::Float(f) => Some(*f),
            InfluxValue::Integer(i) => Some(*i as f64),
            _ => None,
        });
        let lon = point.fields.get("lon").and_then(|v| match v {
            InfluxValue::Float(f) => Some(*f),
            InfluxValue::Integer(i) => Some(*i as f64),
            _ => None,
        });

        if let (Some(lat), Some(lon)) = (lat, lon) {
            out.push(NormalizedPoint {
                context: context.clone(),
                path: "navigation.position.latitude".into(),
                source: source.clone(),
                timestamp: ts,
                value: NormalizedValue::Double(lat),
            });
            out.push(NormalizedPoint {
                context: context.clone(),
                path: "navigation.position.longitude".into(),
                source: source.clone(),
                timestamp: ts,
                value: NormalizedValue::Double(lon),
            });
            out.push(NormalizedPoint {
                context,
                path: "navigation.position".into(),
                source,
                timestamp: ts,
                value: NormalizedValue::Geo { lat, lon },
            });
            return out;
        }
    }

    if let Some(val) = point.fields.get("value") {
        let n_val = match val {
            InfluxValue::Float(f) => NormalizedValue::Double(*f),
            InfluxValue::Integer(i) => NormalizedValue::Double(*i as f64),
            InfluxValue::String(s) => NormalizedValue::String(s.clone()),
            InfluxValue::Boolean(b) => NormalizedValue::Bool(*b),
        };
        out.push(NormalizedPoint {
            context,
            path: point.measurement,
            source,
            timestamp: ts,
            value: n_val,
        });
    } else {
        // Multi-field measurement e.g. attitude
        for (f_name, val) in point.fields {
            let n_val = match val {
                InfluxValue::Float(f) => NormalizedValue::Double(f),
                InfluxValue::Integer(i) => NormalizedValue::Double(i as f64),
                InfluxValue::String(s) => NormalizedValue::String(s),
                InfluxValue::Boolean(b) => NormalizedValue::Bool(b),
            };
            out.push(NormalizedPoint {
                context: context.clone(),
                path: format!("{}.{f_name}", point.measurement),
                source: source.clone(),
                timestamp: ts,
                value: n_val,
            });
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_map_scalar_point() {
        let mut tags = BTreeMap::new();
        tags.insert(
            "context".into(),
            "vessels.urn:mrn:signalk:uuid:boat-1".into(),
        );
        tags.insert("source".into(), "n2k.115".into());

        let mut fields = BTreeMap::new();
        fields.insert("value".into(), InfluxValue::Float(7.85));

        let point = InfluxPoint {
            measurement: "navigation.speedOverGround".into(),
            tags,
            fields,
            timestamp_ns: 1_700_000_000_000_000_000,
        };

        let pts = map_influx_point(point, "vessels.self");
        assert_eq!(pts.len(), 1);
        assert_eq!(pts[0].path, "navigation.speedOverGround");
        assert_eq!(pts[0].timestamp, 1_700_000_000);
        assert_eq!(pts[0].source, "n2k.115");
        assert_eq!(pts[0].value, NormalizedValue::Double(7.85));
    }

    #[test]
    fn test_map_position_point() {
        let mut tags = BTreeMap::new();
        tags.insert("source".into(), "gps".into());

        let mut fields = BTreeMap::new();
        fields.insert("lat".into(), InfluxValue::Float(57.73));
        fields.insert("lon".into(), InfluxValue::Float(11.66));

        let point = InfluxPoint {
            measurement: "navigation.position".into(),
            tags,
            fields,
            timestamp_ns: 1_700_000_050_000_000_000,
        };

        let pts = map_influx_point(point, "vessels.urn:self");
        assert_eq!(pts.len(), 3);
        assert!(pts.iter().any(|p| p.path == "navigation.position.latitude"));
        assert!(pts
            .iter()
            .any(|p| p.path == "navigation.position.longitude"));
    }
}

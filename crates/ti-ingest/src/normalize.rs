//! Normalization and object flattening for Signal K data points.
//!
//! Enforces:
//! - Object flattening: `navigation.position` -> `.latitude`, `.longitude`
//! - Arbitrary object flattening: `{a, b}` -> `path.a`, `path.b`
//! - Root object flattening for `path: ""`
//! - Filtering via allow/deny lists (e.g. `*.ais.*`, `design.*`)
//! - Dropping meta-only objects

use crate::decode::RawDataPoint;

const META_KEYS: &[&str] = &[
    "units",
    "description",
    "displayName",
    "shortName",
    "longName",
    "zones",
    "displayScale",
    "timeout",
    "enum",
    "properties",
    "displayUnits",
    "meta",
];

#[derive(Debug, Clone, PartialEq)]
pub enum NormalizedValue {
    Double(f64),
    String(String),
    Bool(bool),
    Null,
    Geo { lat: f64, lon: f64 },
}

#[derive(Debug, Clone, PartialEq)]
pub struct NormalizedPoint {
    pub context: String,
    pub path: String,
    pub source: String,
    pub timestamp: i64,
    pub value: NormalizedValue,
}

pub fn matches_glob(pattern: &str, path: &str) -> bool {
    if pattern == path || pattern == "*" {
        return true;
    }
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return pattern == path;
    }
    let mut remainder = path;
    for (i, part) in parts.iter().enumerate() {
        if i == 0 {
            if !remainder.starts_with(part) {
                return false;
            }
            remainder = &remainder[part.len()..];
        } else if i == parts.len() - 1 {
            if !remainder.ends_with(part) {
                return false;
            }
        } else {
            if let Some(pos) = remainder.find(part) {
                remainder = &remainder[pos + part.len()..];
            } else {
                return false;
            }
        }
    }
    true
}

pub fn is_path_allowed(path: &str, allow_paths: &[String], deny_paths: &[String]) -> bool {
    // Deny list takes precedence
    for pattern in deny_paths {
        if matches_glob(pattern, path) {
            return false;
        }
    }
    if allow_paths.is_empty() {
        return true;
    }
    allow_paths.iter().any(|p| matches_glob(p, path))
}

fn is_meta_only_object(obj: &serde_json::Map<String, serde_json::Value>) -> bool {
    if obj.is_empty() {
        return false;
    }
    obj.keys().all(|k| META_KEYS.contains(&k.as_str()))
}

/// Normalizes a raw data point, flattening objects and applying allow/deny filters.
pub fn normalize_point(
    raw: RawDataPoint,
    allow_paths: &[String],
    deny_paths: &[String],
) -> Vec<NormalizedPoint> {
    normalize_point_ref(&raw, allow_paths, deny_paths)
}

/// Normalize a borrowed raw point without cloning its context, path, source and
/// JSON value first. Resulting normalized points remain independently owned.
pub fn normalize_point_ref(
    raw: &RawDataPoint,
    allow_paths: &[String],
    deny_paths: &[String],
) -> Vec<NormalizedPoint> {
    let mut out = Vec::new();
    flatten_value(
        &raw.context,
        &raw.path,
        &raw.source,
        raw.timestamp,
        &raw.value,
        allow_paths,
        deny_paths,
        &mut out,
    );
    out
}

#[allow(clippy::too_many_arguments)]
fn flatten_value(
    context: &str,
    path: &str,
    source: &str,
    timestamp: i64,
    value: &serde_json::Value,
    allow_paths: &[String],
    deny_paths: &[String],
    out: &mut Vec<NormalizedPoint>,
) {
    match value {
        serde_json::Value::Null => {
            if !path.is_empty() && is_path_allowed(path, allow_paths, deny_paths) {
                out.push(NormalizedPoint {
                    context: context.to_string(),
                    path: path.to_string(),
                    source: source.to_string(),
                    timestamp,
                    value: NormalizedValue::Null,
                });
            }
        }
        serde_json::Value::Bool(b) => {
            if !path.is_empty() && is_path_allowed(path, allow_paths, deny_paths) {
                out.push(NormalizedPoint {
                    context: context.to_string(),
                    path: path.to_string(),
                    source: source.to_string(),
                    timestamp,
                    value: NormalizedValue::Bool(*b),
                });
            }
        }
        serde_json::Value::Number(n) => {
            if let Some(f) = n.as_f64() {
                if !path.is_empty() && is_path_allowed(path, allow_paths, deny_paths) {
                    out.push(NormalizedPoint {
                        context: context.to_string(),
                        path: path.to_string(),
                        source: source.to_string(),
                        timestamp,
                        value: NormalizedValue::Double(f),
                    });
                }
            }
        }
        serde_json::Value::String(s) => {
            if !path.is_empty() && is_path_allowed(path, allow_paths, deny_paths) {
                out.push(NormalizedPoint {
                    context: context.to_string(),
                    path: path.to_string(),
                    source: source.to_string(),
                    timestamp,
                    value: NormalizedValue::String(s.clone()),
                });
            }
        }
        serde_json::Value::Object(map) => {
            if is_meta_only_object(map) {
                return;
            }

            // Special handling for navigation.position
            if path == "navigation.position" {
                if let (Some(lat_v), Some(lon_v)) = (map.get("latitude"), map.get("longitude")) {
                    if let (Some(lat), Some(lon)) = (lat_v.as_f64(), lon_v.as_f64()) {
                        let lat_path = "navigation.position.latitude";
                        if is_path_allowed(lat_path, allow_paths, deny_paths) {
                            out.push(NormalizedPoint {
                                context: context.to_string(),
                                path: lat_path.to_string(),
                                source: source.to_string(),
                                timestamp,
                                value: NormalizedValue::Double(lat),
                            });
                        }
                        let lon_path = "navigation.position.longitude";
                        if is_path_allowed(lon_path, allow_paths, deny_paths) {
                            out.push(NormalizedPoint {
                                context: context.to_string(),
                                path: lon_path.to_string(),
                                source: source.to_string(),
                                timestamp,
                                value: NormalizedValue::Double(lon),
                            });
                        }
                        if is_path_allowed(path, allow_paths, deny_paths) {
                            out.push(NormalizedPoint {
                                context: context.to_string(),
                                path: path.to_string(),
                                source: source.to_string(),
                                timestamp,
                                value: NormalizedValue::Geo { lat, lon },
                            });
                        }
                        return;
                    }
                }
            }

            // General object flattening
            for (k, v) in map {
                let sub_path = if path.is_empty() {
                    k.clone()
                } else {
                    format!("{path}.{k}")
                };
                flatten_value(
                    context,
                    &sub_path,
                    source,
                    timestamp,
                    v,
                    allow_paths,
                    deny_paths,
                    out,
                );
            }
        }
        serde_json::Value::Array(_) => {
            // Arrays (except top-level values) are not flattened into individual columns
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_glob_matching() {
        assert!(matches_glob("*.ais.*", "vessels.12345.ais.class"));
        assert!(!matches_glob("*.ais.*", "navigation.speed"));
        assert!(matches_glob("design.*", "design.length"));
        assert!(matches_glob("propulsion.*.state", "propulsion.port.state"));
        assert!(!matches_glob("propulsion.*.state", "propulsion.port.rpm"));
    }

    #[test]
    fn test_flatten_position() {
        let raw = RawDataPoint {
            context: "vessels.self".into(),
            path: "navigation.position".into(),
            source: "gps".into(),
            timestamp: 1000,
            value: serde_json::json!({
                "latitude": 60.17,
                "longitude": 24.94,
                "altitude": 5.0
            }),
        };

        let points = normalize_point(raw, &[], &[]);
        assert_eq!(points.len(), 3);
        assert!(points
            .iter()
            .any(|p| p.path == "navigation.position.latitude"
                && p.value == NormalizedValue::Double(60.17)));
        assert!(points
            .iter()
            .any(|p| p.path == "navigation.position.longitude"
                && p.value == NormalizedValue::Double(24.94)));
        assert!(points.iter().any(|p| p.path == "navigation.position"
            && p.value
                == NormalizedValue::Geo {
                    lat: 60.17,
                    lon: 24.94
                }));
    }

    #[test]
    fn test_deny_paths() {
        let raw = RawDataPoint {
            context: "vessels.self".into(),
            path: "vessels.other.ais.name".into(),
            source: "ais".into(),
            timestamp: 1000,
            value: serde_json::json!("Cargo Vessel"),
        };

        let points = normalize_point(raw, &[], &["*.ais.*".into()]);
        assert!(points.is_empty());
    }

    #[test]
    fn test_empty_path_root_merge() {
        let raw = RawDataPoint {
            context: "vessels.self".into(),
            path: "".into(),
            source: "n2k".into(),
            timestamp: 1000,
            value: serde_json::json!({
                "name": "MY BOAT",
                "mmsi": "123456789"
            }),
        };

        let points = normalize_point(raw, &[], &[]);
        assert_eq!(points.len(), 2);
        assert!(points
            .iter()
            .any(|p| p.path == "name" && p.value == NormalizedValue::String("MY BOAT".into())));
        assert!(points
            .iter()
            .any(|p| p.path == "mmsi" && p.value == NormalizedValue::String("123456789".into())));
    }
}

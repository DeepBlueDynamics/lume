//! Signal K delta decoding and source-label resolution.
//!
//! Enforces:
//! - RFC 3339 UTC timestamp parsing with 5-minute skew fallback to receive time
//! - Source label precedence: live `$source`, then `source_label`, then derived `getSourceId` rules
//! - Delta shape from `SignalK/specification` schemas `1.8.4` (updates, context, values, meta)

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignalKDelta {
    #[serde(default)]
    pub context: Option<String>,
    #[serde(default)]
    pub updates: Vec<SignalKUpdate>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignalKUpdate {
    #[serde(default)]
    pub source: Option<SignalKSource>,
    #[serde(default, rename = "$source")]
    pub dollar_source: Option<String>,
    #[serde(default)]
    pub timestamp: Option<String>,
    #[serde(default)]
    pub values: Vec<SignalKPathValue>,
    #[serde(default)]
    pub meta: Vec<SignalKPathMeta>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignalKSource {
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default, rename = "type")]
    pub source_type: Option<String>,
    #[serde(default)]
    pub src: Option<String>,
    #[serde(default, rename = "canName")]
    pub can_name: Option<String>,
    #[serde(default)]
    pub pgn: Option<u32>,
    #[serde(default)]
    pub talker: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignalKPathValue {
    pub path: String,
    pub value: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignalKPathMeta {
    pub path: String,
    pub value: serde_json::Value,
}

/// A decoded, unflattened Signal K data point.
#[derive(Debug, Clone, PartialEq)]
pub struct RawDataPoint {
    pub context: String,
    pub path: String,
    pub source: String,
    pub timestamp: i64,
    pub value: serde_json::Value,
}

/// Derive sourceRef following signalk-server getSourceId rules:
/// `label.canName` -> `label.src` -> `label.talker` -> `label`.
pub fn derive_source_id(source: &SignalKSource) -> Option<String> {
    let label = source.label.as_deref()?;
    if let Some(can) = &source.can_name {
        Some(format!("{label}.{can}"))
    } else if let Some(src) = &source.src {
        Some(format!("{label}.{src}"))
    } else if let Some(talker) = &source.talker {
        Some(format!("{label}.{talker}"))
    } else {
        Some(label.to_string())
    }
}

/// Resolve normalized source label using the frozen precedence rules:
/// Live `$source` -> `source_label` -> derived source object ID -> fallback `"unknown"`.
pub fn resolve_source_label(
    dollar_source: Option<&str>,
    parquet_source_label: Option<&str>,
    source: Option<&SignalKSource>,
) -> String {
    let derived = source.and_then(derive_source_id);
    ti_contracts::normalized_source_label(dollar_source, parquet_source_label, derived.as_deref())
        .unwrap_or_else(|| "unknown".to_string())
}

/// Parse RFC 3339 timestamp. If missing, invalid, or skewed > 5 minutes from `receive_time`,
/// fall back to `receive_time`.
pub fn parse_timestamp(ts_str: Option<&str>, receive_time: i64) -> i64 {
    match ts_str {
        Some(s) => match chrono::DateTime::parse_from_rfc3339(s) {
            Ok(dt) => {
                let ts = dt.timestamp();
                if (ts - receive_time).abs() > 300 {
                    receive_time
                } else {
                    ts
                }
            }
            Err(_) => receive_time,
        },
        None => receive_time,
    }
}

/// Decode a Signal K delta into a stream of raw data points.
pub fn decode_delta(
    delta: &SignalKDelta,
    self_urn: &str,
    receive_time: i64,
) -> (Vec<RawDataPoint>, Vec<(String, serde_json::Value)>) {
    let context = match &delta.context {
        Some(c) if c != "vessels.self" && !c.is_empty() => c.clone(),
        _ => self_urn.to_string(),
    };

    let mut points = Vec::new();
    let mut meta_entries = Vec::new();

    for update in &delta.updates {
        let ts = parse_timestamp(update.timestamp.as_deref(), receive_time);
        let src = resolve_source_label(
            update.dollar_source.as_deref(),
            None,
            update.source.as_ref(),
        );

        for val in &update.values {
            points.push(RawDataPoint {
                context: context.clone(),
                path: val.path.clone(),
                source: src.clone(),
                timestamp: ts,
                value: val.value.clone(),
            });
        }

        for m in &update.meta {
            meta_entries.push((m.path.clone(), m.value.clone()));
        }
    }

    (points, meta_entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_source_resolution_precedence() {
        let source_obj = SignalKSource {
            label: Some("N2K".into()),
            source_type: Some("NMEA2000".into()),
            src: Some("017".into()),
            can_name: Some("can0".into()),
            pgn: Some(127488),
            talker: None,
        };

        // $source wins over source object
        assert_eq!(
            resolve_source_label(Some("explicit.source"), None, Some(&source_obj)),
            "explicit.source"
        );

        // canName derived
        assert_eq!(
            resolve_source_label(None, None, Some(&source_obj)),
            "N2K.can0"
        );

        // src derived when no canName
        let source_src = SignalKSource {
            label: Some("N2K".into()),
            source_type: None,
            src: Some("017".into()),
            can_name: None,
            pgn: None,
            talker: None,
        };
        assert_eq!(
            resolve_source_label(None, None, Some(&source_src)),
            "N2K.017"
        );

        // bare label fallback
        let source_bare = SignalKSource {
            label: Some("plugin-logbook".into()),
            source_type: None,
            src: None,
            can_name: None,
            pgn: None,
            talker: None,
        };
        assert_eq!(
            resolve_source_label(None, None, Some(&source_bare)),
            "plugin-logbook"
        );

        // fallback unknown
        assert_eq!(resolve_source_label(None, None, None), "unknown");
    }

    #[test]
    fn test_source_resolution_matches_contracts_helper() {
        let delta: SignalKDelta = serde_json::from_value(serde_json::json!({
            "context": "vessels.self",
            "updates": [{
                "$source": "can0.115",
                "source": { "label": "actisense", "type": "NMEA2000", "src": "115" },
                "values": [{ "path": "navigation.speedOverGround", "value": 5.2 }]
            }]
        }))
        .unwrap();

        let (points, _) = decode_delta(&delta, "vessels.self", 1577836800);
        assert_eq!(points.len(), 1);

        // Verify that the decoded point's source exactly matches ti_contracts::normalized_source_label
        let contracts_norm =
            ti_contracts::normalized_source_label(Some("can0.115"), None, Some("actisense.115"));
        assert_eq!(Some(points[0].source.clone()), contracts_norm);
    }

    #[test]
    fn test_timestamp_parsing_and_skew() {
        let now = 1_700_000_100;
        // Valid within 5 min
        let valid_iso = "2023-11-14T22:15:10Z";
        let parsed = chrono::DateTime::parse_from_rfc3339(valid_iso)
            .unwrap()
            .timestamp();
        assert_eq!(parse_timestamp(Some(valid_iso), parsed + 10), parsed);

        // Skewed > 5 min (301 seconds ahead)
        assert_eq!(parse_timestamp(Some(valid_iso), parsed - 301), parsed - 301);

        // Missing or invalid
        assert_eq!(parse_timestamp(None, now), now);
        assert_eq!(parse_timestamp(Some("garbage"), now), now);
    }

    #[test]
    fn test_decode_delta() {
        let json_str = r#"{
            "context": "vessels.self",
            "updates": [{
                "timestamp": "2024-01-01T00:00:00Z",
                "$source": "can0.115",
                "values": [
                    {"path": "navigation.speedOverGround", "value": 5.25}
                ]
            }]
        }"#;

        let delta: SignalKDelta = serde_json::from_str(json_str).unwrap();
        let (points, _) =
            decode_delta(&delta, "vessels.urn:mrn:signalk:uuid:boat-1", 1_704_067_200);
        assert_eq!(points.len(), 1);
        assert_eq!(points[0].context, "vessels.urn:mrn:signalk:uuid:boat-1");
        assert_eq!(points[0].path, "navigation.speedOverGround");
        assert_eq!(points[0].source, "can0.115");
        assert_eq!(points[0].value, serde_json::json!(5.25));
    }
}

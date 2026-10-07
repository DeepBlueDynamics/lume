//! Sticky type classification for Signal K telemetry paths.
//!
//! Enforces:
//! - Numbers -> `FieldKind::Bsi { scale }`
//! - Strings, booleans, enums -> `FieldKind::Set`
//! - Positions -> `FieldKind::Geo`
//! - Unit/scale resolution: path_scales -> unit_scales(meta.units) -> default scale 3
//! - Sticky classification: first assignment sticks; type change creates `path#2`

use std::collections::BTreeMap;
use ti_contracts::{FieldKind, TiConfig};

use crate::normalize::NormalizedValue;

#[derive(Debug, Clone)]
pub struct Classifier {
    count_paths: std::collections::BTreeSet<String>,
    unit_scales: BTreeMap<String, u8>,
    path_scales: BTreeMap<String, u8>,
    metric_units: ti_contracts::MetricUnits,
    meta_units: BTreeMap<String, String>,
    assigned: BTreeMap<(String, String), (String, FieldKind)>,
    versioned_paths: BTreeMap<(String, String), u32>,
}

impl Classifier {
    pub fn new(config: &TiConfig) -> Self {
        Self {
            count_paths: config.ingest.count_paths.iter().cloned().collect(),
            unit_scales: config.unit_scales.clone(),
            path_scales: config.path_scales.clone(),
            metric_units: config.units.clone(),
            meta_units: BTreeMap::new(),
            assigned: BTreeMap::new(),
            versioned_paths: BTreeMap::new(),
        }
    }

    /// Apply the event policy captured by the bucket being classified.
    pub fn set_count_paths(&mut self, paths: &std::collections::BTreeSet<String>) {
        if &self.count_paths != paths {
            self.count_paths = paths.clone();
        }
    }

    /// Register units for a path discovered from Signal K metadata.
    pub fn register_meta_units(&mut self, path: &str, units: &str) {
        self.meta_units.insert(path.to_string(), units.to_string());
    }

    /// Resolve the fixed-point scale for a numeric path.
    pub fn resolve_scale(&self, path: &str) -> u8 {
        if let Some(&s) = self.path_scales.get(path) {
            return s;
        }
        if let Some(unit) = ti_contracts::metric_unit(&self.metric_units, path) {
            return unit.scale;
        }
        // Flattened positions are degrees even when no meta.units is available (backfill):
        // use the spec/05 registry's lat/lon scale.
        if path == "navigation.position.latitude" || path == "navigation.position.longitude" {
            if let Some(&s) = self.unit_scales.get("lat/lon") {
                return s;
            }
        }
        if let Some(u) = self.meta_units.get(path) {
            if let Some(&s) = self.unit_scales.get(u) {
                return s;
            }
        }
        *self.unit_scales.get("unknown").unwrap_or(&3)
    }

    /// Classify a path and value. Returns `(effective_path, field_kind)`.
    pub fn classify(
        &mut self,
        context: &str,
        path: &str,
        value: &NormalizedValue,
    ) -> Option<(String, FieldKind)> {
        if self.count_paths.contains(path)
            && !matches!(value, NormalizedValue::Double(v) if v.is_finite())
        {
            return None;
        }
        let desired_kind = match value {
            NormalizedValue::Double(_) => FieldKind::Bsi {
                scale: self.resolve_scale(path),
            },
            NormalizedValue::String(_) | NormalizedValue::Bool(_) => FieldKind::Set,
            NormalizedValue::Geo { .. } => FieldKind::Geo { res: 7 },
            NormalizedValue::Null => {
                // Return existing classification if known
                return self
                    .assigned
                    .get(&(context.to_string(), path.to_string()))
                    .cloned();
            }
        };

        let key = (context.to_string(), path.to_string());
        if let Some((eff_path, existing_kind)) = self.assigned.get(&key) {
            if is_compatible(existing_kind, &desired_kind) {
                return Some((eff_path.clone(), existing_kind.clone()));
            }

            // Incompatible type -> look for or create versioned path e.g. path#2
            let current_v = self.versioned_paths.entry(key.clone()).or_insert(1);
            let next_v = *current_v + 1;
            let eff_v_path = format!("{path}#{next_v}");
            let eff_key = (context.to_string(), eff_v_path.clone());

            if let Some((v_path, v_kind)) = self.assigned.get(&eff_key) {
                if is_compatible(v_kind, &desired_kind) {
                    return Some((v_path.clone(), v_kind.clone()));
                }
            }

            *current_v = next_v;
            self.assigned
                .insert(eff_key, (eff_v_path.clone(), desired_kind.clone()));
            return Some((eff_v_path, desired_kind));
        }

        // First assignment is sticky
        self.assigned
            .insert(key, (path.to_string(), desired_kind.clone()));
        Some((path.to_string(), desired_kind))
    }
}

fn is_compatible(a: &FieldKind, b: &FieldKind) -> bool {
    matches!(
        (a, b),
        (FieldKind::Bsi { .. }, FieldKind::Bsi { .. })
            | (FieldKind::Set, FieldKind::Set)
            | (FieldKind::Geo { .. }, FieldKind::Geo { .. })
            | (FieldKind::Count, FieldKind::Count)
            | (FieldKind::Presence, FieldKind::Presence)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_scale_resolution() {
        let mut cfg = TiConfig::default();
        cfg.path_scales
            .insert("navigation.speedOverGround".into(), 4);
        let mut classifier = Classifier::new(&cfg);
        classifier.register_meta_units("environment.wind.speedApparent", "m/s");

        assert_eq!(classifier.resolve_scale("navigation.speedOverGround"), 4);
        assert_eq!(
            classifier.resolve_scale("environment.wind.speedApparent"),
            3
        );
        assert_eq!(classifier.resolve_scale("unknown.metric"), 3);
    }

    #[test]
    fn test_sticky_classification_and_conflict() {
        let cfg = TiConfig::default();
        let mut classifier = Classifier::new(&cfg);
        let ctx = "vessels.urn:mrn:signalk:uuid:boat-1";

        // First: Double -> BSI
        let (p1, k1) = classifier
            .classify(ctx, "engine.state", &NormalizedValue::Double(1.0))
            .unwrap();
        assert_eq!(p1, "engine.state");
        assert!(matches!(k1, FieldKind::Bsi { .. }));

        // Same type -> returns same
        let (p2, k2) = classifier
            .classify(ctx, "engine.state", &NormalizedValue::Double(2.0))
            .unwrap();
        assert_eq!(p2, "engine.state");
        assert_eq!(k1, k2);

        // Type conflict: String arrives for engine.state -> creates engine.state#2
        let (p3, k3) = classifier
            .classify(
                ctx,
                "engine.state",
                &NormalizedValue::String("running".into()),
            )
            .unwrap();
        assert_eq!(p3, "engine.state#2");
        assert_eq!(k3, FieldKind::Set);
    }
}

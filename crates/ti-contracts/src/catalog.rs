use crate::{Error, FieldSpec, Result, VesselOrd};

/// Metadata needed to register a vessel or remap an imported vessel ordinal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VesselSpec {
    /// Canonical Signal K vessel context URN; never vessels.self.
    pub urn: String,
    /// Human-readable vessel name, if known.
    pub name: Option<String>,
    /// MMSI when known.
    pub mmsi: Option<String>,
}

/// Normalize source identity to the server sourceRef used by all catalog keys.
/// Prefer live $source, then Parquet source_label, then a source-object-derived
/// ID supplied by W3 using the pinned server getSourceId rules. Missing is None.
pub fn normalized_source_label(
    live_ref: Option<&str>,
    parquet_ref: Option<&str>,
    derived_ref: Option<&str>,
) -> Option<String> {
    [live_ref, parquet_ref, derived_ref]
        .into_iter()
        .flatten()
        .find(|s| !s.is_empty())
        .map(str::to_owned)
}

/// Notification states accepted by the Signal K schema; normal is also the
/// explicit cleared state from the server v2 API (null clear remains Clear).
pub const NOTIFICATION_STATES: [&str; 6] =
    ["nominal", "normal", "alert", "warn", "alarm", "emergency"];

/// Ordered source preference for an exact Signal K path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourcePriority {
    /// Exact base path.
    pub path: String,
    /// Most preferred source first; unknown sources follow in first-seen order.
    pub sources: Vec<String>,
}

/// Store-local, durable catalog boundary. Implementations serialize registrations.
/// IDs never change or get reused within a store. Registration is idempotent;
/// field metadata conflicts must return InvalidInput rather than silently change encoding.
pub trait Catalog: Send + Sync {
    /// Resolve/register a canonical URN, allocating a dense store-local ordinal.
    fn register_vessel(&self, vessel: &VesselSpec) -> Result<VesselOrd>;
    /// Look up a canonical URN by ordinal.
    fn vessel_urn(&self, vessel: VesselOrd) -> Result<String>;
    /// Register (path, agg) identity and encoding metadata; input id is ignored; returns stable field ID.
    fn register_field(&self, field: &FieldSpec) -> Result<u32>;
    /// Read field metadata with its assigned ID.
    fn field(&self, id: u32) -> Result<FieldSpec>;
    /// List fields in increasing field-ID order.
    fn fields(&self) -> Result<Vec<FieldSpec>>;
    /// Register a case-sensitive UTF-8 value in this field's dictionary.
    /// Valid for Set fields only; registration precedes emitting SetValue.
    fn register_set_value(&self, field: u32, value: &str) -> Result<u32>;
    /// Read a value from the field-local row-ID dictionary.
    fn set_value(&self, field: u32, row: u32) -> Result<String>;
    /// Replace an exact path's preferred-source ordering.
    fn set_source_priority(&self, priority: &SourcePriority) -> Result<()>;
    /// Read preferences; None means use first-seen ordering.
    fn source_priority(&self, path: &str) -> Result<Option<SourcePriority>>;
}

/// Remap an imported vessel by URN, never by a foreign store's ordinal.
/// The importer rewrites ordinal-bearing manifest keys, WAL records and ColumnIds.
pub fn remap_vessel(
    source: &dyn Catalog,
    destination: &dyn Catalog,
    foreign: VesselOrd,
) -> Result<VesselOrd> {
    destination.register_vessel(&VesselSpec {
        urn: source.vessel_urn(foreign)?,
        name: None,
        mmsi: None,
    })
}

/// Validate the deletion marker before a sink mutates state.
/// Clear must be rewrite-only and the sole value for its bucket/field group.
pub fn validate_clear_records(records: &[crate::BucketRecord]) -> Result<()> {
    for rec in records {
        if rec.value == crate::FieldValue::Clear
            && (!rec.rewrite
                || records
                    .iter()
                    .filter(|other| {
                        other.vessel == rec.vessel
                            && other.bucket == rec.bucket
                            && other.field == rec.field
                    })
                    .count()
                    != 1)
        {
            return Err(Error::InvalidInput(
                "FieldValue::Clear requires rewrite=true and a singleton group".into(),
            ));
        }
    }
    Ok(())
}

/// Check the ordinary-set invariant before publishing a shard: disjoint rows
/// whose union is exactly field presence. Multi-valued source sets use a different invariant.
pub fn validate_ordinary_set_rows(
    presence: &crate::RoaringBitmap,
    rows: &[crate::RoaringBitmap],
) -> Result<()> {
    let mut union = crate::RoaringBitmap::new();
    for row in rows {
        if !(row & &union).is_empty() {
            return Err(Error::Corrupt("ordinary set rows overlap".into()));
        }
        union |= row;
    }
    if &union != presence {
        return Err(Error::Corrupt(
            "ordinary set rows do not match presence".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod source_tests {
    use super::*;
    #[test]
    fn ordinary_sets_project_exactly_one_state() {
        let a: crate::RoaringBitmap = [1, 2].into_iter().collect();
        let b: crate::RoaringBitmap = [3].into_iter().collect();
        let presence: crate::RoaringBitmap = [1, 2, 3].into_iter().collect();
        assert!(validate_ordinary_set_rows(&presence, &[a.clone(), b]).is_ok());
        assert!(validate_ordinary_set_rows(&presence, &[a.clone(), a.clone()]).is_err());
        assert!(validate_ordinary_set_rows(&presence, &[a]).is_err());
    }
    #[test]
    fn source_refs_normalize_with_server_precedence() {
        assert_eq!(
            normalized_source_label(Some("can0.115"), Some("other"), Some("derived")),
            Some("can0.115".into())
        );
        assert_eq!(
            normalized_source_label(None, Some("can0.115"), None),
            Some("can0.115".into())
        );
        assert_eq!(
            normalized_source_label(None, None, Some("label.src")),
            Some("label.src".into())
        );
        assert_eq!(normalized_source_label(None, None, None), None);
        assert!(NOTIFICATION_STATES.contains(&"nominal"));
        assert!(NOTIFICATION_STATES.contains(&"normal"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Agg, BucketRecord, FieldKind, FieldValue};
    use std::sync::Mutex;

    type FixtureState = (
        Vec<VesselSpec>,
        Vec<FieldSpec>,
        Vec<(u32, String)>,
        Vec<SourcePriority>,
    );
    #[derive(Default)]
    struct Fixture(Mutex<FixtureState>);
    impl Catalog for Fixture {
        fn register_vessel(&self, v: &VesselSpec) -> Result<u32> {
            if !v.urn.starts_with("vessels.urn:") {
                return Err(Error::InvalidInput("vessel.urn".into()));
            }
            let mut state = self.0.lock().unwrap();
            if let Some(i) = state.0.iter().position(|x| x.urn == v.urn) {
                return Ok(i as u32);
            }
            state.0.push(v.clone());
            Ok((state.0.len() - 1) as u32)
        }
        fn vessel_urn(&self, v: u32) -> Result<String> {
            self.0
                .lock()
                .unwrap()
                .0
                .get(v as usize)
                .map(|v| v.urn.clone())
                .ok_or_else(|| Error::NotFound("vessel".into()))
        }
        fn register_field(&self, f: &FieldSpec) -> Result<u32> {
            let mut s = self.0.lock().unwrap();
            if let Some(old) = s.1.iter().find(|x| x.path == f.path && x.agg == f.agg) {
                if old.kind != f.kind || old.units != f.units {
                    return Err(Error::InvalidInput("field encoding conflict".into()));
                }
                return Ok(old.id);
            }
            let mut f = f.clone();
            f.id = s.1.len() as u32;
            let id = f.id;
            s.1.push(f);
            Ok(id)
        }
        fn field(&self, id: u32) -> Result<FieldSpec> {
            self.0
                .lock()
                .unwrap()
                .1
                .get(id as usize)
                .cloned()
                .ok_or_else(|| Error::NotFound("field".into()))
        }
        fn fields(&self) -> Result<Vec<FieldSpec>> {
            Ok(self.0.lock().unwrap().1.clone())
        }
        fn register_set_value(&self, f: u32, v: &str) -> Result<u32> {
            if self.field(f)?.kind != FieldKind::Set {
                return Err(Error::InvalidInput("field.kind".into()));
            }
            let mut s = self.0.lock().unwrap();
            if let Some(i) = s.2.iter().position(|x| x.0 == f && x.1 == v) {
                return Ok(i as u32);
            }
            s.2.push((f, v.into()));
            Ok((s.2.len() - 1) as u32)
        }
        fn set_value(&self, f: u32, r: u32) -> Result<String> {
            self.0
                .lock()
                .unwrap()
                .2
                .get(r as usize)
                .filter(|x| x.0 == f)
                .map(|x| x.1.clone())
                .ok_or_else(|| Error::NotFound("row".into()))
        }
        fn set_source_priority(&self, p: &SourcePriority) -> Result<()> {
            let mut s = self.0.lock().unwrap();
            s.3.retain(|x| x.path != p.path);
            s.3.push(p.clone());
            Ok(())
        }
        fn source_priority(&self, p: &str) -> Result<Option<SourcePriority>> {
            Ok(self
                .0
                .lock()
                .unwrap()
                .3
                .iter()
                .find(|x| x.path == p)
                .cloned())
        }
    }

    #[test]
    fn catalog_registration_and_urn_remap() {
        let src = Fixture::default();
        let dst = Fixture::default();
        let v = VesselSpec {
            urn: "vessels.urn:mrn:imo:mmsi:367000000".into(),
            name: Some("PV-1".into()),
            mmsi: None,
        };
        assert_eq!(src.register_vessel(&v).unwrap(), 0);
        assert_eq!(src.register_vessel(&v).unwrap(), 0);
        dst.register_vessel(&VesselSpec {
            urn: "vessels.urn:other".into(),
            name: None,
            mmsi: None,
        })
        .unwrap();
        assert_eq!(remap_vessel(&src, &dst, 0).unwrap(), 1);
        let f = FieldSpec {
            id: 999,
            path: "propulsion.port.state".into(),
            agg: None,
            kind: FieldKind::Set,
            units: None,
        };
        let id = src.register_field(&f).unwrap();
        assert_eq!(src.register_field(&f).unwrap(), id);
        let row = src.register_set_value(id, "started").unwrap();
        assert_eq!(src.register_set_value(id, "started").unwrap(), row);
        assert_eq!(src.set_value(id, row).unwrap(), "started");
        let p = SourcePriority {
            path: f.path.clone(),
            sources: vec!["n2k.115".into(), "fallback".into()],
        };
        src.set_source_priority(&p).unwrap();
        assert_eq!(src.source_priority(&f.path).unwrap(), Some(p));
        let mut conflict = f.clone();
        conflict.kind = FieldKind::Bsi { scale: 3 };
        conflict.agg = Some(Agg::Mean);
        let numeric = src.register_field(&conflict).unwrap();
        assert!(src.register_set_value(numeric, "x").is_err());
    }

    #[test]
    fn clear_requires_exclusive_rewrite() {
        let mut r = BucketRecord {
            vessel: 0,
            bucket: 1,
            field: 2,
            value: FieldValue::Clear,
            rewrite: false,
        };
        assert!(validate_clear_records(&[r.clone()]).is_err());
        r.rewrite = true;
        assert!(validate_clear_records(&[r.clone()]).is_ok());
        let mut extra = r.clone();
        extra.value = FieldValue::Present;
        assert!(validate_clear_records(&[r, extra]).is_err());
    }
}

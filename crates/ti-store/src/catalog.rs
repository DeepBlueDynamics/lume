//! Durable, atomic disk catalog for Lume TI.
//!
//! Enforces:
//! - Canonical Signal K vessel URNs starting with "vessels.urn:"
//! - Monotonic dense vessel ordinals and field IDs without reuse
//! - Field identity (path, agg) with strict conflict detection
//! - Field dictionaries for Set fields
//! - Source priorities per path
//! - Atomic JSON persistence via temp file + fsync + rename

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use arrow_array::{
    Array, RecordBatch, StringArray, TimestampSecondArray, UInt32Array, UInt8Array,
};
use serde::{Deserialize, Serialize};
use ti_contracts::{
    paths_schema, vessels_schema, Agg, Catalog, Error, FieldKind, FieldSpec, Result,
    SourcePriority, VesselOrd, VesselSpec,
};

static TMP_COUNTER: AtomicU64 = AtomicU64::new(1);

fn atomic_write_json<T: Serialize>(path: &Path, data: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let counter = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let tmp_path = path.with_extension(format!("tmp.{}.{}", std::process::id(), counter));
    {
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&tmp_path)?;
        let json_bytes = serde_json::to_vec_pretty(data)
            .map_err(|e| Error::Corrupt(format!("json serialization error: {}", e)))?;
        file.write_all(&json_bytes)?;
        file.flush()?;
        file.sync_all()?;
    }
    fs::rename(&tmp_path, path)?;
    Ok(())
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<Option<T>> {
    if !path.exists() {
        return Ok(None);
    }
    let file = File::open(path)?;
    let data = serde_json::from_reader(file)
        .map_err(|e| Error::Corrupt(format!("Failed to parse {}: {}", path.display(), e)))?;
    Ok(Some(data))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VesselRecord {
    pub ord: u32,
    pub urn: String,
    pub name: Option<String>,
    pub mmsi: Option<String>,
    #[serde(default)]
    pub first_seen: i64,
    #[serde(default)]
    pub last_seen: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PathRecord {
    pub path: String,
    pub field: u32,
    pub agg: Option<String>,
    #[serde(rename = "type")]
    pub field_type: String,
    pub units: Option<String>,
    pub scale: Option<u8>,
    pub depth: Option<u8>,
    pub description: Option<String>,
    #[serde(default)]
    pub first_seen: i64,
    #[serde(default)]
    pub last_seen: i64,
    #[serde(default)]
    pub kind: Option<FieldKind>,
}

impl PathRecord {
    pub fn to_field_spec(&self) -> FieldSpec {
        let agg = self.agg.as_deref().and_then(|a| match a {
            "mean" => Some(Agg::Mean),
            "min" => Some(Agg::Min),
            "max" => Some(Agg::Max),
            "last" => Some(Agg::Last),
            "count" => Some(Agg::Count),
            "starts" => Some(Agg::Starts),
            "edges" => Some(Agg::Edges),
            _ => None,
        });

        let kind = self.kind.clone().unwrap_or_else(|| match self.field_type.as_str() {
            "presence" => FieldKind::Presence,
            "set" => FieldKind::Set,
            "bsi" => FieldKind::Bsi {
                scale: self.scale.unwrap_or(3),
            },
            "count" => FieldKind::Count,
            "geo" => FieldKind::Geo {
                res: self.scale.unwrap_or(7),
            },
            _ => FieldKind::Presence,
        });

        FieldSpec {
            id: self.field,
            path: self.path.clone(),
            agg,
            kind,
            units: self.units.clone(),
        }
    }

    pub fn from_field_spec(spec: &FieldSpec) -> Self {
        let agg = spec.agg.as_ref().map(|a| match a {
            Agg::Mean => "mean".to_string(),
            Agg::Min => "min".to_string(),
            Agg::Max => "max".to_string(),
            Agg::Last => "last".to_string(),
            Agg::Count => "count".to_string(),
            Agg::Starts => "starts".to_string(),
            Agg::Edges => "edges".to_string(),
        });

        let (field_type, scale, depth) = match &spec.kind {
            FieldKind::Presence => ("presence".to_string(), None, None),
            FieldKind::Set => ("set".to_string(), None, None),
            FieldKind::Bsi { scale } => ("bsi".to_string(), Some(*scale), Some(16)),
            FieldKind::Count => ("count".to_string(), None, Some(16)),
            FieldKind::Geo { res } => ("geo".to_string(), Some(*res), None),
        };

        Self {
            path: spec.path.clone(),
            field: spec.id,
            agg,
            field_type,
            units: spec.units.clone(),
            scale,
            depth,
            description: None,
            first_seen: 0,
            last_seen: 0,
            kind: Some(spec.kind.clone()),
        }
    }
}

#[derive(Default)]
struct CatalogState {
    vessels: Vec<VesselRecord>,
    fields: Vec<PathRecord>,
    // field_id -> ordered list of values (index is row id)
    dictionaries: BTreeMap<u32, Vec<String>>,
    // path -> list of sources
    source_priorities: BTreeMap<String, Vec<String>>,
}

pub struct DiskCatalog {
    catalog_dir: PathBuf,
    state: Mutex<CatalogState>,
}

impl DiskCatalog {
    /// Open an existing catalog or initialize a new one in `<root>/catalog`.
    pub fn open_or_create(store_root: &Path) -> Result<Self> {
        let catalog_dir = store_root.join("catalog");
        fs::create_dir_all(&catalog_dir)?;

        let vessels: Vec<VesselRecord> =
            read_json(&catalog_dir.join("vessels.json"))?.unwrap_or_default();
        let fields: Vec<PathRecord> =
            read_json(&catalog_dir.join("paths.json"))?.unwrap_or_default();
        let dictionaries: BTreeMap<u32, Vec<String>> =
            read_json(&catalog_dir.join("dictionaries.json"))?.unwrap_or_default();
        let source_priorities: BTreeMap<String, Vec<String>> =
            read_json(&catalog_dir.join("sources.json"))?.unwrap_or_default();

        let state = CatalogState {
            vessels,
            fields,
            dictionaries,
            source_priorities,
        };

        Ok(Self {
            catalog_dir,
            state: Mutex::new(state),
        })
    }

    fn persist_vessels(&self, state: &CatalogState) -> Result<()> {
        atomic_write_json(&self.catalog_dir.join("vessels.json"), &state.vessels)
    }

    fn persist_paths(&self, state: &CatalogState) -> Result<()> {
        atomic_write_json(&self.catalog_dir.join("paths.json"), &state.fields)
    }

    fn persist_dictionaries(&self, state: &CatalogState) -> Result<()> {
        atomic_write_json(&self.catalog_dir.join("dictionaries.json"), &state.dictionaries)
    }

    fn persist_sources(&self, state: &CatalogState) -> Result<()> {
        atomic_write_json(&self.catalog_dir.join("sources.json"), &state.source_priorities)
    }

    /// Compute the canonical catalog snapshot hash per spec 14 §82.
    pub fn catalog_hash(&self) -> Result<[u8; 32]> {
        let state = self.state.lock().unwrap();

        #[derive(Serialize)]
        struct CompactSnapshot<'a> {
            dictionaries: BTreeMap<String, &'a [String]>,
            fields: Vec<CompactField<'a>>,
            priorities: &'a BTreeMap<String, Vec<String>>,
            vessels: Vec<&'a str>,
        }

        #[derive(Serialize)]
        struct CompactField<'a> {
            agg: Option<&'a str>,
            id: u32,
            kind: &'a FieldKind,
            path: &'a str,
            units: Option<&'a str>,
        }

        let mut vessel_urns: Vec<&str> = state.vessels.iter().map(|v| v.urn.as_str()).collect();
        vessel_urns.sort();

        let mut sorted_fields: Vec<CompactField> = state
            .fields
            .iter()
            .map(|f| CompactField {
                id: f.field,
                path: &f.path,
                agg: f.agg.as_deref(),
                kind: f.kind.as_ref().unwrap(),
                units: f.units.as_deref(),
            })
            .collect();
        sorted_fields.sort_by_key(|f| f.id);

        let mut dicts = BTreeMap::new();
        for (f_id, rows) in &state.dictionaries {
            dicts.insert(f_id.to_string(), rows.as_slice());
        }

        let snapshot = CompactSnapshot {
            vessels: vessel_urns,
            fields: sorted_fields,
            dictionaries: dicts,
            priorities: &state.source_priorities,
        };

        let json = serde_json::to_vec(&snapshot)
            .map_err(|e| Error::Corrupt(format!("json snapshot failed: {}", e)))?;

        let mut hasher = blake3::Hasher::new();
        hasher.update(b"LumeTI/catalog/v1\0");
        hasher.update(&json);
        Ok(*hasher.finalize().as_bytes())
    }

    /// Materialize the `vessels` catalog table as an Arrow RecordBatch.
    pub fn vessels_record_batch(&self) -> Result<RecordBatch> {
        let state = self.state.lock().unwrap();
        let mut ords = Vec::with_capacity(state.vessels.len());
        let mut urns = Vec::with_capacity(state.vessels.len());
        let mut names: Vec<Option<String>> = Vec::with_capacity(state.vessels.len());
        let mut mmsis: Vec<Option<String>> = Vec::with_capacity(state.vessels.len());
        let mut firsts = Vec::with_capacity(state.vessels.len());
        let mut lasts = Vec::with_capacity(state.vessels.len());

        for v in &state.vessels {
            ords.push(v.ord);
            urns.push(v.urn.clone());
            names.push(v.name.clone());
            mmsis.push(v.mmsi.clone());
            firsts.push(v.first_seen);
            lasts.push(v.last_seen);
        }

        let name_refs: Vec<Option<&str>> = names.iter().map(|n| n.as_deref()).collect();
        let mmsi_refs: Vec<Option<&str>> = mmsis.iter().map(|m| m.as_deref()).collect();

        let columns: Vec<Arc<dyn Array>> = vec![
            Arc::new(UInt32Array::from(ords)),
            Arc::new(StringArray::from(urns)),
            Arc::new(StringArray::from(name_refs)),
            Arc::new(StringArray::from(mmsi_refs)),
            Arc::new(TimestampSecondArray::from(firsts).with_timezone("UTC")),
            Arc::new(TimestampSecondArray::from(lasts).with_timezone("UTC")),
        ];

        RecordBatch::try_new(vessels_schema(), columns).map_err(Error::Arrow)
    }

    /// Materialize the `paths` catalog table as an Arrow RecordBatch.
    pub fn paths_record_batch(&self) -> Result<RecordBatch> {
        let state = self.state.lock().unwrap();
        let mut paths = Vec::with_capacity(state.fields.len());
        let mut fields = Vec::with_capacity(state.fields.len());
        let mut aggs: Vec<Option<String>> = Vec::with_capacity(state.fields.len());
        let mut types = Vec::with_capacity(state.fields.len());
        let mut units: Vec<Option<String>> = Vec::with_capacity(state.fields.len());
        let mut scales = Vec::with_capacity(state.fields.len());
        let mut depths = Vec::with_capacity(state.fields.len());
        let mut descs: Vec<Option<String>> = Vec::with_capacity(state.fields.len());
        let mut firsts = Vec::with_capacity(state.fields.len());
        let mut lasts = Vec::with_capacity(state.fields.len());

        for f in &state.fields {
            paths.push(f.path.clone());
            fields.push(f.field);
            aggs.push(f.agg.clone());
            types.push(f.field_type.clone());
            units.push(f.units.clone());
            scales.push(f.scale);
            depths.push(f.depth);
            descs.push(f.description.clone());
            firsts.push(f.first_seen);
            lasts.push(f.last_seen);
        }

        let agg_refs: Vec<Option<&str>> = aggs.iter().map(|a| a.as_deref()).collect();
        let unit_refs: Vec<Option<&str>> = units.iter().map(|u| u.as_deref()).collect();
        let desc_refs: Vec<Option<&str>> = descs.iter().map(|d| d.as_deref()).collect();

        let columns: Vec<Arc<dyn Array>> = vec![
            Arc::new(StringArray::from(paths)),
            Arc::new(UInt32Array::from(fields)),
            Arc::new(StringArray::from(agg_refs)),
            Arc::new(StringArray::from(types)),
            Arc::new(StringArray::from(unit_refs)),
            Arc::new(UInt8Array::from(scales)),
            Arc::new(UInt8Array::from(depths)),
            Arc::new(StringArray::from(desc_refs)),
            Arc::new(TimestampSecondArray::from(firsts).with_timezone("UTC")),
            Arc::new(TimestampSecondArray::from(lasts).with_timezone("UTC")),
        ];

        RecordBatch::try_new(paths_schema(), columns).map_err(Error::Arrow)
    }
}

impl Catalog for DiskCatalog {
    fn register_vessel(&self, vessel: &VesselSpec) -> Result<VesselOrd> {
        if !vessel.urn.starts_with("vessels.urn:") {
            return Err(Error::InvalidInput(
                "vessel.urn must be a canonical Signal K URN".into(),
            ));
        }

        let mut state = self.state.lock().unwrap();
        if let Some(existing) = state.vessels.iter().find(|v| v.urn == vessel.urn) {
            return Ok(existing.ord);
        }

        let ord = state.vessels.len() as u32;
        let record = VesselRecord {
            ord,
            urn: vessel.urn.clone(),
            name: vessel.name.clone(),
            mmsi: vessel.mmsi.clone(),
            first_seen: 0,
            last_seen: 0,
        };
        state.vessels.push(record);
        self.persist_vessels(&state)?;
        Ok(ord)
    }

    fn vessel_urn(&self, vessel: VesselOrd) -> Result<String> {
        let state = self.state.lock().unwrap();
        state
            .vessels
            .get(vessel as usize)
            .map(|v| v.urn.clone())
            .ok_or_else(|| Error::NotFound(format!("vessel {}", vessel)))
    }

    fn register_field(&self, field: &FieldSpec) -> Result<u32> {
        if field.path.is_empty() {
            return Err(Error::InvalidInput("field.path must not be empty".into()));
        }

        let mut state = self.state.lock().unwrap();
        if let Some(existing) = state
            .fields
            .iter()
            .find(|f| f.path == field.path && f.to_field_spec().agg == field.agg)
        {
            let existing_spec = existing.to_field_spec();
            if existing_spec.kind != field.kind || existing_spec.units != field.units {
                return Err(Error::InvalidInput("field encoding conflict".into()));
            }
            return Ok(existing.field);
        }

        let mut spec = field.clone();
        spec.id = state.fields.len() as u32;
        let id = spec.id;
        let record = PathRecord::from_field_spec(&spec);
        state.fields.push(record);
        self.persist_paths(&state)?;
        Ok(id)
    }

    fn field(&self, id: u32) -> Result<FieldSpec> {
        let state = self.state.lock().unwrap();
        state
            .fields
            .get(id as usize)
            .map(|f| f.to_field_spec())
            .ok_or_else(|| Error::NotFound(format!("field {}", id)))
    }

    fn fields(&self) -> Result<Vec<FieldSpec>> {
        let state = self.state.lock().unwrap();
        Ok(state.fields.iter().map(|f| f.to_field_spec()).collect())
    }

    fn register_set_value(&self, field: u32, value: &str) -> Result<u32> {
        let f_spec = self.field(field)?;
        if f_spec.kind != FieldKind::Set {
            return Err(Error::InvalidInput("field.kind must be Set".into()));
        }

        let mut state = self.state.lock().unwrap();
        let values = state.dictionaries.entry(field).or_default();
        if let Some(idx) = values.iter().position(|v| v == value) {
            return Ok(idx as u32);
        }

        let row_id = values.len() as u32;
        values.push(value.to_string());
        self.persist_dictionaries(&state)?;
        Ok(row_id)
    }

    fn set_value(&self, field: u32, row: u32) -> Result<String> {
        let state = self.state.lock().unwrap();
        state
            .dictionaries
            .get(&field)
            .and_then(|vals| vals.get(row as usize).cloned())
            .ok_or_else(|| Error::NotFound(format!("row {} for field {}", row, field)))
    }

    fn set_source_priority(&self, priority: &SourcePriority) -> Result<()> {
        let mut state = self.state.lock().unwrap();
        state
            .source_priorities
            .insert(priority.path.clone(), priority.sources.clone());
        self.persist_sources(&state)?;
        Ok(())
    }

    fn source_priority(&self, path: &str) -> Result<Option<SourcePriority>> {
        let state = self.state.lock().unwrap();
        Ok(state
            .source_priorities
            .get(path)
            .map(|sources| SourcePriority {
                path: path.to_string(),
                sources: sources.clone(),
            }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_disk_catalog_lifecycle_and_reopen() {
        let dir = tempdir().unwrap();

        let urn = "vessels.urn:mrn:signalk:uuid:test-boat";
        let cat1 = DiskCatalog::open_or_create(dir.path()).unwrap();

        // Register vessel
        let v0 = cat1
            .register_vessel(&VesselSpec {
                urn: urn.into(),
                name: Some("Test Boat".into()),
                mmsi: Some("123456789".into()),
            })
            .unwrap();
        assert_eq!(v0, 0);
        assert_eq!(cat1.vessel_urn(0).unwrap(), urn);

        // Register same vessel again -> idempotent
        let v0_again = cat1
            .register_vessel(&VesselSpec {
                urn: urn.into(),
                name: None,
                mmsi: None,
            })
            .unwrap();
        assert_eq!(v0, v0_again);

        // Invalid URN rejected
        assert!(cat1
            .register_vessel(&VesselSpec {
                urn: "invalid-urn".into(),
                name: None,
                mmsi: None,
            })
            .is_err());

        // Register fields
        let f0 = cat1
            .register_field(&FieldSpec {
                id: 999, // ignored
                path: "navigation.speedOverGround".into(),
                agg: Some(Agg::Mean),
                kind: FieldKind::Bsi { scale: 3 },
                units: Some("m/s".into()),
            })
            .unwrap();
        assert_eq!(f0, 0);

        let f1 = cat1
            .register_field(&FieldSpec {
                id: 888,
                path: "navigation.state".into(),
                agg: None,
                kind: FieldKind::Set,
                units: None,
            })
            .unwrap();
        assert_eq!(f1, 1);

        // Conflict check
        assert!(cat1
            .register_field(&FieldSpec {
                id: 0,
                path: "navigation.speedOverGround".into(),
                agg: Some(Agg::Mean),
                kind: FieldKind::Bsi { scale: 4 }, // conflict!
                units: Some("m/s".into()),
            })
            .is_err());

        // Set value dictionaries
        let row0 = cat1.register_set_value(f1, "motoring").unwrap();
        assert_eq!(row0, 0);
        let row1 = cat1.register_set_value(f1, "sailing").unwrap();
        assert_eq!(row1, 1);
        assert_eq!(cat1.register_set_value(f1, "motoring").unwrap(), 0);
        assert_eq!(cat1.set_value(f1, 0).unwrap(), "motoring");
        assert_eq!(cat1.set_value(f1, 1).unwrap(), "sailing");

        // Source priorities
        let prio = SourcePriority {
            path: "navigation.speedOverGround".into(),
            sources: vec!["n2k.115".into(), "gps.1".into()],
        };
        cat1.set_source_priority(&prio).unwrap();
        assert_eq!(
            cat1.source_priority("navigation.speedOverGround").unwrap(),
            Some(prio)
        );

        // Check Arrow RecordBatches
        let v_batch = cat1.vessels_record_batch().unwrap();
        assert_eq!(v_batch.num_rows(), 1);
        let p_batch = cat1.paths_record_batch().unwrap();
        assert_eq!(p_batch.num_rows(), 2);

        // Catalog hash
        let hash1 = cat1.catalog_hash().unwrap();
        assert_ne!(hash1, [0u8; 32]);

        drop(cat1);

        // Re-open from disk
        let cat2 = DiskCatalog::open_or_create(dir.path()).unwrap();
        assert_eq!(cat2.vessel_urn(0).unwrap(), urn);
        assert_eq!(cat2.fields().unwrap().len(), 2);
        assert_eq!(cat2.field(0).unwrap().path, "navigation.speedOverGround");
        assert_eq!(cat2.set_value(1, 0).unwrap(), "motoring");
        assert_eq!(cat2.catalog_hash().unwrap(), hash1);
    }
}

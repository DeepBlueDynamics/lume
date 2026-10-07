use datafusion::arrow::datatypes::SchemaRef;
use datafusion::common::{DataFusionError, Result};
use std::collections::BTreeMap;
use std::sync::Arc;
use ti_contracts::{Agg, FieldKind, FieldSpec, VesselOrd};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct VesselInfo {
    pub ord: VesselOrd,
    pub urn: String,
    pub name: Option<String>,
    pub mmsi: Option<String>,
    pub first_seen: i64,
    pub last_seen: i64,
}

/// Immutable planning snapshot. Catalog updates create a new SQL session/provider.
#[derive(Debug, Clone)]
pub struct SqlCatalog {
    pub width_seconds: u64,
    pub fields: Vec<FieldSpec>,
    pub vessels: BTreeMap<VesselOrd, VesselInfo>,
    pub dictionaries: BTreeMap<u32, BTreeMap<u32, String>>,
    pub schema: SchemaRef,
    pub columns: BTreeMap<String, u32>,
}
pub fn field_name(field: &FieldSpec) -> String {
    match &field.agg {
        None => field.path.clone(),
        Some(agg) => format!(
            "{}@{}",
            field.path,
            match agg {
                Agg::Mean => "mean",
                Agg::Min => "min",
                Agg::Max => "max",
                Agg::Last => "last",
                Agg::Count => "count",
                Agg::Starts => "starts",
                Agg::Edges => "edges",
            }
        ),
    }
}
pub fn core_error(e: ti_contracts::Error) -> DataFusionError {
    DataFusionError::External(Box::new(e))
}
impl SqlCatalog {
    pub fn new(
        width_seconds: u64,
        fields: Vec<FieldSpec>,
        vessels: Vec<VesselInfo>,
        mut dictionaries: BTreeMap<u32, BTreeMap<u32, String>>,
    ) -> Result<Arc<Self>> {
        if width_seconds == 0 {
            return Err(DataFusionError::Plan(
                "width_seconds must be positive".into(),
            ));
        }
        let schema = ti_contracts::telemetry_schema(&fields).map_err(core_error)?;
        if schema.index_of("entity").is_ok() {
            return Err(DataFusionError::Plan(
                "metric name entity conflicts with the entity alias".into(),
            ));
        }
        let mut sql_fields = schema.fields().to_vec();
        sql_fields.push(Arc::new(datafusion::arrow::datatypes::Field::new(
            "entity",
            datafusion::arrow::datatypes::DataType::Utf8,
            false,
        )));
        let schema = Arc::new(datafusion::arrow::datatypes::Schema::new(sql_fields));
        let mut columns = BTreeMap::new();
        let mut ids = std::collections::BTreeSet::new();
        for field in &fields {
            if field.kind == FieldKind::Set {
                dictionaries.entry(field.id).or_default();
            }
            if !ids.insert(field.id) {
                return Err(DataFusionError::Plan("duplicate field ID".into()));
            }
            columns.insert(field_name(field), field.id);
            if ti_contracts::mean_alias_enabled(field, &fields) {
                columns.insert(field.path.clone(), field.id);
            }
            if matches!(field.kind,FieldKind::Bsi{scale} if scale>18) {
                return Err(DataFusionError::Plan("BSI scale exceeds 18".into()));
            }
        }
        let mut vessel_map = BTreeMap::new();
        let mut urns = std::collections::BTreeSet::new();
        for vessel in vessels {
            if ti_contracts::validate_entity_urn(&vessel.urn).is_err()
                || !urns.insert(vessel.urn.clone())
                || vessel_map.insert(vessel.ord, vessel).is_some()
            {
                return Err(DataFusionError::Plan(
                    "duplicate/invalid vessel identity".into(),
                ));
            }
        }
        Ok(Arc::new(Self {
            width_seconds,
            fields,
            vessels: vessel_map,
            dictionaries,
            schema,
            columns,
        }))
    }
    pub fn field(&self, name: &str) -> Option<&FieldSpec> {
        self.columns
            .get(name)
            .and_then(|id| self.fields.iter().find(|f| f.id == *id))
    }
    pub fn source_column(&self, name: &str) -> bool {
        self.field(name)
            .is_some_and(|f| f.kind == FieldKind::Set && f.path.ends_with("$source"))
    }
    pub fn row_ids(&self, field: u32, values: &[String]) -> Vec<u32> {
        self.dictionaries
            .get(&field)
            .map(|d| {
                d.iter()
                    .filter_map(|(id, value)| values.contains(value).then_some(*id))
                    .collect()
            })
            .unwrap_or_default()
    }
}

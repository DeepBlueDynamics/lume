use crate::{Error, FieldKind, FieldSpec, Result};
use arrow_schema::{DataType, Field, Schema, SchemaRef, TimeUnit};
use std::collections::BTreeSet;
use std::sync::Arc;

fn timestamp() -> DataType {
    DataType::Timestamp(TimeUnit::Second, Some("UTC".into()))
}
fn list(item: DataType) -> DataType {
    DataType::List(Arc::new(Field::new("item", item, false)))
}
fn schema(fields: Vec<Field>) -> SchemaRef {
    Arc::new(Schema::new(fields))
}

/// Telemetry columns: vessel URN and UTC bucket start, followed by field-ID order.
/// Fields are nullable; source sets are List<Utf8>, numeric BSIs are physical Float64.
/// Mean fields also expose their bare path as an alias. Virtual text-kind columns follow.
pub fn telemetry_schema(fields: &[FieldSpec]) -> Result<SchemaRef> {
    let mut ordered: Vec<_> = fields.iter().collect();
    ordered.sort_by_key(|f| f.id);
    let mut names = BTreeSet::from(["vessel".to_string(), "ts".to_string()]);
    let mut out = vec![
        Field::new("vessel", DataType::Utf8, false),
        Field::new("ts", timestamp(), false),
    ];
    for f in ordered {
        let name = match &f.agg {
            Some(agg) => format!(
                "{}@{}",
                f.path,
                match agg {
                    crate::Agg::Mean => "mean",
                    crate::Agg::Min => "min",
                    crate::Agg::Max => "max",
                    crate::Agg::Last => "last",
                    crate::Agg::Count => "count",
                    crate::Agg::Starts => "starts",
                    crate::Agg::Edges => "edges",
                }
            ),
            None => f.path.clone(),
        };
        if !names.insert(name.clone()) {
            return Err(Error::InvalidInput(format!(
                "telemetry.{name}: duplicate/reserved column"
            )));
        }
        let kind = match &f.kind {
            FieldKind::Presence => DataType::Boolean,
            FieldKind::Set if f.path.ends_with("$source") => list(DataType::Utf8),
            FieldKind::Set => DataType::Utf8,
            FieldKind::Bsi { .. } => DataType::Float64,
            FieldKind::Count => DataType::UInt64,
            FieldKind::Geo { .. } => list(DataType::UInt64),
        };
        out.push(Field::new(name, kind.clone(), true));
        if mean_alias_enabled(f, fields) {
            if !names.insert(f.path.clone()) {
                return Err(Error::InvalidInput(format!(
                    "telemetry.{}: alias collision",
                    f.path
                )));
            }
            out.push(Field::new(&f.path, kind, true));
        }
    }
    for name in ["notes", "logbook", "alerts"] {
        if !names.insert(name.into()) {
            return Err(Error::InvalidInput(format!(
                "telemetry.{name}: reserved text column"
            )));
        }
        out.push(Field::new(name, DataType::Utf8, true));
    }
    Ok(schema(out))
}

/// Explicit bare count fields override the mean alias, including old numeric shards.
pub fn mean_alias_enabled(field: &FieldSpec, fields: &[FieldSpec]) -> bool {
    field.agg == Some(crate::Agg::Mean)
        && !fields
            .iter()
            .any(|f| f.path == field.path && f.agg.is_none() && f.kind == FieldKind::Count)
}

/// Documents handoff: stable ID, vessel URN, kind, time range, title/body and query-only score.
pub fn docs_schema() -> SchemaRef {
    schema(vec![
        Field::new("id", DataType::Utf8, false),
        Field::new("vessel", DataType::Utf8, false),
        Field::new("kind", DataType::Utf8, false),
        Field::new("ts_start", timestamp(), false),
        Field::new("ts_end", timestamp(), true),
        Field::new("title", DataType::Utf8, false),
        Field::new("body", DataType::Utf8, false),
        Field::new("score", DataType::Float64, true),
    ])
}

/// Vessel catalog; ordinal is store-local, urn is federation-stable.
pub fn vessels_schema() -> SchemaRef {
    schema(vec![
        Field::new("ord", DataType::UInt32, false),
        Field::new("urn", DataType::Utf8, false),
        Field::new("name", DataType::Utf8, true),
        Field::new("mmsi", DataType::Utf8, true),
        Field::new("first_seen", timestamp(), false),
        Field::new("last_seen", timestamp(), false),
    ])
}

/// Path catalog; agg/type are readable strings, scale/depth are nullable for categorical fields.
pub fn paths_schema() -> SchemaRef {
    schema(vec![
        Field::new("path", DataType::Utf8, false),
        Field::new("field", DataType::UInt32, false),
        Field::new("agg", DataType::Utf8, true),
        Field::new("type", DataType::Utf8, false),
        Field::new("units", DataType::Utf8, true),
        Field::new("scale", DataType::UInt8, true),
        Field::new("depth", DataType::UInt8, true),
        Field::new("description", DataType::Utf8, true),
        Field::new("first_seen", timestamp(), false),
        Field::new("last_seen", timestamp(), false),
    ])
}

/// Shard catalog; vessel is a URN, hash a lowercase hexadecimal digest when sealed.
pub fn shards_schema() -> SchemaRef {
    schema(vec![
        Field::new("vessel", DataType::Utf8, false),
        Field::new("shard_no", DataType::UInt32, false),
        Field::new("ts_from", timestamp(), false),
        Field::new("ts_to", timestamp(), false),
        Field::new("sealed", DataType::Boolean, false),
        Field::new("bytes", DataType::UInt64, false),
        Field::new("hash", DataType::Utf8, true),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn telemetry_types_order_and_duplicate_rejection() {
        let f = |id, path: &str, kind| FieldSpec {
            id,
            path: path.into(),
            agg: None,
            kind,
            units: None,
        };
        let s = telemetry_schema(&[
            f(2, "speed", FieldKind::Bsi { scale: 3 }),
            f(1, "speed$source", FieldKind::Set),
        ])
        .unwrap();
        assert_eq!(s.field(0).name(), "vessel");
        assert!(!s.field(0).is_nullable());
        assert_eq!(s.field(1).data_type(), &timestamp());
        assert_eq!(s.field(2).name(), "speed$source");
        assert_eq!(s.field(2).data_type(), &list(DataType::Utf8));
        assert_eq!(s.field(3).data_type(), &DataType::Float64);
        assert!(telemetry_schema(&[f(0, "ts", FieldKind::Set)]).is_err());
        assert!(telemetry_schema(&[f(0, "x", FieldKind::Set), f(1, "x", FieldKind::Set)]).is_err());
    }
    #[test]
    fn mean_aliases_and_virtual_text_columns() {
        let s = telemetry_schema(&[FieldSpec {
            id: 0,
            path: "wind".into(),
            agg: Some(crate::Agg::Mean),
            kind: FieldKind::Bsi { scale: 3 },
            units: None,
        }])
        .unwrap();
        assert_eq!(s.field(2).name(), "wind@mean");
        assert_eq!(s.field(3).name(), "wind");
        assert_eq!(s.field(4).name(), "notes");
        assert_eq!(s.field(6).name(), "alerts");
    }
    #[test]
    fn documents_have_nullable_end_and_query_score() {
        let s = docs_schema();
        assert_eq!(s.field(0).name(), "id");
        assert!(!s.field(3).is_nullable());
        assert!(s.field(4).is_nullable());
        assert_eq!(s.field(7).name(), "score");
        assert!(s.field(7).is_nullable());
    }
    #[test]
    fn vessels_have_urn_and_optional_metadata() {
        let s = vessels_schema();
        assert_eq!(s.field(0).data_type(), &DataType::UInt32);
        assert_eq!(s.field(1).name(), "urn");
        assert!(!s.field(1).is_nullable());
        assert!(s.field(2).is_nullable());
    }
    #[test]
    fn paths_preserve_scale_and_description() {
        let s = paths_schema();
        assert_eq!(s.field(5).name(), "scale");
        assert!(s.field(5).is_nullable());
        assert_eq!(s.field(7).name(), "description");
    }
    #[test]
    fn shards_have_coverage_and_nullable_unsealed_hash() {
        let s = shards_schema();
        assert_eq!(s.field(0).name(), "vessel");
        assert_eq!(s.field(2).data_type(), &timestamp());
        assert_eq!(s.field(4).data_type(), &DataType::Boolean);
        assert!(s.field(6).is_nullable());
    }
}

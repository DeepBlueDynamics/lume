//! Column-mapped documents beside generic telemetry.
use crate::mapped_parquet::{expand_files, scalar, time_seconds, MappingReport};
use arrow_array::Array;
use parquet::arrow::{arrow_reader::ParquetRecordBatchReaderBuilder, ProjectionMask};
use std::{collections::BTreeSet, fs::File};
use ti_contracts::{Document, EntityMapping, Error, Result, TimeUnit};

#[derive(Debug, Clone)]
pub struct DocumentsMapping {
    pub files: String,
    pub entity: EntityMapping,
    pub time: String,
    pub time_end: Option<String>,
    pub time_unit: TimeUnit,
    pub timezone: Option<String>,
    pub id: Option<String>,
    pub kind: Option<String>,
    pub kind_constant: String,
    pub title: String,
    pub body: String,
}
fn invalid(message: impl Into<String>) -> Error { Error::InvalidInput(message.into()) }
fn text(array: &dyn Array, row: usize) -> Result<Option<String>> {
    match scalar(array,row)? {
        Some(serde_json::Value::String(value)) => Ok(Some(value)),
        None => Ok(None),
        _ => Err(invalid("mapped document text/entity/id/kind columns must contain strings")),
    }
}
pub fn read(mapping: &DocumentsMapping, mut consume: impl FnMut(Vec<Document>) -> Result<()>) -> Result<MappingReport> {
    let mut report = MappingReport::default();
    for path in expand_files(&mapping.files)? {
        let builder = ParquetRecordBatchReaderBuilder::try_new(File::open(&path)?).map_err(|e|invalid(e.to_string()))?;
        let mut names = BTreeSet::from([mapping.time.clone(),mapping.title.clone(),mapping.body.clone()]);
        if let EntityMapping::Column(name)=&mapping.entity { names.insert(name.clone()); }
        names.extend([mapping.time_end.clone(),mapping.id.clone(),mapping.kind.clone()].into_iter().flatten());
        let columns=names.iter().map(|name|builder.schema().index_of(name).map_err(|_|invalid(format!("missing mapped document column {name}")))).collect::<Result<Vec<_>>>()?;
        let mask=ProjectionMask::roots(builder.parquet_schema(),columns);
        let reader=builder.with_projection(mask).with_batch_size(8192).build().map_err(|e|invalid(e.to_string()))?;
        let qualified=path.canonicalize()?.to_string_lossy().into_owned();
        let mut file_row=0usize;
        report.files+=1;
        for batch in reader {
            let batch=batch?;
            let column=|name:&str|batch.column_by_name(name).map(|a|a.as_ref()).ok_or_else(||invalid(format!("missing projected document column {name}")));
            let mut docs=Vec::new();
            for row in 0..batch.num_rows() {
                report.rows_read+=1;
                file_row+=1;
                let entity=match &mapping.entity {
                    EntityMapping::Column(name)=>text(column(name)?,row)?,
                    EntityMapping::Constant{constant}=>Some(constant.clone()),
                };
                let Some(entity)=entity.filter(|s|!s.is_empty()) else { report.null_entity+=1;continue; };
                ti_contracts::validate_entity_urn(&entity)?;
                let Some(start)=time_seconds(column(&mapping.time)?,row,mapping.time_unit,mapping.timezone.as_deref())? else { report.null_time+=1;continue; };
                let end=mapping.time_end.as_deref().map(|name|time_seconds(column(name)?,row,mapping.time_unit,mapping.timezone.as_deref())).transpose()?.flatten();
                let id=if let Some(name)=&mapping.id {
                    text(column(name)?,row)?.filter(|s|!s.is_empty()).ok_or_else(||invalid("null/empty mapped document id"))?
                } else {
                    format!("parquet:{}:{file_row}",blake3::hash(qualified.as_bytes()).to_hex())
                };
                let kind=if let Some(name)=&mapping.kind {
                    text(column(name)?,row)?.ok_or_else(||invalid("null mapped document kind"))?
                } else { mapping.kind_constant.clone() };
                let doc=Document { id,vessel:entity,kind,ts_start:start,ts_end:end,
                    title:text(column(&mapping.title)?,row)?.unwrap_or_default(),
                    body:text(column(&mapping.body)?,row)?.unwrap_or_default() };
                doc.validate()?;
                docs.push(doc);
                report.points_read+=1;
            }
            consume(docs)?;
        }
    }
    Ok(report)
}

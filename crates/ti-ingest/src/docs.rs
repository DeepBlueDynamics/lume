//! Backfill documents (notes, logbook, alerts) from signalk-parquet `docs/*.parquet`.
//!
//! Enforces:
//! - Columns `context, kind, ts_start, ts_end, title, body`; RFC 3339 times.
//! - Missing `ts_end` is a point document (spec/14); every row passes `Document::validate`.
//! - Rows carry no id, so the id is `<kind>/<ts_start>/<blake3(title, body)[..16]>`:
//!   stable across re-imports, so a backfill re-run replaces rather than duplicates.

use arrow_array::{Array, RecordBatch, StringArray};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use std::path::Path;
use ti_contracts::{Document, Error, Result};

fn text<'a>(batch: &'a RecordBatch, name: &str) -> Result<&'a StringArray> {
    batch
        .column_by_name(name)
        .and_then(|c| c.as_any().downcast_ref::<StringArray>())
        .ok_or_else(|| Error::InvalidInput(format!("docs parquet: Utf8 column {name} required")))
}

fn seconds(value: &str) -> Result<i64> {
    chrono::DateTime::parse_from_rfc3339(value)
        .map(|t| t.timestamp())
        .map_err(|e| Error::InvalidInput(format!("docs parquet: time {value:?}: {e}")))
}

/// Stable document id for a parquet row.
pub fn document_id(kind: &str, ts_start: i64, title: &str, body: &str) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(title.as_bytes());
    hasher.update(&[0]);
    hasher.update(body.as_bytes());
    format!("{kind}/{ts_start}/{}", &hasher.finalize().to_hex()[..16])
}

/// Read one docs parquet file.
pub fn read_docs_file(path: &Path) -> Result<Vec<Document>> {
    let file = std::fs::File::open(path)?;
    let reader = ParquetRecordBatchReaderBuilder::try_new(file)
        .and_then(|b| b.build())
        .map_err(|e| Error::Corrupt(format!("{}: {e}", path.display())))?;
    let mut out = Vec::new();
    for batch in reader {
        let batch = batch.map_err(Error::Arrow)?;
        let (context, kind, start, end, title, body) = (
            text(&batch, "context")?,
            text(&batch, "kind")?,
            text(&batch, "ts_start")?,
            text(&batch, "ts_end")?,
            text(&batch, "title")?,
            text(&batch, "body")?,
        );
        for i in 0..batch.num_rows() {
            if context.is_null(i) || kind.is_null(i) || start.is_null(i) {
                return Err(Error::InvalidInput(format!(
                    "{}: row {i}: context, kind and ts_start are required",
                    path.display()
                )));
            }
            let ts_start = seconds(start.value(i))?;
            let title = if title.is_null(i) { "" } else { title.value(i) };
            let body = if body.is_null(i) { "" } else { body.value(i) };
            let doc = Document {
                id: document_id(kind.value(i), ts_start, title, body),
                vessel: context.value(i).into(),
                kind: kind.value(i).into(),
                ts_start,
                ts_end: (!end.is_null(i))
                    .then(|| seconds(end.value(i)))
                    .transpose()?,
                title: title.into(),
                body: body.into(),
            };
            doc.validate()?;
            out.push(doc);
        }
    }
    Ok(out)
}

/// Read every `*.parquet` file directly under `dir`, in file-name order.
pub fn read_docs_dir(dir: &Path) -> Result<Vec<Document>> {
    let mut files: Vec<_> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "parquet"))
        .collect();
    files.sort();
    let mut out = Vec::new();
    for file in files {
        out.extend(read_docs_file(&file)?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_stable_and_content_addressed() {
        let a = document_id("notes", 1, "Note 0", "leak");
        assert_eq!(a, document_id("notes", 1, "Note 0", "leak"));
        assert_ne!(a, document_id("notes", 1, "Note 0", "water"));
        assert!(a.starts_with("notes/1/"));
    }
}

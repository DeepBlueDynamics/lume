//! Stored-oracle comparison keeps CI independent of the DuckDB executable.
use crate::SqlSession;
use datafusion::{
    arrow::record_batch::RecordBatch,
    common::{DataFusionError, Result},
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::{collections::BTreeMap, path::Path};

#[derive(Debug, Deserialize)]
pub struct Corpus {
    pub version: u32,
    pub bucket_width_seconds: u64,
    pub entries: Vec<GoldenEntry>,
}
#[derive(Debug, Deserialize)]
pub struct GoldenEntry {
    pub id: String,
    pub ti_sql: String,
    pub oracle_sql: String,
    pub expected_path: String,
    pub tolerance: Tolerance,
}
#[derive(Debug, Default, Deserialize)]
pub struct Tolerance {
    #[serde(default)]
    pub sort: Vec<String>,
    #[serde(default)]
    pub exact: Vec<String>,
    #[serde(default)]
    pub bsi: BTreeMap<String, u8>,
}
#[derive(Debug, Serialize)]
pub struct VerifyEntry {
    pub id: String,
    pub status: String,
    pub reason: Option<String>,
    pub rows: Option<usize>,
}
#[derive(Debug, Serialize, Default)]
pub struct VerifyReport {
    pub passed: usize,
    pub failed: usize,
    pub excluded: usize,
    pub entries: Vec<VerifyEntry>,
}
fn error(e: impl std::fmt::Display) -> DataFusionError {
    DataFusionError::Execution(e.to_string())
}
pub fn rows_json(batches: &[RecordBatch]) -> Result<Vec<Map<String, Value>>> {
    let mut result = vec![];
    for batch in batches {
        let mut writer = datafusion::arrow::json::writer::ArrayWriter::new(Vec::new());
        writer.write_batches(&[batch])?;
        writer.finish()?;
        let mut rows: Vec<Map<String, Value>> =
            serde_json::from_slice(&writer.into_inner()).map_err(error)?;
        for row in &mut rows {
            for field in batch.schema().fields() {
                row.entry(field.name().clone()).or_insert(Value::Null);
            }
        }
        result.extend(rows);
    }
    Ok(result)
}
fn normalized(v: &Value) -> Value {
    if let Value::String(s) = v {
        // DuckDB's TIMESTAMP JSON uses a space; Arrow uses ISO T/Z.
        if s.len() >= 19
            && s.as_bytes().get(4) == Some(&b'-')
            && s.as_bytes().get(7) == Some(&b'-')
            && matches!(s.as_bytes().get(10), Some(b'T' | b' '))
        {
            let mut value = s.replace('T', " ");
            for suffix in ["+00:00", "Z"] {
                if value.ends_with(suffix) {
                    value.truncate(value.len() - suffix.len());
                }
            }
            if value.contains('.') {
                while value.ends_with('0') {
                    value.pop();
                }
                if value.ends_with('.') {
                    value.pop();
                }
            }
            return Value::String(value);
        }
    }
    v.clone()
}
fn compare_value(a: &Value, b: &Value) -> std::cmp::Ordering {
    match (a.as_f64(), b.as_f64()) {
        (Some(a), Some(b)) => a.total_cmp(&b),
        _ => normalized(a).to_string().cmp(&normalized(b).to_string()),
    }
}
pub fn diff_rows(
    mut actual: Vec<Map<String, Value>>,
    mut expected: Vec<Map<String, Value>>,
    tolerance: &Tolerance,
) -> Result<()> {
    let sort = |rows: &mut Vec<Map<String, Value>>| {
        rows.sort_by(|a, b| {
            for key in &tolerance.sort {
                let ord = compare_value(
                    a.get(key).unwrap_or(&Value::Null),
                    b.get(key).unwrap_or(&Value::Null),
                );
                if !ord.is_eq() {
                    return ord;
                }
            }
            std::cmp::Ordering::Equal
        })
    };
    sort(&mut actual);
    sort(&mut expected);
    if actual.len() != expected.len() {
        return Err(error(format!(
            "row count: TI {} oracle {}",
            actual.len(),
            expected.len()
        )));
    }
    for (index, (a, b)) in actual.iter().zip(&expected).enumerate() {
        for key in tolerance.exact.iter().chain(tolerance.bsi.keys()) {
            let a = a
                .get(key)
                .ok_or_else(|| error(format!("TI row {index} missing {key}")))?;
            let b = b
                .get(key)
                .ok_or_else(|| error(format!("oracle row {index} missing {key}")))?;
            let equal = if tolerance.exact.contains(key) {
                normalized(a) == normalized(b)
            } else if a.is_null() || b.is_null() {
                a == b
            } else if let (Some(a), Some(b)) = (a.as_f64(), b.as_f64()) {
                let scale = tolerance.bsi[key];
                if scale > 18 {
                    return Err(error("oracle scale exceeds 18"));
                }
                (a - b).abs() <= 0.5 * 10f64.powi(-(scale as i32))
            } else {
                false
            };
            if !equal {
                return Err(error(format!(
                    "row {index} column {key}: TI {a}, oracle {b}"
                )));
            }
        }
    }
    Ok(())
}
pub fn m3_exclusion(sql: &str) -> Option<String> {
    let sql = sql.to_ascii_lowercase();
    for (function, reason) in [
        ("intervals", "M4 intervals"),
        ("match", "M4 text"),
        ("in_bbox", "M4 geo"),
        ("within_nm", "M4 geo"),
    ] {
        // Function token, allowing SQL whitespace before '('.
        let bytes = sql.as_bytes();
        let mut offset = 0;
        while let Some(found) = sql[offset..].find(function) {
            let start = offset + found;
            let end = start + function.len();
            let boundary = start == 0
                || !(bytes[start - 1].is_ascii_alphanumeric() || bytes[start - 1] == b'_');
            if boundary && sql[end..].trim_start().starts_with('(') {
                return Some(reason.into());
            }
            offset = end;
        }
    }
    None
}
pub async fn verify(
    session: &SqlSession,
    corpus_dir: &Path,
    duckdb: Option<&Path>,
    oracle_setup: Option<&str>,
) -> Result<VerifyReport> {
    let corpus: Corpus =
        serde_json::from_slice(&std::fs::read(corpus_dir.join("corpus.json"))?).map_err(error)?;
    if corpus.version != 1 || corpus.bucket_width_seconds != session.catalog.width_seconds {
        return Err(error("unsupported corpus version or bucket-width mismatch"));
    }
    let mut report = VerifyReport::default();
    for entry in corpus.entries {
        if let Some(reason) = m3_exclusion(&entry.ti_sql) {
            report.excluded += 1;
            report.entries.push(VerifyEntry {
                id: entry.id,
                status: "excluded".into(),
                reason: Some(reason),
                rows: None,
            });
            continue;
        }
        let outcome = async {
            let expected: Vec<Map<String, Value>> = if let Some(binary) = duckdb {
                let setup = oracle_setup
                    .ok_or_else(|| error("--oracle duckdb requires oracle setup SQL"))?;
                let output = std::process::Command::new(binary)
                    .args(["-json", "-c", &format!("{setup}\n{}", entry.oracle_sql)])
                    .output()
                    .map_err(error)?;
                if !output.status.success() {
                    return Err(error(String::from_utf8_lossy(&output.stderr)));
                }
                serde_json::from_slice(&output.stdout).map_err(error)?
            } else {
                serde_json::from_slice(&std::fs::read(corpus_dir.join(&entry.expected_path))?)
                    .map_err(error)?
            };
            let actual = rows_json(&session.query(&entry.ti_sql).await?)?;
            let count = actual.len();
            diff_rows(actual, expected, &entry.tolerance)?;
            Ok::<_, DataFusionError>(count)
        }
        .await;
        let (status, reason, rows) = match outcome {
            Ok(rows) => {
                report.passed += 1;
                ("passed", None, Some(rows))
            }
            Err(e) => {
                report.failed += 1;
                ("failed", Some(e.to_string()), None)
            }
        };
        report.entries.push(VerifyEntry {
            id: entry.id,
            status: status.into(),
            reason,
            rows,
        });
    }
    Ok(report)
}

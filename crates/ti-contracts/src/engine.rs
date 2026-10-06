use crate::{RecordBatch, Result};
use arrow_schema::SchemaRef;

/// Shared synchronous fixture facade. Runtime adapters may schedule it asynchronously.
/// Query batches must use one stable schema and physical units; no SQL mutations.
pub trait TiEngine: Send + Sync {
    /// Resolve a table's schema (telemetry, docs, raw, vessels, paths or shards).
    fn schema(&self, table: &str) -> Result<SchemaRef>;
    /// Execute read-only SQL; max_rows applies to the returned rows.
    fn query(&self, sql: &str, max_rows: usize) -> Result<QueryResult>;
    /// Return the query plan and pushdown diagnostics without executing it.
    fn explain(&self, sql: &str) -> Result<String>;
    /// Read operational status.
    fn status(&self) -> Result<EngineStatus>;
}

/// Result shared by MCP/HTTP/CLI fixtures; production streaming may adapt batches.
#[derive(Debug, Clone, PartialEq)]
pub struct QueryResult {
    /// Arrow output batches with the same schema.
    pub batches: Vec<RecordBatch>,
    /// True if the row cap omitted remaining results.
    pub truncated: bool,
    /// Query elapsed milliseconds.
    pub elapsed_ms: u64,
    /// One-line pushdown summary.
    pub pushdown: String,
}

/// Operational counters in the initial engine facade.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineStatus {
    /// Maximum current ingest lag in milliseconds.
    pub ingest_lag_ms: u64,
    /// Bytes in pending WAL files.
    pub wal_bytes: u64,
    /// Number of mutable shards.
    pub open_shards: u64,
    /// Number of sealed shards.
    pub sealed_shards: u64,
    /// Last completed sync per vessel URN, in Unix seconds; None if never.
    pub last_sync: Vec<(String, Option<i64>)>,
}

/// Document ingestion metadata shared by W5 and the docs table.
/// Query score is not stored: it is attached only to query results.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Document {
    /// Stable resource/plugin-qualified ID; updates replace the same ID.
    pub id: String,
    /// Canonical vessel URN.
    pub vessel: String,
    /// notes, logbook or alerts.
    pub kind: String,
    /// Inclusive start Unix second.
    pub ts_start: i64,
    /// Exclusive end, or None for a point document.
    pub ts_end: Option<i64>,
    /// Display title.
    pub title: String,
    /// Searchable document body.
    pub body: String,
}

impl Document {
    /// Check the supported time domain and nonempty interval.
    pub fn validate(&self) -> Result<()> {
        if self.id.is_empty() {
            return Err(crate::Error::InvalidInput("docs.id".into()));
        }
        if !self.vessel.starts_with("vessels.urn:") {
            return Err(crate::Error::InvalidInput("docs.vessel".into()));
        }
        if !["notes", "logbook", "alerts"].contains(&self.kind.as_str()) {
            return Err(crate::Error::InvalidInput("docs.kind".into()));
        }
        if self.ts_start < crate::EPOCH {
            return Err(crate::Error::InvalidInput("docs.ts_start: pre-2020".into()));
        }
        if self.ts_end.is_some_and(|end| end <= self.ts_start) {
            return Err(crate::Error::InvalidInput("docs.ts_end".into()));
        }
        Ok(())
    }
}

/// W5 document handoff; upsert/delete invalidate affected text-cache shards.
pub trait DocumentIndex: crate::TextIndex {
    /// Persist/index a validated document by ID.
    fn upsert(&self, doc: &Document) -> Result<()>;
    /// Delete the vessel's document by ID; missing IDs are idempotent.
    fn delete(&self, vessel: &str, id: &str) -> Result<()>;
    /// Return document rows, with nullable score when q is None.
    fn documents(
        &self,
        vessel: Option<&str>,
        kind: Option<&str>,
        q: Option<&str>,
    ) -> Result<Vec<RecordBatch>>;
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture;
    impl TiEngine for Fixture {
        fn schema(&self, _: &str) -> Result<SchemaRef> {
            Ok(crate::docs_schema())
        }
        fn query(&self, _: &str, _: usize) -> Result<QueryResult> {
            Ok(QueryResult {
                batches: vec![],
                truncated: false,
                elapsed_ms: 0,
                pushdown: "fixture".into(),
            })
        }
        fn explain(&self, _: &str) -> Result<String> {
            Ok("fixture".into())
        }
        fn status(&self) -> Result<EngineStatus> {
            Ok(EngineStatus {
                ingest_lag_ms: 0,
                wal_bytes: 0,
                open_shards: 0,
                sealed_shards: 0,
                last_sync: vec![("vessels.urn:test".into(), None)],
            })
        }
    }
    #[test]
    fn facade_is_object_safe_and_status_is_comparable() {
        let engine: &dyn TiEngine = &Fixture;
        let r = engine.query("SELECT 1", 10).unwrap();
        assert_eq!(r, r.clone());
        let s = engine.status().unwrap();
        assert_eq!(s, s.clone());
        assert_eq!(engine.schema("docs").unwrap(), crate::docs_schema());
    }
    #[test]
    fn document_identity_and_time_validation() {
        let mut d = Document {
            id: "resource:one".into(),
            vessel: "vessels.urn:test".into(),
            kind: "notes".into(),
            ts_start: crate::EPOCH,
            ts_end: None,
            title: "Leak".into(),
            body: "water".into(),
        };
        assert!(d.validate().is_ok());
        assert_eq!(d, d.clone());
        d.ts_end = Some(d.ts_start);
        assert!(d.validate().is_err());
        d.ts_end = Some(d.ts_start + 10);
        assert!(d.validate().is_ok());
    }
}

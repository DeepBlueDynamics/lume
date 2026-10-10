//! Read-only SQL over ordinary Lume indexes, sharing the TI DataFusion session.
use crate::search::{search, LoadedIndex, SearchMode, SearchOptions};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fmt::{Debug, Formatter};
use std::path::Path;
use std::sync::Arc;
use ti_sql::datafusion::{
    arrow::{
        array::{ArrayRef, Float64Array, StringArray, UInt64Array},
        datatypes::{DataType, Field, Schema, SchemaRef},
        record_batch::RecordBatch,
    },
    catalog::{Session, TableProvider},
    common::{DataFusionError, Result, ScalarValue},
    datasource::MemTable,
    logical_expr::{Expr, TableProviderFilterPushDown, TableType},
    physical_plan::ExecutionPlan,
};
fn error(e: impl ToString) -> DataFusionError {
    DataFusionError::Plan(e.to_string())
}
fn strings(v: Vec<Option<String>>) -> ArrayRef {
    Arc::new(StringArray::from(v))
}
fn schema() -> SchemaRef {
    Arc::new(Schema::new(vec![
        Field::new("id", DataType::UInt64, false),
        Field::new("file", DataType::Utf8, true),
        Field::new("title", DataType::Utf8, false),
        Field::new("line", DataType::UInt64, false),
        Field::new("body", DataType::Utf8, false),
        Field::new("score", DataType::Float64, true),
    ]))
}
enum MatchFilter<'a> {
    Positive(&'a str),
    Negative(&'a str),
}

fn match_query(e: &Expr) -> Option<&str> {
    let Expr::ScalarFunction(f) = e else {
        return None;
    };
    if !f.name().eq_ignore_ascii_case("match") || f.args.len() != 2 {
        return None;
    }
    match (&f.args[0], &f.args[1]) {
        (Expr::Column(c), Expr::Literal(ScalarValue::Utf8(Some(q)), _)) if c.name == "body" => {
            Some(q)
        }
        _ => None,
    }
}

fn match_filter(e: &Expr) -> Option<MatchFilter<'_>> {
    match e {
        Expr::Not(inner) => match_query(inner).map(MatchFilter::Negative),
        _ => match_query(e).map(MatchFilter::Positive),
    }
}
pub struct SectionsTable {
    index: Arc<LoadedIndex>,
}
impl Debug for SectionsTable {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str("SectionsTable")
    }
}
pub fn lexical_options(limit: usize) -> SearchOptions {
    SearchOptions {
        mode: SearchMode::LexicalOnly,
        alpha: 0.0,
        limit,
        bm25_params: crate::bm25::Bm25Params::from_env(),
        ..Default::default()
    }
}
#[async_trait]
impl TableProvider for SectionsTable {
    fn schema(&self) -> SchemaRef {
        schema()
    }
    fn table_type(&self) -> TableType {
        TableType::View
    }
    fn supports_filters_pushdown(
        &self,
        filters: &[&Expr],
    ) -> Result<Vec<TableProviderFilterPushDown>> {
        Ok(filters
            .iter()
            .map(|e| {
                if match_filter(e).is_some() {
                    TableProviderFilterPushDown::Exact
                } else {
                    TableProviderFilterPushDown::Unsupported
                }
            })
            .collect())
    }
    async fn scan(
        &self,
        state: &dyn Session,
        projection: Option<&Vec<usize>>,
        filters: &[Expr],
        limit: Option<usize>,
    ) -> Result<Arc<dyn ExecutionPlan>> {
        let match_filters: Vec<_> = filters.iter().filter_map(match_filter).collect();
        let mut positive_queries = Vec::new();
        let mut negative_queries = Vec::new();
        for f in match_filters {
            match f {
                MatchFilter::Positive(q) => positive_queries.push(q),
                MatchFilter::Negative(q) => negative_queries.push(q),
            }
        }
        let mut selected: BTreeMap<usize, Option<f64>> = match positive_queries.first() {
            Some(q) => search(
                &self.index,
                q,
                &lexical_options(self.index.bm25.sections.len()),
            )
            .map_err(error)?
            .hits
            .into_iter()
            .map(|h| (h.section_index, Some(h.score)))
            .collect(),
            None => (0..self.index.bm25.sections.len())
                .map(|i| (i, None))
                .collect(),
        };
        for q in positive_queries.iter().skip(1) {
            let keep: std::collections::BTreeSet<_> = search(
                &self.index,
                q,
                &lexical_options(self.index.bm25.sections.len()),
            )
            .map_err(error)?
            .hits
            .into_iter()
            .map(|h| h.section_index)
            .collect();
            selected.retain(|id, _| keep.contains(id));
        }
        for q in negative_queries {
            let exclude: std::collections::HashSet<_> = search(
                &self.index,
                q,
                &lexical_options(self.index.bm25.sections.len()),
            )
            .map_err(error)?
            .hits
            .into_iter()
            .map(|h| h.section_index)
            .collect();
            selected.retain(|id, _| !exclude.contains(id));
        }
        let ids: Vec<_> = selected.keys().copied().collect();
        let sections: Vec<_> = ids
            .iter()
            .map(|id| &self.index.bm25.sections[*id])
            .collect();
        let full = self.schema();
        // Build only projected text columns: COUNT(*) need not copy the entire book.
        let columns = projection.cloned().unwrap_or_else(|| (0..6).collect());
        let arrays: Vec<ArrayRef> = columns
            .iter()
            .map(|column| match column {
                0 => Arc::new(UInt64Array::from(
                    ids.iter().map(|i| *i as u64).collect::<Vec<_>>(),
                )) as ArrayRef,
                1 => strings(sections.iter().map(|s| s.filename.clone()).collect()),
                2 => strings(sections.iter().map(|s| Some(s.title.clone())).collect()),
                3 => Arc::new(UInt64Array::from(
                    sections
                        .iter()
                        .map(|s| s.line_number as u64)
                        .collect::<Vec<_>>(),
                )),
                4 => strings(sections.iter().map(|s| Some(s.body.clone())).collect()),
                5 => Arc::new(Float64Array::from(
                    ids.iter().map(|i| selected[i]).collect::<Vec<_>>(),
                )),
                _ => unreachable!("validated projection"),
            })
            .collect();
        let projected = Arc::new(full.project(&columns)?);
        let options = ti_sql::datafusion::arrow::record_batch::RecordBatchOptions::new()
            .with_row_count(Some(ids.len()));
        let batch = RecordBatch::try_new_with_options(projected.clone(), arrays, &options)?;
        MemTable::try_new(projected, vec![vec![batch]])?
            .scan(state, None, &[], limit)
            .await
    }
}
pub fn register_index(session: &ti_sql::SqlSession, index: Arc<LoadedIndex>) -> Result<()> {
    session.register_index_table(
        "sections",
        Arc::new(SectionsTable {
            index: index.clone(),
        }),
    )?;
    if let Some(graph) = &index.entity_graph {
        let entity_schema = Arc::new(Schema::new(vec![
            Field::new("entity", DataType::Utf8, false),
            Field::new("doc_count", DataType::UInt64, false),
        ]));
        let batch = RecordBatch::try_new(
            entity_schema.clone(),
            vec![
                strings(graph.nodes.iter().map(|n| Some(n.id.clone())).collect()),
                Arc::new(UInt64Array::from(
                    graph
                        .nodes
                        .iter()
                        .map(|n| n.frequency as u64)
                        .collect::<Vec<_>>(),
                )),
            ],
        )?;
        session.register_index_table(
            "entities",
            Arc::new(MemTable::try_new(entity_schema, vec![vec![batch]])?),
        )?;
        let edge_schema = Arc::new(Schema::new(vec![
            Field::new("a", DataType::Utf8, false),
            Field::new("b", DataType::Utf8, false),
            Field::new("jaccard", DataType::Float64, false),
            Field::new("relatedness", DataType::Float64, false),
        ]));
        let batch = RecordBatch::try_new(
            edge_schema.clone(),
            vec![
                strings(graph.edges.iter().map(|e| Some(e.source.clone())).collect()),
                strings(graph.edges.iter().map(|e| Some(e.target.clone())).collect()),
                Arc::new(Float64Array::from(
                    graph.edges.iter().map(|e| e.similarity).collect::<Vec<_>>(),
                )),
                Arc::new(Float64Array::from(
                    graph
                        .edges
                        .iter()
                        .map(|e| e.relatedness)
                        .collect::<Vec<_>>(),
                )),
            ],
        )?;
        session.register_index_table(
            "entity_edges",
            Arc::new(MemTable::try_new(edge_schema, vec![vec![batch]])?),
        )?;
    }
    Ok(())
}
pub fn register(session: &ti_sql::SqlSession, root: &Path) -> Result<()> {
    register_index(session, Arc::new(LoadedIndex::open(root).map_err(error)?))
}
pub async fn open(root: &Path) -> Result<ti_sql::TiEngine> {
    let session = ti_sql::SqlSession::new(
        Arc::new(ti_core::MemorySource::new()),
        ti_sql::SqlCatalog::new(10, vec![], vec![], Default::default())?,
    )
    .await?;
    register(&session, root)?;
    Ok(ti_sql::TiEngine::from_session(session, root.into()))
}
pub fn definition() -> Value {
    json!({"name":"lume_sql","description":"Read-only SQL over an ordinary Lume index: sections, entities and entity_edges. match(body, q) uses offline lexical BM25. Capped at 500 rows and 64 KiB.",
        "inputSchema":{"type":"object","properties":{"db":{"type":"string"},"sql":{"type":"string"},"max_rows":{"type":"integer","minimum":1,"maximum":500,"default":500}},"required":["sql"],"additionalProperties":false}})
}
pub fn call(args: Value, default_db: &str) -> std::result::Result<String, String> {
    if !args.is_object() {
        return Err("arguments must be an object".into());
    }
    let sql = args
        .get("sql")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .ok_or("sql is required")?;
    let db = args
        .get("db")
        .map(|v| v.as_str().ok_or("db must be a string"))
        .transpose()?
        .unwrap_or(default_db);
    let limit = args
        .get("max_rows")
        .map(|v| {
            v.as_u64()
                .filter(|n| *n > 0)
                .ok_or("max_rows must be positive")
        })
        .transpose()?
        .unwrap_or(500)
        .min(500) as usize;
    let runtime = ti_sql::surface_runtime().map_err(|e| e.to_string())?;
    runtime.block_on(async {
        let engine = open(Path::new(db)).await.map_err(|e| e.to_string())?;
        let reply =
            crate::ti_mcp::dispatch(&engine, "ti_query", &json!({"sql":sql,"max_rows":limit}))
                .await?;
        serde_json::to_string(&reply).map_err(|e| e.to_string())
    })
}
#[derive(Debug, PartialEq, Eq)]
pub struct Args {
    pub db: std::path::PathBuf,
    pub sql: Option<String>,
    pub format: String,
}
pub fn parse(args: &[String]) -> Result<Args> {
    let mut db = None;
    let mut sql = None;
    let mut format = None;
    let mut repl = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--db" | "--format" => {
                let flag = &args[i];
                let value = args
                    .get(i + 1)
                    .filter(|s| !s.starts_with("--"))
                    .ok_or_else(|| error(format!("{flag} requires a value")))?;
                let slot = if flag == "--db" { &mut db } else { &mut format };
                if slot.replace(value.clone()).is_some() {
                    return Err(error(format!("duplicate {flag}")));
                }
                i += 2;
            }
            "repl" if sql.is_none() && !repl => {
                repl = true;
                i += 1;
            }
            s if s.starts_with('-') => return Err(error(format!("unknown option {s}"))),
            s if !repl && sql.is_none() && !s.trim().is_empty() => {
                sql = Some(s.to_string());
                i += 1;
            }
            _ => return Err(error("expected one SQL argument or repl")),
        }
    }
    if !repl && sql.is_none() {
        return Err(error("SQL or repl is required"));
    }
    let format = format.unwrap_or_else(|| "table".into());
    if !["table", "csv", "json"].contains(&format.as_str()) {
        return Err(error("format must be table, csv or json"));
    }
    Ok(Args {
        db: db.ok_or_else(|| error("--db is required"))?.into(),
        sql,
        format,
    })
}
fn render(reply: &Value, format: &str) -> Result<()> {
    match format {
        "json" => println!("{}", serde_json::to_string_pretty(reply).map_err(error)?),
        "csv" => print!("{}", crate::ti_mcp::csv(reply)),
        _ => print!("{}", ti_sql::cli::table(reply)),
    }
    Ok(())
}
pub fn run_cli(args: &[String]) -> Result<()> {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("lume sql --db <index> <sql> [--format table|csv|json]\nlume sql repl --db <index> [--format table|csv|json]");
        return Ok(());
    }
    let args = parse(args)?;
    let runtime = ti_sql::surface_runtime()?;
    let engine = runtime.block_on(open(&args.db))?;
    if let Some(sql) = args.sql {
        return render(
            &runtime.block_on(engine.query(&sql, ti_sql::MAX_ROWS))?,
            &args.format,
        );
    }
    use std::io::{BufRead, Write};
    println!(
        "Lume SQL: {}. End SQL with ';'; .quit to exit.",
        args.db.display()
    );
    let mut buffer = String::new();
    let stdin = std::io::stdin();
    let mut lines = stdin.lock().lines();
    loop {
        print!("{}", if buffer.is_empty() { "sql> " } else { " -> " });
        std::io::stdout().flush()?;
        let Some(line) = lines.next() else {
            break;
        };
        let line = line?;
        if buffer.is_empty() && matches!(line.trim(), ".quit" | ".exit" | ".q") {
            break;
        }
        buffer.push_str(&line);
        buffer.push('\n');
        if !line.trim_end().ends_with(';') {
            continue;
        }
        let sql = std::mem::take(&mut buffer);
        match runtime.block_on(engine.query(sql.trim(), ti_sql::MAX_ROWS)) {
            Ok(reply) => render(&reply, &args.format)?,
            Err(e) => eprintln!("error: {e}"),
        }
    }
    Ok(())
}

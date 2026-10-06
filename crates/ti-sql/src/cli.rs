//! CLI parsing and rendering, shared by the root executable.
use crate::{core_error, DocumentsFactory, TiEngine};
use datafusion::common::{DataFusionError, Result};
use serde_json::Value;
use std::path::PathBuf;

#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    Query(String),
    Explain(String),
    Status,
    ImportDocs(PathBuf),
    Repl,
}
#[derive(Debug, PartialEq, Eq)]
pub struct Args {
    pub command: Command,
    pub store: PathBuf,
    pub width: Option<u64>,
    pub json: bool,
    pub docs_index: Option<PathBuf>,
}
pub const USAGE: &str = "lume ti query <sql> --store <root> [--json] [--width <seconds>] [--docs-index <lume-index>]\nlume ti explain <sql> --store <root> [--json] [--width <seconds>]\nlume ti status --store <root> [--width <seconds>]\nlume ti import-docs <docs_dir> --store <root> [--width <seconds>]
lume ti import-docs --parquet <glob> --entity <column> --time <column> [--time-end <column>] --title <column> --body <column> --store <root>
lume ti repl --store <root> [--width <seconds>]";
fn invalid(message: impl Into<String>) -> DataFusionError {
    DataFusionError::Plan(message.into())
}
pub fn parse(args: &[String]) -> Result<Args> {
    let Some(name) = args.first() else {
        return Err(invalid(USAGE));
    };
    let mut positional = None;
    let mut store = None;
    let mut width = None;
    let mut docs_index = None;
    let mut json = false;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--store" | "--width" | "--docs-index" => {
                let flag = &args[i];
                let value = args
                    .get(i + 1)
                    .filter(|v| !v.starts_with("--"))
                    .ok_or_else(|| invalid(format!("{flag} requires a value")))?;
                if flag == "--docs-index" {
                    if docs_index.replace(PathBuf::from(value)).is_some() {
                        return Err(invalid("duplicate --docs-index"));
                    }
                } else if flag == "--store" {
                    if store.replace(PathBuf::from(value)).is_some() {
                        return Err(invalid("duplicate --store"));
                    }
                } else {
                    let value: u64 = value
                        .parse()
                        .map_err(|_| invalid("width must be positive integer seconds"))?;
                    if value == 0 || width.replace(value).is_some() {
                        return Err(invalid("invalid or duplicate --width"));
                    }
                }
                i += 2;
            }
            "--json" => {
                if json {
                    return Err(invalid("duplicate --json"));
                }
                json = true;
                i += 1;
            }
            value if value.starts_with('-') => {
                return Err(invalid(format!("unknown option {value}")))
            }
            value => {
                if positional.replace(value.to_string()).is_some() {
                    return Err(invalid("expected one positional argument; quote SQL"));
                }
                i += 1;
            }
        }
    }
    let command = match (name.as_str(), positional) {
        ("query", Some(sql)) if !sql.trim().is_empty() => Command::Query(sql),
        ("explain", Some(sql)) if !sql.trim().is_empty() => Command::Explain(sql),
        ("status", None) => Command::Status,
        ("repl", None) => Command::Repl,
        ("import-docs", Some(path)) => Command::ImportDocs(path.into()),
        _ => return Err(invalid(USAGE)),
    };
    Ok(Args {
        command,
        store: store.ok_or_else(|| invalid("--store is required"))?,
        width,
        json,
        docs_index,
    })
}
pub fn table(response: &Value) -> String {
    let columns: Vec<_> = response["columns"]
        .as_array()
        .map(|c| c.iter().filter_map(|v| v["name"].as_str()).collect())
        .unwrap_or_default();
    let cell = |value: &Value| match value {
        Value::String(s) => s.replace(['\n', '\r', '\t'], " "),
        Value::Null => "NULL".into(),
        _ => value.to_string(),
    };
    let mut out = columns.join(" | ");
    out.push('\n');
    out.push_str(
        &columns
            .iter()
            .map(|_| "---")
            .collect::<Vec<_>>()
            .join(" | "),
    );
    out.push('\n');
    if let Some(rows) = response["rows"].as_array() {
        for row in rows {
            out.push_str(
                &columns
                    .iter()
                    .map(|c| cell(&row[*c]))
                    .collect::<Vec<_>>()
                    .join(" | "),
            );
            out.push('\n');
        }
    }
    if response["truncated"] == true {
        out.push_str("Truncated: aggregate results or narrow the time range.\n");
    }
    out
}
pub fn run(args: &[String], documents: Option<&DocumentsFactory>) -> Result<()> {
    run_with_index(args, documents, None)
}
pub type IndexRegistrar = dyn Fn(&crate::SqlSession, &std::path::Path) -> Result<()>;
fn register_index(
    engine: &TiEngine,
    args: &Args,
    registrar: Option<&IndexRegistrar>,
) -> Result<()> {
    if let Some(root) = &args.docs_index {
        registrar.ok_or_else(|| invalid("--docs-index requires an ordinary-index adapter"))?(
            &engine.session,
            root,
        )?;
    }
    Ok(())
}
pub fn run_with_index(
    args: &[String],
    documents: Option<&DocumentsFactory>,
    registrar: Option<&IndexRegistrar>,
) -> Result<()> {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("{USAGE}");
        return Ok(());
    }
    let args = parse(args)?;
    let runtime = tokio::runtime::Runtime::new()?;
    if args.command == Command::Repl {
        return repl(&runtime, &args, documents, registrar);
    }
    let response = runtime.block_on(async {
        // Validate before writes; an existing telemetry store is required.
        let engine = TiEngine::open(&args.store, args.width, documents).await?;
        register_index(&engine, &args, registrar)?;
        match &args.command {
            Command::Query(sql) => engine.query(sql, crate::MAX_ROWS).await,
            Command::Explain(sql) => engine.explain(sql).await,
            Command::Status => engine.status().await,
            Command::Repl => unreachable!("repl returns before the one-shot path"),
            Command::ImportDocs(dir) => {
                let docs = ti_ingest::docs::read_docs_dir(dir).map_err(core_error)?;
                let count = docs.len();
                let mut store = ti_store::DocStore::open(&args.store).map_err(core_error)?;
                store.upsert_all(docs).map_err(core_error)?;
                Ok(serde_json::json!({"imported":count,"documents":store.len(),"store":args.store}))
            }
        }
    })?;
    if matches!(args.command, Command::Query(_)) && !args.json {
        print!("{}", table(&response));
    } else if matches!(args.command, Command::Explain(_)) && !args.json {
        println!("{}", response["plan"].as_str().unwrap_or_default());
    } else {
        println!(
            "{}",
            serde_json::to_string_pretty(&response)
                .map_err(|e| DataFusionError::External(Box::new(e)))?
        );
    }
    Ok(())
}
const REPL_HELP: &str = "SQL ends with ';' and may span lines. Results are capped at 500 rows.
  .tables            list tables and column counts
  .schema [prefix]   list columns (e.g. .schema environment.wind)
  .explain <sql>     show the plan and which filters ran as bitmaps
  .json              toggle JSON output
  .examples          print a few queries to try
  .help              this help
  .quit              exit (also Ctrl-Z / Ctrl-D)";

const REPL_EXAMPLES: &str = "SELECT v.name, count(*) AS buckets FROM telemetry t JOIN vessels v ON t.vessel = v.urn GROUP BY v.name ORDER BY v.name;

SELECT date_bin(INTERVAL '1 day', ts) AS day, max(\"environment.wind.speedTrue@max\") AS gust_ms
FROM telemetry WHERE vessel = 'vessels.urn:mrn:imo:mmsi:367000000'
GROUP BY day ORDER BY day LIMIT 10;

SELECT ts, \"environment.depth.belowTransducer@min\" AS depth_m FROM telemetry
WHERE vessel = 'vessels.urn:mrn:imo:mmsi:367000000' AND \"environment.depth.belowTransducer@min\" < 3
  AND \"navigation.state\" = 'anchored' ORDER BY ts LIMIT 20;

SELECT ts, \"navigation.speedOverGround@max\" AS sog FROM telemetry
WHERE vessel = 'vessels.urn:mrn:imo:mmsi:367000000' AND match(notes, 'leak OR water')
  AND ts >= TIMESTAMP '2026-05-01' ORDER BY ts LIMIT 20;

SELECT kind, title, ts_start, score FROM docs WHERE match(body, 'anchorage') ORDER BY score DESC LIMIT 5;";

/// Interactive SQL over one store opened once (so queries skip the store-open cost).
fn repl(
    runtime: &tokio::runtime::Runtime,
    args: &Args,
    documents: Option<&DocumentsFactory>,
    registrar: Option<&IndexRegistrar>,
) -> Result<()> {
    use std::io::{BufRead, Write};
    let started = std::time::Instant::now();
    let engine = runtime.block_on(TiEngine::open(&args.store, args.width, documents))?;
    register_index(&engine, args, registrar)?;
    let width = engine.session.catalog.width_seconds;
    println!(
        "Lume TI: {} ({} s buckets, {} vessels, {} fields) opened in {:.2} s. Type .help or .examples.",
        args.store.display(),
        width,
        engine.session.catalog.vessels.len(),
        engine.session.catalog.fields.len(),
        started.elapsed().as_secs_f64()
    );
    let mut json = args.json;
    let mut buffer = String::new();
    let stdin = std::io::stdin();
    let mut lines = stdin.lock().lines();
    loop {
        print!("{}", if buffer.is_empty() { "ti> " } else { " -> " });
        std::io::stdout().flush()?;
        let Some(line) = lines.next() else {
            println!();
            return Ok(());
        };
        let line = line?;
        let trimmed = line.trim();
        if buffer.is_empty() && trimmed.starts_with('.') {
            let (command, rest) = trimmed.split_once(' ').unwrap_or((trimmed, ""));
            let outcome = match command {
                ".quit" | ".exit" | ".q" => return Ok(()),
                ".help" => {
                    println!("{REPL_HELP}");
                    Ok(())
                }
                ".examples" => {
                    println!("{REPL_EXAMPLES}");
                    Ok(())
                }
                ".json" => {
                    json = !json;
                    println!("JSON output {}", if json { "on" } else { "off" });
                    Ok(())
                }
                ".tables" | ".schema" => runtime
                    .block_on(engine.schema((!rest.is_empty()).then_some(rest.trim()), None))
                    .map(|schema| {
                        for table in schema["tables"].as_array().into_iter().flatten() {
                            let columns = table["columns"].as_array().cloned().unwrap_or_default();
                            if command == ".tables" {
                                println!(
                                    "{:<12} {} columns",
                                    table["name"].as_str().unwrap_or(""),
                                    columns.len()
                                );
                                continue;
                            }
                            for column in columns {
                                println!(
                                    "{:<10} {:<56} {:<28} {}",
                                    table["name"].as_str().unwrap_or(""),
                                    column["name"].as_str().unwrap_or(""),
                                    column["type"].as_str().unwrap_or(""),
                                    column["units"].as_str().unwrap_or("")
                                );
                            }
                        }
                    }),
                ".explain" => runtime
                    .block_on(engine.explain(rest.trim_end_matches(';')))
                    .map(|plan| {
                        println!("{}", plan["plan"].as_str().unwrap_or_default());
                    }),
                _ => Err(invalid(format!("unknown command {command}; try .help"))),
            };
            if let Err(e) = outcome {
                println!("error: {e}");
            }
            continue;
        }
        if trimmed.is_empty() && buffer.is_empty() {
            continue;
        }
        buffer.push_str(&line);
        buffer.push('\n');
        if !trimmed.ends_with(';') {
            continue;
        }
        let sql = buffer.trim().trim_end_matches(';').to_string();
        buffer.clear();
        let started = std::time::Instant::now();
        match runtime.block_on(engine.query(&sql, crate::MAX_ROWS)) {
            Ok(response) if json => println!(
                "{}",
                serde_json::to_string_pretty(&response).unwrap_or_default()
            ),
            Ok(response) => {
                print!("{}", table(&response));
                println!(
                    "({} rows, {:.3} s)",
                    response["row_count"],
                    started.elapsed().as_secs_f64()
                );
            }
            Err(e) => println!("error: {e}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn args(s: &[&str]) -> Vec<String> {
        s.iter().map(|s| s.to_string()).collect()
    }
    #[test]
    fn parses_all_commands_and_orderings() {
        let query = parse(&args(&[
            "query",
            "SELECT count(*) FROM telemetry",
            "--json",
            "--store",
            "root",
            "--width",
            "60",
        ]))
        .unwrap();
        assert_eq!(
            query,
            Args {
                command: Command::Query("SELECT count(*) FROM telemetry".into()),
                store: "root".into(),
                width: Some(60),
                json: true,
                docs_index: None,
            }
        );
        assert!(matches!(
            parse(&args(&["explain", "--store", "root", "SELECT 1"]))
                .unwrap()
                .command,
            Command::Explain(_)
        ));
        assert_eq!(
            parse(&args(&["status", "--store", "root"]))
                .unwrap()
                .command,
            Command::Status
        );
        assert_eq!(
            parse(&args(&["import-docs", "docs", "--store", "root"]))
                .unwrap()
                .command,
            Command::ImportDocs("docs".into())
        );
    }
    #[test]
    fn rejects_ambiguous_or_missing_arguments() {
        for input in [
            vec![],
            vec!["query"],
            vec!["query", "SELECT 1"],
            vec!["status", "extra", "--store", "root"],
            vec!["status", "--store"],
            vec!["status", "--store", "root", "--unknown"],
            vec!["status", "--store", "root", "--width", "0"],
            vec!["status", "--store", "a", "--store", "b"],
            vec!["query", "SELECT", "1", "--store", "root"],
        ] {
            assert!(parse(&args(&input)).is_err(), "{input:?}");
        }
    }
}

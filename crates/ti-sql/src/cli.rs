//! CLI parsing and rendering, shared by the root executable.
use crate::{core_error, DocumentsFactory, TiEngine};
use datafusion::common::{DataFusionError, Result};
use serde_json::Value;
use std::path::PathBuf;

#[derive(Debug, PartialEq, Eq)]
pub enum Command { Query(String), Explain(String), Status, ImportDocs(PathBuf) }
#[derive(Debug, PartialEq, Eq)]
pub struct Args { pub command: Command, pub store: PathBuf, pub width: Option<u64>, pub json: bool }
pub const USAGE: &str = "lume ti query <sql> --store <root> [--json] [--width <seconds>]\nlume ti explain <sql> --store <root> [--json] [--width <seconds>]\nlume ti status --store <root> [--width <seconds>]\nlume ti import-docs <docs_dir> --store <root> [--width <seconds>]";
fn invalid(message: impl Into<String>) -> DataFusionError { DataFusionError::Plan(message.into()) }
pub fn parse(args: &[String]) -> Result<Args> {
    let Some(name) = args.first() else { return Err(invalid(USAGE)); };
    let mut positional = None;
    let mut store = None;
    let mut width = None;
    let mut json = false;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--store" | "--width" => {
                let flag = &args[i];
                let value = args.get(i+1).filter(|v|!v.starts_with("--")).ok_or_else(||invalid(format!("{flag} requires a value")))?;
                if flag == "--store" {
                    if store.replace(PathBuf::from(value)).is_some() { return Err(invalid("duplicate --store")); }
                } else {
                    let value: u64 = value.parse().map_err(|_|invalid("width must be positive integer seconds"))?;
                    if value == 0 || width.replace(value).is_some() { return Err(invalid("invalid or duplicate --width")); }
                }
                i += 2;
            }
            "--json" => {
                if json { return Err(invalid("duplicate --json")); }
                json = true; i += 1;
            }
            value if value.starts_with('-') => return Err(invalid(format!("unknown option {value}"))),
            value => {
                if positional.replace(value.to_string()).is_some() { return Err(invalid("expected one positional argument; quote SQL")); }
                i += 1;
            }
        }
    }
    let command = match (name.as_str(), positional) {
        ("query",Some(sql)) if !sql.trim().is_empty() => Command::Query(sql),
        ("explain",Some(sql)) if !sql.trim().is_empty() => Command::Explain(sql),
        ("status",None) => Command::Status,
        ("import-docs",Some(path)) => Command::ImportDocs(path.into()),
        _ => return Err(invalid(USAGE)),
    };
    Ok(Args { command, store: store.ok_or_else(||invalid("--store is required"))?, width, json })
}
pub fn table(response: &Value) -> String {
    let columns: Vec<_> = response["columns"].as_array().map(|c|c.iter().filter_map(|v|v["name"].as_str()).collect()).unwrap_or_default();
    let cell = |value: &Value| match value { Value::String(s)=>s.replace(['\n','\r','\t'], " "), Value::Null=>"NULL".into(), _=>value.to_string() };
    let mut out = columns.join(" | ");
    out.push('\n'); out.push_str(&columns.iter().map(|_|"---").collect::<Vec<_>>().join(" | ")); out.push('\n');
    if let Some(rows)=response["rows"].as_array() { for row in rows {
        out.push_str(&columns.iter().map(|c|cell(&row[*c])).collect::<Vec<_>>().join(" | ")); out.push('\n');
    }}
    if response["truncated"] == true { out.push_str("Truncated: aggregate results or narrow the time range.\n"); }
    out
}
pub fn run(args: &[String], documents: Option<&DocumentsFactory>) -> Result<()> {
    if args.iter().any(|a|a=="--help" || a=="-h") { println!("{USAGE}"); return Ok(()); }
    let args=parse(args)?;
    let runtime=tokio::runtime::Runtime::new()?;
    let response=runtime.block_on(async {
        // Validate before writes; an existing telemetry store is required.
        let engine=TiEngine::open(&args.store,args.width,documents).await?;
        match &args.command {
            Command::Query(sql)=>engine.query(sql,crate::MAX_ROWS).await,
            Command::Explain(sql)=>engine.explain(sql).await,
            Command::Status=>engine.status().await,
            Command::ImportDocs(dir)=>{
                let docs=ti_ingest::docs::read_docs_dir(dir).map_err(core_error)?;
                let count=docs.len();
                let mut store=ti_store::DocStore::open(&args.store).map_err(core_error)?;
                store.upsert_all(docs).map_err(core_error)?;
                Ok(serde_json::json!({"imported":count,"documents":store.len(),"store":args.store}))
            }
        }
    })?;
    if matches!(args.command,Command::Query(_)) && !args.json { print!("{}",table(&response)); }
    else if matches!(args.command,Command::Explain(_)) && !args.json { println!("{}",response["plan"].as_str().unwrap_or_default()); }
    else { println!("{}",serde_json::to_string_pretty(&response).map_err(|e|DataFusionError::External(Box::new(e)))?); }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    fn args(s:&[&str])->Vec<String>{s.iter().map(|s|s.to_string()).collect()}
    #[test]
    fn parses_all_commands_and_orderings() {
        let query=parse(&args(&["query","SELECT count(*) FROM telemetry","--json","--store","root","--width","60"])).unwrap();
        assert_eq!(query,Args{command:Command::Query("SELECT count(*) FROM telemetry".into()),store:"root".into(),width:Some(60),json:true});
        assert!(matches!(parse(&args(&["explain","--store","root","SELECT 1"])).unwrap().command,Command::Explain(_)));
        assert_eq!(parse(&args(&["status","--store","root"])).unwrap().command,Command::Status);
        assert_eq!(parse(&args(&["import-docs","docs","--store","root"])).unwrap().command,Command::ImportDocs("docs".into()));
    }
    #[test]
    fn rejects_ambiguous_or_missing_arguments() {
        for input in [vec![],vec!["query"],vec!["query","SELECT 1"],vec!["status","extra","--store","root"],vec!["status","--store"],vec!["status","--store","root","--unknown"],vec!["status","--store","root","--width","0"],vec!["status","--store","a","--store","b"],vec!["query","SELECT","1","--store","root"]] {
            assert!(parse(&args(&input)).is_err(),"{input:?}");
        }
    }
}

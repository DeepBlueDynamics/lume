//! In-process W7 MCP adapter. No CLI subprocesses.
use serde_json::{json, Value};
pub(crate) fn definitions() -> Vec<Value> {
    ["ti_query","ti_schema","ti_explain","ti_status","ti_resolve"].into_iter().map(|name|{
        let mut properties=json!({"store":{"type":"string","description":"Existing TI store root; default configured --ti-store, otherwise TI_STORE_ROOT or ./ti"},
            "width_seconds":{"type":"integer","minimum":1,"description":"Required only for an empty store without ti.toml"}});
        let required=match name {
            "ti_resolve"=>{properties["phrase"]=json!({"type":"string"});properties["vessel"]=json!({"type":"string","description":"Known vessel URN, name or MMSI"});properties["limit"]=json!({"type":"integer","minimum":1,"maximum":500,"default":8});vec!["phrase"]},
            "ti_query"=>{properties["sql"]=json!({"type":"string"});properties["max_rows"]=json!({"type":"integer","minimum":1,"maximum":500,"default":500});properties["format"]=json!({"type":"string","enum":["json","csv","markdown"],"default":"json"});vec!["sql"]},
            "ti_explain"=>{properties["sql"]=json!({"type":"string"});vec!["sql"]},
            "ti_schema"=>{properties["prefix"]=json!({"type":"string"});properties["type"]=json!({"type":"string"});vec![]},
            _=>vec![],
        };
        json!({"name":name,"description":match name {
            "ti_resolve"=>"Resolve a phrase to ranked stored columns using offline Lume BM25, with units and latest values.",
            "ti_query"=>"Read-only TI SQL, capped at 500 rows and 64 KiB; returns units, truncation, timing and pushdown details.",
            "ti_schema"=>"TI tables and columns, units, scales and time coverage.",
            "ti_explain"=>"Read-only TI plan and measured pushdown report.",
            _=>"TI WAL bytes, open/sealed shard counts and available ingestion/sync status."
        },"inputSchema":{"type":"object","properties":properties,"required":required,"additionalProperties":false}})
    }).collect()
}
pub(crate) async fn dispatch(
    engine: &ti_sql::TiEngine,
    name: &str,
    args: &Value,
) -> Result<Value, String> {
    let string = |key: &str| -> Result<Option<&str>, String> {
        args.get(key)
            .map(|v| v.as_str().ok_or_else(|| format!("{key} must be a string")))
            .transpose()
    };
    let sql = || {
        string("sql")?
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| "sql is required".to_string())
    };
    match name {
        "ti_resolve" => crate::ti_resolve::PathsResolver::new(&engine.session.catalog).resolve(engine,args).await,
        "ti_query" => {
            let limit = args
                .get("max_rows")
                .map(|v| {
                    v.as_u64()
                        .filter(|n| *n > 0)
                        .ok_or_else(|| "max_rows must be a positive integer".to_string())
                })
                .transpose()?
                .unwrap_or(500)
                .min(500) as usize;
            let format = string("format")?.unwrap_or("json");
            if !["json", "csv", "markdown"].contains(&format) {
                return Err("format must be json, csv or markdown".into());
            }
            let mut result = engine
                .query(sql()?, limit)
                .await
                .map_err(|e| e.to_string())?;
            result["format"] = json!(format);
            loop {
                if format != "json" {
                    result["data"] = json!(if format == "markdown" {
                        ti_sql::cli::table(&result)
                    } else {
                        csv(&result)
                    });
                }
                let text = serde_json::to_string(&result).map_err(|e| e.to_string())?;
                let envelope = json!({"content":[{"type":"text","text":text}],"isError":false});
                if serde_json::to_vec(&envelope)
                    .map_err(|e| e.to_string())?
                    .len()
                    <= ti_sql::MAX_BYTES - 256
                {
                    break;
                }
                let rows = result["rows"].as_array_mut().ok_or("missing rows")?;
                if rows.pop().is_none() {
                    return Err("query metadata exceeds 64 KiB".into());
                }
                result["truncated"] = json!(true);
                result["hint"] = json!("Aggregate results or narrow the time range.");
                result["row_count"] = json!(result["rows"].as_array().ok_or("missing rows")?.len());
            }
            Ok(result)
        }
        "ti_schema" => engine
            .schema(string("prefix")?, string("type")?)
            .await
            .map_err(|e| e.to_string()),
        "ti_explain" => engine.explain(sql()?).await.map_err(|e| e.to_string()),
        "ti_status" => engine.status().await.map_err(|e| e.to_string()),
        _ => Err(format!("Unknown TI tool: {name}")),
    }
}
fn csv(result: &Value) -> String {
    let quote = |s: &str| format!("\"{}\"", s.replace('"', "\"\""));
    let columns: Vec<_> = result["columns"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| c["name"].as_str())
        .collect();
    let mut out = columns
        .iter()
        .map(|c| quote(c))
        .collect::<Vec<_>>()
        .join(",");
    out.push('\n');
    for row in result["rows"].as_array().into_iter().flatten() {
        out.push_str(
            &columns
                .iter()
                .map(|c| match &row[*c] {
                    Value::Null => String::new(),
                    Value::String(s) => quote(s),
                    v => quote(&v.to_string()),
                })
                .collect::<Vec<_>>()
                .join(","),
        );
        out.push('\n');
    }
    out
}
pub(crate) fn call(name: &str, args: Value) -> Result<String, String> {
    if !args.is_object() {
        return Err("arguments must be an object".into());
    }
    let default = std::env::var("TI_STORE_ROOT").unwrap_or_else(|_| "ti".into());
    let root = args
        .get("store")
        .map(|v| v.as_str().ok_or("store must be a string"))
        .transpose()?
        .unwrap_or(&default);
    let width = args
        .get("width_seconds")
        .map(|v| {
            v.as_u64()
                .filter(|w| *w > 0)
                .ok_or("width_seconds must be positive")
        })
        .transpose()?;
    let factory = |root: &std::path::Path, store: &ti_store::Store, width: u64| {
        Ok(std::sync::Arc::new(crate::ti_text::LumeText::open(
            root,
            store.catalog().clone(),
            width,
        )?)
            as std::sync::Arc<dyn ti_contracts::DocumentIndex>)
    };
    let runtime = ti_sql::surface_runtime().map_err(|e| e.to_string())?;
    let result = runtime.block_on(async {
        let engine = ti_sql::TiEngine::open(std::path::Path::new(root), width, Some(&factory))
            .await
            .map_err(|e| e.to_string())?;
        dispatch(&engine, name, &args).await
    })?;
    serde_json::to_string(&result).map_err(|e| e.to_string())
}
#[cfg(test)]
mod tests {
    use super::*;
    fn engine() -> ti_sql::TiEngine {
        let runtime = ti_sql::surface_runtime().unwrap();
        let catalog = ti_sql::SqlCatalog::new(10, vec![], vec![], Default::default()).unwrap();
        let session = runtime
            .block_on(ti_sql::SqlSession::new(
                std::sync::Arc::new(ti_core::MemorySource::new()),
                catalog,
            ))
            .unwrap();
        ti_sql::TiEngine::from_session(session, std::path::PathBuf::new())
    }
    #[test]
    fn each_tool_has_the_specified_json_shape() {
        let engine = engine();
        let runtime = ti_sql::surface_runtime().unwrap();
        runtime.block_on(async {
            let q = dispatch(&engine, "ti_query", &json!({"sql":"SELECT 42 AS answer"}))
                .await
                .unwrap();
            assert_eq!(q["rows"][0]["answer"], 42);
            assert_eq!(q["row_count"], 1);
            assert_eq!(q["truncated"], false);
            for key in ["columns", "elapsed_ms", "pushdown", "units", "hint"] {
                assert!(q.get(key).is_some());
            }
            let schema = dispatch(&engine, "ti_schema", &json!({})).await.unwrap();
            assert_eq!(schema["tables"].as_array().unwrap().len(), 5);
            assert!(schema.get("time_coverage").is_some());
            assert_eq!(schema["width_seconds"], 10);
            let explain = dispatch(&engine, "ti_explain", &json!({"sql":"SELECT 1"}))
                .await
                .unwrap();
            assert!(explain["plan"]
                .as_str()
                .unwrap()
                .contains("execution details"));
            assert!(explain.get("units").is_some());
            let status = dispatch(&engine, "ti_status", &json!({})).await.unwrap();
            assert_eq!(status["wal_bytes"], 0);
            assert_eq!(status["shards"], json!({"open":0,"sealed":0}));
            assert!(status["ingest_lag_seconds"].is_null());
            assert!(status["vessels"].is_array());
            assert!(status["unavailable"].is_array());
            assert!(
                dispatch(&engine, "ti_query", &json!({"sql":"SELECT 1","max_rows":0}))
                    .await
                    .is_err()
            );
        });
    }
    #[test]
    fn row_and_byte_caps_apply_to_all_formats() {
        let engine = engine();
        let runtime = ti_sql::surface_runtime().unwrap();
        runtime.block_on(async{
            for format in ["json","csv","markdown"]{
                let result=dispatch(&engine,"ti_query",&json!({"sql":"SELECT * FROM generate_series(1, 700)","max_rows":999,"format":format})).await.unwrap();
                assert!(result["row_count"].as_u64().unwrap()<=500);assert_eq!(result["truncated"],true);assert!(serde_json::to_vec(&result).unwrap().len()<=65536);
                let result=dispatch(&engine,"ti_query",&json!({"sql":"SELECT repeat('é', 40000) AS big","format":format})).await.unwrap();
                assert_eq!(result["row_count"],0);assert_eq!(result["truncated"],true);assert!(serde_json::to_vec(&result).unwrap().len()<=65536);
            }
        });
    }
    #[test]
    fn registered_tools_have_required_sql_and_feature_shapes() {
        let tools = definitions();
        assert_eq!(tools.len(), 5);
        for name in ["ti_query", "ti_schema", "ti_explain", "ti_status", "ti_resolve"] {
            assert!(tools.iter().any(|t| t["name"] == name));
        }
        assert_eq!(tools[0]["inputSchema"]["required"], json!(["sql"]));
    }
}

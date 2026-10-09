//! In-process W7 MCP adapter. No CLI subprocesses.
use serde_json::{json, Value};
pub(crate) fn definitions(width: Option<u64>) -> Vec<Value> {
    let guide = data_model_guide(width);
    ["ti_query","ti_schema","ti_explain","ti_status","ti_resolve"].into_iter().map(|name|{
        let mut properties=json!({"store":{"type":"string","description":"Existing TI store root; default configured --ti-store, otherwise TI_STORE_ROOT or ./ti"},
            "width_seconds":{"type":"integer","minimum":1,"description":"Omit for the served store. Stored bucket width, NOT query duration or resolution; only needed for an empty store without ti.toml"}});
        let required=match name {
            "ti_resolve"=>{properties["phrase"]=json!({"type":"string"});properties["vessel"]=json!({"type":"string","description":"Known vessel URN, name or MMSI"});properties["limit"]=json!({"type":"integer","minimum":1,"maximum":500,"default":8});vec!["phrase"]},
            "ti_query"=>{properties["sql"]=json!({"type":"string"});properties["max_rows"]=json!({"type":"integer","minimum":1,"maximum":500,"default":500});properties["format"]=json!({"type":"string","enum":["json","csv","markdown"],"default":"json"});vec!["sql"]},
            "ti_explain"=>{properties["sql"]=json!({"type":"string"});vec!["sql"]},
            "ti_schema"=>{properties["prefix"]=json!({"type":"string","description":"Signal K column prefix or table name; omit to list all columns"});properties["table"]=json!({"type":"string","description":"Table name, e.g. telemetry or docs"});properties["type"]=json!({"type":"string","description":"Arrow column type filter, e.g. Float64; not the word table"});vec![]},
            _=>vec![],
        };
        json!({"name":name,"description":match name {
            "ti_resolve"=>"Resolve a phrase to ranked stored columns using offline Lume BM25, with units and latest values.",
            "ti_query"=>&guide,
            "ti_schema"=>&guide,
            "ti_explain"=>"Read-only TI plan and measured pushdown report.",
            _=>"TI WAL bytes, open/sealed shard counts and available ingestion/sync status."
        },"inputSchema":{"type":"object","properties":properties,"required":required,"additionalProperties":true}})
    }).collect()
}
fn data_model_guide(width: Option<u64>) -> String {
    let width = width
        .map(|w| format!("{w} s"))
        .unwrap_or_else(|| "reported by ti_schema".into());
    format!("Read-only SQL (500 rows, 64 KiB). ti_schema lists tables and columns with types/units; filter by table or column prefix. telemetry is wide: one row per vessel per bucket. Quote Signal K paths: \"environment.depth.belowTransducer@min\"; suffixes @min/@max/@mean/@last; bare = mean, or count for configured count_paths. ts is TIMESTAMP UTC at bucket start; store bucket width: {width}. vessel is a URN such as 'vessels.urn:mrn:imo:mmsi:367000000'; ti_resolve maps names/MMSIs and phrases to paths. docs(kind in notes/alerts/logbook, body, ts_start, ts_end, vessel); use match(body,'words'). Use date_bin(INTERVAL '1 hour', ts) for time buckets. Example: SELECT max(\"environment.depth.belowTransducer@max\") FROM telemetry WHERE vessel='vessels.urn:mrn:imo:mmsi:367000000' AND ts >= TIMESTAMP '2026-05-01 12:00:00' AND ts < TIMESTAMP '2026-05-01 13:00:00'. Omit store/width_seconds for the served store.")
}
pub(crate) fn ignored_arguments(name: &str, args: &Value) -> Vec<String> {
    let extra: &[&str] = match name {
        "ti_query" => &["sql", "max_rows", "format"],
        "ti_schema" => &["prefix", "table", "type"],
        "ti_explain" => &["sql"],
        "ti_resolve" => &["phrase", "vessel", "limit"],
        _ => &[],
    };
    let mut notes = Vec::new();
    for key in args.as_object().into_iter().flat_map(|a| a.keys()) {
        if !["store", "width_seconds"].contains(&key.as_str()) && !extra.contains(&key.as_str()) {
            notes.push(format!(
                "Ignored unknown argument {}",
                key.chars().take(80).collect::<String>()
            ));
        }
    }
    if name == "ti_schema" && args.get("type").and_then(Value::as_str) == Some("table") {
        notes.push("Ignored type=table; use table=<name> to filter tables.".into());
    }
    notes.truncate(4);
    notes
}
pub(crate) fn add_argument_notes(reply: &mut Value, name: &str, args: &Value) {
    let notes = ignored_arguments(name, args);
    if !notes.is_empty() {
        reply["notes"] = json!(notes);
    }
}
pub(crate) async fn dispatch(
    engine: &ti_sql::TiEngine,
    name: &str,
    args: &Value,
) -> Result<Value, String> {
    let mut reply = dispatch_inner(engine, name, args).await?;
    add_argument_notes(&mut reply, name, args);
    Ok(reply)
}
async fn dispatch_inner(
    engine: &ti_sql::TiEngine,
    name: &str,
    args: &Value,
) -> Result<Value, String> {
    if !args.is_object() {
        return Err("arguments must be an object".into());
    }
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
        "ti_resolve" => {
            crate::ti_resolve::PathsResolver::new(&engine.session.catalog)
                .resolve(engine, args)
                .await
        }
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
            let mut result = match engine.query(sql()?, limit).await {
                Ok(result) => result,
                Err(error) => {
                    let mut message = error.to_string();
                    if message.contains("table") && message.contains("not found") {
                        let schema = engine.schema(None, None).await.map_err(|e| e.to_string())?;
                        let tables: Vec<_> = schema["tables"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(|t| t["name"].as_str())
                            .collect();
                        message.push_str(&format!(
                            "; available tables: {}. Call ti_schema for column names and types.",
                            tables.join(", ")
                        ));
                    }
                    let notes = ignored_arguments(name, args);
                    if !notes.is_empty() {
                        message.push_str(&format!("; {}", notes.join("; ")));
                    }
                    return Err(message);
                }
            };
            result["format"] = json!(format);
            add_argument_notes(&mut result, name, args);
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
        "ti_schema" => {
            let prefix = string("table")?.or(string("prefix")?);
            let kind = string("type")?.filter(|k| *k != "table");
            let mut reply = engine
                .schema(prefix, kind)
                .await
                .map_err(|e| e.to_string())?;
            add_argument_notes(&mut reply, name, args);
            Ok(reply)
        }
        "ti_explain" => engine.explain(sql()?).await.map_err(|e| e.to_string()),
        "ti_status" => engine.status().await.map_err(|e| e.to_string()),
        _ => Err(format!("Unknown TI tool: {name}")),
    }
}
pub(crate) fn csv(result: &Value) -> String {
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
        .filter(|s| !s.trim().is_empty())
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
#[path = "ti_questions_test.rs"]
mod questions_tests;
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
            assert!(status["active_alerts"].is_array());
            assert!(status["active_alert_count"].is_number());
            assert!(status["active_alerts_truncated"].is_boolean());
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
        let tools = definitions(Some(10));
        assert_eq!(tools.len(), 5);
        for name in [
            "ti_query",
            "ti_schema",
            "ti_explain",
            "ti_status",
            "ti_resolve",
        ] {
            assert!(tools.iter().any(|t| t["name"] == name));
        }
        assert_eq!(tools[0]["inputSchema"]["required"], json!(["sql"]));
    }
    #[test]
    fn schema_filters_teach_and_ignore_unknown_arguments() {
        let engine = engine();
        let runtime = ti_sql::surface_runtime().unwrap();
        runtime.block_on(async {
            for prefix in ["true_wind", "datafusion.public."] {
                let reply = dispatch(
                    &engine,
                    "ti_schema",
                    &json!({"prefix":prefix,"type":"table","unexpected":true}),
                )
                .await
                .unwrap();
                assert!(reply["hint"]
                    .as_str()
                    .unwrap()
                    .contains(&format!("no columns match prefix {prefix}")));
                assert!(reply["column_count"].as_u64().unwrap() > 0);
                assert!(reply["tables"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|t| t["column_count"].as_u64().unwrap() > 0));
                assert_eq!(reply["notes"].as_array().unwrap().len(), 2);
            }
            for args in [
                json!({"table":"docs"}),
                json!({"prefix":"datafusion.public.docs"}),
            ] {
                let reply = dispatch(&engine, "ti_schema", &args).await.unwrap();
                let docs = reply["tables"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|t| t["name"] == "docs")
                    .unwrap();
                assert!(docs["columns"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|c| c["name"] == "body"
                        && c["type"].is_string()
                        && c.get("units").is_some()));
                assert!(reply["tables"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|t| t["name"] != "docs")
                    .all(|t| t["columns"].as_array().unwrap().is_empty()));
            }
            let all = dispatch(&engine, "ti_schema", &json!({})).await.unwrap();
            assert_eq!(all["truncated"], false);
            assert_eq!(all["matching_column_count"], all["returned_column_count"]);
            let query = dispatch(
                &engine,
                "ti_query",
                &json!({"sql":"SELECT 1 AS n","type":"table"}),
            )
            .await
            .unwrap();
            assert_eq!(query["rows"][0]["n"], 1);
            assert!(query["notes"][0]
                .as_str()
                .unwrap()
                .contains("Ignored unknown argument type"));
            let error = dispatch(
                &engine,
                "ti_query",
                &json!({"sql":"SELECT * FROM true_wind"}),
            )
            .await
            .unwrap_err();
            assert!(
                error.contains("available tables: telemetry, docs, paths, vessels, shards"),
                "{error}"
            );
            assert!(error.contains("Call ti_schema"));
        });
    }
    #[test]
    fn guide_is_compact_and_uses_served_bucket_width() {
        for width in [None, Some(10), Some(1)] {
            for tool in definitions(width)
                .into_iter()
                .filter(|t| t["name"] == "ti_query" || t["name"] == "ti_schema")
            {
                let text = tool["description"].as_str().unwrap();
                assert!(text.chars().count() <= 1200, "{}", text.len());
                for phrase in [
                    "telemetry is wide",
                    "@min/@max/@mean/@last",
                    "TIMESTAMP UTC",
                    "date_bin",
                    "docs(kind",
                    "ti_resolve",
                    "SELECT max",
                ] {
                    assert!(text.contains(phrase), "{phrase}");
                }
                if let Some(width) = width {
                    assert!(text.contains(&format!("store bucket width: {width} s")));
                }
                assert_eq!(tool["inputSchema"]["additionalProperties"], true);
            }
        }
    }
    #[test]
    fn schema_large_catalog_is_capped_with_a_note() {
        let runtime = ti_sql::surface_runtime().unwrap();
        let fields = (1..=300)
            .map(|id| ti_contracts::FieldSpec {
                id,
                path: format!("navigation.test{id}"),
                agg: Some(ti_contracts::Agg::Mean),
                kind: ti_contracts::FieldKind::Bsi { scale: 3 },
                units: Some("m/s".into()),
            })
            .collect();
        let catalog = ti_sql::SqlCatalog::new(10, fields, vec![], Default::default()).unwrap();
        let session = runtime
            .block_on(ti_sql::SqlSession::new(
                std::sync::Arc::new(ti_core::MemorySource::new()),
                catalog,
            ))
            .unwrap();
        let engine = ti_sql::TiEngine::from_session(session, Default::default());
        runtime.block_on(async {
            let reply = dispatch(&engine, "ti_schema", &json!({})).await.unwrap();
            assert_eq!(reply["truncated"], true);
            assert_eq!(reply["returned_column_count"], 256);
            assert!(reply["hint"].as_str().unwrap().contains("narrow prefix"));
            let narrow = dispatch(
                &engine,
                "ti_schema",
                &json!({"prefix":"navigation.test299"}),
            )
            .await
            .unwrap();
            assert_eq!(narrow["truncated"], false);
            assert!(narrow["tables"][0]["columns"]
                .as_array()
                .unwrap()
                .iter()
                .any(|c| c["units"] == "m/s"));
        });
    }
}

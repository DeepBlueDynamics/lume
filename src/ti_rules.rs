//! W10 root wiring: Lume BM25 injection, live observer and historical backfill.
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
};
use ti_contracts::{BucketIx, Catalog, DocumentIndex, ShardSink, ShardSource, TiConfig, VesselOrd};
use ti_sql::rules::{RuleError as DataFusionError, RuleResult as Result};
use ti_store::DocStore;

pub fn index_factory(
    documents: DocStore,
    catalog: Arc<dyn Catalog>,
    width: u64,
) -> ti_contracts::Result<Arc<dyn DocumentIndex>> {
    Ok(Arc::new(crate::ti_text::LumeText::new(
        documents,
        Arc::new(move |vessel| catalog.vessel_urn(vessel)),
        width,
    )))
}

struct RuleObserver {
    root: PathBuf,
    width: u64,
    runtime: ti_sql::SurfaceRuntime,
    runner: ti_sql::rules::RuleRunner,
}
impl ti_ingest::ClosedBucketObserver for RuleObserver {
    fn on_closed(
        &mut self,
        vessel: VesselOrd,
        from: BucketIx,
        to: BucketIx,
    ) -> ti_contracts::Result<()> {
        self.runtime
            .block_on(async {
                let documents = DocStore::open(&self.root)
                    .map_err(|e| DataFusionError::External(Box::new(e)))?;
                let (session, index) =
                    ti_sql::rules::open_session(&self.root, self.width, &index_factory, documents)
                        .await?;
                self.runner
                    .on_closed(&session, index.as_ref(), vessel, from, to)
                    .await?;
                self.runner.save(&self.root)
            })
            .map_err(|e| ti_contracts::Error::InvalidInput(format!("alert rules: {e}")))
    }
}

/// Install on the default bucketer when config.rules is nonempty.
/// The ingest hook publishes telemetry before this observer opens its SQL snapshot.
pub fn observer(
    root: &Path,
    config: &TiConfig,
) -> Result<Box<dyn ti_ingest::ClosedBucketObserver>> {
    let width = config.resolved_stores()["default"]
        .width_seconds()
        .map_err(|e| DataFusionError::External(Box::new(e)))?;
    let runtime = ti_sql::surface_runtime()?;
    let mut runner = ti_sql::rules::RuleRunner::load(root, config.rules.clone())?;
    if runner.is_fresh() && !config.rules.is_empty() {
        let store = ti_store::Store::open_or_create(root, width)
            .map_err(|e| DataFusionError::External(Box::new(e)))?;
        if !store.shards(None, 0, u32::MAX).is_empty() {
            drop(store);
            runtime.block_on(ti_sql::rules::backfill(
                root,
                width,
                config.rules.clone(),
                &index_factory,
                false,
            ))?;
            runner = ti_sql::rules::RuleRunner::load(root, config.rules.clone())?;
        }
    }
    Ok(Box::new(RuleObserver {
        root: root.into(),
        width,
        runtime,
        runner,
    }))
}

/// Backfill every configured store, then evaluate rules over the complete default
/// history. Per-path parquet files are never treated as complete telemetry buckets.
pub fn backfill_directory(
    raw: &Path,
    self_urn: &str,
    config: &TiConfig,
) -> Result<(Vec<ti_ingest::BackfillStatus>, usize)> {
    let mut stores = BTreeMap::new();
    let resolved = config.resolved_stores();
    for (name, store) in &resolved {
        let root = store.resolved_root(&config.store_root, name);
        stores.insert(
            name.clone(),
            ti_store::Store::open_or_create(
                Path::new(&root),
                store
                    .width_seconds()
                    .map_err(|e| DataFusionError::External(Box::new(e)))?,
            )
            .map_err(|e| DataFusionError::External(Box::new(e)))?,
        );
    }
    let catalogs: BTreeMap<_, _> = stores
        .iter()
        .map(|(n, s)| (n.clone(), s.catalog().clone()))
        .collect();
    let catalogs: BTreeMap<String, &dyn Catalog> = catalogs
        .iter()
        .map(|(n, c)| (n.clone(), c.as_ref() as &dyn Catalog))
        .collect();
    let mut sinks: BTreeMap<String, &mut dyn ShardSink> = stores
        .iter_mut()
        .map(|(n, s)| (n.clone(), s as &mut dyn ShardSink))
        .collect();
    let statuses =
        ti_ingest::backfill_directory_stores(raw, self_urn, None, config, &catalogs, &mut sinks)
            .map_err(|e| DataFusionError::External(Box::new(e)))?;
    for sink in sinks.values_mut() {
        sink.flush()
            .map_err(|e| DataFusionError::External(Box::new(e)))?;
    }
    drop(sinks);
    if config.rules.is_empty() {
        return Ok((statuses, 0));
    }
    let root = resolved["default"].resolved_root(&config.store_root, "default");
    let width = resolved["default"]
        .width_seconds()
        .map_err(|e| DataFusionError::External(Box::new(e)))?;
    let alerts = ti_sql::surface_runtime()?.block_on(ti_sql::rules::backfill(
        Path::new(&root),
        width,
        config.rules.clone(),
        &index_factory,
        false,
    ))?;
    Ok((statuses, alerts.len()))
}

pub const USAGE: &str =
    "lume ti rules list --store <root> [--json]\nlume ti rules test <name> --store <root> [--json]";

#[derive(Debug, PartialEq, Eq)]
pub enum RulesCommand {
    List,
    Test(String),
}
pub fn parse(args: &[String]) -> Result<(RulesCommand, ti_sql::cli::Args)> {
    let mut surface = args.to_vec();
    let command = match surface.first().map(String::as_str) {
        Some("list") => RulesCommand::List,
        Some("test") => {
            let name = surface
                .get(1)
                .filter(|s| !s.starts_with('-'))
                .cloned()
                .ok_or_else(|| DataFusionError::Plan(USAGE.into()))?;
            surface.remove(1);
            RulesCommand::Test(name)
        }
        _ => return Err(DataFusionError::Plan(USAGE.into())),
    };
    surface[0] = "status".into();
    Ok((command, ti_sql::cli::parse(&surface)?))
}
pub fn run_cli(args: &[String]) -> Result<()> {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("{USAGE}");
        return Ok(());
    }
    let (command, args) = parse(args)?;
    let config_path = args.store.join("ti.toml");
    let config = TiConfig::from_toml(&std::fs::read_to_string(config_path)?)
        .map_err(|e| DataFusionError::External(Box::new(e)))?;
    let response = match command {
        RulesCommand::List => serde_json::json!({"rules":config.rules}),
        RulesCommand::Test(name) => {
            if !config.rules.iter().any(|r| r.name == name) {
                return Err(DataFusionError::Plan(format!("unknown rule {name:?}")));
            }
            let width = ti_sql::store_width(&args.store, args.width)?;
            let documents = ti_sql::surface_runtime()?.block_on(ti_sql::rules::backfill(
                &args.store,
                width,
                config.rules,
                &index_factory,
                true,
            ))?;
            let prefix = format!("rules/{name}/");
            let documents: Vec<_> = documents.iter().filter(|d| d.id.starts_with(&prefix)).map(|d| {
                serde_json::json!({"id":d.id,"vessel":d.vessel,"kind":d.kind,"ts_start":d.ts_start,
                    "ts_end":d.ts_end,"title":d.title,"body":d.body})
            }).collect();
            serde_json::json!({"name":name,"dry_run":true,"alerts":documents,"alert_count":documents.len()})
        }
    };
    println!(
        "{}",
        serde_json::to_string_pretty(&response)
            .map_err(|e| DataFusionError::External(Box::new(e)))?
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rule_arguments_are_unambiguous() {
        let input = |args: &[&str]| args.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            parse(&input(&["list", "--store", "root"])).unwrap().0,
            RulesCommand::List
        );
        assert_eq!(
            parse(&input(&["test", "battery", "--json", "--store", "root"]))
                .unwrap()
                .0,
            RulesCommand::Test("battery".into())
        );
        for args in [
            vec!["test", "--store", "root"],
            vec!["list", "extra", "--store", "root"],
            vec!["test", "battery"],
        ] {
            assert!(parse(&input(&args)).is_err());
        }
    }
}

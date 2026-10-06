//! W9 generic Parquet backfill CLI and root store wiring.
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};
use ti_contracts::{
    EntityMapping, ParquetFormat, ParquetMapping, ShardSink, ShardSource, TiConfig, TimeUnit,
};
pub use ti_ingest::mapped_parquet::MappingReport;
use ti_sql::rules::{RuleError, RuleResult};

fn external(error: ti_contracts::Error) -> RuleError {
    RuleError::External(Box::new(error))
}
fn invalid(message: impl Into<String>) -> RuleError {
    RuleError::Plan(message.into())
}
pub const USAGE: &str = "lume ti backfill --parquet <glob> --entity <column> --time <column> (--metric <column> --value <column> | --wide) [--time-unit s|ms|us|ns|rfc3339] [--timezone UTC|+HH:MM] [--units units.toml] --store <root>";

#[derive(Debug)]
pub struct Args {
    pub store: PathBuf,
    pub width: Option<u64>,
    pub mapping: Option<ParquetMapping>,
    pub units: Option<PathBuf>,
}
pub fn parse(args: &[String]) -> RuleResult<Args> {
    let mut options = BTreeMap::new();
    let mut wide = false;
    let mut i = 0;
    while i < args.len() {
        let key = &args[i];
        if key == "--wide" {
            if wide {
                return Err(invalid("duplicate --wide"));
            }
            wide = true;
            i += 1;
            continue;
        }
        if ![
            "--parquet",
            "--entity",
            "--entity-constant",
            "--time",
            "--time-unit",
            "--timezone",
            "--metric",
            "--value",
            "--source",
            "--prefix",
            "--exclude",
            "--units",
            "--store",
            "--width",
        ]
        .contains(&key.as_str())
        {
            return Err(invalid(format!("unknown argument {key}; {USAGE}")));
        }
        let value = args
            .get(i + 1)
            .filter(|s| !s.starts_with("--"))
            .ok_or_else(|| invalid(format!("{key} requires a value")))?;
        if options.insert(key.as_str(), value.clone()).is_some() {
            return Err(invalid(format!("duplicate {key}")));
        }
        i += 2;
    }
    let store = options
        .remove("--store")
        .filter(|s| !s.is_empty())
        .ok_or_else(|| invalid("--store required"))?;
    let width = options
        .remove("--width")
        .map(|s| s.parse::<u64>().map_err(|_| invalid("invalid width")))
        .transpose()?;
    let units = options.remove("--units").map(PathBuf::from);
    let mapping = if let Some(files) = options.remove("--parquet") {
        let entity = match (
            options.remove("--entity"),
            options.remove("--entity-constant"),
        ) {
            (Some(column), None) => EntityMapping::Column(column),
            (None, Some(constant)) => EntityMapping::Constant { constant },
            _ => {
                return Err(invalid(
                    "require exactly one of --entity or --entity-constant",
                ))
            }
        };
        let time = options
            .remove("--time")
            .ok_or_else(|| invalid("--time required"))?;
        let time_unit = match options.remove("--time-unit").as_deref().unwrap_or("s") {
            "s" => TimeUnit::Seconds,
            "ms" => TimeUnit::Milliseconds,
            "us" => TimeUnit::Microseconds,
            "ns" => TimeUnit::Nanoseconds,
            "rfc3339" => TimeUnit::Rfc3339,
            _ => return Err(invalid("invalid time unit")),
        };
        let mapping = ParquetMapping {
            files,
            entity,
            time,
            time_unit,
            timezone: options.remove("--timezone"),
            format: if wide {
                ParquetFormat::Wide
            } else {
                ParquetFormat::Long
            },
            metric: options.remove("--metric"),
            value: options.remove("--value"),
            source: options.remove("--source"),
            prefix: options.remove("--prefix").unwrap_or_default(),
            exclude: options
                .remove("--exclude")
                .map(|s| s.split(',').map(str::to_owned).collect())
                .unwrap_or_default(),
        };
        mapping.validate().map_err(external)?;
        Some(mapping)
    } else {
        if wide || !options.is_empty() {
            return Err(invalid("--parquet required with mapping flags"));
        }
        None
    };
    if !options.is_empty() {
        return Err(invalid("unused mapping flags"));
    }
    Ok(Args {
        store: store.into(),
        width,
        mapping,
        units,
    })
}
/// Same entrypoint for configured source mappings, CLI and fixtures.
pub fn backfill(config: &TiConfig, mappings: &[ParquetMapping]) -> RuleResult<MappingReport> {
    let resolved = config.resolved_stores();
    let mut stores = BTreeMap::new();
    for (name, settings) in &resolved {
        let root = settings.resolved_root(&config.store_root, name);
        stores.insert(
            name.clone(),
            ti_store::Store::open_or_create(
                Path::new(&root),
                settings.width_seconds().map_err(external)?,
            )
            .map_err(external)?,
        );
    }
    let catalogs: BTreeMap<_, _> = stores
        .iter()
        .map(|(name, store)| (name.clone(), store.catalog().clone()))
        .collect();
    let references = catalogs
        .iter()
        .map(|(name, catalog)| (name.clone(), catalog.as_ref() as &dyn ti_contracts::Catalog))
        .collect();
    let mut sinks = stores
        .iter_mut()
        .map(|(name, store)| (name.clone(), store as &mut dyn ShardSink))
        .collect();
    let report = ti_ingest::mapped_parquet::backfill(mappings, config, &references, &mut sinks)
        .map_err(external)?;
    drop(sinks);
    for (name, store) in &mut stores {
        for key in store.shards(None, 0, u32::MAX) {
            store.seal(key).map_err(external)?;
        }
        let root = PathBuf::from(resolved[name].resolved_root(&config.store_root, name));
        save_report(&root, &report)?;
    }
    if !config.rules.is_empty() {
        let root = resolved["default"].resolved_root(&config.store_root, "default");
        ti_sql::surface_runtime()?.block_on(ti_sql::rules::backfill(
            Path::new(&root),
            resolved["default"].width_seconds().map_err(external)?,
            config.rules.clone(),
            &crate::ti_rules::index_factory,
            false,
        ))?;
    }
    Ok(report)
}
fn save_report(root: &Path, report: &MappingReport) -> RuleResult<()> {
    let temporary = root.join("parquet-import.json.new");
    let mut file = std::fs::File::create(&temporary)?;
    let bytes = serde_json::to_vec_pretty(report).map_err(|e| RuleError::External(Box::new(e)))?;
    std::io::Write::write_all(&mut file, &bytes)?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(temporary, root.join("parquet-import.json"))?;
    Ok(())
}
pub fn run_cli(args: &[String]) -> RuleResult<()> {
    if args.iter().any(|s| s == "--help" || s == "-h") {
        println!("{USAGE}");
        return Ok(());
    }
    let args = parse(args)?;
    let config_path = args.store.join("ti.toml");
    let mut config = if config_path.exists() {
        TiConfig::from_toml(&std::fs::read_to_string(&config_path)?).map_err(external)?
    } else {
        TiConfig::default()
    };
    let requested = if config_path.exists() || !args.store.join("shards").exists() {
        args.width.or(Some(config.width_seconds))
    } else {
        args.width
    };
    let width = ti_sql::store_width(&args.store, requested)?;
    config.store_root = args.store.to_string_lossy().into_owned();
    config.width_seconds = width;
    if let Some(units) = args.units {
        config.units.extend(
            TiConfig::from_toml(&std::fs::read_to_string(units)?)
                .map_err(external)?
                .units,
        );
    }
    config.validate().map_err(external)?;
    let mappings = args
        .mapping
        .map(|m| vec![m])
        .unwrap_or_else(|| config.sources.parquet.clone());
    if mappings.is_empty() {
        return Err(invalid("no Parquet mappings configured"));
    }
    let report = backfill(&config, &mappings)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&report).map_err(|e| RuleError::External(Box::new(e)))?
    );
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn explicit_mapping_arguments() {
        let parse_args =
            |args: &[&str]| parse(&args.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        let wide = parse_args(&[
            "--parquet",
            "*.parquet",
            "--entity",
            "robot",
            "--time",
            "timestamp",
            "--wide",
            "--store",
            "store",
        ])
        .unwrap();
        assert_eq!(wide.mapping.unwrap().format, ParquetFormat::Wide);
        for input in [
            vec!["--store", "store", "--wide"],
            vec![
                "--parquet",
                "x",
                "--entity",
                "id",
                "--time",
                "t",
                "--store",
                "store",
            ],
            vec![
                "--parquet",
                "x",
                "--entity",
                "id",
                "--entity-constant",
                "vessels.urn:x",
                "--time",
                "t",
                "--wide",
                "--store",
                "store",
            ],
        ] {
            assert!(parse_args(&input).is_err());
        }
    }
}

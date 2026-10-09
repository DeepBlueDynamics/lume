//! Bounded operator working memory. This is separate from shard-cache and process RSS.
use datafusion::common::{DataFusionError, Result};
use datafusion::execution::runtime_env::{RuntimeEnv, RuntimeEnvBuilder};
use std::sync::Arc;

const DEFAULT_BYTES: usize = 512 * 1024 * 1024;
pub(crate) const ENV: &str = "LUME_TI_QUERY_MEMORY_BYTES";

fn default_limit(ram_bytes: Option<usize>) -> usize {
    ram_bytes.map_or(DEFAULT_BYTES, |ram| DEFAULT_BYTES.min((ram / 4).max(1)))
}

fn configured_limit(value: Option<&str>, ram_bytes: Option<usize>) -> Result<usize> {
    match value {
        None => Ok(default_limit(ram_bytes)),
        Some(value) => value
            .parse::<usize>()
            .ok()
            .filter(|n| *n > 0)
            .ok_or_else(|| {
                DataFusionError::Configuration(format!("{ENV} must be a positive byte count"))
            }),
    }
}

fn available_ram() -> Option<usize> {
    // Linux physical RAM and container limit; unknown platforms retain the 512 MiB ceiling.
    let physical = std::fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|text| {
            text.lines().find_map(|line| {
                line.strip_prefix("MemTotal:")?
                    .split_whitespace()
                    .next()?
                    .parse::<usize>()
                    .ok()?
                    .checked_mul(1024)
            })
        });
    let container = [
        "/sys/fs/cgroup/memory.max",
        "/sys/fs/cgroup/memory/memory.limit_in_bytes",
    ]
    .into_iter()
    .filter_map(|path| {
        std::fs::read_to_string(path)
            .ok()?
            .trim()
            .parse::<usize>()
            .ok()
    })
    .min();
    match (physical, container) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

pub(crate) fn runtime() -> Result<Arc<RuntimeEnv>> {
    let value = std::env::var(ENV).map(Some).or_else(|error| match error {
        std::env::VarError::NotPresent => Ok(None),
        std::env::VarError::NotUnicode(_) => Err(DataFusionError::Configuration(format!(
            "{ENV} must be a positive byte count"
        ))),
    })?;
    runtime_with_limit(configured_limit(value.as_deref(), available_ram())?)
}

fn runtime_with_limit(bytes: usize) -> Result<Arc<RuntimeEnv>> {
    RuntimeEnvBuilder::new()
        .with_memory_limit(bytes, 1.0)
        .build_arc()
}

#[cfg(test)]
mod tests {
    use super::*;
    use datafusion::arrow::{
        array::Int64Array,
        datatypes::{DataType, Field, Schema},
        record_batch::RecordBatch,
    };
    use datafusion::{
        datasource::MemTable,
        prelude::{SessionConfig, SessionContext},
    };

    #[test]
    fn limits_respect_small_containers_and_reject_invalid_overrides() {
        assert_eq!(
            configured_limit(None, Some(256 * 1024 * 1024)).unwrap(),
            64 * 1024 * 1024
        );
        assert_eq!(
            configured_limit(None, Some(8 * 1024 * 1024 * 1024)).unwrap(),
            DEFAULT_BYTES
        );
        assert_eq!(configured_limit(Some("4096"), None).unwrap(), 4096);
        for value in ["0", "-1", "oops", "184467440737095516160"] {
            assert!(configured_limit(Some(value), None)
                .unwrap_err()
                .to_string()
                .contains(ENV));
        }
    }

    #[tokio::test]
    async fn large_group_by_returns_memory_error_and_releases_reservations() {
        let batch = RecordBatch::try_new(
            Arc::new(Schema::new(vec![Field::new("n", DataType::Int64, false)])),
            vec![Arc::new(Int64Array::from_iter_values(0..100_000))],
        )
        .unwrap();
        for (bytes, succeeds) in [(4096, false), (64 * 1024 * 1024, true)] {
            let runtime = runtime_with_limit(bytes).unwrap();
            let context = SessionContext::new_with_config_rt(
                SessionConfig::new().with_target_partitions(2),
                runtime.clone(),
            );
            context
                .register_table(
                    "generated",
                    Arc::new(MemTable::try_new(batch.schema(), vec![vec![batch.clone()]]).unwrap()),
                )
                .unwrap();
            let result = context
                .sql("SELECT n, count(*) FROM generated GROUP BY n")
                .await
                .unwrap()
                .collect()
                .await;
            if succeeds {
                assert_eq!(
                    result.unwrap().iter().map(|b| b.num_rows()).sum::<usize>(),
                    100_000
                );
            } else {
                let error = result.unwrap_err().to_string();
                assert!(
                    error.contains("memory")
                        || error.contains("Memory")
                        || error.contains("Resources exhausted"),
                    "{error}"
                );
            }
            assert_eq!(runtime.memory_pool.reserved(), 0);
            assert_eq!(
                context
                    .sql("SELECT 1")
                    .await
                    .unwrap()
                    .collect()
                    .await
                    .unwrap()[0]
                    .num_rows(),
                1
            );
        }
    }
}

//! Explicit column mappings for generic Parquet inputs (D38).
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct SourcesConfig { pub parquet: Vec<ParquetMapping> }

/// A string means a column; { constant = 'robots.urn:...' } names one fixed entity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum EntityMapping { Column(String), Constant { constant: String } }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ParquetFormat { Long, Wide }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum TimeUnit {
    #[serde(rename = "s")] #[default] Seconds,
    #[serde(rename = "ms")] Milliseconds,
    #[serde(rename = "us")] Microseconds,
    #[serde(rename = "ns")] Nanoseconds,
    #[serde(rename = "rfc3339")] Rfc3339,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ParquetMapping {
    pub files: String,
    pub entity: EntityMapping,
    pub time: String,
    #[serde(default)] pub time_unit: TimeUnit,
    #[serde(default)] pub timezone: Option<String>,
    pub format: ParquetFormat,
    #[serde(default)] pub metric: Option<String>,
    #[serde(default)] pub value: Option<String>,
    #[serde(default)] pub source: Option<String>,
    #[serde(default)] pub prefix: String,
    #[serde(default)] pub exclude: Vec<String>,
}

/// Exact paths win; otherwise the most specific matching glob wins.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetricUnit { pub unit: String, pub scale: u8 }
pub type MetricUnits = BTreeMap<String, MetricUnit>;

impl ParquetMapping {
    pub fn validate(&self) -> Result<()> {
        let invalid = |s: &str| Error::InvalidInput(format!("sources.parquet: {s}"));
        if self.files.trim().is_empty() || self.time.trim().is_empty() {
            return Err(invalid("files and time must be nonempty"));
        }
        let entity = match &self.entity { EntityMapping::Column(s) => s,
            EntityMapping::Constant { constant } => constant };
        if entity.trim().is_empty() { return Err(invalid("entity must be nonempty")); }
        match self.format {
            ParquetFormat::Long if self.metric.as_ref().is_none_or(|s| s.is_empty())
                || self.value.as_ref().is_none_or(|s| s.is_empty()) =>
                return Err(invalid("long format requires metric and value columns")),
            ParquetFormat::Wide if self.metric.is_some() || self.value.is_some() =>
                return Err(invalid("wide format cannot name metric/value columns")),
            _ => {}
        }
        if self.source.as_ref().is_some_and(|s| s.is_empty())
            || self.exclude.iter().any(|s| s.is_empty()) {
            return Err(invalid("source/exclude columns must be nonempty"));
        }
        if let Some(zone) = &self.timezone {
            if zone != "UTC" && zone != "Z" {
                let bytes = zone.as_bytes();
                let valid = bytes.len() == 6 && matches!(bytes[0], b'+' | b'-')
                    && bytes[3] == b':' && bytes[1..3].iter().all(u8::is_ascii_digit)
                    && bytes[4..6].iter().all(u8::is_ascii_digit)
                    && zone[1..3].parse::<u8>().is_ok_and(|h| h < 24)
                    && zone[4..6].parse::<u8>().is_ok_and(|m| m < 60);
                if !valid { return Err(invalid("timezone requires UTC or +/-HH:MM")); }
            }
        }
        Ok(())
    }
}

pub fn metric_unit<'a>(units: &'a MetricUnits, path: &str) -> Option<&'a MetricUnit> {
    fn matches(pattern: &[u8], value: &[u8]) -> bool {
        if pattern.is_empty() { return value.is_empty(); }
        if pattern[0] == b'*' {
            matches(&pattern[1..], value) || (!value.is_empty() && matches(pattern, &value[1..]))
        } else { !value.is_empty() && (pattern[0] == b'?' || pattern[0] == value[0])
            && matches(&pattern[1..], &value[1..]) }
    }
    units.get(path).or_else(|| units.iter()
        .filter(|(pattern, _)| matches(pattern.as_bytes(), path.as_bytes()))
        .max_by_key(|(pattern, _)| (pattern.bytes().filter(|c| !b"*?".contains(c)).count(),
            std::cmp::Reverse(pattern.as_str())))
        .map(|(_, unit)| unit))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mappings_units_and_literal_paths_are_checked() {
        let config = crate::TiConfig::from_toml(r#"
[[sources.parquet]]
files = 'C:\robots\*.parquet'
entity = { constant = 'vessels.urn:robot-fixture' }
time = 'time'
time_unit = 'ms'
timezone = 'UTC'
format = 'wide'
prefix = 'robot.'
exclude = ['debug']
[units]
'*.current' = { unit = 'A', scale = 2 }
'robot.current' = { unit = 'mA', scale = 1 }
"#).unwrap();
        assert_eq!(config.sources.parquet.len(),1);
        assert_eq!(metric_unit(&config.units,"robot.current").unwrap().scale,1);
        assert_eq!(metric_unit(&config.units,"motor.current").unwrap().scale,2);
        assert!(metric_unit(&config.units,"motor.speed").is_none());
        assert!(crate::TiConfig::from_toml("[units]\nx = {unit='A',scale=19}").is_err());
        for zone in ["Europe/Paris","+24:00","+01:60"] {
            let mut mapping = config.sources.parquet[0].clone();
            mapping.timezone = Some(zone.into());
            assert!(mapping.validate().is_err());
        }
    }
}

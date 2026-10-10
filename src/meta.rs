use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use serde::{Deserialize, Serialize};

use crate::fast_retrieval::MiniRoaring;

// ─── Field Types ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FieldType {
    #[serde(rename = "keyword")]
    Keyword,
    #[serde(rename = "keyword[]")]
    KeywordList,
    #[serde(rename = "integer")]
    Integer,
    #[serde(rename = "float")]
    Float,
    #[serde(rename = "date")]
    Date,
}

impl FieldType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Keyword => "keyword",
            Self::KeywordList => "keyword[]",
            Self::Integer => "integer",
            Self::Float => "float",
            Self::Date => "date",
        }
    }

    pub fn from_str_opt(s: &str) -> Option<Self> {
        match s {
            "keyword" => Some(Self::Keyword),
            "keyword[]" => Some(Self::KeywordList),
            "integer" => Some(Self::Integer),
            "float" => Some(Self::Float),
            "date" => Some(Self::Date),
            _ => None,
        }
    }
}

// ─── Date Parsing (Howard Hinnant's civil calendar algorithm) ───────────────

/// Converts year, month [1..12], day [1..31] into days since 1970-01-01 (Unix epoch).
pub fn days_from_civil(y: i64, m: u64, d: u64) -> i64 {
    let y = y - if m <= 2 { 1 } else { 0 };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u64; // [0, 399]
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146097 + (doe as i64) - 719468
}

/// Parses an RFC 3339 or YYYY-MM-DD date string into epoch milliseconds.
pub fn parse_date_to_epoch_ms(s: &str) -> Option<i64> {
    let s = s.trim();
    if s.len() < 10 {
        return None;
    }
    let bytes = s.as_bytes();
    if bytes[4] != b'-' || bytes[7] != b'-' {
        return None;
    }
    let year: i64 = s[0..4].parse().ok()?;
    let month: u64 = s[5..7].parse().ok()?;
    let day: u64 = s[8..10].parse().ok()?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }

    let mut hour: i64 = 0;
    let mut min: i64 = 0;
    let mut sec: i64 = 0;
    let mut millis: i64 = 0;
    let mut offset_ms: i64 = 0;

    let rem = &s[10..];
    if !rem.is_empty() {
        let (sep, rem) = rem.split_at(1);
        if sep != "T" && sep != "t" && sep != " " {
            return None;
        }
        if rem.len() < 8 {
            return None;
        }
        let rem_bytes = rem.as_bytes();
        if rem_bytes[2] != b':' || rem_bytes[5] != b':' {
            return None;
        }
        hour = rem[0..2].parse().ok()?;
        min = rem[3..5].parse().ok()?;
        sec = rem[6..8].parse().ok()?;
        if hour > 23 || min > 59 || sec > 60 {
            return None;
        }

        let mut tz_part = &rem[8..];
        if tz_part.starts_with('.') {
            let end_frac = tz_part[1..]
                .find(|c: char| !c.is_ascii_digit())
                .map(|i| i + 1)
                .unwrap_or(tz_part.len());
            let frac_str = &tz_part[1..end_frac];
            if frac_str.is_empty() {
                return None;
            }
            let mut padded = frac_str.to_string();
            while padded.len() < 3 {
                padded.push('0');
            }
            millis = padded[0..3].parse().ok()?;
            tz_part = &tz_part[end_frac..];
        }

        if !tz_part.is_empty() {
            if tz_part == "Z" || tz_part == "z" {
                offset_ms = 0;
            } else if tz_part.starts_with('+') || tz_part.starts_with('-') {
                let sign: i64 = if tz_part.starts_with('+') { 1 } else { -1 };
                let off_str = &tz_part[1..];
                let (off_h, off_m) = if off_str.len() == 5 && off_str.as_bytes()[2] == b':' {
                    let h: i64 = off_str[0..2].parse().ok()?;
                    let m: i64 = off_str[3..5].parse().ok()?;
                    (h, m)
                } else if off_str.len() == 4 {
                    let h: i64 = off_str[0..2].parse().ok()?;
                    let m: i64 = off_str[2..4].parse().ok()?;
                    (h, m)
                } else if off_str.len() == 2 {
                    let h: i64 = off_str[0..2].parse().ok()?;
                    (h, 0)
                } else {
                    return None;
                };
                if off_h > 23 || off_m > 59 {
                    return None;
                }
                offset_ms = sign * (off_h * 3600 + off_m * 60) * 1000;
            } else {
                return None;
            }
        }
    }

    let days = days_from_civil(year, month, day);
    let total_sec = days * 86400 + hour * 3600 + min * 60 + sec;
    let epoch_ms = total_sec * 1000 + millis - offset_ms;
    Some(epoch_ms)
}

// ─── Frontmatter Parser ──────────────────────────────────────────────────────

fn parse_yaml_scalar(raw: &str) -> serde_json::Value {
    let trimmed = raw.trim();
    if (trimmed.starts_with('"') && trimmed.ends_with('"') && trimmed.len() >= 2)
        || (trimmed.starts_with('\'') && trimmed.ends_with('\'') && trimmed.len() >= 2)
    {
        return serde_json::Value::String(trimmed[1..trimmed.len() - 1].to_string());
    }
    if trimmed.eq_ignore_ascii_case("true") {
        return serde_json::Value::Bool(true);
    }
    if trimmed.eq_ignore_ascii_case("false") {
        return serde_json::Value::Bool(false);
    }
    if let Ok(i) = trimmed.parse::<i64>() {
        return serde_json::Value::Number(serde_json::Number::from(i));
    }
    if let Ok(f) = trimmed.parse::<f64>() {
        if let Some(n) = serde_json::Number::from_f64(f) {
            return serde_json::Value::Number(n);
        }
    }
    serde_json::Value::String(trimmed.to_string())
}

/// Parses an inline list: `[a, b, "c, d"]` into `Vec<serde_json::Value>`.
fn parse_yaml_inline_list(raw: &str) -> Vec<serde_json::Value> {
    let trimmed = raw.trim();
    let inner = if trimmed.starts_with('[') && trimmed.ends_with(']') {
        &trimmed[1..trimmed.len() - 1]
    } else {
        trimmed
    };
    if inner.trim().is_empty() {
        return Vec::new();
    }

    let mut items = Vec::new();
    let mut current = String::new();
    let mut in_quote: Option<char> = None;

    for ch in inner.chars() {
        match ch {
            '"' | '\'' => {
                if in_quote == Some(ch) {
                    in_quote = None;
                } else if in_quote.is_none() {
                    in_quote = Some(ch);
                }
                current.push(ch);
            }
            ',' if in_quote.is_none() => {
                let item = current.trim();
                if !item.is_empty() {
                    items.push(parse_yaml_scalar(item));
                }
                current.clear();
            }
            _ => {
                current.push(ch);
            }
        }
    }
    let item = current.trim();
    if !item.is_empty() {
        items.push(parse_yaml_scalar(item));
    }
    items
}

/// Extracts YAML frontmatter from markdown content and blanks out the frontmatter lines,
/// preserving exact line counts and offsets for markdown sectioning.
pub fn extract_and_blank_frontmatter(content: &str) -> (HashMap<String, serde_json::Value>, String) {
    let mut lines: Vec<&str> = content.split('\n').collect();
    if lines.is_empty() {
        return (HashMap::new(), content.to_string());
    }

    // Find opening delimiter: line 0 (or first non-blank line) must be `---`
    let mut start_idx = None;
    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed == "---" {
            start_idx = Some(i);
            break;
        } else {
            break;
        }
    }

    let start_idx = match start_idx {
        Some(idx) => idx,
        None => return (HashMap::new(), content.to_string()),
    };

    // Find closing delimiter: `---` or `...`
    let mut end_idx = None;
    for (i, line) in lines.iter().enumerate().skip(start_idx + 1) {
        let trimmed = line.trim();
        if trimmed == "---" || trimmed == "..." {
            end_idx = Some(i);
            break;
        }
    }

    let end_idx = match end_idx {
        Some(idx) => idx,
        None => return (HashMap::new(), content.to_string()),
    };

    // Parse frontmatter content
    let mut fields = HashMap::new();
    let mut active_list_key: Option<String> = None;

    for line in &lines[start_idx + 1..end_idx] {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        // List item under active list key: `- item`
        if trimmed.starts_with('-') && (trimmed.len() == 1 || trimmed[1..].starts_with(' ') || trimmed[1..].starts_with('\t')) {
            let item_str = trimmed[1..].trim();
            if let Some(ref key) = active_list_key {
                let item_val = parse_yaml_scalar(item_str);
                if let Some(serde_json::Value::Array(ref mut arr)) = fields.get_mut(key) {
                    arr.push(item_val);
                }
            }
            continue;
        }

        // Key-value pair
        if let Some(colon_pos) = trimmed.find(':') {
            let key = trimmed[..colon_pos].trim().to_string();
            let val_part = trimmed[colon_pos + 1..].trim();

            if val_part.is_empty() {
                // Key with multiline list below
                active_list_key = Some(key.clone());
                fields.insert(key, serde_json::Value::Array(Vec::new()));
            } else if val_part.starts_with('[') && val_part.ends_with(']') {
                // Inline list
                active_list_key = None;
                let list = parse_yaml_inline_list(val_part);
                fields.insert(key, serde_json::Value::Array(list));
            } else {
                // Scalar
                active_list_key = None;
                fields.insert(key, parse_yaml_scalar(val_part));
            }
        }
    }

    // Blank out frontmatter lines (replace with empty string before \n)
    for line in &mut lines[start_idx..=end_idx] {
        if line.ends_with('\r') {
            *line = "\r";
        } else {
            *line = "";
        }
    }

    (fields, lines.join("\n"))
}

// ─── Manifest Parsing & Resolution ──────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestRow {
    pub path: String,
    pub fields: HashMap<String, serde_json::Value>,
}

pub fn normalize_rel_path(path_str: &str) -> String {
    let replaced = path_str.replace('\\', "/");
    let trimmed = replaced.trim();
    let stripped = trimmed.strip_prefix("./").unwrap_or(trimmed);
    stripped.strip_prefix('/').unwrap_or(stripped).to_string()
}

pub fn read_manifest(path: &Path) -> Result<Vec<ManifestRow>, String> {
    let file = File::open(path).map_err(|e| format!("Failed to open manifest {}: {}", path.display(), e))?;
    let reader = BufReader::new(file);
    let mut rows = Vec::new();
    for (line_no, line_res) in reader.lines().enumerate() {
        let line = line_res.map_err(|e| format!("Error reading {} line {}: {}", path.display(), line_no + 1, e))?;
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let row: ManifestRow = serde_json::from_str(trimmed).map_err(|e| {
            format!("Invalid JSON in manifest {} line {}: {}", path.display(), line_no + 1, e)
        })?;
        rows.push(row);
    }
    Ok(rows)
}

/// Discovered manifest with its directory path and depth.
#[derive(Debug, Clone)]
pub struct DiscoveredManifest {
    pub path: PathBuf,
    pub dir: PathBuf,
    pub depth: usize,
    pub rows: Vec<ManifestRow>,
}

// ─── Storage Models & On-Disk Format ────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetaFileEntry {
    pub source: String, // "frontmatter" | "manifest"
    pub fields: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeywordColumnOnDisk {
    pub dict: Vec<String>,
    pub ords: Vec<Option<u32>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeywordListColumnOnDisk {
    pub dict: Vec<String>,
    pub offsets: Vec<usize>,
    pub ords: Vec<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NumericColumnOnDisk<T> {
    pub present_runs: Vec<[usize; 2]>,
    pub values: Vec<T>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetaIndexOnDisk {
    pub meta_version: u32,
    pub num_sections: usize,
    pub generation: String,
    pub schema: HashMap<String, String>,
    pub files: HashMap<String, MetaFileEntry>,
    pub columns: HashMap<String, serde_json::Value>,
}

// ─── In-Memory Columns & MetaIndex ──────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum Column {
    Keyword {
        dict: Vec<String>,
        ords: Vec<Option<u32>>,
        bitmaps: Vec<MiniRoaring>,
    },
    KeywordList {
        dict: Vec<String>,
        offsets: Vec<usize>,
        ords: Vec<u32>,
        bitmaps: Vec<MiniRoaring>,
    },
    Integer {
        present_runs: Vec<[usize; 2]>,
        values: Vec<Option<i64>>,
    },
    Float {
        present_runs: Vec<[usize; 2]>,
        values: Vec<Option<f64>>,
    },
    Date {
        present_runs: Vec<[usize; 2]>,
        values: Vec<Option<i64>>,
    },
}

#[derive(Debug, Clone)]
pub struct MetaIndex {
    pub meta_version: u32,
    pub num_sections: usize,
    pub generation: String,
    pub schema: HashMap<String, FieldType>,
    pub files: HashMap<String, MetaFileEntry>,
    pub columns: HashMap<String, Column>,
}

impl MetaIndex {
    pub fn open(path: &Path) -> Result<Self, String> {
        let disk: MetaIndexOnDisk = crate::search::load_json(path)?;
        if disk.meta_version != 1 {
            return Err(format!(
                "Unsupported meta.json version {}; expected version 1",
                disk.meta_version
            ));
        }

        let mut schema = HashMap::new();
        for (col_name, type_str) in &disk.schema {
            let ft = FieldType::from_str_opt(type_str).ok_or_else(|| {
                format!("Unknown field type '{}' for field '{}'", type_str, col_name)
            })?;
            schema.insert(col_name.clone(), ft);
        }

        let mut columns = HashMap::new();
        for (col_name, col_val) in &disk.columns {
            let ft = match schema.get(col_name) {
                Some(ft) => *ft,
                None => continue,
            };

            let col = match ft {
                FieldType::Keyword => {
                    let col_disk: KeywordColumnOnDisk = serde_json::from_value(col_val.clone())
                        .map_err(|e| format!("Failed to parse keyword column '{}': {}", col_name, e))?;
                    let mut postings = vec![Vec::new(); col_disk.dict.len()];
                    for (sec_idx, ord_opt) in col_disk.ords.iter().enumerate() {
                        if let Some(ord) = *ord_opt {
                            if (ord as usize) < postings.len() {
                                postings[ord as usize].push(sec_idx as u32);
                            }
                        }
                    }
                    let bitmaps = postings
                        .iter()
                        .map(|p| MiniRoaring::from_sorted(p))
                        .collect();
                    Column::Keyword {
                        dict: col_disk.dict,
                        ords: col_disk.ords,
                        bitmaps,
                    }
                }
                FieldType::KeywordList => {
                    let col_disk: KeywordListColumnOnDisk = serde_json::from_value(col_val.clone())
                        .map_err(|e| format!("Failed to parse keyword[] column '{}': {}", col_name, e))?;
                    let mut postings = vec![Vec::new(); col_disk.dict.len()];
                    if col_disk.offsets.len() >= 2 {
                        for sec_idx in 0..col_disk.offsets.len() - 1 {
                            let start = col_disk.offsets[sec_idx];
                            let end = col_disk.offsets[sec_idx + 1].min(col_disk.ords.len());
                            for &ord in &col_disk.ords[start..end] {
                                if (ord as usize) < postings.len() {
                                    postings[ord as usize].push(sec_idx as u32);
                                }
                            }
                        }
                    }
                    let bitmaps = postings
                        .iter()
                        .map(|p| MiniRoaring::from_sorted(p))
                        .collect();
                    Column::KeywordList {
                        dict: col_disk.dict,
                        offsets: col_disk.offsets,
                        ords: col_disk.ords,
                        bitmaps,
                    }
                }
                FieldType::Integer => {
                    let col_disk: NumericColumnOnDisk<i64> = serde_json::from_value(col_val.clone())
                        .map_err(|e| format!("Failed to parse integer column '{}': {}", col_name, e))?;
                    let mut values = vec![None; disk.num_sections];
                    let mut val_idx = 0;
                    for run in &col_disk.present_runs {
                        let start = run[0];
                        let end = run[1].min(disk.num_sections);
                        for sec_idx in start..end {
                            if val_idx < col_disk.values.len() {
                                values[sec_idx] = Some(col_disk.values[val_idx]);
                                val_idx += 1;
                            }
                        }
                    }
                    Column::Integer {
                        present_runs: col_disk.present_runs,
                        values,
                    }
                }
                FieldType::Float => {
                    let col_disk: NumericColumnOnDisk<f64> = serde_json::from_value(col_val.clone())
                        .map_err(|e| format!("Failed to parse float column '{}': {}", col_name, e))?;
                    let mut values = vec![None; disk.num_sections];
                    let mut val_idx = 0;
                    for run in &col_disk.present_runs {
                        let start = run[0];
                        let end = run[1].min(disk.num_sections);
                        for sec_idx in start..end {
                            if val_idx < col_disk.values.len() {
                                values[sec_idx] = Some(col_disk.values[val_idx]);
                                val_idx += 1;
                            }
                        }
                    }
                    Column::Float {
                        present_runs: col_disk.present_runs,
                        values,
                    }
                }
                FieldType::Date => {
                    let col_disk: NumericColumnOnDisk<i64> = serde_json::from_value(col_val.clone())
                        .map_err(|e| format!("Failed to parse date column '{}': {}", col_name, e))?;
                    let mut values = vec![None; disk.num_sections];
                    let mut val_idx = 0;
                    for run in &col_disk.present_runs {
                        let start = run[0];
                        let end = run[1].min(disk.num_sections);
                        for sec_idx in start..end {
                            if val_idx < col_disk.values.len() {
                                values[sec_idx] = Some(col_disk.values[val_idx]);
                                val_idx += 1;
                            }
                        }
                    }
                    Column::Date {
                        present_runs: col_disk.present_runs,
                        values,
                    }
                }
            };
            columns.insert(col_name.clone(), col);
        }

        Ok(Self {
            meta_version: disk.meta_version,
            num_sections: disk.num_sections,
            generation: disk.generation,
            schema,
            files: disk.files,
            columns,
        })
    }

    pub fn to_disk(&self) -> Result<MetaIndexOnDisk, String> {
        let mut schema_map = HashMap::new();
        for (k, v) in &self.schema {
            schema_map.insert(k.clone(), v.as_str().to_string());
        }

        let mut columns_map = HashMap::new();
        for (col_name, col) in &self.columns {
            let val = match col {
                Column::Keyword { dict, ords, .. } => {
                    serde_json::to_value(KeywordColumnOnDisk {
                        dict: dict.clone(),
                        ords: ords.clone(),
                    })
                }
                Column::KeywordList { dict, offsets, ords, .. } => {
                    serde_json::to_value(KeywordListColumnOnDisk {
                        dict: dict.clone(),
                        offsets: offsets.clone(),
                        ords: ords.clone(),
                    })
                }
                Column::Integer { present_runs, values } => {
                    let mut dense_values = Vec::new();
                    for run in present_runs {
                        for sec_idx in run[0]..run[1].min(values.len()) {
                            if let Some(v) = values[sec_idx] {
                                dense_values.push(v);
                            }
                        }
                    }
                    serde_json::to_value(NumericColumnOnDisk {
                        present_runs: present_runs.clone(),
                        values: dense_values,
                    })
                }
                Column::Float { present_runs, values } => {
                    let mut dense_values = Vec::new();
                    for run in present_runs {
                        for sec_idx in run[0]..run[1].min(values.len()) {
                            if let Some(v) = values[sec_idx] {
                                dense_values.push(v);
                            }
                        }
                    }
                    serde_json::to_value(NumericColumnOnDisk {
                        present_runs: present_runs.clone(),
                        values: dense_values,
                    })
                }
                Column::Date { present_runs, values } => {
                    let mut dense_values = Vec::new();
                    for run in present_runs {
                        for sec_idx in run[0]..run[1].min(values.len()) {
                            if let Some(v) = values[sec_idx] {
                                dense_values.push(v);
                            }
                        }
                    }
                    serde_json::to_value(NumericColumnOnDisk {
                        present_runs: present_runs.clone(),
                        values: dense_values,
                    })
                }
            }
            .map_err(|e| format!("Failed to serialize column '{}': {}", col_name, e))?;

            columns_map.insert(col_name.clone(), val);
        }

        Ok(MetaIndexOnDisk {
            meta_version: self.meta_version,
            num_sections: self.num_sections,
            generation: self.generation.clone(),
            schema: schema_map,
            files: self.files.clone(),
            columns: columns_map,
        })
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        let disk = self.to_disk()?;
        crate::search::save_json(path, &disk)
    }

    pub fn has_field(&self, name: &str) -> bool {
        self.schema.contains_key(name)
    }

    pub fn field_type(&self, name: &str) -> Option<FieldType> {
        self.schema.get(name).copied()
    }

    /// Case-insensitive keyword matching for single-valued and multi-valued keywords.
    pub fn keyword_bitmap(&self, field: &str, val: &str) -> Option<&MiniRoaring> {
        let col = self.columns.get(field)?;
        let (dict, bitmaps) = match col {
            Column::Keyword { dict, bitmaps, .. } => (dict, bitmaps),
            Column::KeywordList { dict, bitmaps, .. } => (dict, bitmaps),
            _ => return None,
        };
        let target_lower = val.to_lowercase();
        let ord = dict.iter().position(|d| d.to_lowercase() == target_lower)?;
        bitmaps.get(ord)
    }

    pub fn get_integer(&self, field: &str, sec_id: usize) -> Option<i64> {
        match self.columns.get(field)? {
            Column::Integer { values, .. } => values.get(sec_id).copied().flatten(),
            _ => None,
        }
    }

    pub fn get_float(&self, field: &str, sec_id: usize) -> Option<f64> {
        match self.columns.get(field)? {
            Column::Float { values, .. } => values.get(sec_id).copied().flatten(),
            Column::Integer { values, .. } => values.get(sec_id).copied().flatten().map(|i| i as f64),
            _ => None,
        }
    }

    pub fn get_date(&self, field: &str, sec_id: usize) -> Option<i64> {
        match self.columns.get(field)? {
            Column::Date { values, .. } => values.get(sec_id).copied().flatten(),
            _ => None,
        }
    }
}

// ─── Schema Inference & Builder ─────────────────────────────────────────────

pub fn load_schema_override(path: &Path) -> Result<HashMap<String, FieldType>, String> {
    if !path.exists() {
        return Ok(HashMap::new());
    }
    let val: serde_json::Value = crate::search::load_json(path)?;
    let obj = val.as_object().ok_or_else(|| {
        format!("Schema override at {} must be a JSON object", path.display())
    })?;
    let mut map = HashMap::new();
    for (k, v) in obj {
        let type_str = v.as_str().ok_or_else(|| {
            format!("Schema type for field '{}' in {} must be a string", k, path.display())
        })?;
        let ft = FieldType::from_str_opt(type_str).ok_or_else(|| {
            format!("Invalid field type '{}' for field '{}' in {}", type_str, k, path.display())
        })?;
        map.insert(k.clone(), ft);
    }
    Ok(map)
}

fn compute_present_runs(present: &[bool]) -> Vec<[usize; 2]> {
    let mut runs = Vec::new();
    let mut in_run = false;
    let mut start = 0;
    for (i, &p) in present.iter().enumerate() {
        if p && !in_run {
            in_run = true;
            start = i;
        } else if !p && in_run {
            in_run = false;
            runs.push([start, i]);
        }
    }
    if in_run {
        runs.push([start, present.len()]);
    }
    runs
}

/// Builds a MetaIndex from per-file fields and the ordered list of sections.
pub fn build_meta_index(
    section_files: &[String],
    file_metadata: &HashMap<String, (String, HashMap<String, serde_json::Value>)>,
    schema_override: &HashMap<String, FieldType>,
    generation: &str,
) -> Option<MetaIndex> {
    if file_metadata.is_empty() {
        return None;
    }

    // Collect all field names across all files
    let mut all_fields = std::collections::BTreeSet::new();
    for (_, fields) in file_metadata.values() {
        for k in fields.keys() {
            all_fields.insert(k.clone());
        }
    }
    if all_fields.is_empty() {
        return None;
    }

    // Infer or override schema for each field
    let mut schema = HashMap::new();
    for field in &all_fields {
        if let Some(&ft) = schema_override.get(field) {
            schema.insert(field.clone(), ft);
            continue;
        }

        let mut saw_array = false;
        let mut saw_int = false;
        let mut saw_float = false;
        let mut saw_string = false;
        let mut string_all_dates = true;
        let mut saw_other = false;

        for (_, fields) in file_metadata.values() {
            if let Some(val) = fields.get(field) {
                match val {
                    serde_json::Value::Array(_) => saw_array = true,
                    serde_json::Value::Number(n) => {
                        if n.is_i64() {
                            saw_int = true;
                        } else {
                            saw_float = true;
                        }
                    }
                    serde_json::Value::String(s) => {
                        saw_string = true;
                        if parse_date_to_epoch_ms(s).is_none() {
                            string_all_dates = false;
                        }
                    }
                    _ => saw_other = true,
                }
            }
        }

        let inferred = if saw_array {
            FieldType::KeywordList
        } else if saw_string {
            if string_all_dates && !saw_int && !saw_float && !saw_other {
                FieldType::Date
            } else {
                if saw_int || saw_float || saw_other {
                    eprintln!(
                        "[⚠️] Metadata type conflict for field '{}': falling back to keyword",
                        field
                    );
                }
                FieldType::Keyword
            }
        } else if saw_float {
            if saw_other {
                eprintln!(
                    "[⚠️] Metadata type conflict for field '{}': falling back to keyword",
                    field
                );
                FieldType::Keyword
            } else {
                FieldType::Float
            }
        } else if saw_int {
            if saw_other {
                eprintln!(
                    "[⚠️] Metadata type conflict for field '{}': falling back to keyword",
                    field
                );
                FieldType::Keyword
            } else {
                FieldType::Integer
            }
        } else {
            FieldType::Keyword
        };

        schema.insert(field.clone(), inferred);
    }

    let num_sections = section_files.len();
    let mut columns = HashMap::new();

    for (field, &ft) in &schema {
        match ft {
            FieldType::Keyword => {
                // Collect unique string values
                let mut distinct = std::collections::BTreeSet::new();
                for sec_file in section_files {
                    if let Some((_, fields)) = file_metadata.get(sec_file) {
                        if let Some(val) = fields.get(field) {
                            match val {
                                serde_json::Value::String(s) => {
                                    distinct.insert(s.clone());
                                }
                                serde_json::Value::Number(n) => {
                                    distinct.insert(n.to_string());
                                }
                                serde_json::Value::Bool(b) => {
                                    distinct.insert(b.to_string());
                                }
                                _ => {}
                            }
                        }
                    }
                }
                let dict: Vec<String> = distinct.into_iter().collect();
                let dict_map: HashMap<&str, u32> = dict
                    .iter()
                    .enumerate()
                    .map(|(i, s)| (s.as_str(), i as u32))
                    .collect();

                let mut ords = vec![None; num_sections];
                let mut postings = vec![Vec::new(); dict.len()];
                for (sec_idx, sec_file) in section_files.iter().enumerate() {
                    if let Some((_, fields)) = file_metadata.get(sec_file) {
                        if let Some(val) = fields.get(field) {
                            let s = match val {
                                serde_json::Value::String(s) => s.clone(),
                                serde_json::Value::Number(n) => n.to_string(),
                                serde_json::Value::Bool(b) => b.to_string(),
                                _ => String::new(),
                            };
                            if let Some(&ord) = dict_map.get(s.as_str()) {
                                ords[sec_idx] = Some(ord);
                                postings[ord as usize].push(sec_idx as u32);
                            }
                        }
                    }
                }
                let bitmaps = postings
                    .iter()
                    .map(|p| MiniRoaring::from_sorted(p))
                    .collect();
                columns.insert(
                    field.clone(),
                    Column::Keyword {
                        dict,
                        ords,
                        bitmaps,
                    },
                );
            }
            FieldType::KeywordList => {
                let mut distinct = std::collections::BTreeSet::new();
                for sec_file in section_files {
                    if let Some((_, fields)) = file_metadata.get(sec_file) {
                        if let Some(val) = fields.get(field) {
                            if let serde_json::Value::Array(arr) = val {
                                for item in arr {
                                    match item {
                                        serde_json::Value::String(s) => {
                                            distinct.insert(s.clone());
                                        }
                                        serde_json::Value::Number(n) => {
                                            distinct.insert(n.to_string());
                                        }
                                        serde_json::Value::Bool(b) => {
                                            distinct.insert(b.to_string());
                                        }
                                        _ => {}
                                    }
                                }
                            } else if let serde_json::Value::String(s) = val {
                                distinct.insert(s.clone());
                            }
                        }
                    }
                }
                let dict: Vec<String> = distinct.into_iter().collect();
                let dict_map: HashMap<&str, u32> = dict
                    .iter()
                    .enumerate()
                    .map(|(i, s)| (s.as_str(), i as u32))
                    .collect();

                let mut offsets = Vec::with_capacity(num_sections + 1);
                let mut ords = Vec::new();
                offsets.push(0);

                let mut postings = vec![Vec::new(); dict.len()];
                for (sec_idx, sec_file) in section_files.iter().enumerate() {
                    if let Some((_, fields)) = file_metadata.get(sec_file) {
                        if let Some(val) = fields.get(field) {
                            if let serde_json::Value::Array(arr) = val {
                                for item in arr {
                                    let s = match item {
                                        serde_json::Value::String(s) => s.clone(),
                                        serde_json::Value::Number(n) => n.to_string(),
                                        serde_json::Value::Bool(b) => b.to_string(),
                                        _ => String::new(),
                                    };
                                    if let Some(&ord) = dict_map.get(s.as_str()) {
                                        ords.push(ord);
                                        postings[ord as usize].push(sec_idx as u32);
                                    }
                                }
                            } else if let serde_json::Value::String(s) = val {
                                if let Some(&ord) = dict_map.get(s.as_str()) {
                                    ords.push(ord);
                                    postings[ord as usize].push(sec_idx as u32);
                                }
                            }
                        }
                    }
                    offsets.push(ords.len());
                }
                let bitmaps = postings
                    .iter()
                    .map(|p| MiniRoaring::from_sorted(p))
                    .collect();
                columns.insert(
                    field.clone(),
                    Column::KeywordList {
                        dict,
                        offsets,
                        ords,
                        bitmaps,
                    },
                );
            }
            FieldType::Integer => {
                let mut values = vec![None; num_sections];
                let mut present = vec![false; num_sections];
                for (sec_idx, sec_file) in section_files.iter().enumerate() {
                    if let Some((_, fields)) = file_metadata.get(sec_file) {
                        if let Some(val) = fields.get(field) {
                            if let Some(i) = val.as_i64() {
                                values[sec_idx] = Some(i);
                                present[sec_idx] = true;
                            } else if let Some(f) = val.as_f64() {
                                values[sec_idx] = Some(f as i64);
                                present[sec_idx] = true;
                            }
                        }
                    }
                }
                let present_runs = compute_present_runs(&present);
                columns.insert(
                    field.clone(),
                    Column::Integer {
                        present_runs,
                        values,
                    },
                );
            }
            FieldType::Float => {
                let mut values = vec![None; num_sections];
                let mut present = vec![false; num_sections];
                for (sec_idx, sec_file) in section_files.iter().enumerate() {
                    if let Some((_, fields)) = file_metadata.get(sec_file) {
                        if let Some(val) = fields.get(field) {
                            if let Some(f) = val.as_f64() {
                                values[sec_idx] = Some(f);
                                present[sec_idx] = true;
                            }
                        }
                    }
                }
                let present_runs = compute_present_runs(&present);
                columns.insert(
                    field.clone(),
                    Column::Float {
                        present_runs,
                        values,
                    },
                );
            }
            FieldType::Date => {
                let mut values = vec![None; num_sections];
                let mut present = vec![false; num_sections];
                for (sec_idx, sec_file) in section_files.iter().enumerate() {
                    if let Some((_, fields)) = file_metadata.get(sec_file) {
                        if let Some(val) = fields.get(field) {
                            if let Some(s) = val.as_str() {
                                if let Some(ms) = parse_date_to_epoch_ms(s) {
                                    values[sec_idx] = Some(ms);
                                    present[sec_idx] = true;
                                }
                            }
                        }
                    }
                }
                let present_runs = compute_present_runs(&present);
                columns.insert(
                    field.clone(),
                    Column::Date {
                        present_runs,
                        values,
                    },
                );
            }
        }
    }

    let mut files_map = HashMap::new();
    for (path, (src, fields)) in file_metadata {
        files_map.insert(
            path.clone(),
            MetaFileEntry {
                source: src.clone(),
                fields: fields.clone(),
            },
        );
    }

    Some(MetaIndex {
        meta_version: 1,
        num_sections,
        generation: generation.to_string(),
        schema,
        files: files_map,
        columns,
    })
}

// ─── Unit Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_date_parser() {
        // Epoch 1970-01-01
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(parse_date_to_epoch_ms("1970-01-01"), Some(0));
        assert_eq!(parse_date_to_epoch_ms("1970-01-01T00:00:00Z"), Some(0));

        // 2020-01-01
        let ms_2020 = parse_date_to_epoch_ms("2020-01-01T00:00:00Z").unwrap();
        assert_eq!(ms_2020, 1577836800000);

        // Timezone offsets
        let ms_offset = parse_date_to_epoch_ms("2020-01-01T02:00:00+02:00").unwrap();
        assert_eq!(ms_offset, ms_2020);

        // Fractional seconds
        let ms_frac = parse_date_to_epoch_ms("2020-01-01T00:00:00.500Z").unwrap();
        assert_eq!(ms_frac, ms_2020 + 500);

        // Pre-epoch
        let ms_pre = parse_date_to_epoch_ms("1969-12-31T23:59:59Z").unwrap();
        assert_eq!(ms_pre, -1000);

        // Invalid
        assert!(parse_date_to_epoch_ms("not-a-date").is_none());
        assert!(parse_date_to_epoch_ms("2020-13-01").is_none());
    }

    #[test]
    fn test_frontmatter_subset_and_line_numbers() {
        let md = r#"---
title: "Document Title"
category: biology
tags: [science, health]
year: 2024
authors:
  - Alice Smith
  - Bob Jones
---
# Section 1
Line 11 content here.
"#;

        let (fields, blanked) = extract_and_blank_frontmatter(md);
        assert_eq!(fields.get("title").unwrap().as_str().unwrap(), "Document Title");
        assert_eq!(fields.get("category").unwrap().as_str().unwrap(), "biology");
        assert_eq!(fields.get("year").unwrap().as_i64().unwrap(), 2024);

        let tags = fields.get("tags").unwrap().as_array().unwrap();
        assert_eq!(tags.len(), 2);
        assert_eq!(tags[0].as_str().unwrap(), "science");
        assert_eq!(tags[1].as_str().unwrap(), "health");

        let authors = fields.get("authors").unwrap().as_array().unwrap();
        assert_eq!(authors.len(), 2);
        assert_eq!(authors[0].as_str().unwrap(), "Alice Smith");
        assert_eq!(authors[1].as_str().unwrap(), "Bob Jones");

        // Verify line numbers are preserved
        let orig_lines: Vec<&str> = md.split('\n').collect();
        let blanked_lines: Vec<&str> = blanked.split('\n').collect();
        assert_eq!(orig_lines.len(), blanked_lines.len());

        let sec_orig = orig_lines.iter().position(|l| l.starts_with("# Section 1")).unwrap();
        let sec_blanked = blanked_lines.iter().position(|l| l.starts_with("# Section 1")).unwrap();
        assert_eq!(sec_orig, sec_blanked);
        assert_eq!(sec_blanked, 9); // 0-indexed line 9 = line 10 in 1-indexed

        // Parse markdown with bm25
        let sections = crate::bm25::parse_markdown_with_options(&blanked, false);
        assert_eq!(sections.len(), 1);
        assert_eq!(sections[0].line_number, 10);
    }

    #[test]
    fn test_type_inference_and_widening() {
        let mut files = HashMap::new();

        // 1. Integer
        let mut f1 = HashMap::new();
        f1.insert("num".to_string(), serde_json::json!(42));
        files.insert("doc1.txt".to_string(), ("frontmatter".to_string(), f1));

        let sec_files = vec!["doc1.txt".to_string()];
        let meta = build_meta_index(&sec_files, &files, &HashMap::new(), "gen1").unwrap();
        assert_eq!(meta.schema.get("num"), Some(&FieldType::Integer));

        // 2. Int + Float widens to Float
        let mut f2 = HashMap::new();
        f2.insert("num".to_string(), serde_json::json!(3.14));
        files.insert("doc2.txt".to_string(), ("frontmatter".to_string(), f2));

        let sec_files = vec!["doc1.txt".to_string(), "doc2.txt".to_string()];
        let meta = build_meta_index(&sec_files, &files, &HashMap::new(), "gen1").unwrap();
        assert_eq!(meta.schema.get("num"), Some(&FieldType::Float));

        // 3. Dates
        let mut d1 = HashMap::new();
        d1.insert("pub".to_string(), serde_json::json!("2020-01-01"));
        let mut d2 = HashMap::new();
        d2.insert("pub".to_string(), serde_json::json!("2021-06-15T12:00:00Z"));
        let mut date_files = HashMap::new();
        date_files.insert("d1.txt".to_string(), ("frontmatter".to_string(), d1));
        date_files.insert("d2.txt".to_string(), ("frontmatter".to_string(), d2));

        let meta = build_meta_index(&["d1.txt".to_string(), "d2.txt".to_string()], &date_files, &HashMap::new(), "gen1").unwrap();
        assert_eq!(meta.schema.get("pub"), Some(&FieldType::Date));

        // 4. Conflicting type fallback to Keyword
        let mut c1 = HashMap::new();
        c1.insert("mixed".to_string(), serde_json::json!(123));
        let mut c2 = HashMap::new();
        c2.insert("mixed".to_string(), serde_json::json!("not-a-number"));
        let mut conf_files = HashMap::new();
        conf_files.insert("c1.txt".to_string(), ("frontmatter".to_string(), c1));
        conf_files.insert("c2.txt".to_string(), ("frontmatter".to_string(), c2));

        let meta = build_meta_index(&["c1.txt".to_string(), "c2.txt".to_string()], &conf_files, &HashMap::new(), "gen1").unwrap();
        assert_eq!(meta.schema.get("mixed"), Some(&FieldType::Keyword));
    }

    #[test]
    fn test_column_storage_roundtrip() {
        let mut files = HashMap::new();
        let mut f1 = HashMap::new();
        f1.insert("cat".to_string(), serde_json::json!("bio"));
        f1.insert("tags".to_string(), serde_json::json!(["dna", "rna"]));
        f1.insert("year".to_string(), serde_json::json!(2021));
        files.insert("doc1.txt".to_string(), ("frontmatter".to_string(), f1));

        let mut f2 = HashMap::new();
        f2.insert("cat".to_string(), serde_json::json!("phys"));
        f2.insert("year".to_string(), serde_json::json!(2022));
        files.insert("doc2.txt".to_string(), ("frontmatter".to_string(), f2));

        let sec_files = vec!["doc1.txt".to_string(), "doc2.txt".to_string()];
        let meta = build_meta_index(&sec_files, &files, &HashMap::new(), "gen-1234").unwrap();

        assert_eq!(meta.num_sections, 2);
        assert_eq!(meta.generation, "gen-1234");

        // Convert to disk representation
        let disk = meta.to_disk().unwrap();
        assert_eq!(disk.num_sections, 2);
        assert_eq!(disk.generation, "gen-1234");
        assert!(disk.columns.contains_key("cat"));
        assert!(disk.columns.contains_key("tags"));
        assert!(disk.columns.contains_key("year"));

        // Roundtrip serialization
        let json_str = serde_json::to_string(&disk).unwrap();
        let de_disk: MetaIndexOnDisk = serde_json::from_str(&json_str).unwrap();
        assert_eq!(de_disk.meta_version, 1);
        assert_eq!(de_disk.num_sections, 2);

        // In-memory keyword bitmap lookup
        let bm = meta.keyword_bitmap("cat", "bio").unwrap();
        assert_eq!(bm.iter(), vec![0]);
        let bm_tags = meta.keyword_bitmap("tags", "dna").unwrap();
        assert_eq!(bm_tags.iter(), vec![0]);

        // Integer lookup
        assert_eq!(meta.get_integer("year", 0), Some(2021));
        assert_eq!(meta.get_integer("year", 1), Some(2022));
    }

    #[test]
    fn test_manifest_precedence() {
        let temp_dir = std::env::temp_dir().join(format!(
            "lume_test_manifest_prec_{}_{}",
            std::process::id(),
            crate::uuid_v4()
        ));
        let _ = std::fs::remove_dir_all(&temp_dir);
        let sub_dir = temp_dir.join("sub");
        std::fs::create_dir_all(&sub_dir).unwrap();

        // 1. Target file in sub/doc.md with frontmatter: author: Alice, year: 2020, tags: [a]
        let md_content = "---\nauthor: Alice\nyear: 2020\ntags: [a]\n---\n# Doc\nContent\n";
        let doc_path = sub_dir.join("doc.md");
        std::fs::write(&doc_path, md_content).unwrap();

        // 2. Shallow manifest in temp_dir/lume.meta.jsonl (depth 1): author: Bob, category: tech
        let shallow_manifest = serde_json::json!({
            "path": "sub/doc.md",
            "fields": {
                "author": "Bob",
                "category": "tech"
            }
        });
        std::fs::write(temp_dir.join("lume.meta.jsonl"), format!("{}\n", shallow_manifest)).unwrap();

        // 3. Deeper manifest in temp_dir/sub/lume.meta.jsonl (depth 2): author: Charlie
        let deep_manifest = serde_json::json!({
            "path": "doc.md",
            "fields": {
                "author": "Charlie"
            }
        });
        std::fs::write(sub_dir.join("lume.meta.jsonl"), format!("{}\n", deep_manifest)).unwrap();

        // Read manifests
        let mut manifests = Vec::new();
        let shallow_rows = read_manifest(&temp_dir.join("lume.meta.jsonl")).unwrap();
        manifests.push(DiscoveredManifest {
            path: temp_dir.join("lume.meta.jsonl"),
            dir: temp_dir.clone(),
            depth: temp_dir.components().count(),
            rows: shallow_rows,
        });
        let deep_rows = read_manifest(&sub_dir.join("lume.meta.jsonl")).unwrap();
        manifests.push(DiscoveredManifest {
            path: sub_dir.join("lume.meta.jsonl"),
            dir: sub_dir.clone(),
            depth: sub_dir.components().count(),
            rows: deep_rows,
        });

        // Parse frontmatter
        let (fm_fields, _) = extract_and_blank_frontmatter(md_content);
        let mut frontmatter_by_file = HashMap::new();
        frontmatter_by_file.insert(doc_path.to_string_lossy().to_string(), fm_fields);

        let mut cached_files = HashMap::new();
        let sec = crate::bm25::Section {
            title: "Doc".to_string(),
            body: "Content".to_string(),
            line_number: 6,
            filename: Some(doc_path.to_string_lossy().to_string()),
            entities: Vec::new(),
        };
        cached_files.insert(doc_path.to_string_lossy().to_string(), (100u64, vec![sec]));

        let mut file_fields: HashMap<String, (String, HashMap<String, serde_json::Value>)> = HashMap::new();
        let mut field_depths: HashMap<String, HashMap<String, usize>> = HashMap::new();

        for (path_str, fm) in &frontmatter_by_file {
            file_fields.insert(path_str.clone(), ("frontmatter".to_string(), fm.clone()));
            let mut depths = HashMap::new();
            for k in fm.keys() {
                depths.insert(k.clone(), 0);
            }
            field_depths.insert(path_str.clone(), depths);
        }

        let mut sorted_manifests: Vec<&DiscoveredManifest> = manifests.iter().collect();
        sorted_manifests.sort_by_key(|m| m.depth);

        for manifest in sorted_manifests {
            for row in &manifest.rows {
                let row_rel = normalize_rel_path(&row.path);
                let resolved_path = manifest.dir.join(&row_rel);
                let resolved_norm = normalize_rel_path(&resolved_path.to_string_lossy());

                let mut matched_cached_path = None;
                for cached_path in cached_files.keys() {
                    let cached_norm = normalize_rel_path(cached_path);
                    if cached_norm == resolved_norm {
                        matched_cached_path = Some(cached_path.clone());
                        break;
                    }
                }

                if let Some(target_file) = matched_cached_path {
                    let entry = file_fields
                        .entry(target_file.clone())
                        .or_insert_with(|| ("manifest".to_string(), HashMap::new()));
                    let depths = field_depths.entry(target_file).or_default();

                    for (k, v) in &row.fields {
                        let prev_depth = depths.get(k).copied().unwrap_or(0);
                        if manifest.depth >= prev_depth || !depths.contains_key(k) {
                            entry.1.insert(k.clone(), v.clone());
                            depths.insert(k.clone(), manifest.depth);
                        }
                    }
                }
            }
        }

        let doc_key = doc_path.to_string_lossy().to_string();
        let resolved = &file_fields.get(&doc_key).unwrap().1;

        assert_eq!(resolved.get("author").unwrap().as_str().unwrap(), "Charlie");
        assert_eq!(resolved.get("category").unwrap().as_str().unwrap(), "tech");
        assert_eq!(resolved.get("year").unwrap().as_i64().unwrap(), 2020);

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_version_3_only_when_fields_exist() {
        // No metadata fields -> build_meta_index returns None
        let empty_files: HashMap<String, (String, HashMap<String, serde_json::Value>)> = HashMap::new();
        assert!(build_meta_index(&["a.txt".to_string()], &empty_files, &HashMap::new(), "gen").is_none());

        // Files with empty fields -> None
        let mut no_fields = HashMap::new();
        no_fields.insert("a.txt".to_string(), ("frontmatter".to_string(), HashMap::new()));
        assert!(build_meta_index(&["a.txt".to_string()], &no_fields, &HashMap::new(), "gen").is_none());

        // Files with at least one field -> Some(MetaIndex)
        let mut with_fields = HashMap::new();
        let mut f = HashMap::new();
        f.insert("tag".to_string(), serde_json::json!("val"));
        with_fields.insert("a.txt".to_string(), ("frontmatter".to_string(), f));
        let meta = build_meta_index(&["a.txt".to_string()], &with_fields, &HashMap::new(), "gen");
        assert!(meta.is_some());
    }

    #[test]
    fn test_manifest_edits_without_reparsing() {
        // Simulate cached file sections without re-chunking
        let doc_name = "test_doc.md".to_string();
        let sec = crate::bm25::Section {
            title: "T".to_string(),
            body: "B".to_string(),
            line_number: 1,
            filename: Some(doc_name.clone()),
            entities: Vec::new(),
        };
        let mut cached_files = HashMap::new();
        cached_files.insert(doc_name.clone(), (1000u64, vec![sec]));

        // Run 1: Manifest has category: biology
        let mut files_meta1 = HashMap::new();
        let mut fields1 = HashMap::new();
        fields1.insert("category".to_string(), serde_json::json!("biology"));
        files_meta1.insert(doc_name.clone(), ("manifest".to_string(), fields1));

        let meta1 = build_meta_index(&[doc_name.clone()], &files_meta1, &HashMap::new(), "gen-1").unwrap();
        assert_eq!(meta1.keyword_bitmap("category", "biology").unwrap().iter(), vec![0]);

        // Run 2: Manifest edited to category: chemistry, cached_files UNTOUCHED
        let mut files_meta2 = HashMap::new();
        let mut fields2 = HashMap::new();
        fields2.insert("category".to_string(), serde_json::json!("chemistry"));
        files_meta2.insert(doc_name.clone(), ("manifest".to_string(), fields2));

        let meta2 = build_meta_index(&[doc_name.clone()], &files_meta2, &HashMap::new(), "gen-2").unwrap();
        assert_eq!(meta2.keyword_bitmap("category", "chemistry").unwrap().iter(), vec![0]);
        assert!(meta2.keyword_bitmap("category", "biology").is_none());
    }

    #[test]
    fn test_schema_files_not_indexed() {
        let temp_dir = std::env::temp_dir().join(format!(
            "lume_test_schema_ignore_{}_{}",
            std::process::id(),
            crate::uuid_v4()
        ));
        let _ = std::fs::remove_dir_all(&temp_dir);
        std::fs::create_dir_all(&temp_dir).unwrap();

        std::fs::write(temp_dir.join("guide.md"), "# Guide\nContent").unwrap();
        std::fs::write(temp_dir.join("lume.meta.jsonl"), r#"{"path":"guide.md","fields":{"cat":"docs"}}"#).unwrap();
        std::fs::write(temp_dir.join("lume.schema.json"), r#"{"cat":"keyword"}"#).unwrap();

        let mut files = Vec::new();
        let _db_dir = temp_dir.join(".lume-index");
        // Test scan ignoring schema and manifest files
        for entry in std::fs::read_dir(&temp_dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_file() {
                if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                    if name == "lume.meta.jsonl" || name == "lume.schema.json" {
                        continue;
                    }
                }
                if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                    if ext == "md" {
                        files.push(path);
                    }
                }
            }
        }

        assert_eq!(files.len(), 1);
        assert_eq!(files[0].file_name().unwrap(), "guide.md");

        let _ = std::fs::remove_dir_all(&temp_dir);
    }
}

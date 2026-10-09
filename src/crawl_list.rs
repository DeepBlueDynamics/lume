//! `lume crawl --list <csv>`: fetch a reading list into a directory `lume index` can read.
//!
//! The list is a CSV with a header row containing at least `title`, `format` and `url`
//! (`category`, `subcategory`, `publisher` and `notes` are carried into the manifest when
//! present), as in `docs/cruiser_library.csv`. Each row gets a stable id derived from its URL.
//! Files land in `<out>/<id>.<ext>`, and `<out>/library.json` records what was fetched so
//! reruns skip finished rows and search hits can be mapped back to title, publisher and URL.
//!
//! Documents (PDF, EPUB, TXT) are downloaded as bytes; HTML pages become Markdown. A local Grub
//! crawler (GRUB_BASE_URL, default http://localhost:6792) is used when it answers `/health`;
//! otherwise rows are fetched with a direct GET, so the command also works on a boat computer
//! with no crawler. ZIM archives are skipped unless `--formats` names them explicitly.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

const USER_AGENT: &str =
    "Mozilla/5.0 (X11; Linux aarch64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0 Safari/537.36 lume-library/1";
const DEFAULT_FORMATS: &[&str] = &["pdf", "epub", "txt", "html"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListRow {
    pub id: String,
    pub fields: BTreeMap<String, String>,
}

impl ListRow {
    pub fn get(&self, key: &str) -> &str {
        self.fields.get(key).map(String::as_str).unwrap_or("")
    }
    pub fn format(&self) -> String {
        self.get("format").trim().to_ascii_lowercase()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ManifestEntry {
    pub id: String,
    pub title: String,
    pub category: String,
    pub subcategory: String,
    pub publisher: String,
    pub format: String,
    pub url: String,
    pub file: Option<String>,
    pub bytes: Option<u64>,
    pub status: String,
    pub error: Option<String>,
    pub via: Option<String>,
    pub fetched_at: Option<u64>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Manifest {
    pub source: String,
    pub entries: BTreeMap<String, ManifestEntry>,
}

pub struct ListOptions {
    pub csv: PathBuf,
    pub out: PathBuf,
    pub only: Option<Vec<String>>,
    pub formats: Vec<String>,
    pub category: Option<String>,
    pub limit: Option<usize>,
    pub max_bytes: u64,
    pub force: bool,
    pub dry_run: bool,
    pub timeout: Duration,
}

/// Stable 12-hex-digit id (FNV-1a 64) of the trimmed URL, so ids survive CSV reordering.
pub fn row_id(url: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in url.trim().as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{:012x}", hash >> 16)
}

/// RFC 4180 CSV: quoted fields, doubled quotes, commas and newlines inside quotes.
pub fn parse_csv(text: &str) -> Result<Vec<Vec<String>>, String> {
    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = text.trim_start_matches('\u{feff}').chars().peekable();
    while let Some(c) = chars.next() {
        if quoted {
            match c {
                '"' if chars.peek() == Some(&'"') => {
                    field.push('"');
                    chars.next();
                }
                '"' => quoted = false,
                _ => field.push(c),
            }
            continue;
        }
        match c {
            '"' if field.is_empty() => quoted = true,
            ',' => row.push(std::mem::take(&mut field)),
            '\r' => {}
            '\n' => {
                row.push(std::mem::take(&mut field));
                if row.iter().any(|f| !f.is_empty()) {
                    rows.push(std::mem::take(&mut row));
                } else {
                    row.clear();
                }
            }
            _ => field.push(c),
        }
    }
    if quoted {
        return Err("unterminated quoted field".into());
    }
    if !field.is_empty() || !row.is_empty() {
        row.push(field);
        rows.push(row);
    }
    Ok(rows)
}

pub fn read_list(path: &Path) -> Result<Vec<ListRow>, String> {
    let text = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut rows = parse_csv(&text)?.into_iter();
    let header: Vec<String> = rows
        .next()
        .ok_or("list is empty")?
        .into_iter()
        .map(|h| h.trim().to_ascii_lowercase())
        .collect();
    for required in ["title", "format", "url"] {
        if !header.iter().any(|h| h == required) {
            return Err(format!("list header must include '{required}'"));
        }
    }
    let mut out = Vec::new();
    for (n, values) in rows.enumerate() {
        let fields: BTreeMap<String, String> = header
            .iter()
            .cloned()
            .zip(values.into_iter().map(|v| v.trim().to_string()))
            .collect();
        let url = fields.get("url").cloned().unwrap_or_default();
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            return Err(format!("row {}: url must be http(s): '{url}'", n + 2));
        }
        out.push(ListRow {
            id: row_id(&url),
            fields,
        });
    }
    Ok(out)
}

pub fn select<'a>(rows: &'a [ListRow], options: &ListOptions) -> Vec<&'a ListRow> {
    let mut selected: Vec<&ListRow> = rows
        .iter()
        .filter(|row| match &options.only {
            Some(ids) => ids.iter().any(|id| id == &row.id),
            None => true,
        })
        .filter(|row| options.formats.iter().any(|f| *f == row.format()))
        .filter(|row| match &options.category {
            Some(c) => row.get("category").eq_ignore_ascii_case(c),
            None => true,
        })
        .collect();
    if let Some(limit) = options.limit {
        selected.truncate(limit);
    }
    selected
}

fn extension(format: &str) -> &str {
    match format {
        "html" => "md",
        "pdf" => "pdf",
        "epub" => "epub",
        "zim" => "zim",
        _ => "txt",
    }
}

fn manifest_path(out: &Path) -> PathBuf {
    out.join("library.json")
}

pub fn load_manifest(out: &Path) -> Manifest {
    fs::read_to_string(manifest_path(out))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn save_manifest(out: &Path, manifest: &Manifest) -> Result<(), String> {
    let tmp = out.join("library.json.tmp");
    let text = serde_json::to_string_pretty(manifest).map_err(|e| e.to_string())?;
    fs::write(&tmp, text).map_err(|e| e.to_string())?;
    fs::rename(&tmp, manifest_path(out)).map_err(|e| e.to_string())
}

fn grub_base() -> Option<String> {
    let base = std::env::var("GRUB_BASE_URL").unwrap_or_else(|_| "http://localhost:6792".into());
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(3))
        .build();
    let ok = agent
        .get(&format!("{}/health", base.trim_end_matches('/')))
        .call()
        .map(|r| r.status() == 200)
        .unwrap_or(false);
    ok.then(|| base.trim_end_matches('/').to_string())
}

fn read_capped(reader: impl Read, max_bytes: u64) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    reader
        .take(max_bytes + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("read failed: {e}"))?;
    if bytes.len() as u64 > max_bytes {
        return Err(format!("larger than --max-mb ({} MB)", max_bytes >> 20));
    }
    Ok(bytes)
}

fn fetch_bytes(
    agent: &ureq::Agent,
    url: &str,
    format: &str,
    grub: Option<&str>,
    max_bytes: u64,
) -> Result<(Vec<u8>, &'static str), String> {
    let direct = agent
        .get(url)
        .set("User-Agent", USER_AGENT)
        .call()
        .map_err(|e| e.to_string())
        .and_then(|r| {
            if let Some(len) = r
                .header("Content-Length")
                .and_then(|l| l.parse::<u64>().ok())
            {
                if len > max_bytes {
                    return Err(format!("larger than --max-mb ({} MB)", max_bytes >> 20));
                }
            }
            let bytes = read_capped(r.into_reader(), max_bytes)?;
            // Bot protection usually answers 200 with an HTML page, so validate before accepting.
            check_payload(format, &bytes)?;
            Ok(bytes)
        });
    match (direct, grub) {
        (Ok(bytes), _) => Ok((bytes, "direct")),
        (Err(e), _) if e.contains("--max-mb") => Err(e),
        (Err(_), Some(base)) => {
            let response = agent
                .get(&format!("{base}/download"))
                .query("url", url)
                .query("use_browser", "true")
                .call()
                .map_err(|e| format!("grub download failed: {e}"))?;
            let bytes = read_capped(response.into_reader(), max_bytes)?;
            check_payload(format, &bytes)?;
            Ok((bytes, "grub"))
        }
        (Err(e), None) => Err(e),
    }
}

/// Reject the common failure where a "PDF" URL answers with an HTML error or login page.
fn check_payload(format: &str, bytes: &[u8]) -> Result<(), String> {
    let head = &bytes[..bytes.len().min(512)];
    let looks_html = String::from_utf8_lossy(head)
        .to_ascii_lowercase()
        .contains("<html");
    match format {
        "pdf" if !bytes.starts_with(b"%PDF") => Err(if looks_html {
            "server returned HTML, not a PDF (likely bot protection)".into()
        } else {
            "response is not a PDF".into()
        }),
        "epub" if !bytes.starts_with(b"PK") => Err("response is not an EPUB (zip)".into()),
        "zim" if !bytes.starts_with(&[0x5a, 0x49, 0x4d, 0x04]) => {
            Err("response is not a ZIM archive".into())
        }
        _ if bytes.is_empty() => Err("empty response".into()),
        _ => Ok(()),
    }
}

fn fetch_html(row: &ListRow, grub: Option<&str>) -> Result<(String, &'static str), String> {
    let url = row.get("url");
    let markdown = match grub {
        Some(base) => {
            let agent = ureq::AgentBuilder::new()
                .timeout(Duration::from_secs(90))
                .build();
            let value: serde_json::Value = agent
                .post(&format!("{base}/api/markdown"))
                .send_json(serde_json::json!({"url": url, "javascript_enabled": true}))
                .map_err(|e| format!("grub markdown failed: {e}"))?
                .into_json()
                .map_err(|e| e.to_string())?;
            ["markdown", "markdown_plain", "content"]
                .iter()
                .find_map(|k| value.get(*k).and_then(|v| v.as_str()))
                .map(|s| (s.to_string(), "grub"))
        }
        None => None,
    };
    let (body, via) = match markdown {
        Some(found) => found,
        None => {
            let agent = ureq::AgentBuilder::new()
                .timeout(Duration::from_secs(30))
                .build();
            let html = agent
                .get(url)
                .set("User-Agent", USER_AGENT)
                .call()
                .map_err(|e| e.to_string())?
                .into_string()
                .map_err(|e| e.to_string())?;
            (crate::crawl::clean_html_to_markdown(&html).1, "direct")
        }
    };
    Ok((
        format!(
            "# {}\n\n*   **Publisher**: {}\n*   **Source URL**: {}\n\n---\n\n{}",
            row.get("title"),
            row.get("publisher"),
            url,
            body
        ),
        via,
    ))
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn entry(row: &ListRow) -> ManifestEntry {
    ManifestEntry {
        id: row.id.clone(),
        title: row.get("title").into(),
        category: row.get("category").into(),
        subcategory: row.get("subcategory").into(),
        publisher: row.get("publisher").into(),
        format: row.format(),
        url: row.get("url").into(),
        file: None,
        bytes: None,
        status: "pending".into(),
        error: None,
        via: None,
        fetched_at: None,
    }
}

/// Fetch the selected rows. Returns (ok, failed, skipped) counts.
pub fn run_list(options: &ListOptions) -> Result<(usize, usize, usize), String> {
    let rows = read_list(&options.csv)?;
    let selected = select(&rows, options);
    if let Some(ids) = &options.only {
        let missing: Vec<_> = ids
            .iter()
            .filter(|id| !rows.iter().any(|r| &r.id == *id))
            .collect();
        if !missing.is_empty() {
            return Err(format!("unknown ids in --only: {missing:?}"));
        }
    }
    if options.dry_run {
        for row in &selected {
            println!(
                "{}  {:<5} {:<16} {}",
                row.id,
                row.format(),
                row.get("category"),
                row.get("title")
            );
        }
        println!("{} of {} rows selected", selected.len(), rows.len());
        return Ok((0, 0, selected.len()));
    }
    fs::create_dir_all(&options.out).map_err(|e| format!("{}: {e}", options.out.display()))?;
    let mut manifest = load_manifest(&options.out);
    manifest.source = options.csv.display().to_string();
    let grub = grub_base();
    match &grub {
        Some(base) => {
            println!("Using Grub at {base} for HTML and as a fallback for blocked downloads")
        }
        None => println!("No Grub crawler reachable; fetching directly"),
    }
    let agent = ureq::AgentBuilder::new()
        .timeout(options.timeout)
        .redirects(8)
        .build();
    let (mut ok, mut failed, mut skipped) = (0, 0, 0);
    for (n, row) in selected.iter().enumerate() {
        let format = row.format();
        let file_name = format!("{}.{}", row.id, extension(&format));
        let path = options.out.join(&file_name);
        let done = manifest
            .entries
            .get(&row.id)
            .is_some_and(|e| e.status == "ok" && path.exists());
        if done && !options.force {
            skipped += 1;
            continue;
        }
        print!(
            "[{}/{}] {} ({}) ... ",
            n + 1,
            selected.len(),
            row.get("title"),
            format
        );
        let mut item = entry(row);
        let result: Result<(u64, &str), String> = if format == "html" {
            fetch_html(row, grub.as_deref()).and_then(|(text, via)| {
                fs::write(&path, &text).map_err(|e| e.to_string())?;
                Ok((text.len() as u64, via))
            })
        } else {
            fetch_bytes(
                &agent,
                row.get("url"),
                &format,
                grub.as_deref(),
                options.max_bytes,
            )
            .and_then(|(bytes, via)| {
                let tmp = options.out.join(format!("{file_name}.part"));
                fs::write(&tmp, &bytes).map_err(|e| e.to_string())?;
                fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
                Ok((bytes.len() as u64, via))
            })
        };
        match result {
            Ok((bytes, via)) => {
                println!("ok ({} KB via {via})", bytes / 1024);
                item.file = Some(file_name);
                item.bytes = Some(bytes);
                item.status = "ok".into();
                item.via = Some(via.into());
                item.fetched_at = Some(now());
                ok += 1;
            }
            Err(error) => {
                println!("failed: {error}");
                item.status = "failed".into();
                item.error = Some(error);
                failed += 1;
            }
        }
        manifest.entries.insert(row.id.clone(), item);
        save_manifest(&options.out, &manifest)?;
    }
    save_manifest(&options.out, &manifest)?;
    Ok((ok, failed, skipped))
}

fn value<'a>(args: &'a [String], i: usize, flag: &str) -> Result<&'a String, String> {
    args.get(i + 1)
        .filter(|v| !v.starts_with("--"))
        .ok_or_else(|| format!("{flag} requires a value"))
}

pub fn parse_args(args: &[String]) -> Result<ListOptions, String> {
    let mut options = ListOptions {
        csv: PathBuf::new(),
        out: PathBuf::from("library"),
        only: None,
        formats: DEFAULT_FORMATS.iter().map(|s| s.to_string()).collect(),
        category: None,
        limit: None,
        max_bytes: 128 << 20,
        force: false,
        dry_run: false,
        timeout: Duration::from_secs(120),
    };
    let mut i = 0;
    while i < args.len() {
        let flag = args[i].as_str();
        match flag {
            "--list" => options.csv = PathBuf::from(value(args, i, flag)?),
            "--out" => options.out = PathBuf::from(value(args, i, flag)?),
            "--only" => {
                options.only = Some(
                    value(args, i, flag)?
                        .split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect(),
                )
            }
            "--formats" => {
                options.formats = value(args, i, flag)?
                    .split(',')
                    .map(|s| s.trim().to_ascii_lowercase())
                    .collect()
            }
            "--category" => options.category = Some(value(args, i, flag)?.clone()),
            "--limit" => {
                options.limit = Some(
                    value(args, i, flag)?
                        .parse()
                        .map_err(|_| "--limit must be a number")?,
                )
            }
            "--max-mb" => {
                let mb: u64 = value(args, i, flag)?
                    .parse()
                    .map_err(|_| "--max-mb must be a number")?;
                options.max_bytes = mb << 20;
            }
            "--timeout" => {
                let secs: u64 = value(args, i, flag)?
                    .parse()
                    .map_err(|_| "--timeout must be seconds")?;
                options.timeout = Duration::from_secs(secs);
            }
            "--force" => {
                options.force = true;
                i += 1;
                continue;
            }
            "--dry-run" => {
                options.dry_run = true;
                i += 1;
                continue;
            }
            other => return Err(format!("unknown option for crawl --list: {other}")),
        }
        i += 2;
    }
    if options.csv.as_os_str().is_empty() {
        return Err("--list <csv> is required".into());
    }
    Ok(options)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csv_handles_quotes_commas_and_newlines() {
        let rows = parse_csv("a,b\n\"x, y\",\"say \"\"hi\"\"\nthere\"\n\n1,2\r\n").unwrap();
        assert_eq!(
            rows[1],
            vec!["x, y".to_string(), "say \"hi\"\nthere".to_string()]
        );
        assert_eq!(rows[2], vec!["1", "2"]);
        assert!(parse_csv("a\n\"open").is_err());
    }

    #[test]
    fn ids_are_stable_and_url_derived() {
        assert_eq!(
            row_id("https://a.example/x.pdf"),
            row_id(" https://a.example/x.pdf ")
        );
        assert_ne!(
            row_id("https://a.example/x.pdf"),
            row_id("https://a.example/y.pdf")
        );
        assert_eq!(row_id("https://a.example/x.pdf").len(), 12);
    }

    #[test]
    fn payload_checks_catch_html_error_pages() {
        assert!(check_payload("pdf", b"%PDF-1.7 ...").is_ok());
        let err = check_payload("pdf", b"<!doctype html><html><body>blocked").unwrap_err();
        assert!(err.contains("bot protection"));
        assert!(check_payload("epub", b"PK\x03\x04").is_ok());
        assert!(check_payload("txt", b"").is_err());
    }

    #[test]
    fn selection_filters_formats_ids_category_and_limit() {
        let dir = std::env::temp_dir().join(format!("lume-list-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let csv = dir.join("l.csv");
        fs::write(
            &csv,
            "category,title,format,url\nNav,A,PDF,https://e.x/a.pdf\nMed,B,ZIM,https://e.x/b.zim\nNav,C,HTML,https://e.x/c\n",
        )
        .unwrap();
        let rows = read_list(&csv).unwrap();
        let mut options = parse_args(&["--list".into(), csv.display().to_string()]).unwrap();
        assert_eq!(select(&rows, &options).len(), 2, "ZIM is opt-in");
        options.category = Some("nav".into());
        options.limit = Some(1);
        assert_eq!(select(&rows, &options)[0].get("title"), "A");
        options.only = Some(vec![rows[2].id.clone()]);
        options.limit = None;
        assert_eq!(select(&rows, &options)[0].get("title"), "C");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn real_cruiser_library_parses() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/cruiser_library.csv");
        let rows = read_list(&path).unwrap();
        assert_eq!(rows.len(), 471);
        let ids: std::collections::BTreeSet<_> = rows.iter().map(|r| &r.id).collect();
        assert_eq!(ids.len(), rows.len(), "ids must be unique");
    }
}

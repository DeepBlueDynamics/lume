//! Isolated, bounded offline document extraction.
use crate::bm25::Section;
use serde::{Deserialize, Serialize};
use std::{
    io::Read,
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

pub const INPUT_LIMIT: u64 = 128 << 20;
pub const UNIT_LIMIT: usize = 8 << 20;
pub const TEXT_LIMIT: usize = 64 << 20;
const OUTPUT_LIMIT: u64 = 128 << 20;
const RSS_LIMIT: u64 = 512 << 20;
const FILE_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Extraction {
    pub sections: Vec<Section>,
    pub skipped_units: usize,
    pub warnings: Vec<String>,
}
impl Extraction {
    fn skip(&mut self, reason: impl Into<String>) {
        self.skipped_units += 1;
        if self.warnings.len() < 8 {
            self.warnings
                .push(reason.into().chars().take(300).collect());
        }
    }
}

/// UV remains first for PDF; its failures fall back to the optional Rust backend.
pub fn extract(path: &Path, script: Option<&Path>) -> Result<Extraction, String> {
    let metadata = std::fs::metadata(path).map_err(|e| e.to_string())?;
    if metadata.len() > INPUT_LIMIT {
        return Err(format!("input exceeds {} MiB limit", INPUT_LIMIT >> 20));
    }
    let deadline = Instant::now() + FILE_TIMEOUT;
    let pdf = path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("pdf"));
    if pdf {
        if let Some(script) = script.filter(|p| p.is_file()) {
            let mut command = Command::new("uv");
            command.arg("run").arg(script).arg("pdf").arg(path);
            match bounded_output(command, deadline, RSS_LIMIT) {
                Ok(output) => match uv_result(path, &output) {
                    Ok(result) => return Ok(result),
                    Err(error) => {
                        eprintln!("PDF Python extractor unavailable: {error}; trying Rust")
                    }
                },
                Err(error) => eprintln!("PDF Python extractor unavailable: {error}; trying Rust"),
            }
        }
    }
    #[cfg(feature = "pdf")]
    {
        let mut command = Command::new(std::env::current_exe().map_err(|e| e.to_string())?);
        command.arg("__extract-document").arg(path);
        let output = bounded_output(command, deadline, RSS_LIMIT)?;
        serde_json::from_slice::<Result<Extraction, String>>(&output)
            .map_err(|e| format!("invalid Rust extraction output: {e}"))?
    }
    #[cfg(not(feature = "pdf"))]
    Err("offline PDF/EPUB extraction requires --features pdf (included by ti)".into())
}

fn uv_result(path: &Path, output: &[u8]) -> Result<Extraction, String> {
    let value: serde_json::Value = serde_json::from_slice(output).map_err(|e| e.to_string())?;
    if value["success"] != true {
        return Err(value["error"].as_str().unwrap_or("extractor failed").into());
    }
    let pages = value["pages"].as_array().ok_or("missing pages")?;
    let mut result = Extraction::default();
    let mut total = 0usize;
    for page in pages {
        let number = page["page_number"].as_u64().ok_or("missing page number")?;
        let text = page["text"].as_str().unwrap_or("");
        if text.trim().is_empty() || text.len() > UNIT_LIMIT {
            result.skip(format!("Page {number}: empty or oversized text"));
            continue;
        }
        total += text.len();
        if total > TEXT_LIMIT {
            return Err("total extracted text limit exceeded".into());
        }
        result.sections.push(section(
            path,
            format!("Page {number}"),
            number as usize,
            text.into(),
        ));
    }
    // The Python extractor omits empty pages from pages[].
    if let Some(count) = value["total_pages"].as_u64() {
        result.skipped_units += (count as usize).saturating_sub(pages.len());
    }
    Ok(result)
}

fn section(path: &Path, title: String, line_number: usize, body: String) -> Section {
    Section {
        title,
        body,
        line_number,
        filename: Some(path.to_string_lossy().into()),
        entities: vec![],
    }
}

fn stop(child: &mut std::process::Child) {
    #[cfg(unix)]
    {
        // Each extractor has its own process group; terminate UV's Python descendants too.
        let _ = Command::new("kill")
            .args(["-KILL", "--", &format!("-{}", child.id())])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    let _ = child.kill();
    let _ = child.wait();
}

fn bounded_output(
    mut command: Command,
    deadline: Instant,
    rss_limit: u64,
) -> Result<Vec<u8>, String> {
    if Instant::now() >= deadline {
        return Err("per-file extraction timeout".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;
    let stdout = child.stdout.take().ok_or("missing extractor stdout")?;
    let (sender, receiver) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut output = Vec::new();
        let result = stdout
            .take(OUTPUT_LIMIT + 1)
            .read_to_end(&mut output)
            .map_err(|e| e.to_string())
            .and_then(|_| {
                if output.len() as u64 > OUTPUT_LIMIT {
                    Err("extractor output limit exceeded".into())
                } else {
                    Ok(output)
                }
            });
        let _ = sender.send(result);
    });
    let mut status = None;
    let mut captured = None;
    let result = loop {
        if Instant::now() >= deadline {
            break Err("per-file extraction timeout".into());
        }
        if rss_bytes(child.id()) > rss_limit {
            break Err("extractor memory limit exceeded".into());
        }
        if captured.is_none() {
            match receiver.try_recv() {
                Ok(Ok(output)) => captured = Some(output),
                Ok(Err(error)) => break Err(error),
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    break Err("extractor reader disconnected".into());
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }
        if status.is_none() {
            match child.try_wait() {
                Ok(value) => status = value,
                Err(error) => break Err(error.to_string()),
            }
        }
        if let Some(status) = status.as_ref().filter(|_| captured.is_some()) {
            if !status.success() {
                break Err(format!("extractor exited with {status}"));
            }
            // Move the bounded output instead of duplicating the allocation.
            break Ok(captured.take().unwrap());
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    // Killing the group ensures the reader reaches EOF even when UV spawned Python.
    if result.is_err() {
        stop(&mut child);
    }
    let _ = reader.join();
    result
}

#[cfg(target_os = "linux")]
fn rss_bytes(pid: u32) -> u64 {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).unwrap_or_default();
    let own = status
        .lines()
        .find_map(|line| line.strip_prefix("VmRSS:"))
        .and_then(|line| line.split_whitespace().next())
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0)
        * 1024;
    let children =
        std::fs::read_to_string(format!("/proc/{pid}/task/{pid}/children")).unwrap_or_default();
    own + children
        .split_whitespace()
        .filter_map(|p| p.parse::<u32>().ok())
        .map(rss_bytes)
        .sum::<u64>()
}
#[cfg(not(target_os = "linux"))]
fn rss_bytes(_: u32) -> u64 {
    0
}

/// Worker entrypoint: parser panics and resource limits cannot stop the parent indexer.
#[cfg(feature = "pdf")]
pub fn worker(path: &Path) -> Result<Extraction, String> {
    if std::fs::metadata(path).map_err(|e| e.to_string())?.len() > INPUT_LIMIT {
        return Err("input limit exceeded".into());
    }
    if path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("epub"))
    {
        epub(path)
    } else {
        pdf(path)
    }
}

#[cfg(feature = "pdf")]
fn pdf(path: &Path) -> Result<Extraction, String> {
    let document = lopdf::Document::load_with_options(
        path,
        lopdf::LoadOptions::with_max_decompressed_size(UNIT_LIMIT),
    )
    .map_err(|e| format!("PDF cannot be read (possibly encrypted): {e}"))?;
    let pages = document.get_pages();
    let mut result = Extraction::default();
    if document.was_encrypted() || document.is_encrypted() {
        result.skipped_units = pages.len();
        result.warnings.push("encrypted PDF skipped".into());
        return Ok(result);
    }
    let mut total = 0usize;
    for number in pages.keys() {
        let extracted = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            document.extract_text_with_limit(&[*number], UNIT_LIMIT)
        }))
        .map_err(|_| "page decoder panicked".to_string())
        .and_then(|r| r.map_err(|e| e.to_string()));
        match extracted {
            Ok(text) if !text.trim().is_empty() && text.len() <= UNIT_LIMIT => {
                total += text.len();
                if total > TEXT_LIMIT {
                    return Err("total extracted text limit exceeded".into());
                }
                result.sections.push(section(
                    path,
                    format!("Page {number}"),
                    *number as usize,
                    text,
                ));
            }
            Ok(_) => result.skip(format!("Page {number}: image-only, empty or oversized")),
            Err(error) => result.skip(format!("Page {number}: {error}")),
        }
    }
    Ok(result)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    #[test]
    fn watchdog_kills_worker_and_descendants_at_deadline() {
        let mut command = Command::new("sh");
        command.args(["-c", "sleep 10"]);
        let started = Instant::now();
        let error =
            bounded_output(command, started + Duration::from_millis(40), RSS_LIMIT).unwrap_err();
        assert!(error.contains("timeout"), "{error}");
        assert!(started.elapsed() < Duration::from_secs(3));
    }
    #[cfg(target_os = "linux")]
    #[test]
    fn watchdog_reports_memory_budget() {
        let mut command = Command::new("sh");
        command.args(["-c", "sleep 10"]);
        let error =
            bounded_output(command, Instant::now() + Duration::from_secs(3), 1).unwrap_err();
        assert!(error.contains("memory"), "{error}");
    }
}

#[cfg(feature = "pdf")]
fn zip_text(archive: &mut zip::ZipArchive<std::fs::File>, name: &str) -> Result<String, String> {
    let entry = archive.by_name(name).map_err(|e| e.to_string())?;
    if entry.size() > UNIT_LIMIT as u64 {
        return Err(format!("{name}: chapter/metadata limit exceeded"));
    }
    let mut text = String::new();
    entry
        .take(UNIT_LIMIT as u64 + 1)
        .read_to_string(&mut text)
        .map_err(|e| e.to_string())?;
    if text.len() > UNIT_LIMIT {
        return Err(format!("{name}: decompression limit exceeded"));
    }
    Ok(text)
}

#[cfg(feature = "pdf")]
type XmlAttributeMap = std::collections::BTreeMap<String, String>;

#[cfg(feature = "pdf")]
fn xml_elements(text: &str) -> Result<Vec<(String, XmlAttributeMap)>, String> {
    use quick_xml::events::Event;
    let mut reader = quick_xml::Reader::from_str(text);
    let mut elements = Vec::new();
    loop {
        match reader.read_event().map_err(|e| e.to_string())? {
            Event::Start(element) | Event::Empty(element) => {
                let mut attributes = XmlAttributeMap::new();
                for attribute in element.attributes() {
                    let attribute = attribute.map_err(|e| e.to_string())?;
                    attributes.insert(
                        attribute.key.local_name().as_ref().to_string(),
                        attribute
                            .normalized_value(quick_xml::XmlVersion::Explicit1_0)
                            .map_err(|e| e.to_string())?
                            .into_owned(),
                    );
                }
                elements.push((element.local_name().as_ref().to_string(), attributes));
                if elements.len() > 10000 {
                    return Err("EPUB element count limit exceeded".into());
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(elements)
}

#[cfg(feature = "pdf")]
fn chapter_name(base: &Path, href: &str) -> Result<String, String> {
    let href = href.split('#').next().unwrap_or("");
    if href.starts_with('/') || href.contains(':') {
        return Err("external EPUB chapter reference skipped".into());
    }
    let bytes = href.as_bytes();
    let mut decoded = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let encoded = bytes
                .get(i + 1..i + 3)
                .ok_or("invalid EPUB percent escape")?;
            let encoded = std::str::from_utf8(encoded).map_err(|e| e.to_string())?;
            decoded.push(u8::from_str_radix(encoded, 16).map_err(|e| e.to_string())?);
            i += 3;
        } else {
            decoded.push(bytes[i]);
            i += 1;
        }
    }
    let decoded = String::from_utf8(decoded).map_err(|e| e.to_string())?;
    let joined = base.join(decoded).to_string_lossy().replace('\\', "/");
    let mut parts = Vec::new();
    for part in joined.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if parts.pop().is_none() {
                    return Err("EPUB chapter escapes archive root".into());
                }
            }
            other => parts.push(other),
        }
    }
    Ok(parts.join("/"))
}

#[cfg(feature = "pdf")]
fn epub(path: &Path) -> Result<Extraction, String> {
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| e.to_string())?;
    if archive.len() > 10000 {
        return Err("EPUB entry count limit exceeded".into());
    }
    let container = xml_elements(&zip_text(&mut archive, "META-INF/container.xml")?)?;
    let package = container
        .iter()
        .find(|(name, _)| name == "rootfile")
        .and_then(|(_, attrs)| attrs.get("full-path"))
        .ok_or("missing EPUB package")?
        .clone();
    let elements = xml_elements(&zip_text(&mut archive, &package)?)?;
    let mut items = std::collections::BTreeMap::new();
    for (name, attrs) in &elements {
        if name == "item"
            && attrs
                .get("media-type")
                .is_some_and(|t| t == "application/xhtml+xml" || t == "text/html")
        {
            if let (Some(id), Some(href)) = (attrs.get("id"), attrs.get("href")) {
                items.insert(id.clone(), href.clone());
            }
        }
    }
    let encrypted: std::collections::BTreeSet<_> = if archive
        .file_names()
        .any(|name| name == "META-INF/encryption.xml")
    {
        xml_elements(&zip_text(&mut archive, "META-INF/encryption.xml")?)?
            .into_iter()
            .filter(|(name, _)| name == "CipherReference")
            .filter_map(|(_, attrs)| attrs.get("URI").cloned())
            .collect()
    } else {
        Default::default()
    };
    let base = Path::new(&package).parent().unwrap_or(Path::new(""));
    let mut result = Extraction::default();
    let mut total = 0usize;
    for (name, attrs) in &elements {
        if name != "itemref" {
            continue;
        }
        let Some(href) = attrs.get("idref").and_then(|id| items.get(id)) else {
            result.skip("unsupported EPUB spine item");
            continue;
        };
        let chapter = match chapter_name(base, href) {
            Ok(chapter) => chapter,
            Err(error) => {
                result.skip(error);
                continue;
            }
        };
        if encrypted.contains(&chapter) {
            result.skip(format!("{chapter}: encrypted chapter"));
            continue;
        }
        let html = match zip_text(&mut archive, &chapter) {
            Ok(html) => html,
            Err(error) => {
                result.skip(error);
                continue;
            }
        };
        let (title, body) = crate::crawl::clean_html_to_markdown(&html);
        if body.trim().is_empty() {
            result.skip(format!("{chapter}: empty chapter"));
            continue;
        }
        total += body.len();
        if total > TEXT_LIMIT {
            return Err("total extracted text limit exceeded".into());
        }
        let number = result.sections.len() + result.skipped_units + 1;
        result.sections.push(section(
            path,
            if title.trim().is_empty() {
                format!("Chapter {number}")
            } else {
                title
            },
            number,
            body,
        ));
    }
    Ok(result)
}

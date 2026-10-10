//! Best-effort v4 retention. Concurrent writers are not supported.
use super::generation::{self, Manifest};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const HISTORY: &str = ".retention.json";
const MAX_HISTORY_BYTES: u64 = 1024 * 1024;

#[derive(Clone, Debug)]
pub struct Options {
    pub keep_previous: usize,
    pub grace_secs: u64,
    pub dry_run: bool,
    pub include_unknown: bool,
}
impl Default for Options {
    fn default() -> Self {
        Self {
            keep_previous: 1,
            grace_secs: 600,
            dry_run: false,
            include_unknown: false,
        }
    }
}
#[derive(Debug, Default)]
pub struct Report {
    pub removed: Vec<PathBuf>,
    pub eligible: Vec<PathBuf>,
    pub unknown: Vec<PathBuf>,
    pub warnings: Vec<String>,
}
#[derive(Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct History {
    version: u32,
    retired: Vec<Retired>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Retired {
    generation: String,
    retired_at: u64,
}
fn now() -> Result<u64, String> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|t| t.as_secs())
        .map_err(|e| e.to_string())
}
fn directory(root: &Path) -> Result<PathBuf, String> {
    let root = fs::canonicalize(root)
        .map_err(|e| format!("Cannot resolve index {}: {e}", root.display()))?;
    let path = root.join("generations");
    let metadata = fs::symlink_metadata(&path)
        .map_err(|e| format!("Cannot inspect {}: {e}", path.display()))?;
    if !metadata.is_dir() || unsafe_link(&metadata) {
        return Err(format!("Unsafe generations directory {}", path.display()));
    }
    let actual =
        fs::canonicalize(&path).map_err(|e| format!("Cannot resolve {}: {e}", path.display()))?;
    if actual.parent() != Some(root.as_path()) {
        return Err(format!(
            "Generations directory escapes index {}",
            path.display()
        ));
    }
    Ok(actual)
}
fn unsafe_link(metadata: &fs::Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return true;
        }
    }
    false
}
fn history(directory: &Path) -> Result<History, String> {
    let path = directory.join(HISTORY);
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(History {
                version: 1,
                retired: Vec::new(),
            })
        }
        Err(error) => return Err(format!("Cannot inspect {}: {error}", path.display())),
    };
    if !metadata.is_file() || unsafe_link(&metadata) || metadata.len() > MAX_HISTORY_BYTES {
        return Err(format!(
            "Unsafe or oversized retention history {}",
            path.display()
        ));
    }
    let mut bytes = Vec::new();
    fs::File::open(&path)
        .map_err(|e| format!("Cannot open {}: {e}", path.display()))?
        .take(MAX_HISTORY_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("Cannot read {}: {e}", path.display()))?;
    if bytes.len() as u64 > MAX_HISTORY_BYTES {
        return Err(format!(
            "Retention history exceeds 1 MiB {}",
            path.display()
        ));
    }
    let value: History = serde_json::from_slice(&bytes)
        .map_err(|e| format!("Invalid retention history {}: {e}", path.display()))?;
    let mut seen = HashSet::new();
    if value.version != 1
        || value
            .retired
            .iter()
            .any(|r| !generation::valid_generation(&r.generation) || !seen.insert(&r.generation))
    {
        return Err(format!(
            "Invalid retention history entries {}",
            path.display()
        ));
    }
    Ok(value)
}

/// Write before pointer replacement: only the old, witnessed-current ID is recorded.
pub(super) fn before_publish(root: &Path) -> Result<(), String> {
    let directory = directory(root)?;
    let mut history = history(&directory)?;
    let pointer = root.join(generation::POINTER);
    if pointer.exists() {
        let current = generation::read_manifest(root)?;
        history
            .retired
            .retain(|r| r.generation != current.generation);
        history.retired.insert(
            0,
            Retired {
                generation: current.generation,
                retired_at: now()?,
            },
        );
    }
    let bytes = serde_json::to_vec(&history).map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_HISTORY_BYTES {
        return Err("Retention history exceeds 1 MiB; run index gc before publishing".into());
    }
    crate::search::save_json(&directory.join(HISTORY), &history)
}

pub(super) fn after_publish(root: &Path) {
    match collect(root, &Options::default()) {
        Ok(report) => {
            for warning in report.warnings {
                eprintln!("[⚠️] Generation GC: {warning}");
            }
        }
        Err(error) => eprintln!("[⚠️] Generation GC skipped for {}: {error}", root.display()),
    }
}

/// Reject all links/reparse points before removal, including nested ones.
fn safe_tree(path: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|e| format!("Cannot inspect {}: {e}", path.display()))?;
    if unsafe_link(&metadata) || (!metadata.is_file() && !metadata.is_dir()) {
        return Err(format!("Unsafe generation entry {}", path.display()));
    }
    if metadata.is_dir() {
        for entry in
            fs::read_dir(path).map_err(|e| format!("Cannot enumerate {}: {e}", path.display()))?
        {
            let entry = entry.map_err(|e| format!("Cannot enumerate {}: {e}", path.display()))?;
            safe_tree(&entry.path())?;
        }
    }
    Ok(())
}

pub fn collect(root: &Path, options: &Options) -> Result<Report, String> {
    collect_with(root, options, now()?, |path| fs::remove_dir_all(path))
}

fn collect_with(
    root: &Path,
    options: &Options,
    now: u64,
    mut remove: impl FnMut(&Path) -> std::io::Result<()>,
) -> Result<Report, String> {
    if options.keep_previous > 100 {
        return Err("keep-generations must be at most 100".into());
    }
    let directory = directory(root)?;
    let current: Manifest = generation::read_manifest(root)?;
    let mut history = history(&directory)?;
    let previous: HashSet<_> = history
        .retired
        .iter()
        .filter(|r| r.generation != current.generation)
        .take(options.keep_previous)
        .map(|r| r.generation.clone())
        .collect();
    let mut report = Report::default();
    let mut candidates = fs::read_dir(&directory)
        .map_err(|e| format!("Cannot enumerate {}: {e}", directory.display()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("Cannot enumerate {}: {e}", directory.display()))?;
    candidates.sort_by_key(|entry| entry.file_name());
    for entry in candidates {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !generation::valid_generation(name)
            || name == current.generation
            || previous.contains(name)
        {
            continue;
        }
        let path = entry.path();
        let metadata = match fs::symlink_metadata(&path) {
            Ok(value) => value,
            Err(error) => {
                report
                    .warnings
                    .push(format!("Cannot inspect {}: {error}", path.display()));
                continue;
            }
        };
        if !metadata.is_dir() || unsafe_link(&metadata) {
            report
                .warnings
                .push(format!("Skipping unsafe generation {}", path.display()));
            continue;
        }
        let retired_at = match history.retired.iter().find(|r| r.generation == name) {
            Some(retired) => retired.retired_at,
            None => {
                report.unknown.push(path.clone());
                if !options.include_unknown {
                    continue;
                }
                match metadata.modified().and_then(|time| {
                    time.duration_since(UNIX_EPOCH)
                        .map_err(std::io::Error::other)
                }) {
                    Ok(time) => time.as_secs(),
                    Err(error) => {
                        report
                            .warnings
                            .push(format!("Cannot date {}: {error}", path.display()));
                        continue;
                    }
                }
            }
        };
        // Clock rollback/future timestamps never accelerate collection.
        if now < retired_at || now - retired_at < options.grace_secs {
            continue;
        }
        if let Err(error) = safe_tree(&path) {
            report.warnings.push(error);
            continue;
        }
        let actual = fs::canonicalize(&path)
            .map_err(|e| format!("Cannot resolve {}: {e}", path.display()))?;
        if actual.parent() != Some(directory.as_path()) {
            report.warnings.push(format!(
                "Skipping generation outside directory {}",
                path.display()
            ));
            continue;
        }
        // Extra defensive check, not a guarantee for unsupported concurrent writers.
        if generation::read_manifest(root)?.generation != current.generation {
            return Err("Index pointer changed during GC; retry with no concurrent writer".into());
        }
        report.eligible.push(path.clone());
        if !options.dry_run {
            match remove(&path) {
                Ok(()) => report.removed.push(path),
                Err(error) => report.warnings.push(format!(
                    "Cannot remove generation {}: {error}; retry next GC",
                    path.display()
                )),
            }
        }
    }
    if !report.removed.is_empty() {
        history.retired.retain(|r| {
            !report.removed.iter().any(|path| {
                path.file_name()
                    .is_some_and(|name| name == r.generation.as_str())
            })
        });
        if let Err(error) = crate::search::save_json(&directory.join(HISTORY), &history) {
            report.warnings.push(format!(
                "Cannot update retention history {}: {error}",
                directory.join(HISTORY).display()
            ));
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!("lume-gc-{}", crate::uuid_v4()));
            fs::create_dir(&root).unwrap();
            Self(root)
        }
        fn publish(&self, fail: bool) -> Result<Manifest, String> {
            generation::publish(
                &self.0,
                Manifest {
                    format_version: 4,
                    generation: crate::uuid_v4(),
                    sections: 0,
                    source_files: 0,
                    corpus_fingerprint: [0; 2],
                    segments: BTreeMap::new(),
                    entity_overlay: None,
                },
                &generation::CORE_FILES
                    .iter()
                    .map(|name| ((*name).into(), vec![0]))
                    .collect(),
                |step| {
                    if fail && step == generation::PublishStep::RetentionSaved {
                        Err("injected pointer replacement failure".into())
                    } else {
                        Ok(())
                    }
                },
            )
        }
        fn path(&self, id: &str) -> PathBuf {
            self.0.join("generations").join(id)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn options(keep: usize) -> Options {
        Options {
            keep_previous: keep,
            grace_secs: 0,
            ..Options::default()
        }
    }

    #[test]
    fn retains_current_and_requested_previous_and_removes_owned_overlays() {
        let fixture = Fixture::new();
        let a = fixture.publish(false).unwrap();
        fs::create_dir(fixture.path(&a.generation).join("overlays")).unwrap();
        fs::write(
            fixture.path(&a.generation).join("overlays/node.json"),
            b"old",
        )
        .unwrap();
        let b = fixture.publish(false).unwrap();
        let c = fixture.publish(false).unwrap();
        let preview = collect(
            &fixture.0,
            &Options {
                dry_run: true,
                ..options(1)
            },
        )
        .unwrap();
        assert_eq!(preview.eligible, [fixture.path(&a.generation)]);
        assert!(fixture.path(&a.generation).exists());
        let report = collect(&fixture.0, &options(1)).unwrap();
        assert_eq!(report.removed, [fixture.path(&a.generation)]);
        assert!(fixture.path(&b.generation).exists());
        assert!(fixture.path(&c.generation).exists());
        collect(&fixture.0, &options(0)).unwrap();
        assert!(!fixture.path(&b.generation).exists());
        assert!(fixture.path(&c.generation).exists());
        assert!(collect(&fixture.0, &options(101)).is_err());
    }

    #[test]
    fn grace_and_clock_rollback_keep_recently_retired_generations() {
        let fixture = Fixture::new();
        let a = fixture.publish(false).unwrap();
        fixture.publish(false).unwrap();
        let dir = directory(&fixture.0).unwrap();
        let mut h = history(&dir).unwrap();
        h.retired[0].retired_at = 1_000;
        crate::search::save_json(&dir.join(HISTORY), &h).unwrap();
        let opts = Options {
            keep_previous: 0,
            grace_secs: 600,
            ..Options::default()
        };
        for time in [999, 1_000, 1_599] {
            assert!(
                collect_with(&fixture.0, &opts, time, |p| fs::remove_dir_all(p))
                    .unwrap()
                    .removed
                    .is_empty()
            );
        }
        assert_eq!(
            collect_with(&fixture.0, &opts, 1_600, |p| fs::remove_dir_all(p))
                .unwrap()
                .removed,
            [fixture.path(&a.generation)]
        );
    }

    #[test]
    fn unknown_generation_requires_opt_in_and_mtime_grace() {
        let fixture = Fixture::new();
        fixture.publish(false).unwrap();
        let orphan = fixture.path(&crate::uuid_v4());
        fs::create_dir(&orphan).unwrap();
        let stamp = fs::metadata(&orphan)
            .unwrap()
            .modified()
            .unwrap()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let report = collect(&fixture.0, &options(1)).unwrap();
        assert_eq!(report.unknown, [orphan.clone()]);
        assert!(orphan.exists());
        let opts = Options {
            include_unknown: true,
            grace_secs: 600,
            ..Options::default()
        };
        assert!(
            collect_with(&fixture.0, &opts, stamp + 599, |p| fs::remove_dir_all(p))
                .unwrap()
                .removed
                .is_empty()
        );
        assert_eq!(
            collect_with(&fixture.0, &opts, stamp + 600, |p| fs::remove_dir_all(p))
                .unwrap()
                .removed,
            [orphan]
        );
    }

    #[test]
    fn pointer_failure_keeps_current_and_real_rollback_generation() {
        let fixture = Fixture::new();
        let a = fixture.publish(false).unwrap();
        let b = fixture.publish(false).unwrap();
        let pointer = fs::read(fixture.0.join(generation::POINTER)).unwrap();
        assert!(fixture.publish(true).is_err());
        assert_eq!(
            fs::read(fixture.0.join(generation::POINTER)).unwrap(),
            pointer
        );
        collect(&fixture.0, &options(1)).unwrap();
        assert!(fixture.path(&a.generation).exists());
        assert!(fixture.path(&b.generation).exists());
        // A failed history save also must not replace the pointer.
        fs::remove_file(fixture.0.join("generations").join(HISTORY)).unwrap();
        fs::create_dir(fixture.0.join("generations").join(HISTORY)).unwrap();
        assert!(fixture.publish(false).is_err());
        assert_eq!(
            fs::read(fixture.0.join(generation::POINTER)).unwrap(),
            pointer
        );
        assert!(collect(&fixture.0, &options(0)).is_err());
        assert!(fixture.path(&a.generation).exists());
    }

    #[test]
    fn deletion_failure_is_reported_and_next_collection_retries() {
        let fixture = Fixture::new();
        let a = fixture.publish(false).unwrap();
        let b = fixture.publish(false).unwrap();
        assert_eq!(
            generation::read_manifest(&fixture.0).unwrap().generation,
            b.generation
        );
        let report = collect_with(&fixture.0, &options(0), now().unwrap(), |_| {
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "injected sharing violation",
            ))
        })
        .unwrap();
        assert!(report.removed.is_empty());
        assert!(report.warnings[0].contains("injected sharing violation"));
        assert!(report.warnings[0].contains(&fixture.path(&a.generation).display().to_string()));
        assert_eq!(
            generation::read_manifest(&fixture.0).unwrap().generation,
            b.generation
        );
        assert_eq!(
            collect(&fixture.0, &options(0)).unwrap().removed,
            [fixture.path(&a.generation)]
        );
    }

    #[test]
    fn corrupt_pointer_and_history_fail_closed() {
        let fixture = Fixture::new();
        let a = fixture.publish(false).unwrap();
        fixture.publish(false).unwrap();
        let history_path = fixture.0.join("generations").join(HISTORY);
        let saved = fs::read(&history_path).unwrap();
        fs::write(&history_path, b"corrupt").unwrap();
        assert!(collect(&fixture.0, &options(0)).is_err());
        assert!(fixture.path(&a.generation).exists());
        fs::write(history_path, saved).unwrap();
        fs::write(fixture.0.join(generation::POINTER), b"corrupt").unwrap();
        assert!(collect(&fixture.0, &options(0)).is_err());
        assert!(fixture.path(&a.generation).exists());
    }

    #[cfg(unix)]
    #[test]
    fn links_and_unknown_names_never_delete_outside_data() {
        let fixture = Fixture::new();
        fixture.publish(false).unwrap();
        let outside = fixture.0.join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("sentinel"), b"keep").unwrap();
        let alias = fixture.path(&crate::uuid_v4());
        std::os::unix::fs::symlink(&outside, &alias).unwrap();
        let nested = fixture.path(&crate::uuid_v4());
        fs::create_dir(&nested).unwrap();
        std::os::unix::fs::symlink(&outside, nested.join("link")).unwrap();
        let unknown_name = fixture.path("do-not-delete");
        fs::create_dir(&unknown_name).unwrap();
        let report = collect(
            &fixture.0,
            &Options {
                include_unknown: true,
                ..options(0)
            },
        )
        .unwrap();
        assert!(report.removed.is_empty());
        assert!(outside.join("sentinel").exists());
        assert!(unknown_name.exists());
        assert!(alias.symlink_metadata().is_ok());
    }
}

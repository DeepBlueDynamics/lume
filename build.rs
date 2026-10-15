use std::path::{Path, PathBuf};
use std::process::Command;

fn git_watch_paths(manifest_dir: &Path) -> Vec<PathBuf> {
    let dot_git = manifest_dir.join(".git");
    if dot_git.is_dir() {
        return vec![dot_git.join("HEAD"), dot_git.join("refs")];
    }
    if dot_git.is_file() {
        if let Ok(contents) = std::fs::read_to_string(&dot_git) {
            if let Some(raw) = contents.trim().strip_prefix("gitdir:") {
                let gitdir = manifest_dir.join(raw.trim());
                return vec![gitdir.join("HEAD"), gitdir.join("commondir")];
            }
        }
    }
    Vec::new()
}

fn detect_commit(manifest_dir: &Path) -> String {
    if let Ok(val) = std::env::var("LUME_BUILD_COMMIT") {
        let trimmed = val.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    if let Ok(output) = Command::new("git")
        .args(["rev-parse", "--short=7", "HEAD"])
        .current_dir(manifest_dir)
        .output()
    {
        if output.status.success() {
            if let Ok(text) = String::from_utf8(output.stdout) {
                let trimmed = text.trim();
                if !trimmed.is_empty() {
                    return trimmed.to_string();
                }
            }
        }
    }
    if let Ok(sha) = std::env::var("GITHUB_SHA") {
        let trimmed = sha.trim();
        if !trimmed.is_empty() {
            return trimmed.chars().take(7).collect();
        }
    }
    "unknown".to_string()
}

fn main() {
    println!("cargo:rerun-if-env-changed=LUME_BUILD_COMMIT");
    println!("cargo:rerun-if-env-changed=GITHUB_SHA");
    let manifest_dir =
        PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".to_string()));
    for path in git_watch_paths(&manifest_dir) {
        if path.exists() {
            println!("cargo:rerun-if-changed={}", path.display());
        }
    }
    let commit = detect_commit(&manifest_dir);
    println!("cargo:rustc-env=LUME_BUILD_COMMIT={commit}");
}

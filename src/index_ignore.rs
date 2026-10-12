//! Which files `lume index` skips: a gitignore-style subset read from the
//! index root's `.gitignore` and `.lumeignore`, on top of built-in defaults.
//!
//! Supported per line: `#` comments, `!` negation (the last matching rule
//! wins), a trailing `/` for directories only, a leading `/` (or any inner `/`)
//! to anchor the pattern to the index root, and the globs `*`, `?` and `**`.
//! A pattern without a slash matches a file or directory name at any depth.
//! Only the root's two files are read; nested `.gitignore` files are not.

use std::path::Path;

/// Skipped unless a rule re-includes them: macOS AppleDouble sidecars and
/// Finder metadata, and JavaScript dependency trees.
pub const DEFAULT_RULES: &[&str] = &["._*", ".DS_Store", "node_modules/"];

#[derive(Debug, Clone, PartialEq, Eq)]
struct Rule {
    pattern: String,
    negate: bool,
    dir_only: bool,
    anchored: bool,
}

/// Ordered ignore rules; later rules override earlier ones.
#[derive(Debug, Clone, Default)]
pub struct IgnoreRules {
    rules: Vec<Rule>,
    /// Rule count per source, for the indexing log.
    pub sources: Vec<(String, usize)>,
}

impl IgnoreRules {
    /// Defaults, then the root `.gitignore` (when `use_gitignore`), then the
    /// root `.lumeignore`, which has the last word.
    pub fn load(root: &Path, use_gitignore: bool) -> Self {
        let mut rules = Self::default();
        rules.add_lines("defaults", DEFAULT_RULES.iter().copied());
        if use_gitignore {
            if let Ok(text) = std::fs::read_to_string(root.join(".gitignore")) {
                rules.add_lines(".gitignore", text.lines());
            }
        }
        if let Ok(text) = std::fs::read_to_string(root.join(".lumeignore")) {
            rules.add_lines(".lumeignore", text.lines());
        }
        rules
    }

    /// Rules parsed from `lines` alone, without defaults.
    pub fn from_lines<'a>(lines: impl IntoIterator<Item = &'a str>) -> Self {
        let mut rules = Self::default();
        rules.add_lines("rules", lines);
        rules
    }

    fn add_lines<'a>(&mut self, source: &str, lines: impl IntoIterator<Item = &'a str>) {
        let before = self.rules.len();
        self.rules.extend(lines.into_iter().filter_map(parse_rule));
        self.sources
            .push((source.to_string(), self.rules.len() - before));
    }

    /// Whether `path` (under `root`) should be skipped.
    pub fn is_ignored(&self, path: &Path, root: &Path, is_dir: bool) -> bool {
        let rel = path
            .strip_prefix(root)
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or_default();
        self.is_ignored_rel(&rel, is_dir)
    }

    /// Whether the root-relative, `/`-separated path `rel` should be skipped.
    pub fn is_ignored_rel(&self, rel: &str, is_dir: bool) -> bool {
        let name = rel.rsplit('/').next().unwrap_or(rel);
        let mut ignored = false;
        for rule in &self.rules {
            if rule.dir_only && !is_dir {
                continue;
            }
            let hit = if rule.anchored {
                glob_match(&rule.pattern, rel)
            } else {
                glob_match(&rule.pattern, name)
            };
            if hit {
                ignored = !rule.negate;
            }
        }
        ignored
    }
}

fn parse_rule(line: &str) -> Option<Rule> {
    let line = line.trim_end();
    let trimmed = line.trim_start();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return None;
    }
    let mut pattern = trimmed.replace('\\', "/");
    let negate = pattern.starts_with('!');
    if negate {
        pattern.remove(0);
    }
    let dir_only = pattern.ends_with('/');
    let pattern = pattern.trim_end_matches('/');
    let leading_slash = pattern.starts_with('/');
    let body = pattern.trim_start_matches('/');
    // `**/name` matches the name at any depth, exactly like a bare `name`.
    let body = match body.strip_prefix("**/") {
        Some(rest) if !rest.contains('/') => rest,
        _ => body,
    };
    if body.is_empty() {
        return None;
    }
    Some(Rule {
        anchored: leading_slash || body.contains('/'),
        pattern: body.to_string(),
        negate,
        dir_only,
    })
}

/// Glob match: `*` and `?` stay within one path segment, `**` crosses `/`.
pub fn glob_match(pattern: &str, text: &str) -> bool {
    fn go(p: &[u8], t: &[u8]) -> bool {
        match p.first() {
            None => t.is_empty(),
            Some(b'*') if p.get(1) == Some(&b'*') => {
                // `**/` may also match zero segments.
                let rest = &p[2..];
                let rest = rest.strip_prefix(b"/").unwrap_or(rest);
                (0..=t.len()).any(|i| go(rest, &t[i..]))
            }
            Some(b'*') => {
                let mut i = 0;
                loop {
                    if go(&p[1..], &t[i..]) {
                        return true;
                    }
                    if i == t.len() || t[i] == b'/' {
                        return false;
                    }
                    i += 1;
                }
            }
            Some(b'?') => !t.is_empty() && t[0] != b'/' && go(&p[1..], &t[1..]),
            Some(&c) => t.first() == Some(&c) && go(&p[1..], &t[1..]),
        }
    }
    go(pattern.as_bytes(), text.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs_respect_segments() {
        assert!(glob_match("._*", "._report.pdf"));
        assert!(!glob_match("._*", "report.pdf"));
        assert!(glob_match("*.log", "build.log"));
        assert!(!glob_match("*.log", "logs/build.log"));
        assert!(glob_match("a?c", "abc"));
        assert!(glob_match("docs/**/*.md", "docs/a/b/c.md"));
        assert!(glob_match("docs/**/*.md", "docs/c.md"));
        assert!(!glob_match("docs/*.md", "docs/a/c.md"));
    }

    #[test]
    fn bare_names_match_at_any_depth_and_anchors_do_not() {
        let rules = IgnoreRules::from_lines(["dist", "/build", "data/raw", "*.tmp"]);
        assert!(rules.is_ignored_rel("dist", true));
        assert!(rules.is_ignored_rel("web/dist", true));
        assert!(rules.is_ignored_rel("build", true));
        assert!(!rules.is_ignored_rel("web/build", true));
        assert!(rules.is_ignored_rel("data/raw", true));
        assert!(!rules.is_ignored_rel("other/data/raw", true));
        assert!(rules.is_ignored_rel("a/b/x.tmp", false));
    }

    #[test]
    fn directory_only_rules_skip_files_with_the_same_name() {
        let rules = IgnoreRules::from_lines(["out/"]);
        assert!(rules.is_ignored_rel("out", true));
        assert!(!rules.is_ignored_rel("out", false));
    }

    #[test]
    fn negation_and_comments() {
        let rules = IgnoreRules::from_lines(["# comment", "", "*.md", "!keep.md"]);
        assert!(rules.is_ignored_rel("notes.md", false));
        assert!(!rules.is_ignored_rel("keep.md", false));
        assert!(!rules.is_ignored_rel("# comment", false));
    }

    #[test]
    fn defaults_skip_appledouble_finder_and_node_modules() {
        let rules = IgnoreRules::from_lines(DEFAULT_RULES.iter().copied());
        assert!(rules.is_ignored_rel("papers/._paper.pdf", false));
        assert!(rules.is_ignored_rel(".DS_Store", false));
        assert!(rules.is_ignored_rel("web/node_modules", true));
        assert!(!rules.is_ignored_rel("papers/paper.pdf", false));
    }

    #[test]
    fn lumeignore_overrides_gitignore_and_defaults() {
        let root = std::env::temp_dir().join(format!("lume-ignore-{}", crate::uuid_v4()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join(".gitignore"), "*.md\nsecret/\n").unwrap();
        std::fs::write(root.join(".lumeignore"), "!README.md\n!._keep.txt\n").unwrap();
        let rules = IgnoreRules::load(&root, true);
        assert!(rules.is_ignored(&root.join("notes.md"), &root, false));
        assert!(!rules.is_ignored(&root.join("README.md"), &root, false));
        assert!(rules.is_ignored(&root.join("secret"), &root, true));
        assert!(!rules.is_ignored(&root.join("._keep.txt"), &root, false));
        let without_git = IgnoreRules::load(&root, false);
        assert!(!without_git.is_ignored(&root.join("notes.md"), &root, false));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn legacy_exact_lumeignore_entries_still_match() {
        let rules = IgnoreRules::from_lines(["vendor", "data/archive/"]);
        assert!(rules.is_ignored_rel("vendor", true));
        assert!(rules.is_ignored_rel("lib/vendor", true));
        assert!(rules.is_ignored_rel("data/archive", true));
    }
}

//! Bodyless source bookkeeping for v4; query opens never reconstruct cached bodies.
use crate::bm25::Section;
use crate::search::IndexState;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    pub path: String,
    pub modified: u64,
    pub first_section: u32,
    pub section_count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildState {
    pub settings: IndexState,
    pub sources: Vec<Source>,
}

impl BuildState {
    pub fn from_legacy(state: &IndexState, sections: &[Section]) -> Result<Self, String> {
        let mut paths: Vec<_> = state.cached_files.keys().collect();
        paths.sort();
        let mut sources = Vec::with_capacity(paths.len());
        let mut cursor = 0usize;
        for path in paths {
            let (modified, cached) = &state.cached_files[path];
            let end = cursor
                .checked_add(cached.len())
                .ok_or("Source section range overflow")?;
            let actual = sections
                .get(cursor..end)
                .ok_or("Source sections exceed index")?;
            // IDs and file ordering must agree with the legacy indexing order.
            if actual.iter().zip(cached).any(|(a, b)| {
                a.title != b.title
                    || a.body != b.body
                    || a.filename != b.filename
                    || a.line_number != b.line_number
                    || a.entities != b.entities
            }) {
                return Err(format!("Source section range disagrees with index: {path}"));
            }
            sources.push(Source {
                path: path.clone(),
                modified: *modified,
                first_section: u32::try_from(cursor)
                    .map_err(|_| "Source section ID exceeds u32")?,
                section_count: u32::try_from(cached.len())
                    .map_err(|_| "Source section count exceeds u32")?,
            });
            cursor = end;
        }
        if cursor != sections.len() {
            return Err("Source ranges do not cover index".into());
        }
        // Clone settings only; cloning cached bodies just to clear them adds a
        // corpus-sized temporary allocation at the start of publication.
        let settings = IndexState {
            format_version: 4,
            target_dir: state.target_dir.clone(),
            db_dir: state.db_dir.clone(),
            semantic_enabled: state.semantic_enabled,
            ollama_entities: state.ollama_entities,
            ollama_model: state.ollama_model.clone(),
            ollama_url: state.ollama_url.clone(),
            tag_dict_path: state.tag_dict_path.clone(),
            semantic_session_id: state.semantic_session_id.clone(),
            cached_files: HashMap::new(),
            stemmed: state.stemmed,
            keep_hyphens: state.keep_hyphens,
        };
        Ok(Self { settings, sources })
    }

    pub fn validate(&self, section_count: usize) -> Result<(), String> {
        if self.settings.format_version != 4 || !self.settings.cached_files.is_empty() {
            return Err("Invalid v4 build settings or duplicated section bodies".into());
        }
        let mut cursor = 0usize;
        let mut previous: Option<&str> = None;
        for source in &self.sources {
            if previous.is_some_and(|path| path >= source.path.as_str())
                || source.first_section as usize != cursor
            {
                return Err("Noncanonical source ranges".into());
            }
            cursor = cursor
                .checked_add(source.section_count as usize)
                .ok_or("Source range overflow")?;
            if cursor > section_count {
                return Err("Source range exceeds index".into());
            }
            previous = Some(&source.path);
        }
        if cursor != section_count {
            return Err("Source ranges do not cover index".into());
        }
        Ok(())
    }

    /// Explicitly used by incremental indexing, never by a query open.
    pub fn restore_cached_files(&self, sections: &[Section]) -> Result<IndexState, String> {
        self.validate(sections.len())?;
        let mut state = self.settings.clone();
        state.cached_files = HashMap::with_capacity(self.sources.len());
        for source in &self.sources {
            let start = source.first_section as usize;
            let end = start + source.section_count as usize;
            state.cached_files.insert(
                source.path.clone(),
                (source.modified, sections[start..end].to_vec()),
            );
        }
        Ok(state)
    }
}

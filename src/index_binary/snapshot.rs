//! Shared v4 detection and snapshot reconstruction for all ordinary-index readers.
use super::{build_state::BuildState, generation};
use crate::bm25::Bm25Index;
use crate::search::{IndexState, LoadedIndex, OpenEnvChecks};
use std::path::Path;
use std::sync::OnceLock;

pub fn present(root: &Path) -> bool {
    // Existence selects the new reader; corruption must never fall back to JSON.
    root.join(generation::POINTER).exists()
}

pub fn open(root: &Path, checks: OpenEnvChecks) -> Result<LoadedIndex, String> {
    let manifest = generation::read_manifest(root)?;
    let segments = generation::read_segments(root, &manifest)?;
    let bm25 = Bm25Index::from_v4_segments_for_open(&segments)?;
    let build_span = crate::index_timing::Span::new("v4.decode.build_state");
    let build: BuildState = serde_json::from_slice(&segments["build-state.json"])
        .map_err(|e| format!("Invalid v4 build state: {e}"))?;
    build.validate(bm25.sections.len())?;
    if manifest.sections as usize != bm25.sections.len()
        || manifest.source_files as usize != build.sources.len()
    {
        return Err("V4 pointer counts disagree with snapshot".into());
    }
    drop(build_span);
    let state = build.settings;
    if state.keep_hyphens || bm25.keep_hyphens || state.stemmed != bm25.stemmed {
        return Err("V4 analyzer settings disagree or use unsupported keep_hyphens".into());
    }
    if checks
        .check_stem
        .is_some_and(|value| value != state.stemmed)
    {
        return Err("Stemming configuration mismatch for v4 index".into());
    }
    let decode = |name: &str| -> Result<Option<serde_json::Value>, String> {
        segments
            .get(name)
            .map(|bytes| {
                serde_json::from_slice(bytes)
                    .map_err(|e| format!("Invalid v4 ancillary segment {name}: {e}"))
            })
            .transpose()
    };
    let spelling_span = crate::index_timing::Span::new("v4.decode.spelling");
    let spelling = decode("spelling.json")?
        .map(serde_json::from_value)
        .transpose()
        .map_err(|e| format!("Invalid v4 spelling: {e}"))?;
    drop(spelling_span);
    let graph_span = crate::index_timing::Span::new("v4.decode.graph");
    let entity_graph = decode("entity_graph.json")?
        .map(serde_json::from_value)
        .transpose()
        .map_err(|e| format!("Invalid v4 entity graph: {e}"))?;
    drop(graph_span);
    let directory = generation::generation_directory(root, &manifest)?;
    let meta = if segments.contains_key("meta.json") {
        let value = crate::meta::MetaIndex::open(&directory.join("meta.json"))?;
        if value.num_sections != bm25.sections.len() {
            return Err("V4 metadata section count mismatch".into());
        }
        Some(value)
    } else {
        None
    };
    let tagger = state
        .tag_dict_path
        .as_ref()
        .and_then(|path| crate::search::load_tagger_csv(Path::new(path)).ok());
    let local_vectors = crate::local_vectors::LocalVectors::open(&directory, &bm25.sections)?;
    if let Ok(model) = std::env::var("LUME_EMBED_MODEL") {
        if local_vectors
            .as_ref()
            .is_none_or(|vectors| vectors.profile.model != model)
        {
            return Err("Requested embedding model does not match the local-vector index; reindex with --embed-model".into());
        }
    }
    if let Ok(dimensions) = std::env::var("LUME_EMBED_DIMENSIONS") {
        let dimensions = dimensions
            .parse::<usize>()
            .map_err(|_| "Invalid --embed-dimensions")?;
        if local_vectors
            .as_ref()
            .is_some_and(|vectors| vectors.profile.dimensions != dimensions)
        {
            return Err(
                "Requested embedding dimensions do not match the local-vector index".into(),
            );
        }
    }
    Ok(LoadedIndex {
        state: Some(state),
        bm25,
        spelling,
        entity_graph,
        tagger,
        // Mutable session caches stay outside the immutable generation.
        cache_dir: Some(root.to_path_buf()),
        meta,
        local_vectors,
        corpus_fingerprint: OnceLock::from((
            manifest.corpus_fingerprint[0],
            manifest.corpus_fingerprint[1],
        )),
    })
}

pub fn settings(root: &Path) -> Result<IndexState, String> {
    if !present(root) {
        return crate::search::load_json(&root.join("state.json"));
    }
    let manifest = generation::read_manifest(root)?;
    let directory = generation::generation_directory(root, &manifest)?;
    let bytes = std::fs::read(directory.join("build-state.json")).map_err(|e| e.to_string())?;
    let seal = &manifest.segments["build-state.json"];
    if bytes.len() as u64 != seal.bytes || generation::sha256(&bytes) != seal.sha256 {
        return Err("V4 build-state seal mismatch".into());
    }
    let build: BuildState = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    build.validate(manifest.sections as usize)?;
    Ok(build.settings)
}

pub fn load_bm25(root: &Path) -> Result<Bm25Index, String> {
    if !present(root) {
        return crate::search::load_json(&root.join("bm25.json"));
    }
    let manifest = generation::read_manifest(root)?;
    Bm25Index::from_v4_segments_for_open(&generation::read_segments(root, &manifest)?)
}

pub fn restore_state(root: &Path) -> Result<IndexState, String> {
    let manifest = generation::read_manifest(root)?;
    let segments = generation::read_segments(root, &manifest)?;
    let bm25 = Bm25Index::from_v4_segments_for_open(&segments)?;
    let build: BuildState = serde_json::from_slice(&segments["build-state.json"])
        .map_err(|e| format!("Invalid v4 build state: {e}"))?;
    build.restore_cached_files(&bm25.sections)
}

pub fn publish(
    root: &Path,
    state: &IndexState,
    bm25: Bm25Index,
    spelling: &crate::spelling::SpellIndex,
    graph: &crate::semantic_mesh::EntityGraph,
    meta: Option<&crate::meta::MetaIndex>,
) -> Result<generation::Manifest, String> {
    let build = BuildState::from_legacy(state, &bm25.sections)?;
    publish_prepared(root, build, bm25, spelling, graph, meta)
}

/// The index command transfers ownership so cached source bodies can be freed
/// once their ranges have been validated, before compact rows are allocated.
pub fn publish_owned(
    root: &Path,
    state: IndexState,
    bm25: Bm25Index,
    spelling: &crate::spelling::SpellIndex,
    graph: &crate::semantic_mesh::EntityGraph,
    meta: Option<&crate::meta::MetaIndex>,
) -> Result<generation::Manifest, String> {
    let build = BuildState::from_legacy(&state, &bm25.sections)?;
    drop(state);
    publish_prepared(root, build, bm25, spelling, graph, meta)
}

fn publish_prepared(
    root: &Path,
    build: BuildState,
    mut bm25: Bm25Index,
    spelling: &crate::spelling::SpellIndex,
    graph: &crate::semantic_mesh::EntityGraph,
    meta: Option<&crate::meta::MetaIndex>,
) -> Result<generation::Manifest, String> {
    let fingerprint = crate::hybrid::index_fingerprint(&bm25.sections);
    crate::index_timing::memory_checkpoint("v4.memory.publish_start");
    let mut segments = bm25.v4_segments()?;
    crate::index_timing::memory_checkpoint("v4.memory.core_buffers");
    segments.insert(
        "build-state.json".into(),
        serde_json::to_vec(&build).map_err(|e| e.to_string())?,
    );
    segments.insert(
        "spelling.json".into(),
        serde_json::to_vec(spelling).map_err(|e| e.to_string())?,
    );
    segments.insert(
        "entity_graph.json".into(),
        serde_json::to_vec(graph).map_err(|e| e.to_string())?,
    );
    let id = crate::uuid_v4();
    if let Some(meta) = meta {
        let mut meta = meta.clone();
        meta.generation = id.clone();
        segments.insert(
            "meta.json".into(),
            serde_json::to_vec(&meta.to_disk()?).map_err(|e| e.to_string())?,
        );
    }
    let vectors = root.join(crate::local_vectors::FILE);
    if vectors.exists() {
        crate::local_vectors::LocalVectors::open(root, &bm25.sections)?;
        segments.insert(
            "local-vectors.json".into(),
            std::fs::read(vectors).map_err(|e| e.to_string())?,
        );
    }
    crate::index_timing::memory_checkpoint("v4.memory.all_buffers");
    generation::publish(
        root,
        generation::Manifest {
            format_version: 4,
            generation: id,
            sections: u32::try_from(bm25.sections.len())
                .map_err(|_| "Section count exceeds u32")?,
            source_files: u32::try_from(build.sources.len())
                .map_err(|_| "Source count exceeds u32")?,
            corpus_fingerprint: [fingerprint.0, fingerprint.1],
            segments: Default::default(),
        },
        &segments,
        |_| Ok(()),
    )
}

pub fn component<T: serde::de::DeserializeOwned>(root: &Path, name: &str) -> Result<T, String> {
    if !present(root) {
        return crate::search::load_json(&root.join(name));
    }
    let manifest = generation::read_manifest(root)?;
    let seal = manifest
        .segments
        .get(name)
        .ok_or_else(|| format!("V4 component missing: {name}"))?;
    let directory = generation::generation_directory(root, &manifest)?;
    let bytes = std::fs::read(directory.join(name)).map_err(|e| e.to_string())?;
    if bytes.len() as u64 != seal.bytes || generation::sha256(&bytes) != seal.sha256 {
        return Err(format!("V4 component seal mismatch: {name}"));
    }
    serde_json::from_slice(&bytes).map_err(|e| e.to_string())
}

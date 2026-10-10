use lume::index_binary::generation::{self, Manifest, PublishStep};
use std::collections::BTreeMap;
use std::path::PathBuf;

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("lume-generation-{}", lume::uuid_v4()));
        std::fs::create_dir_all(&root).unwrap();
        Self(root)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn manifest() -> Manifest {
    Manifest {
        format_version: 4,
        generation: lume::uuid_v4().to_string(),
        sections: 0,
        source_files: 0,
        corpus_fingerprint: [0; 2],
        segments: BTreeMap::new(),
    }
}
fn segments(value: u8) -> BTreeMap<String, Vec<u8>> {
    generation::CORE_FILES
        .iter()
        .map(|name| ((*name).into(), vec![value]))
        .collect()
}
#[test]
fn interrupted_publication_keeps_the_previous_complete_generation() {
    let root = Scratch::new();
    let original = generation::publish(&root.0, manifest(), &segments(1), |_| Ok(())).unwrap();
    for step in [
        PublishStep::CreatedGeneration,
        PublishStep::SyncedSegment,
        PublishStep::SyncedGeneration,
    ] {
        let result = generation::publish(&root.0, manifest(), &segments(2), |current| {
            if current == step {
                Err("injected interruption".into())
            } else {
                Ok(())
            }
        });
        assert!(result.is_err());
        let actual = generation::read_manifest(&root.0).unwrap();
        assert_eq!(actual.generation, original.generation);
        assert_eq!(
            generation::read_segments(&root.0, &actual).unwrap(),
            segments(1)
        );
    }
    let replacement = generation::publish(&root.0, manifest(), &segments(3), |_| Ok(())).unwrap();
    assert_eq!(
        generation::read_manifest(&root.0).unwrap().generation,
        replacement.generation
    );
    assert_eq!(
        generation::read_segments(&root.0, &replacement).unwrap(),
        segments(3)
    );
    assert!(root
        .0
        .join("generations")
        .join(original.generation)
        .exists());
}
#[test]
fn sealed_generation_rejects_changed_and_truncated_segments() {
    let root = Scratch::new();
    let published = generation::publish(&root.0, manifest(), &segments(1), |_| Ok(())).unwrap();
    let path = root
        .0
        .join("generations")
        .join(&published.generation)
        .join("text.bin");
    std::fs::write(&path, [2]).unwrap();
    assert!(generation::read_segments(&root.0, &published).is_err());
    std::fs::write(&path, []).unwrap();
    assert!(generation::read_segments(&root.0, &published).is_err());
}
#[test]
fn pointer_rejects_duplicate_segments_and_path_traversal() {
    let seal = format!(r#"{{"bytes":0,"sha256":"{}"}}"#, "0".repeat(64));
    let duplicate = format!(
        r#"{{"format_version":4,"generation":"00000000-0000-0000-0000-000000000000","sections":0,"source_files":0,"corpus_fingerprint":[0,0],"segments":{{"text.bin":{seal},"text.bin":{seal}}}}}"#
    );
    assert!(serde_json::from_str::<Manifest>(&duplicate).is_err());
    let mut invalid = manifest();
    invalid.generation = "../outside".into();
    assert!(invalid.validate().is_err());
}

#[test]
fn streamed_seals_match_and_failed_or_incomplete_writes_preserve_pointer() {
    let root = Scratch::new();
    let expected = segments(1);
    let original = generation::publish(&root.0, manifest(), &expected, |_| Ok(())).unwrap();
    let mut staged = generation::StagedGeneration::new(&root.0, manifest()).unwrap();
    for (name, bytes) in &expected {
        staged
            .write(name, |output| {
                output.write_all(bytes).map_err(|e| e.to_string())
            })
            .unwrap();
        assert_eq!(
            generation::read_manifest(&root.0).unwrap().generation,
            original.generation
        );
    }
    let complete = staged.finish().unwrap();
    assert_eq!(
        generation::read_segments(&root.0, &complete).unwrap(),
        expected
    );
    for name in generation::CORE_FILES {
        assert_eq!(
            complete.segments[*name].sha256,
            original.segments[*name].sha256
        );
        assert_eq!(
            complete.segments[*name].bytes,
            original.segments[*name].bytes
        );
    }
    let mut failed = generation::StagedGeneration::new(&root.0, manifest()).unwrap();
    assert!(failed
        .write("text.bin", |output| {
            output.write_all(b"partial").map_err(|e| e.to_string())?;
            Err("injected encoder failure".into())
        })
        .is_err());
    assert!(failed.finish().is_err());
    let mut incomplete = generation::StagedGeneration::new(&root.0, manifest()).unwrap();
    assert!(incomplete.write("../escape", |_| Ok(())).is_err());
    incomplete
        .write("text.bin", |output| {
            output.write_all(b"partial").map_err(|e| e.to_string())
        })
        .unwrap();
    assert!(incomplete.finish().is_err());
    assert_eq!(
        generation::read_manifest(&root.0).unwrap().generation,
        complete.generation
    );
    assert_eq!(
        generation::read_segments(&root.0, &complete).unwrap(),
        expected
    );
}

use lume::index_binary::generation;
use std::collections::BTreeMap;
use std::path::PathBuf;

struct Fixture {
    root: PathBuf,
    manifest: generation::Manifest,
    expected: BTreeMap<String, Vec<u8>>,
}

impl Fixture {
    fn new() -> Self {
        let root = loop {
            let candidate = std::env::temp_dir().join(format!("lume-read-{}", lume::uuid_v4()));
            match std::fs::create_dir(&candidate) {
                Ok(()) => break candidate,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("Cannot create {}: {error}", candidate.display()),
            }
        };
        let expected: BTreeMap<_, _> = generation::CORE_FILES
            .iter()
            .enumerate()
            .map(|(i, name)| ((*name).into(), vec![i as u8; (i + 1) * 1024]))
            .collect();
        let manifest = generation::publish(
            &root,
            generation::Manifest {
                format_version: 4,
                generation: lume::uuid_v4(),
                sections: 0,
                source_files: 0,
                corpus_fingerprint: [0; 2],
                segments: BTreeMap::new(),
                entity_overlay: None,
            },
            &expected,
            |_| Ok(()),
        )
        .unwrap();
        Self {
            root,
            manifest,
            expected,
        }
    }

    fn path(&self, name: &str) -> PathBuf {
        generation::generation_directory(&self.root, &self.manifest)
            .unwrap()
            .join(name)
    }

    fn error(&self, workers: usize) -> String {
        generation::read_segments_with_threads(&self.root, &self.manifest, workers).unwrap_err()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn serial_and_parallel_reads_return_identical_sealed_bytes() {
    let fixture = Fixture::new();
    for workers in [1, 2, 3, 4, 8] {
        assert_eq!(
            generation::read_segments_with_threads(&fixture.root, &fixture.manifest, workers)
                .unwrap(),
            fixture.expected
        );
    }
}

#[test]
fn multiple_errors_select_manifest_order_not_worker_order() {
    let fixture = Fixture::new();
    let names: Vec<_> = fixture.manifest.segments.keys().cloned().collect();
    for name in [&names[0], &names[names.len() - 1]] {
        let mut bytes = fixture.expected[name].clone();
        bytes[0] ^= 1;
        std::fs::write(fixture.path(name), bytes).unwrap();
    }
    let error = fixture.error(1);
    assert_eq!(error, format!("Ordinary-index seal mismatch: {}", names[0]));
    for workers in 2..=4 {
        assert_eq!(fixture.error(workers), error);
    }
}

#[test]
fn missing_truncated_and_directory_segments_fail_in_both_modes() {
    for case in 0..3 {
        let fixture = Fixture::new();
        let path = fixture.path("text.bin");
        match case {
            0 => std::fs::remove_file(&path).unwrap(),
            1 => std::fs::write(&path, b"truncated").unwrap(),
            _ => {
                std::fs::remove_file(&path).unwrap();
                std::fs::create_dir(&path).unwrap();
            }
        }
        let error = fixture.error(1);
        for workers in 2..=4 {
            assert_eq!(fixture.error(workers), error);
        }
    }
}

#[cfg(unix)]
#[test]
fn symlink_segments_are_rejected_in_both_modes() {
    let fixture = Fixture::new();
    let path = fixture.path("text.bin");
    std::fs::remove_file(&path).unwrap();
    let target = fixture.root.join("outside.bin");
    std::fs::write(&target, &fixture.expected["text.bin"]).unwrap();
    std::os::unix::fs::symlink(target, path).unwrap();
    let error = fixture.error(1);
    assert!(error.contains("length or type"));
    for workers in 2..=4 {
        assert_eq!(fixture.error(workers), error);
    }
}

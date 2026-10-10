use lume::bm25::Section;
use lume::index_binary::{generation, overlays};
use std::collections::BTreeMap;
use std::path::PathBuf;

struct Fixture {
    root: PathBuf,
    sections: Vec<Section>,
}
impl Fixture {
    fn new(count: usize) -> Self {
        let root = std::env::temp_dir().join(format!("lume-overlays-{}", lume::uuid_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let sections = (0..count)
            .map(|i| Section {
                title: format!("Section {i}"),
                body: "bilge pump water".into(),
                line_number: i + 1,
                filename: Some("boat.txt".into()),
                entities: Vec::new(),
            })
            .collect();
        let manifest = generation::Manifest {
            format_version: 4,
            generation: lume::uuid_v4(),
            sections: count as u32,
            source_files: 1,
            corpus_fingerprint: [0; 2],
            entity_overlay: None,
            segments: BTreeMap::new(),
        };
        let segments = generation::CORE_FILES
            .iter()
            .map(|name| ((*name).into(), vec![0]))
            .collect();
        generation::publish(&root, manifest, &segments, |_| Ok(())).unwrap();
        Self { root, sections }
    }
    fn record(&self, doc: usize, entities: &[&str]) -> overlays::Replacement {
        overlays::Replacement {
            section: doc as u32,
            source_hash: overlays::source_hash(&self.sections[doc]),
            entities: entities.iter().map(|value| (*value).into()).collect(),
        }
    }
    fn replay(&self) -> Vec<Section> {
        let manifest = generation::read_manifest(&self.root).unwrap();
        let records = overlays::read(&self.root, &manifest).unwrap();
        let mut sections = self.sections.clone();
        overlays::apply(&mut sections, &records).unwrap();
        sections
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn entity_batches_are_visible_and_replay_replaces_without_duplication() {
    let fixture = Fixture::new(2);
    overlays::publish(
        &fixture.root,
        vec![fixture.record(0, &["Pump"])],
        |_| Ok(()),
    )
    .unwrap();
    assert_eq!(fixture.replay()[0].entities, ["Pump"]);
    overlays::publish(
        &fixture.root,
        vec![fixture.record(0, &["Battery"]), fixture.record(1, &[])],
        |_| Ok(()),
    )
    .unwrap();
    let manifest = generation::read_manifest(&fixture.root).unwrap();
    let records = overlays::read(&fixture.root, &manifest).unwrap();
    let mut sections = fixture.sections.clone();
    overlays::apply(&mut sections, &records).unwrap();
    assert_eq!(sections[0].entities, ["Battery"]);
    assert_eq!(sections[1].entities, ["__LUME_PROCESSED__"]);
    overlays::apply(&mut sections, &records).unwrap();
    assert_eq!(sections[0].entities, ["Battery"]);
    assert_eq!(sections[1].entities, ["__LUME_PROCESSED__"]);
    let mut changed = fixture.sections.clone();
    changed[1].body.push('!');
    assert!(overlays::apply(&mut changed, &records).is_err());
    assert!(
        changed[0].entities.is_empty(),
        "validate all records before mutating"
    );
}

#[test]
fn interrupted_nodes_are_never_replayed_and_bad_seals_fail_closed() {
    let fixture = Fixture::new(2);
    overlays::publish(
        &fixture.root,
        vec![fixture.record(0, &["Pump"])],
        |_| Ok(()),
    )
    .unwrap();
    let before = generation::read_manifest(&fixture.root).unwrap();
    for stop in [
        overlays::PublishStep::NodeSynced,
        overlays::PublishStep::DirectorySynced,
    ] {
        assert!(overlays::publish(
            &fixture.root,
            vec![fixture.record(0, &["Battery"])],
            |step| {
                if step == stop {
                    Err("injected interruption".into())
                } else {
                    Ok(())
                }
            }
        )
        .is_err());
        let after = generation::read_manifest(&fixture.root).unwrap();
        assert_eq!(
            after.entity_overlay.as_ref().unwrap().node,
            before.entity_overlay.as_ref().unwrap().node
        );
        assert_eq!(fixture.replay()[0].entities, ["Pump"]);
    }
    let head = before.entity_overlay.as_ref().unwrap();
    let file = generation::generation_directory(&fixture.root, &before)
        .unwrap()
        .join("overlays")
        .join(format!("{}.json", head.node));
    std::fs::write(file, b"{corrupt").unwrap();
    assert!(overlays::read(&fixture.root, &before).is_err());
}

#[test]
fn checkpoint_bytes_include_pointer_and_scale_linearly() {
    fn bytes(count: usize) -> u64 {
        let fixture = Fixture::new(count);
        (0..count)
            .map(|doc| {
                overlays::publish(&fixture.root, vec![fixture.record(doc, &["Pump"])], |_| {
                    Ok(())
                })
                .unwrap()
                .1
            })
            .sum()
    }
    let n = bytes(32);
    let twice = bytes(64);
    println!("checkpoint bytes including pointers: n=32 {n}, 2n=64 {twice}");
    assert!(twice >= n * 2, "{n} vs {twice}");
    assert!(
        twice <= n * 21 / 10,
        "checkpoint prefixes were rewritten: {n} vs {twice}"
    );
}

#[test]
fn invalid_duplicate_and_out_of_range_records_do_not_change_head() {
    let fixture = Fixture::new(2);
    let record = fixture.record(0, &["Pump"]);
    assert!(overlays::publish(&fixture.root, vec![record.clone(), record], |_| Ok(())).is_err());
    let mut invalid = fixture.record(0, &["Pump"]);
    invalid.section = 2;
    assert!(overlays::publish(&fixture.root, vec![invalid], |_| Ok(())).is_err());
    assert!(generation::read_manifest(&fixture.root)
        .unwrap()
        .entity_overlay
        .is_none());
}

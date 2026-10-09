use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use ti_contracts::{
    self as c, AggOp, AggPartial, BucketRecord, CmpOp, Error, FieldKind, FieldSpec, FieldValue,
    Predicate, Result, RoaringBitmap, RoaringTreemap, ShardKey, ShardSource, TextIndex,
};
use ti_core::{
    BsiField, CountField, FieldData, GeoIndex, MemoryShard, MemorySource, PresenceRow, SetField,
};

fn spec(id: u32, path: &str, kind: FieldKind) -> FieldSpec {
    FieldSpec {
        id,
        path: path.into(),
        kind,
        agg: None,
        units: None,
    }
}
fn record(key: ShardKey, col: u32, field: u32, value: FieldValue, rewrite: bool) -> BucketRecord {
    BucketRecord {
        vessel: key.vessel,
        bucket: key.shard << 16 | col,
        field,
        value,
        rewrite,
    }
}
fn fixture() -> MemoryShard {
    let key = ShardKey {
        vessel: 42,
        shard: 1,
    };
    let mut s = MemoryShard::new(key).unwrap();
    for f in [
        spec(0, "exists", FieldKind::Presence),
        spec(1, "state", FieldKind::Set),
        spec(2, "number", FieldKind::Bsi { scale: 0 }),
        spec(3, "geo", FieldKind::Geo { res: 7 }),
        spec(4, "number$source", FieldKind::Set),
        spec(5, "count", FieldKind::Count),
    ] {
        s.register_field(f).unwrap();
    }
    for f in [1, 4] {
        for id in 0..3 {
            s.register_set_value(f, id, &id.to_string()).unwrap();
        }
    }
    s.apply(&[
        record(key, 0, 0, FieldValue::Present, false),
        record(key, 1, 0, FieldValue::Present, false),
        record(key, 2, 0, FieldValue::Present, false),
        record(key, 0, 2, FieldValue::Int(-4), false),
        record(key, 1, 1, FieldValue::SetValue(1), false),
        record(key, 0, 3, FieldValue::Cells(vec![99]), false),
    ])
    .unwrap();
    s
}
#[test]
fn depth_growth_preserves_old_rows_and_extreme_values() {
    let mut b = BsiField::new(18).unwrap();
    b.set(0, -3).unwrap();
    let old = b.bits().to_vec();
    b.set(1, 1 << 16).unwrap();
    assert_eq!(&b.bits()[..old.len()], &old);
    assert_eq!(b.depth(), 17);
    b.set(65535, i64::MIN).unwrap();
    b.set(2, i64::MAX).unwrap();
    assert_eq!(b.depth(), 64);
    let cols = [0, 1, 2, 65535].into_iter().collect();
    assert_eq!(
        b.values(&cols),
        vec![Some(-3), Some(65536), Some(i64::MAX), Some(i64::MIN)]
    );
    assert_eq!(b.min(&cols), Some(i64::MIN));
    assert_eq!(b.max(&cols), Some(i64::MAX));
    for threshold in [i64::MIN, i64::MAX, 0, -1, 1] {
        for op in [
            CmpOp::Eq,
            CmpOp::Ne,
            CmpOp::Lt,
            CmpOp::Le,
            CmpOp::Gt,
            CmpOp::Ge,
            CmpOp::Between,
        ] {
            let actual = b
                .compare(op.clone(), threshold, Some(i64::MAX), &cols)
                .unwrap();
            let expected = b
                .values(&cols)
                .into_iter()
                .zip(cols.iter())
                .filter_map(|(v, c)| {
                    ti_core::reference::compare(v.unwrap(), op.clone(), threshold, Some(i64::MAX))
                        .unwrap()
                        .then_some(c)
                })
                .collect::<RoaringBitmap>();
            assert_eq!(actual, expected);
        }
    }
    let before = b.clone();
    assert!(b.set(65536, 9).is_err());
    assert_eq!(b, before);
    b.clear(65535);
    assert_eq!(b.depth(), 64);
    assert!(!b.sign().contains(65535));
    assert!(b.bits().iter().all(|row| !row.contains(65535)));
    assert!(BsiField::new(19).is_err());
    assert!(b.compare(CmpOp::Between, 0, None, &cols).is_err());
}
#[test]
fn count_has_no_sign_and_zero_stays_present() {
    let mut c = CountField::new();
    c.set(0, 0).unwrap();
    c.set(1, u64::MAX).unwrap();
    assert_eq!(c.depth(), 64);
    assert!(c.exists().contains(0));
    let cols = [0, 1, 2].into_iter().collect();
    assert_eq!(c.values(&cols), vec![Some(0), Some(u64::MAX), None]);
    assert_eq!(c.sum(&cols), u64::MAX as i128);
    assert_eq!(
        c.compare(CmpOp::Gt, i64::MAX, None, &cols).unwrap(),
        [1].into_iter().collect()
    );
    c.clear(1);
    assert_eq!(c.values(&cols), vec![Some(0), None, None]);
    assert_eq!(c.depth(), 64);
}
#[test]
fn ordinary_sets_and_source_sets_have_distinct_invariants() {
    let mut single = SetField::new();
    let mut multi = SetField::source_set();
    for f in [&mut single, &mut multi] {
        f.register(10, "a").unwrap();
        f.register(20, "b").unwrap();
        f.set(1, 10).unwrap();
        f.set(1, 20).unwrap();
        f.validate().unwrap();
    }
    assert_eq!(single.values(1), vec![20]);
    assert_eq!(multi.values(1), vec![10, 20]);
    assert!(single.membership(&[10], false).is_empty());
    assert!(single.membership(&[10], true).contains(1));
    assert!(multi.membership(&[10], false).contains(1));
    assert!(!multi.membership(&[10], true).contains(1));
    assert!(single.register(10, "other").is_err());
    assert!(single.register(21, "b").is_err());
    assert!(single.set(1, 999).is_err());
    assert_eq!(single.values(1), vec![20]);
    multi.clear(1);
    assert!(multi.presence().is_empty());
    assert!(multi.rows().values().all(|row| row.is_empty()));
    let mut p = PresenceRow::default();
    p.set(65535).unwrap();
    assert!(p.set(65536).is_err());
    p.clear(65535);
    assert!(p.bitmap().is_empty());
}
#[test]
fn failed_transactions_publish_nothing_and_clear_all_rows() {
    let mut s = fixture();
    let key = s.key;
    let before = s.fields().clone();
    for recs in [
        vec![
            record(key, 0, 2, FieldValue::Int(99), true),
            record(key, 0, 1, FieldValue::SetValue(999), true),
        ],
        vec![
            record(key, 0, 1, FieldValue::Clear, true),
            record(key, 0, 1, FieldValue::SetValue(1), true),
        ],
        vec![
            record(key, 0, 2, FieldValue::Int(1), false),
            record(key, 0, 2, FieldValue::Int(1), true),
        ],
        vec![
            record(key, 0, 2, FieldValue::Int(1), true),
            record(key, 0, 2, FieldValue::Int(2), true),
        ],
        vec![record(key, 0, 5, FieldValue::Int(-1), true)],
        vec![record(key, 0, 2, FieldValue::Cells(vec![1]), true)],
        vec![record(
            ShardKey { vessel: 99, ..key },
            0,
            2,
            FieldValue::Int(1),
            true,
        )],
        vec![record(key, 0, 2, FieldValue::Int(99), false)],
    ] {
        assert!(s.apply(&recs).is_err());
        assert_eq!(s.fields(), &before);
    }
    s.apply(&[
        record(key, 0, 2, FieldValue::Clear, true),
        record(key, 0, 3, FieldValue::Clear, true),
    ])
    .unwrap();
    assert!(!s.field(2).unwrap().presence().contains(0));
    assert!(!s.field(3).unwrap().presence().contains(0));
    if let FieldData::Bsi(f) = s.field(2).unwrap() {
        assert!(!f.sign().contains(0));
        assert!(f.bits().iter().all(|r| !r.contains(0)));
    }
    s.apply(&[
        record(key, 0, 4, FieldValue::SetValue(0), true),
        record(key, 0, 4, FieldValue::SetValue(1), true),
        record(key, 0, 4, FieldValue::SetValue(1), true),
    ])
    .unwrap();
    if let FieldData::Set(f) = s.field(4).unwrap() {
        assert_eq!(f.values(0), vec![0, 1]);
    }
}
#[test]
fn null_negation_and_sparse_universe_are_sql_correct() {
    let s = fixture();
    let cmp = Predicate::BsiCmp {
        field: 2,
        op: CmpOp::Lt,
        lo: 0,
        hi: None,
    };
    let masks = s
        .eval_masks(&Predicate::Not(Box::new(cmp)), None, None)
        .unwrap();
    assert!(masks.truth.is_empty());
    assert_eq!(masks.falsity, [0].into_iter().collect());
    assert_eq!(masks.unknown, [1, 2].into_iter().collect());
    assert_eq!(
        s.eval_masks(&Predicate::Not(Box::new(Predicate::Present(2))), None, None)
            .unwrap()
            .truth,
        [1, 2].into_iter().collect()
    );
    assert_eq!(
        s.eval_masks(&Predicate::All, None, None).unwrap().truth,
        [0, 1, 2].into_iter().collect()
    );
    let p = Predicate::And(vec![
        Predicate::BsiCmp {
            field: 2,
            op: CmpOp::Eq,
            lo: 9,
            hi: None,
        },
        Predicate::Present(0),
    ]);
    assert_eq!(
        s.eval_masks(&p, None, None).unwrap().unknown,
        [1, 2].into_iter().collect()
    );
    // Empty TRUE alone cannot short-circuit: FALSE AND UNKNOWN is FALSE.
    let p = Predicate::And(vec![
        Predicate::BsiCmp {
            field: 2,
            op: CmpOp::Eq,
            lo: 9,
            hi: None,
        },
        Predicate::None,
    ]);
    assert!(s.eval_masks(&p, None, None).unwrap().unknown.is_empty());
}
struct TextFixture(Arc<AtomicUsize>);
impl TextIndex for TextFixture {
    fn match_buckets(
        &self,
        vessel: u32,
        _: &str,
        _: &str,
        from: u32,
        _: u32,
    ) -> Result<RoaringTreemap> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok([
            c::column_id(vessel, from),
            c::column_id(vessel + 1, from + 1),
            c::column_id(vessel, from - 1),
        ]
        .into_iter()
        .collect())
    }
}
struct GeoFixture;
impl GeoIndex for GeoFixture {
    fn cover(&self, _: ShardKey, _: u32, _: &[u64]) -> Result<RoaringBitmap> {
        Ok([0, 2, 99999].into_iter().collect())
    }
}
#[test]
fn delegates_are_bounded_and_inexact_geo_is_never_complemented() {
    let s = fixture();
    let calls = Arc::new(AtomicUsize::new(0));
    let text = TextFixture(calls.clone());
    let p = Predicate::Text {
        kind: "notes".into(),
        query: "leak".into(),
    };
    assert_eq!(
        s.eval_masks(&p, Some(&text), None).unwrap().truth,
        [0].into_iter().collect()
    );
    assert!(s.eval_masks(&p, None, None).is_err());
    assert!(s
        .eval_masks(
            &Predicate::And(vec![Predicate::None, p.clone()]),
            Some(&text),
            None
        )
        .unwrap()
        .truth
        .is_empty());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let geo = Predicate::GeoCover {
        field: 3,
        cells: vec![99],
    };
    let result = s.eval_masks(&geo, None, Some(&GeoFixture)).unwrap();
    assert_eq!(result.truth, [0].into_iter().collect());
    assert!(!result.exact);
    for p in [
        Predicate::Not(Box::new(geo.clone())),
        Predicate::Not(Box::new(Predicate::Or(vec![Predicate::None, geo.clone()]))),
        Predicate::And(vec![Predicate::None, Predicate::Not(Box::new(geo.clone()))]),
    ] {
        assert!(matches!(
            s.eval_masks(&p, None, Some(&GeoFixture)),
            Err(Error::Unsupported(_))
        ));
    }
    assert_eq!(
        s.eval_masks(&geo, None, None).unwrap().truth,
        [0].into_iter().collect()
    );
}
#[test]
fn shard_source_ranges_aggregates_and_reconstruction_handoff() {
    let s = fixture();
    let key = s.key;
    let mut source = MemorySource::new()
        .with_text(Arc::new(TextFixture(Arc::new(AtomicUsize::new(0)))))
        .with_geo(Arc::new(GeoFixture));
    source.insert(s);
    source.insert(
        MemoryShard::new(ShardKey {
            vessel: 7,
            shard: 65535,
        })
        .unwrap(),
    );
    assert_eq!(source.shards(Some(&[42]), 65536, 65538), vec![key]);
    assert!(source.shards(None, 2, 1).is_empty());
    assert_eq!(
        source
            .eval(
                key,
                &Predicate::TsRange {
                    from: 65537,
                    to: 65538
                }
            )
            .unwrap(),
        [1, 2].into_iter().collect()
    );
    assert!(source
        .eval(key, &Predicate::TsRange { from: 0, to: 65535 })
        .unwrap()
        .is_empty());
    let cols = [0, 1, 2, 999].into_iter().collect();
    assert_eq!(
        source.agg(key, &cols, 999, AggOp::CountAll).unwrap(),
        AggPartial::Count(3)
    );
    assert_eq!(
        source.agg(key, &cols, 2, AggOp::Count).unwrap(),
        AggPartial::Count(1)
    );
    assert_eq!(
        source.agg(key, &cols, 2, AggOp::Sum).unwrap(),
        AggPartial::Sum { sum: -4, count: 1 }
    );
    assert_eq!(
        source.agg(key, &cols, 2, AggOp::Min).unwrap(),
        AggPartial::Min(Some(-4))
    );
    assert_eq!(
        source.agg(key, &cols, 2, AggOp::Max).unwrap(),
        AggPartial::Max(Some(-4))
    );
    assert!(matches!(
        source.read(key, &cols, &[2]),
        Err(Error::Unsupported(_))
    ));
    let last = ShardKey {
        vessel: 7,
        shard: 65535,
    };
    source
        .shard_mut(last)
        .unwrap()
        .register_field(spec(0, "p", FieldKind::Presence))
        .unwrap();
    source
        .shard_mut(last)
        .unwrap()
        .apply(&[record(last, 65535, 0, FieldValue::Present, false)])
        .unwrap();
    assert_eq!(
        source
            .eval(
                last,
                &Predicate::TsRange {
                    from: u32::MAX,
                    to: u32::MAX
                }
            )
            .unwrap(),
        [65535].into_iter().collect()
    );
}

#[test]
fn persistence_rows_roundtrip_and_reject_corruption() {
    use std::collections::BTreeMap;
    let mut b = BsiField::new(3).unwrap();
    for (col, value) in [(0, 0), (1, -17), (2, i64::MIN), (3, i64::MAX)] {
        b.set(col, value).unwrap();
    }
    assert_eq!(
        BsiField::from_rows(
            b.scale(),
            b.exists().clone(),
            b.sign().clone(),
            b.bits().to_vec()
        )
        .unwrap(),
        b
    );
    let mut counts = CountField::new();
    counts.set(1, u64::MAX).unwrap();
    assert_eq!(
        CountField::from_rows(counts.exists().clone(), counts.bits().to_vec()).unwrap(),
        counts
    );
    let one: RoaringBitmap = [1].into_iter().collect();
    let two: RoaringBitmap = [2].into_iter().collect();
    assert!(PresenceRow::from_bitmap([65536].into_iter().collect()).is_err());
    assert!(CountField::from_rows(one.clone(), vec![two.clone()]).is_err());
    assert!(CountField::from_rows(one.clone(), vec![RoaringBitmap::new(); 65]).is_err());
    assert!(BsiField::from_rows(0, one.clone(), one.clone(), vec![]).is_err());
    assert!(BsiField::from_rows(0, one.clone(), two, vec![one.clone()]).is_err());
    let mut bits = vec![RoaringBitmap::new(); 64];
    bits[63] = one.clone();
    assert!(BsiField::from_rows(0, one.clone(), RoaringBitmap::new(), bits.clone()).is_err());
    bits[0] = one.clone();
    assert!(BsiField::from_rows(0, one.clone(), one.clone(), bits).is_err());
    let dictionary = BTreeMap::from([("a".into(), 7), ("b".into(), 9)]);
    let rows = BTreeMap::from([(7, one.clone()), (9, one.clone())]);
    assert!(SetField::from_rows(dictionary.clone(), rows.clone(), false).is_err());
    let multi = SetField::from_rows(dictionary, rows, true).unwrap();
    assert_eq!(multi.values(1), vec![7, 9]);
    let restored =
        SetField::from_rows(multi.dictionary().clone(), multi.rows().clone(), true).unwrap();
    assert_eq!(restored, multi);
    assert!(SetField::from_rows(
        BTreeMap::from([("a".into(), 7), ("b".into(), 7)]),
        BTreeMap::new(),
        false
    )
    .is_err());
    assert!(SetField::from_rows(BTreeMap::new(), BTreeMap::from([(7, one)]), false).is_err());
}

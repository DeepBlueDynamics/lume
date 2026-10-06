//! Golden SQL-to-IR fixtures for W4. These execute the frozen IR, not a SQL parser.
use ti_contracts::{
    AggOp, AggPartial, BucketRecord, CmpOp, FieldKind, FieldSpec, FieldValue, Predicate, ShardKey,
};
use ti_core::{MemoryShard, RoaringBitmap};

#[test]
fn golden_filter_and_aggregate_handoff() {
    let key = ShardKey {
        vessel: 0,
        shard: 0,
    };
    let mut s = MemoryShard::new(key).unwrap();
    for (id, path, kind) in [
        (0, "p", FieldKind::Presence),
        (1, "n", FieldKind::Bsi { scale: 0 }),
        (2, "state", FieldKind::Set),
    ] {
        s.register_field(FieldSpec {
            id,
            path: path.into(),
            kind,
            agg: None,
            units: None,
        })
        .unwrap();
    }
    s.register_set_value(2, 0, "started").unwrap();
    s.register_set_value(2, 1, "stopped").unwrap();
    for col in 0..4 {
        let rec = |field, value| BucketRecord {
            vessel: 0,
            bucket: col,
            field,
            value,
            rewrite: false,
        };
        let mut records = vec![rec(0, FieldValue::Present)];
        if let Some(v) = [Some(-2), Some(0), Some(3), None][col as usize] {
            records.push(rec(1, FieldValue::Int(v)));
        }
        if let Some(v) = [Some(0), Some(1), None, Some(0)][col as usize] {
            records.push(rec(2, FieldValue::SetValue(v)));
        }
        s.apply(&records).unwrap();
    }
    let cmp = |op, lo, hi| Predicate::BsiCmp {
        field: 1,
        op,
        lo,
        hi,
    };
    let set = |rows, negate| Predicate::SetEq {
        field: 2,
        rows,
        negate,
    };
    let fixtures: Vec<(&str, Predicate, Vec<u32>)> = vec![
        ("TRUE", Predicate::All, vec![0, 1, 2, 3]),
        ("FALSE", Predicate::None, vec![]),
        ("n = 0", cmp(CmpOp::Eq, 0, None), vec![1]),
        ("n != 0", cmp(CmpOp::Ne, 0, None), vec![0, 2]),
        ("n < 0", cmp(CmpOp::Lt, 0, None), vec![0]),
        ("n <= 0", cmp(CmpOp::Le, 0, None), vec![0, 1]),
        ("n > 0", cmp(CmpOp::Gt, 0, None), vec![2]),
        ("n >= 0", cmp(CmpOp::Ge, 0, None), vec![1, 2]),
        (
            "n BETWEEN -2 AND 0",
            cmp(CmpOp::Between, -2, Some(0)),
            vec![0, 1],
        ),
        ("n IS NOT NULL", Predicate::Present(1), vec![0, 1, 2]),
        (
            "n IS NULL",
            Predicate::Not(Box::new(Predicate::Present(1))),
            vec![3],
        ),
        ("state = 'started'", set(vec![0], false), vec![0, 3]),
        ("state NOT IN ('started')", set(vec![0], true), vec![1]),
        (
            "NOT (n > 0)",
            Predicate::Not(Box::new(cmp(CmpOp::Gt, 0, None))),
            vec![0, 1],
        ),
        (
            "n < 0 OR state = 'started'",
            Predicate::Or(vec![cmp(CmpOp::Lt, 0, None), set(vec![0], false)]),
            vec![0, 3],
        ),
        (
            "n >= 0 AND state != 'started'",
            Predicate::And(vec![cmp(CmpOp::Ge, 0, None), set(vec![0], true)]),
            vec![1],
        ),
        (
            "ts BETWEEN '2020-01-01T00:00:10Z' AND '2020-01-01T00:00:20Z'",
            Predicate::TsRange { from: 1, to: 2 },
            vec![1, 2],
        ),
    ];
    for (sql, predicate, expected) in fixtures {
        assert_eq!(
            s.eval_masks(&predicate, None, None).unwrap().truth,
            expected.into_iter().collect::<RoaringBitmap>(),
            "SELECT ts FROM telemetry WHERE {sql}"
        );
    }
    let all = s.universe();
    for (sql, op, expected) in [
        ("COUNT(*)", AggOp::CountAll, AggPartial::Count(4)),
        ("COUNT(n)", AggOp::Count, AggPartial::Count(3)),
        ("SUM(n)", AggOp::Sum, AggPartial::Sum { sum: 1, count: 3 }),
        ("MIN(n)", AggOp::Min, AggPartial::Min(Some(-2))),
        ("MAX(n)", AggOp::Max, AggPartial::Max(Some(3))),
    ] {
        assert_eq!(
            s.aggregate(&all, 1, op).unwrap(),
            expected,
            "SELECT {sql} FROM telemetry"
        );
    }
}

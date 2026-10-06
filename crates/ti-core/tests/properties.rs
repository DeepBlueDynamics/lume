//! Independent 10,000-case checks; no bitmap implementation helper supplies expectations.
use proptest::prelude::*;
use ti_contracts::{
    BucketRecord, CmpOp, FieldKind, FieldSpec, FieldValue, Predicate, RoaringBitmap, ShardKey,
};
use ti_core::reference::{self, ScalarField, ScalarShard, Truth};
use ti_core::{BsiField, CountField, FieldData, MemoryShard, SHARD_COLUMNS};

fn number() -> impl Strategy<Value = i64> {
    prop_oneof![4=>-100_000i64..=100_000,2=>any::<i64>(),1=>Just(i64::MIN),1=>Just(i64::MAX),1=>Just(0),1=>Just(-1)]
}
fn op() -> impl Strategy<Value = CmpOp> {
    prop_oneof![
        Just(CmpOp::Eq),
        Just(CmpOp::Ne),
        Just(CmpOp::Lt),
        Just(CmpOp::Le),
        Just(CmpOp::Gt),
        Just(CmpOp::Ge),
        Just(CmpOp::Between)
    ]
}
fn leaf() -> BoxedStrategy<Predicate> {
    prop_oneof![
        Just(Predicate::All),
        Just(Predicate::None),
        Just(Predicate::And(vec![])),
        Just(Predicate::Or(vec![])),
        (0u32..5).prop_map(Predicate::Present),
        (
            prop::collection::vec(0u32..8, 0..5),
            any::<bool>(),
            prop_oneof![Just(2u32), Just(3u32)]
        )
            .prop_map(|(rows, negate, field)| Predicate::SetEq {
                field,
                rows,
                negate
            }),
        (
            op(),
            number(),
            number(),
            prop_oneof![Just(1u32), Just(4u32)]
        )
            .prop_map(|(op, lo, hi, field)| Predicate::BsiCmp {
                field,
                op,
                lo,
                hi: Some(hi)
            }),
        (
            prop_oneof![0u32..100, 65530u32..65630, any::<u32>()],
            prop_oneof![0u32..100, 65530u32..65630, any::<u32>()]
        )
            .prop_map(|(from, to)| Predicate::TsRange { from, to }),
    ]
    .boxed()
}
fn tree() -> BoxedStrategy<Predicate> {
    leaf()
        .prop_recursive(4, 64, 4, |inner| {
            prop_oneof![
                inner.clone().prop_map(|p| Predicate::Not(Box::new(p))),
                prop::collection::vec(inner.clone(), 0..4).prop_map(Predicate::And),
                prop::collection::vec(inner, 0..4).prop_map(Predicate::Or),
            ]
        })
        .boxed()
}
fn spec(id: u32, path: &str, kind: FieldKind) -> FieldSpec {
    FieldSpec {
        id,
        path: path.into(),
        agg: None,
        kind,
        units: None,
    }
}
fn record(key: ShardKey, col: u32, field: u32, value: FieldValue, rewrite: bool) -> BucketRecord {
    BucketRecord {
        vessel: key.vessel,
        bucket: (key.shard << 16) | col,
        field,
        value,
        rewrite,
    }
}

proptest! {
    #![proptest_config(ProptestConfig {cases:10_000,..ProptestConfig::default()})]
    #[test]
    fn signed_bsi_vs_scalar(
        values in prop::collection::vec(prop::option::of(number()),0..64),
        scale in 0u8..=18, op in op(),lo in number(),hi in number(),seed in any::<u64>()
    ) {
        let mut bsi=BsiField::new(scale).unwrap();
        let mut last_depth=0;
        for (col,value) in values.iter().enumerate() {
            if let Some(value)=value { bsi.set(col as u32,*value).unwrap(); }
            prop_assert!(bsi.depth()>=last_depth);last_depth=bsi.depth();
        }
        let selected:Vec<_>=(0..values.len()).map(|i|(seed.rotate_left(i as u32)&1)!=0).collect();
        let filter:RoaringBitmap=selected.iter().enumerate().filter_map(|(i,b)|b.then_some(i as u32)).collect();
        let actual=bsi.compare(op.clone(),lo,Some(hi),&filter).unwrap();
        let expected:RoaringBitmap=values.iter().enumerate().filter_map(|(i,v)| {
            v.filter(|v|selected[i] && reference::compare(*v,op.clone(),lo,Some(hi)).unwrap()).map(|_|i as u32)
        }).collect();
        prop_assert_eq!(actual,expected);
        let (sum,min,max,_)=reference::aggregate(&values,&selected);
        prop_assert_eq!(bsi.sum(&filter),sum);
        prop_assert_eq!(bsi.min(&filter),min);
        prop_assert_eq!(bsi.max(&filter),max);
        let all:RoaringBitmap=(0..values.len() as u32).collect();
        prop_assert_eq!(bsi.values(&all),values.clone());
        // Column clear/rewrite preserves other columns and never shrinks depth.
        if !values.is_empty() {
            let col=(seed as usize)%values.len();
            bsi.clear(col as u32);
            let mut after=values.clone();after[col]=None;
            prop_assert_eq!(bsi.values(&all),after.clone());
            bsi.set(col as u32,lo).unwrap();after[col]=Some(lo);
            prop_assert_eq!(bsi.values(&all),after);
            prop_assert!(bsi.depth()>=last_depth);
        }
    }
    #[test]
    fn count_bsi_full_unsigned_domain(
        values in prop::collection::vec(prop::option::of(any::<u64>()),0..48),
        op in op(),lo in number(),hi in number(),seed in any::<u64>()
    ) {
        let mut counts=CountField::new();
        for (i,v) in values.iter().enumerate() {if let Some(v)=v {counts.set(i as u32,*v).unwrap();}}
        let filter:RoaringBitmap=(0..values.len() as u32).filter(|i|seed.rotate_left(*i)&1!=0).collect();
        let samples:Vec<_>=values.iter().enumerate().filter_map(|(i,v)|if filter.contains(i as u32){*v}else{None}).collect();
        prop_assert_eq!(counts.sum(&filter),samples.iter().map(|v|*v as i128).sum::<i128>());
        prop_assert_eq!(counts.min(&filter),samples.iter().min().copied());
        prop_assert_eq!(counts.max(&filter),samples.iter().max().copied());
        let all:RoaringBitmap=(0..values.len() as u32).collect();
        prop_assert_eq!(counts.values(&all),values.clone());
        let expected:RoaringBitmap=values.iter().enumerate().filter_map(|(i,value)|value.and_then(|v| {
            let v=v as i128;let l=lo as i128;let h=hi as i128;
            let yes=match op {CmpOp::Eq=>v==l,CmpOp::Ne=>v!=l,CmpOp::Lt=>v<l,CmpOp::Le=>v<=l,CmpOp::Gt=>v>l,CmpOp::Ge=>v>=l,CmpOp::Between=>v>=l&&v<=h};
            (yes&&filter.contains(i as u32)).then_some(i as u32)
        })).collect();
        prop_assert_eq!(counts.compare(op,lo,Some(hi),&filter).unwrap(),expected);
    }
    #[test]
    fn random_sql_trees_match_scalar_truth_tables(
        data in prop::collection::vec((any::<bool>(),prop::option::of(number()),prop::option::of(0u32..8),prop::option::of(prop::collection::vec(0u32..8,0..4)),prop::option::of(0u64..100)),0..48),
        predicate in tree(),shard_no in prop_oneof![Just(0u32),Just(1u32),Just(65535u32)]
    ) {
        let key=ShardKey{vessel:17,shard:shard_no};
        let mut bitmap=MemoryShard::new(key).unwrap();
        for f in [spec(0,"presence",FieldKind::Presence),spec(1,"n",FieldKind::Bsi{scale:3}),spec(2,"state",FieldKind::Set),spec(3,"n$source",FieldKind::Set),spec(4,"count",FieldKind::Count)] {bitmap.register_field(f).unwrap();}
        for id in [2,3] {for row in 0..8 {bitmap.register_set_value(id,row,&row.to_string()).unwrap();}}
        let mut scalar=ScalarShard::new(key,data.len());
        scalar.fields.insert(0,ScalarField::Presence(data.iter().map(|d|d.0).collect()));
        scalar.fields.insert(1,ScalarField::Bsi(data.iter().map(|d|d.1).collect()));
        scalar.fields.insert(2,ScalarField::Set(data.iter().map(|d|d.2).collect()));
        scalar.fields.insert(3,ScalarField::Sources(data.iter().map(|d|d.3.clone()).collect()));
        scalar.fields.insert(4,ScalarField::Count(data.iter().map(|d|d.4).collect()));
        let mut recs=vec![];
        for (col,(present,n,state,sources,count)) in data.iter().enumerate() {
            let col=col as u32;
            if *present {recs.push(record(key,col,0,FieldValue::Present,false));}
            if let Some(n)=n {recs.push(record(key,col,1,FieldValue::Int(*n),false));}
            if let Some(state)=state {recs.push(record(key,col,2,FieldValue::SetValue(*state),false));}
            if let Some(sources)=sources {
                // A present empty source list has no reporting source and is
                // modeled as absent, matching the bitmap membership encoding.
                if sources.is_empty() {
                    if let ScalarField::Sources(v)=scalar.fields.get_mut(&3).unwrap() {v[col as usize]=None;}
                }
                for row in sources {recs.push(record(key,col,3,FieldValue::SetValue(*row),false));}
            }
            if let Some(count)=count {recs.push(record(key,col,4,FieldValue::Int(*count as i64),false));}
        }
        bitmap.apply(&recs).unwrap();
        let actual=bitmap.eval_masks(&predicate,None,None).unwrap();
        let expected=scalar.eval(&predicate).unwrap();
        let collect=|truth|expected.iter().enumerate().filter_map(|(i,t)|(*t==Some(truth)).then_some(i as u32)).collect::<RoaringBitmap>();
        prop_assert_eq!(&actual.truth,&collect(Truth::True));
        prop_assert_eq!(&actual.falsity,&collect(Truth::False));
        prop_assert_eq!(&actual.unknown,&collect(Truth::Unknown));
        prop_assert!((&actual.truth & &actual.falsity).is_empty());
        prop_assert_eq!(&actual.truth | &actual.falsity | &actual.unknown,bitmap.universe());
    }
    #[test]
    fn d21_holds_after_arbitrary_apply_and_rewrites(
        ops in prop::collection::vec((0u32..32,prop::option::of(0u32..8),any::<bool>()),0..128)
    ) {
        let key=ShardKey{vessel:1,shard:1};
        let mut bitmap=MemoryShard::new(key).unwrap();
        bitmap.register_field(spec(0,"state",FieldKind::Set)).unwrap();
        for row in 0..8 {bitmap.register_set_value(0,row,&row.to_string()).unwrap();}
        let mut scalar=vec![None;32];
        for (col,value,rewrite) in ops {
            let rec=record(key,col,0,value.map(FieldValue::SetValue).unwrap_or(FieldValue::Clear),rewrite);
            let before=bitmap.clone();
            let result=bitmap.apply(std::slice::from_ref(&rec));
            if value.is_none() && !rewrite {
                prop_assert!(result.is_err());
                prop_assert_eq!(bitmap.fields(),before.fields());
            } else {
                result.unwrap();scalar[col as usize]=value;
                bitmap.apply(&[rec]).unwrap(); // replay idempotence
            }
            let set=match bitmap.field(0).unwrap() {FieldData::Set(f)=>f,_=>unreachable!()};
            set.validate().unwrap();
            let reconstructed:Vec<_>=(0..32).map(|c|set.values(c).first().copied()).collect();
            prop_assert_eq!(&reconstructed,&scalar);
            let present:RoaringBitmap=scalar.iter().enumerate().filter_map(|(i,v)|v.map(|_|i as u32)).collect();
            prop_assert_eq!(set.presence(),&present);
            prop_assert!(set.rows().values().all(|row|row.max().is_none_or(|col|col<SHARD_COLUMNS)));
        }
    }
}

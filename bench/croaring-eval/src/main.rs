//! Same-process roaring/CRoaring evaluation. Unsafe is isolated to validated,
//! self-produced, aligned immutable view buffers, never a production decoder.
use croaring::{Bitmap, BitmapView, Frozen, Portable};
use roaring::RoaringBitmap;
use serde_json::json;
use std::{hint::black_box, time::Instant};
trait Bits: Clone {
    fn empty() -> Self;
    fn and(&self, other: &Self) -> Self;
    fn minus(&self, other: &Self) -> Self;
    fn union(&mut self, other: &Self);
    fn intersect(&mut self, other: &Self);
    fn subtract(&mut self, other: &Self);
    fn count(&self) -> u64;
    fn values(&self) -> Vec<u32>;
    fn intervals(&self) -> u64;
}
fn intervals(values: impl Iterator<Item=u32>) -> u64 {
    let mut last = None; let mut count = 0;
    for value in values { if last.is_none_or(|last| value != last+1) { count += 1; } last = Some(value); }
    count
}
impl Bits for RoaringBitmap {
    fn empty() -> Self { Self::new() }
    fn and(&self, other:&Self) -> Self { self & other }
    fn minus(&self, other:&Self) -> Self { self - other }
    fn union(&mut self, other:&Self) { *self |= other; }
    fn intersect(&mut self, other:&Self) { *self &= other; }
    fn subtract(&mut self, other:&Self) { *self -= other; }
    fn count(&self) -> u64 { self.len() }
    fn values(&self) -> Vec<u32> { self.iter().collect() }
    fn intervals(&self) -> u64 { intervals(self.iter()) }
}
impl Bits for Bitmap {
    fn empty() -> Self { Self::new() }
    fn and(&self, other:&Self) -> Self { Bitmap::and(self, other) }
    fn minus(&self, other:&Self) -> Self { self.andnot(other) }
    fn union(&mut self, other:&Self) { self.or_inplace(other); }
    fn intersect(&mut self, other:&Self) { self.and_inplace(other); }
    fn subtract(&mut self, other:&Self) { self.andnot_inplace(other); }
    fn count(&self) -> u64 { self.cardinality() }
    fn values(&self) -> Vec<u32> { self.to_vec() }
    fn intervals(&self) -> u64 { intervals(self.iter()) }
}
// Same high-to-low equal-prefix partition as ti_core::MagnitudeRows::partitions.
fn bsi<B: Bits>(exists:&B, rows:&[&B], low:u32, high:u32) -> B {
    let mut less = B::empty(); let mut greater = B::empty();
    let mut equal_low = exists.clone(); let mut equal_high = exists.clone();
    for (bit, row) in rows.iter().enumerate().rev() {
        if equal_low.count() != 0 {
            if low & (1 << bit) != 0 { less.union(&equal_low.minus(row)); equal_low.intersect(row); }
            else { equal_low.subtract(row); }
        }
        if equal_high.count() != 0 {
            if high & (1 << bit) == 0 { greater.union(&equal_high.and(row)); equal_high.subtract(row); }
            else { equal_high.intersect(row); }
        }
    }
    let mut out = exists.minus(&less); out.subtract(&greater); out
}
fn chain<B: Bits>(rows:&[&B]) -> B {
    let mut out = rows[0].clone();
    for (i,row) in rows.iter().enumerate().skip(1) {
        if i % 2 == 1 { out.intersect(row); } else { out.subtract(row); }
    }
    out
}
fn sample(f: &mut impl FnMut()->u64, iterations:usize) -> f64 {
    let start = Instant::now();
    let mut checksum = 0;
    for _ in 0..iterations { checksum ^= black_box(f()); }
    black_box(checksum);
    start.elapsed().as_nanos() as f64 / iterations as f64
}
fn compare(name:&str, mut a:impl FnMut()->u64, mut b:impl FnMut()->u64, mut frozen:impl FnMut()->u64, mut portable:impl FnMut()->u64, iterations:usize) -> serde_json::Value {
    for _ in 0..20 { black_box(a()); black_box(b()); black_box(frozen()); black_box(portable()); }
    let mut r = vec![]; let mut c = vec![]; let mut v = vec![]; let mut p = vec![];
    for round in 0..9 {
        if round%2 == 0 { r.push(sample(&mut a,iterations)); c.push(sample(&mut b,iterations)); v.push(sample(&mut frozen,iterations)); p.push(sample(&mut portable,iterations)); }
        else { p.push(sample(&mut portable,iterations)); v.push(sample(&mut frozen,iterations)); c.push(sample(&mut b,iterations)); r.push(sample(&mut a,iterations)); }
    }
    for values in [&mut r,&mut c,&mut v,&mut p] { values.sort_by(f64::total_cmp); }
    json!({"case":name,"iterations":iterations,"roaring_ns":r[4],"croaring_owned_ns":c[4],"croaring_frozen_ns":v[4],"croaring_portable_view_ns":p[4],"portable_view_speedup":r[4]/p[4],
        "owned_speedup":r[4]/c[4],"frozen_speedup":r[4]/v[4],"roaring_range_ns":[r[0],r[8]],"frozen_range_ns":[v[0],v[8]]})
}
fn frozen_buffers(rows:&[Bitmap]) -> (Vec<Vec<u8>>,Vec<(usize,usize)>) {
    let mut buffers = vec![]; let mut slices = vec![];
    for row in rows {
        let mut buf = vec![];
        let view = row.serialize_into_vec::<Frozen>(&mut buf);
        let length = view.len();
        let offset = view.as_ptr() as usize - buf.as_ptr() as usize;
        assert_eq!((buf.as_ptr() as usize + offset)%32,0);
        slices.push((offset,length)); buffers.push(buf);
    }
    (buffers,slices)
}
fn main() {
    let iterations = std::env::args().nth(1).map(|s| s.parse().unwrap()).unwrap_or(1000);
    let mut results = vec![];
    for distribution in ["dense","sparse","runs"] {
        let mut values = vec![None;65536]; let mut seed = 0x12345678u64;
        for (col,value) in values.iter_mut().enumerate() {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            if distribution != "sparse" || seed%10 == 0 { *value = Some(if distribution == "runs" { (col/16) as u32 } else { ((seed>>32)&4095) as u32 }); }
        }
        let mut rrows: Vec<RoaringBitmap> = vec![RoaringBitmap::new();13];
        for (col,value) in values.iter().enumerate() { if let Some(value) = value { rrows[0].insert(col as u32); for bit in 0..12 { if value & (1<<bit) != 0 { rrows[bit+1].insert(col as u32); } } } }
        let crows: Vec<Bitmap> = rrows.iter().map(|row| { let mut c = Bitmap::of(&row.iter().collect::<Vec<_>>()); c.run_optimize(); c.shrink_to_fit(); c }).collect();
        let (buffers,slices) = frozen_buffers(&crows);
        // SAFETY: buffers were just serialized using Frozen, with library-provided
        // 32-byte alignment and exact lengths; they remain alive and immutable.
        let views: Vec<BitmapView<'_>> = buffers.iter().zip(&slices).map(|(buf,(off,len))| unsafe { BitmapView::deserialize::<Frozen>(&buf[*off..off+len]) }).collect();
        let pbytes:Vec<_>=rrows.iter().map(|r| { let mut bytes=vec![]; r.serialize_into(&mut bytes).unwrap(); bytes }).collect();
        // SAFETY: byte slices were generated by Portable serialization and are
        // immutable, with exact lengths for their entire borrowed lifetimes.
        let pviews:Vec<_>=pbytes.iter().map(|b|unsafe{BitmapView::deserialize::<Portable>(b)}).collect();
        let prefs:Vec<&Bitmap>=pviews[1..].iter().map(|v|&**v).collect();
        let rrefs: Vec<_> = rrows[1..].iter().collect(); let crefs: Vec<_> = crows[1..].iter().collect();
        let vrefs: Vec<&Bitmap> = views[1..].iter().map(|v| &**v).collect();
        let expected: Vec<u32> = values.iter().enumerate().filter_map(|(i,v)| v.filter(|v| (1200..=2400).contains(v)).map(|_|i as u32)).collect();
        assert_eq!(bsi(&rrows[0],&rrefs,1200,2400).values(),expected);
        assert_eq!(bsi(&crows[0],&crefs,1200,2400).values(),expected);
        assert_eq!(bsi(&*views[0],&vrefs,1200,2400).values(),expected);
        assert_eq!(bsi(&*pviews[0],&prefs,1200,2400).values(),expected);
        results.push(compare(&format!("bsi-range/{distribution}"),
            || bsi(black_box(&rrows[0]),black_box(&rrefs),1200,2400).count(),
            || bsi(black_box(&crows[0]),black_box(&crefs),1200,2400).count(),
            || bsi(black_box(&*views[0]),black_box(&vrefs),1200,2400).count(),
            || bsi(black_box(&*pviews[0]),black_box(&prefs),1200,2400).count(),iterations));
        let chain_r: Vec<RoaringBitmap> = (0..6).map(|j| (0..65536u32).filter(|i| match distribution {
            "runs" => if j%2 == 0 && j != 0 { (i/256+j)%19 == 0 } else { (i/256+j)%7 != 0 },
            "sparse" => if j == 0 { i%10 == 0 } else if j%2 == 0 { i.wrapping_mul(1103515245).wrapping_add(j*12345)%37 == 0 } else { i.wrapping_mul(1103515245).wrapping_add(j*12345)%17 < 13 },
            _ => if j%2 == 0 && j != 0 { i.wrapping_mul(1103515245).wrapping_add(j*12345)%37 == 0 } else { i.wrapping_mul(1103515245).wrapping_add(j*12345)%17 < 13 },
        }).collect()).collect();
        let chain_c: Vec<Bitmap> = chain_r.iter().map(|r| { let mut c=Bitmap::of(&r.iter().collect::<Vec<_>>()); c.run_optimize(); c.shrink_to_fit(); c }).collect();
        let (chain_buf,chain_slices)=frozen_buffers(&chain_c);
        // SAFETY: same generated/immutable/aligned/exact-length buffer invariant.
        let chain_v: Vec<BitmapView<'_>>=chain_buf.iter().zip(&chain_slices).map(|(b,(o,n))| unsafe{BitmapView::deserialize::<Frozen>(&b[*o..o+n])}).collect();
        let rr:Vec<_>=chain_r.iter().collect();let cr:Vec<_>=chain_c.iter().collect();let vr:Vec<&Bitmap>=chain_v.iter().map(|v|&**v).collect();
        assert!(chain(&rr).count() > 0, "chain fixture must be non-empty"); assert_eq!(chain(&rr).values(),chain(&cr).values()); assert_eq!(chain(&rr).values(),chain(&vr).values());
        let chain_pb:Vec<_>=chain_r.iter().map(|r| { let mut bytes=vec![]; r.serialize_into(&mut bytes).unwrap(); bytes }).collect();
        // SAFETY: immutable, self-generated, exact-length Portable buffers.
        let chain_pv:Vec<_>=chain_pb.iter().map(|b|unsafe{BitmapView::deserialize::<Portable>(b)}).collect();
        let pr:Vec<&Bitmap>=chain_pv.iter().map(|v|&**v).collect();
        assert_eq!(chain(&rr).values(),chain(&pr).values());
        results.push(compare(&format!("and-andnot-chain/{distribution}"),||chain(black_box(&rr)).count(),||chain(black_box(&cr)).count(),||chain(black_box(&vr)).count(),||chain(black_box(&pr)).count(),iterations));
        let run_r: RoaringBitmap = (0..65536u32).filter(|i| i%1024<800).collect();
        let mut run_c=Bitmap::of(&run_r.iter().collect::<Vec<_>>());run_c.run_optimize();run_c.shrink_to_fit();
        let mut run_buf=vec![];let run_bytes=run_c.serialize_into_vec::<Frozen>(&mut run_buf);
        // SAFETY: generated aligned frozen slice, kept immutable while borrowed.
        let run_v=unsafe{BitmapView::deserialize::<Frozen>(run_bytes)};
        assert_eq!(run_r.intervals(),run_c.intervals()); assert_eq!(run_r.intervals(),run_v.intervals());
        let mut run_pb=vec![];run_r.serialize_into(&mut run_pb).unwrap();
        // SAFETY: immutable self-generated exact-length Portable buffer.
        let run_pv=unsafe{BitmapView::deserialize::<Portable>(&run_pb)};
        if distribution=="runs" { results.push(compare("runs/interval-enumeration",||black_box(&run_r).intervals(),||black_box(&run_c).intervals(),||black_box(&*run_v).intervals(),||black_box(&*run_pv).intervals(),iterations/10+1)); }
        let mut rb=vec![]; rrows[12].serialize_into(&mut rb).unwrap();
        let cb=crows[12].serialize::<Portable>();
        assert_eq!(Bitmap::try_deserialize::<Portable>(&rb).unwrap().to_vec(),rrows[12].iter().collect::<Vec<_>>());
        assert_eq!(RoaringBitmap::deserialize_from(cb.as_slice()).unwrap(),rrows[12]);
        results.push(json!({"case":format!("serialization/{distribution}"),"roaring_portable_bytes":rb.len(),"croaring_portable_bytes":cb.len(),"croaring_frozen_bytes":slices[12].1,
            "portable_cross_read":true,"frozen_alignment":32}));
        // Include the per-open view creation cost, rather than timing prebuilt views only.
        let frozen_data=&buffers[12][slices[12].0..slices[12].0+slices[12].1];
        results.push(compare(&format!("deserialize/{distribution}"),
            ||RoaringBitmap::deserialize_from(black_box(rb.as_slice())).unwrap().len(),
            ||Bitmap::try_deserialize::<Portable>(black_box(cb.as_slice())).unwrap().cardinality(),
            ||{ // SAFETY: self-generated immutable aligned frozen buffer above.
                unsafe{BitmapView::deserialize::<Frozen>(black_box(frozen_data))}.cardinality()
            },||{ // SAFETY: same immutable generated Portable bytes above.
                unsafe{BitmapView::deserialize::<Portable>(black_box(rb.as_slice()))}.cardinality()
            },iterations));
    }
    println!("{}",json!({"roaring":"0.11.5","croaring":"2.8.0","shard_buckets":65536,"rounds":9,"results":results}));
}

//! Deterministic wide-format generic Parquet robot fleet for W9.
use arrow::{array::{ArrayRef, BooleanArray, Float64Array, Int64Array, StringArray, TimestampMillisecondArray},
    datatypes::{Field, Schema}, record_batch::RecordBatch};
use parquet::{arrow::ArrowWriter, basic::Compression, file::properties::WriterProperties};
use std::{path::Path, sync::Arc};

pub const START: i64 = 1_777_593_600; // 2026-05-01 UTC
pub const DAYS: i64 = 2;
pub const ROBOTS: usize = 3;
fn write(path: &Path, columns: Vec<(&str, ArrayRef)>) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let schema = Arc::new(Schema::new(columns.iter().map(|(name, array)|
        Field::new(*name,array.data_type().clone(),true)).collect::<Vec<_>>()));
    let batch = RecordBatch::try_new(schema.clone(),columns.into_iter().map(|(_,a)|a).collect()).unwrap();
    let props = WriterProperties::builder().set_compression(Compression::SNAPPY).build();
    let mut writer = ArrowWriter::try_new(std::fs::File::create(path).unwrap(),schema,Some(props)).unwrap();
    writer.write(&batch).unwrap(); writer.close().unwrap();
}
/// One file per robot/day bounds generator memory. Units and mapping accompany the data.
pub fn generate(root: &Path, seed: u64) -> (usize,usize) {
    let mut files=0;
    for robot in 0..ROBOTS {
        let entity=format!("robots.urn:robot:{}",robot+1);
        for day in 0..DAYS {
            let mut times=Vec::new(); let mut battery=Vec::new(); let mut current=Vec::new();
            let mut modes=Vec::new(); let mut faults=Vec::new(); let mut enabled=Vec::new();
            let mut x=Vec::new(); let mut y=Vec::new(); let mut temperature=Vec::new();
            for second in 0..86400i64 {
                let t=START+day*86400+second;
                let cycle=(second+(robot as i64)*120).rem_euclid(1200);
                let low=(300..900).contains(&cycle);
                let moving=cycle<1000;
                let phase=((seed%97) as f64+second as f64)/31.0;
                times.push(t*1000);
                battery.push(if second%37==0{None}else{Some(if low{24.0+(phase.sin()+1.0)*0.2}else{25.8+phase.sin()*0.1})});
                current.push(if moving{12.0+(phase.cos()+1.0)*3.0}else{1.5});
                modes.push(if moving{"moving"}else{"charging"});
                faults.push(if low{"battery"}else if (100..160).contains(&cycle){"overcurrent"}else{"none"});
                enabled.push(moving);
                x.push(robot as f64*100.0+(second%600) as f64*0.1);
                y.push(robot as f64*10.0+((second%3600) as f64/360.0).sin()*5.0);
                temperature.push(30.0+phase.cos()*2.0);
            }
            let length=times.len();
            write(&root.join(format!("raw/robot-{}-day-{day}.parquet",robot+1)),vec![
                ("robot_id",Arc::new(StringArray::from(vec![entity.as_str();length]))),
                ("timestamp",Arc::new(TimestampMillisecondArray::from(times).with_timezone("UTC"))),
                ("battery_voltage",Arc::new(Float64Array::from(battery))),
                ("motor_current",Arc::new(Float64Array::from(current))),
                ("mode",Arc::new(StringArray::from(modes))),
                ("fault",Arc::new(StringArray::from(faults))),
                ("enabled",Arc::new(BooleanArray::from(enabled))),
                ("pose_x",Arc::new(Float64Array::from(x))),
                ("pose_y",Arc::new(Float64Array::from(y))),
                ("temperature",Arc::new(Float64Array::from(temperature))),
            ]);
            files+=1;
        }
    }
    let mut entities=Vec::new(); let mut ids=Vec::new(); let mut starts=Vec::new(); let mut ends=Vec::new();
    let mut kinds=Vec::new();let mut titles=Vec::new();let mut bodies=Vec::new();
    for robot in 0..ROBOTS {
        for day in 0..DAYS {
            entities.push(format!("robots.urn:robot:{}",robot+1));
            ids.push(format!("robot-{robot}-battery-{day}"));
            starts.push((START+day*86400+300)*1000);
            ends.push(Some((START+day*86400+900)*1000));
            kinds.push("notes"); titles.push("Battery inspection");
            bodies.push("Battery voltage low; inspect battery and motor current.");
        }
    }
    write(&root.join("documents/incidents.parquet"),vec![
        ("entity",Arc::new(StringArray::from(entities))),
        ("id",Arc::new(StringArray::from(ids))),
        ("start_ms",Arc::new(Int64Array::from(starts))),
        ("end_ms",Arc::new(Int64Array::from(ends))),
        ("kind",Arc::new(StringArray::from(kinds))),
        ("title",Arc::new(StringArray::from(titles))),
        ("body",Arc::new(StringArray::from(bodies))),
    ]);
    std::fs::write(root.join("units.toml"),r#"[units]
'robot.battery_voltage' = {unit='V',scale=3}
'robot.motor_current' = {unit='A',scale=2}
'robot.pose_*' = {unit='m',scale=3}
"#).unwrap();
    // A real Signal K raw-tier boat fixture exercises mixed identity front doors.
    let boat = "vessels.urn:mrn:signalk:uuid:robot-oracle-boat";
    write(&root.join("signalk/tier=raw/context=boat/path=navigation__speedOverGround/boat.parquet"), vec![
        ("context",Arc::new(StringArray::from(vec![boat;6]))),
        ("path",Arc::new(StringArray::from(vec!["navigation.speedOverGround";6]))),
        ("signalk_timestamp",Arc::new(TimestampMillisecondArray::from((0..6).map(|i|(START+i*10)*1000).collect::<Vec<_>>()).with_timezone("UTC"))),
        ("source_label",Arc::new(StringArray::from(vec!["gps";6]))),
        ("value",Arc::new(Float64Array::from(vec![1.5;6]))),
    ]);
    (files,1)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn robot_profile_is_byte_identical_and_has_six_day_partitions() {
        let root=Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("robot-gen-{}",std::process::id()));
        let first=root.join("first");let second=root.join("second");
        assert_eq!(generate(&first,42),(6,1));
        assert_eq!(generate(&second,42),(6,1));
        for entry in std::fs::read_dir(first.join("raw")).unwrap() {
            let name=entry.unwrap().file_name();
            assert_eq!(std::fs::read(first.join("raw").join(&name)).unwrap(),std::fs::read(second.join("raw").join(&name)).unwrap());
        }
        assert_eq!(std::fs::read(first.join("documents/incidents.parquet")).unwrap(),std::fs::read(second.join("documents/incidents.parquet")).unwrap());
        std::fs::remove_dir_all(root).unwrap();
    }
}

//! Time each stage of opening a Lume TI store for SQL.
//!
//! ```text
//! cargo run --release -p ti-sql --example open_timing -- <store_root> [width_seconds]
//! ```

use std::path::Path;
use std::sync::Arc;
use std::time::Instant;
use ti_contracts::Catalog;
use ti_store::Store;

#[tokio::main]
async fn main() -> datafusion::common::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let root = Path::new(
        args.first()
            .expect("usage: open_timing <store_root> [width]"),
    );
    let width: u64 = args.get(1).map_or(10, |w| w.parse().expect("width"));

    let t = Instant::now();
    let store = Store::open_or_create(root, width).expect("open store");
    println!(
        "Store::open_or_create   {:>9.3} s",
        t.elapsed().as_secs_f64()
    );

    let t = Instant::now();
    let fields = store.catalog().fields().expect("fields");
    println!(
        "catalog.fields ({:>5})  {:>9.3} s",
        fields.len(),
        t.elapsed().as_secs_f64()
    );

    let t = Instant::now();
    let session = ti_sql::session_from_store(Arc::new(store), width).await?;
    println!(
        "session_from_store      {:>9.3} s",
        t.elapsed().as_secs_f64()
    );

    for sql in [
        "SELECT count(*) FROM shards",
        "SELECT count(*) FROM telemetry",
        "SELECT count(*) FROM telemetry WHERE vessel = 'vessels.urn:mrn:imo:mmsi:367000000'",
    ] {
        let t = Instant::now();
        let rows = session.query(sql).await?;
        println!(
            "{:<70} {:>9.3} s ({} batches)",
            sql,
            t.elapsed().as_secs_f64(),
            rows.len()
        );
    }
    Ok(())
}

//! Compatibility wrapper over the common backfill front door.
//! `backfill_store <raw_dir> <store_root> [self_urn]`; TI_WIDTH/TI_OPT_IN remain supported.
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    ti_ingest::backfill::run_signalk(&args);
}

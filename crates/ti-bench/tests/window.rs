//! The correctness window must match the dates the golden corpus queries anchor to
//! (tests/golden/corpus.json uses 2026-03-01 .. 2026-06-01 UTC).

use chrono::{TimeZone, Utc};

#[test]
fn correctness_window_matches_corpus_dates() {
    let start = Utc
        .with_ymd_and_hms(2026, 3, 1, 0, 0, 0)
        .unwrap()
        .timestamp();
    let end = Utc
        .with_ymd_and_hms(2026, 6, 1, 0, 0, 0)
        .unwrap()
        .timestamp();
    assert_eq!(
        ti_bench::gen::START_SECS,
        start,
        "START_SECS must be 2026-03-01T00:00:00Z"
    );
    assert_eq!(
        ti_bench::gen::END_SECS,
        end,
        "END_SECS must be 2026-06-01T00:00:00Z"
    );
}

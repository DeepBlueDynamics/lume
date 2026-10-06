# ti-core

W1 implements bitmap rows and the frozen predicate/aggregate semantics. It depends
only on ti-contracts at runtime, is an optional root dependency under `ti`, and
contains no unsafe code. All algorithms are original implementations; no external
source code was copied. Module documentation identifies the prefix, popcount,
greedy-extreme and SQL truth-mask algorithms.

[rows.rs](src/rows.rs) supplies:
- PresenceRow with one existence bitmap.
- SetField with stable catalog dictionary IDs. Ordinary sets replace the old
  column value; source_set() retains multiple sources. clear() removes the
  column from every row and presence.
- BsiField with exists, sign and low-to-high magnitude slices. Scale 0..18;
  stored values cover the complete i64 domain, including MIN. Depth growth only
  appends slices; clearing never shrinks depth. compare() tracks BETWEEN bounds
  in one row traversal. sum() uses i128; min/max use greedy bitmap prefixes.
- CountField with unsigned u64 magnitudes and no sign row. Predicate literals
  are signed because the frozen IR is signed; negative thresholds are below
  every count. Checked aggregation reports overflow if an unsigned extreme
  cannot fit the frozen AggPartial i64 variant.
- Batch values() in ascending requested-column order, preserving nulls.
- Checked from_bitmap/from_rows constructors for portable persistence import.
  They validate local-column bounds, depth, existence subsets, dictionaries,
  the D21 invariant, negative zero and invalid sign/magnitude combinations.

[evaluator.rs](src/evaluator.rs) provides MemoryShard, an atomic in-memory
apply fixture, and MemorySource, a ShardSource-shaped evaluator. SQL TRUE,
FALSE and UNKNOWN are disjoint masks over the existing-bucket universe.
Short-circuiting requires a decisive FALSE/TRUE result; an empty true mask
alone is insufficient. Text delegates to the frozen TextIndex trait. Geo uses
stored cell rows or an injected GeoIndex and carries inexactness through
compound expressions; enclosing NOT returns Unsupported pending W6 refinement.

Arrow batch materialization is W4 work: MemorySource::read explicitly returns
Unsupported. Durable WAL/flush/seal are W2 work: MemoryShard::apply stages a clone
and publishes only after complete validation, without claiming disk durability.
An ordinary scalar group rejects multiple distinct values; source/geo rewrite
groups install all values after one clear. Non-rewrite numeric replacement with
a different value is rejected; rewrite makes replacement explicit.

[reference.rs](src/reference.rs) is an independent scalar model using
Vec<Option<i64>>, Vec<Option<u32>>, plain comparisons and explicit Kleene truth
tables. It does not reuse row algorithms or bitmap masks.

Run checks with:
```sh
cargo test --locked -p ti-core
cargo clippy --locked -p ti-core --all-targets -- -D warnings
cargo fmt -p ti-core --check
```

Four proptests each fix the case count at 10,000: signed comparisons/aggregates/
reconstruction/depth growth, unsigned counts, random predicate trees of depth
at most four, and arbitrary set apply/rewrite sequences with D21 and replay
checks. Edge tests cover extrema, corruption on import, failed transaction
atomicity, nulls, delegates and shard boundaries. Golden SQL-to-IR fixtures
provide W4 with literal expected results for filter and aggregate paths; they
exercise IR and do not parse SQL.

## Performance baseline

Command: `cargo run --locked --release -p ti-core --example compare_baseline`.

2026-10-06 container measurement: rustc 1.99.0, x86_64 Intel Core Ultra 9 285K,
Microsoft hypervisor reported by lscpu. One positive depth-16 BSI over a full
65,536-column shard; values are a multiplication permutation of 0..65535.
LT 32768 returns 32,768 matches. After 100 warmups, 2,000 iterations took
82.301 ms, mean **41.150 microseconds** per comparison. Construction is excluded;
comparison includes bitmap result allocation. debug_assertions=false. Other
builds were active on the host. This is a non-gating baseline for W8, not a Pi
measurement.

## Verification (2026-10-06)

The final full `cargo test --locked -p ti-core` run passed 13 tests: eight edge
tests (0.01 s), one golden fixture test (0.00 s), and four property suites at
10,000 cases each (28.04 s, excluding compilation). Earlier property runs were
32.21 s and 48.17 s under concurrent compilation; the reported final run is
28.04 s. The golden test contains 17 filter and five aggregate fixtures.

Root `cargo build --locked`, `cargo test --locked` (42 tests), and
`cargo build --locked --features ti` passed. Strict all-targets core clippy,
scoped formatting and diff whitespace checks passed. The root's pre-existing
unused-assignment warning at src/main.rs:1799 remains within the integrator's
accepted exception. Cargo metadata verifies four active root default
dependencies and only ti-contracts as ti-core's runtime dependency. D23 records
proptest 1.x (lockfile resolves 1.11.0); numbering was coordinated with Prawn,
whose D22 is reserved for ti-bench. No ti-contracts source was changed.

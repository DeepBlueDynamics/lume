# 14. Shared semantics (W0 contracts freeze)

Status: contracts-freeze draft for integrator review; spec 10 mirrors the crate interfaces.
Sources: spec 05 (column space/encoding), 06 (rewrite/WAL), 07 (SQL), 10 (contracts).
W0 implements shared types, schema/config builders and checked helpers; evaluator,
store, ingest and interval behavior below must be implemented and tested by their lanes.

## Nulls and negation (gap 1)

- `All` is the existing telemetry bucket universe in the shard, not all 65,536 positions. Rationale: SQL telemetry rows exist only for buckets with any data.
- `None` is empty; `And([])` is All and `Or([])` is None. Rationale: boolean identities make generated IR deterministic.
- Internally evaluate SQL expressions as true/false/unknown masks; `eval` returns only the true mask. `Not` exchanges true and false and preserves unknown. Rationale: complementing the true bitmap alone would turn null comparisons into matches.
- Missing field values make comparisons unknown, including `!=`, `NOT IN` and compound negation; `Present` and its negation are two-valued IS NOT NULL/IS NULL tests. Rationale: preserve SQL three-valued logic and the specified presence-ANDNOT rule.
- Nullable IN lists cannot be expressed by today's row-ID-only IR; leave such filters in DataFusion unless an exact translation is proved. Rationale: IN containing NULL changes negation behavior.
- For multi-valued sets, positive membership means any matching row; negative membership means present and no matching row. Rationale: matches the spec's row-OR/presence-ANDNOT rule; SQL list representation remains gap 3.
- Inexact geo filters remain inexact through compound boolean expressions, and NOT must not complement a conservative geo cover to obtain an exact result. Rationale: a cover's false positives can become false negatives under negation.

## Rewrite and durability (gap 4)

- One `apply` slice is a transaction boundary. Group records by (vessel, bucket, field); all records in a group must agree on rewrite. Rationale: a per-record clear would discard preceding set values.
- A rewrite group clears the field's entire column once, then installs every group value; duplicate set rows/cells are idempotent. Reject incompatible value encodings or multiple distinct numeric values before mutation. Rationale: late-data repair replaces a complete field aggregate.
- Validate the complete slice before appending or publishing; query readers see either the old or new state. A failed apply may have uncertain durable outcome after an I/O failure and requires recovery before retry. Rationale: errors must not expose half a multi-field rewrite or claim an unprovable rollback.
- D16: `apply` acknowledges after a buffered framed WAL append, then publishes in-memory state. Group-commit fsync runs at least every 1 second and always before `flush` advances/truncates the WAL and at shutdown. Catalog dependencies are persisted before dependent records become durable. Rationale: bound power-loss exposure to about one second without paying per-apply fsync on the Pi.
- `seal` flushes, durably writes the immutable version, then atomically publishes and syncs the manifest; it returns only after durable publication. Rationale: replication must never discover a partial version.
- Partial backfill must reconstruct the complete bucket/field from authoritative samples, including previously ingested samples, before rewrite; it must not overwrite a full aggregate with only the new fragment. Rationale: rewrite idempotence alone does not guarantee merge correctness.
- W2 may benchmark per-apply fsync as an optional stronger durability mode; buffered acknowledgement is the contract. Rationale: apply success does not claim power-loss durability before group commit.
- `FieldValue::Clear` is valid only with rewrite=true and as the sole record for its bucket/field group; it removes every row bit including presence. `validate_clear_records` rejects misuse before mutation. Rationale: deletion has an explicit marker rather than a numeric sentinel.

## Numeric and time edges (gap 5)

- Helpers use Unix seconds (`i64` timestamp), EPOCH = 1577836800, and positive whole-second `u64` width. Reject pre-epoch time and bucket indices beyond u32. Pre-2020 history cannot be ingested; the lead accepts this for PV-1. W3 preserves subsecond event timestamps for ordering before bucketing. Rationale: matches the unsigned address space while avoiding subtraction overflow.
- Buckets cover [start, start + W); TsRange endpoints are inclusive bucket indices, and reversed ranges are empty. Rationale: distinguish sample-time bucketing from the explicitly inclusive IR range.
- Fixed-point uses f64 input, decimal scale 0..=18 and ties away from zero; reject NaN, infinity and results outside [-2^63, 2^63). Negative zero encodes zero. Rationale: give ingest, literals and the DuckDB oracle one checked rule.
- Scale >18 is rejected even though FieldKind stores u8; this is an explicit proposal to constrain configuration, not a type change. Rationale: decimal factors above signed-integer precision are not useful for this v1 encoding.
- `from_fixed` returns f64 and may lose integer precision above 2^53; tolerance(scale) = 0.5 / 10^scale covers quantization only. Rationale: callers must not assume lossless integer-to-float conversion or a constant tolerance across units.
- SQL literal thresholds use the same to_fixed helper as ingest. Rationale: the spec requests consistent scaling and rounding.
- A document with ts_end covers [ts_start, ts_end); missing ts_end is a point assigned to its start bucket. Empty/reversed intervals are rejected; a point must use missing ts_end. Rationale: avoid matching the next bucket when an interval ends exactly at its boundary.
- intervals() returns half-open [start, end), merges runs only within a vessel (including across shard boundaries), and measures max_gap by missing-bucket duration. Rationale: consecutive shards must not split a continuous passage.
- Apply min_len after gap merging to the wall-clock span; buckets counts only matching buckets and excludes bridged gaps. Rationale: preserve evidence count while reporting elapsed passage duration.

### Required fixtures for downstream lanes

- Null comparison, IS NULL, nested NOT/AND/OR, IN with NULL, present multi-valued sets, sparse All universe, and NOT of inexact geo.
- Multiple set/cell rewrite records, mixed rewrite flags, invalid later record, crash at each WAL/flush/seal boundary, empty rewrite, partial backfill and replay twice.
- Implemented helper tests: epoch/pre-epoch, zero width, u32 max, shard boundary reconstruction, negative ties, negative zero, nonfinite input, i64 overflow, scale bounds and per-scale tolerance.
- Text ending on a bucket boundary, point documents, reversed ranges; interval runs crossing a shard, gap exactly at max_gap, min_len after merge and count excluding gaps.

## Newly defined contract types

- AggOp: CountAll, Count, Sum, Min, Max. Rationale: query operations differ from ingest Agg profiles.
- AggPartial: Count(u64), Sum { sum: i128, count: u64 }, Min(Option<i64>), Max(Option<i64>). Rationale: preserve empty/null state and enough integer width for per-shard sums; consumers must validate operation/variant agreement and merge with checked arithmetic.
- ShardManifestEntry: key, version (u64 starting at 1), inclusive from/to buckets, bytes (u64), raw 32-byte BLAKE3 hash. Rationale: minimum identity and coverage for sealing; canonical envelope is gap 6.
- Error/Result: invalid input, overflow, corruption, missing item, unsupported capability, I/O and Arrow errors. Rationale: share diagnostics without adding a runtime error dependency.
- D17: shared value types derive Debug, Clone, PartialEq and Eq where possible; WAL value types also derive serde serialization. FieldValue gains Clear. Error remains Debug-only because it retains non-cloneable/non-comparable OS errors; QueryResult has PartialEq but no Eq because Arrow values may be floating point. Rationale: fixtures/proptests can print and compare values without throwing away error causes.

## Catalogs and dictionaries (gap 2)

- Catalog registration is serialized, idempotent and persistent. IDs are u32, monotonically allocated without reuse, and overflow errors rather than wrapping. Field identity is (exact path, aggregate); incompatible kinds/scales/units at that identity are rejected. Rationale: W3 cannot silently reinterpret W2's stored rows.
- W3 resolves vessels.self to the hello/self URN, calls register_vessel, then register_field, then register_set_value before emitting SetValue(row). Registration/catalog dependencies must be durable before their referencing WAL is durable. Rationale: crash replay must resolve every ordinal, field and row.
- Ordinary state fields retain the last event-time value of the preferred source per bucket; repeated timestamp ties use a stable receive sequence. Source fields retain all reporting sources. For an ordinary set field, row bitmaps are pairwise disjoint and their union equals field presence; validate_ordinary_set_rows checks it, and W1/W2 proptest it. Mid-bucket changes belong to @starts/edge counters. Rationale: Exact bitmap pushdown and scalar projection describe the same last state.
- Source priority is exact-path and most-preferred-first; absent preferences use first-seen order. Keys are normalized sourceRef strings: live $source first, Parquet source_label next, else W3 derives getSourceId from source. Source-object rules follow pinned plan/design/signalk-formats.md §2.1: label.canName, label.src, plugin label, else label.talker or label.XX. Rationale: compare the same identity across live and history.
- Unknown source is absence, not a synthetic named source. Notification domain includes nominal, normal, alert, warn, alarm, emergency; null clear emits Clear while v2 state normal remains a present normal value. Rationale: preserve both real clear styles without treating nominal as invalid.
- Shore registers URNs to local vessel ordinals; source store ordinals never identify a vessel globally. Keep immutable field files and original field/dictionary namespaces under catalog_hash, with an import mapping to local catalog IDs. Adapt predicates/read results through that mapping. WAL replay maps ordinal-bearing records; global ColumnIds are recomputed. Rationale: importing into a populated shore store must not alter source content hashes.

## Arrow and engine boundaries (gap 3)

- telemetry_schema emits non-null vessel Utf8 (URN), ts Timestamp(Second, UTC), then field-ID order; BSI is nullable physical-unit Float64, Count UInt64, Presence Boolean, Set Utf8, Geo List<UInt64>. Mean adds its bare-path alias immediately after @mean. Virtual notes/logbook/alerts Utf8 placeholders follow, meaningful only to match() planning. Rationale: stable schemas expose physical units and preserve nulls.
- $source is nullable List<Utf8> with non-null items, lexicographically sorted distinct sourceRefs when materialized. Missing means null. W4 rewrites $source scalar equality to array_has and IN to an OR of array_has during resolution before DataFusion list/string type coercion; pushdown remains Exact; general list SQL remains available. Rationale: a delimited string would lose source boundaries and escaping.
- Ordinary Set columns are scalar last-state fields; positive membership/negative membership rules apply to source sets, not a hidden scalar concatenation. Rationale: projections and bitmap predicates must agree.
- docs_schema order is id, vessel, kind, ts_start, nullable ts_end, title, body, nullable Float64 score. DocumentIndex upserts stable IDs, deletes idempotently and invalidates text caches on changes; documents(q=None) yields null score and documents(q=Some) attaches query-time BM25 score. Raw oracle context is explicitly renamed to TI vessel. Rationale: score is query-dependent, not persistent document metadata.
- Catalog builders freeze names/types/nullability/order in schemas.rs: vessel ordinal UInt32 and URN Utf8; paths include nullable UInt8 scale/depth; shards use URN, UInt32 shard_no, UTC-second ts_from/ts_to, Boolean sealed, UInt64 bytes and nullable lowercase-hex hash. ts_to is exclusive. Rationale: catalogs have consistent time and identity types.
- TiEngine is a minimal object-safe synchronous fixture boundary: schema, query, explain, status. QueryResult carries same-schema RecordBatches, truncation, elapsed milliseconds and pushdown summary; EngineStatus reports lag/WAL/open/sealed/sync. Production W7 may adapt to streaming and async scheduling without adding runtime dependencies here. Rationale: W4/W5/W7 can develop independent fixtures.
- Document start/end validation rejects pre-epoch, missing ID/URN, unsupported kind and non-positive explicit ranges. Title/body are non-null strings, possibly empty. Rationale: resource updates preserve stable identity and known time semantics.

## Versioned persistence and transfer (gap 6)

- All integer envelope fields are fixed-width little endian; versions start at 1 and unknown versions are rejected before payload decoding. WAL magic LUMETIW1, shard magic LUMETIS1. Header URNs are u32-length-prefixed UTF-8. Rationale: avoid platform-native layouts or silently parsing a future format.
- WAL segment header is magic[8], version u16, URN length u32, URN bytes. Every record frame is payload_len u32, sequence u64 (starts at 1), IEEE CRC32 u32, payload bytes; CRC covers sequence LE bytes plus payload. Rationale: detect torn records and deduplicate replay/tail uploads.
- WAL payload is a complete apply slice Vec<BucketRecord> serialized by bincode 1.x fixed-integer little-endian encoding, reject trailing bytes; the codec/dependency will be added by W2 with a decisions entry. Enum order is the declaration order frozen in spec 10, including Clear as variant 0. Rationale: multi-field and deletion rewrites replay as one transaction. Frame lengths must be bounded by reader configuration before allocation.
- Shard header is magic[8], version u16, length-prefixed URN, shard u32, field u32, width_seconds u64, row_count u32. Then ordered rows: kind u8 (0 presence, 1 set, 2 exists, 3 sign, 4 magnitude-bit, 5 geo), key u64, portable_payload_len u64, portable roaring bytes. Order by (kind,key); magnitude keys are bit indices, set keys are source dictionary row IDs, geo keys are H3 IDs, other keys zero. Rationale: deterministic portable rows support independent readers.
- Canonical BLAKE3 input is domain bytes LumeTI/shard/v1 followed by NUL, then ascending field-ID files framed as field u32, file_len u64, exact versioned file bytes. canonical_shard_input implements the ordering/framing and rejects duplicate IDs; W2/W8 stream equivalent bytes into pure-feature BLAKE3. Never hash tar headers, repair version, mtimes or transfer chunk order. Rationale: identical contents produce identical hashes.
- Catalog snapshot hash uses domain LumeTI/catalog/v1 plus NUL and compact UTF-8 JSON with lexicographically sorted object keys; vessels sorted by URN, fields by ID, dictionaries by (field,row), priorities by path; omit observation times and mutable descriptions. It includes field kind/scale/units, source dictionary strings and source namespaces. Rationale: transferred row IDs cannot be decoded with a different dictionary.
- TransferIdentity is (URN, shard, repair version, width, inclusive coverage, content hash, catalog_hash); validate coverage remains in that shard and width/version are positive. Local ShardManifestEntry is a store-local view, translated into transfer identity using its catalog. Import validates both hashes and installs the catalog before publishing the manifest. Preserve origin identity when remapping local IDs. Rationale: versioning and federation must not depend on foreign ordinals.
- Upload chunks use transfer identity plus byte offset; duplicate bytes are idempotent, differing bytes at the same offset are errors. WAL tails use URN and inclusive sequence bounds; duplicate records are ignored, gaps require retransmission. Rationale: resume safely over intermittent links.

## ti.toml and corpus coordination

- TiConfig is serde/TOML with strict unknown-key rejection, recursive defaults and key-qualified validation; partial unit tables override named units while retaining unspecified defaults. Width 10s, root ./ti, retention 2y, field cap 2000; default aggregate mean/min/max, slow last and opt-in list; deny *.ais.* and design.*; unit table follows spec 05 (deg/lat-lon 7, unknown numeric 3). Rationale: deployments start with the documented budget and typed values.
- Listener defaults bind 0.0.0.0 inside containers with host exposure restricted to LAN, HTTP 8080/pg 5432; auth supports optional bearer, NUTS and SCRAM-SHA-256 verifier users. Query defaults are 30s, 1GiB, whole-unit 1.5GiB, 2 partitions, 1 heavy query, 75°C; configure 512MiB for Pi4. Rationale: schema includes the complete boat governance boundary without storing plaintext pg passwords.
- Signal K stores the server-returned access_request_href verbatim, and no_auth_required when a security-disabled access request returns 404; W7 polls that href rather than constructing one. Rationale: server polling path differs from the POST path.
- Config validates nonzero widths/limits/ports, IP addresses, distinct ports, scales <=18, unique source priorities/users, known aggregate/rule names and transition-state presence; persisted width changes require rebuild. SCRAM cryptographic validation belongs to pgwire. Rationale: invalid config names the key before opening a store.
- Agree tests/golden/paths.md path names and twin-motor starboard fields with Prawn, retaining exact planted keywords. Revolutions scale2 is an explicit override against Hz default1. Corpus correction work belongs to Prawn; schema context→vessel mapping and aggregate/endpoints must agree. Rationale: synthetic fixtures and oracle read the same semantics.

## W6 geographic candidates and refinement

Geo SQL binds `navigation.position.latitude@last` and `navigation.position.longitude@last` during typed analysis. Bbox endpoints are inclusive; longitude min greater than max crosses the antimeridian. The +180/-180 meridian is equivalent. Radius is finite and nonnegative, with an inclusive haversine comparison on the oracle sphere (R = 3440.065 nautical miles); missing coordinates produce SQL UNKNOWN.

The Inexact candidate predicate is OR(GeoCover, BSI bounding envelope). H3 rows include every reported position at resolutions 5, 7 and 9. The BSI envelope independently retains true matches when the last preferred-source coordinates were reported separately, coordinates round across a cell edge, or old shards contain placeholder/missing cells. A cover construction failure falls back to the BSI envelope. DataFusion always re-applies exact bbox or haversine refinement; NOT of a geo candidate remains Unsupported and never complements the cover. This safety design was approved by Industrial Pike on 2026-10-06.

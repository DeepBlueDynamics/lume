# D50 — agent telemetry via OTLP HTTP/JSON

Approved scope: A1 in next-phase-2026-10-08.md. No dependency additions.

Opt-in POST /v1/metrics and /v1/logs share the TI HTTP listener when --otlp is present on serve --ti-store or ti ingest --serve. The standalone command is lume ti otlp --store ROOT --bind 127.0.0.1 --port 4318. Standalone defaults to 4318; integrated mode retains the HTTP port (5863). --otlp-token-file reads a bearer token at startup. A non-loopback bind requires this token. application/x-protobuf receives 415 with: lume accepts OTLP http/json only; set protocol json. JSON only, maximum 8 MiB (413), invalid JSON or semantic fields 400, storage failures 503. Disabled endpoints return 404. No wildcard CORS.

Resource identity chooses service.instance.id, pane, pane.id, then service.name and prefixes agent.urn:. Metrics use their names as paths in ROOT/stores/agents, a 10-second store auto-registered as telemetry_agents. Monotonic sums are running totals per entity, metric and sorted attribute set. Delta (aggregationTemporality=1) values add to the total; cumulative (2) values replace the current segment, with a changed startTimeUnixNano adding a new segment on top of the previous total. Decreases without a cumulative start reset and out-of-order counter points are rejected explicitly. Totals and cumulative segment state survive receiver restarts in ROOT/stores/agents/otlp-counters.json (version 1). After each flushed metrics batch, a temporary checkpoint is written, fsynced and atomically renamed; its directory is fsynced on Unix before acknowledging success. A missing checkpoint starts from zero. An invalid checkpoint fails startup with a clear error rather than resetting totals. Loading and writing enforce 4096 series and an 8 MiB checkpoint limit. @last is enabled in the agents store. Non-monotonic sums and gauges use ordinary numeric aggregates (@mean with a bare alias); no @sum aggregate is added. All numeric paths use six decimal places, including fractional seconds and costs, and retain exporter units.

Datapoint dimensions are in deterministic paths: type=input becomes NAME.input; model=claude-sonnet adds .model.claude-sonnet, giving claude_code.token.usage.input.model.claude-sonnet. Other sorted attributes append .KEY.VALUE. Components percent-escape every byte except ASCII alphanumerics, underscore and hyphen, preventing aliases between different dimensions. Histograms expose NAME.sum and NAME.count (including these same dimensions). Model is therefore in the path, rather than a separate document. Counter cardinality is capped at 4096 series; the latest 128 exact request payloads are deduplicated in memory. This payload dedup cache is lost on restart: older multi-point payload retries can be rejected as out of order. The last point's timestamp/value remains in the checkpoint for point-level retry detection. The numeric 128-window repair snapshots are also memory-only, so restart does not preserve full value aggregate history for repairs within an already flushed bucket. The checkpoint's last counter observation seeds the bounded repair cache so a same-bucket continuation can rewrite @last correctly; @mean/@min/@max in that restarted bucket use the seeded last observation plus post-restart samples. A crash between the telemetry flush and checkpoint publication can leave an unacknowledged batch in telemetry; these two files are not a single transaction.

Usage over an observed window is MAX(@last) - MIN(@last) per entity and dimensional path. Include an initial baseline observation; receiver restarts preserve the running total. For the fixture:

```sql
SELECT vessel,
  max("claude_code.token.usage.input.model.claude-sonnet@last")
  - min("claude_code.token.usage.input.model.claude-sonnet@last") AS tokens
FROM telemetry_agents
WHERE ts >= TIMESTAMP '2020-01-01 00:00:10'
  AND ts < TIMESTAMP '2020-01-01 00:01:00'
GROUP BY vessel;
```

The delta fixture returns 30; cumulative including an exporter start reset returns 35.

Each batch passes through its own WatermarkBucketer and flushes before success. The OTLP bucketer opts into the existing bounded 128-window repair cache for numeric snapshots, so multiple exports inside a bucket preserve prior samples. Older repairs outside the cache are explicitly rejected; vessel ingestion keeps its existing behavior. Repeated log exports are idempotent using a content-derived BLAKE3 document id.

Log records become kind=logbook documents in ROOT/docs (the existing shared docs table), keyed by agent entity; title is eventName or event.name, body contains attributes and the exported body, ts_start is event time (observed time when event time is absent/zero). This avoids replacing vessel documents or adding a separate docs table. Agent telemetry has independent 90-day retention; the same retention applies only to otlp:-owned documents. The retention clock follows the newest event in the export so historical fixtures/backfill are deterministic.

Hand-written serde structs cover resourceMetrics/resourceLogs and ignore unknown OTLP fields. Decimal-string 64-bit numbers and numeric JSON numbers are accepted. Unsupported metric kinds fail explicitly; there is no protobuf, gRPC, traces, exponential histogram, or OTLP SDK dependency. JSON examples follow the [OTLP specification](https://opentelemetry.io/docs/specs/otlp/).

Validation: tests/golden/otlp/{metrics,logs,delta,cumulative}.json are offline Claude Code-style fixtures. tests/ti_otlp.rs checks SQL totals, histogram paths, identity, full-text file-path search, repeated same-bucket exports, idempotent logs, bearer accept/reject, malformed payloads, 413 before body allocation, disabled endpoints, and standalone loopback defaults. A4 adds process-restart delta/cumulative fixtures, exporter reset after restart, and corrupt-checkpoint startup failure.

Verified locally with rustc 1.96.0: cargo test --locked --features ti, cargo test --locked -p ti-ingest, strict clippy on ti-ingest and ti-sql --all-targets, rustfmt --check on touched Rust files, and cargo build --locked without features all passed. Existing fixture-dependent ignores remain. Test-only stabilization disables background warm-up in the catalog-rename HTTP fixture and waits for observable ingest/status progress before the live-ingest clock jump; production defaults are unchanged.

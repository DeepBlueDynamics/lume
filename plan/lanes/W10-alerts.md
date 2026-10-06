# W10: alerts as documents (Signal K notifications, and rules that write their own)

Depends on: W3 ingest (live stream, derived notification fields), W5 docs (`DocStore`, `match()`), W4 SQL.
Spec: [05-data-model](../spec/05-data-model.md) (notifications), [06-ingest](../spec/06-ingest.md) §4 (notification messages become documents), [14-semantics](../spec/14-semantics.md) (document ranges).

The aim is to close the loop: every alarm, whether Signal K raised it or Lume TI did, becomes a searchable `alerts` document that covers its time range. `match(alerts, 'battery')`, "every alert this month and what the boat was doing", and rules that reference earlier alerts all then work through the existing SQL.

## Step 1: Signal K notification messages become `alerts` documents

Today a `notifications.*` path becomes a state column (`normal`/`alert`/`warn`/`alarm`/`emergency`) plus a raise count, but its message text is dropped.

- [x] On every transition out of `normal`, open an `alerts` document:
  - `id = notifications/<path>/<raise ts>`;
  - `title` = the notification path, with the state;
  - `body` = the Signal K `message` text plus `method` and `state`;
  - `ts_start` = the raise time.
- [x] On the return to `normal`, close it by setting `ts_end`. While the alert is still active, it stays a point document at its start bucket (spec/14). If the state changes while raised, update the same id.
- [x] Wire it into the live stream (`run_stream_loop` and `run_stream_loop_multi`) and the parquet backfill of `notifications.*` rows. Documents are upserted through `DocStore`, so a re-run is idempotent.

Acceptance: a recorded delta stream with a raise, an escalation and a clear produces exactly one `alerts` document with the right range and text. Then `match(alerts, '<word from the message>')` returns the covering buckets, and a replay leaves the document set unchanged.

## Step 2: rules that write their own alerts

- [ ] **Rules in `ti.toml`** (`[[rules]]`), each with:
  - `name`, `severity`;
  - `when`: a SQL boolean over telemetry columns, the same expression language as `WHERE`;
  - optional `for = "10m"`, the hold duration, using the `intervals()` semantics;
  - optional `vessel` filter;
  - `message`: a template that can reference columns, such as `"house battery {electrical.batteries.house.voltage@min} V"`.
- [ ] **Evaluation:** an incremental evaluator per closed bucket, running the predicate as bitmap pushdown over just the newly closed range. A rule opens an alert document after the condition has held for `for`, and closes it when the condition clears. Backfill evaluates rules over history, so turning a rule on gives you its past alerts too.
- [ ] **Generated alerts are documents:**
  - `kind = 'alerts'`, `id = rules/<name>/<start>`;
  - the body carries the rule name, the message with values filled in, and the predicate text.
  - They are searchable and joinable like imported alerts.
- [ ] **Loop safety:**
  - a rule may reference `match(alerts, ...)`, so "a second bilge alert within an hour" is expressible;
  - evaluation is one pass per closed bucket in rule order;
  - a rule never re-triggers on its own output within the same bucket;
  - each rule has a per-hour cap on alerts.
- [ ] **Surfaces:**
  - `lume ti rules list|test <name> --store` (`test` dry-runs over history and prints what would fire);
  - alerts appear in `lume ti status`;
  - the MCP `ti_status` tool lists active alerts.

Acceptance:
- Over the golden store, a rule like `"electrical.batteries.house.voltage@min" < 24.6 for 5m` produces alert documents whose ranges equal the `intervals()` result for the same predicate. A DuckDB oracle cross-checks them.
- The generated alerts come back from `match(alerts, 'battery')` and the docs-to-telemetry join.
- Re-running the backfill is idempotent (same ids, no duplicates).
- A rule that references earlier alerts fires exactly where expected, and the per-hour cap holds.

## Next (not in this lane)

- **Rules extracted from documents** ("specs that watch themselves"): read an indexed manual or datasheet, propose rules with a citation to the passage, and have a human approve them before they go live.
- **Delivery** of alerts (push notifications, email, Signal K `notifications.lume.*` writes back to the server).

## Step 1 implementation and checks

Notification lifecycle handling is in `ti-ingest::notifications::NotificationDocuments`.
Both live loops decode notification objects before telemetry normalization, using wall-clock
receive time for the existing five-minute skew rule. The parquet reader preserves JSON and
flattened notification messages/method arrays; directory backfill retains episode state
across files. The notification document root is the resolved default-store root.
`backfill_store` sets `config.store_root` to its actual destination.

Replay restores known episode IDs and ranges from DocStore. Escalation replaces title/body
with the latest raised state and message; clearing retains that text and supplies the
exclusive end. An active episode stays a point document. Document timestamps are whole
seconds. Per the lead's ruling, an equal-second raise/clear remains a point document
(`ts_end = None`), preserving the alarm and the frozen contract. A generated trailing
`notification_closed_at: <start>` body marker distinguishes this cleared point from an
active episode on restart; `is_closed_point` exposes that distinction to status readers.

Checks: `cargo test -p ti-ingest` passed 31 tests, with two existing data-dependent tests
ignored. This includes actual mock WebSocket checks for both live loops and a four-file
parquet lifecycle replay. The root feature suite includes a recorded notification stream
whose `match(alerts, 'battery')` hits exactly the three covered buckets.
Default `cargo build` passed. Host rustfmt/strict clippy remain the lead's merge checks.

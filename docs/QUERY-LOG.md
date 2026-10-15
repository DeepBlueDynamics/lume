# Query usage log

The TI engine records query completion as `query_log` in the same SQL session as
telemetry and documents. Read it from the console, Ask, MCP or PostgreSQL:

```sql
SELECT ts, source, identity, sql, elapsed_ms, rows, bytes, error
FROM query_log ORDER BY ts DESC LIMIT 20;
```

`ts` is the query start in UTC. Sources are http, mcp, pg, chat, or cli.
A verified NUTS subject or PostgreSQL user is recorded when known; anonymous and
static-token callers have NULL identity. Credentials, headers, bound parameter
values and non-SQL request fields are never passed to the logger. SQL itself may
contain private literals: protect this table like the rest of the vessel data.
SQL is capped at 4096 UTF-8 bytes with `sql_truncated`; error text at 1024 bytes.
Durations include query execution and result encoding, not socket delivery.
JSON bytes count the engine reply, Arrow bytes the IPC stream, and pg bytes the
encoded response estimate. Failed/cancelled streams preserve their delivered-row
counts and a cause; a query selecting this table appears after its own snapshot.

## Limits and storage

```toml
[query_log]
enabled = true
retention_days = 30
max_rows = 16384
max_bytes = 67108864
```

`lume serve --ti-store ... --no-query-log` and
`lume ti ingest --serve ... --no-query-log` disable recording. The equivalent
config key is `[query_log] enabled=false`. The table remains queryable and empty
when disabled on a fresh store. Existing files are not deleted by disabling.

Each engine shares a per-store writer across reloads. Producers submit through a
256-event nonblocking queue; they never do filesystem IO. A background thread
writes private daily JSONL files in `<store>/query-log/` (directory 0700, files
0600 on Unix), expires old files, and maintains a bounded recent-row view.
This uses a separate event log because SQL/errors are strings and individual
query events must not be averaged into the self-telemetry store's 10 s buckets.
No new dependencies are required.

Age retention is a maximum, not a promise to keep every event: the SQL view holds
at most max_rows recent events; disk writes drop at max_bytes until expiry frees
space. `/ti/status` exposes `query_log_dropped` (queue full, writer unavailable,
IO or byte-budget failure), process lifetime only. Startup reloads retained events
asynchronously, so the table can initially be empty. Expiry runs at least once
per minute while the writer exists; query snapshots also filter expired rows.
Disconnected producers let the background writer drain while the process remains
alive. Process exits can lose queued events; request paths never wait for disk IO. This is operational history, not an audit trail.

Tests cover HTTP GET/POST, MCP, pg identity, failure, UTF-8 truncation, the flag,
retention/reload and a deterministic full-queue drop.

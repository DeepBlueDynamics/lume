# InfluxDB 2.x versus Lume on the same Signal K feed

`influx_vs_lume.py` uses Python 3 standard-library HTTP, CSV, JSON and statistics
code. No pip packages, database writes or cache eviction commands are used.

Supply these existing environment variables:

| Variable | Value |
|---|---|
| INFLUX_URL | Influx server base URL, e.g. http://127.0.0.1:8086 |
| INFLUX_ORG | Organization name or ID |
| INFLUX_BUCKET | Bucket receiving the same vessel's Signal K feed |
| INFLUX_TOKEN | Read token; used only in the HTTP Authorization header |
| LUME_URL | Lume base URL or full /ti/query URL, e.g. http://127.0.0.1:5863 |

The assumed writer layout is measurement = Signal K path, scalar field
`value`, tags `context`/`source`; navigation.position has `lat` and `lon`
fields. Measurements/fields/context are explicitly filtered. Scalar sources
are merged after selecting one context. Lume uses its retained preferred-source
values; it does not retain independently queryable raw values for every source.

Run from the repository root, with the environment already configured:

```bash
python3 bench/influx_vs_lume.py --window 24h --runs 20
python3 bench/influx_vs_lume.py --context vessels.urn:mrn:signalk:uuid:YOUR-UUID --window 6h
python3 bench/influx_vs_lume.py --from 2026-10-01T00:00:00Z --to 2026-10-02T00:00:00Z --runs 20
python3 bench/influx_vs_lume.py --dry-run --from 2026-10-01T00:00:00Z --to 2026-10-02T00:00:00Z
```

The token is never intentionally rendered, and reflected backend errors are
redacted before printing. No environment dump or verbose HTTP logging is used.
Set the token through your existing shell/secret mechanism, not a command-line
argument. URL-embedded credentials and query parameters are rejected.

A single-vessel Lume schema selects that vessel automatically. Multiple
vessels require `--context`. If the writer's context tag omits/adds a prefix,
use `--influx-context` with its exact value; it must identify the same vessel.
The script does not silently pool boats or guess context aliases.

By default, discovery queries find the first/latest depth or SOG timestamps in
each database. The frozen half-open window ends at the earlier latest timestamp
and starts at the later first timestamp or 24 hours before the end, whichever
is later. Excluding the latest timestamp avoids racing the newest sample.
Discovery is outside the timed runs. JSON records the actual bounds; short
coverage can yield less than 24 hours. Supplying both `--from` and `--to`
skips discovery and uses your exact bounds. Supplying just one retains the
other bound's discovery/window behavior. All timestamps require a timezone.

## Query meanings

| Pair | Influx native query | Lume retained query |
|---|---|---|
| 1: depth range | Raw value/time samples | @last if retained, otherwise the catalog's mean/last/min/max preference |
| 2: hourly max depth | Maximum raw values per UTC hour | max(depth@max) per UTC hour |
| 3: minute mean SOG | Mean raw values per UTC minute | avg(SOG@mean) per UTC minute |
| 4: multi-condition | Minutes with mean SOG > X and minimum depth < Y | Same predicate over avg(@mean) and min(@min) |
| 5: minimum depth + position | Earliest raw minimum, exact-timestamp lat/lon join | Earliest minimum bucket, retained lat/lon from that bucket |
| 6: points per path | Raw points for the catalog's retained path set | Populated buckets for that same path set |

X defaults to 2 m/s, Y to 5 m; change `--sog-gt`/`--depth-lt`.
Depth defaults to `environment.depth.belowTransducer`; change `--depth-path`
if the boat uses belowKeel/belowSurface. `--sog-path` is also configurable.
No unit conversions are performed: both writers must contain the same native
Signal K units. `--table telemetry_hr` selects the configured 1-second store.

Aggregate timestamps use UTC bin starts in both languages: Flux
`aggregateWindow(timeSrc: "_start", createEmpty: false)` and SQL `date_bin`
anchored at the Unix epoch. Missing min/max/mean fields produce an ERROR;
a different retained aggregate is not substituted to make a result pass.
Position uses @last when available, otherwise the catalog's available
aggregate; missing position at the exact raw minimum remains null.
Counts include only scalar paths present in Lume's catalog; position is counted
once via latitude/lat, not once for each coordinate.

**These native queries can disagree on the same feed.** Lume buckets discard
raw timestamps, repeated samples and some sources. Raw counts and range values
can therefore differ; a mean of bucket means differs from a sample-weighted
mean when buckets have unequal populations. The raw minimum's actual position
may differ from the position retained in its bucket. Partial boundary buckets
can include Lume aggregates derived from samples just outside the requested
range. Such discrepancies are MISMATCH results with details, not excluded
queries or benchmark successes. Matching aggregate/timestamp/count tolerances
is not proof of raw-sample equivalence.

## Output and timing

Every pair executes 20 **total** runs by default: one cold-first observation
and 19 warm observations. Engine order alternates between iterations.
Cold-first means the first timed request of that pair, **not** a flushed
server/OS cache; earlier discovery and other pairs can warm caches.

Measured latency includes HTTP transfer, response decoding, canonical result
ordering and any Lume pagination. It is an end-to-end client measurement, not
server CPU time. Influx queries the full range once. Lume's 500-row/64-KiB
envelope can require several HTTP queries: on truncation, ranges split only
at complete output-bin boundaries, and the original partial answer is discarded.
Counts are partitioned into batches of 100 paths. Unsplittable truncated bins
or count batches are ERROR; partial rows never masquerade as a complete answer.
JSON includes each run's HTTP request count, so transport overhead stays visible.
Timeout defaults to 60 seconds **per HTTP request**, configurable with `--timeout`.

Stdout contains a Markdown comparison table followed by a fenced JSON document.
The JSON has both query texts, frozen window/context/table, sampling notes,
each run's time/row count/request count or error, and each run's value check.
Warm p50/p95 use linear interpolation among successful warm observations;
failed observations remain in the report and make the pair ERROR.
The Markdown table shows first-run rows; JSON retains all runs' row counts.

Numbers default to absolute tolerance 0.001 and relative tolerance 1e-6.
`--abs-tol` and `--rel-tol` apply to numeric values in native units.
Counts are exact regardless of tolerance. Timestamp checks retain nanoseconds,
with zero tolerance by default; `--time-tol` explicitly allows seconds of
timestamp difference. NaN/infinite values fail checks. An empty answer from
both engines is EMPTY, not a claimed correctness pass. At most 20 individual
difference examples are recorded per run; row-count differences are always
reported. No result rows are capped for comparison.

Exit codes: 0 = all six pairs PASS; 1 = at least one MISMATCH/EMPTY/ERROR;
2 = setup/discovery failure. Dry run is offline, needs no credentials, prints
all six query pairs and returns 0. It uses an assumed schema and now-relative
bounds unless you supply from/to, and labels those assumptions explicitly.

## Local verification

```bash
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s bench -p test_influx_vs_lume.py -v
```

Tests use separate Influx/Lume mock HTTP servers on ephemeral **127.0.0.1**
ports. They cover all six pairs and headers, common-window discovery, explicit
windows, every warm-run check, Markdown/JSON output, mismatch/error/token
redaction, repeated annotated CSV tables, numeric/null/count/time checks
including nanoseconds, safe quoting and full-bin truncation splitting.
They verify client transport and generated query mappings; they do not execute
Flux in a real Influx server. Pi timings and live-feed agreement still need the
real endpoints and read token on the Pi.

Primary references checked for the query/API mapping:
[Influx query API](https://docs.influxdata.com/influxdb/v2/api/query/),
[annotated CSV](https://docs.influxdata.com/influxdb/v2/reference/syntax/annotated-csv/),
[aggregateWindow](https://docs.influxdata.com/flux/v0/stdlib/universe/aggregatewindow/),
[pivot](https://docs.influxdata.com/flux/v0/stdlib/universe/pivot/),
[Flux string escapes](https://docs.influxdata.com/flux/v0/spec/lexical-elements/).

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
fields (the Pi writer also tags self and s2_cell_id). Measurements/fields/context are explicitly filtered. Scalar sources
are merged after selecting one context. Lume uses its retained preferred-source
values; it does not retain independently queryable raw values for every source.

Run from the repository root, with the environment already configured:

```bash
python3 bench/influx_vs_lume.py --window 24h --runs 20
python3 bench/influx_vs_lume.py --context vessels.urn:mrn:signalk:uuid:0eb191d0-1f5a-42da-979e-ead792d676ee --window 6h
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

Discovery finds common first/latest depth or SOG coverage. With default
24h and no explicit bounds, select the latest available complete common
UTC hours, up to 24h, so Q2 has whole hours. If no full common hour exists,
use the short common interval and label it common_data_coverage_partial_hour.
Smaller --window durations also retain partial-hour coverage.

All requested/discovered bounds, including explicit --from/--to, snap
**outward** to the Lume bucket width: floor start, ceil stop (10s by default,
1s for telemetry_hr). Both databases use those same half-open bounds.
JSON preserves requested bounds and effective snapped bounds. Snapping can
expand a short/discovered interval to the edge of the newest bucket; pause
ingest or use completed historical bounds when requiring a stable snapshot.
Supplying both bounds skips discovery; one bound retains the other's
discovery/window behavior. Discovery is outside timing. Timestamps require
a timezone. The Pi vessel UUID above is supplied by the lead; multi-vessel/AIS
stores still require explicit --context and never silently select that boat.

## Query meanings

| Pair | Influx native query | Lume retained query |
|---|---|---|
| 1: depth range | Raw value/time samples | @last if retained, otherwise the catalog's mean/last/min/max preference |
| 2: hourly max depth | Maximum raw values per UTC hour | max(depth@max) per UTC hour |
| 3: minute mean SOG | Mean raw values per UTC minute | avg(SOG@mean) per UTC minute |
| 4: multi-condition | Minutes with mean SOG > X and minimum depth < Y | Same predicate over avg(@mean) and min(@min) |
| 5: minimum depth + position | Earliest raw minimum, nearest paired lat/lon sample within its bucket | Earliest minimum bucket, retained lat/lon from that bucket |
| 6: points per path | All raw scalar/position paths, plus distinct bucket counts | Populated buckets for all retained populated paths |

X defaults to 2 m/s, Y to 5 m; change `--sog-gt`/`--depth-lt`.
Depth defaults to `environment.depth.belowTransducer`; change `--depth-path`
if the boat uses belowKeel/belowSurface. `--sog-path` is also configurable.
No unit conversions are performed: both writers must contain the same native
Signal K units. `--table telemetry_hr` selects the configured 1-second store.

Aggregate timestamps use UTC bin starts in both languages: Flux
`aggregateWindow(timeSrc: "_start", createEmpty: false)` followed by
`date.truncate(_time, unit: 1h/1m)`, and SQL `date_bin` anchored at the
Unix epoch. This fixes Flux's clipped first-window start on partial hours/minutes. Missing min/max/mean fields produce an ERROR;
a different retained aggregate is not substituted to make a result pass.
Position uses @last when available, otherwise the catalog's available
aggregate. Flux filters position lat/lon, removes context/source/self/s2_cell_id
from the pivot key, pivots paired coordinates at each sample time, joins
positions to the minimum's bucket, and sorts by distance from the raw minimum
(ties use earliest position time). Missing positions in the bucket remain null;
the raw minimum is retained through a separate depth result even without a
position match. Q6 fetches all Influx paths and compares the union of observed
path sets. JSON lists influx_only, lume_only and symmetric_difference on every
run. Position is counted once via latitude/lat; repeated sources/samples in a
bucket collapse for the explicit distinct-bucket count.

**These native queries can disagree on the same feed.** Lume buckets discard
raw timestamps, repeated samples and some sources. Raw counts and range values
can therefore differ; a mean of bucket means differs from a sample-weighted
mean when buckets have unequal populations. The raw minimum's actual position
may differ from the position retained in its bucket. Outward snapping removes
partial-bucket boundary inclusion differences. Source/coordinate discrepancies
and unexplained errors still produce MISMATCH; they are not automatically excused.

FIDELITY is separate from PASS and MISMATCH, and requires bounded evidence:

- Q1: grouping raw samples into the store-width buckets and applying the
  retained last/mean/min/max must match Lume's bucket values and bucket count
  strictly. Then raw row-count differences and sample offsets < bucket width
  are classified FIDELITY.
- Q3, and matched Q4 rows' speed: mean differences exceeding normal tolerance
  but within --fidelity-rel (default 0.01, 1% of the larger magnitude) qualify.
  Predicate membership/timestamp sets and Q4 minimum depth remain strict.
- Q5: min-sample offset must be nonnegative, strictly less than bucket width,
  and in the exact same bucket. Minimum depth and both coordinates remain
  strict. --time-tol cannot excuse a minimum in a different bucket.
- Q6: each shared path's Influx distinct bucket count must equal Lume exactly,
  with raw count >= bucket count. Only then may the raw-vs-bucket count
  difference qualify. Missing paths/unmatched bucket counts remain MISMATCH.

Q2 maximum and Q5/Q4 minimum values never use the mean-fidelity allowance.
PASS still requires ordinary numeric tolerance (fixed-point quantization);
bucket counts are exact. Fidelity is not proof of raw-sample equivalence.
JSON reports max_observed_difference per run and the maxima across all runs
per query: numeric absolute/relative, time seconds and count differences,
plus Q1 sample offset/population and strict bucket-count difference.

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

Exit codes: 0 = all six pairs PASS or bounded FIDELITY; 1 = at least one MISMATCH/EMPTY/ERROR;
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
The 15-test suite includes the five reported Pi regressions and latest-full-hour
selection. It verifies client transport and generated query mappings; they do not execute
Flux in a real Influx server. The lead reported a real Influx 2.9.1/Pi run of the previous revision and its
alignment/position failures. This corrected revision still needs that real
Flux/Pi replay; local mocks do not prove server execution or performance.

Primary references checked for the query/API mapping:
[Influx query API](https://docs.influxdata.com/influxdb/v2/api/query/),
[annotated CSV](https://docs.influxdata.com/influxdb/v2/reference/syntax/annotated-csv/),
[aggregateWindow](https://docs.influxdata.com/flux/v0/stdlib/universe/aggregatewindow/),
[pivot](https://docs.influxdata.com/flux/v0/stdlib/universe/pivot/),
[Flux string escapes](https://docs.influxdata.com/flux/v0/spec/lexical-elements/).

#!/usr/bin/env bash
# Run on the Pi after deployment. Credentials come from PGPASSWORD / PGPASSFILE.
# Usage: bash tests/pg_smoke.sh host:port [username] [database]
# Twenty SQL cases (source-pinned Grafana/psql probes, expanded macros, typed values),
# plus psql's actual \d and a negative SCRAM check. No passwords in arguments/output.
set -euo pipefail
address="${1:?usage: pg_smoke.sh host:port [username] [database]}"
export PGHOST="${address%:*}" PGPORT="${address##*:}" PGUSER="${2:-grafana}" PGDATABASE="${3:-ti}" PGSSLMODE=disable PGCONNECT_TIMEOUT=10
psql -X -v ON_ERROR_STOP=1 -c '\d telemetry'
fixture="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)/golden/grafana-smoke.json"
python3 - "$fixture" <<'PY'
import json, subprocess, sys
with open(sys.argv[1], encoding="utf-8") as stream:
    queries = json.load(stream)["queries"]
assert len(queries) == 20, "Smoke fixture must have exactly twenty cases"
for number, query in enumerate(queries, 1):
    print("Smoke %02d/20: %s" % (number, query["id"]), flush=True)
    # Grafana expands $__timeFilter(ts) into BETWEEN and $__timeGroup(ts,'1m')
    # into floor(extract(epoch from ts)/60)*60. Use recent recorded data on the Pi.
    sql = query["sql"].replace("'2020-01-01T00:00:10Z'", "(now() - INTERVAL '24 hours')").replace("'2020-01-01T00:00:20Z'", "now()")
    subprocess.run(["psql", "-X", "-v", "ON_ERROR_STOP=1", "-c", sql], check=True)
PY
if failure="$(PGPASSWORD=lume-deliberately-invalid-smoke-password psql -X -v ON_ERROR_STOP=1 -c 'SELECT 42' 2>&1)"; then
    echo 'FAIL: invalid SCRAM password accepted' >&2
    exit 1
fi
if [[ "$failure" != *"SCRAM authentication failed"* ]]; then
    echo "FAIL: rejection did not report SCRAM authentication failure" >&2
    exit 1
fi
echo 'PASS: 20 SQL cases, psql metadata, typed timestamps/doubles and SCRAM accept/reject'

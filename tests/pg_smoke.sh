#!/usr/bin/env bash
# Run on the Pi after deployment. Credentials come from PGPASSWORD / PGPASSFILE.
# Usage: bash tests/pg_smoke.sh host:port [username] [database]
set -euo pipefail
address="${1:?usage: pg_smoke.sh host:port [username] [database]}"
export PGHOST="${address%:*}" PGPORT="${address##*:}" PGUSER="${2:-lume}" PGDATABASE="${3:-ti}" PGSSLMODE=disable PGCONNECT_TIMEOUT=10
psql -X -v ON_ERROR_STOP=1 -c '\d telemetry'
psql -X -v ON_ERROR_STOP=1 -c 'SELECT ts, "navigation.speedOverGround", count(*) OVER () AS n, true AS flag FROM telemetry LIMIT 1'
psql -X -v ON_ERROR_STOP=1 -c "SELECT current_setting('server_version_num')::int/100 AS version"
psql -X -v ON_ERROR_STOP=1 -c "SELECT extversion FROM pg_extension WHERE extname='timescaledb'"
psql -X -v ON_ERROR_STOP=1 -c "SELECT table_schema,table_name FROM information_schema.tables ORDER BY 1,2"
psql -X -v ON_ERROR_STOP=1 -c "SELECT column_name,data_type FROM information_schema.columns WHERE table_name='telemetry' ORDER BY ordinal_position"
# Replay the source-pinned client probes (Python stdlib only).
fixture="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)/golden/grafana-pg.json"
python3 - "$fixture" <<'PY'
import json, subprocess, sys
with open(sys.argv[1], encoding="utf-8") as stream:
    queries = json.load(stream)["queries"]
for query in queries:
    print("Probe:", query["id"], flush=True)
    subprocess.run(["psql", "-X", "-v", "ON_ERROR_STOP=1", "-c", query["sql"]], check=True)
PY
psql -X -v ON_ERROR_STOP=1 -c 'SELECT 42::BIGINT AS authenticated'
if failure="$(PGPASSWORD=lume-deliberately-invalid-smoke-password psql -X -v ON_ERROR_STOP=1 -c 'SELECT 42' 2>&1)"; then
    echo 'FAIL: invalid SCRAM password accepted' >&2
    exit 1
fi
if [[ "$failure" != *"SCRAM authentication failed"* ]]; then
    echo "FAIL: rejection did not report SCRAM authentication failure: $failure" >&2
    exit 1
fi
echo 'PASS: psql metadata, typed query and SCRAM accept/reject'

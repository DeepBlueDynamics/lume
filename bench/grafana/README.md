# Lume TI PostgreSQL datasource

The lead inspected the live HaLOS Pi: Signal K/Lume uses host networking;
Grafana shares the InfluxDB container's bridge network namespace. Its
halos.local host-gateway alias resolves to docker0, 172.17.0.1. Configure the
plugin with enablePg=true, pgPort=5864, pgUser=grafana, pgBind=172.17.0.1.
The plugin maps pgBind to --pg-bind; HTTP remains 127.0.0.1.

Copy lume-ti-datasource.yaml into Grafana's /etc/grafana/provisioning/datasources/
directory. Supply LUME_PG_PASSWORD in Grafana's environment through the
deployment's secret mechanism, with the same password set in the plugin
webapp PostgreSQL form. Only SCRAM verifiers are stored by the plugin.
Restart Grafana, select Lume TI and use Save & Test. No wildcard bind is needed.

The provisioning file's sslmode=disable is suitable only for local transport
over loopback (127.0.0.1) or the docker0 gateway (172.17.0.1). For any external
or non-loopback bind, sslmode=require is required (unless Lume is explicitly
configured with --pg-allow-plaintext). When TLS is active, Lume uses the
configured --pg-tls-cert/--pg-tls-key or auto-generates <store>/pg_cert.pem.
For host-networked Grafana, change the datasource URL to 127.0.0.1:5864 and
leave pgBind at 127.0.0.1. For the inspected Pi topology use halos.local:5864
and the specific docker0 bind above (sslmode disable is fine over docker0 172.17.0.1;
require elsewhere). Do not publish plaintext pgwire to the LAN/shore.

Run bash tests/pg_smoke.sh halos.local:5864 grafana ti with PGPASSWORD or
PGPASSFILE configured. It uses twenty source-pinned SQL cases plus actual
psql describe and incorrect-password rejection. Grafana macros are expanded
by Grafana, not by Lume; the tests replay their SQL expansions. The fixture
is tests/golden/grafana-smoke.json, a selected set from grafana-pg.json.

Local harness-only verification (stub psql, not database execution):
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s bench/grafana -p test_pg_smoke.py

Actual SQL, typed decoding and Node-verifier SCRAM acceptance are covered by:
CARGO_INCREMENTAL=0 cargo test --features ti --test ti_http

The live Pi datasource Save & Test and actual psql smoke must be checked after
deployment; loopback/stub tests do not establish that container connectivity.

## Importing the Agent Telemetry Dashboard

`lume-agents-dashboard.json` visualizes agent metrics and event logs ingested via OTLP (`POST /v1/metrics` and `POST /v1/logs`, D50) and queried over pgwire (`telemetry_agents` and `docs`):
- **Tokens Over Time**: MAX-MIN usage over the window using D50 dimensional paths (`"claude_code.token.usage@last"`).
- **Active Time**: tracks `claude_code.active_time` in seconds over time.
- **Active Buckets per Agent**: counts active 10-second telemetry buckets grouped by agent entity (`vessel AS entity`, `count(DISTINCT ts)`).
- **Recent Logbook Docs**: table of recent OTLP logbook records (`title`, `vessel AS entity`, `ts_start`) with interactive text-search filtering via `match(body, ${q:sqlstring})`.

### Import steps

#### Option A: Via Grafana Web UI
1. Ensure the `Lume TI` datasource (`uid: lume-ti`) is provisioned and tested (Save & Test).
2. Open Grafana in your browser (e.g. `http://halos.local:3000` or local port).
3. Navigate to **Dashboards** → **New** → **Import** (or browse to `/dashboard/import`).
4. Click **Upload dashboard JSON file** and select `bench/grafana/lume-agents-dashboard.json`.
5. Select the **Lume TI** datasource for any datasource prompt, then click **Import**.
6. Use the top toolbar `$q` text box to filter logbook events by body content (e.g. `service.rs`, `edit`, `tool_call`).

#### Option B: Via Provisioning Directory
Copy the dashboard file into Grafana's dashboard provisioning tree:
```sh
cp bench/grafana/lume-agents-dashboard.json /var/lib/grafana/dashboards/
```
Or create a dashboard provider configuration in `/etc/grafana/provisioning/dashboards/lume.yaml`:
```yaml
apiVersion: 1
providers:
  - name: 'Lume Dashboards'
    orgId: 1
    folder: 'Lume'
    type: file
    disableDeletion: false
    editable: true
    options:
      path: /var/lib/grafana/dashboards
```
Restart Grafana to load the dashboard automatically.

### Running dashboard tests
- Static structure, datasource, and credential sanity checks:
  ```sh
  py -3 -m unittest bench/grafana/test_agents_dashboard.py
  ```
- Live SQL query execution against OTLP ingest:
  ```sh
  py -3 -m unittest bench/grafana/test_agents_dashboard_sql.py
  ```
- Discover all:
  ```sh
  py -3 -m unittest discover -s bench/grafana -p 'test_*.py'
  ```

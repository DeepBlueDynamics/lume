# signalk-lume-ti

Signal K server plugin and web application for **Lume TI** — high-resolution timeseries store and query engine.

Supervises `lume ti ingest --serve` as a managed child process inside the Signal K environment, providing continuous live telemetry recording, WAL crash safety, automated shard sealing, and an in-browser SQL query console.

---

## Architecture

- **Supervision**: Runs `lume ti ingest --signalk ws://127.0.0.1:3000 --store <dataDir>/lume-ti --serve --bind 127.0.0.1 --port 5863` as a child process. Automatically restarts on crash with exponential backoff and gracefully shuts down via `SIGTERM` (which flushes in-memory WAL buffers and bucket accumulators before exit).
- **Storage**: Columnar parquet-backed TI store located under `/home/node/.signalk/lume-ti` (via `app.getDataDirPath()`).
- **Security & Loopback Proxy**: The query server binds `127.0.0.1` only. The browser communicates exclusively via the Signal K plugin router (`/plugins/signalk-lume-ti/api/query`, `/api/schema`, `/api/status`), avoiding open network ports or CORS exposure.
- **Authentication**: Supports Signal K device access-request flow (`POST /signalk/v1/access/requests`, poll until approved) with persistent token storage in `<dataDir>/token.txt`. If Signal K runs with anonymous read-only access enabled (`readOnlyAccess: true`, standard in local marine setups), ingest connects immediately without requiring a token.
- **Webapp**: Full SQL console, preset queries, CSV export, and live schema browser accessible via Signal K Webapps menu (`/signalk-lume-ti/`).

---

## Target Environment & Libc Compatibility

- **Device**: Raspberry Pi 5 (8 GB) running HaLOS (Halos-Marine-RPI).
- **Signal K Container**: `signalk-server-docker` (v2.31.1).
  - OS: Ubuntu 24.04.4 LTS (Noble Numbat)
  - Architecture: `aarch64` / `arm64`
  - C Library: `glibc 2.39`
  - Node.js runtime: Node v18+ / v20+
- **Host**: Debian 13 (Trixie), `glibc 2.41`, `aarch64`.
- **Libc Compatibility**: Because the container runs standard Ubuntu 24.04 with `glibc 2.39`, a musl static build is **not required**. Any `aarch64-unknown-linux-gnu` binary compiled on Debian/Ubuntu with `GLIBC <= 2.39` will run directly inside the container.

---

## Installation into Signal K Docker Container

Signal K server plugins and webapps are installed inside the Signal K data directory, which is host-mounted at `/home/node/.signalk`:

### 1. Install Plugin Package
Inside the container (or on the host at the mounted volume directory `/home/node/.signalk`):

```bash
cd /home/node/.signalk
npm install /path/to/signalk-lume-ti
```

Or copy the plugin directory into `node_modules`:
```bash
cp -r /path/to/plugins/signalk-lume-ti /home/node/.signalk/node_modules/
```

### 2. Install Lume Binary
Place the native `aarch64` `lume` binary in either:

1. **Plugin bundled location**:
   ```bash
   mkdir -p /home/node/.signalk/node_modules/signalk-lume-ti/bin/linux-arm64
   cp /path/to/lume /home/node/.signalk/node_modules/signalk-lume-ti/bin/linux-arm64/lume
   chmod +x /home/node/.signalk/node_modules/signalk-lume-ti/bin/linux-arm64/lume
   ```

2. **Or system PATH inside the container**:
   ```bash
   cp /path/to/lume /usr/local/bin/lume
   chmod +x /usr/local/bin/lume
   ```

3. **Or custom path**: Set `lumePath` in the plugin configuration UI.

### 3. Enable in Admin UI
1. Navigate to Signal K Server Admin UI: `http://<pi-ip>:3000/admin/`.
2. Go to **Server** -> **Plugin Config** -> **Lume TI**.
3. Toggle the plugin to **Enabled**.
4. Configure optional settings:
   - **Signal K WebSocket URL**: `ws://127.0.0.1:3000` (default)
   - **Query Server Port**: `5863` (default)
   - **Custom binary path**: leave empty to auto-detect bundled binary or PATH
5. Click **Submit**.

---

## Verification & Testing

Run unit tests via Node's native test runner:

```bash
cd plugins/signalk-lume-ti
npm test
```

Test coverage includes:
- Process supervision and argument construction (`--signalk`, `--store`, `--serve`, `--port`, `--bind`, `--token`).
- Automatic crash recovery with exponential backoff.
- Clean shutdown via `SIGTERM` triggering WAL synchronization.
- Signal K device access-request flow and polling.
- Status API and query/schema HTTP proxying.

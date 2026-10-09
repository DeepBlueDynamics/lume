# 9. Install and fleet topology

Spec pp. 19–20. Owners: install [W7](../lanes/W7-serve.md); fleet [W8](../lanes/W8-sync-bench.md).

## Install on a HALPI2 (HaLOS)

Lume TI ships as a container app in the HaLOS Marine store, next to Signal K,
Grafana and InfluxDB — not a process spawned from inside the Signal K container.

- **Package.** `lume-ti-container`, built with Hat Labs' container-packaging-tools as an apt package. Goes upstream to halos-marine-containers, or a DeepBlue store definition until accepted.
- **Image.** Multi-arch OCI (arm64, amd64) holding the static `lume` binary. Store on a named volume on the HALPI's NVMe.
- **Signal K.** Joins the Signal K container's network, subscribes over WebSocket. Token from an access request approved once in the Signal K admin UI. The thin Signal K plugin is optional here; it adds the webapp inside Signal K.
- **Routing.** HTTP and webapp behind HaLOS's Traefik + Authelia SSO, with a Homarr tile. Postgres wire on LAN port 5432 with SCRAM.
- **Grafana.** Provisions a PostgreSQL data source pointing at Lume TI, plus a starter energy dashboard from the PV-1 saved queries.
- **Backfill.** Config form offers a one-time import from the bundled InfluxDB container; live ingest then takes over.

Steps:

1. Cockpit → Container Apps → Marine store → install Lume TI.
2. Approve its access request in Signal K.
3. Optionally run the InfluxDB backfill from the app's config form.
4. Open Lume TI from Homarr, or query it from Grafana or psql.

## Install on an OpenPlotter Pi

1. In Signal K Admin, App Store → install `signalk-lume-ti`, restart.
2. Approve the plugin's access request under Security → Access Requests.
3. Pick a store path in Plugin Config. USB or NVMe SSD recommended; plugin warns if the store is on the SD card.
4. If signalk-parquet is installed, click Backfill.
5. Open the webapp, or `psql -h <pi> -p 5432 -U ti`.

## Relationship to OpenCPN

- OpenCPN keeps its existing Signal K or NMEA connection. Lume TI subscribes separately and never transmits on NMEA 2000.
- In v1, OpenCPN shows no Lume TI results. Whether OpenCPN can display Signal K resources (notes, regions) is open; if not, a small OpenCPN plugin is a v2 option.

## Fleet topology

- Each vessel runs `lume ti ingest` plus `serve`. Sealed shards are immutable and content-hashed — the unit of replication.
- `lume ti sync` diffs local manifest against shore and ships missing shard versions. Resumes chunked uploads; tolerates Starlink and cellular drops.
- The open shard ships as WAL tail segments every 5 min when online, so shore lag ≤ 5 min.
- The shore node mounts all vessels' shards. Its `telemetry` table spans every vessel ordinal; vessel ordinals are re-mapped by URN at import.

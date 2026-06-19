# Lume · Vector Field (live 3D search-dynamics visualizer)

Watch Lume's **phase-binding + Weber relaxation** play out in 3D: the query and
its top-K retrieved candidates as a force field, where you can *see the
acceleration* of each candidate toward (or away from) the query as the dynamics
settle — the second-order signal that static cosine layouts (UMAP/t-SNE) throw
away.

```
 lume stream <query>  ──NDJSON──▶  server.js (bridge)  ──WebSocket──▶  React + three.js
   (Rust: phase/Weber                spawns lume,                       points + acceleration
    relaxation, PCA→3D)              relays frames                      arrows, clusters, R-meter
```

## What you see

- **Spheres** = the query (white, large) and candidate passages. Candidate size +
  glow scale with cosine-to-query.
- **Colors** = emergent phase clusters (assemblies that synchronize during the run).
- **Acceleration arrows** = each candidate's 3D acceleration this step. **Green =
  accelerating toward the query, red = away.** Crank the *acc arrow scale* slider.
- **R meter** = global Kuramoto phase coherence; it climbs as assemblies bind.
- **Scrubber** = replay/scrub the relaxation; play/pause and speed on the bottom bar.

## Prerequisites

- The `lume` release binary built: from the repo root, `cargo build --release`.
- A reachable **shivvr** endpoint (used read-only to embed the query + candidates).
  Defaults to `http://localhost:8085`.
- An index with content, e.g. the eval index used in the demo:
  `lume index --db .lume-eval-index --tag-dict eval-tmp/dict/character.csv eval-tmp/corpus`

## Run

```bash
cd viz
npm install

# Production: build the app, then serve it + the bridge on one port
npm run build
npm run bridge          # http://localhost:8086

# OR dev mode (hot reload): two terminals
npm run bridge          # terminal A — WebSocket + lume spawner on :8086
npm run dev             # terminal B — Vite UI on :5173, talks to the bridge
```

Open the page, type a query, pick the `--db`, hit **Search**. The bridge runs
`lume stream` and streams frames in.

## Config

- `PORT` (bridge HTTP/WS port, default `8086`).
- `LUME_BIN` (path to the lume binary; defaults to `../target/release/lume[.exe]`).

The bridge runs `lume` with the repo root as CWD, so `--db` paths are relative to
the repo root (e.g. `.lume-eval-index`).

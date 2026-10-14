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

- A `lume` binary: a repo build (`cargo build --release`) if there is one, otherwise
  the installed copy (`npm install -g @deepbluedynamics/lume`). The page shows which
  version the bridge runs.
- A reachable **shivvr** endpoint (used read-only to embed the query + candidates).
  Defaults to `http://localhost:8085`; see [Shivvr setup](../README.md#shivvr-setup-semantic-search).
- An index with content. The page defaults to the Monte Cristo demo; build it once from
  the repo root with `lume index docs/monte_cristo --db docs/monte_cristo/.lume-index`.
  Under the `--db` box the page says whether the index exists, its format and when it
  was built.
- For **Ask**: [Ollama](https://ollama.com/) at `http://localhost:11434`. Pick the model
  from the dropdown, which lists your Ollama models under **Cloud** (ollama.com) and
  **Local**. It defaults to `glm-5.3:cloud` and remembers your choice. Cloud models
  answer in seconds; large local models can take minutes.

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

- `PORT` (bridge HTTP/WS port, default `8086`) and `HOST` (default `127.0.0.1`).
- `LUME_BIN` (path to the lume binary; defaults to `../target/release/lume[.exe]`,
  then the global npm install, then `lume` on PATH).
- `OLLAMA_URL` (default `http://localhost:11434`), used for the model list and passed
  to `lume answer`.

The bridge runs `lume` with the repo root as CWD, so `--db` paths are relative to
the repo root (e.g. `docs/monte_cristo/.lume-index`). It only accepts requests from
pages on this machine, so another website open in the browser can't drive it.

---
title: "How Lume Works, Part 2: The Visualization Field"
date: 2026-06-19
series: "How Lume Works"
part: 2
tags: [search, visualization, rust, three.js, kuramoto, weber, physics-simulation, websocket]
description: >
  A deep dive into Lume's visualization engine — projecting 768-D vectors into 3D,
  running a Weber-style relaxation and Kuramoto phase-binding simulation in Rust,
  relaying NDJSON frames over WebSockets, and rendering a hyperspace warp-in using
  Three.js.
---

# How Lume Works, Part 2: The Visualization Field

In [Part 1: The Retrieval Primitives](./how-lume-works-part1-primitives.md), we went inside Lume's retrieval core to see how BM25, dense embeddings, and entity graphs fuse into a single ranked list. Ranking is only half the story.

Lume renders this ranked list as a living 3D vector field. This is Part 2: the visualization. We'll show how a Rust core simulates multi-query vector-space relaxation, streams frames over WebSockets, and drives a Three.js front end where results don't just load — they warp in from hyperspace.

> **Stack at a glance.** A Rust core runs the spatial relaxation simulation and streams frames as **NDJSON** on stdout (`lume stream`, `lume answer`). A Node **WebSocket bridge** (`viz/server.js:91`) spawns this binary and relays each frame to the client. A **React + Three.js** front end (`viz/src/App.jsx:178`) interpolates frames and drives a custom useFrame warp-in animation (per-frame transform, no GLSL) (`viz/src/VectorField.jsx:84`).

---

## 1. Mapping Candidates to 3D Coordinates

Lume does not use pre-computed layout coordinates or static snapshots. The candidate nodes we visualize are generated dynamically by the retrieval union. 

Each surviving search hit is represented as a `Candidate` struct (`Candidate` in `src/stream.rs:43`). The stream layer receives the union produced by retrieval (`retrieve_union` in `src/main.rs:1704`): section id, score, and the query indices that surfaced it. The renderer uses that set directly; the dynamic part is position, phase, cluster id, and visual layout.

---

## 2. The Layout is a Simulation, Not a t-SNE Snapshot

Static projection methods throw away the *dynamics* of how results relate to a query. Lume preserves some of that motion. The module `src/stream.rs:150` runs a **phase-binding + Weber relaxation** simulation, emitting one frame per simulation step.

### 2a. Single-Basis PCA Projection
All candidates and queries are projected into 3D using a top-3 **PCA basis** fitted via power iteration (`Pca::fit` in `src/stream.rs:97`). By sharing a single projection space, multi-query overlap orbs land in the exact same physical coordinates for all queries, rather than requiring reconciled separate coordinates.

### 2b. Weber Coupling
For each non-query node against every other node, Lume calculates relative vector distances and their finite differences, then computes a **Weber-style** coupling term. This term modulates the phase coupling and vector warp:

```rust
// src/stream.rs:339
let term1 = (rdot * rdot) / (2.0 * params.c_weber * params.c_weber);
let term2 = (d * rddot) / (params.c_weber * params.c_weber);
let b_ij = (1.0 - term1 + term2).clamp(-2.0, 2.0);

// Gated by phase alignment and a semantic Gaussian:
let sem = distance(&vi, &snapshot[j]);
let s_ij = (-(sem * sem) / (params.sigma_v * params.sigma_v)).exp();
couplings[i][j] = params.k0 * wij * s_ij * b_ij;
```

### 2c. Kuramoto Phase Synchronization
To make semantic clusters visually bundle together, each node carries an oscillator phase `θ`. The coupling force drives a Kuramoto update:

```rust
// src/stream.rs:369
let mut torque = 0.0;
for j in 0..m {
    if i != j {
        torque += couplings[i][j] * (thetas[j] - thetas[i]).sin();
    }
}
let xi = (rng.next_u64() as f64 / u64::MAX as f64 - 0.5) * 2.0;
next_theta[i] = thetas[i] + (nodes[i].omega + torque) * DT + params.noise * DT.sqrt() * xi;
```

The global **order parameter** `r_global` (the "phase coherence" bar in the UI) tracks the overall synchronization of non-query nodes:

```rust
// src/stream.rs:408
let r_global = if cnt > 0.0 { ((cs / cnt).powi(2) + (sn / cnt).powi(2)).sqrt() } else { 0.0 };
```

### 2d. Approach Acceleration
A node's radius from its query anchor is a function of cosine similarity (`radius = 0.7 + 3.0 * (1.0 - cos)`, `src/stream.rs:264`). To expose the motion, Lume computes **approach acceleration** (`approach_acc` in `src/stream.rs:397`) as the second finite difference of cosine distance to the anchor:

```rust
// src/stream.rs:396
let nav = (dq - nodes[i].dq) / DT;
nodes[i].approach_acc = (nav - nodes[i].approach_vel) / DT;
nodes[i].approach_vel = nav;
nodes[i].dq = dq;
```

---

## 3. The WebSocket Bridge: Spawn, Stream, and Relay

Since browsers cannot directly spawn native binaries or read child-process stdout, `viz/server.js` acts as the bridge. On receiving a WebSocket request, it spawns `lume` in a child process, forwards stdout JSON lines verbatim, and keeps stderr for status/error messages:

```js
// viz/server.js:91
ws.send(JSON.stringify({ type: "status", state: "running", bin: LUME_BIN, args }));
child = spawn(LUME_BIN, args, { cwd: join(__dirname, "..") });

const rl = createInterface({ input: child.stdout });
rl.on("line", (line) => {
  const t = line.trim();
  if (t.startsWith("{") && ws.readyState === ws.OPEN) ws.send(t); // forward frame verbatim
});
```

Because a new process is spawned per request, rebuilding the Rust core takes effect immediately on the next search without requiring a bridge server restart.

---

## 4. Visualizing the Answering Agent's Orchestration

When you ask a question (`lume answer` in `src/main.rs:1740`), the binary runs an agentic loop: **Plan → Retrieve → Evaluate → Refine → Answer**.

Rather than hiding this workflow behind a loading spinner, Lume streams the state changes directly into the spatial field:

1. **`type: "plan"`**: The planner's generated search queries are shown in the control panel.
2. **`type: "frame"`**: The results retrieved for these queries warp into the field.
3. **`type: "evaluate"`**: The model's evaluation notes (indicating whether the retrieved passages are sufficient) appear as status text.
4. **`type: "answer"`**: The final inline-cited answer is displayed over the final field. The citations are parsed via `parse_citations` in `src/answer.rs:107`, causing the cited source orbs in the 3D field to light up as provenance anchors.

---

## 5. Front-End Rendering: Interpolate, Lay Out, and Warp

The browser stores streamed frames in a React ref, then uses its own animation clock to play them smoothly.

### 5a. Frame Interpolation
To achieve sub-frame fluid motion, each animation tick interpolates selected node fields between successive frames:

```jsx
// viz/src/App.jsx:178
const nodes = frames.length ? layout(interpolate(meta, frames[lo], frames[hi], frac), spread) : [];
```

The `interpolate` function (`viz/src/App.jsx:60`) linearly interpolates `pos`, `acc`, `cos_q`, and `approach_acc`. It does not interpolate velocity or phase; those are either unused by the renderer or represented through the interpolated position/acceleration visuals.

### 5b. Collision Separation
To prevent overlapping results, the `layout` function (`viz/src/App.jsx:12`) applies iterative 3D sphere separation after interpolation and optional cluster spread. It pads each render radius by 25% to leave room for stretched orb visuals, resolving intersections in up to 26 iterations.

### 5c. The Hyperspace Warp-In
Every new result set triggers a warp entrance animation. Nodes start far out along their radial lines from the center, stretched thin along their axis of travel, and decelerate hard into place:

```jsx
// viz/src/VectorField.jsx:84
useFrame(({ clock }) => {
  const g = grpRef.current;
  if (!g) return;
  const t0 = warpRef.current.t0;
  let p = t0 <= -100 ? 1 : (clock.elapsedTime - t0 - warpIn.delay) / WARP_DUR;
  p = p < 0 ? 0 : p > 1 ? 1 : p;
  const k = 1 - easeOutExpo(p); // 1 → 0 over the jump
  g.position.set(warpIn.off[0] * k, warpIn.off[1] * k, warpIn.off[2] * k);
  if (p < 1 && meshRef.current) {
    // Streak: stretch along the radial travel axis, thinning out the sides
    meshRef.current.quaternion.copy(warpIn.quat);
    meshRef.current.scale.set(1 / (1 + 2.2 * k), 1 + 7 * k, 1 / (1 + 2.2 * k));
  }
});
```

A staggered delay is computed using `hash01(node.id) * WARP_STAGGER` (`WARP_STAGGER = 0.30` in `viz/src/VectorField.jsx:18`) to create a cascade effect, and the stretch is applied mesh-locally so layout logic remains uncorrupted.

---

## Why Visual Search Matters

* **Spatial Semantics:** The layout relaxation puts related documents near their query anchors and exposes cluster membership frame by frame.
* **Auditable Answering:** The source orbs that light up on the screen are the passages fed to the answerer or cited by it. Visual citations act as a provenance log.
* **Responsive Interactions:** NDJSON streaming combined with an independent animation clock ensures the layout begins animating immediately, without waiting for the full simulation to complete.

Cue *Derezzed* and watch it jump to hyperspace. 🌌

---

*Line numbers are against the current tree and drift as code moves — grep the symbol names (`stream::run`, `handle_answer`, `plan_queries`, `parse_citations`) if they've shifted.*

`#Rust` · `#ThreeJS` · `#VectorSearch` · `#RAG` · `#AgenticAI` · `#Kuramoto` · `#WeberRelaxation`

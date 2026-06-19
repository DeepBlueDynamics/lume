import React, { useEffect, useRef, useState, useCallback } from "react";
import VectorField from "./VectorField.jsx";

const WS_URL = `ws://${location.hostname}:8086`;

// Assign a render radius per node (query fixed + modest; candidates scale with
// retrieval weight) then run a position-based collision-separation: iteratively
// push overlapping orbs apart along their centre line until none intersect.
// Queries are immovable; candidate–candidate overlaps split the push. Mutates in
// place on the per-frame node objects.
function layout(nodes) {
  if (!nodes.length) return nodes;
  let lo = Infinity, hi = -Infinity;
  for (const n of nodes) if (!n.is_query) { lo = Math.min(lo, n.score); hi = Math.max(hi, n.score); }
  const span = hi - lo || 1;
  for (const n of nodes) n.r = n.is_query ? 0.3 : 0.15 + 0.4 * ((n.score - lo) / span);
  const margin = 0.06;
  for (let iter = 0; iter < 12; iter++) {
    for (let i = 0; i < nodes.length; i++) {
      for (let j = i + 1; j < nodes.length; j++) {
        const a = nodes[i], b = nodes[j];
        const dx = b.pos[0] - a.pos[0], dy = b.pos[1] - a.pos[1], dz = b.pos[2] - a.pos[2];
        const d = Math.hypot(dx, dy, dz) || 1e-4;
        const min = a.r + b.r + margin;
        if (d < min) {
          const push = min - d, ux = dx / d, uy = dy / d, uz = dz / d;
          const aw = a.is_query ? 0 : b.is_query ? 1 : 0.5;
          const bw = b.is_query ? 0 : a.is_query ? 1 : 0.5;
          a.pos = [a.pos[0] - ux * push * aw, a.pos[1] - uy * push * aw, a.pos[2] - uz * push * aw];
          b.pos = [b.pos[0] + ux * push * bw, b.pos[1] + uy * push * bw, b.pos[2] + uz * push * bw];
        }
      }
    }
  }
  return nodes;
}

function interpolate(meta, a, b, t) {
  if (!a) return [];
  const lerp3 = (p, q) => [p[0] + (q[0] - p[0]) * t, p[1] + (q[1] - p[1]) * t, p[2] + (q[2] - p[2]) * t];
  return a.nodes.map((na, i) => {
    const nb = (b && b.nodes[i]) || na;
    const mn = meta?.nodes?.[i] || {};
    return {
      id: na.id,
      label: mn.label,
      score: mn.score ?? 0,
      text: mn.text || "",
      is_query: na.is_query,
      query_index: na.query_index ?? 0,
      members: mn.members || na.members || [],
      pos: lerp3(na.pos, nb.pos),
      acc: lerp3(na.acc, nb.acc),
      cos_q: na.cos_q + (nb.cos_q - na.cos_q) * t,
      approach_acc: na.approach_acc + (nb.approach_acc - na.approach_acc) * t,
      cluster: nb.cluster,
    };
  });
}

export default function App() {
  const [input, setInput] = useState("Dantès in prison at the Château d'If");
  const [queries, setQueries] = useState([]);
  const [db, setDb] = useState(".lume-eval-index");
  const [candidates, setCandidates] = useState(20);
  const [steps, setSteps] = useState(160);

  const [meta, setMeta] = useState(null);
  const framesRef = useRef([]);
  const [frameCount, setFrameCount] = useState(0);
  const [idx, setIdx] = useState(0);
  const [playing, setPlaying] = useState(true);
  const [speed, setSpeed] = useState(1.2);
  const [accScale, setAccScale] = useState(120);
  const [warp, setWarp] = useState(14);
  const [conn, setConn] = useState("connecting");
  const [status, setStatus] = useState("");

  const wsRef = useRef(null);

  useEffect(() => {
    const ws = new WebSocket(WS_URL);
    wsRef.current = ws;
    ws.onopen = () => setConn("connected");
    ws.onclose = () => setConn("disconnected");
    ws.onerror = () => setConn("error");
    ws.onmessage = (ev) => {
      const m = JSON.parse(ev.data);
      if (m.type === "meta") {
        setMeta(m);
        framesRef.current = []; setFrameCount(0); setIdx(0); setPlaying(true);
        const nq = m.nodes.filter((n) => n.is_query).length;
        setStatus(`running · ${nq} quer${nq === 1 ? "y" : "ies"} · ${m.nodes.length - nq} candidates`);
      } else if (m.type === "frame") {
        framesRef.current.push(m); setFrameCount(framesRef.current.length);
      } else if (m.type === "status") {
        if (m.state === "closed" && m.code !== 0) setStatus(`lume exited ${m.code}: ${(m.stderr || "").split("\n").pop()}`);
        else if (m.state === "running") setStatus("running…");
        else if (m.state === "error") setStatus(`bridge error: ${m.message}`);
        else if (m.state === "closed") setStatus(`done · ${framesRef.current.length} frames`);
      } else if (m.type === "done") {
        setStatus(`done · ${framesRef.current.length} frames`);
      }
    };
    return () => ws.close();
  }, []);

  useEffect(() => {
    let raf, last = performance.now();
    const tick = (now) => {
      const dt = (now - last) / 1000; last = now;
      if (playing && framesRef.current.length > 1) {
        setIdx((p) => Math.min(framesRef.current.length - 1, p + dt * speed * 24));
      }
      raf = requestAnimationFrame(tick);
    };
    raf = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(raf);
  }, [playing, speed]);

  const runSearch = useCallback((qs) => {
    const ws = wsRef.current;
    const list = qs.filter(Boolean);
    if (!list.length) return;
    if (!ws || ws.readyState !== ws.OPEN) { setStatus("not connected to bridge"); return; }
    framesRef.current = []; setFrameCount(0); setIdx(0); setMeta(null);
    setQueries(list);
    ws.send(JSON.stringify({ type: "search", queries: list, db, candidates: Number(candidates), steps: Number(steps) }));
  }, [db, candidates, steps]);

  const search = () => runSearch([input]);                       // fresh search
  const addSearch = () => runSearch([...queries, input]);        // additive: union into the field

  const frames = framesRef.current;
  const lo = Math.floor(idx), hi = Math.min(frames.length - 1, lo + 1), frac = idx - lo;
  const nodes = frames.length ? layout(interpolate(meta, frames[lo], frames[hi], frac)) : [];
  const rGlobal = frames.length ? frames[Math.round(idx)].r_global : 0;

  return (
    <div className="app">
      <div className="canvas-wrap">
        <VectorField nodes={nodes} accScale={accScale} warp={warp} queryCount={queries.length || 1} />
      </div>

      <div className="panel">
        <h1>LUME · Vector Field</h1>
        <p className="sub">phase-binding + Weber relaxation · additive search</p>

        <div className="row">
          <input type="text" value={input} onChange={(e) => setInput(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && (e.ctrlKey ? addSearch() : search())} placeholder="search query" />
        </div>
        <div className="row">
          <input type="text" value={db} onChange={(e) => setDb(e.target.value)} placeholder="--db" />
          <input className="small" type="number" value={candidates} min={4} max={48}
            onChange={(e) => setCandidates(e.target.value)} title="candidates/query" />
          <input className="small" type="number" value={steps} min={20} max={600}
            onChange={(e) => setSteps(e.target.value)} title="steps" />
        </div>
        <div className="row">
          <button onClick={search} disabled={conn !== "connected"} style={{ flex: 1 }}>▶ Search</button>
          <button className="ghost" onClick={addSearch} disabled={conn !== "connected" || !queries.length}>＋ Add</button>
        </div>

        {queries.length > 0 && (
          <div className="chips">
            {queries.map((q, i) => (
              <span key={i} className="chip" style={{ borderColor: QCHIP[i % QCHIP.length] }}>
                <i style={{ background: QCHIP[i % QCHIP.length] }} />{q.length > 26 ? q.slice(0, 25) + "…" : q}
              </span>
            ))}
          </div>
        )}

        <div className="stat" style={{ marginTop: 8 }}><span>phase coherence (R)</span><b>{rGlobal.toFixed(3)}</b></div>
        <div className="bar"><i style={{ width: `${rGlobal * 100}%` }} /></div>
        <div className="stat"><span>frame</span><b>{frames.length ? `${Math.round(idx)} / ${frameCount - 1}` : "—"}</b></div>

        <div className="stat" style={{ marginTop: 8 }}><span>acc arrow scale</span><b>{accScale}</b></div>
        <input type="range" min={10} max={400} value={accScale} style={{ width: "100%" }}
          onChange={(e) => setAccScale(Number(e.target.value))} />
        <div className="stat" style={{ marginTop: 4 }}><span>orb warp</span><b>{warp}</b></div>
        <input type="range" min={0} max={60} value={warp} style={{ width: "100%" }}
          onChange={(e) => setWarp(Number(e.target.value))} />

        <div className="legend">
          <div>labels = retrieval <b>weight</b> · <b>hover</b> for the passage</div>
          <div><span className="dot" style={{ background: "#ffd23b" }} /> haloed = <b>overlap</b> (found by 2+ queries)</div>
          <div>orbs cluster around their query · nearer = more relevant</div>
          <div>Enter = search · Ctrl-Enter = add · Ctrl-drag = pan · scroll = zoom</div>
        </div>
      </div>

      <div className="controls">
        <button onClick={() => setPlaying((p) => !p)}>{playing ? "❚❚" : "▶"}</button>
        <input type="range" min={0} max={Math.max(0, frameCount - 1)} step="0.01" value={idx}
          onChange={(e) => { setPlaying(false); setIdx(Number(e.target.value)); }} />
        <span style={{ fontSize: 11, color: "#7b829a" }}>×</span>
        <input type="range" min={0.2} max={4} step={0.1} value={speed} style={{ width: 90 }}
          onChange={(e) => setSpeed(Number(e.target.value))} />
      </div>

      <div className="status">
        bridge: <span className={conn === "connected" ? "ok" : "err"}>{conn}</span>
        {status ? ` · ${status}` : ""}
      </div>
    </div>
  );
}

// Query chip colors, matched to the per-query hues in VectorField.
const QCHIP = ["#3fa9ff", "#ff9d3b", "#e06bff", "#36d399", "#ffd23b"];

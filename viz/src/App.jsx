import React, { useEffect, useRef, useState, useCallback } from "react";
import VectorField from "./VectorField.jsx";

const WS_URL = `ws://${location.hostname}:8086`;

// Linear interpolation between two streamed frames so playback is smooth
// regardless of how fast `lume stream` dumps steps. Positions/accelerations are
// lerped; discrete fields (cluster, labels) come from the later frame.
function interpolate(meta, a, b, t) {
  if (!a) return [];
  const lerp3 = (p, q) => [p[0] + (q[0] - p[0]) * t, p[1] + (q[1] - p[1]) * t, p[2] + (q[2] - p[2]) * t];
  return a.nodes.map((na, i) => {
    const nb = (b && b.nodes[i]) || na;
    const label = meta?.nodes?.[i]?.label;
    return {
      id: na.id,
      label,
      is_query: na.is_query,
      pos: lerp3(na.pos, nb.pos),
      acc: lerp3(na.acc, nb.acc),
      cos_q: na.cos_q + (nb.cos_q - na.cos_q) * t,
      approach_acc: na.approach_acc + (nb.approach_acc - na.approach_acc) * t,
      cluster: nb.cluster,
    };
  });
}

export default function App() {
  const [query, setQuery] = useState("Dantès escapes the prison and finds the treasure");
  const [db, setDb] = useState(".lume-eval-index");
  const [candidates, setCandidates] = useState(24);
  const [steps, setSteps] = useState(160);

  const [meta, setMeta] = useState(null);
  const framesRef = useRef([]);
  const [frameCount, setFrameCount] = useState(0);
  const [idx, setIdx] = useState(0);
  const [playing, setPlaying] = useState(true);
  const [speed, setSpeed] = useState(1.2);
  const [accScale, setAccScale] = useState(120);
  const [showLabels, setShowLabels] = useState(true);
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
        framesRef.current = [];
        setFrameCount(0);
        setIdx(0);
        setPlaying(true);
        setStatus(`running · ${m.nodes.length - 1} candidates`);
      } else if (m.type === "frame") {
        framesRef.current.push(m);
        setFrameCount(framesRef.current.length);
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

  // Playback loop.
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

  const search = useCallback(() => {
    const ws = wsRef.current;
    if (!ws || ws.readyState !== ws.OPEN) { setStatus("not connected to bridge"); return; }
    framesRef.current = []; setFrameCount(0); setIdx(0); setMeta(null);
    ws.send(JSON.stringify({ type: "search", query, db, candidates: Number(candidates), steps: Number(steps) }));
  }, [query, db, candidates, steps]);

  const frames = framesRef.current;
  const lo = Math.floor(idx), hi = Math.min(frames.length - 1, lo + 1), frac = idx - lo;
  const nodes = frames.length ? interpolate(meta, frames[lo], frames[hi], frac) : [];
  const rGlobal = frames.length ? frames[Math.round(idx)].r_global : 0;

  return (
    <div className="app">
      <div className="canvas-wrap">
        <VectorField nodes={nodes} accScale={accScale} showLabels={showLabels} />
      </div>

      <div className="panel">
        <h1>LUME · Vector Field</h1>
        <p className="sub">phase-binding + Weber search relaxation, live in 3D</p>

        <div className="row">
          <input type="text" value={query} onChange={(e) => setQuery(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && search()} placeholder="search query" />
        </div>
        <div className="row">
          <input type="text" value={db} onChange={(e) => setDb(e.target.value)} placeholder="--db" />
          <input className="small" type="number" value={candidates} min={4} max={64}
            onChange={(e) => setCandidates(e.target.value)} title="candidates" />
          <input className="small" type="number" value={steps} min={20} max={600}
            onChange={(e) => setSteps(e.target.value)} title="steps" />
        </div>
        <div className="row">
          <button onClick={search} disabled={conn !== "connected"}>▶ Search</button>
          <button className="ghost" onClick={() => setShowLabels((s) => !s)}>{showLabels ? "Hide" : "Show"} labels</button>
        </div>

        <div className="stat"><span>phase coherence (R)</span><b>{rGlobal.toFixed(3)}</b></div>
        <div className="bar"><i style={{ width: `${rGlobal * 100}%` }} /></div>
        <div className="stat"><span>frame</span><b>{frames.length ? `${Math.round(idx)} / ${frameCount - 1}` : "—"}</b></div>

        <div className="stat" style={{ marginTop: 8 }}><span>acc arrow scale</span><b>{accScale}</b></div>
        <input type="range" min={10} max={400} value={accScale} style={{ width: "100%" }}
          onChange={(e) => setAccScale(Number(e.target.value))} />

        <div className="legend">
          <div><span className="dot" style={{ background: "#ffffff" }} /> query · sphere size = cosine to query</div>
          <div><span className="dot" style={{ background: "#36d399" }} /> accelerating <b>toward</b> query &nbsp;
            <span className="dot" style={{ background: "#ff5b6e" }} /> away</div>
          <div>colors = emergent phase clusters (assemblies)</div>
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

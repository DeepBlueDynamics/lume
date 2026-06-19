import React, { useEffect, useRef, useState, useCallback } from "react";
import VectorField from "./VectorField.jsx";
import ResultsPanel from "./ResultsPanel.jsx";
import { QHUES } from "./colors.js";

const WS_URL = `ws://${location.hostname}:8086`;
const QCHIP = QHUES.map((h) => `hsl(${h}, 85%, 62%)`);

// Per-node render radius (query fixed; candidates scale with weight), an optional
// cluster "spread" that scales each cluster out from its query for viewing, then
// a position-based collision-separation so no orbs intersect.
function layout(nodes, spread) {
  if (!nodes.length) return nodes;
  let lo = Infinity, hi = -Infinity;
  for (const n of nodes) if (!n.is_query) { lo = Math.min(lo, n.score); hi = Math.max(hi, n.score); }
  const span = hi - lo || 1;
  for (const n of nodes) n.r = n.is_query ? 0.3 : 0.15 + 0.4 * ((n.score - lo) / span);

  if (spread !== 1) {
    const origQ = new Map(), newQ = new Map();
    let cx = 0, cy = 0, cz = 0, nq = 0;
    for (const n of nodes) if (n.is_query) { origQ.set(n.query_index, n.pos.slice()); cx += n.pos[0]; cy += n.pos[1]; cz += n.pos[2]; nq++; }
    const c = [nq ? cx / nq : 0, nq ? cy / nq : 0, nq ? cz / nq : 0];
    for (const n of nodes) if (n.is_query) {
      n.pos = [c[0] + (n.pos[0] - c[0]) * spread, c[1] + (n.pos[1] - c[1]) * spread, c[2] + (n.pos[2] - c[2]) * spread];
      newQ.set(n.query_index, n.pos.slice());
    }
    for (const n of nodes) if (!n.is_query) {
      const oq = origQ.get(n.query_index) || c, nqp = newQ.get(n.query_index) || c;
      n.pos = [nqp[0] + (n.pos[0] - oq[0]) * spread, nqp[1] + (n.pos[1] - oq[1]) * spread, nqp[2] + (n.pos[2] - oq[2]) * spread];
    }
  }

  // Collision radius padded ~25% over the sphere so velocity-warped ellipsoids
  // don't intersect either; iterate to convergence.
  const margin = 0.12;
  for (let iter = 0; iter < 26; iter++) {
    let moved = false;
    for (let i = 0; i < nodes.length; i++) {
      for (let j = i + 1; j < nodes.length; j++) {
        const a = nodes[i], b = nodes[j];
        const dx = b.pos[0] - a.pos[0], dy = b.pos[1] - a.pos[1], dz = b.pos[2] - a.pos[2];
        const d = Math.hypot(dx, dy, dz) || 1e-4;
        const min = a.r * 1.25 + b.r * 1.25 + margin;
        if (d < min) {
          moved = true;
          const push = min - d, ux = dx / d, uy = dy / d, uz = dz / d;
          const aw = a.is_query ? 0 : b.is_query ? 1 : 0.5;
          const bw = b.is_query ? 0 : a.is_query ? 1 : 0.5;
          a.pos = [a.pos[0] - ux * push * aw, a.pos[1] - uy * push * aw, a.pos[2] - uz * push * aw];
          b.pos = [b.pos[0] + ux * push * bw, b.pos[1] + uy * push * bw, b.pos[2] + uz * push * bw];
        }
      }
    }
    if (!moved) break;
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
      id: na.id, label: mn.label, score: mn.score ?? 0, text: mn.text || "",
      is_query: na.is_query, query_index: na.query_index ?? 0, members: mn.members || na.members || [],
      pos: lerp3(na.pos, nb.pos), acc: lerp3(na.acc, nb.acc),
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
  const [spread, setSpread] = useState(1);
  const [sortKey, setSortKey] = useState("relevance");
  const [hoveredId, setHoveredId] = useState(null);
  const [conn, setConn] = useState("connecting");
  const [status, setStatus] = useState("");
  const [answer, setAnswer] = useState(null);   // { text, used:Set, cites:[ids], model }
  const [agentLog, setAgentLog] = useState([]); // plan/evaluate round lines
  const [question, setQuestion] = useState("");

  const wsRef = useRef(null);

  useEffect(() => {
    const ws = new WebSocket(WS_URL);
    wsRef.current = ws;
    ws.onopen = () => setConn("connected");
    ws.onclose = () => setConn("disconnected");
    ws.onerror = () => setConn("error");
    ws.onmessage = (ev) => {
      const m = JSON.parse(ev.data);
      if (m.type === "question") {
        setQuestion(m.text); setAnswer(null); setAgentLog([]);
      } else if (m.type === "plan") {
        setQueries(m.queries);
        setAgentLog((l) => [...l, { kind: "plan", round: m.round, queries: m.queries, note: m.note }]);
      } else if (m.type === "evaluate") {
        setAgentLog((l) => [...l, { kind: "eval", round: m.round, sufficient: m.sufficient, note: m.note }]);
      } else if (m.type === "answer") {
        setAnswer({ text: m.text, used: new Set(m.used || []), cites: m.cites || [], model: m.model });
        setStatus("answered");
      } else if (m.type === "meta") {
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
      if (playing && framesRef.current.length > 1) setIdx((p) => Math.min(framesRef.current.length - 1, p + dt * speed * 24));
      raf = requestAnimationFrame(tick);
    };
    raf = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(raf);
  }, [playing, speed]);

  const runSearch = useCallback((qs) => {
    const ws = wsRef.current;
    const list = qs.filter(Boolean);
    setQueries(list); setAnswer(null); setAgentLog([]); setQuestion("");
    if (!list.length) { setMeta(null); framesRef.current = []; setFrameCount(0); setIdx(0); return; }
    if (!ws || ws.readyState !== ws.OPEN) { setStatus("not connected to bridge"); return; }
    framesRef.current = []; setFrameCount(0); setIdx(0); setMeta(null);
    ws.send(JSON.stringify({ type: "search", queries: list, db, candidates: Number(candidates), steps: Number(steps) }));
  }, [db, candidates, steps]);

  const search = () => runSearch([input]);
  const addSearch = () => runSearch([...queries, input]);
  const removeQuery = (i) => runSearch(queries.filter((_, k) => k !== i)); // delete + rerun

  const ask = useCallback(() => {
    const ws = wsRef.current;
    if (!input.trim()) return;
    if (!ws || ws.readyState !== ws.OPEN) { setStatus("not connected to bridge"); return; }
    framesRef.current = []; setFrameCount(0); setIdx(0); setMeta(null);
    setAnswer(null); setAgentLog([]); setQueries([]); setQuestion(input);
    ws.send(JSON.stringify({ type: "ask", question: input, db, candidates: Number(candidates), steps: Number(steps) }));
  }, [input, db, candidates, steps]);

  const frames = framesRef.current;
  const lo = Math.floor(idx), hi = Math.min(frames.length - 1, lo + 1), frac = idx - lo;
  const nodes = frames.length ? layout(interpolate(meta, frames[lo], frames[hi], frac), spread) : [];
  const rGlobal = frames.length ? frames[Math.round(idx)].r_global : 0;
  const multi = queries.length > 1;
  const usedIds = answer ? answer.used : undefined;
  const citedIds = answer ? new Set(answer.cites) : undefined;
  const labelOf = (id) => (meta?.nodes?.[id]?.label) || `#${id}`;

  return (
    <div className="app">
      <div className="canvas-wrap">
        <VectorField nodes={nodes} accScale={accScale} warp={warp} queryCount={queries.length || 1}
          hoveredId={hoveredId} onHover={setHoveredId} usedIds={usedIds} citedIds={citedIds} />
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
          <button className="ask" onClick={ask} disabled={conn !== "connected"} title="agentic answer with citations">✦ Ask</button>
        </div>

        {queries.length > 0 && (
          <div className="chips">
            {queries.map((q, i) => (
              <span key={i} className="chip" style={{ borderColor: QCHIP[i % QCHIP.length] }}>
                <i style={{ background: QCHIP[i % QCHIP.length] }} />
                {q.length > 22 ? q.slice(0, 21) + "…" : q}
                <button className="chip-x" title="remove + rerun" onClick={() => removeQuery(i)}>×</button>
              </span>
            ))}
          </div>
        )}

        <div className="stat" style={{ marginTop: 8 }}><span>phase coherence (R)</span><b>{rGlobal.toFixed(3)}</b></div>
        <div className="bar"><i style={{ width: `${rGlobal * 100}%` }} /></div>
        <div className="stat"><span>frame</span><b>{frames.length ? `${Math.round(idx)} / ${frameCount - 1}` : "—"}</b></div>

        <div className="stat" style={{ marginTop: 8 }}><span>cluster spread</span><b>{spread.toFixed(1)}×</b></div>
        <input type="range" min={1} max={3.5} step={0.1} value={spread} style={{ width: "100%" }}
          onChange={(e) => setSpread(Number(e.target.value))} />
        <div className="stat" style={{ marginTop: 4 }}><span>acc arrow scale</span><b>{accScale}</b></div>
        <input type="range" min={10} max={400} value={accScale} style={{ width: "100%" }}
          onChange={(e) => setAccScale(Number(e.target.value))} />
        <div className="stat" style={{ marginTop: 4 }}><span>orb warp</span><b>{warp}</b></div>
        <input type="range" min={0} max={60} value={warp} style={{ width: "100%" }}
          onChange={(e) => setWarp(Number(e.target.value))} />

        <div className="legend">
          <div>labels = <b>weight</b> · hover for the passage · each search has its own colour</div>
          <div><span className="dot" style={{ background: "#ffd23b" }} /> haloed = <b>overlap</b> (2+ searches)</div>
          <div>Enter = search · Ctrl-Enter = add · × on a chip removes + reruns · Ctrl-drag = pan</div>
        </div>
      </div>

      {nodes.length > 0 && (
        <ResultsPanel nodes={nodes} multi={multi} sortKey={sortKey} setSortKey={setSortKey}
          hoveredId={hoveredId} onHover={setHoveredId} />
      )}

      {(question || agentLog.length > 0) && (
        <div className="answer-panel">
          <div className="ap-q">✦ {question}</div>
          {agentLog.length > 0 && (
            <div className="ap-log">
              {agentLog.map((e, i) => e.kind === "plan"
                ? <div key={i} className="ap-plan">plan {e.round}: {e.queries.join(" · ")}{e.note ? ` (${e.note})` : ""}</div>
                : <div key={i} className="ap-eval">→ {e.sufficient ? "sufficient" : "insufficient — refining"}{e.note ? `: ${e.note}` : ""}</div>)}
            </div>
          )}
          {answer ? (
            <>
              <div className="ap-answer">{answer.text}</div>
              {answer.cites.length > 0 && (
                <div className="ap-sources">
                  <span>sources:</span>
                  {answer.cites.map((id) => (
                    <button key={id} className="src" onMouseEnter={() => setHoveredId(id)}
                      onMouseLeave={() => setHoveredId(null)} onClick={() => setHoveredId(id)}>{labelOf(id)}</button>
                  ))}
                </div>
              )}
              <div className="ap-model">— {answer.model}</div>
            </>
          ) : <div className="ap-thinking">running agent…</div>}
        </div>
      )}

      <div className="controls">
        <button onClick={() => setPlaying((p) => !p)}>{playing ? "❚❚" : "▶"}</button>
        <input type="range" min={0} max={Math.max(0, frameCount - 1)} step="0.01" value={idx}
          onChange={(e) => { setPlaying(false); setIdx(Number(e.target.value)); }} />
        <span style={{ fontSize: 11, color: "#7b829a" }}>×</span>
        <input type="range" min={0.2} max={4} step={0.1} value={speed} style={{ width: 90 }}
          onChange={(e) => setSpeed(Number(e.target.value))} />
      </div>

      <div className="status">
        bridge: <span className={conn === "connected" ? "ok" : "err"}>{conn}</span>{status ? ` · ${status}` : ""}
      </div>
    </div>
  );
}

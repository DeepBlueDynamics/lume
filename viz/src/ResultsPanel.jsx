import React from "react";
import { colorOfNode, nodeColors, OVERLAP } from "./colors.js";

const fmt = (n) => (Math.abs(n) >= 100 ? n.toFixed(0) : n.toFixed(2));

const SORTS = {
  relevance: { label: "relevance", cmp: (a, b) => b.cos_q - a.cos_q },
  weight: { label: "weight", cmp: (a, b) => b.score - a.score },
  binding: { label: "binding", cmp: (a, b) => a.approach_acc - b.approach_acc }, // most negative = toward
  overlap: { label: "overlap", cmp: (a, b) => (b.members?.length || 1) - (a.members?.length || 1) || b.cos_q - a.cos_q },
};

export default function ResultsPanel({ nodes, multi, sortKey, setSortKey, hoveredId, onHover }) {
  const colors = React.useMemo(() => nodeColors(nodes, multi), [nodes, multi]);

  const cands = nodes.filter((n) => !n.is_query).slice().sort(SORTS[sortKey].cmp);

  return (
    <div className="results">
      <div className="results-head">
        <span>results · {cands.length}</span>
        <div className="sorts">
          {Object.entries(SORTS).map(([k, s]) => (
            <button key={k} className={k === sortKey ? "on" : ""} onClick={() => setSortKey(k)}>{s.label}</button>
          ))}
        </div>
      </div>
      <div className="results-list">
        {cands.map((n) => {
          const c = colorOfNode(n, colors, multi);
          const overlap = n.members && n.members.length > 1;
          const toward = n.approach_acc < 0;
          return (
            <div
              key={n.id}
              className={"result" + (n.id === hoveredId ? " hot" : "")}
              onMouseEnter={() => onHover(n.id)}
              onMouseLeave={() => onHover(null)}
            >
              <span className="rdot" style={{ background: c }} />
              <span className="rweight">{fmt(n.score)}</span>
              <span className="rlabel" title={n.text || n.label}>
                {overlap && <b style={{ color: OVERLAP }}>★ </b>}
                {n.label}
              </span>
              <span className="rbar"><i style={{ width: `${Math.max(0, Math.min(1, n.cos_q)) * 100}%`, background: c }} /></span>
              <span className="racc" style={{ color: toward ? "#36d399" : "#ff5b6e" }}>
                {toward ? "▼" : "▲"}{Math.abs(n.approach_acc).toFixed(2)}
              </span>
            </div>
          );
        })}
      </div>
    </div>
  );
}

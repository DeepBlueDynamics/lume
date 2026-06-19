// Shared colour logic so the 3D orbs and the results list agree.

export const QHUES = [205, 32, 288, 150, 48];
export const qHsl = (qi, L = 62) => `hsl(${QHUES[qi % QHUES.length]}, 85%, ${L}%)`;
export const OVERLAP = "#ffd23b";

// Map node id -> colour. Single query → full spectrum ordered by relatedness.
// Multiple queries → coloured by anchor query (overlaps gold).
export function nodeColors(nodes, multi) {
  const map = new Map();
  if (!multi) {
    const cands = nodes.filter((n) => !n.is_query).slice().sort((a, b) => b.cos_q - a.cos_q);
    const n = Math.max(1, cands.length - 1);
    cands.forEach((nd, i) => map.set(nd.id, `hsl(${(i / n) * 300}, 85%, 58%)`));
  } else {
    for (const nd of nodes) {
      if (nd.is_query) continue;
      if (nd.members && nd.members.length > 1) map.set(nd.id, OVERLAP);
      else map.set(nd.id, qHsl(nd.query_index, 46 + 30 * Math.max(0, Math.min(1, nd.cos_q))));
    }
  }
  return map;
}

export const colorOfNode = (nd, map, multi) =>
  nd.is_query ? (multi ? qHsl(nd.query_index, 66) : "#ffffff") : (map.get(nd.id) || "#888");

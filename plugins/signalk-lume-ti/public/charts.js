'use strict';
// LumeCharts: a line chart drawn from query rows, with no libraries. A result can be
// charted when it has a time column and at least one numeric column.
(function (root) {
  const NS = 'http://www.w3.org/2000/svg';
  const COLORS = ['#13daeb', '#fbbf24', '#a78bfa', '#34d399'];
  const TIME_NAMES = /^(ts|time|timestamp|bucket|hour|day|minute|start|end|date|t)$/i;

  function svg(tag, attrs = {}, parent) {
    const node = document.createElementNS(NS, tag);
    for (const [k, v] of Object.entries(attrs)) node.setAttribute(k, v);
    if (parent) parent.appendChild(node);
    return node;
  }
  const asTime = v => {
    if (typeof v !== 'string' || !/^\d{4}-\d{2}-\d{2}/.test(v)) return NaN;
    return Date.parse(v.replace(' ', 'T').replace(/(\.\d{3})\d+/, '$1') + (/[zZ]|[+-]\d{2}:?\d{2}$/.test(v) ? '' : 'Z'));
  };
  const asNumber = v => (typeof v === 'number' ? v : typeof v === 'string' && v.trim() !== '' && !Number.isNaN(Number(v)) ? Number(v) : NaN);

  /** Which columns to plot, or null when the rows aren't a time series. */
  function detect(rows) {
    if (!Array.isArray(rows) || rows.length < 2) return null;
    const columns = Object.keys(rows[0] || {});
    const x = columns.find(c => TIME_NAMES.test(c) && rows.every(r => r[c] == null || Number.isFinite(asTime(r[c]))))
      || columns.find(c => rows.filter(r => Number.isFinite(asTime(r[c]))).length >= rows.length * 0.9);
    if (!x) return null;
    const ys = columns.filter(c => c !== x && rows.filter(r => Number.isFinite(asNumber(r[c]))).length >= rows.length * 0.5).slice(0, 4);
    return ys.length ? {x, ys} : null;
  }

  function niceTicks(lo, hi, count = 5) {
    const span = hi - lo || Math.abs(hi) || 1;
    const step = Math.pow(10, Math.floor(Math.log10(span / count)));
    const unit = [1, 2, 5, 10].map(m => m * step).find(s => span / s <= count) || step * 10;
    const ticks = [];
    for (let v = Math.ceil(lo / unit) * unit; v <= hi + unit * 1e-9; v += unit) ticks.push(+v.toFixed(10));
    return ticks;
  }
  const fmt = v => (Math.abs(v) >= 1000 ? v.toFixed(0) : Math.abs(v) >= 10 ? v.toFixed(1) : v.toFixed(2));
  function timeLabel(ms, spanMs) {
    const d = new Date(ms);
    if (spanMs > 3 * 86400000) return d.toLocaleDateString(undefined, {month: 'short', day: 'numeric'});
    return d.toLocaleTimeString(undefined, {hour: '2-digit', minute: '2-digit'});
  }

  /** Draw a line chart of rows into el. Returns false when the rows can't be charted. */
  function line(el, rows, plan = detect(rows)) {
    if (!plan) return false;
    const points = rows.map(r => ({t: asTime(r[plan.x]), ys: plan.ys.map(c => asNumber(r[c]))}))
      .filter(p => Number.isFinite(p.t)).sort((a, b) => a.t - b.t);
    if (points.length < 2) return false;
    const values = points.flatMap(p => p.ys).filter(Number.isFinite);
    if (!values.length) return false;
    let lo = Math.min(...values);
    let hi = Math.max(...values);
    if (lo === hi) { lo -= 1; hi += 1; }
    const pad = (hi - lo) * 0.08;
    lo -= pad; hi += pad;
    const t0 = points[0].t;
    const t1 = points[points.length - 1].t;
    const W = 720, H = 260, L = 52, R = 14, T = 14, B = 34;
    const X = t => L + (W - L - R) * (t1 === t0 ? 0.5 : (t - t0) / (t1 - t0));
    const Y = v => T + (H - T - B) * (1 - (v - lo) / (hi - lo));

    el.innerHTML = '';
    el.classList.add('lume-chart');
    const s = svg('svg', {viewBox: `0 0 ${W} ${H}`, role: 'img', 'aria-label': `Chart of ${plan.ys.join(', ')} over ${plan.x}`}, el);
    for (const v of niceTicks(lo, hi)) {
      svg('line', {x1: L, x2: W - R, y1: Y(v), y2: Y(v), stroke: 'rgba(238,246,255,0.08)'}, s);
      svg('text', {x: L - 8, y: Y(v) + 3, 'text-anchor': 'end', 'font-size': 10, fill: '#8ea3bd'}, s).textContent = fmt(v);
    }
    const xTicks = Math.min(6, points.length);
    for (let i = 0; i < xTicks; i++) {
      const t = t0 + (t1 - t0) * (xTicks === 1 ? 0.5 : i / (xTicks - 1));
      // The end labels sit inside the chart so they are never clipped.
      const anchor = xTicks > 1 && i === 0 ? 'start' : xTicks > 1 && i === xTicks - 1 ? 'end' : 'middle';
      svg('text', {x: X(t), y: H - 12, 'text-anchor': anchor, 'font-size': 10, fill: '#8ea3bd'}, s).textContent = timeLabel(t, t1 - t0);
    }
    plan.ys.forEach((name, k) => {
      const color = COLORS[k % COLORS.length];
      const pts = points.filter(p => Number.isFinite(p.ys[k]));
      if (pts.length === 1 || points.length <= 40) {
        for (const p of pts) svg('circle', {cx: X(p.t), cy: Y(p.ys[k]), r: 3, fill: color}, s);
      }
      if (pts.length > 1) {
        const d = pts.map((p, i) => `${i ? 'L' : 'M'} ${X(p.t).toFixed(1)} ${Y(p.ys[k]).toFixed(1)}`).join(' ');
        if (k === 0) {
          const area = `${d} L ${X(pts[pts.length - 1].t).toFixed(1)} ${H - B} L ${X(pts[0].t).toFixed(1)} ${H - B} Z`;
          svg('path', {d: area, fill: color, opacity: 0.08}, s);
        }
        svg('path', {d, fill: 'none', stroke: color, 'stroke-width': 2, 'stroke-linejoin': 'round'}, s);
      }
    });

    // Hover: a guide line and the values at the nearest point.
    const guide = svg('line', {y1: T, y2: H - B, stroke: 'rgba(238,246,255,0.35)', 'stroke-dasharray': '3 3', visibility: 'hidden'}, s);
    const tip = document.createElement('div');
    tip.className = 'lume-chart-tip';
    tip.hidden = true;
    el.appendChild(tip);
    s.addEventListener('mousemove', e => {
      const box = s.getBoundingClientRect();
      const vx = ((e.clientX - box.left) / box.width) * W;
      let best = points[0];
      for (const p of points) if (Math.abs(X(p.t) - vx) < Math.abs(X(best.t) - vx)) best = p;
      guide.setAttribute('x1', X(best.t));
      guide.setAttribute('x2', X(best.t));
      guide.setAttribute('visibility', 'visible');
      tip.hidden = false;
      tip.textContent = `${new Date(best.t).toLocaleString()} · ${plan.ys.map((n, k) => `${n}: ${Number.isFinite(best.ys[k]) ? fmt(best.ys[k]) : '–'}`).join(' · ')}`;
    });
    s.addEventListener('mouseleave', () => { guide.setAttribute('visibility', 'hidden'); tip.hidden = true; });

    const legend = document.createElement('div');
    legend.className = 'lume-chart-legend';
    plan.ys.forEach((name, k) => {
      const item = document.createElement('span');
      item.innerHTML = `<i style="background:${COLORS[k % COLORS.length]}"></i>`;
      item.appendChild(document.createTextNode(name));
      legend.appendChild(item);
    });
    el.appendChild(legend);
    return true;
  }

  const api = {detect, line};
  if (typeof module !== 'undefined' && module.exports) module.exports = api;
  else root.LumeCharts = api;
})(typeof window !== 'undefined' ? window : globalThis);

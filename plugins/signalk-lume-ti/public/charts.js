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

  /** Which columns to plot, or null when the rows aren't a time series or a track. */
  function detect(rows) {
    if (!Array.isArray(rows) || rows.length < 2) return null;
    const columns = Object.keys(rows[0] || {});
    // Latitude and longitude columns make a track (x = longitude, y = latitude).
    const numeric = c => rows.filter(r => Number.isFinite(asNumber(r[c]))).length >= rows.length * 0.5;
    const lat = columns.find(c => /lat/i.test(c) && numeric(c));
    const lon = columns.find(c => /lon|lng/i.test(c) && numeric(c));
    if (lat && lon) {
      const time = columns.find(c => TIME_NAMES.test(c) && rows.some(r => Number.isFinite(asTime(r[c]))));
      return {kind: 'track', lat, lon, time: time || null};
    }
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

  /**
   * A position track: longitude across, latitude up, scaled by cos(latitude) so the
   * shape is true, with start (green) and latest (cyan) marked.
   */
  function track(el, rows, plan) {
    let points = rows.map(r => ({lat: asNumber(r[plan.lat]), lon: asNumber(r[plan.lon]), t: plan.time ? asTime(r[plan.time]) : NaN}))
      .filter(p => Number.isFinite(p.lat) && Number.isFinite(p.lon) && Math.abs(p.lat) <= 90 && Math.abs(p.lon) <= 180);
    if (points.length < 2) return false;
    if (plan.time && points.every(p => Number.isFinite(p.t))) points = points.sort((a, b) => a.t - b.t);
    const W = 720, H = 360, P = 36;
    const midLat = points.reduce((s, p) => s + p.lat, 0) / points.length;
    const k = Math.cos(midLat * Math.PI / 180) || 1;
    const xs = points.map(p => p.lon * k);
    const ys = points.map(p => p.lat);
    let [x0, x1, y0, y1] = [Math.min(...xs), Math.max(...xs), Math.min(...ys), Math.max(...ys)];
    const span = Math.max(x1 - x0, y1 - y0, 1e-6);
    const cx = (x0 + x1) / 2;
    const cy = (y0 + y1) / 2;
    const scale = Math.min((W - 2 * P) / span, (H - 2 * P) / span);
    const X = x => W / 2 + (x - cx) * scale;
    const Y = y => H / 2 - (y - cy) * scale;

    el.innerHTML = '';
    el.classList.add('lume-chart');
    const s = svg('svg', {viewBox: `0 0 ${W} ${H}`, role: 'img', 'aria-label': 'Track of recent positions'}, el);
    for (let i = 1; i < 6; i++) {
      svg('line', {x1: (W * i) / 6, x2: (W * i) / 6, y1: 0, y2: H, stroke: 'rgba(238,246,255,0.05)'}, s);
      svg('line', {x1: 0, x2: W, y1: (H * i) / 6, y2: (H * i) / 6, stroke: 'rgba(238,246,255,0.05)'}, s);
    }
    const d = points.map((p, i) => `${i ? 'L' : 'M'} ${X(p.lon * k).toFixed(1)} ${Y(p.lat).toFixed(1)}`).join(' ');
    svg('path', {d, fill: 'none', stroke: '#13daeb', 'stroke-width': 2.5, 'stroke-linejoin': 'round', 'stroke-linecap': 'round'}, s);
    const start = points[0];
    const end = points[points.length - 1];
    svg('circle', {cx: X(start.lon * k), cy: Y(start.lat), r: 5, fill: '#34d399'}, s);
    svg('circle', {cx: X(end.lon * k), cy: Y(end.lat), r: 6, fill: '#13daeb', stroke: '#eef6ff', 'stroke-width': 2}, s);
    // Scale bar: a round distance near a fifth of the width.
    const metresPerUnit = 111320;
    const widthMetres = (W - 2 * P) / scale * metresPerUnit;
    const nice = [10, 20, 50, 100, 200, 500, 1000, 2000, 5000, 10000, 20000, 50000, 100000].find(m => m >= widthMetres / 5) || 100000;
    const barPx = nice / metresPerUnit * scale;
    svg('line', {x1: P, x2: P + barPx, y1: H - 14, y2: H - 14, stroke: '#8ea3bd', 'stroke-width': 2}, s);
    svg('text', {x: P, y: H - 20, 'font-size': 10, fill: '#8ea3bd'}, s).textContent = nice >= 1852 ? `${(nice / 1852).toFixed(nice >= 18520 ? 0 : 1)} NM` : `${nice} m`;
    svg('text', {x: W - P, y: H - 10, 'text-anchor': 'end', 'font-size': 10, fill: '#8ea3bd'}, s).textContent =
      `${end.lat.toFixed(5)}, ${end.lon.toFixed(5)}`;
    const legend = document.createElement('div');
    legend.className = 'lume-chart-legend';
    legend.innerHTML = '<span><i style="background:#34d399"></i>first</span><span><i style="background:#13daeb"></i>latest</span>';
    legend.appendChild(document.createTextNode(`${points.length} positions`));
    el.appendChild(legend);
    return true;
  }

  /** Draw a chart of rows into el. Returns false when the rows can't be charted. */
  function line(el, rows, plan = detect(rows)) {
    if (!plan) return false;
    if (plan.kind === 'track') return track(el, rows, plan);
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

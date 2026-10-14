// lume-gauges.js: dependency-free SVG gauges for the Lume TI dashboard.
// Every renderer is render(el, options) -> {update(value)}; `el` is an empty element.
// Values arrive in Signal K base units (ratio 0..1 for tank levels, m/s, K, Pa, V).
(function (root) {
  'use strict';
  const NS = 'http://www.w3.org/2000/svg';
  let uid = 0;

  function svg(tag, attrs = {}, parent) {
    const node = document.createElementNS(NS, tag);
    for (const [k, v] of Object.entries(attrs)) node.setAttribute(k, v);
    if (parent) parent.appendChild(node);
    return node;
  }
  const clamp = (v, lo, hi) => Math.min(hi, Math.max(lo, v));
  const finite = v => typeof v === 'number' && Number.isFinite(v);
  function zoneColor(value, zones, fallback) {
    for (const z of zones || []) if (value >= z.from && value <= z.to) return z.color;
    return fallback;
  }
  function polar(cx, cy, r, deg) {
    const rad = (deg - 90) * Math.PI / 180;
    return [cx + r * Math.cos(rad), cy + r * Math.sin(rad)];
  }
  function arcPath(cx, cy, r, a0, a1) {
    const [x0, y0] = polar(cx, cy, r, a0);
    const [x1, y1] = polar(cx, cy, r, a1);
    return `M ${x0} ${y0} A ${r} ${r} 0 ${a1 - a0 > 180 ? 1 : 0} 1 ${x1} ${y1}`;
  }

  // 1. Tank: a vessel outline filling with animated water, level marks and a readout.
  //    options: {label, capacity (litres), unit: 'L'|'gal'|'%', color, warnBelow (ratio)}
  function tank(el, o = {}) {
    const id = `lume-tank-${++uid}`;
    const color = o.color || '#13daeb';
    const s = svg('svg', {viewBox: '0 0 120 180', class: 'lume-gauge lume-tank', role: 'img'}, el);
    const defs = svg('defs', {}, s);
    const clip = svg('clipPath', {id: `${id}-clip`}, defs);
    svg('rect', {x: 14, y: 14, width: 92, height: 140, rx: 18}, clip);
    const grad = svg('linearGradient', {id: `${id}-water`, x1: 0, y1: 0, x2: 0, y2: 1}, defs);
    svg('stop', {offset: '0%', 'stop-color': color, 'stop-opacity': 0.95}, grad);
    svg('stop', {offset: '100%', 'stop-color': '#0960ff', 'stop-opacity': 0.9}, grad);
    svg('rect', {x: 14, y: 14, width: 92, height: 140, rx: 18, fill: 'rgba(5,13,24,0.85)'}, s);
    // Clip on the outer group; only the inner group moves, so the tank wall stays put.
    const clipped = svg('g', {'clip-path': `url(#${id}-clip)`}, s);
    const water = svg('g', {}, clipped);
    // Two offset waves drift sideways; the group moves up and down with the level.
    const wave = d => svg('path', {d, fill: `url(#${id}-water)`}, water);
    const back = wave('M -60 6 Q -45 0 -30 6 T 0 6 T 30 6 T 60 6 T 90 6 T 120 6 T 150 6 T 180 6 V 200 H -60 Z');
    const front = wave('M -60 6 Q -45 12 -30 6 T 0 6 T 30 6 T 60 6 T 90 6 T 120 6 T 150 6 T 180 6 V 200 H -60 Z');
    back.setAttribute('opacity', '0.45');
    for (const [node, dur] of [[back, '6s'], [front, '3.5s']]) {
      svg('animateTransform', {attributeName: 'transform', type: 'translate', from: '0 0', to: '60 0', dur, repeatCount: 'indefinite'}, node);
    }
    for (const f of [0.25, 0.5, 0.75]) {
      const y = 154 - 140 * f;
      svg('line', {x1: 14, x2: 26, y1: y, y2: y, stroke: 'rgba(238,246,255,0.35)', 'stroke-width': 1.5}, s);
      svg('text', {x: 30, y: y + 3, 'font-size': 8, fill: 'rgba(238,246,255,0.45)'}, s).textContent = `${f * 100}%`;
    }
    svg('rect', {x: 14, y: 14, width: 92, height: 140, rx: 18, fill: 'none', stroke: 'rgba(238,246,255,0.35)', 'stroke-width': 2}, s);
    svg('rect', {x: 22, y: 20, width: 10, height: 120, rx: 5, fill: 'rgba(255,255,255,0.07)'}, s);
    const pct = svg('text', {x: 60, y: 92, 'text-anchor': 'middle', 'font-size': 22, 'font-weight': 700, fill: '#eef6ff'}, s);
    const sub = svg('text', {x: 60, y: 108, 'text-anchor': 'middle', 'font-size': 9, fill: 'rgba(238,246,255,0.75)'}, s);
    svg('text', {x: 60, y: 172, 'text-anchor': 'middle', 'font-size': 11, 'font-weight': 600, fill: '#8ea3bd'}, s).textContent = o.label || 'Fresh water';
    let shown = 0;
    let frame = 0;
    function draw(level) {
      water.style.transform = `translateY(${154 - 140 * level - 6}px)`;
      pct.textContent = `${Math.round(level * 100)}%`;
      const low = finite(o.warnBelow) && level < o.warnBelow;
      pct.setAttribute('fill', low ? '#fbbf24' : '#eef6ff');
      if (finite(o.capacity)) {
        const litres = level * o.capacity;
        sub.textContent = o.unit === 'gal' ? `${(litres / 3.785).toFixed(0)} of ${(o.capacity / 3.785).toFixed(0)} gal` : `${litres.toFixed(0)} of ${o.capacity} L`;
      } else sub.textContent = low ? 'Low' : '';
    }
    function update(value) {
      if (!finite(value)) { pct.textContent = '—'; sub.textContent = 'No data'; return; }
      const target = clamp(value, 0, 1);
      cancelAnimationFrame(frame);
      const step = () => {
        shown += (target - shown) * 0.12;
        if (Math.abs(target - shown) < 0.001) shown = target;
        draw(shown);
        if (shown !== target) frame = requestAnimationFrame(step);
      };
      step();
    }
    update(o.value);
    return {update};
  }

  // 2. Dial: 270-degree gauge with coloured zones, ticks and a needle.
  //    options: {label, min, max, unit, decimals, zones: [{from, to, color}], scale (multiplier)}
  function dial(el, o = {}) {
    const min = o.min ?? 0;
    const max = o.max ?? 100;
    const scale = o.scale ?? 1;
    const s = svg('svg', {viewBox: '0 0 200 190', class: 'lume-gauge lume-dial', role: 'img'}, el);
    const a = v => -135 + 270 * (clamp(v, min, max) - min) / (max - min);
    svg('path', {d: arcPath(100, 100, 80, -135, 135), stroke: 'rgba(238,246,255,0.12)', 'stroke-width': 12, fill: 'none', 'stroke-linecap': 'round'}, s);
    for (const z of o.zones || []) {
      svg('path', {d: arcPath(100, 100, 80, a(z.from), a(z.to)), stroke: z.color, 'stroke-width': 12, fill: 'none', opacity: 0.85}, s);
    }
    for (let i = 0; i <= 10; i++) {
      const deg = -135 + 27 * i;
      const [x0, y0] = polar(100, 100, 64, deg);
      const [x1, y1] = polar(100, 100, i % 5 ? 70 : 58, deg);
      svg('line', {x1: x0, y1: y0, x2: x1, y2: y1, stroke: 'rgba(238,246,255,0.5)', 'stroke-width': i % 5 ? 1 : 2}, s);
      if (i % 5 === 0) {
        const [tx, ty] = polar(100, 100, 46, deg);
        svg('text', {x: tx, y: ty + 3, 'text-anchor': 'middle', 'font-size': 9, fill: '#8ea3bd'}, s).textContent = String(+(min + (max - min) * i / 10).toFixed(1));
      }
    }
    const needle = svg('g', {}, s);
    svg('path', {d: 'M 97 100 L 100 28 L 103 100 Z', fill: '#13daeb'}, needle);
    svg('circle', {cx: 100, cy: 100, r: 7, fill: '#0e1b2e', stroke: '#13daeb', 'stroke-width': 2}, s);
    needle.style.transformOrigin = '100px 100px';
    needle.style.transition = 'transform 0.6s cubic-bezier(.3,1.4,.5,1)';
    const val = svg('text', {x: 100, y: 140, 'text-anchor': 'middle', 'font-size': 22, 'font-weight': 700, fill: '#eef6ff'}, s);
    svg('text', {x: 100, y: 156, 'text-anchor': 'middle', 'font-size': 10, fill: '#8ea3bd'}, s).textContent = o.unit || '';
    svg('text', {x: 100, y: 182, 'text-anchor': 'middle', 'font-size': 11, 'font-weight': 600, fill: '#8ea3bd'}, s).textContent = o.label || '';
    function update(value) {
      if (!finite(value)) { val.textContent = '—'; return; }
      const v = value * scale;
      needle.style.transform = `rotate(${a(v)}deg)`;
      val.textContent = v.toFixed(o.decimals ?? 1);
      val.setAttribute('fill', zoneColor(v, o.zones, '#eef6ff'));
    }
    update(o.value);
    return {update};
  }

  // 3. Arc: half-donut progress, good for battery state of charge or a compact tank.
  //    options: {label, min, max, unit, decimals, scale, zones}
  function arc(el, o = {}) {
    const min = o.min ?? 0;
    const max = o.max ?? 100;
    const scale = o.scale ?? 1;
    const s = svg('svg', {viewBox: '0 0 200 120', class: 'lume-gauge lume-arc', role: 'img'}, el);
    svg('path', {d: arcPath(100, 100, 78, -90, 90), stroke: 'rgba(238,246,255,0.12)', 'stroke-width': 16, fill: 'none', 'stroke-linecap': 'round'}, s);
    const bar = svg('path', {d: arcPath(100, 100, 78, -90, -89.9), stroke: '#13daeb', 'stroke-width': 16, fill: 'none', 'stroke-linecap': 'round'}, s);
    const val = svg('text', {x: 100, y: 92, 'text-anchor': 'middle', 'font-size': 26, 'font-weight': 700, fill: '#eef6ff'}, s);
    svg('text', {x: 100, y: 114, 'text-anchor': 'middle', 'font-size': 10, fill: '#8ea3bd'}, s).textContent = [o.label, o.unit].filter(Boolean).join(' · ');
    function update(value) {
      if (!finite(value)) { val.textContent = '—'; return; }
      const v = value * scale;
      const f = (clamp(v, min, max) - min) / (max - min);
      bar.setAttribute('d', arcPath(100, 100, 78, -90, -90 + Math.max(0.1, 180 * f)));
      bar.setAttribute('stroke', zoneColor(v, o.zones, '#13daeb'));
      val.textContent = v.toFixed(o.decimals ?? 0);
    }
    update(o.value);
    return {update};
  }

  // 4. Bar: linear level with zone bands, horizontal or vertical.
  //    options: {label, min, max, unit, decimals, scale, zones, vertical}
  function bar(el, o = {}) {
    const min = o.min ?? 0;
    const max = o.max ?? 100;
    const scale = o.scale ?? 1;
    const vertical = o.vertical === true;
    const [w, h] = vertical ? [70, 200] : [240, 70];
    const s = svg('svg', {viewBox: `0 0 ${w} ${h}`, class: 'lume-gauge lume-bar', role: 'img'}, el);
    const track = vertical ? {x: 26, y: 10, width: 18, height: 150} : {x: 10, y: 26, width: 220, height: 16};
    svg('rect', {...track, rx: 8, fill: 'rgba(238,246,255,0.1)'}, s);
    const fill = svg('rect', {...track, rx: 8, fill: '#13daeb'}, s);
    const val = svg('text', vertical ? {x: 35, y: 182, 'text-anchor': 'middle', 'font-size': 14, 'font-weight': 700, fill: '#eef6ff'}
      : {x: 230, y: 18, 'text-anchor': 'end', 'font-size': 14, 'font-weight': 700, fill: '#eef6ff'}, s);
    svg('text', vertical ? {x: 35, y: 196, 'text-anchor': 'middle', 'font-size': 9, fill: '#8ea3bd'}
      : {x: 10, y: 18, 'font-size': 11, 'font-weight': 600, fill: '#8ea3bd'}, s).textContent = o.label || '';
    function update(value) {
      if (!finite(value)) { val.textContent = '—'; return; }
      const v = value * scale;
      const f = (clamp(v, min, max) - min) / (max - min);
      if (vertical) { fill.setAttribute('height', track.height * f); fill.setAttribute('y', track.y + track.height * (1 - f)); }
      else fill.setAttribute('width', track.width * f);
      fill.setAttribute('fill', zoneColor(v, o.zones, '#13daeb'));
      val.textContent = `${v.toFixed(o.decimals ?? 1)}${o.unit ? ` ${o.unit}` : ''}`;
    }
    update(o.value);
    return {update};
  }

  // 5. Digital: big number with a sparkline of the recent history.
  //    options: {label, unit, decimals, scale, history: number of points to keep}
  function digital(el, o = {}) {
    const scale = o.scale ?? 1;
    const keep = o.history ?? 60;
    const s = svg('svg', {viewBox: '0 0 220 110', class: 'lume-gauge lume-digital', role: 'img'}, el);
    svg('text', {x: 12, y: 20, 'font-size': 11, 'font-weight': 600, fill: '#8ea3bd'}, s).textContent = o.label || '';
    const val = svg('text', {x: 12, y: 62, 'font-size': 34, 'font-weight': 700, fill: '#eef6ff'}, s);
    const unit = svg('text', {x: 12, y: 78, 'font-size': 10, fill: '#8ea3bd'}, s);
    unit.textContent = o.unit || '';
    const line = svg('polyline', {fill: 'none', stroke: '#13daeb', 'stroke-width': 2, 'stroke-linejoin': 'round'}, s);
    const points = [];
    function update(value) {
      if (!finite(value)) { val.textContent = '—'; return; }
      const v = value * scale;
      val.textContent = v.toFixed(o.decimals ?? 1);
      points.push(v);
      if (points.length > keep) points.shift();
      const lo = Math.min(...points);
      const hi = Math.max(...points);
      const span = hi - lo || 1;
      line.setAttribute('points', points.map((p, i) => `${12 + 196 * i / Math.max(1, points.length - 1)},${104 - 18 * (p - lo) / span}`).join(' '));
    }
    update(o.value);
    return {update};
  }

  const renderers = {tank, dial, arc, bar, digital};

  // Live values straight from Signal K: one WebSocket, values per path as they arrive.
  // onValue(path, value) is called for every update of a subscribed path.
  function subscribe(paths, onValue, {context = 'vessels.self', period = 1000} = {}) {
    const proto = location.protocol === 'https:' ? 'wss' : 'ws';
    let ws;
    let closed = false;
    let retry = 1000;
    const open = () => {
      ws = new WebSocket(`${proto}://${location.host}/signalk/v1/stream?subscribe=none`);
      ws.onopen = () => {
        retry = 1000;
        ws.send(JSON.stringify({context, subscribe: paths.map(path => ({path, period, policy: 'instant'}))}));
      };
      ws.onmessage = event => {
        let msg;
        try { msg = JSON.parse(event.data); } catch (_) { return; }
        for (const u of msg.updates || []) for (const v of u.values || []) onValue(v.path, v.value);
      };
      ws.onclose = () => { if (!closed) setTimeout(open, retry = Math.min(retry * 2, 30000)); };
    };
    open();
    return {close() { closed = true; if (ws) ws.close(); }};
  }

  // A saved procedure: read-only SQL the plugin runs, newest value in the first row.
  async function runProcedure(proc) {
    const res = await fetch('/plugins/signalk-lume-ti/api/query', {
      method: 'POST',
      credentials: 'same-origin',
      headers: {'Content-Type': 'application/json', Accept: 'application/json'},
      body: JSON.stringify({sql: proc.sql}),
    });
    if (!res.ok) throw new Error(`HTTP ${res.status}`);
    const data = await res.json();
    return data.rows || [];
  }

  // Mount one widget from its spec (see "Widget spec" in the reference document).
  function mount(el, spec) {
    const render = renderers[spec.type];
    if (!render) throw new Error(`Unknown gauge type ${spec.type}`);
    const card = document.createElement('div');
    card.className = `lume-widget size-${spec.size || '1x1'}`;
    el.appendChild(card);
    const gauge = render(card, spec.options || {});
    let stop = () => {};
    if (spec.source && spec.source.kind === 'signalk') {
      const sub = subscribe([spec.source.path], (path, value) => {
        if (path === spec.source.path) gauge.update(typeof value === 'object' && value !== null ? value[spec.source.field] : value);
      });
      stop = () => sub.close();
    } else if (spec.source && spec.source.kind === 'procedure') {
      const tick = () => runProcedure(spec.source).then(rows => {
        const row = rows[0] || {};
        gauge.update(row[spec.source.column || 'value']);
      }).catch(() => gauge.update(NaN));
      tick();
      const timer = setInterval(tick, Math.max(2, spec.source.everySeconds || 10) * 1000);
      stop = () => clearInterval(timer);
    }
    return {gauge, stop};
  }

  root.LumeGauges = {...renderers, subscribe, runProcedure, mount};
})(typeof window !== 'undefined' ? window : globalThis);

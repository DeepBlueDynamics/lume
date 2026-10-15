'use strict';

const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const test = require('node:test');
const assert = require('node:assert/strict');
const W = require('../public/lume-svg-widget');
const G = require('../public/lume-gauges');
const D = require('../public/dashboard');
const {Dashboard, validateWidget} = require('../lib/dashboard');

test('expressions compute values and refuse anything that reaches outside', () => {
  assert.equal(W.template('rotate({{-deg(hdg)}} 100 100)')({hdg: Math.PI / 2}), 'rotate(-90 100 100)');
  assert.equal(W.template('{{nav ? "#34d399" : "#1f2937"}}')({nav: true}), '#34d399');
  assert.equal(W.template('{{nav ? "#34d399" : "#1f2937"}}')({nav: 0}), '#1f2937');
  assert.equal(W.template('{{fixed(knots(sog), 1)}} kn')({sog: 5}), '9.7 kn');
  assert.equal(W.template('{{has(x) ? x : "—"}}')({}), '—');
  for (const bad of ["constructor.constructor('x')()", 'alert(1)', 'a.b', 'x = 1', 'a[0]', '`x`']) {
    assert.throws(() => W.compile(bad), String(bad));
  }
  for (const name of ['window', 'globalThis', 'constructor', '__proto__', 'toString', 'process']) {
    assert.equal(W.compile(name)({}), undefined, `${name} is not reachable`);
  }
});

test('custom widgets validate on the server', () => {
  const ok = validateWidget({id: 'compass', type: 'svg', options: {svg: "<circle cx='100' cy='100' r='80' fill='url(#g)'/><text>{{fixed(deg(cog),0)}}</text>"},
    source: {kind: 'signalk', paths: {cog: 'navigation.courseOverGroundTrue'}}});
  assert.equal(ok.options.viewBox, '0 0 200 200');
  assert.deepEqual(ok.source.paths, {cog: 'navigation.courseOverGroundTrue'});
  for (const [svg, why] of [
    ['<script>alert(1)</script>', /no script/],
    ["<circle onclick='alert(1)'/>", /event handlers/],
    ["<image href='http://x/y.png'/>", /no script/],
    ["<a href='javascript:alert(1)'><text>x</text></a>", /no script/],
    ["<rect fill='url(http://evil/x)'/>", /no script/],
    ['<style>*{}</style>', /no script/],
    ['<foreignObject><div/></foreignObject>', /no script/],
    ["<set attributeName='x'/>", /no script/],
    ['', /needs options.svg/],
    ['x'.repeat(13000), /at most/],
  ]) {
    assert.throws(() => validateWidget({id: 'w', type: 'svg', options: {svg}, source: {kind: 'signalk', paths: {v: 'a.b'}}}), why, svg.slice(0, 40));
  }
  assert.throws(() => validateWidget({id: 'w', type: 'svg', options: {svg: '<g/>', viewBox: '0 0 100'}, source: {kind: 'signalk', paths: {v: 'a.b'}}}), /viewBox/);
  assert.throws(() => validateWidget({id: 'w', type: 'svg', options: {svg: '<g/>'}, source: {kind: 'signalk', paths: {'bad name': 'a.b'}}}), /identifier/);
  assert.throws(() => validateWidget({id: 'w', type: 'svg', options: {svg: '<g/>'}, source: {kind: 'signalk', paths: {v: 'a b'}}}), /Signal K path/);
});

test('line graph widgets validate, with their time-series query', () => {
  const w = validateWidget({id: 'wind-24h', title: 'Wind, 24 h', type: 'line', size: '2x1',
    options: {label: 'Wind', unit: 'kn', scale: 1.94384, windowMinutes: 30, staleSeconds: 45},
    source: {kind: 'procedure', everySeconds: 60,
      sql: "SELECT date_bin(INTERVAL '15 minutes', ts) AS ts, avg(\"environment.wind.speedTrue@mean\") AS wind FROM telemetry GROUP BY 1 ORDER BY 1"}});
  assert.equal(w.type, 'line');
  assert.equal(w.options.windowMinutes, 30);
  assert.equal(w.options.staleSeconds, 45);
  assert.equal(w.source.everySeconds, 60);
});

test('dashboard drag reorder updates widget order and persists to dashboard.json', () => {
  assert.deepEqual(D.reorderIds(['sog', 'wind', 'depth'], 'depth', 'sog', false), ['depth', 'sog', 'wind']);
  assert.deepEqual(D.reorderIds(['sog', 'wind', 'depth'], 'sog', 'wind', true), ['wind', 'sog', 'depth']);

  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'lume-dash-order-'));
  try {
    const dash = new Dashboard(tmpDir);
    dash.put({id: 'sog', type: 'dial', source: {kind: 'signalk', path: 'navigation.speedOverGround'}});
    dash.put({id: 'wind', type: 'dial', source: {kind: 'signalk', path: 'environment.wind.speedTrue'}});
    dash.put({id: 'depth', type: 'digital', source: {kind: 'signalk', path: 'environment.depth.belowTransducer'}});
    const reordered = dash.move(D.reorderIds(['sog', 'wind', 'depth'], 'depth', 'sog', false));
    assert.deepEqual(reordered, ['depth', 'sog', 'wind']);
    assert.deepEqual(dash.load().widgets.map(w => w.id), ['depth', 'sog', 'wind']);
  } finally {
    fs.rmSync(tmpDir, {recursive: true, force: true});
  }
});

test('night mode toggle updates classes, aria-pressed, and button label', () => {
  const mkEl = () => {
    const classes = new Set();
    const attrs = new Map();
    return {
      textContent: '',
      classList: {
        toggle(c, force) {
          const on = force === undefined ? !classes.has(c) : Boolean(force);
          if (on) classes.add(c); else classes.delete(c);
          return on;
        },
        contains(c) { return classes.has(c); },
      },
      setAttribute(k, v) { attrs.set(k, v); },
      getAttribute(k) { return attrs.get(k); },
    };
  };
  const body = mkEl();
  const panel = mkEl();
  const grid = mkEl();
  const btn = mkEl();
  const doc = {
    body,
    getElementById(id) {
      if (id === 'tab-dashboard') return panel;
      if (id === 'dash-grid') return grid;
      if (id === 'dash-night-btn') return btn;
      return null;
    },
  };

  D.applyNightMode(true, doc);
  assert.equal(body.classList.contains('lume-night-mode'), true);
  assert.equal(panel.classList.contains('night-mode'), true);
  assert.equal(grid.classList.contains('night-mode'), true);
  assert.equal(btn.getAttribute('aria-pressed'), 'true');
  assert.match(btn.textContent, /Day mode/);

  D.applyNightMode(false, doc);
  assert.equal(body.classList.contains('lume-night-mode'), false);
  assert.equal(panel.classList.contains('night-mode'), false);
  assert.equal(btn.getAttribute('aria-pressed'), 'false');
  assert.match(btn.textContent, /Night mode/);

  const html = fs.readFileSync(path.join(__dirname, '..', 'public', 'index.html'), 'utf8');
  assert.match(html, /id="dash-night-btn"/);
});

test('stale-value marker flags overdue or errored widgets and clears on fresh update', () => {
  assert.equal(G.staleThresholdMs({source: {kind: 'signalk', path: 'a.b'}}), 30000);
  assert.equal(G.staleThresholdMs({options: {staleSeconds: 15}, source: {kind: 'signalk', path: 'a.b'}}), 15000);
  assert.equal(G.staleThresholdMs({source: {kind: 'procedure', everySeconds: 25, sql: 'SELECT 1'}}), 50000);

  // Fresh update 5s ago with 30s threshold -> not stale
  const fresh = G.computeStaleState(10000, 5000, 15000, 30000, false);
  assert.equal(fresh.stale, false);

  // 35s since last update -> stale with age
  const overdue = G.computeStaleState(10000, 5000, 45000, 30000, false);
  assert.equal(overdue.stale, true);
  assert.equal(overdue.label, 'Stale · 35s');

  // Never updated after 31s -> stale with "no data"
  const neverUpdated = G.computeStaleState(null, 0, 31000, 30000, false);
  assert.equal(neverUpdated.stale, true);
  assert.equal(neverUpdated.label, 'Stale · no data');

  // Procedure error -> immediately stale
  const errored = G.computeStaleState(10000, 5000, 12000, 30000, true);
  assert.equal(errored.stale, true);
});

test('shared procedure cache coalesces concurrent requests and serves fresh cached rows', async () => {
  G.clearProcedureCache();
  let fetchCalls = 0;
  const fakeFetch = async () => {
    fetchCalls++;
    await new Promise(r => setTimeout(r, 20));
    return {
      ok: true,
      status: 200,
      async json() {
        return {rows: [{value: 12.8}]};
      },
    };
  };

  const proc = {kind: 'procedure', sql: 'SELECT 12.8 AS value FROM telemetry LIMIT 1;', everySeconds: 10};
  // Two concurrent widgets with the same SQL should coalesce into 1 fetch
  const [r1, r2] = await Promise.all([
    G.runProcedure(proc, {now: 1000, fetchImpl: fakeFetch}),
    G.runProcedure(proc, {now: 1000, fetchImpl: fakeFetch}),
  ]);
  assert.equal(fetchCalls, 1);
  assert.deepEqual(r1, [{value: 12.8}]);
  assert.deepEqual(r2, [{value: 12.8}]);

  // Subsequent call within TTL returns from cache with 0 additional fetches
  const r3 = await G.runProcedure(proc, {now: Date.now(), fetchImpl: fakeFetch});
  assert.equal(fetchCalls, 1);
  assert.deepEqual(r3, [{value: 12.8}]);
  const stats = G.procedureCacheStats();
  assert.equal(stats.misses, 1);
  assert.equal(stats.coalesced, 1);
  assert.equal(stats.hits, 1);
});

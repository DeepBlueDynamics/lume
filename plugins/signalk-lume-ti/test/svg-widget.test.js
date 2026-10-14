'use strict';

const test = require('node:test');
const assert = require('node:assert/strict');
const W = require('../public/lume-svg-widget');
const {validateWidget} = require('../lib/dashboard');

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
    options: {label: 'Wind', unit: 'kn', scale: 1.94384, windowMinutes: 30},
    source: {kind: 'procedure', everySeconds: 60,
      sql: "SELECT date_bin(INTERVAL '15 minutes', ts) AS ts, avg(\"environment.wind.speedTrue@mean\") AS wind FROM telemetry GROUP BY 1 ORDER BY 1"}});
  assert.equal(w.type, 'line');
  assert.equal(w.options.windowMinutes, 30);
  assert.equal(w.source.everySeconds, 60);
});

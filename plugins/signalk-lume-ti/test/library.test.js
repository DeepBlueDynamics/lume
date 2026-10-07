'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const {Library, readList, listWithState, parseCsv, rowId, sqlString, DEFAULT_URLS, paths} = require('../lib/library');

test('row ids match the Rust crawl --list ids (FNV-1a 64 >> 16)', () => {
  // Same constant as src/crawl_list.rs; computed independently in Python.
  assert.equal(rowId('https://www.navcen.uscg.gov/sites/default/files/pdf/navRules/navrules.pdf'), 'dc45ebdf001f');
  assert.equal(rowId(' https://a.example/x.pdf '), rowId('https://a.example/x.pdf'));
});

test('bundled list parses, ids are unique, and every default exists', () => {
  const items = readList();
  assert.equal(items.length, 471);
  assert.equal(new Set(items.map(i => i.id)).size, items.length);
  assert.equal(items.filter(i => i.default).length, DEFAULT_URLS.length);
  const docsCopy = path.join(__dirname, '..', '..', '..', 'docs', 'cruiser_library.csv');
  if (fs.existsSync(docsCopy)) {
    assert.equal(fs.readFileSync(path.join(__dirname, '..', 'library', 'cruiser_library.csv'), 'utf8'),
      fs.readFileSync(docsCopy, 'utf8'), 'plugin copy must match docs/cruiser_library.csv');
  }
});

test('csv parser handles quotes, commas and embedded newlines', () => {
  const rows = parseCsv('a,b\n"x, y","say ""hi""\nthere"\n\n1,2\r\n');
  assert.deepEqual(rows[1], ['x, y', 'say "hi"\nthere']);
  assert.deepEqual(rows[2], ['1', '2']);
  assert.throws(() => parseCsv('a\n"open'));
});

test('state comes from the fetch manifest and the indexed list', () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'lume-lib-'));
  const p = paths(dir);
  const [a, b, c] = readList().slice(0, 3);
  fs.mkdirSync(p.files, {recursive: true});
  fs.writeFileSync(p.manifest, JSON.stringify({entries: {
    [a.id]: {id: a.id, status: 'ok', bytes: 2048},
    [b.id]: {id: b.id, status: 'ok', bytes: 10},
    [c.id]: {id: c.id, status: 'failed', error: 'server returned HTML, not a PDF'},
  }}));
  fs.writeFileSync(p.indexed, JSON.stringify({ids: [a.id]}));
  const byId = Object.fromEntries(listWithState(dir).map(i => [i.id, i]));
  assert.equal(byId[a.id].state, 'indexed');
  assert.equal(byId[b.id].state, 'fetched');
  assert.equal(byId[c.id].state, 'failed');
  assert.match(byId[c.id].error, /not a PDF/);
  fs.rmSync(dir, {recursive: true, force: true});
});

test('alerts map to reference queries, with a fallback', () => {
  const lib = new Library({binary: 'lume', dataDir: os.tmpdir()});
  assert.match(lib.ruleFor({id: 'notifications/notifications.environment.depth.belowKeel/1'}).query, /grounding/);
  assert.match(lib.ruleFor({title: 'Engine overTemperature'}).query, /overheating/);
  assert.match(lib.ruleFor({id: 'notifications.mob'}).query, /man overboard/);
  assert.match(lib.ruleFor({title: 'Battery voltage low'}).query, /battery/);
  assert.equal(lib.ruleFor({title: 'something unusual'}).query, 'emergency procedure checklist');
});

test('search text is reduced to plain words before it reaches SQL', () => {
  assert.equal(sqlString("bilge'); DROP TABLE x;--"), 'bilge DROP TABLE x --');
  assert.equal(sqlString('  engine   overheating  '), 'engine overheating');
  assert.equal(sqlString('x'.repeat(500)).length, 200);
});

test('index rejects unknown ids and concurrent jobs', async () => {
  const lib = new Library({binary: process.execPath, dataDir: fs.mkdtempSync(path.join(os.tmpdir(), 'lume-lib-'))});
  await assert.rejects(lib.index(['000000000000']), /Select at least one/);
  lib.job.running = true;
  await assert.rejects(lib.index([readList()[0].id]), /already running/);
});

test('search without an index reports it instead of failing', async () => {
  const lib = new Library({binary: 'lume', dataDir: fs.mkdtempSync(path.join(os.tmpdir(), 'lume-lib-'))});
  const result = await lib.search('bilge pump');
  assert.deepEqual(result.hits, []);
  assert.match(result.note, /not indexed/);
});

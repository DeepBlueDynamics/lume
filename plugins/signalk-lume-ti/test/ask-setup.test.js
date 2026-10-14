'use strict';

const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const {ChatManager, registerChatRoutes, chatSetup, explainChatFailure, needsCloudKey, looksLikeKey} = require('../lib/chat');
const md = require('../public/md');

const KEY = '0123456789abcdef0123456789abcdef.FakeTestKeyForLumeAskTests00';

function mockApp(dataDir, saved = {}) {
  return {
    saved,
    securityStrategy: {
      isDummy: () => false,
      allowConfigure: req => req.skIsAuthenticated && req.skPrincipal?.permissions === 'admin',
    },
    getDataDirPath: () => dataDir,
    savePluginOptions(options, cb) { this.saved = options; cb(); },
  };
}

function routesFor(app, config) {
  const routes = {};
  const router = {post: (p, h) => { routes[`POST ${p}`] = h; }, get: (p, h) => { routes[`GET ${p}`] = h; }};
  let current = config;
  registerChatRoutes(router, app, () => null, {getConfig: () => current, onConfigSaved: next => { current = next; }});
  return {routes, config: () => current};
}

function call(handler, {admin = true, body} = {}) {
  return new Promise(resolve => {
    const headers = body === undefined ? {} : {'content-type': 'application/json', 'content-length': String(JSON.stringify(body).length)};
    const req = {skIsAuthenticated: admin, skPrincipal: {permissions: admin ? 'admin' : 'readonly'}, headers, body};
    const res = {
      code: 200,
      status(c) { this.code = c; return this; },
      json(data) { resolve({code: this.code, body: data}); return this; },
    };
    handler(req, res);
  });
}

test('a pasted key is recognised as a key, a path is not', () => {
  assert.equal(looksLikeKey(KEY), true);
  assert.equal(looksLikeKey('/home/node/.signalk/plugin-config-data/signalk-lume-ti/ollama.key'), false);
  assert.equal(looksLikeKey('C:\\keys\\ollama.key'), false);
  assert.equal(looksLikeKey('ollama.key'), false);
});

test('only an all-ollama.com endpoint list needs a cloud key', () => {
  assert.equal(needsCloudKey('https://ollama.com'), true);
  assert.equal(needsCloudKey('api.ollama.com'), true);
  assert.equal(needsCloudKey('https://ollama.com,http://192.168.1.20:11434'), false);
  assert.equal(needsCloudKey('http://127.0.0.1:11434'), false);
  assert.equal(needsCloudKey('https://ollama.com.evil.example'), false);
});

test('a key pasted into the path setting is refused without repeating it', async () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'lume-ask-'));
  const chat = new ChatManager({binary: process.execPath, dataDir: dir, getOptions: () => ({chatApiKeyFile: KEY})});
  await assert.rejects(chat.ask('hello'), err => {
    assert.equal(err.code, 'key_not_path');
    assert.ok(!err.message.includes(KEY), 'message must not contain the key');
    assert.match(err.message, /chatApiKeyFile/);
    return true;
  });
  fs.rmSync(dir, {recursive: true, force: true});
});

test('lume chat failures become sentences a user can act on', () => {
  assert.equal(explainChatFailure('HTTP status client error (401 Unauthorized)', 'https://ollama.com').code, 'bad_key');
  const offline = explainChatFailure('error sending request: dns error: failed to lookup address', 'https://ollama.com');
  assert.equal(offline.code, 'unreachable');
  assert.match(offline.message, /internet connection/);
  assert.equal(explainChatFailure('model "x" not found, try pulling it first', 'https://ollama.com').code, 'no_model');
  const other = explainChatFailure(`panic at ${KEY}`, 'https://ollama.com');
  assert.equal(other.code, 'failed');
  assert.ok(!other.message.includes(KEY));
});

test('POST /api/chat/key writes a private key file and points the setting at it', async () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'lume-ask-'));
  const app = mockApp(dir);
  const {routes, config} = routesFor(app, {chatApiKeyFile: KEY, chatModel: 'glm-5.3:cloud'});

  assert.equal((await call(routes['POST /api/chat/key'], {admin: false, body: {key: KEY}})).code, 401);

  const saved = await call(routes['POST /api/chat/key'], {body: {key: ` ${KEY}\n`}});
  assert.equal(saved.code, 200);
  assert.ok(!JSON.stringify(saved.body).includes(KEY), 'response must not echo the key');
  assert.equal(saved.body.key, 'ok');
  assert.equal(saved.body.ready, true);

  const keyFile = path.join(dir, 'ollama.key');
  assert.equal(fs.readFileSync(keyFile, 'utf8'), `${KEY}\n`);
  if (process.platform !== 'win32') assert.equal(fs.statSync(keyFile).mode & 0o777, 0o600);
  assert.equal(app.saved.chatApiKeyFile, keyFile);
  assert.equal(app.saved.chatOllamaUrl, 'https://ollama.com');
  assert.ok(!JSON.stringify(app.saved).includes(KEY), 'plugin settings must not hold the key');
  assert.equal(config().chatApiKeyFile, keyFile);

  const bad = await call(routes['POST /api/chat/key'], {body: {key: 'two words'}});
  assert.equal(bad.code, 400);
  fs.rmSync(dir, {recursive: true, force: true});
});

test('GET /api/chat/setup reports key state without the key', async () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'lume-ask-'));
  const keyFile = path.join(dir, 'ollama.key');
  fs.writeFileSync(keyFile, `${KEY}\n`, {mode: 0o600});
  assert.deepEqual(
    [chatSetup({}).key, chatSetup({}).ready, chatSetup({chatApiKeyFile: KEY}).key, chatSetup({chatApiKeyFile: keyFile}).key],
    ['none', false, 'pasted_in_path', 'ok']
  );
  assert.equal(chatSetup({chatOllamaUrl: 'http://127.0.0.1:11434'}).ready, true);
  assert.equal(chatSetup({chatApiKeyFile: path.join(dir, 'missing.key')}).key, 'unreadable');

  const {routes} = routesFor(mockApp(dir), {chatApiKeyFile: KEY});
  const res = await call(routes['GET /api/chat/setup']);
  assert.equal(res.code, 200);
  assert.equal(res.body.key, 'pasted_in_path');
  assert.ok(!JSON.stringify(res.body).includes(KEY));
  fs.rmSync(dir, {recursive: true, force: true});
});

test('Ask answers render as Markdown with no way to inject HTML', () => {
  const html = md.render('## Library\n\n| Doc | Sections |\n|---|---|\n| `a.pdf` | **226** |\n\n- one\n- two\n\n<img src=x onerror=alert(1)> [x](javascript:alert(1)) [ok](https://ollama.com)');
  assert.match(html, /<h4>Library<\/h4>/);
  assert.match(html, /<table>.*<th>Doc<\/th>.*<td><code>a\.pdf<\/code><\/td><td><strong>226<\/strong><\/td>/s);
  assert.match(html, /<ul><li>one<\/li><li>two<\/li><\/ul>/);
  assert.ok(!/<img/i.test(html), 'raw HTML is escaped');
  assert.ok(!/href="javascript:/i.test(html), 'only http(s) links become links');
  assert.match(html, /<a href="https:\/\/ollama\.com" target="_blank" rel="noopener noreferrer">ok<\/a>/);
  assert.equal(md.render('snake_case_name and 2*3*4'), '<p>snake_case_name and 2*3*4</p>');
});

test('links keep their attributes when underscores sit nearby', () => {
  const html = md.render('See [docs](https://x.com/_a_/b) or foo_ bar_baz and _real italic_');
  assert.match(html, /<a href="https:\/\/x\.com\/_a_\/b" target="_blank" rel="noopener noreferrer">docs<\/a>/);
  assert.match(html, /<em>real italic<\/em>/);
  assert.ok(!/target="<em>/.test(html));
  // A literal NUL can't forge a placeholder: it is dropped and the text stays.
  assert.equal(md.render('a\u00000\u0000b'), '<p>a0b</p>');
});

test('Ask gets the Library titles for the stored file names, bounded', () => {
  const {libraryContext} = require('../lib/chat');
  assert.equal(libraryContext([]), '');
  const text = libraryContext([{file: 'dc45ebdf001f.pdf', title: 'Navigation Rules and Regulations Handbook', publisher: 'USCG NAVCEN', category: 'Navigation'}]);
  assert.match(text, /not from the user/);
  assert.match(text, /- dc45ebdf001f\.pdf: "Navigation Rules and Regulations Handbook", USCG NAVCEN \(Navigation\)/);
  const many = Array.from({length: 2000}, (_, i) => ({file: `${String(i).padStart(12, '0')}.pdf`, title: 'A fairly long reference title for testing', publisher: 'Somebody'}));
  const bounded = libraryContext(many);
  assert.ok(bounded.length < 25000, `context stays bounded (${bounded.length})`);
  assert.match(bounded, /…and \d+ more/);
});

test('Ask can link Library copies at a page, but no other local path', () => {
  const html = md.render('See [Rules, page 98]( /plugins/signalk-lume-ti/api/library/file/dc45ebdf001f.pdf#page=98 ) and [x](/admin/#/security) and [y](/plugins/signalk-lume-ti/api/chat/key)');
  assert.match(html, /<a href="\/plugins\/signalk-lume-ti\/api\/library\/file\/dc45ebdf001f\.pdf#page=98" target="_blank" rel="noopener noreferrer">Rules, page 98<\/a>/);
  assert.ok(!/href="\/admin/.test(html) && !/href="\/plugins\/signalk-lume-ti\/api\/chat/.test(html), 'other local paths stay text');
});

test('catalog suggestions find not-yet-indexed references by topic', () => {
  const {Library} = require('../lib/library');
  const lib = new Library({binary: process.execPath, dataDir: fs.mkdtempSync(path.join(os.tmpdir(), 'lume-sugg-'))});
  const titles = lib.suggestions('diesel injector problems').map(s => s.title).join(' | ');
  assert.match(titles, /Universal M|Engineman/);
  assert.deepEqual(lib.suggestions('what do you have'), []);
});

test('Ask is told what the page numbers mean and which recent queries failed', () => {
  const {statusContext, historyContext, ChatManager} = require('../lib/chat');
  const status = statusContext({running: true, pid: 30, uptimeSecs: 90, updates: 56000, lagSeconds: 0, disk: '640 KB', wal: '396 B'});
  assert.match(status, /"Updates".*56000.*not a row count/s);
  assert.match(status, /one row per 10-second bucket/);
  assert.equal(statusContext(null), '');

  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'lume-hist-'));
  const chat = new ChatManager({binary: process.execPath, dataDir: dir});
  chat.record('what are the 56K updates?', {tool_calls: [{name: 'ti_query', args: {sql: 'SELECT "x@max" FROM telemetry'}, error: 'Schema error: No field named "x@max". Valid fields are a, b, c'}]});
  chat.record('depth now?', {tool_calls: [{name: 'ti_query', args: {sql: 'SELECT 1'}, rows: 1}]});
  for (let i = 0; i < 120; i++) chat.record(`q${i}`, {tool_calls: []});
  assert.equal(chat.history().length, 100, 'history is capped');
  const fresh = new ChatManager({binary: process.execPath, dataDir: fs.mkdtempSync(path.join(os.tmpdir(), 'lume-hist2-'))});
  fresh.record('what are the 56K updates?', {tool_calls: [{name: 'ti_query', args: {sql: 'SELECT "x@max" FROM telemetry'}, error: 'Schema error: No field named "x@max". Valid fields are a, b, c'}]});
  const ctx = historyContext(fresh.history());
  assert.match(ctx, /- DO NOT REUSE: SELECT "x@max" FROM telemetry -> Schema error: No field named "x@max"\./);
  assert.match(ctx, /Never run one of them again as written/);

  // The same bad query twice is listed once; one that later worked is not bad; good ones are offered.
  fresh.record('again', {tool_calls: [{name: 'ti_query', args: {sql: 'SELECT "x@max" FROM telemetry'}, error: 'Schema error: No field named "x@max".'}]});
  fresh.record('fixed', {tool_calls: [{name: 'ti_query', args: {sql: 'SELECT "y@max" FROM telemetry'}, error: 'boom'}]});
  fresh.record('fixed later', {tool_calls: [{name: 'ti_query', args: {sql: 'SELECT "y@max" FROM telemetry'}, rows: 3}]});
  const ctx2 = historyContext(fresh.history());
  assert.equal(ctx2.match(/DO NOT REUSE: SELECT "x@max"/g).length, 1);
  assert.ok(!/DO NOT REUSE: SELECT "y@max"/.test(ctx2), 'a query that later worked is not flagged');
  assert.match(ctx2, /Known good:[\s\S]*SELECT "y@max" FROM telemetry \(3 rows\)/);
  assert.ok(!/Valid fields/.test(ctx), 'column lists are dropped');
  fs.rmSync(dir, {recursive: true, force: true});
});

test('charts are offered only for a time column with numbers', () => {
  const charts = require('../public/charts');
  const series = [{bucket: '2026-10-14T00:00:00Z', avg_depth: 15.29}, {bucket: '2026-10-14T01:00:00Z', avg_depth: '15.56'}];
  assert.deepEqual(charts.detect(series), {x: 'bucket', ys: ['avg_depth']});
  assert.deepEqual(charts.detect([{ts: '2026-10-14 00:00:00.123456', a: 1, b: 2, name: 'x'}, {ts: '2026-10-14 00:00:10', a: 2, b: 3, name: 'y'}]), {x: 'ts', ys: ['a', 'b']});
  assert.equal(charts.detect([{total_buckets: 84}]), null, 'one row is not a series');
  assert.equal(charts.detect([{file: 'a.pdf', title: 'x'}, {file: 'b.pdf', title: 'y'}]), null, 'no time column');
  assert.equal(charts.detect([{ts: '2026-10-14T00:00:00Z', label: 'a'}, {ts: '2026-10-14T00:01:00Z', label: 'b'}]), null, 'no numbers');
});

test('versions report the plugin, lume, Signal K and Node', async () => {
  const versions = require('../lib/versions');
  const v = await versions.collect({app: {config: {version: '2.33.0'}}, binaryInfo: {found: true, path: process.execPath, source: 'config.lumePath'}});
  assert.equal(v.plugin.version, require('../package.json').version);
  assert.equal(v.signalk.version, '2.33.0');
  assert.match(v.node.version, /^v\d+/);
  assert.match(v.lume.sha256, /^[0-9a-f]{64}$/);
  assert.ok(v.lume.bytes > 0);
  assert.equal(versions.cached(), v);
});

test('Ask is told how much history exists and a graph step that fits it', () => {
  const {statusContext} = require('../lib/chat');
  const hour = statusContext({running: true, updates: 1, firstTs: '2026-10-14T00:12:10Z', lastTs: '2026-10-14T01:12:10Z', buckets: 360});
  assert.match(hour, /covers 2026-10-14T00:12:10Z to 2026-10-14T01:12:10Z \(60 minutes, 360 buckets\)/);
  assert.match(hour, /about INTERVAL '1 minute'/);
  assert.match(hour, /fewer than 10 points/);
  const week = statusContext({running: true, firstTs: '2026-10-07T00:00:00Z', lastTs: '2026-10-14T00:00:00Z', buckets: 60480});
  assert.match(week, /7\.0 days/);
  assert.match(week, /about INTERVAL '3 hours'/);
});

test('positions chart as a track, not a time series', () => {
  const charts = require('../public/charts');
  const rows = [
    {ts: '2026-10-14T00:00:00Z', 'navigation.position.latitude': 60.15, 'navigation.position.longitude': 24.95},
    {ts: '2026-10-14T00:00:10Z', 'navigation.position.latitude': 60.151, 'navigation.position.longitude': 24.952},
  ];
  assert.deepEqual(charts.detect(rows), {kind: 'track', lat: 'navigation.position.latitude', lon: 'navigation.position.longitude', time: 'ts'});
});

test('dashboard widgets are validated as data', () => {
  const {validateWidget, Dashboard} = require('../lib/dashboard');
  const w = validateWidget({id: 'wind-speed', title: 'True wind', type: 'dial', size: '1x1',
    options: {label: 'True wind', unit: 'kn', min: 0, max: 40, scale: 1.94384, zones: [{from: 25, to: 40, color: '#f87171'}], onload: 'alert(1)'},
    source: {kind: 'signalk', path: 'environment.wind.speedTrue'}});
  assert.equal(w.options.onload, undefined, 'unknown options are dropped');
  assert.equal(w.options.zones.length, 1);
  for (const [spec, why] of [
    [{id: 'Bad ID', type: 'dial', source: {kind: 'signalk', path: 'a.b'}}, /id must/],
    [{id: 'x', type: 'pie', source: {kind: 'signalk', path: 'a.b'}}, /type must/],
    [{id: 'x', type: 'dial', source: {kind: 'signalk', path: 'a b; drop'}}, /Signal K path/],
    [{id: 'x', type: 'dial', source: {kind: 'procedure', sql: 'DELETE FROM telemetry'}}, /SELECT or WITH/],
    [{id: 'x', type: 'dial', source: {kind: 'procedure', sql: 'SELECT 1; SELECT 2'}}, /single statement/],
    [{id: 'x', type: 'dial', source: {kind: 'script', code: 'x'}}, /source.kind/],
  ]) assert.throws(() => validateWidget(spec), why);
  const proc = validateWidget({id: 'avg', type: 'digital', source: {kind: 'procedure', sql: 'SELECT 1 AS value;', everySeconds: 0.5}});
  assert.equal(proc.source.everySeconds, 2, 'refresh is at most every 2 seconds');
  assert.equal(proc.source.sql, 'SELECT 1 AS value');

  const dash = new Dashboard(fs.mkdtempSync(path.join(os.tmpdir(), 'lume-dash-')));
  dash.put(w);
  dash.put({...w, id: 'second'});
  dash.put({...w, title: 'Wind (kn)'});
  assert.deepEqual(dash.load().widgets.map(x => [x.id, x.title]), [['wind-speed', 'Wind (kn)'], ['second', 'True wind']]);
  assert.deepEqual(dash.move(['second', 'wind-speed']), ['second', 'wind-speed']);
  assert.equal(dash.remove('second'), true);
  assert.equal(dash.load().widgets.length, 1);
});

test('charts average several rows at one time into one point', () => {
  // Drawing needs a DOM; check the averaging through detect + a tiny fake element instead.
  const charts = require('../public/charts');
  const rows = [
    {bucket: '2026-10-14T00:40:00Z', cpu: 2}, {bucket: '2026-10-14T00:40:00Z', cpu: 4},
    {bucket: '2026-10-14T00:41:00Z', cpu: 6},
  ];
  assert.deepEqual(charts.detect(rows), {x: 'bucket', ys: ['cpu']});
});

test('the Ask page keeps statements that start with a line break', () => {
  const src = fs.readFileSync(path.join(__dirname, '..', 'public', 'chat.js'), 'utf8');
  const isSql = eval(src.match(/const isSql = (s => .*?);\n/)[1]);
  assert.equal(isSql('\n    SELECT 1'), true);
  assert.equal(isSql('Here is a dial showing the wind.'), false);
  const shortError = eval(src.match(/const shortError = (text => .*?);\n/)[1]);
  assert.equal(shortError('No field named "x".\nValid fields are a, b'), 'No field named "x". (the Schema Browser lists every column)');
});

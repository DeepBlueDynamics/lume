'use strict';

const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const http = require('node:http');
const os = require('node:os');
const path = require('node:path');

const {
  ShivvrMonitor,
  probeShivvr,
  probeInternet,
  DEFAULT_SHIVVR_URL,
  INSTALL_README_URL,
  INSTALL_DOCKER_CPU,
  INSTALL_DOCKER_GPU,
} = require('../lib/shivvr');
const pluginFactory = require('../index');

function createServer(handler) {
  const server = http.createServer(handler);
  return new Promise(resolve => {
    server.listen(0, '127.0.0.1', () => {
      const {port} = server.address();
      resolve({
        url: `http://127.0.0.1:${port}`,
        close: () => new Promise(r => server.close(r)),
      });
    });
  });
}

const SAMPLE_HEALTH = {
  status: 'ok',
  version: '0.6.0',
  default_model: 'embeddinggemma-2',
  models: [
    {name: 'gtr-t5-base', role: 'organize', dimension: 768, status: 'active'},
    {name: 'embeddinggemma-2', role: 'text', dimension: 768, status: 'active'},
  ],
  gpu: true,
};

test('probeShivvr reports online when /health is ok and required model is active', async () => {
  const srv = await createServer((req, res) => {
    assert.equal(req.url, '/health');
    res.writeHead(200, {'Content-Type': 'application/json'});
    res.end(JSON.stringify(SAMPLE_HEALTH));
  });
  try {
    const res = await probeShivvr(srv.url);
    assert.equal(res.state, 'online');
    assert.equal(res.running, true);
    assert.equal(res.version, '0.6.0');
    assert.equal(res.default_model, 'embeddinggemma-2');
    assert.equal(res.gpu, true);
    assert.equal(res.models.length, 2);
    assert.equal(res.error, null);
  } finally {
    await srv.close();
  }
});

test('probeShivvr reports degraded when Shivvr is up but required model is missing or inactive', async () => {
  const srv = await createServer((req, res) => {
    res.writeHead(200, {'Content-Type': 'application/json'});
    res.end(JSON.stringify({
      status: 'ok',
      version: '0.6.0',
      default_model: 'gtr-t5-base',
      models: [{name: 'gtr-t5-base', role: 'organize', dimension: 768, status: 'active'}],
      gpu: false,
    }));
  });
  try {
    const res = await probeShivvr(srv.url, {requiredModel: 'embeddinggemma-2'});
    assert.equal(res.state, 'degraded');
    assert.equal(res.running, true);
    assert.equal(res.gpu, false);
    assert.match(res.error, /embeddinggemma-2.*not active/);
  } finally {
    await srv.close();
  }
});

test('probeShivvr reports offline on connection error, HTTP error, invalid JSON, or timeout', async () => {
  const dead = await probeShivvr('http://127.0.0.1:9', {timeoutMs: 300});
  assert.equal(dead.state, 'offline');
  assert.equal(dead.running, false);
  assert.ok(dead.error);

  const errSrv = await createServer((req, res) => {
    res.writeHead(503);
    res.end('unavailable');
  });
  try {
    const res503 = await probeShivvr(errSrv.url);
    assert.equal(res503.state, 'offline');
    assert.equal(res503.running, false);
    assert.equal(res503.error, 'HTTP 503');
  } finally {
    await errSrv.close();
  }

  const badJsonSrv = await createServer((req, res) => {
    res.writeHead(200);
    res.end('not-json');
  });
  try {
    const bad = await probeShivvr(badJsonSrv.url);
    assert.equal(bad.state, 'offline');
    assert.equal(bad.running, false);
    assert.match(bad.error, /Invalid JSON/);
  } finally {
    await badJsonSrv.close();
  }

  const slowSrv = await createServer((_req, _res) => {
    // Never respond so timeout triggers.
  });
  try {
    const timedOut = await probeShivvr(slowSrv.url, {timeoutMs: 80});
    assert.equal(timedOut.state, 'offline');
    assert.equal(timedOut.running, false);
    assert.match(timedOut.error, /timed out/i);
  } finally {
    await slowSrv.close();
  }
});

test('ShivvrMonitor keeps Shivvr running and Internet online as independent lights', async () => {
  const shivvrSrv = await createServer((req, res) => {
    res.writeHead(200, {'Content-Type': 'application/json'});
    res.end(JSON.stringify(SAMPLE_HEALTH));
  });
  const netSrv = await createServer((req, res) => {
    assert.equal(req.method, 'HEAD');
    res.writeHead(200);
    res.end();
  });

  try {
    // 1. At sea: local Shivvr is running, internet is unreachable.
    const atSea = new ShivvrMonitor({
      shivvrUrl: shivvrSrv.url,
      internetUrl: 'http://127.0.0.1:9',
      intervalMs: 0,
      timeoutMs: 300,
    });
    const seaSnap = await atSea.probe();
    assert.equal(seaSnap.state, 'online');
    assert.equal(seaSnap.running, true);
    assert.equal(seaSnap.online, false);
    assert.ok(seaSnap.checked_at);

    // 2. In port with internet, but local Shivvr stopped.
    const shivvrDown = new ShivvrMonitor({
      shivvrUrl: 'http://127.0.0.1:9',
      internetUrl: netSrv.url,
      intervalMs: 0,
      timeoutMs: 300,
    });
    const downSnap = await shivvrDown.probe();
    assert.equal(downSnap.state, 'offline');
    assert.equal(downSnap.running, false);
    assert.equal(downSnap.online, true);

    // 3. Both local Shivvr and internet online.
    const bothUp = new ShivvrMonitor({
      shivvrUrl: shivvrSrv.url,
      internetUrl: netSrv.url,
      intervalMs: 0,
      timeoutMs: 300,
    });
    const upSnap = await bothUp.probe();
    assert.equal(upSnap.state, 'online');
    assert.equal(upSnap.running, true);
    assert.equal(upSnap.online, true);
  } finally {
    await shivvrSrv.close();
    await netSrv.close();
  }
});

test('probeInternet uses HEAD and returns true on 2xx/3xx/4xx and false on 5xx or unreachable', async () => {
  const redirectSrv = await createServer((req, res) => {
    assert.equal(req.method, 'HEAD');
    res.writeHead(301, {Location: 'https://ollama.com/'});
    res.end();
  });
  const failSrv = await createServer((req, res) => {
    res.writeHead(502);
    res.end();
  });
  try {
    assert.equal(await probeInternet(redirectSrv.url, {timeoutMs: 500}), true);
    assert.equal(await probeInternet(failSrv.url, {timeoutMs: 500}), false);
    assert.equal(await probeInternet('http://127.0.0.1:9', {timeoutMs: 200}), false);
  } finally {
    await redirectSrv.close();
    await failSrv.close();
  }
});

test('Plugin schema exposes shivvrUrl and router exposes /api/shivvr/status and /api/status.shivvr', async () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'lume-shivvr-plugin-'));
  const plugin = pluginFactory({
    getDataDirPath: () => tmpDir,
    debug: () => {},
    setPluginStatus: () => {},
    setPluginError: () => {},
  });
  const schema = plugin.schema();
  assert.ok(schema.properties.shivvrUrl);
  assert.equal(schema.properties.shivvrUrl.default, DEFAULT_SHIVVR_URL);

  const routes = {GET: {}, POST: {}};
  plugin.registerWithRouter({
    get: (p, h) => { routes.GET[p] = h; },
    post: (p, h) => { routes.POST[p] = h; },
  });
  assert.equal(typeof routes.GET['/api/shivvr/status'], 'function');

  let shivvrPayload = null;
  await routes.GET['/api/shivvr/status']({query: {}}, {json: data => { shivvrPayload = data; }});
  assert.equal(shivvrPayload.state, 'offline');
  assert.equal(shivvrPayload.running, false);

  let statusPayload = null;
  routes.GET['/api/status']({}, {json: data => { statusPayload = data; }});
  assert.ok(statusPayload.shivvr);
  assert.equal(statusPayload.shivvr.state, 'offline');

  fs.rmSync(tmpDir, {recursive: true, force: true});
});

test('UI markup and updateShivvrStatus wire header pill, Status card, install commands, and search badges', () => {
  const html = fs.readFileSync(path.join(__dirname, '..', 'public', 'index.html'), 'utf8');
  assert.match(html, /id="shivvr-pill"/);
  assert.match(html, /id="shivvr-card"/);
  assert.match(html, /id="diag-shivvr-running"/);
  assert.match(html, /id="diag-internet-online"/);
  assert.match(html, /id="shivvr-install-box"/);
  assert.ok(html.includes(INSTALL_README_URL), 'Install link points to README #shivvr-setup-semantic-search');
  assert.ok(html.includes(INSTALL_DOCKER_CPU.replace(/&/g, '&amp;')), 'CPU docker run command is in install box');
  assert.ok(html.includes(INSTALL_DOCKER_GPU), 'GPU docker run command is in install box');
  assert.match(html, /id="lib-search-mode"/);
  assert.match(html, /id="dash-search-mode"/);

  const appSrc = fs.readFileSync(path.join(__dirname, '..', 'public', 'app.js'), 'utf8');
  const fnMatch = appSrc.match(/function updateShivvrStatus\(shivvr\) \{[\s\S]*?\n  \}/);
  assert.ok(fnMatch, 'updateShivvrStatus found in public/app.js');

  const elements = {};
  for (const id of [
    'shivvr-pill', 'diag-shivvr-running', 'diag-shivvr-state-text',
    'diag-internet-online', 'diag-internet-text', 'diag-shivvr-url',
    'diag-shivvr-version', 'diag-shivvr-models', 'diag-shivvr-accel',
    'diag-shivvr-checked', 'shivvr-install-box', 'shivvr-halos-link-wrap',
    'shivvr-halos-link', 'lib-search-mode', 'dash-search-mode',
  ]) {
    elements[id] = {id, className: '', textContent: '', hidden: false, href: '', title: ''};
  }
  const fakeDoc = {getElementById: id => elements[id] || null};
  const setElem = (id, val) => { if (elements[id]) elements[id].textContent = String(val); };
  const runUpdate = new Function('document', 'setElem', 'location', 'window', `return (${fnMatch[0]});`)(
    fakeDoc, setElem, {hostname: 'halos.local', port: '4430'}, {}
  );

  // Offline state: install box visible, badges say keyword only, pill is grey.
  runUpdate({state: 'offline', running: false, online: true, url: 'http://127.0.0.1:8085'});
  assert.equal(elements['shivvr-pill'].className, 'badge shivvr-pill offline-grey');
  assert.equal(elements['diag-shivvr-state-text'].textContent, 'stopped');
  assert.equal(elements['diag-internet-text'].textContent, 'online');
  assert.equal(elements['shivvr-install-box'].hidden, false);
  assert.equal(elements['shivvr-halos-link-wrap'].hidden, false);
  assert.equal(elements['lib-search-mode'].textContent, 'keyword only');
  assert.equal(elements['dash-search-mode'].textContent, 'keyword only');

  // Online state at sea (internet offline): install box hidden, badges say meaning search on, pill is green.
  runUpdate({
    state: 'online',
    running: true,
    online: false,
    url: 'http://127.0.0.1:8085',
    version: '0.6.0',
    models: [{name: 'embeddinggemma-2', status: 'active'}, {name: 'gtr-t5-base', status: 'active'}],
    gpu: true,
    checked_at: '2026-10-14T23:00:00Z',
  });
  assert.equal(elements['shivvr-pill'].className, 'badge shivvr-pill online');
  assert.equal(elements['diag-shivvr-state-text'].textContent, 'running');
  assert.equal(elements['diag-internet-text'].textContent, 'offline');
  assert.equal(elements['diag-shivvr-version'].textContent, '0.6.0');
  assert.equal(elements['diag-shivvr-models'].textContent, 'embeddinggemma-2, gtr-t5-base');
  assert.equal(elements['diag-shivvr-accel'].textContent, 'GPU');
  assert.equal(elements['shivvr-install-box'].hidden, true);
  assert.equal(elements['lib-search-mode'].textContent, 'meaning search on');
  assert.equal(elements['dash-search-mode'].textContent, 'meaning search on');
});

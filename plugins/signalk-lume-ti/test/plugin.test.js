'use strict';

const test = require('node:test');
const assert = require('node:assert');
const fs = require('fs');
const path = require('path');
const os = require('os');
const http = require('http');

const pluginFactory = require('../index');
const mockLumeBin = path.join(__dirname, 'stubs', 'mock-lume.js');

test('Plugin lifecycle, supervision, status, and router proxying', async () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'lume-plugin-test-'));

  const statusUpdates = [];
  const debugLogs = [];

  const mockApp = {
    getDataDirPath: () => tmpDir,
    debug: (msg) => debugLogs.push(msg),
    setPluginStatus: (msg) => statusUpdates.push(msg),
    setPluginError: (msg) => statusUpdates.push(`ERROR: ${msg}`),
  };

  const plugin = pluginFactory(mockApp);
  assert.strictEqual(plugin.id, 'signalk-lume-ti');
  assert.strictEqual(typeof plugin.schema, 'function');

  // Router route store for testing
  const routes = { GET: {}, POST: {} };
  const mockRouter = {
    get: (routePath, handler) => { routes.GET[routePath] = handler; },
    post: (routePath, handler) => { routes.POST[routePath] = handler; },
  };

  plugin.registerWithRouter(mockRouter);
  assert.ok(routes.GET['/api/status'], 'Registered GET /api/status');
  assert.ok(routes.POST['/api/query'], 'Registered POST /api/query');
  assert.ok(routes.GET['/api/schema'], 'Registered GET /api/schema');

  // Start plugin
  const servePort = 5897;
  plugin.start({
    lumePath: mockLumeBin,
    servePort,
    signalkUrl: 'ws://127.0.0.1:3000',
    autoRequestToken: false,
  });

  // Give child time to boot and start HTTP mock
  await new Promise((r) => setTimeout(r, 600));

  // 1. Verify status updates
  assert.ok(statusUpdates.length > 0, 'Status updates were sent to Signal K');
  const latestStatus = statusUpdates[statusUpdates.length - 1];
  assert.ok(latestStatus.includes('Running'), `Status should contain 'Running', got: ${latestStatus}`);

  // 2. Test GET /api/status route
  {
    const req = {};
    let responseData = null;
    const res = {
      json: (data) => { responseData = data; },
    };
    routes.GET['/api/status'](req, res);

    assert.ok(responseData, 'Returned JSON status');
    assert.strictEqual(responseData.ok, true);
    assert.strictEqual(responseData.supervisor.running, true);
    assert.strictEqual(responseData.supervisor.servePort, servePort);
    assert.ok(responseData.store.exists, 'Store directory exists');
  }

  // 3. Test POST /api/query route (proxied to mock lume)
  {
    let statusCode = 0;
    let headers = {};
    let bodyData = '';

    const req = {
      headers: { 'content-type': 'application/json' },
      body: { sql: 'SELECT * FROM telemetry LIMIT 2' },
      on: () => {},
    };

    const res = {
      status: (code) => { statusCode = code; return res; },
      setHeader: (k, v) => { headers[k] = v; },
      write: (chunk) => { bodyData += chunk; },
      end: (chunk) => { if (chunk) bodyData += chunk; },
    };

    // Use pipe mechanism
    const EventEmitter = require('events');
    const proxyMockRes = new EventEmitter();
    proxyMockRes.pipe = (destination) => {
      destination.status(200);
      destination.write(JSON.stringify([{ ts: '2026-06-01T00:00:00Z', 'navigation.speedOverGround': 5.2 }]));
      destination.end();
    };

    // Invoke router query handler through an actual local HTTP request to avoid mock complexity
    const testServer = http.createServer((sReq, sRes) => {
      sRes.status = (c) => { sRes.statusCode = c; return sRes; };
      sRes.json = (data) => {
        sRes.setHeader('Content-Type', 'application/json');
        sRes.end(JSON.stringify(data));
      };
      if (sReq.url === '/api/query') {
        routes.POST['/api/query'](sReq, sRes);
      } else if (sReq.url === '/api/schema') {
        routes.GET['/api/schema'](sReq, sRes);
      }
    });

    await new Promise((r) => testServer.listen(0, '127.0.0.1', r));
    const testPort = testServer.address().port;

    // Send HTTP POST /api/query
    const queryRes = await httpFetch(`http://127.0.0.1:${testPort}/api/query`, 'POST', JSON.stringify({ sql: 'SELECT 1' }));
    assert.strictEqual(queryRes.statusCode, 200);
    const parsedQuery = JSON.parse(queryRes.body);
    assert.ok(Array.isArray(parsedQuery));
    assert.strictEqual(parsedQuery.length, 2);
    assert.strictEqual(parsedQuery[0]['navigation.speedOverGround'], 5.2);

    // Send HTTP GET /api/schema
    const schemaRes = await httpFetch(`http://127.0.0.1:${testPort}/api/schema`, 'GET');
    assert.strictEqual(schemaRes.statusCode, 200);
    const parsedSchema = JSON.parse(schemaRes.body);
    assert.ok(parsedSchema.tables);
    assert.strictEqual(parsedSchema.tables[0].name, 'telemetry');

    await new Promise((r) => testServer.close(r));
  }

  // 4. Stop plugin
  plugin.stop();
  await new Promise((r) => setTimeout(r, 400));

  const stoppedStatus = statusUpdates[statusUpdates.length - 1];
  assert.ok(stoppedStatus.includes('Stopped'), `Status should contain 'Stopped', got: ${stoppedStatus}`);

  fs.rmSync(tmpDir, { recursive: true, force: true });
});

function httpFetch(urlStr, method, body) {
  const { URL } = require('url');
  const u = new URL(urlStr);
  return new Promise((resolve, reject) => {
    const req = http.request({
      hostname: u.hostname,
      port: u.port,
      path: u.pathname,
      method: method,
      headers: {
        'Content-Type': 'application/json',
        'Content-Length': body ? Buffer.byteLength(body) : 0,
      },
    }, (res) => {
      let b = '';
      res.on('data', (c) => { b += c; });
      res.on('end', () => resolve({ statusCode: res.statusCode, body: b }));
    });
    req.on('error', reject);
    if (body) req.write(body);
    req.end();
  });
}

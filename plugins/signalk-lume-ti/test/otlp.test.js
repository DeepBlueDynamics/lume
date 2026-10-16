'use strict';

const test = require('node:test');
const assert = require('node:assert');
const fs = require('fs');
const path = require('path');
const os = require('os');

const { withNodeStub } = require('./stubs/node-stub');
const { validateBind, isLoopback } = require('../lib/supervisor');
const pluginFactory = withNodeStub(() => require('../index'));

test('isLoopback and validateBind mirror src/ti_otlp.rs', () => {
  // Loopback IPv4 addresses
  assert.strictEqual(isLoopback('127.0.0.1'), true);
  assert.strictEqual(isLoopback('127.0.0.2'), true);
  assert.strictEqual(isLoopback('127.1.2.3'), true);
  // Loopback IPv6
  assert.strictEqual(isLoopback('::1'), true);

  // Non-loopback
  assert.strictEqual(isLoopback('192.168.1.1'), false);
  assert.strictEqual(isLoopback('10.0.0.1'), false);
  assert.strictEqual(isLoopback('172.17.0.1'), false);
  assert.strictEqual(isLoopback('0.0.0.0'), false);
  assert.strictEqual(isLoopback('::'), false);
  assert.strictEqual(isLoopback('localhost'), false);
  assert.strictEqual(isLoopback(''), false);
  assert.strictEqual(isLoopback(null), false);

  // validateBind tests matching ti_otlp.rs explicit_non_loopback_requires_auth_without_binding_any_socket
  assert.strictEqual(validateBind('127.0.0.1', null), null);
  assert.strictEqual(validateBind('127.0.0.2', null), null);
  assert.strictEqual(validateBind('::1', null), null);
  assert.strictEqual(validateBind('192.0.2.1', null), 'Non-loopback OTLP requires --otlp-token-file');
  assert.strictEqual(validateBind('192.0.2.1', '/path/to/token'), null);
  assert.strictEqual(validateBind('localhost', null), 'OTLP bind must be an IP address');
  assert.strictEqual(validateBind('0.0.0.0', null), 'Non-loopback OTLP requires --otlp-token-file');
  assert.strictEqual(validateBind('0.0.0.0', '/path/to/token'), null);
});

test('Plugin schema includes otlpEnabled and otlpTokenFile defaults', () => {
  const plugin = pluginFactory({});
  const schema = plugin.schema();
  assert.ok(schema.properties.otlpEnabled, 'otlpEnabled in schema');
  assert.strictEqual(schema.properties.otlpEnabled.type, 'boolean');
  assert.strictEqual(schema.properties.otlpEnabled.default, false);
  assert.strictEqual(schema.properties.otlpEnabled.title, 'Accept radio and agent logs from hertz (OTLP)');

  assert.ok(schema.properties.otlpTokenFile, 'otlpTokenFile in schema');
  assert.strictEqual(schema.properties.otlpTokenFile.type, 'string');
  assert.strictEqual(schema.properties.otlpTokenFile.default, '');
});

test('Plugin ignores a serveBind in config: query server stays on loopback', async () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'lume-plugin-otlp-err-'));
  const errors = [];
  const debugLogs = [];

  const mockApp = {
    getDataDirPath: () => tmpDir,
    debug: (msg) => debugLogs.push(msg),
    setPluginStatus: () => {},
    setPluginError: (msg) => errors.push(msg),
  };

  const plugin = pluginFactory(mockApp);
  plugin.start({
    lumePath: process.execPath,
    serveBind: '192.168.1.50',
    otlpEnabled: true,
    otlpTokenFile: '',
    autoRequestToken: false,
  });

  // serveBind in plugin config is ignored: the query server is always loopback.
  assert.strictEqual(errors.length, 0, 'A config serveBind must not take effect');

  plugin.stop();
  fs.rmSync(tmpDir, { recursive: true, force: true });
});

test('Plugin start with valid loopback OTLP does not report config error', async () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'lume-plugin-otlp-ok-'));
  const errors = [];

  const mockApp = {
    getDataDirPath: () => tmpDir,
    debug: () => {},
    setPluginStatus: () => {},
    setPluginError: (msg) => errors.push(msg),
  };

  const plugin = pluginFactory(mockApp);
  plugin.start({
    lumePath: process.execPath,
    serveBind: '127.0.0.1',
    otlpEnabled: true,
    autoRequestToken: false,
  });

  assert.strictEqual(errors.length, 0, 'Should not record plugin error on loopback');

  plugin.stop();
  fs.rmSync(tmpDir, { recursive: true, force: true });
});

test('OTLP enabled creates 0600 token file, passes file paths only on argv, and omits flags when disabled', async () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'lume-plugin-otlp-token-'));
  const logs = [];
  const routes = {};
  const mockRouter = {
    get: (route, handler) => { routes[`GET ${route}`] = handler; },
    post: (route, handler) => { routes[`POST ${route}`] = handler; },
    delete: (route, handler) => { routes[`DELETE ${route}`] = handler; },
  };
  const mockApp = {
    getDataDirPath: () => tmpDir,
    debug: (msg) => logs.push(msg),
    setPluginStatus: () => {},
    setPluginError: () => {},
  };

  // 1. Disabled: no otlp.token created, no OTLP/discovery flags in args
  const pluginOff = pluginFactory(mockApp);
  pluginOff.start({
    lumePath: process.execPath,
    otlpEnabled: false,
    autoRequestToken: false,
    serverMode: 'external',
  });
  pluginOff.registerWithRouter(mockRouter);
  assert.strictEqual(fs.existsSync(path.join(tmpDir, 'otlp.token')), false, 'No token file created when disabled');
  const offArgs = fs.readFileSync(path.join(tmpDir, 'lume-ti', 'server.args'), 'utf8').trim().split('\n');
  assert.strictEqual(offArgs.includes('--otlp'), false);
  assert.strictEqual(offArgs.includes('--otlp-token-file'), false);
  assert.strictEqual(offArgs.includes('--discovery-file'), false);
  let offStatus = null;
  routes['GET /api/status']({}, { json: (obj) => { offStatus = obj; } });
  assert.strictEqual(offStatus.otlp.enabled, false);
  assert.strictEqual(offStatus.otlp.tokenFile, null);
  pluginOff.stop();

  // 2. Enabled: creates 32-byte hex otlp.token with mode 0600, passes --otlp, --otlp-token-file, --discovery-file, and never logs or passes token value
  const pluginOn = pluginFactory(mockApp);
  pluginOn.start({
    lumePath: process.execPath,
    otlpEnabled: true,
    autoRequestToken: false,
    serverMode: 'external',
  });
  pluginOn.registerWithRouter(mockRouter);
  const expectedTokenPath = path.join(tmpDir, 'otlp.token');
  const expectedDiscoveryPath = path.join(tmpDir, 'serve.json');
  assert.strictEqual(fs.existsSync(expectedTokenPath), true, 'otlp.token created when enabled');
  const tokenValue = fs.readFileSync(expectedTokenPath, 'utf8').trim();
  assert.match(tokenValue, /^[0-9a-f]{64}$/, '32 random bytes hex (64 hex chars)');
  if (process.platform !== 'win32') {
    const mode = fs.statSync(expectedTokenPath).mode & 0o777;
    assert.strictEqual(mode, 0o600, 'otlp.token must be mode 0600');
  }

  const onArgs = fs.readFileSync(path.join(tmpDir, 'lume-ti', 'server.args'), 'utf8').trim().split('\n');
  assert.strictEqual(onArgs.includes('--otlp'), true);
  const tokIdx = onArgs.indexOf('--otlp-token-file');
  assert.ok(tokIdx !== -1, '--otlp-token-file present');
  assert.strictEqual(onArgs[tokIdx + 1], expectedTokenPath);
  const discIdx = onArgs.indexOf('--discovery-file');
  assert.ok(discIdx !== -1, '--discovery-file present');
  assert.strictEqual(onArgs[discIdx + 1], expectedDiscoveryPath);
  assert.strictEqual(onArgs.some((a) => a.includes(tokenValue)), false, 'argv must never contain the token string');
  assert.strictEqual(logs.some((l) => l.includes(tokenValue)), false, 'logs must never contain the token string');

  let onStatus = null;
  routes['GET /api/status']({}, { json: (obj) => { onStatus = obj; } });
  assert.strictEqual(onStatus.otlp.enabled, true);
  assert.strictEqual(onStatus.otlp.tokenFile, expectedTokenPath);
  assert.strictEqual(JSON.stringify(onStatus).includes(tokenValue), false, '/api/status must never leak the token string');

  const html = fs.readFileSync(path.join(__dirname, '..', 'public', 'index.html'), 'utf8');
  assert.ok(html.includes('OTLP receiver:'), 'Status tab shows OTLP receiver label');
  assert.ok(html.includes('diag-otlp-state'), 'Status tab has diag-otlp-state element');
  assert.ok(html.includes('diag-otlp-token-file'), 'Status tab has diag-otlp-token-file element');

  pluginOn.stop();
  fs.rmSync(tmpDir, { recursive: true, force: true });
});

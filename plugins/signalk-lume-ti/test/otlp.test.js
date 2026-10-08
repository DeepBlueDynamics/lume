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

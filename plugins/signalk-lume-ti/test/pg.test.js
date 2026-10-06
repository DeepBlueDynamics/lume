'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const http = require('node:http');
const {deriveVerifier, pgOptions, writePgConfig, registerPgRoutes} = require('../lib/pg');
const {withNodeStub, waitFor} = require('./stubs/node-stub');
const {Supervisor} = withNodeStub(() => require('../lib/supervisor'));
const pluginFactory = require('../index');
const EXPECTED = 'SCRAM-SHA-256$4096:W22ZaJ0SNY7soEsUEjb6gQ==$WG5d8oPm3OtcPnkdi4Uo7BkeZkBFzpcXkuLmtbsT4qY=:wfPLwcE6nTWhTAmQ7tl2KeoiWGPlZqQxSrmfPwDl2dU=';
test('Node derives the PostgreSQL SCRAM verifier; production salts are random', async () => {
  assert.equal(await deriveVerifier('pencil', Buffer.from('W22ZaJ0SNY7soEsUEjb6gQ==', 'base64')), EXPECTED);
  assert.notEqual(await deriveVerifier('pencil'), await deriveVerifier('pencil'));
  await assert.rejects(deriveVerifier('non-ascii-é'));
});
test('PG defaults disabled; options map only to pg args, auth file is private/verifier-only', async () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'pg-options-'));
  try {
    assert.equal(pgOptions({}).enablePg, false);
    assert.equal(pgOptions({}).pgPort, 5864);
    assert.equal(pgOptions({}).pgUser, 'grafana');
    assert.equal(pgOptions({}).pgBind, '127.0.0.1');
    const original = 'width_seconds=10\n[signal_k]\nurl="ws://127.0.0.1:1234"\n';
    fs.mkdirSync(path.join(root, 'store'));
    fs.writeFileSync(path.join(root, 'store', 'ti.toml'), original);
    const config = {enablePg: true, pgPort: 5864, pgUser: 'grafana', pgBind: '127.0.0.2', pgVerifier: EXPECTED};
    const auth = writePgConfig(root, config);
    const text = fs.readFileSync(auth, 'utf8');
    assert.ok(text.includes(EXPECTED));
    assert.ok(!text.includes('pencil'));
    assert.equal(fs.readFileSync(path.join(root, 'store', 'ti.toml'), 'utf8'), original);
    if (process.platform !== 'win32') assert.equal(fs.statSync(auth).mode & 0o777, 0o600);
    const supervisor = new Supervisor({binaryPath: process.execPath, storeDir: path.join(root, 'store'),
      pgPort: config.pgPort, pgBind: config.pgBind, pgAuthConfig: auth});
    const args = supervisor.buildArgs();
    assert.deepEqual(args.slice(-6), ['--pg', '5864', '--pg-bind', '127.0.0.2', '--pg-auth-config', auth]);
    assert.equal(args[args.indexOf('--bind') + 1], '127.0.0.1');
    assert.ok(!args.join(' ').includes('pencil'));
    assert.ok(!args.join(' ').includes(EXPECTED));
    assert.ok(!new Supervisor({storeDir: root}).buildArgs().includes('--pg'));
    for (const invalid of [{pgPort: 5432.5}, {pgBind: '0.0.0.0'}, {pgBind: '::'}, {pgUser: "bad'user"}, {enablePg: true}]) {
      assert.throws(() => pgOptions(invalid));
    }
  } finally { fs.rmSync(root, {recursive: true, force: true}); }
});
test('admin JSON save hashes before persistence; readonly/anonymous, malformed/oversize fail closed', async () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'pg-save-'));
  let config = {};
  let restartCount = 0;
  let writes = 0;
  let dummy = false;
  const routes = {};
  const app = {
    securityStrategy: {isDummy: () => dummy, allowConfigure: req => req.skIsAuthenticated === true && req.skPrincipal?.permissions === 'admin'},
    savePluginOptions: (safe, cb) => {
      writes++;
      assert.ok(!JSON.stringify(safe).includes('SUPER_SECRET_PASSWORD'));
      fs.writeFileSync(path.join(root, 'options.json'), JSON.stringify(safe));
      config = safe;
      cb(null);
    },
  };
  registerPgRoutes({get: (p, h) => {routes['GET ' + p] = h;}, post: (p, h) => {routes['POST ' + p] = h;}},
    app, () => config, () => {restartCount++; writePgConfig(root, config);});
  const server = http.createServer((req, res) => {
    // Mock Signal K middleware, not production authorization via request headers.
    const role = req.headers['x-test-principal'];
    if (role) {req.skIsAuthenticated = true; req.skPrincipal = {identifier: 'test', permissions: role};}
    res.status = code => {res.statusCode = code; return res;};
    res.json = value => {res.setHeader('Content-Type', 'application/json'); res.end(JSON.stringify(value));};
    const handler = routes[req.method + ' ' + req.url];
    if (handler) Promise.resolve(handler(req, res)).catch(() => {res.statusCode = 500; res.end('internal');});
    else {res.statusCode = 404; res.end();}
  });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  const url = 'http://127.0.0.1:' + server.address().port + '/api/pg/config';
  async function send(role, text, type = 'application/json') {
    const headers = {'Content-Type': type};
    if (role) headers['x-test-principal'] = role;
    const response = await fetch(url, {method: 'POST', headers, body: text});
    return {status: response.status, text: await response.text()};
  }
  const body = JSON.stringify({enablePg: true, pgPort: 5864, pgBind: '127.0.0.2',
    pgUser: 'grafana', pgPassword: 'SUPER_SECRET_PASSWORD'});
  try {
    assert.equal((await send(null, body)).status, 401);
    assert.equal((await send('readonly', body)).status, 403);
    assert.equal((await send('readwrite', body)).status, 403);
    assert.equal((await send('admin', body, 'text/plain')).status, 415);
    assert.equal((await send('admin', 'x'.repeat(4097))).status, 413);
    assert.equal((await send('admin', '{"pgPassword":"SUPER_SECRET_PASSWORD",')).status, 400);
    assert.equal(writes, 0);
    const saved = await send('admin', body);
    assert.equal(saved.status, 200);
    assert.ok(!saved.text.includes('SUPER_SECRET_PASSWORD'));
    assert.equal(config.pgPassword, '');
    assert.ok(config.pgVerifier.startsWith('SCRAM-SHA-256$4096:'));
    assert.equal(restartCount, 1);
    for (const file of fs.readdirSync(root)) assert.ok(!fs.readFileSync(path.join(root, file), 'utf8').includes('SUPER_SECRET_PASSWORD'));
    const verifier = config.pgVerifier;
    assert.equal((await send('admin', JSON.stringify({pgPassword: '', pgPort: 5865}))).status, 200);
    assert.equal(config.pgVerifier, verifier);
    assert.equal(config.pgPort, 5865);
    const response = await fetch(url, {headers: {'x-test-principal': 'admin'}});
    const visible = await response.json();
    assert.equal(visible.pgPassword, '');
    assert.equal(visible.passwordConfigured, true);
    assert.ok(!JSON.stringify(visible).includes(verifier));
    dummy = true;
    assert.equal((await send(null, JSON.stringify({enablePg: false}))).status, 200);
  } finally {
    await new Promise(resolve => server.close(resolve));
    fs.rmSync(root, {recursive: true, force: true});
  }
});

test('plugin saved safe options become supervised pg flags while HTTP stays loopback', async () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'pg-plugin-'));
  const argsLog = path.join(root, 'args.json');
  process.env.MOCK_LUME_LOG_ARGS = argsLog;
  const config = {enablePg: true, pgPort: 5864, pgUser: 'grafana', pgBind: '127.0.0.2',
    pgVerifier: EXPECTED, lumePath: process.execPath, servePort: 0, autoRequestToken: false};
  const plugin = pluginFactory({getDataDirPath: () => root, debug: () => {}});
  try {
    assert.ok(!Object.hasOwn(plugin.schema().properties, 'pgPassword'));
    plugin.start(config);
    await waitFor(() => fs.existsSync(argsLog));
    const args = JSON.parse(fs.readFileSync(argsLog, 'utf8'));
    assert.equal(args[args.indexOf('--pg') + 1], '5864');
    assert.equal(args[args.indexOf('--pg-bind') + 1], '127.0.0.2');
    assert.equal(args[args.indexOf('--bind') + 1], '127.0.0.1');
    assert.equal(args[args.indexOf('--pg-auth-config') + 1], path.join(root, 'ti.toml'));
    assert.ok(!args.includes('--config'));
  } finally {
    plugin.stop();
    // Wait for stop before deleting the child fixture's directory.
    await new Promise(resolve => setTimeout(resolve, 100));
    delete process.env.MOCK_LUME_LOG_ARGS;
    fs.rmSync(root, {recursive: true, force: true});
  }
});

'use strict';

const test = require('node:test');
const assert = require('node:assert');
const fs = require('fs');
const path = require('path');
const os = require('os');

const {withNodeStub, waitFor} = require('./stubs/node-stub');
const { Supervisor } = withNodeStub(() => require('../lib/supervisor'));

const mockLumeBin = process.execPath;

test('Supervisor launches process with correct arguments', async () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'lume-sup-test-'));
  const storeDir = path.join(tmpDir, 'store');
  const tokenFile = path.join(tmpDir, 'token.txt');
  const argsLog = path.join(tmpDir, 'args.json');

  fs.writeFileSync(tokenFile, 'secret-jwt-token-12345', 'utf8');
  process.env.MOCK_LUME_LOG_ARGS = argsLog;

  const logs = [];
  const supervisor = new Supervisor({
    binaryPath: mockLumeBin,
    signalkUrl: 'ws://127.0.0.1:3000',
    storeDir,
    servePort: 5899,
    serveBind: '127.0.0.1',
    tokenPath: tokenFile,
    onLog: (line) => logs.push(line),
  });

  const started = supervisor.start();
  assert.strictEqual(started, true, 'Supervisor should start');
  assert.strictEqual(supervisor.running, true, 'Supervisor should be running');
  assert.ok(supervisor.child.pid > 0, 'Child PID should be positive');

  // Allow child to write args log
  await waitFor(() => fs.existsSync(argsLog));

  assert.ok(fs.existsSync(argsLog), 'Child wrote args.json');
  const receivedArgs = JSON.parse(fs.readFileSync(argsLog, 'utf8'));

  assert.deepStrictEqual(receivedArgs, [
    'ti',
    'ingest',
    '--signalk', 'ws://127.0.0.1:3000',
    '--store', storeDir,
    '--serve',
    '--port', '5899',
    '--bind', '127.0.0.1',
    '--token', tokenFile,
  ]);

  // Clean shutdown via SIGTERM
  await supervisor.stop();
  assert.strictEqual(supervisor.running, false, 'Supervisor stopped');

  // Verify status
  const st = supervisor.getStatus();
  assert.strictEqual(st.running, false);
  assert.strictEqual(st.pid, null);

  // Cleanup
  delete process.env.MOCK_LUME_LOG_ARGS;
  fs.rmSync(tmpDir, { recursive: true, force: true });
});

test('Supervisor restarts on crash with backoff', async () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'lume-sup-crash-'));
  const storeDir = path.join(tmpDir, 'store');

  process.env.MOCK_LUME_CRASH = '1';

  let restartDetected = false;
  const supervisor = new Supervisor({
    binaryPath: mockLumeBin,
    signalkUrl: 'ws://127.0.0.1:3000',
    storeDir,
    servePort: 5898,
    backoffInitialMs: 50,
    backoffMaxMs: 200,
    onLog: (line) => {
      if (line.includes('Scheduling restart #1')) {
        restartDetected = true;
      }
    },
  });

  supervisor.start();

  // Wait for child to exit with 42 and supervisor to schedule restart
  await waitFor(() => restartDetected);

  assert.strictEqual(restartDetected, true, 'Supervisor scheduled restart #1');
  assert.ok(supervisor.restarts >= 1, 'Restarts count incremented');
  assert.strictEqual(supervisor.lastExitCode, 42, 'Recorded last exit code');

  await supervisor.stop();

  delete process.env.MOCK_LUME_CRASH;
  fs.rmSync(tmpDir, { recursive: true, force: true });
});

test('Supervisor passes TLS options only when set', async () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'lume-sup-tls-'));
  const storeDir = path.join(tmpDir, 'store');
  const authFile = path.join(tmpDir, 'ti.toml');
  fs.writeFileSync(authFile, 'auth', 'utf8');

  // When unset, no TLS args
  const supNoTls = new Supervisor({
    binaryPath: mockLumeBin,
    signalkUrl: 'ws://127.0.0.1:3000',
    storeDir,
    servePort: 5898,
    pgPort: 5864,
    pgAuthConfig: authFile,
  });
  const noTlsArgs = supNoTls.buildArgs();
  assert.strictEqual(noTlsArgs.includes('--pg-require-tls'), false);
  assert.strictEqual(noTlsArgs.some(a => a.startsWith('--pg-require-tls')), false);
  assert.strictEqual(noTlsArgs.includes('--pg-tls-cert'), false);
  assert.strictEqual(noTlsArgs.includes('--pg-tls-key'), false);
  assert.strictEqual(noTlsArgs.includes('--pg-allow-plaintext'), false);

  // When set, passes respective flags
  const supTls = new Supervisor({
    binaryPath: mockLumeBin,
    signalkUrl: 'ws://127.0.0.1:3000',
    storeDir,
    servePort: 5898,
    pgPort: 5864,
    pgAuthConfig: authFile,
    pgRequireTls: true,
    pgTlsCert: '/path/to/cert.pem',
    pgTlsKey: '/path/to/key.pem',
    pgAllowPlaintext: true,
  });
  const tlsArgs = supTls.buildArgs();
  assert.strictEqual(tlsArgs.includes('--pg-require-tls'), true);
  assert.strictEqual(tlsArgs[tlsArgs.indexOf('--pg-tls-cert') + 1], '/path/to/cert.pem');
  assert.strictEqual(tlsArgs[tlsArgs.indexOf('--pg-tls-key') + 1], '/path/to/key.pem');
  assert.strictEqual(tlsArgs.includes('--pg-allow-plaintext'), true);

  fs.rmSync(tmpDir, { recursive: true, force: true });
});

test('Supervisor OTLP options and validation', () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'lume-sup-otlp-'));
  const storeDir = path.join(tmpDir, 'store');

  // 1. disabled means no --otlp
  const supDisabled = new Supervisor({
    binaryPath: mockLumeBin,
    signalkUrl: 'ws://127.0.0.1:3000',
    storeDir,
    otlpEnabled: false,
  });
  const disabledArgs = supDisabled.buildArgs();
  assert.strictEqual(disabledArgs.includes('--otlp'), false);
  assert.strictEqual(disabledArgs.includes('--otlp-token-file'), false);

  // Default options also means no --otlp
  const supDefault = new Supervisor({
    binaryPath: mockLumeBin,
    signalkUrl: 'ws://127.0.0.1:3000',
    storeDir,
  });
  assert.strictEqual(supDefault.buildArgs().includes('--otlp'), false);

  // 2. enabled adds --otlp
  const supEnabled = new Supervisor({
    binaryPath: mockLumeBin,
    signalkUrl: 'ws://127.0.0.1:3000',
    storeDir,
    otlpEnabled: true,
  });
  const enabledArgs = supEnabled.buildArgs();
  assert.strictEqual(enabledArgs.includes('--otlp'), true);
  assert.strictEqual(enabledArgs.includes('--otlp-token-file'), false);

  // 3. a token file adds the flag and path
  const supWithToken = new Supervisor({
    binaryPath: mockLumeBin,
    signalkUrl: 'ws://127.0.0.1:3000',
    storeDir,
    otlpEnabled: true,
    otlpTokenFile: '/etc/lume/otlp.token',
  });
  const tokenArgs = supWithToken.buildArgs();
  assert.strictEqual(tokenArgs.includes('--otlp'), true);
  const tokenIdx = tokenArgs.indexOf('--otlp-token-file');
  assert.ok(tokenIdx !== -1, '--otlp-token-file should be present');
  assert.strictEqual(tokenArgs[tokenIdx + 1], '/etc/lume/otlp.token');

  // 4. non-loopback without a token file gives the error
  const logs = [];
  const supNonLoopback = new Supervisor({
    binaryPath: mockLumeBin,
    signalkUrl: 'ws://127.0.0.1:3000',
    storeDir,
    serveBind: '192.168.1.100',
    otlpEnabled: true,
    onLog: (line, isErr) => logs.push({ line, isErr }),
  });
  const nonLoopbackArgs = supNonLoopback.buildArgs();
  assert.strictEqual(nonLoopbackArgs.includes('--otlp'), false);
  assert.strictEqual(nonLoopbackArgs.includes('--otlp-token-file'), false);
  assert.strictEqual(supNonLoopback.otlpError, 'Non-loopback OTLP requires --otlp-token-file');
  assert.ok(logs.some(l => l.isErr && l.line.includes('Non-loopback OTLP requires --otlp-token-file')));

  // non-loopback with a token file succeeds and adds both flags
  const supNonLoopbackWithToken = new Supervisor({
    binaryPath: mockLumeBin,
    signalkUrl: 'ws://127.0.0.1:3000',
    storeDir,
    serveBind: '192.168.1.100',
    otlpEnabled: true,
    otlpTokenFile: '/etc/lume/otlp.token',
  });
  const nonLoopbackTokenArgs = supNonLoopbackWithToken.buildArgs();
  assert.strictEqual(nonLoopbackTokenArgs.includes('--otlp'), true);
  assert.strictEqual(nonLoopbackTokenArgs.includes('--otlp-token-file'), true);
  assert.strictEqual(nonLoopbackTokenArgs[nonLoopbackTokenArgs.indexOf('--otlp-token-file') + 1], '/etc/lume/otlp.token');

  fs.rmSync(tmpDir, { recursive: true, force: true });
});

test('External mode writes the server arguments and spawns nothing', async () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'lume-sup-ext-'));
  const storeDir = path.join(tmpDir, 'store');
  const argsFile = path.join(storeDir, 'server.args');
  const tokenFile = path.join(tmpDir, 'token.txt');
  fs.writeFileSync(tokenFile, 'secret-jwt-token-12345', 'utf8');

  const supervisor = new Supervisor({
    binaryPath: path.join(tmpDir, 'does-not-exist'),
    signalkUrl: 'ws://127.0.0.1:3000',
    storeDir,
    servePort: 5899,
    tokenPath: tokenFile,
    otlpEnabled: true,
    external: true,
    argsFile,
  });

  assert.strictEqual(supervisor.start(), true);
  assert.strictEqual(supervisor.child, null, 'external mode must not spawn lume');
  assert.strictEqual(supervisor.getStatus().mode, 'external');
  const lines = fs.readFileSync(argsFile, 'utf8').trimEnd().split('\n');
  assert.deepStrictEqual(lines, supervisor.buildArgs());
  assert.deepStrictEqual(lines.slice(0, 4), ['ti', 'ingest', '--signalk', 'ws://127.0.0.1:3000']);
  assert.ok(lines.includes('--otlp'));
  assert.strictEqual(lines[lines.indexOf('--token') + 1], tokenFile, 'token path, never the token itself');
  assert.ok(!fs.readFileSync(argsFile, 'utf8').includes('secret-jwt-token'), 'token value stays out of the args file');
  assert.deepStrictEqual(fs.readdirSync(storeDir).filter(n => n.includes('.tmp-')), [], 'no temp file left behind');
  await supervisor.stop();
  assert.ok(fs.existsSync(argsFile), 'plugin stop leaves the container app running');
  fs.rmSync(tmpDir, {recursive: true, force: true});
});

test('External mode refuses arguments with line breaks', () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'lume-sup-ext-'));
  const storeDir = path.join(tmpDir, 'store');
  const argsFile = path.join(storeDir, 'server.args');
  const supervisor = new Supervisor({
    binaryPath: process.execPath, storeDir, external: true, argsFile,
    signalkUrl: 'ws://127.0.0.1:3000\n--bind\n0.0.0.0',
  });
  assert.strictEqual(supervisor.start(), false);
  assert.match(supervisor.getStatus().lastError, /line breaks/);
  assert.ok(!fs.existsSync(argsFile));
  fs.rmSync(tmpDir, {recursive: true, force: true});
});

test('Embedded mode removes a stale server.args so the container app idles', async () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'lume-sup-emb-'));
  const storeDir = path.join(tmpDir, 'store');
  const argsFile = path.join(storeDir, 'server.args');
  fs.mkdirSync(storeDir, {recursive: true});
  fs.writeFileSync(argsFile, 'ti\ningest\n');
  const supervisor = new Supervisor({binaryPath: mockLumeBin, storeDir, servePort: 5898, argsFile});
  assert.strictEqual(supervisor.start(), true);
  assert.ok(!fs.existsSync(argsFile));
  await supervisor.stop();
  fs.rmSync(tmpDir, {recursive: true, force: true});
});

'use strict';

const test = require('node:test');
const assert = require('node:assert');
const fs = require('fs');
const path = require('path');
const os = require('os');

const { Supervisor } = require('../lib/supervisor');

const mockLumeBin = path.join(__dirname, 'stubs', 'mock-lume.js');

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
  await new Promise((r) => setTimeout(r, 400));

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
  await new Promise((r) => setTimeout(r, 200));

  assert.strictEqual(restartDetected, true, 'Supervisor scheduled restart #1');
  assert.ok(supervisor.restarts >= 1, 'Restarts count incremented');
  assert.strictEqual(supervisor.lastExitCode, 42, 'Recorded last exit code');

  await supervisor.stop();

  delete process.env.MOCK_LUME_CRASH;
  fs.rmSync(tmpDir, { recursive: true, force: true });
});

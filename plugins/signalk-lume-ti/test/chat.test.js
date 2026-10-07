'use strict';

const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const {ChatManager, registerChatRoutes} = require('../lib/chat');

function createMockApp({isAdmin = true, isAuth = true} = {}) {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'lume-chat-test-'));
  return {
    tmpDir,
    securityStrategy: {
      isDummy: () => false,
      allowConfigure: req => req.skIsAuthenticated && req.skPrincipal?.permissions === 'admin',
    },
    getDataDirPath: () => tmpDir,
  };
}

test('POST /api/chat requires administrator authentication', async () => {
  const routes = {};
  const mockRouter = {
    post: (p, handler) => { routes[p] = handler; },
    get: (p, handler) => { routes[p] = handler; },
  };

  const app = createMockApp();
  const chatManager = new ChatManager({binary: process.execPath, dataDir: app.tmpDir});
  registerChatRoutes(mockRouter, app, () => chatManager);

  // 1. Anonymous request (no session) -> 401
  {
    let statusCode = null;
    let jsonBody = null;
    const req = {
      skIsAuthenticated: false,
      headers: {'content-type': 'application/json', 'content-length': '18'},
      body: {question: 'ping'},
    };
    const res = {
      status: code => { statusCode = code; return res; },
      json: data => { jsonBody = data; return res; },
    };
    await routes['/api/chat'](req, res);
    assert.equal(statusCode, 401);
    assert.match(jsonBody.error, /administrator required/i);
  }

  // 2. Authenticated non-admin request -> 403
  {
    let statusCode = null;
    let jsonBody = null;
    const req = {
      skIsAuthenticated: true,
      skPrincipal: {permissions: 'read-only'},
      headers: {'content-type': 'application/json', 'content-length': '18'},
      body: {question: 'ping'},
    };
    const res = {
      status: code => { statusCode = code; return res; },
      json: data => { jsonBody = data; return res; },
    };
    await routes['/api/chat'](req, res);
    assert.equal(statusCode, 403);
    assert.match(jsonBody.error, /administrator required/i);
  }

  fs.rmSync(app.tmpDir, {recursive: true, force: true});
});

test('argv is passed directly to spawn with no shell interpretation', async () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'lume-chat-argv-'));
  // Create a mock script that writes its raw process.argv to a json file
  const recordFile = path.join(tmpDir, 'argv.json');
  const mockScript = path.join(tmpDir, 'mock_lume.js');
  fs.writeFileSync(
    mockScript,
    `#!/usr/bin/env node
const fs = require('fs');
fs.writeFileSync(${JSON.stringify(recordFile)}, JSON.stringify(process.argv.slice(2)));
console.log(JSON.stringify({answer: "Mock answer", sql: ["SELECT 1;"], tool_calls: []}));
`
  );
  fs.chmodSync(mockScript, 0o755);

  const chatManager = new ChatManager({
    binary: mockScript,
    dataDir: tmpDir,
    getOptions: () => ({
      chatOllamaUrl: 'http://192.168.1.100:11434',
      chatModel: 'custom-model:latest',
    }),
  });

  const dangerousQuestion = '"; rm -rf /; $(id) && `touch foo` --flag \'single\' "double"';
  const result = await chatManager.ask(dangerousQuestion);

  assert.equal(result.answer, 'Mock answer');
  assert.deepEqual(result.sql, ['SELECT 1;']);

  // Inspect the captured argv passed to the process
  const capturedArgs = JSON.parse(fs.readFileSync(recordFile, 'utf8'));
  assert.equal(capturedArgs[0], 'chat');
  assert.equal(capturedArgs[1], '--json');
  assert.equal(capturedArgs[2], '--ti-store');
  assert.equal(capturedArgs[3], path.join(tmpDir, 'lume-ti'));
  assert.equal(capturedArgs[4], '--docs-index');
  assert.equal(capturedArgs[5], path.join(tmpDir, 'library', 'index'));
  assert.equal(capturedArgs[6], '--ollama-url');
  assert.equal(capturedArgs[7], 'http://192.168.1.100:11434');
  assert.equal(capturedArgs[8], '--ollama-model');
  assert.equal(capturedArgs[9], 'custom-model:latest');
  // Exact unescaped question preserved as a single argv element!
  assert.equal(capturedArgs[10], dangerousQuestion);

  fs.rmSync(tmpDir, {recursive: true, force: true});
});

test('Chat job enforces one at a time and times out', async () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'lume-chat-timeout-'));
  // Script that sleeps for 5 seconds
  const mockScript = path.join(tmpDir, 'slow_lume.js');
  fs.writeFileSync(
    mockScript,
    `#!/usr/bin/env node
setTimeout(() => {
  console.log(JSON.stringify({answer: "done", sql: [], tool_calls: []}));
}, 5000);
`
  );
  fs.chmodSync(mockScript, 0o755);

  const chatManager = new ChatManager({binary: mockScript, dataDir: tmpDir});

  // Start slow job with 100ms timeout
  const slowJobPromise = chatManager.ask('slow question', {timeoutMs: 100});
  assert.equal(chatManager.isRunning(), true);

  // Attempting another job while one is running returns 409
  await assert.rejects(
    chatManager.ask('concurrent question'),
    err => err.status === 409 && /Another chat job is in progress/.test(err.message)
  );

  // Slow job should time out and reset running state
  await assert.rejects(
    slowJobPromise,
    err => err.status === 504 && /timed out/.test(err.message)
  );
  assert.equal(chatManager.isRunning(), false);

  fs.rmSync(tmpDir, {recursive: true, force: true});
});

test('Ollama-unreachable message is returned plainly', async () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'lume-chat-unreachable-'));
  const mockScript = path.join(tmpDir, 'mock_unreachable.js');
  fs.writeFileSync(
    mockScript,
    `#!/usr/bin/env node
console.log(JSON.stringify({
  answer: "Ollama is unreachable at http://127.0.0.1:11434. Check that Ollama is running and accessible.",
  sql: [],
  tool_calls: []
}));
`
  );
  fs.chmodSync(mockScript, 0o755);

  const routes = {};
  const mockRouter = {
    post: (p, handler) => { routes[p] = handler; },
    get: (p, handler) => { routes[p] = handler; },
  };

  const app = createMockApp();
  const chatManager = new ChatManager({binary: mockScript, dataDir: tmpDir});
  registerChatRoutes(mockRouter, app, () => chatManager);

  let statusCode = 200;
  let jsonBody = null;
  const req = {
    skIsAuthenticated: true,
    skPrincipal: {permissions: 'admin'},
    headers: {'content-type': 'application/json', 'content-length': '30'},
    body: {question: 'Any vessels nearby?'},
  };
  const res = {
    status: code => { statusCode = code; return res; },
    json: data => { jsonBody = data; return res; },
  };

  await routes['/api/chat'](req, res);
  assert.equal(statusCode, 200);
  assert.ok(jsonBody);
  assert.match(jsonBody.answer, /Ollama is unreachable at http:\/\/127\.0\.0\.1:11434/);
  assert.deepEqual(jsonBody.sql, []);
  assert.deepEqual(jsonBody.tool_calls, []);

  fs.rmSync(tmpDir, {recursive: true, force: true});
  fs.rmSync(app.tmpDir, {recursive: true, force: true});
});

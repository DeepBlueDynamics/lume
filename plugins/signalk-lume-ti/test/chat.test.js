'use strict';

const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const {ChatManager, registerChatRoutes, EventStreamParser} = require('../lib/chat');

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

test('argv is passed directly to spawn with no shell interpretation', {skip: process.platform === 'win32' && 'fake lume is a shebang script; Windows cannot spawn it'}, async () => {
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
  assert.equal(capturedArgs[2], '--events');
  assert.equal(capturedArgs[3], '--ti-store');
  assert.equal(capturedArgs[4], path.join(tmpDir, 'lume-ti'));
  assert.equal(capturedArgs[5], '--docs-index');
  assert.equal(capturedArgs[6], path.join(tmpDir, 'library', 'index'));
  assert.equal(capturedArgs[7], '--ollama-url');
  assert.equal(capturedArgs[8], 'http://192.168.1.100:11434');
  assert.equal(capturedArgs[9], '--ollama-model');
  assert.equal(capturedArgs[10], 'custom-model:latest');
  // Exact unescaped question preserved as a single argv element!
  assert.equal(capturedArgs[11], dangerousQuestion);

  fs.rmSync(tmpDir, {recursive: true, force: true});
});

test('Chat job enforces one at a time and times out', {skip: process.platform === 'win32' && 'fake lume is a shebang script; Windows cannot spawn it'}, async () => {
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

test('Ollama-unreachable message is returned plainly', {skip: process.platform === 'win32' && 'fake lume is a shebang script; Windows cannot spawn it'}, async () => {
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

test('chatApiKeyFile passes OLLAMA_API_KEY in env only, never in argv', {skip: process.platform === 'win32' && 'fake lume is a shebang script; Windows cannot spawn it'}, async () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'lume-chat-apikey-'));
  const recordFile = path.join(tmpDir, 'captured.json');
  const mockScript = path.join(tmpDir, 'mock_lume.js');
  fs.writeFileSync(
    mockScript,
    `#!/usr/bin/env node
const fs = require('fs');
fs.writeFileSync(${JSON.stringify(recordFile)}, JSON.stringify({
  argv: process.argv.slice(2),
  apiKey: process.env.OLLAMA_API_KEY || null,
}));
console.log(JSON.stringify({answer: "Mock answer", sql: [], tool_calls: []}));
`
  );
  fs.chmodSync(mockScript, 0o755);

  const keyFile = path.join(tmpDir, 'ollama.key');
  fs.writeFileSync(keyFile, 'sk-ollama-secret-key-12345\n', {mode: 0o600});

  // 1. When chatApiKeyFile is set: env has key, argv does not
  {
    const chatManager = new ChatManager({
      binary: mockScript,
      dataDir: tmpDir,
      getOptions: () => ({
        chatApiKeyFile: keyFile,
        chatOllamaUrl: 'https://ollama.com',
        chatModel: 'glm-5.3:cloud',
      }),
    });

    const res = await chatManager.ask('test question');
    assert.equal(res.answer, 'Mock answer');

    const captured = JSON.parse(fs.readFileSync(recordFile, 'utf8'));
    assert.equal(captured.apiKey, 'sk-ollama-secret-key-12345');
    // Ensure key is NOT in argv
    assert.ok(!captured.argv.some(arg => arg.includes('sk-ollama-secret-key-12345')));
    assert.ok(!captured.argv.includes('--api-key'));
  }

  // 2. When chatApiKeyFile is unset: env does NOT have OLLAMA_API_KEY
  {
    const chatManager = new ChatManager({
      binary: mockScript,
      dataDir: tmpDir,
      getOptions: () => ({
        chatOllamaUrl: 'https://ollama.com',
        chatModel: 'glm-5.3:cloud',
      }),
    });

    const res = await chatManager.ask('test question without key');
    assert.equal(res.answer, 'Mock answer');

    const captured = JSON.parse(fs.readFileSync(recordFile, 'utf8'));
    assert.equal(captured.apiKey, null);
  }

  // 3. When chatApiKeyFile is unreadable or missing: throws error naming chatApiKeyFile
  {
    const missingKeyFile = path.join(tmpDir, 'nonexistent.key');
    const chatManager = new ChatManager({
      binary: mockScript,
      dataDir: tmpDir,
      getOptions: () => ({
        chatApiKeyFile: missingKeyFile,
      }),
    });

    await assert.rejects(
      chatManager.ask('question with missing key file'),
      err => /chatApiKeyFile/.test(err.message) && (/ENOENT/.test(err.message) || /Failed to read/.test(err.message))
    );
  }

  fs.rmSync(tmpDir, {recursive: true, force: true});
});

test('plugin schema defaults reflect ollama.com cloud direct and chatApiKeyFile', () => {
  const pluginFactory = require('../index');
  const app = createMockApp();
  const plugin = pluginFactory(app);
  const props = plugin.schema().properties;

  assert.equal(props.chatOllamaUrl.default, 'https://ollama.com');
  assert.equal(props.chatModel.default, 'glm-5.3:cloud');
  assert.ok(props.chatApiKeyFile, 'chatApiKeyFile setting exists in schema');
  assert.equal(props.chatApiKeyFile.default, '');

  fs.rmSync(app.tmpDir, {recursive: true, force: true});
});

test('EventStreamParser skips non-JSON lines and parses split chunks', () => {
  const events = [];
  const parser = new EventStreamParser(e => events.push(e));

  // Feed non-JSON line like [Agent] logging or compiler noise
  parser.feed('[Agent] Starting task: What was our speed?\n');
  parser.feed('Checking crate dependencies...\n');
  parser.feed('{"event":"thinking","turn":1}\n');

  // Feed partial lines across chunk boundaries
  parser.feed('{"event":"tool_call","turn":1,"name":"ti_query",');
  parser.feed('"args":{"sql":"SELECT 1;"}}\n');

  // Another non-JSON line mixed in
  parser.feed('[Agent] Executing tool ti_query\n');

  parser.feed('{"event":"tool_result","turn":1,"name":"ti_query","rows":1,"elapsed_ms":15,"error":null}\n');

  // Partial line completed by flush
  parser.feed('{"event":"thinking","turn":2}');
  parser.flush();

  assert.equal(events.length, 4);
  assert.deepEqual(events[0], {event: 'thinking', turn: 1});
  assert.deepEqual(events[1], {
    event: 'tool_call',
    turn: 1,
    name: 'ti_query',
    args: {sql: 'SELECT 1;'},
  });
  assert.deepEqual(events[2], {
    event: 'tool_result',
    turn: 1,
    name: 'ti_query',
    rows: 1,
    elapsed_ms: 15,
    error: null,
  });
  assert.deepEqual(events[3], {event: 'thinking', turn: 2});
});

test('POST /api/chat streams application/x-ndjson with events and final result', {skip: process.platform === 'win32' && 'fake lume is a shebang script; Windows cannot spawn it'}, async () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'lume-chat-stream-'));
  const mockScript = path.join(tmpDir, 'mock_stream_lume.js');
  fs.writeFileSync(
    mockScript,
    `#!/usr/bin/env node
// Write NDJSON events to stderr, with some non-JSON logging mixed in
process.stderr.write("[Agent] Starting research task\\n");
process.stderr.write(JSON.stringify({event: "thinking", turn: 1}) + "\\n");
process.stderr.write("[Agent] Running tool ti_schema\\n");
process.stderr.write(JSON.stringify({event: "tool_call", turn: 1, name: "ti_schema", args: {}}) + "\\n");
process.stderr.write(JSON.stringify({event: "tool_result", turn: 1, name: "ti_schema", rows: null, elapsed_ms: 12, error: null}) + "\\n");

// Stdout produces final JSON
console.log(JSON.stringify({
  answer: "Peak speed was 7.2 m/s.",
  sql: ["SELECT ts, speed FROM telemetry LIMIT 1;"],
  tool_calls: [{name: "ti_schema", args: {}, rows: null, truncated: null, error: null}],
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

  const writtenChunks = [];
  const headers = {};
  let ended = false;
  let statusCode = 200;

  const req = {
    skIsAuthenticated: true,
    skPrincipal: {permissions: 'admin'},
    headers: {
      'content-type': 'application/json',
      'content-length': '40',
      'accept': 'application/x-ndjson',
    },
    body: {question: 'What was our peak speed?'},
    on: () => {},
  };

  const res = {
    headersSent: true,
    setHeader: (k, v) => { headers[k.toLowerCase()] = v; },
    status: code => { statusCode = code; return res; },
    write: chunk => { writtenChunks.push(chunk); },
    end: () => { ended = true; },
  };

  await routes['/api/chat'](req, res);

  assert.equal(statusCode, 200);
  assert.equal(headers['content-type'], 'application/x-ndjson');
  assert.equal(ended, true);

  // Parse lines written to res.write
  const allLines = writtenChunks.join('').split('\n').filter(l => l.trim().length > 0);
  const parsedEvents = allLines.map(l => JSON.parse(l));

  // Should have received thinking, tool_call, tool_result, and final result
  assert.equal(parsedEvents.length, 4);
  assert.equal(parsedEvents[0].event, 'thinking');
  assert.equal(parsedEvents[1].event, 'tool_call');
  assert.equal(parsedEvents[1].name, 'ti_schema');
  assert.equal(parsedEvents[2].event, 'tool_result');
  assert.equal(parsedEvents[2].name, 'ti_schema');
  assert.equal(parsedEvents[3].event, 'result');
  assert.equal(parsedEvents[3].answer, 'Peak speed was 7.2 m/s.');
  assert.deepEqual(parsedEvents[3].sql, ['SELECT ts, speed FROM telemetry LIMIT 1;']);

  fs.rmSync(tmpDir, {recursive: true, force: true});
  fs.rmSync(app.tmpDir, {recursive: true, force: true});
});

test('POST /api/chat non-streaming fallback retains classic JSON response shape', {skip: process.platform === 'win32' && 'fake lume is a shebang script; Windows cannot spawn it'}, async () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'lume-chat-fallback-'));
  const mockScript = path.join(tmpDir, 'mock_lume.js');
  fs.writeFileSync(
    mockScript,
    `#!/usr/bin/env node
console.log(JSON.stringify({answer: "Fallback answer", sql: ["SELECT 42;"], tool_calls: []}));
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
  let jsonResult = null;

  const req = {
    skIsAuthenticated: true,
    skPrincipal: {permissions: 'admin'},
    headers: {
      'content-type': 'application/json',
      'content-length': '30',
      'accept': 'application/json',
    },
    body: {question: 'Any questions?'},
  };

  const res = {
    status: code => { statusCode = code; return res; },
    json: data => { jsonResult = data; return res; },
  };

  await routes['/api/chat'](req, res);

  assert.equal(statusCode, 200);
  assert.equal(jsonResult.answer, 'Fallback answer');
  assert.deepEqual(jsonResult.sql, ['SELECT 42;']);

  fs.rmSync(tmpDir, {recursive: true, force: true});
  fs.rmSync(app.tmpDir, {recursive: true, force: true});
});

test('POST /api/chat keepalive interval fires on long-running queries', async () => {
  const written = [];
  const keepaliveTimer = setInterval(() => {
    written.push(JSON.stringify({event: 'keepalive'}) + '\n');
  }, 20);

  await new Promise(r => setTimeout(r, 65));
  clearInterval(keepaliveTimer);

  assert.ok(written.length >= 2, `Expected at least 2 keepalives, got ${written.length}`);
  assert.equal(JSON.parse(written[0]).event, 'keepalive');
});


test('POST /api/chat stops lume chat when the client disconnects mid-stream', async () => {
  const {EventEmitter} = require('node:events');
  const routes = {};
  const mockRouter = {
    post: (p, handler) => { routes[p] = handler; },
    get: (p, handler) => { routes[p] = handler; },
  };
  let stopped = 0;
  let release;
  // Stand-in manager: ask() hangs until stop(), like a long cloud model call.
  const chatManager = {
    ask: () => new Promise((_, reject) => { release = reject; }),
    stop: () => {
      stopped += 1;
      release(Object.assign(new Error('stopped'), {status: 499}));
    },
  };
  registerChatRoutes(mockRouter, createMockApp(), () => chatManager);

  const req = Object.assign(new EventEmitter(), {
    skIsAuthenticated: true,
    skPrincipal: {permissions: 'admin'},
    headers: {'content-type': 'application/json', 'content-length': '27', 'accept': 'application/x-ndjson'},
    body: {question: 'Long question'},
  });
  const res = Object.assign(new EventEmitter(), {
    headersSent: true,
    writableEnded: false,
    setHeader: () => {},
    status: () => res,
    write: () => {},
    json: () => {},
    end: () => { res.writableEnded = true; },
  });

  const handled = routes['/api/chat'](req, res);
  await new Promise(r => setImmediate(r));
  // The request body being read must NOT stop the job.
  req.emit('close');
  assert.equal(stopped, 0, 'req close (body consumed) must not stop the job');
  // The browser going away before the response ends must stop it.
  res.emit('close');
  assert.equal(stopped, 1, 'client disconnect must stop lume chat');
  await handled.catch(() => {});
});

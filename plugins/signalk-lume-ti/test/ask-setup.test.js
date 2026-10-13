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

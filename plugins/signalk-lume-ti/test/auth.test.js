'use strict';

const test = require('node:test');
const assert = require('node:assert');
const fs = require('fs');
const path = require('path');
const os = require('os');
const http = require('http');

const { TokenManager } = require('../lib/auth');

test('TokenManager executes Signal K device access request and polls until approved', async () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'lume-auth-test-'));
  const tokenPath = path.join(tmpDir, 'token.txt');

  let pollCount = 0;
  const requestId = 'req-12345';
  const expectedToken = 'approved-jwt-token-abcdef';

  // Mock Signal K server
  const server = http.createServer((req, res) => {
    if (req.method === 'POST' && req.url === '/signalk/v1/access/requests') {
      let body = '';
      req.on('data', (c) => { body += c; });
      req.on('end', () => {
        const parsed = JSON.parse(body);
        assert.match(parsed.clientId, /^[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i);
        assert.strictEqual(parsed.permissions, 'readonly');
        res.writeHead(202, { 'Content-Type': 'application/json' });
        res.end(JSON.stringify({
          state: 'PENDING',
          statusCode: 202,
          message: 'Access request pending approval',
          href: `/signalk/v1/access/requests/${requestId}`,
        }));
      });
      return;
    }

    if (req.method === 'GET' && req.url === `/signalk/v1/access/requests/${requestId}`) {
      pollCount++;
      if (pollCount < 2) {
        res.writeHead(200, { 'Content-Type': 'application/json' });
        res.end(JSON.stringify({
          state: 'PENDING',
          statusCode: 202,
        }));
      } else {
        res.writeHead(200, { 'Content-Type': 'application/json' });
        res.end(JSON.stringify({
          state: 'COMPLETED',
          statusCode: 200,
          accessRequest: {
            token: expectedToken,
            permission: 'read',
          },
        }));
      }
      return;
    }

    res.writeHead(404);
    res.end();
  });

  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
  const serverPort = server.address().port;
  const signalkUrl = `ws://127.0.0.1:${serverPort}`;

  let tokenReceived = null;
  const mgr = new TokenManager({
    tokenPath,
    signalkUrl,
    pollIntervalMs: 50,
    onTokenReceived: (tok) => {
      tokenReceived = tok;
    },
  });

  assert.strictEqual(mgr.loadToken(), null, 'Initially no token on disk');

  await mgr.ensureToken();

  // Wait for polling to complete
  await new Promise((r) => setTimeout(r, 250));

  assert.strictEqual(tokenReceived, expectedToken, 'Token received via callback');
  assert.strictEqual(mgr.token, expectedToken, 'Token stored in manager');
  assert.ok(fs.existsSync(tokenPath), 'Token written to disk');
  assert.strictEqual(fs.readFileSync(tokenPath, 'utf8'), expectedToken);

  mgr.stop();
  await new Promise((resolve) => server.close(resolve));
  fs.rmSync(tmpDir, { recursive: true, force: true });
});

test('TokenManager loads existing token from disk without network request', async () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'lume-auth-existing-'));
  const tokenPath = path.join(tmpDir, 'token.txt');
  const existingToken = 'pre-existing-token-999';

  fs.writeFileSync(tokenPath, existingToken, 'utf8');

  const mgr = new TokenManager({
    tokenPath,
    signalkUrl: 'ws://127.0.0.1:9999', // Invalid port to ensure no network calls
  });

  const token = await mgr.ensureToken();
  assert.strictEqual(token, existingToken, 'Loaded pre-existing token');

  fs.rmSync(tmpDir, { recursive: true, force: true });
});

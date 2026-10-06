'use strict';

const fs = require('fs');
const http = require('http');
const https = require('https');
const { URL } = require('url');

/**
 * Signal K Device Access-Request Flow Manager.
 *
 * Implements the standard Signal K device access request flow:
 * 1. POST /signalk/v1/access/requests with clientId and description.
 * 2. Receives a pending request with a tracking URL (href).
 * 3. Polls the href until approved (COMPLETED) or DENIED.
 * 4. Persists the approved token to disk in the plugin data directory.
 *
 * Never requires a user password.
 */
class TokenManager {
  /**
   * @param {object} options
   * @param {string} options.tokenPath - Absolute path where token should be saved.
   * @param {string} options.signalkUrl - Signal K HTTP or WS URL (e.g. ws://127.0.0.1:3000).
   * @param {string} [options.clientId='signalk-lume-ti'] - Client ID identifier.
   * @param {string} [options.description] - Description shown to administrator.
   * @param {number} [options.pollIntervalMs=2000] - Polling interval in ms.
   * @param {function} [options.onTokenReceived] - Callback when token is obtained.
   * @param {function} [options.onStatus] - Status log callback.
   */
  constructor(options) {
    this.tokenPath = options.tokenPath;
    this.signalkHttpUrl = toHttpUrl(options.signalkUrl || 'ws://127.0.0.1:3000');
    this.clientId = options.clientId || 'signalk-lume-ti';
    this.description = options.description || 'Lume TI Ingest and Query Engine';
    this.pollIntervalMs = options.pollIntervalMs || 2000;
    this.onTokenReceived = options.onTokenReceived || (() => {});
    this.onStatus = options.onStatus || (() => {});

    this.token = this.loadToken();
    this.activePollTimer = null;
    this.isRequestPending = false;
    this.lastRequestId = null;
    this.lastState = this.token ? 'COMPLETED' : 'NONE';
  }

  /**
   * Read the existing token from disk, if present.
   *
   * @returns {string|null}
   */
  loadToken() {
    try {
      if (fs.existsSync(this.tokenPath)) {
        const raw = fs.readFileSync(this.tokenPath, 'utf8').trim();
        if (raw.startsWith('{')) {
          const parsed = JSON.parse(raw);
          return parsed.token || parsed.accessRequest?.token || null;
        }
        return raw.length > 0 ? raw : null;
      }
    } catch (err) {
      this.onStatus(`Failed reading token file: ${err.message}`);
    }
    return null;
  }

  /**
   * Save an approved token to disk.
   *
   * @param {string} token
   */
  saveToken(token) {
    this.token = token;
    try {
      fs.writeFileSync(this.tokenPath, token, { encoding: 'utf8', mode: 0o600 });
      this.onStatus(`Access token saved to ${this.tokenPath}`);
    } catch (err) {
      this.onStatus(`Failed saving token: ${err.message}`);
    }
  }

  /**
   * Initiate or resume the access-request flow.
   *
   * @returns {Promise<string|null>}
   */
  async ensureToken() {
    if (this.token) {
      return this.token;
    }

    // Try reading again in case it was written externally
    const existing = this.loadToken();
    if (existing) {
      this.token = existing;
      return existing;
    }

    if (this.isRequestPending) {
      return null;
    }

    return this.requestDeviceAccess();
  }

  /**
   * Request device access via POST /signalk/v1/access/requests.
   *
   * @returns {Promise<string|null>}
   */
  async requestDeviceAccess() {
    this.isRequestPending = true;
    this.lastState = 'PENDING';
    const postData = JSON.stringify({
      clientId: this.clientId,
      description: this.description,
      permissions: 'read',
    });

    const targetUrl = new URL('/signalk/v1/access/requests', this.signalkHttpUrl);
    this.onStatus(`Requesting device access from ${targetUrl.toString()}...`);

    try {
      const res = await httpRequest(targetUrl, {
        method: 'POST',
        headers: {
          'Content-Type': 'application/json',
          'Content-Length': Buffer.byteLength(postData),
        },
      }, postData);

      if (res.statusCode >= 200 && res.statusCode < 300) {
        const body = JSON.parse(res.body || '{}');
        const state = body.state || 'PENDING';
        this.lastState = state;

        if (state === 'COMPLETED') {
          const token = body.token || body.accessRequest?.token;
          if (token) {
            this.saveToken(token);
            this.isRequestPending = false;
            this.onTokenReceived(token);
            return token;
          }
        }

        if (body.href) {
          const pollUrl = new URL(body.href, this.signalkHttpUrl);
          this.lastRequestId = body.href;
          this.onStatus(`Access request pending administrator approval: ${body.href}`);
          this.startPolling(pollUrl);
        }
      } else {
        this.onStatus(`Access request returned HTTP ${res.statusCode}: ${res.body}`);
        this.isRequestPending = false;
      }
    } catch (err) {
      this.onStatus(`Error requesting device access: ${err.message}`);
      this.isRequestPending = false;
    }

    return null;
  }

  /**
   * Poll tracking href until completed or denied.
   *
   * @param {URL} pollUrl
   */
  startPolling(pollUrl) {
    if (this.activePollTimer) {
      clearTimeout(this.activePollTimer);
    }

    const poll = async () => {
      try {
        const res = await httpRequest(pollUrl, { method: 'GET' });
        if (res.statusCode === 200) {
          const body = JSON.parse(res.body || '{}');
          const state = body.state || 'PENDING';
          this.lastState = state;

          if (state === 'COMPLETED') {
            const token = body.token || body.accessRequest?.token;
            if (token) {
              this.onStatus('Signal K device access request APPROVED!');
              this.saveToken(token);
              this.isRequestPending = false;
              this.activePollTimer = null;
              this.onTokenReceived(token);
              return;
            }
          } else if (state === 'DENIED') {
            this.onStatus('Signal K device access request was DENIED by administrator.');
            this.isRequestPending = false;
            this.activePollTimer = null;
            return;
          }
        }
      } catch (err) {
        this.onStatus(`Polling error: ${err.message}`);
      }

      if (this.isRequestPending) {
        this.activePollTimer = setTimeout(poll, this.pollIntervalMs);
      }
    };

    this.activePollTimer = setTimeout(poll, this.pollIntervalMs);
  }

  /**
   * Stop any active polling.
   */
  stop() {
    if (this.activePollTimer) {
      clearTimeout(this.activePollTimer);
      this.activePollTimer = null;
    }
    this.isRequestPending = false;
  }
}

/**
 * Convert a WebSocket or HTTP URL to an HTTP base URL.
 *
 * @param {string} rawUrl
 * @returns {string}
 */
function toHttpUrl(rawUrl) {
  let u = rawUrl.trim();
  if (u.startsWith('ws://')) {
    u = 'http://' + u.slice(5);
  } else if (u.startsWith('wss://')) {
    u = 'https://' + u.slice(6);
  }
  return u;
}

/**
 * Helper to make a standard Node http/https request with Promises.
 *
 * @param {URL|string} urlObj
 * @param {object} options
 * @param {string} [bodyData]
 * @returns {Promise<{ statusCode: number, headers: object, body: string }>}
 */
function httpRequest(urlObj, options, bodyData) {
  return new Promise((resolve, reject) => {
    const parsed = typeof urlObj === 'string' ? new URL(urlObj) : urlObj;
    const client = parsed.protocol === 'https:' ? https : http;

    const reqOpts = {
      protocol: parsed.protocol,
      hostname: parsed.hostname,
      port: parsed.port || (parsed.protocol === 'https:' ? 443 : 80),
      path: parsed.pathname + parsed.search,
      method: options.method || 'GET',
      headers: options.headers || {},
      timeout: options.timeout || 10000,
    };

    const req = client.request(reqOpts, (res) => {
      let data = '';
      res.setEncoding('utf8');
      res.on('data', (chunk) => { data += chunk; });
      res.on('end', () => {
        resolve({
          statusCode: res.statusCode,
          headers: res.headers,
          body: data,
        });
      });
    });

    req.on('error', reject);
    req.on('timeout', () => {
      req.destroy(new Error(`HTTP request timed out: ${parsed.toString()}`));
    });

    if (bodyData) {
      req.write(bodyData);
    }
    req.end();
  });
}

module.exports = {
  TokenManager,
  toHttpUrl,
  httpRequest,
};

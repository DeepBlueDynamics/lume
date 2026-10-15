'use strict';
// Shivvr embedding server status monitor: probes GET <shivvrUrl>/health every 30 s with a 3 s
// timeout, and runs a separate cheap HEAD check against https://ollama.com so "Shivvr running"
// (local embedding server) and "Internet online" (cloud reachability) are two independent lights.
const fs = require('node:fs');
const http = require('node:http');
const https = require('node:https');

const DEFAULT_SHIVVR_URL = 'http://127.0.0.1:8085';
const DEFAULT_INTERNET_URL = 'https://ollama.com';
const DEFAULT_REQUIRED_MODEL = 'embeddinggemma-2';

const INSTALL_README_URL = 'https://github.com/DeepBlueDynamics/lume#shivvr-setup-semantic-search';
const INSTALL_DOCKER_CPU = 'docker volume create shivvr-models && docker run --rm -v shivvr-models:/target deepbluedynamics/shivvr-models:int8 && docker run -d --name shivvr --restart unless-stopped -p 127.0.0.1:8085:8080 -v shivvr-models:/models:ro deepbluedynamics/shivvr:latest-cpu';
const INSTALL_DOCKER_GPU = 'docker run -d --name shivvr --restart unless-stopped --gpus all -p 127.0.0.1:8085:8080 deepbluedynamics/shivvr';

function isHalosHost() {
  try {
    return fs.existsSync('/var/lib/container-apps') || fs.existsSync('/etc/halos-release');
  } catch (_) {
    return false;
  }
}

function normalizeUrl(raw, fallback = DEFAULT_SHIVVR_URL) {
  const s = String(raw || '').trim();
  return (s || fallback).replace(/\/+$/, '');
}

function defaultOfflineStatus(shivvrUrl = DEFAULT_SHIVVR_URL) {
  return {
    state: 'offline',
    running: false,
    online: false,
    url: normalizeUrl(shivvrUrl),
    version: null,
    default_model: null,
    models: [],
    gpu: null,
    halos: isHalosHost(),
    checked_at: null,
    error: 'Not checked yet',
  };
}

/**
 * Probe GET <shivvrUrl>/health with a 3 s timeout and classify state as
 * 'online', 'degraded' (up, but required model isn't active), or 'offline'.
 */
function probeShivvr(shivvrUrl = DEFAULT_SHIVVR_URL, {timeoutMs = 3000, requiredModel = DEFAULT_REQUIRED_MODEL} = {}) {
  const baseUrl = normalizeUrl(shivvrUrl);
  return new Promise(resolve => {
    const offline = error => resolve({
      state: 'offline',
      running: false,
      version: null,
      default_model: null,
      models: [],
      gpu: null,
      error: String(error || 'unreachable'),
    });

    let target;
    try {
      target = new URL(`${baseUrl}/health`);
    } catch (err) {
      return offline(`Invalid Shivvr URL: ${err.message}`);
    }

    const client = target.protocol === 'https:' ? https : http;
    let settled = false;
    const done = fn => {
      if (settled) return;
      settled = true;
      fn();
    };

    const req = client.request(target, {
      method: 'GET',
      timeout: timeoutMs,
      headers: {Accept: 'application/json'},
    }, res => {
      let body = '';
      res.setEncoding('utf8');
      res.on('data', chunk => {
        if (body.length < 65536) body += chunk;
      });
      res.on('end', () => done(() => {
        if (res.statusCode !== 200) {
          return offline(`HTTP ${res.statusCode}`);
        }
        let parsed;
        try {
          parsed = JSON.parse(body);
        } catch (_) {
          return offline('Invalid JSON from /health');
        }
        if (!parsed || typeof parsed !== 'object') {
          return offline('Invalid /health payload');
        }
        const models = Array.isArray(parsed.models)
          ? parsed.models
            .filter(m => m && typeof m === 'object' && m.name)
            .map(m => ({
              name: String(m.name),
              role: m.role ? String(m.role) : '',
              dimension: Number.isFinite(Number(m.dimension)) ? Number(m.dimension) : null,
              status: m.status ? String(m.status) : 'active',
            }))
          : [];
        const version = parsed.version ? String(parsed.version) : null;
        const defaultModel = parsed.default_model ? String(parsed.default_model) : null;
        const gpu = typeof parsed.gpu === 'boolean' ? parsed.gpu : null;
        const needed = requiredModel === undefined ? DEFAULT_REQUIRED_MODEL : requiredModel;
        const modelActive = !needed || needed === 'off'
          || models.some(m => m.name === needed && m.status === 'active');
        const statusOk = !parsed.status || parsed.status === 'ok';

        let state = 'online';
        let error = null;
        if (!modelActive) {
          state = 'degraded';
          error = `Model "${needed}" is not active on Shivvr`;
        } else if (!statusOk) {
          state = 'degraded';
          error = `Shivvr reported status "${parsed.status}"`;
        }
        resolve({
          state,
          running: true,
          version,
          default_model: defaultModel,
          models,
          gpu,
          error,
        });
      }));
    });

    req.on('timeout', () => {
      req.destroy(new Error('timed out'));
    });
    req.on('error', err => done(() => offline(err.message || 'unreachable')));
    req.end();
  });
}

/**
 * Cheap internet reachability check (HEAD https://ollama.com, 3 s timeout).
 * Independent of local Shivvr status so a boat at sea shows Shivvr running + Internet offline.
 */
function probeInternet(internetUrl = DEFAULT_INTERNET_URL, {timeoutMs = 3000} = {}) {
  if (!internetUrl) return Promise.resolve(false);
  return new Promise(resolve => {
    let target;
    try {
      target = new URL(String(internetUrl).trim());
    } catch (_) {
      return resolve(false);
    }
    const client = target.protocol === 'https:' ? https : http;
    let settled = false;
    const finish = ok => {
      if (settled) return;
      settled = true;
      resolve(ok);
    };
    const req = client.request(target, {
      method: 'HEAD',
      timeout: timeoutMs,
    }, res => {
      res.resume();
      finish(res.statusCode >= 100 && res.statusCode < 500);
    });
    req.on('timeout', () => {
      req.destroy(new Error('timed out'));
    });
    req.on('error', () => finish(false));
    req.end();
  });
}

class ShivvrMonitor {
  constructor({
    shivvrUrl = DEFAULT_SHIVVR_URL,
    getUrl = null,
    internetUrl = DEFAULT_INTERNET_URL,
    getInternetUrl = null,
    intervalMs = 30000,
    timeoutMs = 3000,
    getRequiredModel = () => DEFAULT_REQUIRED_MODEL,
    onUpdate = null,
  } = {}) {
    this.getUrl = typeof getUrl === 'function' ? () => normalizeUrl(getUrl()) : () => normalizeUrl(shivvrUrl);
    this.getInternetUrl = typeof getInternetUrl === 'function' ? getInternetUrl : () => internetUrl;
    this.intervalMs = intervalMs;
    this.timeoutMs = timeoutMs;
    this.getRequiredModel = getRequiredModel;
    this.onUpdate = onUpdate;
    this.timer = null;
    this.inFlight = null;
    this.cached = defaultOfflineStatus(this.getUrl());
  }

  async probe() {
    if (this.inFlight) return this.inFlight;
    const url = this.getUrl();
    const internetUrl = this.getInternetUrl();
    const requiredModel = typeof this.getRequiredModel === 'function'
      ? this.getRequiredModel()
      : DEFAULT_REQUIRED_MODEL;

    this.inFlight = Promise.all([
      probeShivvr(url, {timeoutMs: this.timeoutMs, requiredModel}),
      probeInternet(internetUrl, {timeoutMs: this.timeoutMs}),
    ]).then(([health, online]) => {
      this.cached = {
        ...health,
        online,
        url,
        halos: isHalosHost(),
        checked_at: new Date().toISOString(),
      };
      if (typeof this.onUpdate === 'function') {
        try { this.onUpdate(this.cached); } catch (_) {}
      }
      return this.cached;
    }).finally(() => {
      this.inFlight = null;
    });

    return this.inFlight;
  }

  start() {
    this.stop();
    this.probe().catch(() => {});
    if (this.intervalMs > 0) {
      this.timer = setInterval(() => {
        this.probe().catch(() => {});
      }, this.intervalMs);
      if (typeof this.timer.unref === 'function') {
        this.timer.unref();
      }
    }
  }

  stop() {
    if (this.timer) {
      clearInterval(this.timer);
      this.timer = null;
    }
  }

  status() {
    return {
      ...this.cached,
      url: this.getUrl(),
    };
  }
}

module.exports = {
  ShivvrMonitor,
  probeShivvr,
  probeInternet,
  defaultOfflineStatus,
  normalizeUrl,
  isHalosHost,
  DEFAULT_SHIVVR_URL,
  DEFAULT_INTERNET_URL,
  DEFAULT_REQUIRED_MODEL,
  INSTALL_README_URL,
  INSTALL_DOCKER_CPU,
  INSTALL_DOCKER_GPU,
};

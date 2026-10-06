'use strict';

const fs = require('fs');
const path = require('path');
const http = require('http');

const { resolveLumeBinary } = require('./lib/resolver');
const { TokenManager } = require('./lib/auth');
const { Supervisor } = require('./lib/supervisor');
const { collectStoreStatus } = require('./lib/status');

/**
 * Signal K Plugin Factory Function.
 *
 * @param {object} app - Signal K Server application instance.
 * @returns {object} Plugin object conforming to Signal K Plugin specification.
 */
module.exports = function (app) {
  let supervisor = null;
  let tokenManager = null;
  let statusInterval = null;
  let pluginConfig = {};
  let currentBinaryInfo = null;

  const plugin = {
    id: 'signalk-lume-ti',
    name: 'Lume TI',
    description: 'High-resolution timeseries store and query engine for Signal K',

    schema: () => ({
      type: 'object',
      properties: {
        signalkUrl: {
          type: 'string',
          title: 'Signal K WebSocket URL',
          default: 'ws://127.0.0.1:3000',
          description: 'WebSocket endpoint for live Signal K delta stream (usually ws://127.0.0.1:3000)',
        },
        servePort: {
          type: 'integer',
          title: 'Query Server Port',
          default: 5863,
          description: 'Local loopback port for the integrated query server (default 5863)',
        },
        lumePath: {
          type: 'string',
          title: 'Custom lume binary path (optional)',
          default: '',
          description: 'Leave empty to auto-detect bundled binary or system PATH',
        },
        autoRequestToken: {
          type: 'boolean',
          title: 'Request Signal K Access Token Automatically',
          default: true,
          description: 'Initiate device access-request flow if anonymous read-only access is disabled',
        },
      },
    }),

    /**
     * Start the plugin.
     *
     * @param {object} configuration - Saved plugin configuration.
     * @param {function} restartPlugin - Signal K callback to restart plugin.
     */
    start: function (configuration, restartPlugin) {
      pluginConfig = configuration || {};
      const dataDir = app.getDataDirPath();
      const storeDir = path.join(dataDir, 'lume-ti');
      const tokenPath = path.join(dataDir, 'token.txt');
      const signalkUrl = pluginConfig.signalkUrl || 'ws://127.0.0.1:3000';
      const servePort = pluginConfig.servePort || 5863;

      const log = (msg, isErr) => {
        if (typeof app.debug === 'function') {
          app.debug(msg);
        }
        if (isErr) {
          console.error(`[signalk-lume-ti] ${msg}`);
        } else {
          console.log(`[signalk-lume-ti] ${msg}`);
        }
      };

      log(`Starting Lume TI plugin. Data dir: ${dataDir}, Store dir: ${storeDir}`);

      // 1. Resolve binary
      currentBinaryInfo = resolveLumeBinary(pluginConfig, __dirname);
      if (!currentBinaryInfo.found) {
        const errMsg = `lume executable not found. Expected at bin/${process.platform}-${process.arch}/lume, or set custom lumePath.`;
        log(errMsg, true);
        if (typeof app.setPluginError === 'function') {
          app.setPluginError(errMsg);
        }
        return;
      }
      log(`Resolved lume executable: ${currentBinaryInfo.path} (${currentBinaryInfo.source})`);

      // 2. Setup Token Manager
      const autoAuth = pluginConfig.autoRequestToken !== false;
      tokenManager = new TokenManager({
        tokenPath,
        signalkUrl,
        clientId: 'signalk-lume-ti',
        description: 'Lume TI Ingest and Query Engine',
        onTokenReceived: (token) => {
          log('Received Signal K device access token.');
          // If supervisor was started without token, restart with token
          if (supervisor && !supervisor.tokenPath && fs.existsSync(tokenPath)) {
            supervisor.tokenPath = tokenPath;
            supervisor.stop().then(() => supervisor.start());
          }
        },
        onStatus: (st) => log(`[auth] ${st}`),
      });

      if (autoAuth) {
        tokenManager.ensureToken().catch((err) => {
          log(`Token request background check: ${err.message}`);
        });
      }

      // 3. Setup Supervisor
      supervisor = new Supervisor({
        binaryPath: currentBinaryInfo.path,
        signalkUrl,
        storeDir,
        servePort,
        serveBind: '127.0.0.1',
        tokenPath: fs.existsSync(tokenPath) ? tokenPath : null,
        onLog: (line, isErr) => log(line, isErr),
        onStateChange: () => updateStatus(app, supervisor, storeDir),
      });

      supervisor.start();

      // 4. Periodic status updates (every 5 seconds)
      statusInterval = setInterval(() => {
        updateStatus(app, supervisor, storeDir);
      }, 5000);

      updateStatus(app, supervisor, storeDir);
    },

    /**
     * Stop the plugin.
     */
    stop: function () {
      if (statusInterval) {
        clearInterval(statusInterval);
        statusInterval = null;
      }

      if (tokenManager) {
        tokenManager.stop();
        tokenManager = null;
      }

      if (supervisor) {
        const sup = supervisor;
        supervisor = null;
        sup.stop().then(() => {
          if (typeof app.setPluginStatus === 'function') {
            app.setPluginStatus('Stopped');
          }
        });
      }
    },

    /**
     * Register REST API routes and webapp static files with Signal K router.
     *
     * @param {object} router - Express router.
     */
    registerWithRouter: function (router) {
      // 1. Status API
      router.get('/api/status', (req, res) => {
        const dataDir = typeof app.getDataDirPath === 'function' ? app.getDataDirPath() : '';
        const storeDir = path.join(dataDir, 'lume-ti');
        const supStatus = supervisor ? supervisor.getStatus() : { running: false };
        const storeStatus = collectStoreStatus(storeDir);

        res.json({
          ok: true,
          binary: currentBinaryInfo,
          supervisor: supStatus,
          store: storeStatus,
          timestamp: new Date().toISOString(),
        });
      });

      // 2. Query proxy API (POST /api/query -> http://127.0.0.1:5863/ti/query)
      router.post('/api/query', (req, res) => {
        const servePort = pluginConfig.servePort || 5863;
        proxyHttpRequest({
          targetPort: servePort,
          targetPath: '/ti/query',
          method: 'POST',
          req,
          res,
        });
      });

      // 3. Schema proxy API (GET /api/schema -> http://127.0.0.1:5863/ti/schema)
      router.get('/api/schema', (req, res) => {
        const servePort = pluginConfig.servePort || 5863;
        proxyHttpRequest({
          targetPort: servePort,
          targetPath: '/ti/schema',
          method: 'GET',
          req,
          res,
        });
      });

      // 4. Static webapp files
      const publicDir = path.join(__dirname, 'public');
      if (fs.existsSync(publicDir)) {
        router.get('/', (req, res) => {
          res.sendFile(path.join(publicDir, 'index.html'));
        });
        router.get('/index.html', (req, res) => {
          res.sendFile(path.join(publicDir, 'index.html'));
        });
        router.get('/app.js', (req, res) => {
          res.sendFile(path.join(publicDir, 'app.js'));
        });
        router.get('/style.css', (req, res) => {
          res.sendFile(path.join(publicDir, 'style.css'));
        });
      }
    },
  };

  return plugin;
};

/**
 * Update Signal K status string via app.setPluginStatus / app.setPluginError.
 *
 * @param {object} app
 * @param {Supervisor} supervisor
 * @param {string} storeDir
 */
function updateStatus(app, supervisor, storeDir) {
  if (!supervisor) return;
  const status = supervisor.getStatus();
  const store = collectStoreStatus(storeDir);

  if (status.running) {
    let summary = `Running (PID: ${status.pid}, up: ${formatUptime(status.uptimeSecs)})`;
    if (store.ingestStatus) {
      const lag = typeof store.ingestStatus.ingest_lag_seconds === 'number'
        ? `${store.ingestStatus.ingest_lag_seconds.toFixed(2)}s`
        : 'n/a';
      const records = typeof store.ingestStatus.records_ingested === 'number'
        ? store.ingestStatus.records_ingested.toLocaleString()
        : '0';
      summary += ` | Lag: ${lag} | Ingested: ${records}`;
    }
    summary += ` | Disk: ${store.diskHuman}`;
    if (store.walBytes > 0) {
      summary += ` (WAL: ${store.walHuman})`;
    }

    if (typeof app.setPluginStatus === 'function') {
      app.setPluginStatus(summary);
    }
  } else if (status.lastError) {
    if (typeof app.setPluginError === 'function') {
      app.setPluginError(`Process error: ${status.lastError} (restarts: ${status.restarts})`);
    }
  } else {
    if (typeof app.setPluginStatus === 'function') {
      app.setPluginStatus(`Stopped (exit code: ${status.lastExitCode ?? 'none'})`);
    }
  }
}

/**
 * Format seconds into human readable duration (e.g. 2h 15m 30s).
 *
 * @param {number} totalSecs
 * @returns {string}
 */
function formatUptime(totalSecs) {
  if (totalSecs < 60) return `${totalSecs}s`;
  const m = Math.floor(totalSecs / 60);
  const s = totalSecs % 60;
  if (m < 60) return `${m}m ${s}s`;
  const h = Math.floor(m / 60);
  const remM = m % 60;
  return `${h}h ${remM}m`;
}

/**
 * Proxy an HTTP request to the local `lume --serve` query server on loopback.
 *
 * @param {object} opts
 * @param {number} opts.targetPort
 * @param {string} opts.targetPath
 * @param {string} opts.method
 * @param {object} opts.req - Express req
 * @param {object} opts.res - Express res
 */
function proxyHttpRequest({ targetPort, targetPath, method, req, res }) {
  let bodyBuffer = Buffer.alloc(0);

  const onData = (chunk) => {
    bodyBuffer = Buffer.concat([bodyBuffer, chunk]);
  };

  const onEnd = () => {
    const proxyReq = http.request({
      hostname: '127.0.0.1',
      port: targetPort,
      path: targetPath,
      method: method,
      headers: {
        'Content-Type': req.headers['content-type'] || 'application/json',
        'Accept': req.headers['accept'] || 'application/json',
        'Content-Length': bodyBuffer.length,
      },
      timeout: 15000,
    }, (proxyRes) => {
      res.status(proxyRes.statusCode);
      for (const [k, v] of Object.entries(proxyRes.headers)) {
        res.setHeader(k, v);
      }
      proxyRes.pipe(res);
    });

    proxyReq.on('error', (err) => {
      res.status(503).json({
        ok: false,
        error: `Query server on 127.0.0.1:${targetPort} unavailable: ${err.message}`,
      });
    });

    proxyReq.on('timeout', () => {
      proxyReq.destroy();
      res.status(504).json({
        ok: false,
        error: `Query server request timed out on 127.0.0.1:${targetPort}`,
      });
    });

    if (bodyBuffer.length > 0) {
      proxyReq.write(bodyBuffer);
    }
    proxyReq.end();
  };

  if (req.body && typeof req.body === 'object' && Object.keys(req.body).length > 0) {
    // If body-parser already parsed it
    bodyBuffer = Buffer.from(JSON.stringify(req.body));
    onEnd();
  } else {
    req.on('data', onData);
    req.on('end', onEnd);
  }
}

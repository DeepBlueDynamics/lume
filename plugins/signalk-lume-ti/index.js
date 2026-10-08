'use strict';

const fs = require('fs');
const path = require('path');
const http = require('http');

const { resolveLumeBinary } = require('./lib/resolver');
const { TokenManager } = require('./lib/auth');
const { Supervisor, validateBind } = require('./lib/supervisor');
const { collectStoreStatus } = require('./lib/status');
const { createHistoryProvider } = require('./lib/history');
const {pgOptions, writePgConfig, registerPgRoutes, adminStatus, readJson} = require('./lib/pg');
const {Library} = require('./lib/library');
const {ChatManager, registerChatRoutes} = require('./lib/chat');

/**
 * Signal K Plugin Factory Function.
 *
 * @param {object} app - Signal K Server application instance.
 * @returns {object} Plugin object conforming to Signal K Plugin specification.
 */
module.exports = function (app) {
  let supervisor = null;
  let history = null;
  let tokenManager = null;
  let statusInterval = null;
  let pluginConfig = {};
  let restartWithConfig = null;
  let currentBinaryInfo = null;
  let library = null;
  let chatManager = null;

  const libraryUnavailable = res => res.status(503).json({error: 'Library unavailable: lume binary not resolved'});
  /** Library API: list, admin-only indexing job, search, and alert references. */
  function registerLibraryRoutes(router) {
    router.get('/api/library', (req, res) => {
      if (!library) return libraryUnavailable(res);
      res.json({items: library.items(), ...library.status()});
    });
    router.post('/api/library/index', async (req, res) => {
      const status = adminStatus(app, req);
      if (status !== 200) return res.status(status).json({error: 'Signal K administrator required'});
      if (!library) return libraryUnavailable(res);
      try {
        const body = await readJson(req);
        if (!Array.isArray(body.ids) || body.ids.length > 500) throw Object.assign(new Error('ids must be an array'), {status: 400});
        res.json(await library.index(body.ids));
      } catch (error) {
        res.status(error.status || 400).json({error: error.message});
      }
    });
    router.get('/api/library/search', async (req, res) => {
      if (!library) return libraryUnavailable(res);
      try { res.json(await library.search(String(req.query.q || ''), Number(req.query.limit) || 8)); }
      catch (error) { res.status(500).json({error: error.message}); }
    });
    router.get('/api/library/references', async (req, res) => {
      if (!library) return libraryUnavailable(res);
      const servePort = pluginConfig.servePort || 5863;
      http.get({host: '127.0.0.1', port: servePort, path: '/ti/status', timeout: 5000}, upstream => {
        let text = '';
        upstream.on('data', c => { text += c; });
        upstream.on('end', async () => {
          try {
            const alerts = (JSON.parse(text).active_alerts || [])
              .filter(a => !/notifications\.security\.accessRequest/.test(String(a.id || '')));
            res.json({alerts: alerts.length, references: await library.references(alerts)});
          } catch (error) { res.status(502).json({error: `Lume status unavailable: ${error.message}`}); }
        });
      }).on('error', error => res.status(502).json({error: `Lume status unavailable: ${error.message}`}));
    });
  }

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
        otlpEnabled: {
          type: 'boolean',
          title: 'Enable OTLP Receiver',
          default: false,
          description: 'Enable OTLP HTTP/JSON receiver on the query server (/v1/metrics and /v1/logs)',
        },
        otlpTokenFile: {
          type: 'string',
          title: 'OTLP Bearer Token File Path (optional)',
          default: '',
          description: 'Path to a file containing the bearer token for OTLP ingestion. Required if serveBind is non-loopback; optional on loopback. The token is read by the lume process; never put the token in config or logs.',
        },
        enablePg: {type: 'boolean', title: 'Enable PostgreSQL (Grafana)', default: false},
        pgPort: {type: 'integer', title: 'PostgreSQL port', default: 5864, minimum: 1, maximum: 65535},
        pgUser: {type: 'string', title: 'PostgreSQL user', default: 'grafana'},
        pgBind: {type: 'string', title: 'PostgreSQL bind IP', default: '127.0.0.1',
          description: 'Use 172.17.0.1 (docker0) for HaLOS Grafana; HTTP remains on loopback.'},
        pgVerifier: {type: 'string', title: 'SCRAM verifier (managed)',
          description: 'Set/change password in the webapp PostgreSQL settings. Never paste a plaintext password here.'},
        pgRequireTls: {
          type: 'string',
          title: 'Require TLS for PostgreSQL',
          default: 'auto',
          enum: ['auto', 'true', 'false'],
          description: 'Require TLS: auto (bind-address policy: required off loopback/docker0), true (always require), false (disable)',
        },
        pgTlsCert: {
          type: 'string',
          title: 'PostgreSQL TLS Certificate Path (optional)',
          default: '',
          description: 'Path to PEM certificate file. If omitted, self-signed cert is generated at <store>/pg_cert.pem',
        },
        pgTlsKey: {
          type: 'string',
          title: 'PostgreSQL TLS Private Key Path (optional)',
          default: '',
          description: 'Path to PEM private key file (must be chmod 0600 on Unix). If omitted, self-signed key is generated at <store>/pg_key.pem',
        },
        pgAllowPlaintext: {
          type: 'boolean',
          title: 'Allow Plaintext PostgreSQL Connections',
          default: false,
          description: 'Allow unencrypted connections on non-loopback binds',
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
        chatOllamaUrl: {
          type: 'string',
          title: 'Chat Ollama API URLs',
          default: 'https://ollama.com',
          description: 'Ollama endpoints for the Ask tab, comma-separated and tried in order: the first reachable one that has the model is used. Default: https://ollama.com. When using a local or LAN Ollama, set e.g. http://127.0.0.1:11434,http://192.168.1.20:11434.',
        },
        chatModel: {
          type: 'string',
          title: 'Chat Ollama Model',
          default: 'glm-5.3:cloud',
          description: 'Model name on Ollama for chat Q&A and SQL analytics (default glm-5.3:cloud)',
        },
        chatApiKeyFile: {
          type: 'string',
          title: 'Chat API Key File Path (optional)',
          default: '',
          description: 'Path to a file containing the ollama.com API key (e.g. /path/to/ollama.key, mode 0600). Passed to the chat subprocess via OLLAMA_API_KEY environment variable only, never stored in plugin configuration or logs.',
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
      pluginConfig = {...(configuration || {})};
      // Passwords only enter through the dedicated pre-persistence endpoint.
      if (pluginConfig.pgPassword) {
        delete pluginConfig.pgPassword;
        throw new Error('Use the webapp PostgreSQL password form');
      }
      const pg = pgOptions(pluginConfig);
      restartWithConfig = restartPlugin;
      const pgAuthConfig = writePgConfig(app.getDataDirPath(), pg);
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
      library = new Library({binary: currentBinaryInfo.path, dataDir, servePort, log: line => log(line)});
      chatManager = new ChatManager({
        binary: currentBinaryInfo.path,
        dataDir,
        getOptions: () => pluginConfig,
        log: line => log(line),
      });

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
      const serveBind = pluginConfig.serveBind || '127.0.0.1';
      const otlpEnabled = pluginConfig.otlpEnabled === true;
      const otlpTokenFile = pluginConfig.otlpTokenFile ? String(pluginConfig.otlpTokenFile).trim() : null;

      if (otlpEnabled) {
        const bindErr = validateBind(serveBind, otlpTokenFile);
        if (bindErr) {
          const errMsg = `Config error: ${bindErr}`;
          log(errMsg, true);
          if (typeof app.setPluginError === 'function') {
            app.setPluginError(errMsg);
          }
        }
      }

      supervisor = new Supervisor({
        binaryPath: currentBinaryInfo.path,
        signalkUrl,
        storeDir,
        servePort,
        serveBind,
        otlpEnabled,
        otlpTokenFile,
        pgPort: pg.enablePg ? pg.pgPort : null,
        pgBind: pg.pgBind,
        pgAuthConfig,
        docsIndex: path.join(dataDir, 'library', 'index'),
        pgRequireTls: pg.pgRequireTls,
        pgTlsCert: pg.pgTlsCert,
        pgTlsKey: pg.pgTlsKey,
        pgAllowPlaintext: pg.pgAllowPlaintext,
        tokenPath: fs.existsSync(tokenPath) ? tokenPath : null,
        onLog: (line, isErr) => log(line, isErr),
        onStateChange: () => updateStatus(app, supervisor, storeDir),
      });

      supervisor.start();
      if (typeof app.registerHistoryApiProvider === 'function') {
        history = createHistoryProvider({app, port:servePort});
        app.registerHistoryApiProvider(history.provider);
      } else log('History API provider registration unavailable; Signal K 2.31+ required.');

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
      restartWithConfig = null;
      if (history) {
        history.stop();
        history = null;
        if (typeof app.unregisterHistoryApiProvider === 'function') app.unregisterHistoryApiProvider();
      }
      if (statusInterval) {
        clearInterval(statusInterval);
        statusInterval = null;
      }

      if (tokenManager) {
        tokenManager.stop();
        tokenManager = null;
      }

      if (chatManager) {
        chatManager.stop();
        chatManager = null;
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
      registerPgRoutes(router, app, () => {
        const saved = typeof app.readPluginOptions === 'function' ? app.readPluginOptions() : null;
        return saved?.configuration || pluginConfig;
      }, safe => {
        pluginConfig = safe;
        if (typeof restartWithConfig === 'function') restartWithConfig(safe);
      });
      router.get('/pg.js', (req, res) => res.sendFile(path.join(__dirname, 'public', 'pg.js')));
      router.get('/library.js', (req, res) => res.sendFile(path.join(__dirname, 'public', 'library.js')));
      registerLibraryRoutes(router);
      registerChatRoutes(router, app, () => chatManager);
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

      router.get('/pin.js', (req, res) => res.sendFile(path.join(__dirname, 'public', 'pin.js')));

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

'use strict';

const fs = require('fs');
const path = require('path');
const { spawn } = require('child_process');
const {ensureLastDefault} = require('./store-config');

/**
 * Child Process Supervisor for `lume ti ingest --serve`.
 *
 * Handles:
 * - Process spawning with configuration flags.
 * - Automatic crash recovery with exponential backoff.
 * - Graceful shutdown via SIGTERM (triggering Rust WAL flush & bucket seal).
 * - Process state and uptime tracking.
 */
class Supervisor {
  /**
   * @param {object} options
   * @param {string} options.binaryPath - Absolute path to `lume` executable.
   * @param {string} options.signalkUrl - WebSocket URL for Signal K (e.g. ws://127.0.0.1:3000).
   * @param {string} options.storeDir - Path to store directory.
   * @param {number} [options.servePort=5863] - Port for integrated query server.
   * @param {string} [options.serveBind='127.0.0.1'] - Bind address for query server.
   * @param {string} [options.tokenPath] - Optional path to token file.
   * @param {string} [options.configPath] - Optional path to ti.toml.
   * @param {string[]} [options.extraArgs] - Additional command line arguments.
   * @param {number} [options.backoffInitialMs=1000] - Initial crash restart delay.
   * @param {number} [options.backoffMaxMs=30000] - Max restart delay.
   * @param {number} [options.killTimeoutMs=15000] - Max time to wait for SIGTERM before SIGKILL.
   * @param {function} [options.onLog] - Log callback (line, isErr).
   * @param {function} [options.onStateChange] - State change callback.
   */
  constructor(options) {
    this.binaryPath = options.binaryPath;
    this.signalkUrl = options.signalkUrl || 'ws://127.0.0.1:3000';
    this.storeDir = options.storeDir;
    this.servePort = options.servePort || 5863;
    this.serveBind = options.serveBind || '127.0.0.1';
    this.tokenPath = options.tokenPath || null;
    this.configPath = options.configPath || null;
    this.pgPort = options.pgPort ?? null;
    this.pgBind = options.pgBind || '127.0.0.1';
    this.pgAuthConfig = options.pgAuthConfig || null;
    this.pgRequireTls = options.pgRequireTls ?? 'auto';
    this.pgTlsCert = options.pgTlsCert || null;
    this.pgTlsKey = options.pgTlsKey || null;
    this.pgAllowPlaintext = options.pgAllowPlaintext === true;
    this.extraArgs = options.extraArgs || [];

    this.backoffInitialMs = options.backoffInitialMs || 1000;
    this.backoffMaxMs = options.backoffMaxMs || 30000;
    this.killTimeoutMs = options.killTimeoutMs || 15000;
    this.onLog = options.onLog || (() => {});
    this.onStateChange = options.onStateChange || (() => {});

    this.child = null;
    this.running = false;
    this.stopping = false;
    this.startTime = null;
    this.restarts = 0;
    this.lastExitCode = null;
    this.lastExitSignal = null;
    this.lastError = null;
    this.restartTimer = null;
    this.killTimer = null;
  }

  /**
   * Build command line arguments for `lume ti ingest`.
   *
   * @returns {string[]}
   */
  buildArgs() {
    const args = [
      'ti',
      'ingest',
      '--signalk', this.signalkUrl,
      '--store', this.storeDir,
      '--serve',
      '--port', String(this.servePort),
      '--bind', this.serveBind,
    ];

    if (this.configPath && fs.existsSync(this.configPath)) {
      args.push('--config', this.configPath);
    }

    if (this.tokenPath && fs.existsSync(this.tokenPath)) {
      args.push('--token', this.tokenPath);
    }

    if (this.pgPort !== null) {
      if (!this.pgAuthConfig || !fs.existsSync(this.pgAuthConfig)) throw new Error('PostgreSQL auth config is missing');
      args.push('--pg', String(this.pgPort), '--pg-bind', this.pgBind, '--pg-auth-config', this.pgAuthConfig);
      if (this.pgRequireTls === true || this.pgRequireTls === 'true') {
        args.push('--pg-require-tls');
      } else if (this.pgRequireTls === false || this.pgRequireTls === 'false') {
        args.push('--pg-require-tls=false');
      }
      if (this.pgTlsCert) {
        args.push('--pg-tls-cert', this.pgTlsCert);
      }
      if (this.pgTlsKey) {
        args.push('--pg-tls-key', this.pgTlsKey);
      }
      if (this.pgAllowPlaintext) {
        args.push('--pg-allow-plaintext');
      }
    }

    if (Array.isArray(this.extraArgs) && this.extraArgs.length > 0) {
      args.push(...this.extraArgs);
    }

    return args;
  }

  /**
   * Start the supervised process.
   *
   * @returns {boolean}
   */
  start() {
    if (this.running || this.child) {
      return true;
    }

    this.stopping = false;

    // Ensure store directory exists
    try {
      fs.mkdirSync(this.storeDir, { recursive: true });
      ensureLastDefault(this.storeDir);
    } catch (err) {
      this.lastError = `Failed preparing store configuration: ${err.message}`;
      this.onLog(`[supervisor] ${this.lastError}`, true);
      this.onStateChange(this.getStatus());
      return false;
    }

    const args = this.buildArgs();
    this.onLog(`[supervisor] Spawning: ${this.binaryPath} ${args.join(' ')}`);

    try {
      this.child = spawn(this.binaryPath, args, {
        stdio: ['ignore', 'pipe', 'pipe'],
        detached: false,
        env: {
          ...process.env,
          RUST_BACKTRACE: '1',
        },
      });
    } catch (err) {
      this.lastError = `Failed spawning ${this.binaryPath}: ${err.message}`;
      this.onLog(`[supervisor] ${this.lastError}`, true);
      this.scheduleRestart();
      this.onStateChange(this.getStatus());
      return false;
    }

    this.running = true;
    this.startTime = Date.now();
    this.lastError = null;

    this.child.stdout.on('data', (chunk) => {
      const text = chunk.toString('utf8');
      for (const line of text.split(/\r?\n/)) {
        if (line.trim().length > 0) {
          this.onLog(`[lume] ${line}`, false);
        }
      }
    });

    this.child.stderr.on('data', (chunk) => {
      const text = chunk.toString('utf8');
      for (const line of text.split(/\r?\n/)) {
        if (line.trim().length > 0) {
          this.onLog(`[lume:err] ${line}`, true);
        }
      }
    });

    this.child.on('error', (err) => {
      this.lastError = err.message;
      this.onLog(`[supervisor] Child error: ${err.message}`, true);
    });

    this.child.on('exit', (code, signal) => {
      this.running = false;
      this.lastExitCode = code;
      this.lastExitSignal = signal;
      const pid = this.child ? this.child.pid : null;
      this.child = null;

      this.onLog(`[supervisor] Process ${pid || ''} exited with code ${code}, signal ${signal}`);

      if (this.killTimer) {
        clearTimeout(this.killTimer);
        this.killTimer = null;
      }

      if (!this.stopping) {
        this.scheduleRestart();
      }
      this.onStateChange(this.getStatus());
    });

    this.onStateChange(this.getStatus());
    return true;
  }

  /**
   * Schedule automatic restart with exponential backoff on crash.
   */
  scheduleRestart() {
    if (this.stopping || this.restartTimer) {
      return;
    }

    this.restarts++;
    const delay = Math.min(
      this.backoffInitialMs * Math.pow(2, Math.min(this.restarts - 1, 6)),
      this.backoffMaxMs
    );

    this.onLog(`[supervisor] Scheduling restart #${this.restarts} in ${delay}ms...`);
    this.restartTimer = setTimeout(() => {
      this.restartTimer = null;
      if (!this.stopping) {
        this.start();
      }
    }, delay);
  }

  /**
   * Stop the child process gracefully with SIGTERM.
   *
   * @returns {Promise<void>}
   */
  async stop() {
    this.stopping = true;

    if (this.restartTimer) {
      clearTimeout(this.restartTimer);
      this.restartTimer = null;
    }

    if (!this.child) {
      this.running = false;
      this.onStateChange(this.getStatus());
      return;
    }

    const currentChild = this.child;
    this.onLog(`[supervisor] Sending SIGTERM to pid ${currentChild.pid}...`);

    return new Promise((resolve) => {
      let resolved = false;
      const finish = () => {
        if (!resolved) {
          resolved = true;
          this.running = false;
          if (this.killTimer) {
            clearTimeout(this.killTimer);
            this.killTimer = null;
          }
          this.onStateChange(this.getStatus());
          resolve();
        }
      };

      currentChild.once('exit', finish);

      // Trigger SIGTERM
      try {
        currentChild.kill('SIGTERM');
      } catch (err) {
        this.onLog(`[supervisor] Error sending SIGTERM: ${err.message}`, true);
        finish();
        return;
      }

      // Force kill fallback if graceful shutdown exceeds killTimeoutMs
      this.killTimer = setTimeout(() => {
        if (this.child && !resolved) {
          this.onLog(`[supervisor] Timeout waiting for SIGTERM, sending SIGKILL to ${currentChild.pid}...`, true);
          try {
            currentChild.kill('SIGKILL');
          } catch (_e) {
            // Already dead
          }
          finish();
        }
      }, this.killTimeoutMs);
    });
  }

  /**
   * Get current supervisor status.
   *
   * @returns {object}
   */
  getStatus() {
    const uptimeSecs = this.running && this.startTime
      ? Math.floor((Date.now() - this.startTime) / 1000)
      : 0;

    return {
      running: this.running,
      stopping: this.stopping,
      pid: this.child ? this.child.pid : null,
      uptimeSecs,
      restarts: this.restarts,
      lastExitCode: this.lastExitCode,
      lastExitSignal: this.lastExitSignal,
      lastError: this.lastError,
      binaryPath: this.binaryPath,
      storeDir: this.storeDir,
      servePort: this.servePort,
      serveBind: this.serveBind,
    };
  }
}

module.exports = {
  Supervisor,
};

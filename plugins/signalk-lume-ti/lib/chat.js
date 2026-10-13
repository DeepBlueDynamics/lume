'use strict';

const fs = require('node:fs');
const path = require('node:path');
const {spawn} = require('node:child_process');
const {adminStatus, readJson} = require('./pg');

/** The Ask tab's default model server, matching the plugin schema default. */
const DEFAULT_CHAT_URL = 'https://ollama.com';
/** Where the Ask tab's Save key button writes the key, inside the plugin data dir. */
const KEY_FILE_NAME = 'ollama.key';

function chatError(message, status, code) {
  const err = new Error(message);
  err.status = status;
  err.code = code;
  return err;
}

/** True when every configured endpoint is ollama.com, so a key is required. */
function needsCloudKey(urls) {
  const list = String(urls || '').split(',').map(u => u.trim()).filter(Boolean);
  if (!list.length) return false;
  return list.every(u => {
    try {
      const host = new URL(/^[a-z]+:\/\//i.test(u) ? u : `https://${u}`).hostname;
      return host === 'ollama.com' || host.endsWith('.ollama.com');
    } catch (_) {
      return false;
    }
  });
}

/** A setting value that is a key, pasted where a file path belongs. */
function looksLikeKey(value) {
  return !/[\\/]/.test(value) && !/\.key$/i.test(value) && /^[A-Za-z0-9._-]{20,}$/.test(value);
}

/**
 * Turn a failed `lume chat` run into a sentence a boater can act on. The raw output
 * is kept for the log only; it never carries the key (the key is in the env).
 */
function explainChatFailure(output, url) {
  const text = String(output || '');
  if (/\b401\b|unauthori[sz]ed|invalid api key|forbidden|\b403\b/i.test(text)) {
    return chatError(`${url} rejected the API key. Save a new key in the Ask tab.`, 502, 'bad_key');
  }
  if (/dns|resolve|lookup|getaddrinfo|ENOTFOUND|EAI_AGAIN|no route|network is unreachable|connection refused|ECONNREFUSED|timed? ?out|ETIMEDOUT|error sending request|tls|certificate/i.test(text)) {
    return chatError(`Ask can't reach ${url}. Check that the boat has an internet connection, or point Chat Ollama API URLs at a model on your own network.`, 502, 'unreachable');
  }
  if (/model .*not found|no such model|pull/i.test(text)) {
    return chatError(`${url} doesn't have the model set in Chat Ollama Model.`, 502, 'no_model');
  }
  return chatError('Ask failed. The plugin log has the details.', 500, 'failed');
}

class EventStreamParser {
  constructor(onEvent) {
    this.buffer = '';
    this.onEvent = onEvent;
  }

  feed(chunk) {
    this.buffer += chunk.toString();
    let idx;
    while ((idx = this.buffer.indexOf('\n')) !== -1) {
      const line = this.buffer.slice(0, idx).trim();
      this.buffer = this.buffer.slice(idx + 1);
      if (!line) continue;
      try {
        const obj = JSON.parse(line);
        if (obj && typeof obj === 'object') {
          this.onEvent(obj);
        }
      } catch (_) {
        // Skip non-JSON lines (e.g. [Agent] logs, debug output)
      }
    }
  }

  flush() {
    const line = this.buffer.trim();
    this.buffer = '';
    if (line) {
      try {
        const obj = JSON.parse(line);
        if (obj && typeof obj === 'object') {
          this.onEvent(obj);
        }
      } catch (_) {}
    }
  }
}

class ChatManager {
  constructor({binary, dataDir, getOptions = () => ({}), log = () => {}}) {
    this.binary = binary;
    this.dataDir = dataDir;
    this.getOptions = getOptions;
    this.log = log;
    this.currentJob = null;
  }

  isRunning() {
    return this.currentJob !== null;
  }

  stop() {
    if (this.currentJob) {
      try {
        this.currentJob.kill('SIGTERM');
      } catch (_) {}
      this.currentJob = null;
    }
  }

  async ask(question, {timeoutMs = 180000, onEvent = null} = {}) {
    if (this.currentJob) {
      const err = new Error('Another chat job is in progress');
      err.status = 409;
      throw err;
    }

    if (typeof question !== 'string' || !question.trim()) {
      const err = new Error('question must be a non-empty string');
      err.status = 400;
      throw err;
    }

    const trimmedQuestion = question.trim();
    const store = path.join(this.dataDir, 'lume-ti');
    const docsIndex = path.join(this.dataDir, 'library', 'index');
    const options = (typeof this.getOptions === 'function' ? this.getOptions() : {}) || {};

    const url = (typeof options.chatOllamaUrl === 'string' && options.chatOllamaUrl.trim()) || DEFAULT_CHAT_URL;
    const firstUrl = url.split(',')[0].trim();
    // Messages never repeat the setting's value: users paste the key itself there.
    let apiKey = null;
    if (typeof options.chatApiKeyFile === 'string' && options.chatApiKeyFile.trim()) {
      const keyFile = options.chatApiKeyFile.trim();
      if (looksLikeKey(keyFile)) {
        throw chatError('Chat API Key File Path (chatApiKeyFile) holds what looks like an API key, not a file path. Save the key with the Ask tab\'s key box instead.', 400, 'key_not_path');
      }
      try {
        apiKey = fs.readFileSync(keyFile, 'utf8').trim();
      } catch (readErr) {
        throw chatError(`Failed to read the Ask key file set in Chat API Key File Path (chatApiKeyFile): ${readErr.code || 'unreadable'}. Save the key again with the Ask tab's key box.`, 400, 'key_unreadable');
      }
      if (!apiKey) {
        throw chatError('The Ask key file set in Chat API Key File Path (chatApiKeyFile) is empty. Save the key with the Ask tab\'s key box.', 400, 'key_empty');
      }
    }
    if (!apiKey && needsCloudKey(url)) {
      throw chatError(`Ask needs an API key for ${firstUrl}. Paste one into the Ask tab's key box, or point Chat Ollama API URLs at a model on your own network.`, 400, 'needs_key');
    }

    // Build arguments array: question is passed directly as an argv element, NEVER evaluated through a shell
    const args = ['chat', '--json', '--events', '--ti-store', store, '--docs-index', docsIndex];
    args.push('--ollama-url', url);
    if (options.chatModel) {
      args.push('--ollama-model', options.chatModel);
    }
    args.push(trimmedQuestion);

    const childEnv = {...process.env};
    if (apiKey) {
      childEnv.OLLAMA_API_KEY = apiKey;
    } else {
      delete childEnv.OLLAMA_API_KEY;
    }

    return new Promise((resolve, reject) => {
      let child;
      try {
        child = spawn(this.binary, args, {stdio: ['ignore', 'pipe', 'pipe'], env: childEnv});
      } catch (err) {
        return reject(err);
      }

      this.currentJob = child;
      let stdout = '';
      let stderr = '';
      const parser = onEvent ? new EventStreamParser(onEvent) : null;

      const timer = timeoutMs
        ? setTimeout(() => {
            try {
              child.kill('SIGTERM');
            } catch (_) {}
            const err = new Error('Chat job timed out after 180s');
            err.status = 504;
            this.currentJob = null;
            reject(err);
          }, timeoutMs)
        : null;

      child.stdout.on('data', chunk => {
        stdout += chunk.toString();
      });
      child.stderr.on('data', chunk => {
        stderr += chunk.toString();
        if (parser) {
          parser.feed(chunk);
        }
      });

      child.on('error', err => {
        if (timer) clearTimeout(timer);
        this.currentJob = null;
        reject(err);
      });

      child.on('close', (code, signal) => {
        if (timer) clearTimeout(timer);
        if (parser) {
          parser.flush();
        }
        this.currentJob = null;

        let parsed = null;
        const trimmed = stdout.trim();
        if (trimmed) {
          try {
            parsed = JSON.parse(trimmed);
          } catch (_) {
            const firstBrace = trimmed.indexOf('{');
            const lastBrace = trimmed.lastIndexOf('}');
            if (firstBrace !== -1 && lastBrace !== -1 && lastBrace > firstBrace) {
              try {
                parsed = JSON.parse(trimmed.slice(firstBrace, lastBrace + 1));
              } catch (_) {}
            }
          }
        }

        if (parsed && typeof parsed === 'object') {
          return resolve(parsed);
        }

        if (code !== 0) {
          this.log(`[chat] lume chat exited with code ${code}: ${(stderr || stdout).slice(-2000)}`);
          return reject(explainChatFailure(`${stderr}\n${stdout}`, firstUrl));
        }

        resolve({
          answer: stdout.trim(),
          sql: [],
          tool_calls: [],
        });
      });
    });
  }
}

/**
 * What the Ask tab needs to know before asking: whether a key is set and readable.
 * Never returns the key or the setting's raw value.
 */
function chatSetup(options) {
  const url = (typeof options.chatOllamaUrl === 'string' && options.chatOllamaUrl.trim()) || DEFAULT_CHAT_URL;
  const keyFile = typeof options.chatApiKeyFile === 'string' ? options.chatApiKeyFile.trim() : '';
  let key = 'none';
  if (keyFile) {
    if (looksLikeKey(keyFile)) key = 'pasted_in_path';
    else {
      try {
        key = fs.readFileSync(keyFile, 'utf8').trim() ? 'ok' : 'empty';
      } catch (_) {
        key = 'unreadable';
      }
    }
  }
  return {
    url,
    model: options.chatModel || 'glm-5.3:cloud',
    needsKey: needsCloudKey(url),
    key,
    ready: key === 'ok' || (key === 'none' && !needsCloudKey(url)),
  };
}

/**
 * Write the key to <dataDir>/ollama.key (mode 600, temp + rename) and point
 * chatApiKeyFile at it. The key itself never enters plugin configuration.
 */
function saveChatKey(dataDir, key) {
  const value = typeof key === 'string' ? key.trim() : '';
  if (!value || value.length > 512 || /\s/.test(value)) {
    throw chatError('Paste the whole API key: one line, no spaces.', 400, 'bad_input');
  }
  fs.mkdirSync(dataDir, {recursive: true});
  const target = path.join(dataDir, KEY_FILE_NAME);
  const tmp = `${target}.tmp-${process.pid}`;
  fs.writeFileSync(tmp, `${value}\n`, {mode: 0o600});
  fs.chmodSync(tmp, 0o600);
  fs.renameSync(tmp, target);
  return target;
}

function registerChatRoutes(router, app, getChatManager, {getConfig = () => ({}), onConfigSaved = () => {}} = {}) {
  router.get('/api/chat/setup', (req, res) => {
    const status = adminStatus(app, req);
    if (status !== 200) return res.status(status).json({error: 'Signal K administrator required'});
    res.json(chatSetup(getConfig() || {}));
  });

  let savingKey = false;
  router.post('/api/chat/key', async (req, res) => {
    const status = adminStatus(app, req);
    if (status !== 200) return res.status(status).json({error: 'Signal K administrator required'});
    if (savingKey) return res.status(409).json({error: 'A key is already being saved'});
    savingKey = true;
    try {
      const body = await readJson(req);
      const dataDir = typeof app.getDataDirPath === 'function' ? app.getDataDirPath() : '';
      if (!dataDir) throw chatError('Plugin data folder unavailable', 500, 'no_data_dir');
      const keyFile = saveChatKey(dataDir, body && body.key);
      const current = {...(getConfig() || {})};
      const next = {...current, chatApiKeyFile: keyFile};
      if (!next.chatOllamaUrl) next.chatOllamaUrl = DEFAULT_CHAT_URL;
      await new Promise((resolve, reject) => {
        if (typeof app.savePluginOptions !== 'function') return reject(chatError('Save unavailable', 500, 'no_save'));
        app.savePluginOptions(next, err => (err ? reject(err) : resolve()));
      });
      // Ask reads its options on every question, so no plugin restart is needed.
      onConfigSaved(next);
      res.json({ok: true, ...chatSetup(next)});
    } catch (error) {
      // The body (the key) is never echoed back or logged.
      res.status(error.status || 400).json({error: error.code === 'bad_input' ? error.message : 'The key could not be saved', code: error.code || 'save_failed'});
    } finally {
      savingKey = false;
    }
  });

  router.post('/api/chat', async (req, res) => {
    const status = adminStatus(app, req);
    if (status !== 200) {
      return res.status(status).json({error: 'Signal K administrator required'});
    }

    const chatManager = typeof getChatManager === 'function' ? getChatManager() : getChatManager;
    if (!chatManager) {
      return res.status(503).json({error: 'Chat unavailable: lume binary not resolved'});
    }

    try {
      const body = await readJson(req);
      if (!body || typeof body.question !== 'string') {
        return res.status(400).json({error: 'question is required'});
      }

      const acceptHeader = req.headers && req.headers.accept ? String(req.headers.accept) : '';
      const wantsStream = acceptHeader.includes('application/x-ndjson');

      if (!wantsStream) {
        const result = await chatManager.ask(body.question);
        return res.json(result);
      }

      // Stream application/x-ndjson
      if (typeof res.setHeader === 'function') {
        res.setHeader('Content-Type', 'application/x-ndjson');
        res.setHeader('Cache-Control', 'no-cache, no-transform');
        res.setHeader('Connection', 'keep-alive');
        res.setHeader('X-Accel-Buffering', 'no');
      }
      if (typeof res.status === 'function') {
        res.status(200);
      }

      let keepaliveTimer = null;
      if (typeof res.write === 'function') {
        keepaliveTimer = setInterval(() => {
          try {
            res.write(JSON.stringify({event: 'keepalive'}) + '\n');
          } catch (_) {}
        }, 10000);
        if (typeof keepaliveTimer.unref === 'function') {
          keepaliveTimer.unref();
        }
      }

      const cleanup = () => {
        if (keepaliveTimer) {
          clearInterval(keepaliveTimer);
          keepaliveTimer = null;
        }
      };

      // A client that leaves mid-answer must not keep `lume chat` (and its paid cloud
      // calls) running, or hold the single chat slot for up to 180 s. `res` 'close'
      // before the response ends is the disconnect; req 'close' fires once the body is read.
      if (typeof res.on === 'function') {
        res.on('close', () => {
          cleanup();
          if (!res.writableEnded) {
            if (chatManager && typeof chatManager.stop === 'function') chatManager.stop();
          }
        });
      }

      const onEvent = eventObj => {
        if (typeof res.write === 'function') {
          try {
            res.write(JSON.stringify(eventObj) + '\n');
          } catch (_) {}
        }
      };

      try {
        const result = await chatManager.ask(body.question, {onEvent});
        cleanup();
        if (typeof res.write === 'function') {
          res.write(JSON.stringify({event: 'result', ...result}) + '\n');
        }
        if (typeof res.end === 'function') {
          res.end();
        }
      } catch (err) {
        cleanup();
        if (typeof res.write === 'function' && res.headersSent) {
          try {
            res.write(JSON.stringify({event: 'error', error: err.message, code: err.code, status: err.status || 500}) + '\n');
          } catch (_) {}
          if (typeof res.end === 'function') {
            res.end();
          }
        } else if (typeof res.status === 'function' && typeof res.json === 'function') {
          res.status(err.status || 500).json({error: err.message, code: err.code});
        }
      }
    } catch (error) {
      if (typeof res.status === 'function' && typeof res.json === 'function') {
        res.status(error.status || 500).json({error: error.message});
      }
    }
  });

  router.get('/chat.js', (req, res) => {
    res.sendFile(path.join(__dirname, '..', 'public', 'chat.js'));
  });
}

module.exports = {
  ChatManager,
  registerChatRoutes,
  EventStreamParser,
  chatSetup,
  saveChatKey,
  explainChatFailure,
  needsCloudKey,
  looksLikeKey,
};

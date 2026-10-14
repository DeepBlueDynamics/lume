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

const HISTORY_FILE = 'ask-history.jsonl';
const HISTORY_KEEP = 100;
const shortError = text => String(text || '').replace(/\s+Valid fields are .*$/s, '').slice(0, 300);

/** What the numbers on the Lume TI page mean right now, so Ask can explain them. */
function statusContext(status) {
  if (!status) return '';
  const lines = ['[Context from the Lume TI plugin, not from the user: the plugin status and what each number on its page means.]'];
  lines.push(`- Lume is ${status.running ? `running (pid ${status.pid}, up ${status.uptimeSecs ?? '?'} s)` : 'not running'}.`);
  if (status.updates !== undefined) {
    lines.push(`- "Updates" (top right, also "Updates received" on the Status tab) = ${status.updates}: Signal K value updates lume has received since it started; every change of every path counts once. It is not a row count.`);
  }
  lines.push('- Storage keeps one row per 10-second bucket in the telemetry table, with @mean, @min, @max and @last columns for each Signal K path.');
  if (status.disk) lines.push(`- "Disk Size" = ${status.disk}: the whole store. "WAL Size" = ${status.wal || '0 B'}: the crash-recovery log for buckets not yet sealed into shard files.`);
  if (status.lagSeconds !== undefined) lines.push(`- "Ingest Lag" = ${status.lagSeconds} s: how far behind the live Signal K stream lume is.`);
  lines.push('- The plugin itself is not a table: answer questions about these numbers from this context, not by querying for them.');
  if (status.firstTs && status.lastTs) {
    const spanSeconds = Math.max(0, (Date.parse(status.lastTs) - Date.parse(status.firstTs)) / 1000);
    lines.push(`- Recorded telemetry covers ${status.firstTs} to ${status.lastTs} (${describeSpan(spanSeconds)}, ${status.buckets} buckets). ` +
      'There is no data before or after that.');
    lines.push(`- For a graph, use date_bin with a step that gives roughly 30 to 200 points over the part of that span the user asked about; ` +
      `for the whole span now, about ${suggestStep(spanSeconds)}. Never use a step that leaves fewer than 10 points, and say so if the data covers less time than was asked for.`);
  }
  return `\n\n${lines.join('\n')}`;
}

function describeSpan(seconds) {
  if (seconds < 120) return `${Math.round(seconds)} seconds`;
  if (seconds < 7200) return `${Math.round(seconds / 60)} minutes`;
  if (seconds < 172800) return `${(seconds / 3600).toFixed(1)} hours`;
  return `${(seconds / 86400).toFixed(1)} days`;
}

/** A date_bin step giving about 60 points over the span, from the 10-second bucket up. */
function suggestStep(seconds) {
  const steps = [[10, "INTERVAL '10 seconds'"], [30, "INTERVAL '30 seconds'"], [60, "INTERVAL '1 minute'"], [300, "INTERVAL '5 minutes'"],
    [900, "INTERVAL '15 minutes'"], [1800, "INTERVAL '30 minutes'"], [3600, "INTERVAL '1 hour'"], [10800, "INTERVAL '3 hours'"],
    [21600, "INTERVAL '6 hours'"], [86400, "INTERVAL '1 day'"]];
  const target = seconds / 60;
  return (steps.find(([s]) => s >= target) || steps[steps.length - 1])[1];
}

/**
 * Recent Ask runs, so "which queries broke?" has an answer, and so a query that
 * failed before is flagged instead of tried again.
 */
function historyContext(runs, {maxBad = 12, maxGood = 6} = {}) {
  const all = runs || [];
  const recent = all.slice(-8);
  if (!recent.length) return '';
  const lines = ['[Context from the Lume TI plugin, not from the user: the most recent Ask questions on this boat, oldest first.]'];
  for (const run of recent) {
    lines.push(`- ${run.ts} "${run.question}": ${run.ok ? 'answered' : `failed (${run.error || 'error'})`}`);
  }
  // Each distinct SQL once, newest result wins: a query that later worked is not bad.
  const outcome = new Map();
  for (const run of all) {
    for (const call of run.calls || []) {
      if (!call.sql) continue;
      outcome.set(call.sql.trim(), call.error ? {bad: true, error: call.error} : {bad: false, rows: call.rows});
    }
  }
  const bad = [...outcome].filter(([, o]) => o.bad).slice(-maxBad);
  const good = [...outcome].filter(([, o]) => !o.bad && o.rows).slice(-maxGood);
  if (bad.length) {
    lines.push('DO NOT REUSE: these queries failed on this boat. Never run one of them again as written; fix the cause first, usually a column that does not exist (check ti_schema).');
    for (const [sql, o] of bad) lines.push(`- DO NOT REUSE: ${sql} -> ${o.error}`);
  }
  if (good.length) {
    lines.push('Known good: these queries returned rows here and can be reused.');
    for (const [sql, o] of good) lines.push(`- ${sql} (${o.rows} rows)`);
  }
  return `\n\n${lines.join('\n')}`;
}

/**
 * Appended to the question so the model can map the search index's stored file names
 * to real titles. Bounded so a large library can't blow up argv.
 */
function libraryContext(catalog, maxChars = 24000, suggestions = []) {
  const indexed = Array.isArray(catalog) ? catalog : [];
  const extra = Array.isArray(suggestions) ? suggestions : [];
  if (!indexed.length && !extra.length) return '';
  const lines = [];
  let size = 0;
  const add = line => {
    if (size + line.length > maxChars) return false;
    lines.push(line);
    size += line.length + 1;
    return true;
  };
  add('[Context from the Lume TI plugin, not from the user.]');
  if (indexed.length) {
    add('Library documents indexed on this boat (the sections table stores them by file name; ' +
      'its `line` column is the page number for PDFs):');
    for (let i = 0; i < indexed.length; i++) {
      const doc = indexed[i];
      const ok = add(`- ${doc.file}: "${doc.title}"${doc.publisher ? `, ${doc.publisher}` : ''}${doc.category ? ` (${doc.category})` : ''}` +
        `${doc.localUrl ? `; link ${doc.localUrl}` : ''}`);
      if (!ok) { lines.push(`- …and ${indexed.length - i} more`); break; }
    }
  }
  if (extra.length) {
    add('Catalog documents that match this question but are NOT indexed yet (the user can tick them in the Library tab and press Index selected):');
    for (const doc of extra) {
      if (!add(`- "${doc.title}"${doc.publisher ? `, ${doc.publisher}` : ''}${doc.category ? ` (${doc.category})` : ''}${doc.url ? `; ${doc.url}` : ''}`)) break;
    }
  }
  add('When you use or recommend a document, name it by title and link it in Markdown: an indexed one as ' +
    '[Title, page N](its link#page=N), one that is not indexed as [Title](its URL), saying it can be indexed from the Library tab.');
  return `\n\n${lines.join('\n')}`;
}

class ChatManager {
  constructor({binary, dataDir, getOptions = () => ({}), getCatalog = () => [], getSuggestions = () => [], getStatus = () => null, log = () => {}}) {
    this.binary = binary;
    this.dataDir = dataDir;
    this.getOptions = getOptions;
    this.getCatalog = getCatalog;
    this.getSuggestions = getSuggestions;
    this.getStatus = getStatus;
    this.log = log;
    this.currentJob = null;
  }

  isRunning() {
    return this.currentJob !== null;
  }

  /** The last runs from <dataDir>/ask-history.jsonl (bad lines are skipped). */
  history() {
    try {
      return fs.readFileSync(path.join(this.dataDir, HISTORY_FILE), 'utf8').split('\n').filter(Boolean)
        .map(line => { try { return JSON.parse(line); } catch (_) { return null; } }).filter(Boolean);
    } catch (_) {
      return [];
    }
  }

  /** Append one run, keeping the newest HISTORY_KEEP. Never stores keys or env. */
  record(question, result, error) {
    try {
      const calls = (result && Array.isArray(result.tool_calls) ? result.tool_calls : []).map(c => ({
        tool: c.name || 'tool',
        sql: c.args && typeof c.args.sql === 'string' ? c.args.sql.slice(0, 500) : undefined,
        rows: c.rows ?? undefined,
        error: c.error ? shortError(c.error) : undefined,
      }));
      const run = {ts: new Date().toISOString(), question: question.slice(0, 300), ok: !error, error: error ? shortError(error.message) : undefined, calls};
      const runs = [...this.history(), run].slice(-HISTORY_KEEP);
      fs.mkdirSync(this.dataDir, {recursive: true});
      fs.writeFileSync(path.join(this.dataDir, HISTORY_FILE), runs.map(r => JSON.stringify(r)).join('\n') + '\n');
    } catch (err) {
      this.log(`[chat] could not save Ask history: ${err.message}`);
    }
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
    let catalog = [];
    let suggestions = [];
    try { catalog = (typeof this.getCatalog === 'function' ? this.getCatalog() : []) || []; } catch (_) {}
    try { suggestions = (typeof this.getSuggestions === 'function' ? this.getSuggestions(trimmedQuestion) : []) || []; } catch (_) {}
    // Hold the one chat slot while the status is gathered, so a second question
    // arriving meanwhile is still refused.
    this.currentJob = {kill() {}};
    let status = null;
    try { status = typeof this.getStatus === 'function' ? await this.getStatus() : null; } catch (_) {}
    args.push(trimmedQuestion + statusContext(status) + historyContext(this.history()) + libraryContext(catalog, 24000, suggestions));

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
          this.record(trimmedQuestion, parsed);
          return resolve(parsed);
        }

        if (code !== 0) {
          this.log(`[chat] lume chat exited with code ${code}: ${(stderr || stdout).slice(-2000)}`);
          const failure = explainChatFailure(`${stderr}\n${stdout}`, firstUrl);
          this.record(trimmedQuestion, null, failure);
          return reject(failure);
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
  try {
    fs.writeFileSync(tmp, `${value}\n`, {mode: 0o600});
    fs.chmodSync(tmp, 0o600);
    fs.renameSync(tmp, target);
  } catch (err) {
    // Don't leave a stray copy of the key behind.
    try { fs.unlinkSync(tmp); } catch (_) {}
    throw err;
  }
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
  libraryContext,
  statusContext,
  historyContext,
};

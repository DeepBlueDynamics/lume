'use strict';

const fs = require('node:fs');
const path = require('node:path');
const {spawn} = require('node:child_process');
const {adminStatus, readJson} = require('./pg');

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

  async ask(question, {timeoutMs = 180000} = {}) {
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

    let apiKey = null;
    if (typeof options.chatApiKeyFile === 'string' && options.chatApiKeyFile.trim()) {
      const keyFile = options.chatApiKeyFile.trim();
      try {
        apiKey = fs.readFileSync(keyFile, 'utf8').trim();
      } catch (readErr) {
        const err = new Error(`Failed to read chatApiKeyFile (${keyFile}): ${readErr.message}`);
        err.status = 400;
        throw err;
      }
      if (!apiKey) {
        const err = new Error(`chatApiKeyFile (${keyFile}) is empty`);
        err.status = 400;
        throw err;
      }
    }

    // Build arguments array: question is passed directly as an argv element, NEVER evaluated through a shell
    const args = ['chat', '--json', '--ti-store', store, '--docs-index', docsIndex];
    if (options.chatOllamaUrl) {
      args.push('--ollama-url', options.chatOllamaUrl);
    }
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
      });

      child.on('error', err => {
        if (timer) clearTimeout(timer);
        this.currentJob = null;
        reject(err);
      });

      child.on('close', (code, signal) => {
        if (timer) clearTimeout(timer);
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
          const err = new Error(`Lume chat failed (code ${code}): ${stderr || stdout}`);
          err.status = 500;
          return reject(err);
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

function registerChatRoutes(router, app, getChatManager) {
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
      const result = await chatManager.ask(body.question);
      res.json(result);
    } catch (error) {
      res.status(error.status || 500).json({error: error.message});
    }
  });

  router.get('/chat.js', (req, res) => {
    res.sendFile(path.join(__dirname, '..', 'public', 'chat.js'));
  });
}

module.exports = {
  ChatManager,
  registerChatRoutes,
};

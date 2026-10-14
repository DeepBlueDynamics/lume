'use strict';

document.addEventListener('DOMContentLoaded', () => {
  const form = document.getElementById('chat-form');
  const input = document.getElementById('chat-question');
  const submitBtn = document.getElementById('chat-submit-btn');
  const loading = document.getElementById('chat-loading');
  const container = document.getElementById('chat-response-container');
  const answerEl = document.getElementById('chat-answer');
  const sqlSection = document.getElementById('chat-sql-section');
  const sqlList = document.getElementById('chat-sql-list');
  const errorEl = document.getElementById('chat-error');

  if (!form || !input) return;

  const keyPanel = document.getElementById('chat-key-panel');
  const keyTitle = document.getElementById('chat-key-title');
  const keyDetail = document.getElementById('chat-key-detail');
  const keyForm = document.getElementById('chat-key-form');
  const keyInput = document.getElementById('chat-key-input');
  const keyStatus = document.getElementById('chat-key-status');
  const readyLine = document.getElementById('chat-ready');
  const changeKeyBtn = document.getElementById('chat-change-key');
  const loginHint = 'Sign in to Signal K as an administrator to use Ask: open Signal K Admin, choose Login, then reload this page.';

  const KEY_TEXT = {
    none: ['Ask needs an API key', 'Paste your key from ollama.com/settings/keys. It is saved on this server in a private file, never in the plugin settings.'],
    pasted_in_path: ['Your API key is in the wrong setting', 'It was pasted into Chat API Key File Path, which expects a file path. Paste the key here instead; it will be moved into a private file.'],
    unreadable: ['The saved API key can\'t be read', 'Paste the key again to replace the key file.'],
    empty: ['The saved API key is empty', 'Paste the key again.'],
  };

  function showKeyPanel(state, url) {
    if (!keyPanel) return;
    const [title, detail] = KEY_TEXT[state] || KEY_TEXT.none;
    keyTitle.textContent = title;
    keyDetail.textContent = url && /ollama\.com/.test(url) ? detail : `${detail} (Model server: ${url || 'ollama.com'}.)`;
    keyPanel.hidden = false;
    if (readyLine) readyLine.hidden = true;
  }

  function showReady(setup) {
    if (keyPanel) keyPanel.hidden = true;
    if (!readyLine) return;
    readyLine.hidden = false;
    readyLine.querySelector('.ask-ready-text').textContent =
      `Ready: ${setup.model} on ${setup.url}${setup.key === 'ok' ? ', key saved' : ''}.`;
  }

  async function checkSetup() {
    try {
      const res = await fetch('/plugins/signalk-lume-ti/api/chat/setup', {credentials: 'same-origin', headers: {Accept: 'application/json'}});
      if (res.status === 401 || res.status === 403) {
        if (readyLine) { readyLine.hidden = false; readyLine.querySelector('.ask-ready-text').textContent = loginHint; }
        return;
      }
      if (!res.ok) return;
      const setup = await res.json();
      if (setup.ready) showReady(setup);
      else showKeyPanel(setup.key === 'none' ? 'none' : setup.key, setup.url);
    } catch (_) {
      // Server restarting; the next tab visit checks again.
    }
  }

  if (keyForm) {
    keyForm.addEventListener('submit', async (e) => {
      e.preventDefault();
      const key = keyInput.value.trim();
      if (!key) return;
      keyStatus.textContent = 'Saving…';
      try {
        const res = await fetch('/plugins/signalk-lume-ti/api/chat/key', {
          method: 'POST',
          credentials: 'same-origin',
          headers: {'Content-Type': 'application/json', Accept: 'application/json'},
          body: JSON.stringify({key}),
        });
        const data = await res.json().catch(() => ({}));
        if (res.status === 401 || res.status === 403) throw new Error(loginHint);
        if (!res.ok) throw new Error(data.error || 'The key could not be saved');
        keyInput.value = '';
        keyStatus.textContent = 'Key saved.';
        showReady(data);
      } catch (err) {
        keyStatus.textContent = err.message;
      }
    });
  }
  if (changeKeyBtn) changeKeyBtn.addEventListener('click', () => showKeyPanel('none', ''));

  const askTab = document.querySelector('.tab-btn[data-tab="tab-ask"]');
  if (askTab) askTab.addEventListener('click', checkSetup);
  checkSetup();

  function renderAnswer(text) {
    const md = window.LumeMarkdown;
    if (md && typeof md.render === 'function') answerEl.innerHTML = md.render(text);
    else answerEl.textContent = text;
    addIndexButtons().catch(() => {});
  }

  // A catalog document Ask recommends but this boat hasn't indexed gets an Index
  // button right beside its link, so it can be added without leaving the answer.
  async function addIndexButtons() {
    const links = [...answerEl.querySelectorAll('a[href^="http"]')];
    if (!links.length) return;
    const res = await fetch('/plugins/signalk-lume-ti/api/library', {credentials: 'same-origin', cache: 'no-store'});
    if (!res.ok) return;
    const {items = []} = await res.json();
    const byUrl = new Map(items.map(item => [item.url, item]));
    for (const link of links) {
      const item = byUrl.get(link.getAttribute('href').replace(/#.*$/, ''));
      if (!item) continue;
      const btn = document.createElement('button');
      btn.type = 'button';
      btn.className = 'index-inline-btn';
      if (item.state === 'indexed') {
        btn.textContent = 'Indexed';
        btn.disabled = true;
      } else {
        btn.textContent = 'Index';
        btn.title = `Download "${item.title}" to this server and make it searchable offline`;
        btn.addEventListener('click', () => indexOne(item, btn));
      }
      link.after(btn);
    }
  }

  async function indexOne(item, btn) {
    btn.disabled = true;
    btn.textContent = 'Indexing…';
    try {
      const res = await fetch('/plugins/signalk-lume-ti/api/library/index', {
        method: 'POST',
        credentials: 'same-origin',
        headers: {'Content-Type': 'application/json', Accept: 'application/json'},
        body: JSON.stringify({ids: [item.id]}),
      });
      const data = await res.json().catch(() => ({}));
      if (res.status === 401 || res.status === 403) throw new Error('Needs the Signal K admin login');
      if (!res.ok) throw new Error(data.error || `HTTP ${res.status}`);
      btn.textContent = 'Indexing — see Library tab';
    } catch (err) {
      btn.disabled = false;
      btn.textContent = 'Index';
      btn.title = err.message;
      if (errorEl) {
        errorEl.textContent = `Couldn't index "${item.title}": ${err.message}`;
        errorEl.classList.remove('hidden');
      }
    }
  }

  // Graph the answer: rerun each query that returned rows and chart the time series.
  async function chartAnswer(data) {
    const box = document.getElementById('chat-charts');
    const charts = window.LumeCharts;
    if (!box || !charts) return;
    box.innerHTML = '';
    const calls = Array.isArray(data.tool_calls) ? data.tool_calls : [];
    const statements = (Array.isArray(data.sql) ? data.sql : []).filter(sql => {
      const call = calls.find(c => c.args && typeof c.args.sql === 'string' && c.args.sql.trim() === sql.trim());
      return !call || (!call.error && (call.rows == null || call.rows >= 2));
    }).slice(0, 3);
    for (const sql of statements) {
      const res = await fetch('/plugins/signalk-lume-ti/api/query', {
        method: 'POST',
        credentials: 'same-origin',
        headers: {'Content-Type': 'application/json', Accept: 'application/json'},
        body: JSON.stringify({sql}),
      });
      if (!res.ok) continue;
      const result = await res.json();
      const rows = Array.isArray(result) ? result : result.rows || [];
      const plan = charts.detect(rows);
      if (!plan) continue;
      const figure = document.createElement('div');
      box.appendChild(figure);
      if (!charts.line(figure, rows, plan)) figure.remove();
    }
    if (box.children.length) {
      const heading = document.createElement('h3');
      heading.textContent = box.children.length > 1 ? 'Charts' : 'Chart';
      box.prepend(heading);
    }
  }

  // Errors that the key box can fix open it.
  const KEY_ERRORS = new Set(['needs_key', 'key_not_path', 'key_unreadable', 'key_empty', 'bad_key']);

  // DataFusion appends every column name to a schema error; keep the sentence, drop the list.
  const shortError = text => String(text || '').replace(/s+Valid fields are .*$/s, ' (the Schema Browser lists every column)');

  function formatPreview(args) {
    if (!args || typeof args !== 'object') return '';
    if (typeof args.sql === 'string' && args.sql.trim()) {
      const clean = args.sql.trim().replace(/\s+/g, ' ');
      return clean.length > 80 ? clean.slice(0, 77) + '…' : clean;
    }
    if (typeof args.query === 'string' && args.query.trim()) {
      const clean = args.query.trim().replace(/\s+/g, ' ');
      return clean.length > 80 ? clean.slice(0, 77) + '…' : clean;
    }
    const keys = Object.keys(args).filter(k => k !== 'store');
    if (keys.length > 0) {
      const summary = keys.map(k => `${k}=${JSON.stringify(args[k])}`).join(', ');
      return summary.length > 60 ? summary.slice(0, 57) + '…' : summary;
    }
    return '';
  }

  function renderSqlCards(data) {
    sqlList.innerHTML = '';
    const sqlStatements = Array.isArray(data.sql) ? data.sql : [];
    if (sqlStatements.length > 0) {
      sqlSection.classList.remove('hidden');
      sqlStatements.forEach((sql, idx) => {
        const card = document.createElement('div');
        card.className = 'chat-sql-card card';

        const header = document.createElement('div');
        header.className = 'chat-sql-header';

        const title = document.createElement('span');
        title.className = 'chat-sql-title';
        title.textContent = `Query ${idx + 1}`;

        const openBtn = document.createElement('button');
        openBtn.className = 'secondary-btn chat-open-sql-btn';
        openBtn.textContent = 'Open in SQL Console';
        openBtn.type = 'button';
        openBtn.addEventListener('click', () => {
          const sqlInput = document.getElementById('sql-input');
          if (sqlInput) {
            sqlInput.value = sql;
            sqlInput.focus();
          }
          const consoleTabBtn = document.querySelector('.tab-btn[data-tab="tab-console"]');
          if (consoleTabBtn) {
            consoleTabBtn.click();
          }
        });

        header.appendChild(title);
        header.appendChild(openBtn);

        const pre = document.createElement('pre');
        pre.className = 'chat-sql-code monospace';
        pre.textContent = sql;

        card.appendChild(header);
        card.appendChild(pre);

        if (Array.isArray(data.tool_calls)) {
          const call = data.tool_calls.find(c => c.args && c.args.sql && c.args.sql.trim() === sql.trim());
          if (call) {
            const meta = document.createElement('div');
            meta.className = 'chat-sql-meta';
            if (call.rows !== null && call.rows !== undefined) {
              meta.textContent = `${call.rows} result rows returned${call.truncated ? ' (truncated)' : ''}`;
            } else if (call.error) {
              meta.className += ' chat-sql-meta-error';
              meta.textContent = `Error: ${shortError(call.error)}`;
            }
            card.appendChild(meta);
          }
        }

        sqlList.appendChild(card);
      });
    } else {
      sqlSection.classList.add('hidden');
    }
  }

  form.addEventListener('submit', async (e) => {
    e.preventDefault();
    const question = input.value.trim();
    if (!question) return;

    submitBtn.disabled = true;
    container.classList.add('hidden');
    if (errorEl) {
      errorEl.classList.add('hidden');
      errorEl.textContent = '';
    }

    // Replace static loading message with dynamic events list and status
    loading.classList.remove('hidden');
    loading.innerHTML = '';

    const eventsList = document.createElement('div');
    eventsList.className = 'chat-events-list';

    const statusEl = document.createElement('div');
    statusEl.className = 'chat-thinking-indicator';
    statusEl.textContent = 'Thinking, inspecting schema and executing queries…';

    loading.appendChild(eventsList);
    loading.appendChild(statusEl);

    // Map tool calls by turn + name
    const toolCallItems = new Map();

    function handleEvent(event) {
      if (!event || typeof event !== 'object') return;

      if (event.event === 'thinking') {
        const turnText = event.turn ? ` (turn ${event.turn})` : '';
        statusEl.textContent = `Thinking${turnText}…`;
      } else if (event.event === 'tool_call') {
        statusEl.textContent = `Executing ${event.name || 'tool'}…`;

        const item = document.createElement('div');
        item.className = 'chat-event-item';

        const turnSpan = document.createElement('span');
        turnSpan.className = 'chat-event-turn';
        turnSpan.textContent = event.turn ? `T${event.turn}` : '•';

        const nameSpan = document.createElement('span');
        nameSpan.className = 'chat-event-name';
        nameSpan.textContent = event.name || 'tool';

        const previewText = formatPreview(event.args);
        const previewSpan = document.createElement('span');
        previewSpan.className = 'chat-event-preview monospace';
        previewSpan.textContent = previewText;

        const metaSpan = document.createElement('span');
        metaSpan.className = 'chat-event-meta';
        metaSpan.textContent = 'running…';

        item.appendChild(turnSpan);
        item.appendChild(nameSpan);
        if (previewText) {
          item.appendChild(previewSpan);
        }
        item.appendChild(metaSpan);

        eventsList.appendChild(item);
        const key = `${event.turn || 0}:${event.name || ''}`;
        if (!toolCallItems.has(key)) {
          toolCallItems.set(key, []);
        }
        toolCallItems.get(key).push({item, metaSpan});
      } else if (event.event === 'tool_result') {
        const key = `${event.turn || 0}:${event.name || ''}`;
        const queue = toolCallItems.get(key);
        const record = queue && queue.length > 0 ? queue.shift() : null;

        if (record) {
          const {metaSpan} = record;
          const ms = typeof event.elapsed_ms === 'number' ? `${event.elapsed_ms}ms` : '';
          if (event.error) {
            metaSpan.className = 'chat-event-meta error';
            metaSpan.textContent = `error: ${shortError(event.error)}${ms ? ` (${ms})` : ''}`;
          } else if (event.rows !== null && event.rows !== undefined) {
            metaSpan.className = 'chat-event-meta success';
            metaSpan.textContent = `${event.rows} row${event.rows === 1 ? '' : 's'}${ms ? ` (${ms})` : ''}`;
          } else {
            metaSpan.className = 'chat-event-meta success';
            metaSpan.textContent = `done${ms ? ` (${ms})` : ''}`;
          }
        }
        statusEl.textContent = 'Thinking…';
      }
    }

    try {
      let res;
      try {
        res = await fetch('/plugins/signalk-lume-ti/api/chat', {
          method: 'POST',
          headers: {
            'Content-Type': 'application/json',
            'Accept': 'application/x-ndjson',
          },
          credentials: 'same-origin',
          body: JSON.stringify({ question }),
        });
      } catch (_) {
        throw new Error('Lost the connection to the Signal K server. Check that this device is still on the boat network, then try again.');
      }

      if (res.status === 401 || res.status === 403) throw new Error(loginHint);
      if (!res.ok) {
        let errMessage = `Ask failed (HTTP ${res.status}).`;
        let code;
        try {
          const errData = await res.json();
          if (errData && errData.error) errMessage = errData.error;
          code = errData && errData.code;
        } catch (_) {}
        const err = new Error(errMessage);
        err.code = code;
        throw err;
      }

      const contentType = res.headers.get('content-type') || '';
      let finalData = null;

      if (!contentType.includes('application/x-ndjson') || !res.body) {
        // Non-streaming fallback
        finalData = await res.json();
      } else {
        const reader = res.body.getReader();
        const decoder = new TextDecoder();
        let buffer = '';

        while (true) {
          const {done, value} = await reader.read();
          if (done) break;
          buffer += decoder.decode(value, {stream: true});
          const lines = buffer.split('\n');
          buffer = lines.pop(); // save incomplete line

          for (const line of lines) {
            const trimmed = line.trim();
            if (!trimmed) continue;
            try {
              const eventObj = JSON.parse(trimmed);
              if (eventObj.event === 'result') {
                finalData = eventObj;
              } else if (eventObj.event === 'error') {
                const streamErr = new Error(eventObj.error || 'Ask failed.');
                streamErr.code = eventObj.code;
                throw streamErr;
              } else {
                handleEvent(eventObj);
              }
            } catch (err) {
              if (err.message && !err.message.includes('JSON')) {
                throw err;
              }
            }
          }
        }

        if (buffer.trim()) {
          try {
            const eventObj = JSON.parse(buffer.trim());
            if (eventObj.event === 'result') {
              finalData = eventObj;
            } else if (eventObj.event === 'error') {
              const streamErr = new Error(eventObj.error || 'Ask failed.');
              streamErr.code = eventObj.code;
              throw streamErr;
            } else {
              handleEvent(eventObj);
            }
          } catch (err) {
            if (err.message && !err.message.includes('JSON')) {
              throw err;
            }
          }
        }
      }

      if (!finalData) {
        throw new Error('No answer returned from assistant');
      }

      // Remove the thinking indicator when complete
      statusEl.remove();

      // Render answer
      renderAnswer(finalData.answer || '(No answer returned)');

      // Render SQL statements
      renderSqlCards(finalData);
      chartAnswer(finalData).catch(() => {});

      container.classList.remove('hidden');
    } catch (err) {
      statusEl.remove();
      if (eventsList && !eventsList.children.length) loading.classList.add('hidden');
      if (KEY_ERRORS.has(err.code)) checkSetup();
      if (errorEl) {
        errorEl.textContent = err.message;
        errorEl.classList.remove('hidden');
      }
    } finally {
      submitBtn.disabled = false;
    }
  });
});

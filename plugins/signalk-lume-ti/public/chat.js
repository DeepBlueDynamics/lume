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
              meta.textContent = `Error: ${call.error}`;
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
            metaSpan.textContent = `error: ${event.error}${ms ? ` (${ms})` : ''}`;
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
      const res = await fetch('/plugins/signalk-lume-ti/api/chat', {
        method: 'POST',
        headers: {
          'Content-Type': 'application/json',
          'Accept': 'application/x-ndjson',
        },
        credentials: 'same-origin',
        body: JSON.stringify({ question }),
      });

      if (!res.ok) {
        let errMessage = `Error ${res.status}: ${res.statusText}`;
        try {
          const errData = await res.json();
          if (errData && errData.error) errMessage = errData.error;
        } catch (_) {}
        throw new Error(errMessage);
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
                throw new Error(eventObj.error || 'Chat query failed');
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
              throw new Error(eventObj.error || 'Chat query failed');
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
      answerEl.textContent = finalData.answer || '(No answer returned)';

      // Render SQL statements
      renderSqlCards(finalData);

      container.classList.remove('hidden');
    } catch (err) {
      statusEl.remove();
      if (errorEl) {
        errorEl.textContent = err.message;
        errorEl.classList.remove('hidden');
      } else {
        alert(err.message);
      }
    } finally {
      submitBtn.disabled = false;
    }
  });
});

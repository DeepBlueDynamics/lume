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

  form.addEventListener('submit', async (e) => {
    e.preventDefault();
    const question = input.value.trim();
    if (!question) return;

    submitBtn.disabled = true;
    loading.classList.remove('hidden');
    container.classList.add('hidden');
    if (errorEl) {
      errorEl.classList.add('hidden');
      errorEl.textContent = '';
    }

    try {
      const res = await fetch('/plugins/signalk-lume-ti/api/chat', {
        method: 'POST',
        headers: {
          'Content-Type': 'application/json',
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

      const data = await res.json();

      // Render answer (plain message when unreachable or answers)
      answerEl.textContent = data.answer || '(No answer returned)';

      // Render SQL statements
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

      container.classList.remove('hidden');
    } catch (err) {
      if (errorEl) {
        errorEl.textContent = err.message;
        errorEl.classList.remove('hidden');
      } else {
        alert(err.message);
      }
    } finally {
      submitBtn.disabled = false;
      loading.classList.add('hidden');
    }
  });
});

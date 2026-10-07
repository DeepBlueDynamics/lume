'use strict';

(function () {
  // Base API path: handles both standalone webapp path and plugin route
  // The webapp is served at /signalk-lume-ti/, but the plugin router (and its API) lives at
  // /plugins/signalk-lume-ti/. Requests need the user's Signal K login (same-origin cookie).
  const apiBase = '/plugins/signalk-lume-ti';
  // Signal K guards plugin routes; a 401 means this browser has no Signal K session yet.
  const loginHint = 'Not logged in to Signal K. Open Signal K Admin by its host name (on HaLOS: https://halos.local:4430/admin/), choose Login, sign in (HaLOS SSO if offered), then reload this page from that same address.';

  let lastQueryRows = [];
  let lastQueryColumns = [];
  let lastQuerySql = '';
  let lastQueryTruncated = false;

  document.addEventListener('DOMContentLoaded', () => {
    setupTabs();
    setupPresets();
    setupQueryRunner();
    setupCsvExport();
    setupChartActions();
    setupSchemaBrowser();
    pollStatus();
    setInterval(pollStatus, 3000);
  });

  // 1. Tab Switching
  function setupTabs() {
    const tabs = document.querySelectorAll('.tab-btn');
    tabs.forEach((tab) => {
      tab.addEventListener('click', () => {
        tabs.forEach((t) => t.classList.remove('active'));
        document.querySelectorAll('.tab-panel').forEach((p) => p.classList.remove('active'));

        tab.classList.add('active');
        const targetId = tab.getAttribute('data-tab');
        const panel = document.getElementById(targetId);
        if (panel) {
          panel.classList.add('active');
        }

        if (targetId === 'tab-schema') {
          loadSchema();
        }
      });
    });
  }

  // 2. Query Presets
  function setupPresets() {
    const presets = document.querySelectorAll('.preset-btn');
    const input = document.getElementById('sql-input');
    presets.forEach((btn) => {
      btn.addEventListener('click', () => {
        const query = btn.getAttribute('data-query');
        if (query && input) {
          input.value = query;
          input.focus();
        }
      });
    });
  }

  // 3. Query Runner
  function setupQueryRunner() {
    const runBtn = document.getElementById('run-btn');
    const input = document.getElementById('sql-input');

    if (runBtn && input) {
      runBtn.addEventListener('click', () => runQuery());
      input.addEventListener('keydown', (e) => {
        if ((e.ctrlKey || e.metaKey) && e.key === 'Enter') {
          e.preventDefault();
          runQuery();
        }
      });
    }
  }

  async function runQuery() {
    const input = document.getElementById('sql-input');
    const runBtn = document.getElementById('run-btn');
    const errBox = document.getElementById('query-error');
    const metaSpan = document.getElementById('query-meta');
    const tbody = document.getElementById('results-tbody');
    const thead = document.getElementById('results-thead');
    const countSpan = document.getElementById('results-count');
    const exportBtn = document.getElementById('export-csv-btn');

    const sql = input.value.trim();
    if (!sql) return;

    errBox.classList.add('hidden');
    errBox.textContent = '';
    runBtn.disabled = true;
    runBtn.textContent = 'Running...';
    metaSpan.textContent = '';

    document.getElementById('pin-chart-btn').disabled = true;
    lastQueryRows = [];
    lastQuerySql = '';
    const t0 = performance.now();

    try {
      const res = await fetch(`${apiBase}/api/query`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ sql }),
      });

      const elapsedMs = Math.round(performance.now() - t0);

      if (res.status === 401) throw new Error(loginHint);
      if (!res.ok) {
        const errJson = await res.json().catch(() => ({ error: res.statusText }));
        let message = errJson.error || `HTTP ${res.status}: ${res.statusText}`;
        // DataFusion lists every valid column; keep the hint, drop the wall of names.
        message = message.replace(/ Valid fields are .*$/s, ' (open Schema Browser for the column list)');
        throw new Error(message);
      }

      const data = await res.json();
      const rows = Array.isArray(data) ? data : (data.rows || []);
      lastQueryRows = rows;
      lastQuerySql = sql;
      lastQueryTruncated = Boolean(data.truncated);
      document.getElementById('pin-chart-btn').disabled = !rows.length || lastQueryTruncated || !/\b(intervals|in_bbox)\s*\(/i.test(sql);

      metaSpan.textContent = `Elapsed: ${elapsedMs} ms`;
      countSpan.textContent = `${rows.length} rows`;
      exportBtn.disabled = rows.length === 0;

      if (rows.length === 0) {
        thead.innerHTML = '';
        tbody.innerHTML = '<tr><td class="empty-state">Query returned 0 rows.</td></tr>';
        return;
      }

      // Determine column keys
      const columns = Object.keys(rows[0]);
      lastQueryColumns = columns;

      // Header row
      thead.innerHTML = `<tr>${columns.map((c) => `<th>${escapeHtml(c)}</th>`).join('')}</tr>`;

      // Data rows (render up to 500 rows)
      const displayRows = rows.slice(0, 500);
      tbody.innerHTML = displayRows
        .map((r) => {
          return `<tr>${columns
            .map((c) => {
              const val = r[c];
              const text = val === null || val === undefined ? '<span style="color:var(--text-dim)">null</span>' : escapeHtml(String(val));
              return `<td>${text}</td>`;
            })
            .join('')}</tr>`;
        })
        .join('');
    } catch (err) {
      errBox.textContent = `Error executing query: ${err.message}`;
      errBox.classList.remove('hidden');
      countSpan.textContent = '0 rows';
      exportBtn.disabled = true;
    } finally {
      runBtn.disabled = false;
      runBtn.textContent = '▶ Run Query (Ctrl+Enter)';
    }
  }

  function setupChartActions() {
    const pinBtn = document.getElementById('pin-chart-btn');
    const unpinBtn = document.getElementById('unpin-chart-btn');
    const status = document.getElementById('pin-status');
    const action = async (fn) => {
      pinBtn.disabled = true; unpinBtn.disabled = true;
      try { status.textContent = await fn(); }
      catch (error) { status.textContent = error.message; }
      finally {
        unpinBtn.disabled = false;
        pinBtn.disabled = !lastQuerySql || !lastQueryRows.length || lastQueryTruncated || !/\b(intervals|in_bbox)\s*\(/i.test(lastQuerySql);
      }
    };
    pinBtn.addEventListener('click', () => action(async () => {
      const lat = document.getElementById('pin-lat').value.trim();
      const lon = document.getElementById('pin-lon').value.trim();
      if (Boolean(lat) !== Boolean(lon)) throw new Error('Supply both latitude and longitude.');
      const position = lat && lon ? {latitude:Number(lat),longitude:Number(lon)} : undefined;
      const result = await window.LumeChart.pin({sql:lastQuerySql,rows:lastQueryRows,
        truncated:lastQueryTruncated,position});
      return `Pinned ${result.notes} notes and ${result.regions} regions. Notes without coordinates appear in the resource list.`;
    }));
    unpinBtn.addEventListener('click', () => {
      if (!window.confirm('Remove all chart notes and regions created by lume-ti?')) return;
      action(async () => `Removed ${(await window.LumeChart.unpin()).deleted} lume-ti resources.`);
    });
  }

  // 4. CSV Export
  function setupCsvExport() {
    const exportBtn = document.getElementById('export-csv-btn');
    if (!exportBtn) return;

    exportBtn.addEventListener('click', () => {
      if (!lastQueryRows || lastQueryRows.length === 0) return;

      const cols = lastQueryColumns;
      const headerLine = cols.map(quoteCsv).join(',');
      const rowLines = lastQueryRows.map((r) => cols.map((c) => quoteCsv(r[c])).join(','));
      const csvContent = [headerLine, ...rowLines].join('\r\n');

      const blob = new Blob([csvContent], { type: 'text/csv;charset=utf-8;' });
      const url = URL.createObjectURL(blob);
      const a = document.createElement('a');
      a.href = url;
      a.download = `lume-ti-query-${Date.now()}.csv`;
      document.body.appendChild(a);
      a.click();
      document.body.removeChild(a);
      URL.revokeObjectURL(url);
    });
  }

  function quoteCsv(val) {
    if (val === null || val === undefined) return '';
    const str = String(val);
    if (str.includes(',') || str.includes('"') || str.includes('\n') || str.includes('\r')) {
      return `"${str.replace(/"/g, '""')}"`;
    }
    return str;
  }

  // 5. Schema Browser
  async function setupSchemaBrowser() {
    const refreshBtn = document.getElementById('refresh-schema-btn');
    if (refreshBtn) {
      refreshBtn.addEventListener('click', () => loadSchema());
    }
  }

  async function loadSchema() {
    const tree = document.getElementById('schema-tree');
    const errBox = document.getElementById('schema-error');
    if (!tree) return;

    tree.innerHTML = '<div class="loading-state">Loading schema...</div>';
    errBox.classList.add('hidden');

    try {
      const res = await fetch(`${apiBase}/api/schema`);
      if (res.status === 401) throw new Error(loginHint);
      if (!res.ok) {
        throw new Error(`Failed loading schema: HTTP ${res.status}`);
      }
      const data = await res.json();
      const tables = data.tables || [];

      if (tables.length === 0) {
        tree.innerHTML = '<div class="empty-state">No tables registered yet.</div>';
        return;
      }

      tree.innerHTML = tables
        .map((tbl) => {
          const colRows = (tbl.columns || [])
            .map(
              (c) =>
                `<tr>
                  <td><strong>${escapeHtml(c.name)}</strong></td>
                  <td class="monospace">${escapeHtml(c.data_type || 'unknown')}</td>
                  <td>${escapeHtml(c.unit || '-')}</td>
                  <td>${escapeHtml(String(c.scale ?? '-'))}</td>
                </tr>`
            )
            .join('');

          return `
            <div class="table-card">
              <div class="table-card-header">
                <span>Table: ${escapeHtml(tbl.name)}</span>
                <span style="font-weight:normal;font-size:0.85rem">${tbl.columns ? tbl.columns.length : 0} columns</span>
              </div>
              <table>
                <thead>
                  <tr>
                    <th>Column</th>
                    <th>Data Type</th>
                    <th>Unit</th>
                    <th>Scale</th>
                  </tr>
                </thead>
                <tbody>${colRows}</tbody>
              </table>
            </div>`;
        })
        .join('');
    } catch (err) {
      errBox.textContent = `Error loading schema: ${err.message}`;
      errBox.classList.remove('hidden');
      tree.innerHTML = '';
    }
  }

  // 6. Polling Live Status
  async function pollStatus() {
    try {
      const res = await fetch(`${apiBase}/api/status`);
      const banner = document.getElementById('login-banner');
      if (banner) banner.hidden = res.status !== 401;
      if (res.status === 401) {
        document.getElementById('supervisor-badge').textContent = 'Not logged in';
        return;
      }
      if (!res.ok) return;
      const data = await res.json();

      const sup = data.supervisor || {};
      const store = data.store || {};
      const ingest = store.ingestStatus || {};

      // Header Badge
      const badge = document.getElementById('supervisor-badge');
      if (badge) {
        if (sup.running) {
          badge.className = 'badge online';
          badge.textContent = `Online (PID ${sup.pid})`;
        } else {
          badge.className = 'badge offline';
          badge.textContent = 'Offline';
        }
      }

      // Header Metrics
      setElem('metric-lag', typeof ingest.ingest_lag_seconds === 'number' ? `${ingest.ingest_lag_seconds.toFixed(2)}s` : '--');
      setElem('metric-disk', store.diskHuman || '--');
      setElem('metric-wal', store.walHuman || '--');
      setElem('metric-records', typeof ingest.records_ingested === 'number' ? ingest.records_ingested.toLocaleString() : '--');

      // Status Tab Details
      setElem('diag-state', sup.running ? 'Running' : (sup.lastError ? 'Error' : 'Stopped'));
      setElem('diag-pid', sup.pid || '--');
      setElem('diag-uptime', sup.uptimeSecs ? `${Math.floor(sup.uptimeSecs / 60)}m ${sup.uptimeSecs % 60}s` : '--');
      setElem('diag-restarts', sup.restarts ?? 0);
      setElem('diag-binary', sup.binaryPath || '--');
      setElem('diag-source', data.binary ? data.binary.source : '--');
      setElem('diag-port', sup.servePort || 5863);

      setElem('diag-store-dir', store.exists ? sup.storeDir : `${sup.storeDir} (not initialized)`);
      setElem('diag-disk-bytes', store.diskHuman || '0 B');
      setElem('diag-wal-bytes', store.walHuman || '0 B');
      setElem('diag-shards-bytes', store.shardsBytes ? formatBytes(store.shardsBytes) : '0 B');
      setElem('diag-shard-count', store.shardCount ?? 0);

      setElem('diag-lag', typeof ingest.ingest_lag_seconds === 'number' ? `${ingest.ingest_lag_seconds.toFixed(2)}s` : '--');
      setElem('diag-last-delta', ingest.last_delta || '--');
      setElem('diag-records', typeof ingest.records_ingested === 'number' ? ingest.records_ingested.toLocaleString() : '--');
      setElem('diag-reconnects', ingest.reconnects ?? '--');
      setElem('diag-document-rejections', ingest.documents_rejected_pre_epoch ?? '--');
      setElem('diag-ingest-running', ingest.running !== undefined ? String(ingest.running) : '--');
    } catch (_e) {
      // Network hiccup or server reloading
    }
  }

  function setElem(id, val) {
    const el = document.getElementById(id);
    if (el) el.textContent = val;
  }

  function escapeHtml(str) {
    return String(str)
      .replace(/&/g, '&amp;')
      .replace(/</g, '&lt;')
      .replace(/>/g, '&gt;')
      .replace(/"/g, '&quot;');
  }

  function formatBytes(bytes) {
    if (!bytes || bytes <= 0) return '0 B';
    const units = ['B', 'KB', 'MB', 'GB', 'TB'];
    const i = Math.floor(Math.log(bytes) / Math.log(1024));
    const val = bytes / Math.pow(1024, i);
    return `${val.toFixed(i === 0 ? 0 : 1)} ${units[i]}`;
  }
})();

'use strict';
// Library tab: pick reading-list items (a few preselected), index the ones not yet indexed,
// search the library, and show references for active alerts.
document.addEventListener('DOMContentLoaded', () => {
  const base = '/plugins/signalk-lume-ti/api/library';
  const $ = id => document.getElementById(id);
  const list = $('lib-list');
  const indexBtn = $('lib-index-btn');
  const jobBox = $('lib-job');
  const filter = $('lib-filter');
  const embedPanel = $('lib-embeddings-panel');
  const embedForm = $('lib-embed-form');
  const embedModelSel = $('lib-embed-model');
  const embedDimsSel = $('lib-embed-dimensions');
  const embedDimsWrap = $('lib-embed-dims-wrap');
  const embedEstimate = $('lib-embed-estimate');
  const embedCurrent = $('lib-embed-current');
  const embedStatus = $('lib-embed-status');
  const selected = new Set();
  let items = [];
  let seeded = false;
  let poll = null;
  let lastEmbeddings = null;
  let lastShivvr = null;
  let embedDirty = false;

  const esc = s => String(s ?? '').replace(/[&<>"']/g, c => ({'&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;'}[c]));
  const kb = n => (n == null ? '' : n > 1 << 20 ? `${(n / (1 << 20)).toFixed(1)} MB` : `${Math.max(1, Math.round(n / 1024))} KB`);
  const stateLabel = {indexed: 'Indexed', fetched: 'Fetched', failed: 'Failed', none: 'Not indexed'};
  // The title opens the copy on this server when there is one, otherwise the original.
  const titleLink = item => {
    const href = item.localUrl || item.url;
    return href
      ? `<a href="${esc(href)}" target="_blank" rel="noopener" title="${item.localUrl ? 'Open the copy on this server' : 'Open the original'}">${esc(item.title)}</a>`
      : esc(item.title);
  };

  async function getJson(url, options) {
    const res = await fetch(url, {credentials: 'same-origin', cache: 'no-store', ...options});
    const body = await res.json().catch(() => ({}));
    if (res.status === 401) throw new Error('Not logged in to Signal K. Log in via Signal K Admin (on HaLOS: https://halos.local:4430/admin/, HaLOS SSO), then reload from that address.');
    if (!res.ok) throw new Error(body.error || `HTTP ${res.status}`);
    return body;
  }

  function pending() {
    return [...selected].filter(id => items.find(i => i.id === id)?.state !== 'indexed');
  }

  function renderButton(job) {
    const todo = pending();
    indexBtn.hidden = todo.length === 0 && !job?.running;
    indexBtn.disabled = Boolean(job?.running);
    indexBtn.textContent = job?.running ? `Indexing… (${job.phase})` : `Index ${todo.length} selected`;
  }

  function renderList() {
    const q = filter.value.trim().toLowerCase();
    const groups = {};
    for (const item of items) {
      if (q && !`${item.title} ${item.category} ${item.subcategory} ${item.publisher}`.toLowerCase().includes(q)) continue;
      (groups[item.category || 'Other'] ||= []).push(item);
    }
    list.innerHTML = Object.keys(groups).sort().map(category => `
      <details class="lib-group" ${q || groups[category].some(i => selected.has(i.id)) ? 'open' : ''}>
        <summary>${esc(category)} <span class="lib-count">${groups[category].length}</span></summary>
        ${groups[category].map(item => `
          <label class="lib-row">
            <input type="checkbox" data-id="${item.id}" ${selected.has(item.id) ? 'checked' : ''}>
            <span class="lib-title">${titleLink(item)}${item.default ? ' <span class="lib-default">default</span>' : ''}</span>
            <span class="lib-meta">${esc(item.format.toUpperCase())} · ${esc(item.publisher)}${item.bytes ? ` · ${kb(item.bytes)}` : ''}</span>
            ${item.state === 'none' || item.state === 'failed'
              ? `<button type="button" class="lib-state lib-index-one" data-id="${item.id}" title="${esc(item.error || 'Download this document to the server and make it searchable')}">${item.state === 'failed' ? 'Retry?' : 'Index?'}</button>`
              : `<span class="lib-state lib-${item.state}" title="${esc(item.error || '')}">${stateLabel[item.state]}</span>`}
          </label>`).join('')}
      </details>`).join('') || '<p class="empty-state">No matching items.</p>';
  }

  function renderJob(status) {
    const {job, counts} = status;
    $('lib-counts').textContent = `${counts.indexed} indexed · ${counts.fetched} fetched · ${counts.failed} failed · ${counts.total} in list`;
    if (job.phase === 'idle') { jobBox.hidden = true; return; }
    jobBox.hidden = false;
    jobBox.className = `alert-box ${job.error ? 'error' : ''}`;
    jobBox.textContent = `${job.running ? `Working: ${job.phase}` : `Finished ${job.finishedAt || ''}`}${job.error ? ` — ${job.error}` : ''}\n${job.lines.join('\n')}`;
  }

  const MODEL_LABELS = {
    'embeddinggemma-2': 'EmbeddingGemma 2',
    'gtr-t5-base': 'GTR-T5',
    off: 'Off (keyword only)',
  };

  function formatEstimateText(dims, estimates, sections = 0) {
    const est = estimates && estimates[dims];
    if (est && est.label) return est.label;
    const perKb = Number(dims) === 128 ? '0.5 KB' : `${Math.round((Number(dims) * 4) / 1024)} KB`;
    if (sections > 0) {
      const bytes = sections * (Number(dims) * 4 + 20) + 64;
      const total = bytes >= 100 * 1024 ? `${(bytes / (1024 * 1024)).toFixed(1)} MB` : `${Math.max(1, Math.round(bytes / 1024))} KB`;
      return `≈ ${perKb} per section, ${total} for ${sections.toLocaleString('en-US')} sections`;
    }
    return `≈ ${perKb} per section`;
  }

  function updateEmbedControls() {
    if (!embedModelSel || !embedDimsSel) return;
    const model = embedModelSel.value;
    const estimates = (lastEmbeddings && lastEmbeddings.estimates) || {};
    const sections = (lastEmbeddings && lastEmbeddings.sections) || 0;
    for (const opt of Array.from(embedDimsSel.options || [])) {
      const d = Number(opt.value);
      const def = d === 256 ? ' (default)' : '';
      opt.textContent = `${d}${def} — ${formatEstimateText(d, estimates, sections)}`;
      const allowed = model === 'gtr-t5-base' ? d === 768 : model !== 'off';
      opt.disabled = !allowed;
      opt.hidden = model === 'gtr-t5-base' && d !== 768;
    }
    if (model === 'off') {
      if (embedDimsWrap) embedDimsWrap.hidden = true;
      embedDimsSel.disabled = true;
      if (embedEstimate) embedEstimate.textContent = 'Keyword-only search — no vector file stored.';
    } else if (model === 'gtr-t5-base') {
      if (embedDimsWrap) embedDimsWrap.hidden = false;
      embedDimsSel.value = '768';
      embedDimsSel.disabled = true;
      if (embedEstimate) embedEstimate.textContent = `GTR-T5 uses fixed 768-d vectors (${formatEstimateText(768, estimates, sections)}).`;
    } else {
      if (embedDimsWrap) embedDimsWrap.hidden = false;
      embedDimsSel.disabled = false;
      const dims = Number(embedDimsSel.value) || 256;
      if (embedEstimate) embedEstimate.textContent = `Estimated vector storage: ${formatEstimateText(dims, estimates, sections)}.`;
    }
  }

  function renderEmbeddings(embeddings, shivvr) {
    if (!embeddings || !embedModelSel || !embedDimsSel) return;
    lastEmbeddings = embeddings;
    if (shivvr) lastShivvr = shivvr;

    const activeSet = new Set(
      ((lastShivvr && lastShivvr.models) || [])
        .filter(m => !m.status || m.status === 'active')
        .map(m => m.name)
    );
    for (const opt of Array.from(embedModelSel.options || [])) {
      const baseLabel = MODEL_LABELS[opt.value] || opt.value;
      if (opt.value !== 'off' && lastShivvr && lastShivvr.running && activeSet.size > 0 && !activeSet.has(opt.value)) {
        opt.textContent = `${baseLabel} (not active on Shivvr)`;
      } else {
        opt.textContent = baseLabel;
      }
    }

    const cfg = embeddings.config || {model: 'embeddinggemma-2', dimensions: 256};
    if (!embedDirty) {
      embedModelSel.value = cfg.model || 'embeddinggemma-2';
      embedDimsSel.value = String(cfg.model === 'gtr-t5-base' ? 768 : (cfg.dimensions || 256));
    }
    updateEmbedControls();

    if (embedCurrent) {
      if (embeddings.present) {
        const label = MODEL_LABELS[embeddings.model] || embeddings.model;
        embedCurrent.textContent = `Stored vectors: ${label} · ${embeddings.dimensions}d · ${kb(embeddings.bytes)} (${embeddings.count} sections)`;
      } else if (cfg.model === 'off') {
        embedCurrent.textContent = 'Current mode: Off (keyword only)';
      } else {
        const label = MODEL_LABELS[cfg.model] || cfg.model;
        embedCurrent.textContent = `Configured: ${label} · ${cfg.dimensions}d${embeddings.indexed ? ' (rebuild to generate vectors)' : ''}`;
      }
    }
  }

  async function load() {
    try {
      const data = await getJson(base);
      items = data.items;
      if (!seeded) {
        items.filter(i => i.default || i.state === 'indexed').forEach(i => selected.add(i.id));
        seeded = true;
      }
      renderList();
      renderJob(data);
      renderButton(data.job);
      renderEmbeddings(data.embeddings, data.shivvr);
      if (data.job.running && !poll) poll = setInterval(load, 3000);
      if (!data.job.running && poll) { clearInterval(poll); poll = null; loadReferences(); }
    } catch (error) {
      list.innerHTML = `<div class="alert-box error">${esc(error.message)}</div>`;
    }
  }

  list.addEventListener('change', event => {
    const id = event.target.dataset?.id;
    if (!id) return;
    if (event.target.checked) selected.add(id); else selected.delete(id);
    renderButton();
  });
  filter.addEventListener('input', renderList);

  async function startIndex(ids) {
    try {
      const status = await getJson(`${base}/index`, {method: 'POST', headers: {'Content-Type': 'application/json'},
        body: JSON.stringify({ids})});
      renderJob(status);
      renderButton(status.job);
      if (!poll) poll = setInterval(load, 3000);
      return true;
    } catch (error) {
      jobBox.hidden = false;
      jobBox.className = 'alert-box error';
      jobBox.textContent = /administrator/.test(error.message) ? 'Indexing needs a Signal K administrator login.' : error.message;
      return false;
    }
  }

  indexBtn.addEventListener('click', () => startIndex(pending()));

  // "Index?" on a row indexes just that document.
  list.addEventListener('click', async event => {
    const btn = event.target.closest && event.target.closest('.lib-index-one');
    if (!btn) return;
    event.preventDefault();
    btn.disabled = true;
    btn.textContent = 'Indexing…';
    if (!(await startIndex([btn.dataset.id]))) {
      btn.disabled = false;
      btn.textContent = 'Index?';
    }
  });

  // Open the copy on this server at the matching page; the original stays one click away.
  function hitHtml(hit) {
    const page = Number.isInteger(hit.page) && hit.page > 0 ? `#page=${hit.page}` : '';
    const href = hit.localUrl ? `${hit.localUrl}${page}` : (hit.url ? `${hit.url}${page}` : '');
    const link = href ? `<a href="${esc(href)}" target="_blank" rel="noopener">${esc(hit.title)}</a>` : esc(hit.title);
    const online = hit.localUrl && hit.url
      ? ` · <a class="lib-online" href="${esc(hit.url)}${page}" target="_blank" rel="noopener">original</a>` : '';
    return `<li><strong>${link}</strong> <span class="lib-meta">${esc(hit.section || '')}${hit.publisher ? ` · ${esc(hit.publisher)}` : ''}${online}</span>
      <div class="lib-excerpt">${esc(hit.excerpt)}</div></li>`;
  }

  $('lib-search').addEventListener('submit', async event => {
    event.preventDefault();
    const out = $('lib-results');
    out.innerHTML = '<p class="loading-state">Searching…</p>';
    try {
      const data = await getJson(`${base}/search?q=${encodeURIComponent($('lib-q').value)}`);
      out.innerHTML = data.hits.length ? `<ol class="lib-hits">${data.hits.map(hitHtml).join('')}</ol>`
        : `<p class="empty-state">${esc(data.note || 'No matches.')}</p>`;
    } catch (error) { out.innerHTML = `<div class="alert-box error">${esc(error.message)}</div>`; }
  });

  async function loadReferences() {
    const out = $('lib-references');
    try {
      const data = await getJson(`${base}/references`);
      out.innerHTML = data.references.length ? data.references.map(ref => `
        <div class="card lib-ref">
          <h4>${esc(ref.alert.title || ref.alert.id)} <span class="lib-meta">${esc(ref.why)} · “${esc(ref.query)}”</span></h4>
          ${ref.hits.length ? `<ol class="lib-hits">${ref.hits.map(hitHtml).join('')}</ol>` : `<p class="empty-state">${esc(ref.error || 'No library matches yet.')}</p>`}
        </div>`).join('') : '<p class="empty-state">No active alerts.</p>';
    } catch (error) { out.innerHTML = `<div class="alert-box error">${esc(error.message)}</div>`; }
  }
  $('lib-refresh-refs').addEventListener('click', loadReferences);

  if (embedModelSel) {
    embedModelSel.addEventListener('change', () => {
      embedDirty = true;
      updateEmbedControls();
    });
  }
  if (embedDimsSel) {
    embedDimsSel.addEventListener('change', () => {
      embedDirty = true;
      updateEmbedControls();
    });
  }
  if (embedForm) {
    embedForm.addEventListener('submit', async event => {
      event.preventDefault();
      if (!embedModelSel || !embedDimsSel) return;
      const model = embedModelSel.value;
      const dimensions = model === 'gtr-t5-base' ? 768 : (Number(embedDimsSel.value) || 256);
      const cfg = (lastEmbeddings && lastEmbeddings.config) || {};
      const hasIndexed = Boolean(
        (lastEmbeddings && (lastEmbeddings.indexed || lastEmbeddings.present))
        || items.some(i => i.state === 'indexed' || i.state === 'fetched')
      );
      const changed = model !== cfg.model
        || (model !== 'off' && dimensions !== cfg.dimensions)
        || (lastEmbeddings && lastEmbeddings.present && (lastEmbeddings.model !== model || lastEmbeddings.dimensions !== dimensions));
      if (hasIndexed && changed) {
        const targetDesc = model === 'off'
          ? 'Off (keyword only)'
          : `${MODEL_LABELS[model] || model} (${dimensions}d)`;
        if (!window.confirm(`Switch Library embeddings to ${targetDesc} and rebuild the Library's vectors now? Vectors from different models or sizes cannot be mixed.`)) {
          return;
        }
      }
      if (embedStatus) embedStatus.textContent = 'Saving embedding settings…';
      try {
        const res = await getJson(`${base}/embeddings`, {
          method: 'POST',
          headers: {'Content-Type': 'application/json'},
          body: JSON.stringify({model, dimensions, confirmRebuild: true}),
        });
        embedDirty = false;
        renderJob(res);
        renderButton(res.job);
        renderEmbeddings(res.embeddings, res.shivvr);
        if (embedStatus) {
          embedStatus.textContent = res.job && res.job.running
            ? 'Embedding settings saved — rebuilding Library index…'
            : 'Embedding settings saved.';
        }
        if (res.job && res.job.running && !poll) poll = setInterval(load, 3000);
      } catch (error) {
        if (embedStatus) {
          embedStatus.textContent = /administrator/.test(error.message)
            ? 'Changing embedding settings needs a Signal K administrator login.'
            : error.message;
        }
      }
    });
  }

  window.addEventListener('lume-shivvr-status', event => {
    if (event && event.detail && lastEmbeddings) {
      renderEmbeddings(lastEmbeddings, event.detail);
    }
  });

  // Hide admin-only controls when Signal K reports a non-admin session.
  fetch('/skServer/loginStatus', {credentials: 'same-origin', headers: {Accept: 'application/json'}})
    .then(r => (r.ok ? r.json() : null))
    .then(login => {
      if (login && login.authenticationRequired !== false && login.status === 'loggedIn' && login.userLevel && login.userLevel !== 'admin') {
        if (embedPanel) embedPanel.hidden = true;
      }
    })
    .catch(() => {});

  document.querySelector('[data-tab="tab-library"]').addEventListener('click', () => { load(); loadReferences(); });
});

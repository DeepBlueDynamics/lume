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
  const selected = new Set();
  let items = [];
  let seeded = false;
  let poll = null;

  const esc = s => String(s ?? '').replace(/[&<>"']/g, c => ({'&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;'}[c]));
  const kb = n => (n == null ? '' : n > 1 << 20 ? `${(n / (1 << 20)).toFixed(1)} MB` : `${Math.max(1, Math.round(n / 1024))} KB`);
  const stateLabel = {indexed: 'Indexed', fetched: 'Fetched', failed: 'Failed', none: 'Not indexed'};

  async function getJson(url, options) {
    const res = await fetch(url, {credentials: 'same-origin', cache: 'no-store', ...options});
    const body = await res.json().catch(() => ({}));
    if (res.status === 401) throw new Error('Log in to Signal K (Admin → Login), then reload.');
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
            <span class="lib-title">${esc(item.title)}${item.default ? ' <span class="lib-default">default</span>' : ''}</span>
            <span class="lib-meta">${esc(item.format.toUpperCase())} · ${esc(item.publisher)}${item.bytes ? ` · ${kb(item.bytes)}` : ''}</span>
            <span class="lib-state lib-${item.state}" title="${esc(item.error || '')}">${stateLabel[item.state]}</span>
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

  indexBtn.addEventListener('click', async () => {
    try {
      const status = await getJson(`${base}/index`, {method: 'POST', headers: {'Content-Type': 'application/json'},
        body: JSON.stringify({ids: pending()})});
      renderJob(status);
      renderButton(status.job);
      if (!poll) poll = setInterval(load, 3000);
    } catch (error) {
      jobBox.hidden = false;
      jobBox.className = 'alert-box error';
      jobBox.textContent = /administrator/.test(error.message) ? 'Indexing needs a Signal K administrator login.' : error.message;
    }
  });

  function hitHtml(hit) {
    const link = hit.url ? `<a href="${esc(hit.url)}" target="_blank" rel="noopener">${esc(hit.title)}</a>` : esc(hit.title);
    return `<li><strong>${link}</strong> <span class="lib-meta">${esc(hit.section || '')}${hit.publisher ? ` · ${esc(hit.publisher)}` : ''}</span>
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

  document.querySelector('[data-tab="tab-library"]').addEventListener('click', () => { load(); loadReferences(); });
});

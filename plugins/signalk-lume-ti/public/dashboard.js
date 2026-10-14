'use strict';
// The Dashboard tab: saved widgets, live from Signal K or refreshed from saved queries.
document.addEventListener('DOMContentLoaded', () => {
  const grid = document.getElementById('dash-grid');
  const empty = document.getElementById('dash-empty');
  const message = document.getElementById('dash-message');
  const tab = document.querySelector('.tab-btn[data-tab="tab-dashboard"]');
  if (!grid || !window.LumeGauges) return;
  const base = '/plugins/signalk-lume-ti/api/dashboard';
  let mounted = [];

  function stopAll() {
    for (const m of mounted) { try { m.stop(); } catch (_) {} }
    mounted = [];
    grid.innerHTML = '';
  }

  async function post(path, body) {
    const res = await fetch(`${base}${path}`, {
      method: 'POST', credentials: 'same-origin',
      headers: {'Content-Type': 'application/json', Accept: 'application/json'},
      body: JSON.stringify(body),
    });
    const data = await res.json().catch(() => ({}));
    if (res.status === 401 || res.status === 403) throw new Error('Changing the dashboard needs the Signal K admin login.');
    if (!res.ok) throw new Error(data.error || `HTTP ${res.status}`);
    return data;
  }

  async function load() {
    message.textContent = '';
    let data;
    try {
      const res = await fetch(base, {credentials: 'same-origin', cache: 'no-store'});
      if (!res.ok) throw new Error(res.status === 401 ? 'Sign in to Signal K to see the dashboard.' : `HTTP ${res.status}`);
      data = await res.json();
    } catch (err) {
      message.textContent = err.message;
      return;
    }
    stopAll();
    const widgets = data.widgets || [];
    empty.hidden = widgets.length > 0;
    for (const widget of widgets) {
      const m = window.LumeGauges.mount(grid, widget);
      mounted.push(m);
      const card = grid.lastElementChild;
      card.dataset.id = widget.id;
      const head = document.createElement('div');
      head.className = 'lume-widget-head';
      const title = document.createElement('span');
      title.className = 'lume-widget-title';
      title.textContent = widget.title || widget.options.label || widget.id;
      const remove = document.createElement('button');
      remove.type = 'button';
      remove.className = 'lume-widget-remove';
      remove.title = 'Remove from the dashboard';
      remove.textContent = '×';
      remove.addEventListener('click', async () => {
        if (!window.confirm(`Remove "${title.textContent}" from the dashboard?`)) return;
        try { await post('/remove', {id: widget.id}); load(); }
        catch (err) { message.textContent = err.message; }
      });
      head.append(title, remove);
      card.prepend(head);
    }
  }

  if (tab) tab.addEventListener('click', load);
  window.addEventListener('lume-dashboard-changed', () => {
    if (document.getElementById('tab-dashboard').classList.contains('active')) load();
  });
});

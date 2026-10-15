'use strict';
// The Dashboard tab: saved widgets, live from Signal K or refreshed from saved queries,
// with drag-and-drop reordering, bridge night mode, and stale-value indicators.
(function (root) {
  const NIGHT_STORAGE_KEY = 'lume-dash-night';

  function reorderIds(ids, fromId, toId, placeAfter = false) {
    const list = [...(ids || [])];
    const fromIdx = list.indexOf(fromId);
    const toIdx = list.indexOf(toId);
    if (fromIdx === -1 || toIdx === -1 || fromIdx === toIdx) return list;
    const [moved] = list.splice(fromIdx, 1);
    const targetIdx = list.indexOf(toId);
    list.splice(placeAfter ? targetIdx + 1 : targetIdx, 0, moved);
    return list;
  }

  function applyNightMode(enabled, doc = typeof document !== 'undefined' ? document : null) {
    const on = Boolean(enabled);
    if (!doc) return on;
    const panel = doc.getElementById('tab-dashboard');
    const grid = doc.getElementById('dash-grid');
    const btn = doc.getElementById('dash-night-btn');
    if (doc.body && doc.body.classList) doc.body.classList.toggle('lume-night-mode', on);
    if (panel && panel.classList) panel.classList.toggle('night-mode', on);
    if (grid && grid.classList) grid.classList.toggle('night-mode', on);
    if (btn) {
      btn.setAttribute('aria-pressed', on ? 'true' : 'false');
      btn.textContent = on ? '☀ Day mode' : '🌙 Night mode';
    }
    return on;
  }

  if (typeof document !== 'undefined') {
    document.addEventListener('DOMContentLoaded', () => {
      const grid = document.getElementById('dash-grid');
      const empty = document.getElementById('dash-empty');
      const message = document.getElementById('dash-message');
      const nightBtn = document.getElementById('dash-night-btn');
      const tab = document.querySelector('.tab-btn[data-tab="tab-dashboard"]');
      if (!grid || !window.LumeGauges) return;
      const base = '/plugins/signalk-lume-ti/api/dashboard';
      let mounted = [];
      let draggedCard = null;

      let nightOn = false;
      try { nightOn = window.localStorage.getItem(NIGHT_STORAGE_KEY) === '1'; } catch (_) {}
      applyNightMode(nightOn);
      if (nightBtn) {
        nightBtn.addEventListener('click', () => {
          nightOn = !nightOn;
          applyNightMode(nightOn);
          try { window.localStorage.setItem(NIGHT_STORAGE_KEY, nightOn ? '1' : '0'); } catch (_) {}
        });
      }

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

      function currentOrder() {
        return [...grid.querySelectorAll('.lume-widget')].map(c => c.dataset.id).filter(Boolean);
      }

      async function persistOrder() {
        try {
          await post('/order', {order: currentOrder()});
        } catch (err) {
          message.textContent = err.message;
        }
      }

      function wireCardDrag(card) {
        card.draggable = true;
        card.addEventListener('dragstart', e => {
          draggedCard = card;
          card.classList.add('dragging');
          if (e.dataTransfer) {
            e.dataTransfer.effectAllowed = 'move';
            try { e.dataTransfer.setData('text/plain', card.dataset.id || ''); } catch (_) {}
          }
        });
        card.addEventListener('dragover', e => {
          if (!draggedCard || draggedCard === card) return;
          e.preventDefault();
          if (e.dataTransfer) e.dataTransfer.dropEffect = 'move';
          card.classList.add('drag-over');
          const rect = card.getBoundingClientRect();
          const after = (e.clientX - rect.left) > rect.width / 2 || (e.clientY - rect.top) > rect.height / 2;
          grid.insertBefore(draggedCard, after ? card.nextSibling : card);
        });
        card.addEventListener('dragleave', () => {
          card.classList.remove('drag-over');
        });
        card.addEventListener('drop', async e => {
          e.preventDefault();
          card.classList.remove('drag-over');
          if (draggedCard) {
            await persistOrder();
          }
        });
        card.addEventListener('dragend', () => {
          card.classList.remove('dragging');
          for (const c of grid.querySelectorAll('.lume-widget')) {
            c.classList.remove('drag-over');
          }
          draggedCard = null;
        });
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
          const handle = document.createElement('span');
          handle.className = 'lume-widget-handle';
          handle.title = 'Drag to reorder';
          handle.textContent = '⋮⋮';
          const title = document.createElement('span');
          title.className = 'lume-widget-title';
          title.textContent = widget.title || widget.options.label || widget.id;
          const right = document.createElement('div');
          right.className = 'lume-widget-actions';
          if (m.staleBadge) {
            right.appendChild(m.staleBadge);
          }
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
          right.appendChild(remove);
          head.append(handle, title, right);
          card.prepend(head);
          wireCardDrag(card);
        }
      }

      if (tab) tab.addEventListener('click', load);
      window.addEventListener('lume-dashboard-changed', () => {
        if (document.getElementById('tab-dashboard').classList.contains('active')) load();
      });
    });
  }

  const api = {reorderIds, applyNightMode, NIGHT_STORAGE_KEY};
  if (typeof module !== 'undefined' && module.exports) module.exports = api;
  else root.LumeDashboard = api;
})(typeof window !== 'undefined' ? window : globalThis);

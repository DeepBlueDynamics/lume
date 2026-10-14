'use strict';
// Dashboard widgets: validated, stored in <plugin data>/dashboard.json, and served to
// the Dashboard tab. A widget spec is data only; nothing in it is ever run as code,
// and procedure SQL runs through the read-only /ti/query.
const fs = require('node:fs');
const path = require('node:path');
const {adminStatus, readJson} = require('./pg');

const TYPES = new Set(['tank', 'dial', 'arc', 'bar', 'digital', 'svg']);
const MAX_SVG = 12000;
const VIEWBOX = /^-?\d+(\.\d+)? -?\d+(\.\d+)? \d+(\.\d+)? \d+(\.\d+)?$/;
// The page whitelists elements and attributes again when it draws; the server refuses
// anything that could only be there to run code or load something.
const FORBIDDEN_SVG = /<\s*\/?\s*(script|foreignObject|iframe|object|embed|image|use|a|style|animate\w*|set|feImage|link|meta)\b|\son[a-z]+\s*=|(xlink:)?href\s*=|javascript:|url\s*\(\s*['"]?\s*(?!#)|@import|<!DOCTYPE|<!ENTITY/i;

function cleanSvg(options) {
  const svg = options && typeof options.svg === 'string' ? options.svg : '';
  if (!svg.trim()) throw bad('An svg widget needs options.svg, an SVG fragment');
  if (svg.length > MAX_SVG) throw bad(`options.svg must be at most ${MAX_SVG} characters`);
  if (FORBIDDEN_SVG.test(svg)) throw bad('options.svg may only contain shapes, text and gradients (no script, links, images, styles or event handlers)');
  const viewBox = options.viewBox === undefined ? '0 0 200 200' : String(options.viewBox);
  if (!VIEWBOX.test(viewBox)) throw bad('options.viewBox must be four numbers, such as "0 0 200 200"');
  return {svg, viewBox};
}
const SIZES = new Set(['1x1', '2x1', '1x2', '2x2']);
const ID = /^[a-z0-9][a-z0-9-]{0,39}$/;
const SK_PATH = /^[A-Za-z0-9._]{1,120}$/;
const COLOR = /^#[0-9a-fA-F]{6}$/;
const MAX_WIDGETS = 60;

/** The options each gauge type accepts, and their kinds. Anything else is dropped. */
const OPTION_KINDS = {
  label: 'text', unit: 'text', min: 'number', max: 'number', decimals: 'int', scale: 'number',
  capacity: 'number', warnBelow: 'number', color: 'color', vertical: 'bool', history: 'int', zones: 'zones',
};

function bad(message) {
  return Object.assign(new Error(message), {status: 400});
}

function cleanOptions(options) {
  const out = {};
  for (const [key, value] of Object.entries(options || {})) {
    const kind = OPTION_KINDS[key];
    if (!kind) continue;
    if (kind === 'text' && typeof value === 'string') out[key] = value.slice(0, 40);
    else if (kind === 'number' && Number.isFinite(value)) out[key] = value;
    else if (kind === 'int' && Number.isInteger(value) && value >= 0 && value <= 600) out[key] = value;
    else if (kind === 'color' && typeof value === 'string' && COLOR.test(value)) out[key] = value;
    else if (kind === 'bool' && typeof value === 'boolean') out[key] = value;
    else if (kind === 'zones' && Array.isArray(value)) {
      out[key] = value.slice(0, 6).filter(z => z && Number.isFinite(z.from) && Number.isFinite(z.to) && COLOR.test(String(z.color)))
        .map(z => ({from: z.from, to: z.to, color: z.color}));
    }
  }
  return out;
}

/** Validate and normalise one widget spec, or throw a 400 with the reason. */
function validateWidget(spec) {
  if (!spec || typeof spec !== 'object' || Array.isArray(spec)) throw bad('A widget must be a JSON object');
  if (!ID.test(String(spec.id || ''))) throw bad('id must be lowercase letters, digits and dashes, up to 40');
  if (!TYPES.has(spec.type)) throw bad(`type must be one of ${[...TYPES].join(', ')}`);
  const size = spec.size || '1x1';
  if (!SIZES.has(size)) throw bad(`size must be one of ${[...SIZES].join(', ')}`);
  const source = spec.source || {};
  let cleanSource;
  if (source.kind === 'signalk' && source.paths !== undefined) {
    // Several named inputs, for custom widgets: {"hdg": "navigation.headingTrue", ...}
    const entries = source.paths && typeof source.paths === 'object' && !Array.isArray(source.paths) ? Object.entries(source.paths) : [];
    if (!entries.length || entries.length > 12) throw bad('source.paths must name 1 to 12 Signal K paths');
    for (const [name, p] of entries) {
      if (!/^[A-Za-z_][A-Za-z0-9_]{0,30}$/.test(name)) throw bad('each source.paths name must be a plain identifier');
      if (!SK_PATH.test(String(p))) throw bad(`source.paths.${name} must be a Signal K path`);
    }
    cleanSource = {kind: 'signalk', paths: Object.fromEntries(entries)};
  } else if (source.kind === 'signalk') {
    if (!SK_PATH.test(String(source.path || ''))) throw bad('source.path must be a Signal K path such as environment.wind.speedTrue');
    cleanSource = {kind: 'signalk', path: source.path};
    if (source.field !== undefined) {
      if (!/^[A-Za-z]{1,40}$/.test(String(source.field))) throw bad('source.field must be a plain name');
      cleanSource.field = source.field;
    }
  } else if (source.kind === 'procedure') {
    const sql = typeof source.sql === 'string' ? source.sql.trim() : '';
    if (!sql || sql.length > 4096) throw bad('source.sql must be one read-only query, up to 4 KB');
    if (!/^(select|with)\b/i.test(sql)) throw bad('source.sql must start with SELECT or WITH');
    if (/;\s*\S/.test(sql)) throw bad('source.sql must be a single statement');
    const every = Number.isFinite(source.everySeconds) ? Math.max(2, Math.min(3600, Math.round(source.everySeconds))) : 10;
    cleanSource = {kind: 'procedure', sql: sql.replace(/;\s*$/, ''), everySeconds: every};
    if (source.name !== undefined) {
      if (!ID.test(String(source.name))) throw bad('source.name must be lowercase letters, digits and dashes');
      cleanSource.name = source.name;
    }
    if (source.column !== undefined) {
      if (!/^[A-Za-z0-9_@."$]{1,120}$/.test(String(source.column))) throw bad('source.column must be a column name');
      cleanSource.column = source.column;
    }
  } else {
    throw bad('source.kind must be "signalk" or "procedure"');
  }
  return {
    id: spec.id,
    title: typeof spec.title === 'string' ? spec.title.slice(0, 60) : '',
    type: spec.type,
    size,
    options: spec.type === 'svg' ? {...cleanOptions(spec.options), ...cleanSvg(spec.options)} : cleanOptions(spec.options),
    source: cleanSource,
  };
}

class Dashboard {
  constructor(dataDir) {
    this.file = path.join(dataDir, 'dashboard.json');
  }

  load() {
    try {
      const data = JSON.parse(fs.readFileSync(this.file, 'utf8'));
      return {version: 1, widgets: Array.isArray(data.widgets) ? data.widgets : []};
    } catch (_) {
      return {version: 1, widgets: []};
    }
  }

  save(data) {
    fs.mkdirSync(path.dirname(this.file), {recursive: true});
    const tmp = `${this.file}.tmp-${process.pid}`;
    fs.writeFileSync(tmp, `${JSON.stringify(data, null, 2)}\n`);
    fs.renameSync(tmp, this.file);
  }

  put(spec) {
    const widget = validateWidget(spec);
    const data = this.load();
    const at = data.widgets.findIndex(w => w.id === widget.id);
    if (at >= 0) data.widgets[at] = widget;
    else {
      if (data.widgets.length >= MAX_WIDGETS) throw bad(`A dashboard holds at most ${MAX_WIDGETS} widgets`);
      data.widgets.push(widget);
    }
    this.save(data);
    return widget;
  }

  remove(id) {
    const data = this.load();
    const before = data.widgets.length;
    data.widgets = data.widgets.filter(w => w.id !== id);
    if (data.widgets.length !== before) this.save(data);
    return before !== data.widgets.length;
  }

  move(order) {
    const data = this.load();
    const rank = new Map((order || []).map((id, i) => [id, i]));
    data.widgets.sort((a, b) => (rank.get(a.id) ?? 1e9) - (rank.get(b.id) ?? 1e9));
    this.save(data);
    return data.widgets.map(w => w.id);
  }
}

function registerDashboardRoutes(router, app, getDashboard) {
  const admin = (req, res) => {
    const status = adminStatus(app, req);
    if (status !== 200) { res.status(status).json({error: 'Signal K administrator required'}); return false; }
    return true;
  };
  router.get('/api/dashboard', (req, res) => {
    const dash = getDashboard();
    if (!dash) return res.status(503).json({error: 'Dashboard unavailable'});
    res.json(dash.load());
  });
  router.post('/api/dashboard/validate', async (req, res) => {
    try { res.json({ok: true, widget: validateWidget(await readJson(req))}); }
    catch (error) { res.status(error.status || 400).json({ok: false, error: error.message}); }
  });
  router.post('/api/dashboard/widgets', async (req, res) => {
    if (!admin(req, res)) return;
    const dash = getDashboard();
    if (!dash) return res.status(503).json({error: 'Dashboard unavailable'});
    try { res.json({ok: true, widget: dash.put(await readJson(req))}); }
    catch (error) { res.status(error.status || 400).json({ok: false, error: error.message}); }
  });
  router.post('/api/dashboard/remove', async (req, res) => {
    if (!admin(req, res)) return;
    const dash = getDashboard();
    if (!dash) return res.status(503).json({error: 'Dashboard unavailable'});
    try {
      const body = await readJson(req);
      res.json({ok: dash.remove(String(body.id || ''))});
    } catch (error) { res.status(error.status || 400).json({ok: false, error: error.message}); }
  });
  router.post('/api/dashboard/order', async (req, res) => {
    if (!admin(req, res)) return;
    const dash = getDashboard();
    if (!dash) return res.status(503).json({error: 'Dashboard unavailable'});
    try { res.json({ok: true, order: dash.move((await readJson(req)).order)}); }
    catch (error) { res.status(error.status || 400).json({ok: false, error: error.message}); }
  });
}

module.exports = {Dashboard, validateWidget, registerDashboardRoutes};

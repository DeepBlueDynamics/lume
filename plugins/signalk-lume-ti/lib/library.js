'use strict';
// Offline cruiser library: list the bundled reading list, fetch and index selected rows with
// `lume crawl --list` + `lume index`, search the index through the plugin's running lume server
// (falling back to `lume sql`), and map active alerts
// to reference searches. Row ids match src/crawl_list.rs (FNV-1a 64 of the URL, >> 16).
const fs = require('node:fs');
const http = require('node:http');
const path = require('node:path');
const {spawn} = require('node:child_process');

const LIST = path.join(__dirname, '..', 'library', 'cruiser_library.csv');
const RULES = path.join(__dirname, '..', 'library', 'alert_references.json');
// Small, high-value references preselected in the UI.
const DEFAULT_URLS = [
  'https://www.navcen.uscg.gov/sites/default/files/pdf/navRules/navrules.pdf',
  'https://www.irishlights.ie/media/62985/R1001-Ed20-The-IALA-Maritime-Buoyage-System.pdf',
  'https://www.weather.gov/media/owlie/mw_coastal.pdf',
  'https://www.safeboatingcouncil.org/wp-content/uploads/2020/05/VHF-DSC-Marine-Radio.pdf',
  'https://assets.publishing.service.gov.uk/media/5da849dded915d429bbb9ffe/mcga-shs_capt_guide_chap1.pdf',
  'https://tc.canada.ca/sites/default/files/2023-11/template-abandon-ship.pdf',
  'https://defender.ca/assets/pdf/battle-born/100ah-12v-smart-battery-manual.pdf',
];

function rowId(url) {
  let hash = 0xcbf29ce484222325n;
  for (const byte of Buffer.from(String(url).trim(), 'utf8')) {
    hash ^= BigInt(byte);
    hash = (hash * 0x100000001b3n) & 0xffffffffffffffffn;
  }
  return (hash >> 16n).toString(16).padStart(12, '0');
}

function parseCsv(text) {
  const rows = [];
  let row = [];
  let field = '';
  let quoted = false;
  const s = text.replace(/^﻿/, '');
  for (let i = 0; i < s.length; i++) {
    const c = s[i];
    if (quoted) {
      if (c === '"' && s[i + 1] === '"') { field += '"'; i++; } else if (c === '"') quoted = false; else field += c;
      continue;
    }
    if (c === '"' && field === '') quoted = true;
    else if (c === ',') { row.push(field); field = ''; }
    else if (c === '\n') { row.push(field); field = ''; if (row.some(f => f !== '')) rows.push(row); row = []; }
    else if (c !== '\r') field += c;
  }
  if (quoted) throw new Error('unterminated quoted field');
  if (field !== '' || row.length) { row.push(field); rows.push(row); }
  return rows;
}

function readList(file = LIST) {
  const [header, ...rows] = parseCsv(fs.readFileSync(file, 'utf8'));
  const keys = header.map(h => h.trim().toLowerCase());
  const defaults = new Set(DEFAULT_URLS.map(rowId));
  return rows.map(values => {
    const r = Object.fromEntries(keys.map((k, i) => [k, (values[i] || '').trim()]));
    const id = rowId(r.url);
    return {id, title: r.title, category: r.category, subcategory: r.subcategory, publisher: r.publisher,
      format: (r.format || '').toLowerCase(), url: r.url, notes: r.notes || '', default: defaults.has(id)};
  });
}

function paths(dataDir) {
  const root = path.join(dataDir, 'library');
  return {root, files: path.join(root, 'files'), index: path.join(root, 'index'),
    indexed: path.join(root, 'indexed.json'), manifest: path.join(root, 'files', 'library.json')};
}

/** POST read-only SQL to the plugin's own `lume ti ingest --serve` on loopback. */
function serverQuery(port, sql, timeoutMs = 10000) {
  return new Promise((resolve, reject) => {
    const body = JSON.stringify({sql});
    const req = http.request({host: '127.0.0.1', port, path: '/ti/query', method: 'POST', timeout: timeoutMs,
      headers: {'Content-Type': 'application/json', Accept: 'application/json', 'Content-Length': Buffer.byteLength(body)}},
    res => {
      let data = '';
      res.setEncoding('utf8');
      res.on('data', chunk => { data += chunk; });
      res.on('end', () => {
        let parsed;
        try { parsed = JSON.parse(data); } catch (_) {
          reject(new Error(`HTTP ${res.statusCode}: ${data.slice(0, 200)}`));
          return;
        }
        if (res.statusCode !== 200 || parsed.error) reject(new Error(parsed.error || `HTTP ${res.statusCode}`));
        else resolve(parsed);
      });
    });
    req.on('timeout', () => req.destroy(new Error('timed out')));
    req.on('error', reject);
    req.end(body);
  });
}

function readJson(file, fallback) {
  try { return JSON.parse(fs.readFileSync(file, 'utf8')); } catch (_) { return fallback; }
}

/** Per-row state: indexed, fetched (not yet indexed), failed, or none. */
function listWithState(dataDir, file = LIST) {
  const p = paths(dataDir);
  const manifest = readJson(p.manifest, {entries: {}}).entries || {};
  const indexed = new Set(readJson(p.indexed, {ids: []}).ids);
  return readList(file).map(item => {
    const m = manifest[item.id];
    const state = indexed.has(item.id) ? 'indexed' : m?.status === 'ok' ? 'fetched' : m?.status === 'failed' ? 'failed' : 'none';
    return {...item, state, bytes: m?.bytes ?? null, error: m?.status === 'failed' ? m.error : null};
  });
}

function sqlString(text) {
  // Plain words only: match() takes a BM25 query; strip quotes and control characters.
  return String(text).replace(/[^\p{L}\p{N}\s.\-]/gu, ' ').replace(/\s+/g, ' ').trim().slice(0, 200);
}

function run(binary, args, {timeoutMs = 0, onLine} = {}) {
  return new Promise(resolve => {
    const child = spawn(binary, args, {stdio: ['ignore', 'pipe', 'pipe']});
    let out = '';
    let err = '';
    const timer = timeoutMs ? setTimeout(() => child.kill('SIGTERM'), timeoutMs) : null;
    const feed = (chunk, isErr) => {
      const text = chunk.toString();
      if (isErr) err += text; else out += text;
      if (onLine) text.split(/\r?\n/).filter(Boolean).forEach(line => onLine(line));
    };
    child.stdout.on('data', c => feed(c, false));
    child.stderr.on('data', c => feed(c, true));
    child.on('error', e => { if (timer) clearTimeout(timer); resolve({code: -1, out, err: err + e.message}); });
    child.on('close', code => { if (timer) clearTimeout(timer); resolve({code, out, err}); });
  });
}

class Library {
  constructor({binary, dataDir, list = LIST, rules = RULES, log = () => {}, servePort = null}) {
    this.binary = binary;
    // The plugin's lume server already holds the library index (--docs-index); asking it skips
    // starting a process and loading the index per search (~575 ms on a Pi 5 against tens of ms).
    this.servePort = servePort;
    this.dataDir = dataDir;
    this.list = list;
    this.rules = readJson(rules, {rules: [], fallback: null});
    this.log = log;
    this.job = {running: false, phase: 'idle', ids: [], lines: [], startedAt: null, finishedAt: null, error: null};
  }

  items() { return listWithState(this.dataDir, this.list); }

  status() {
    const items = this.items();
    const count = state => items.filter(i => i.state === state).length;
    return {job: {...this.job, lines: this.job.lines.slice(-12)},
      counts: {total: items.length, indexed: count('indexed'), fetched: count('fetched'), failed: count('failed')}};
  }

  /** Fetch the selected rows, then rebuild the index over every fetched file. */
  async index(ids) {
    if (this.job.running) throw Object.assign(new Error('An indexing job is already running'), {status: 409});
    const known = new Set(readList(this.list).map(i => i.id));
    const wanted = [...new Set(ids)].filter(id => /^[0-9a-f]{12}$/.test(id) && known.has(id));
    if (!wanted.length) throw Object.assign(new Error('Select at least one library item'), {status: 400});
    const p = paths(this.dataDir);
    fs.mkdirSync(p.files, {recursive: true});
    this.job = {running: true, phase: 'fetching', ids: wanted, lines: [], startedAt: new Date().toISOString(), finishedAt: null, error: null};
    const note = line => { this.job.lines.push(line); if (this.job.lines.length > 200) this.job.lines.shift(); this.log(`[library] ${line}`); };
    (async () => {
      const fetched = await run(this.binary, ['crawl', '--list', this.list, '--out', p.files, '--only', wanted.join(',')], {onLine: note});
      // A failed row exits 1 but the others are still usable; index whatever was fetched.
      this.job.phase = 'indexing';
      const indexed = await run(this.binary, ['index', p.files, '--db', p.index], {onLine: note});
      if (indexed.code !== 0) {
        this.job.error = `lume index exited ${indexed.code}`;
      } else {
        const manifest = readJson(p.manifest, {entries: {}}).entries || {};
        const ok = Object.values(manifest).filter(e => e.status === 'ok').map(e => e.id);
        fs.writeFileSync(p.indexed, JSON.stringify({ids: ok, at: new Date().toISOString()}, null, 2));
        if (fetched.code !== 0) this.job.error = 'Some items could not be fetched; see the list for reasons';
      }
      this.job.phase = 'done';
      this.job.running = false;
      this.job.finishedAt = new Date().toISOString();
    })().catch(e => { this.job = {...this.job, running: false, phase: 'done', error: e.message, finishedAt: new Date().toISOString()}; });
    return this.status();
  }

  /** BM25 search over the library index; hits carry the source document's title and URL. */
  async search(query, limit = 8) {
    const p = paths(this.dataDir);
    const q = sqlString(query);
    if (!q) return {query: q, hits: []};
    if (!fs.existsSync(p.index)) return {query: q, hits: [], note: 'Library not indexed yet'};
    const sql = `SELECT file, title, line, score, substr(body, 1, 320) AS excerpt FROM sections WHERE match(body, '${q}') ORDER BY score DESC LIMIT ${Math.min(Math.max(1, limit | 0), 25)}`;
    let parsed = null;
    let via = 'server';
    if (this.servePort) {
      try { parsed = await serverQuery(this.servePort, sql); } catch (e) {
        this.log(`Library search through the lume server failed (${e.message}); using lume sql`);
      }
    }
    if (!parsed) {
      via = 'cli';
      const result = await run(this.binary, ['sql', '--db', p.index, sql, '--format', 'json'], {timeoutMs: 30000});
      if (result.code !== 0) throw new Error((result.err || 'lume sql failed').trim().split('\n').pop());
      parsed = JSON.parse(result.out);
    }
    const rows = Array.isArray(parsed) ? parsed : parsed.rows || [];
    const byFile = Object.fromEntries(this.items().map(i => [i.id, i]));
    return {query: q, via, hits: rows.map(r => {
      const id = path.basename(String(r.file || '')).split('.')[0];
      const source = byFile[id] || {};
      return {title: source.title || r.file, section: r.title, line: r.line, score: r.score,
        excerpt: String(r.excerpt || '').replace(/\s+/g, ' ').trim(), url: source.url || null,
        publisher: source.publisher || null, category: source.category || null};
    })};
  }

  /** Pick the reference query for an alert from the rules file. */
  ruleFor(alert) {
    const text = `${alert.path || ''} ${alert.id || ''} ${alert.title || ''}`;
    for (const rule of this.rules.rules || []) {
      if (new RegExp(rule.match, 'i').test(text)) return rule;
    }
    return this.rules.fallback;
  }

  /** For each active alert, run its mapped search and return the top references. */
  async references(alerts, perAlert = 3) {
    const out = [];
    for (const alert of alerts) {
      const rule = this.ruleFor(alert);
      if (!rule) continue;
      const found = await this.search(rule.query, perAlert).catch(e => ({query: rule.query, hits: [], error: e.message}));
      out.push({alert: {id: alert.id, title: alert.title, path: alert.path, since: alert.ts_start ?? null},
        why: rule.why, query: found.query, hits: found.hits, error: found.error || found.note || null});
    }
    return out;
  }
}

module.exports = {Library, readList, listWithState, parseCsv, rowId, sqlString, DEFAULT_URLS, paths};

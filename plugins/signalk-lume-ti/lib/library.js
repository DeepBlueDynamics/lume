'use strict';
// Offline cruiser library: list the bundled reading list, fetch and index selected rows with
// `lume crawl --list` + `lume index`, search the index through `lume search -a 0.5` when vectors
// and Shivvr are active or through the plugin's running lume server / `lume sql` in keyword mode,
// and map active alerts to reference searches. Row ids match src/crawl_list.rs (FNV-1a 64 >> 16).
const fs = require('node:fs');
const http = require('node:http');
const path = require('node:path');
const {spawn} = require('node:child_process');
const {DEFAULT_SHIVVR_URL, normalizeUrl} = require('./shivvr');
const {
  IMAGE_EXTENSIONS,
  MIME_BY_EXT,
  uploadError,
  sanitizeUploadName,
  userPaths,
  readJsonSafe,
  writeJsonAtomic,
  streamUploadToDisk,
  extractPdfPageImageAsync,
  inspectPdfPages,
  resolveCloudVisionConfig,
  callOllamaCloudVision,
  buildImageSidecarMarkdown,
  buildPdfSidecarMarkdown,
  looksLikeBookIndexPage,
} = require('./uploads');

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
  // How to build Dashboard gauges; Ask reads it when asked for one.
  'https://raw.githubusercontent.com/DeepBlueDynamics/lume/main/plugins/signalk-lume-ti/library/reference/dashboard-gauges.md',
];

const DEFAULT_EMBED_MODEL = 'embeddinggemma-2';
const DEFAULT_EMBED_DIMENSIONS = 256;
const VALID_EMBED_DIMENSIONS = [768, 512, 256, 128];
const EMBED_MODELS = [
  {id: 'embeddinggemma-2', label: 'EmbeddingGemma 2', dimensions: [768, 512, 256, 128], defaultDimensions: 256},
  {id: 'gtr-t5-base', label: 'GTR-T5', dimensions: [768], defaultDimensions: 768},
  {id: 'off', label: 'Off (keyword only)', dimensions: [], defaultDimensions: null},
];

function normalizeEmbedConfig(raw = {}, {strict = false} = {}) {
  const rawModel = raw.libraryEmbedModel ?? raw.model;
  let model = rawModel === undefined || rawModel === null || rawModel === ''
    ? DEFAULT_EMBED_MODEL
    : String(rawModel).trim().toLowerCase();
  if (model === 'gtr' || model === 'gtr-t5') model = 'gtr-t5-base';
  if (!['embeddinggemma-2', 'gtr-t5-base', 'off'].includes(model)) {
    if (strict) {
      throw Object.assign(new Error(`Invalid embedding model "${rawModel}": expected embeddinggemma-2, gtr-t5-base, or off`), {status: 400});
    }
    model = DEFAULT_EMBED_MODEL;
  }

  const rawDims = raw.libraryEmbedDimensions ?? raw.dimensions;
  let dimensions = DEFAULT_EMBED_DIMENSIONS;
  if (model === 'gtr-t5-base') {
    if (strict && rawDims !== undefined && rawDims !== null && Number(rawDims) !== 768) {
      throw Object.assign(new Error('GTR-T5 fixed vector size is 768'), {status: 400});
    }
    dimensions = 768;
  } else if (rawDims !== undefined && rawDims !== null && rawDims !== '') {
    const parsed = Number(rawDims);
    if (!VALID_EMBED_DIMENSIONS.includes(parsed)) {
      if (strict) {
        throw Object.assign(new Error(`Invalid embedding dimensions "${rawDims}": expected one of 768, 512, 256, 128`), {status: 400});
      }
      dimensions = DEFAULT_EMBED_DIMENSIONS;
    } else {
      dimensions = parsed;
    }
  }

  const shivvrUrl = normalizeUrl(raw.shivvrUrl || DEFAULT_SHIVVR_URL);
  return {model, dimensions, shivvrUrl};
}

function estimateVectorStorage(dimensions, sections = 0) {
  const dims = Number(dimensions) || DEFAULT_EMBED_DIMENSIONS;
  const rowBytes = dims * 4;
  const perSectionKb = dims === 128 ? '0.5 KB' : `${Math.round(rowBytes / 1024)} KB`;
  const count = Math.max(0, Number(sections) || 0);
  // Each document entry in local-vectors.bin stores a 4-byte length prefix + 16-byte hash id + 4*D bytes.
  const totalBytes = count > 0 ? count * (rowBytes + 20) + 64 : 0;
  let totalHuman = null;
  if (count > 0) {
    totalHuman = totalBytes >= 100 * 1024
      ? `${(totalBytes / (1024 * 1024)).toFixed(1)} MB`
      : `${Math.max(1, Math.round(totalBytes / 1024))} KB`;
  }
  const label = count > 0
    ? `≈ ${perSectionKb} per section, ${totalHuman} for ${count.toLocaleString('en-US')} sections`
    : `≈ ${perSectionKb} per section`;
  return {
    dimensions: dims,
    sections: count,
    perSectionBytes: rowBytes,
    perSectionHuman: perSectionKb,
    totalBytes,
    totalHuman,
    label,
  };
}

function readBinVectorHeader(vecPath) {
  let fd = null;
  try {
    const stat = fs.statSync(vecPath);
    if (stat.size < 46) return null;
    fd = fs.openSync(vecPath, 'r');
    const buf = Buffer.alloc(Math.min(512, stat.size));
    const read = fs.readSync(fd, buf, 0, buf.length, 0);
    if (read < 46 || buf.subarray(0, 8).toString('ascii') !== 'LUMEVEC1') return null;
    const version = buf.readUInt16LE(8);
    if (version !== 1) return null;
    const dimensions = buf.readUInt32LE(24);
    const count = buf.readUInt32LE(28);
    const modelLen = buf.readUInt16LE(40);
    if (42 + modelLen > read) return null;
    const model = buf.subarray(42, 42 + modelLen).toString('utf8');
    return {
      present: true,
      format: 'bin',
      path: vecPath,
      model,
      dimensions,
      count,
      bytes: stat.size,
    };
  } catch (_) {
    return null;
  } finally {
    if (fd !== null) {
      try { fs.closeSync(fd); } catch (_) {}
    }
  }
}

function inspectIndexVectors(indexDir) {
  const empty = {
    indexed: false,
    sections: 0,
    present: false,
    format: null,
    model: null,
    dimensions: null,
    count: 0,
    bytes: 0,
  };
  if (!indexDir || !fs.existsSync(indexDir)) return empty;

  let sections = 0;
  let generation = null;
  let indexed = false;

  const pointer = readJson(path.join(indexDir, 'index.json'), null);
  if (pointer && typeof pointer === 'object') {
    indexed = true;
    sections = Number(pointer.sections) || 0;
    generation = typeof pointer.generation === 'string' ? pointer.generation : null;
  } else if (fs.existsSync(path.join(indexDir, 'bm25.json')) || fs.existsSync(path.join(indexDir, 'state.json'))) {
    indexed = true;
    const bm25 = readJson(path.join(indexDir, 'bm25.json'), null);
    if (bm25 && Array.isArray(bm25.sections)) sections = bm25.sections.length;
  }

  const candidates = [];
  if (generation) {
    candidates.push(path.join(indexDir, generation, 'local-vectors.bin'));
    candidates.push(path.join(indexDir, generation, 'local-vectors.json'));
  }
  candidates.push(path.join(indexDir, 'local-vectors.bin'));
  candidates.push(path.join(indexDir, 'local-vectors.json'));

  for (const candidate of candidates) {
    if (!fs.existsSync(candidate)) continue;
    if (candidate.endsWith('.bin')) {
      const header = readBinVectorHeader(candidate);
      if (header) {
        return {
          indexed: true,
          sections: sections || header.count,
          ...header,
        };
      }
    } else if (candidate.endsWith('.json')) {
      const parsed = readJson(candidate, null);
      if (parsed && parsed.profile) {
        const stat = fs.statSync(candidate);
        const count = parsed.documents ? Object.keys(parsed.documents).length : 0;
        return {
          indexed: true,
          sections: sections || count,
          present: true,
          format: 'json',
          path: candidate,
          model: parsed.profile.model || null,
          dimensions: Number(parsed.profile.dimensions) || null,
          count,
          bytes: stat.size,
        };
      }
    }
  }

  return {...empty, indexed, sections};
}

function removeVectorFiles(indexDir) {
  if (!indexDir || !fs.existsSync(indexDir)) return;
  for (const name of ['local-vectors.bin', 'local-vectors.json']) {
    try { fs.rmSync(path.join(indexDir, name), {force: true}); } catch (_) {}
  }
  try {
    for (const entry of fs.readdirSync(indexDir, {withFileTypes: true})) {
      if (entry.isDirectory()) {
        for (const name of ['local-vectors.bin', 'local-vectors.json']) {
          try { fs.rmSync(path.join(indexDir, entry.name, name), {force: true}); } catch (_) {}
        }
      }
    }
  } catch (_) {}
}

function parseSearchCliOutput(stdout) {
  const lines = String(stdout || '').split(/\r?\n/);
  const hits = [];
  let current = null;
  const headerRe = /^\[(\d+)\]\s+(?:Hybrid\s+)?Score:\s*([0-9.eE+-]+).*?\|\s*(.*?)\s*\(File:\s*(.+?),\s*Line:\s*(\d+)\)\s*$/;

  const flush = () => {
    if (!current) return;
    const excerpt = current.snippetLines.join(' ').replace(/\s+/g, ' ').trim().slice(0, 320);
    hits.push({
      file: current.file,
      title: current.title,
      line: current.line,
      score: current.score,
      excerpt,
    });
    current = null;
  };

  for (const line of lines) {
    const m = headerRe.exec(line);
    if (m) {
      flush();
      current = {
        rank: Number(m[1]),
        score: Number(m[2]),
        title: m[3].trim(),
        file: m[4].trim(),
        line: Number(m[5]),
        snippetLines: [],
      };
      continue;
    }
    if (!current) continue;
    if (/^\s*Entities:\s*\[/.test(line) && current.snippetLines.length === 0) continue;
    if (/^Facets:/.test(line)) {
      flush();
      break;
    }
    current.snippetLines.push(line);
  }
  flush();
  return hits;
}

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
    // The crawler stores <id>.<ext>; HTML pages are saved as Markdown.
    const ext = {html: 'md', pdf: 'pdf', epub: 'epub', zim: 'zim'}[item.format] || 'txt';
    const stored = `${item.id}.${ext}`;
    const localUrl = fs.existsSync(path.join(p.files, stored)) ? `/plugins/signalk-lume-ti/api/library/file/${stored}` : null;
    return {...item, state, localUrl, bytes: m?.bytes ?? null, error: m?.status === 'failed' ? m.error : null};
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
  constructor({
    binary,
    dataDir,
    list = LIST,
    rules = RULES,
    log = () => {},
    servePort = null,
    getEmbedOptions = () => ({}),
    getShivvrStatus = () => null,
  }) {
    this.binary = binary;
    // The plugin's lume server already holds the library index (--docs-index); asking it skips
    // starting a process and loading the index per search (~575 ms on a Pi 5 against tens of ms).
    this.servePort = servePort;
    this.dataDir = dataDir;
    this.list = list;
    this.rules = readJson(rules, {rules: [], fallback: null});
    this.log = log;
    this.getEmbedOptions = getEmbedOptions;
    this.getShivvrStatus = getShivvrStatus;
    this.job = {running: false, phase: 'idle', ids: [], lines: [], startedAt: null, finishedAt: null, error: null};
  }

  items() {
    return [...this.userItems(), ...listWithState(this.dataDir, this.list)];
  }

  _readUserMeta() {
    const u = userPaths(this.dataDir);
    const parsed = readJsonSafe(u.metaFile, {items: {}});
    return parsed && typeof parsed.items === 'object' && parsed.items ? parsed.items : {};
  }

  _writeUserMeta(itemsMap) {
    const u = userPaths(this.dataDir);
    writeJsonAtomic(u.metaFile, {items: itemsMap});
  }

  _readVisionCache(safeName) {
    const u = userPaths(this.dataDir);
    const cacheFile = path.join(u.cacheDir, `${safeName}.json`);
    const parsed = readJsonSafe(cacheFile, {pages: {}});
    return parsed && typeof parsed.pages === 'object' && parsed.pages ? parsed : {pages: {}};
  }

  _writeVisionCache(safeName, cacheObj) {
    const u = userPaths(this.dataDir);
    const cacheFile = path.join(u.cacheDir, `${safeName}.json`);
    writeJsonAtomic(cacheFile, cacheObj);
  }

  userItems() {
    const u = userPaths(this.dataDir);
    if (!fs.existsSync(u.userDir)) return [];
    const meta = this._readUserMeta();
    let files = [];
    try {
      files = fs.readdirSync(u.userDir);
    } catch (_) {
      return [];
    }
    const fileSet = new Set(files);
    const out = [];
    for (const name of files.sort()) {
      // Skip hidden temp files and .md vision sidecar files that belong to an original image or PDF.
      if (name.startsWith('.') || name.endsWith('.part')) continue;
      if (name.endsWith('.md') && fileSet.has(name.slice(0, -3))) continue;
      let stat;
      try {
        stat = fs.statSync(path.join(u.userDir, name));
        if (!stat.isFile()) continue;
      } catch (_) {
        continue;
      }
      const ext = path.extname(name).toLowerCase();
      const entry = meta[name] || {};
      const visionStatus = entry.visionStatus || (fileSet.has(`${name}.md`) ? 'described' : 'none');
      const visionLabel = visionStatus === 'waiting_for_description'
        ? 'waiting for a description'
        : (visionStatus === 'described' ? 'described' : null);
      out.push({
        id: `user:${name}`,
        userFile: name,
        addedByYou: true,
        title: entry.title || name,
        category: 'Added by you',
        subcategory: '',
        publisher: 'Added by you',
        format: ext.slice(1) || 'txt',
        url: null,
        localUrl: `/plugins/signalk-lume-ti/api/library/file/user/${encodeURIComponent(name)}`,
        bytes: stat.size,
        uploadedAt: entry.uploadedAt || stat.mtime.toISOString(),
        state: entry.state || 'indexed',
        error: entry.error || null,
        visionStatus,
        visionLabel,
        visionError: entry.visionError || null,
        totalPages: entry.totalPages || null,
        describedPages: Array.isArray(entry.describedPages) ? entry.describedPages : [],
        indexPages: Array.isArray(entry.indexPages) ? entry.indexPages : [],
      });
    }
    return out;
  }

  userFile(name) {
    let safeName;
    try {
      safeName = sanitizeUploadName(name);
    } catch (_) {
      return null;
    }
    const u = userPaths(this.dataDir);
    const full = path.join(u.userDir, safeName);
    return fs.existsSync(full) ? full : null;
  }

  userFileMime(name) {
    const ext = path.extname(String(name || '')).toLowerCase();
    return MIME_BY_EXT[ext] || 'application/octet-stream';
  }

  _visionOptions() {
    const raw = typeof this.getEmbedOptions === 'function' ? (this.getEmbedOptions() || {}) : {};
    return resolveCloudVisionConfig(raw, this.dataDir);
  }

  async _describePdfPageWithCache(safeName, pdfPath, pageNumber, cache, prompt) {
    const key = String(pageNumber);
    if (cache.pages && cache.pages[key] && cache.pages[key].text) {
      return {text: cache.pages[key].text, cached: true, method: cache.pages[key].method || 'cache'};
    }
    const extracted = await extractPdfPageImageAsync(pdfPath, pageNumber);
    if (!extracted || !extracted.buffer) {
      throw uploadError(`Page ${pageNumber} has no embedded JPEG and pdftoppm is unavailable`, 422, 'no_page_image');
    }
    const vcfg = this._visionOptions();
    const text = await callOllamaCloudVision({
      ...vcfg,
      imageBuffer: extracted.buffer,
      prompt: prompt || `Transcribe and describe page ${pageNumber} of "${safeName}". Include all headings, tables, numbers, part numbers, and warnings verbatim.`,
    });
    cache.pages[key] = {
      page: pageNumber,
      text,
      method: extracted.method,
      model: vcfg.model,
      at: new Date().toISOString(),
    };
    this._writeVisionCache(safeName, cache);
    return {text, cached: false, method: extracted.method};
  }

  _syncPdfSidecar(safeName, title, cache) {
    const u = userPaths(this.dataDir);
    const pagesMap = new Map(Object.entries(cache.pages || {}));
    if (pagesMap.size === 0) return;
    const md = buildPdfSidecarMarkdown({safeName, title, pagesMap});
    fs.writeFileSync(path.join(u.userDir, `${safeName}.md`), md, 'utf8');
  }

  async _readPdfBookIndexBackwards(safeName, pdfPath, {totalPages, textByPage, cache}) {
    const indexPages = [];
    let waitingOffline = false;
    let offlineError = null;

    for (let page = totalPages; page >= 1; page--) {
      let pageText = textByPage.get(page) || null;
      if (!pageText) {
        try {
          const res = await this._describePdfPageWithCache(
            safeName,
            pdfPath,
            page,
            cache,
            `Transcribe page ${page} of "${safeName}" verbatim. If this page is part of the book's back-of-book alphabetical index, begin with [BOOK_INDEX_PAGE]; if it is normal book prose/chapters before the index, begin with [BOOK_TEXT_START].`
          );
          pageText = res.text;
        } catch (err) {
          if (err && err.offline) {
            waitingOffline = true;
            offlineError = err.message;
          }
          break;
        }
      }
      if (looksLikeBookIndexPage(pageText)) {
        indexPages.push(page);
      } else {
        // Stop walking backwards as soon as the back-of-book index ends and the book text starts.
        break;
      }
    }
    return {indexPages: indexPages.sort((a, b) => a - b), waitingOffline, offlineError};
  }

  async _processUploadVision(safeName, {readBookIndex = false} = {}) {
    const u = userPaths(this.dataDir);
    const fullPath = path.join(u.userDir, safeName);
    const ext = path.extname(safeName).toLowerCase();
    const meta = this._readUserMeta();
    const entry = meta[safeName] || {name: safeName, title: safeName, uploadedAt: new Date().toISOString()};
    entry.readBookIndex = Boolean(readBookIndex || entry.readBookIndex);

    if (IMAGE_EXTENSIONS.has(ext)) {
      const sidecarPath = path.join(u.userDir, `${safeName}.md`);
      try {
        const vcfg = this._visionOptions();
        const imageBuffer = fs.readFileSync(fullPath);
        const description = await callOllamaCloudVision({
          ...vcfg,
          imageBuffer,
          prompt: `Describe this marine image ("${safeName}") and transcribe every visible label, gauge reading, serial number, wiring label, and warning verbatim.`,
        });
        fs.writeFileSync(sidecarPath, buildImageSidecarMarkdown({safeName, title: entry.title || safeName, description}), 'utf8');
        entry.visionStatus = 'described';
        entry.visionError = null;
      } catch (err) {
        if (err && err.offline) {
          entry.visionStatus = 'waiting_for_description';
          entry.visionError = err.message;
        } else {
          entry.visionStatus = 'waiting_for_description';
          entry.visionError = err.message;
          if (err && err.code === 'local_model_disallowed') {
            meta[safeName] = entry;
            this._writeUserMeta(meta);
            throw err;
          }
        }
      }
      meta[safeName] = entry;
      this._writeUserMeta(meta);
      return entry;
    }

    if (ext === '.pdf') {
      const {totalPages, textByPage} = await inspectPdfPages(fullPath, this.binary);
      entry.totalPages = totalPages;
      const cache = this._readVisionCache(safeName);
      let waitingOffline = false;
      let offlineError = null;
      let neededVision = false;

      // Describe the first 5 pages at index time when those pages have no extracted text.
      const firstLimit = Math.min(5, totalPages);
      for (let page = 1; page <= firstLimit; page++) {
        if (textByPage.has(page)) continue;
        neededVision = true;
        try {
          await this._describePdfPageWithCache(safeName, fullPath, page, cache);
        } catch (err) {
          if (err && err.offline) {
            waitingOffline = true;
            offlineError = err.message;
            break;
          }
          if (err && err.code === 'local_model_disallowed') {
            throw err;
          }
        }
      }

      if (!waitingOffline && entry.readBookIndex) {
        neededVision = true;
        const idxRes = await this._readPdfBookIndexBackwards(safeName, fullPath, {totalPages, textByPage, cache});
        entry.indexPages = idxRes.indexPages;
        if (idxRes.waitingOffline) {
          waitingOffline = true;
          offlineError = idxRes.offlineError;
        }
      }

      this._syncPdfSidecar(safeName, entry.title || safeName, cache);
      entry.describedPages = Object.keys(cache.pages || {}).map(Number).filter(n => n > 0).sort((a, b) => a - b);

      if (waitingOffline) {
        entry.visionStatus = 'waiting_for_description';
        entry.visionError = offlineError || 'Waiting for Ollama cloud connection';
      } else if (entry.describedPages.length > 0) {
        entry.visionStatus = 'described';
        entry.visionError = null;
      } else if (neededVision) {
        entry.visionStatus = 'waiting_for_description';
        entry.visionError = 'No embedded page images available';
      } else {
        entry.visionStatus = 'none';
        entry.visionError = null;
      }

      meta[safeName] = entry;
      this._writeUserMeta(meta);
      return entry;
    }

    entry.visionStatus = 'none';
    entry.visionError = null;
    meta[safeName] = entry;
    this._writeUserMeta(meta);
    return entry;
  }

  /**
   * Stream an uploaded file to `<library>/files/user/<safeName>`, describe images / scanned PDFs
   * with Ollama Cloud vision (keeping the original untouched and writing `<safeName>.md` beside it),
   * and run an incremental `lume index` update.
   */
  async upload(req, {name, title, readBookIndex = false} = {}) {
    const safeName = sanitizeUploadName(name);
    const ext = path.extname(safeName).toLowerCase();
    const u = userPaths(this.dataDir);
    const destPath = path.join(u.userDir, safeName);
    const {bytes} = await streamUploadToDisk(req, destPath, ext);

    const meta = this._readUserMeta();
    meta[safeName] = {
      name: safeName,
      title: String(title || safeName).trim() || safeName,
      bytes,
      uploadedAt: new Date().toISOString(),
      state: 'fetched',
      readBookIndex: Boolean(readBookIndex),
      visionStatus: 'none',
      visionError: null,
      totalPages: null,
      describedPages: [],
      indexPages: [],
    };
    this._writeUserMeta(meta);

    const entry = await this._processUploadVision(safeName, {readBookIndex});

    const p = paths(this.dataDir);
    fs.mkdirSync(p.files, {recursive: true});
    const indexed = await this._runIndexStep(p, {force: false});
    const updatedMeta = this._readUserMeta();
    if (updatedMeta[safeName]) {
      updatedMeta[safeName].state = indexed.code === 0 ? 'indexed' : 'fetched';
      if (indexed.code !== 0) {
        updatedMeta[safeName].error = `lume index exited ${indexed.code}`;
      }
      this._writeUserMeta(updatedMeta);
    }

    const item = this.userItems().find(i => i.userFile === safeName) || entry;
    return {
      ok: true,
      item,
      ...this.status(),
    };
  }

  /**
   * Describe a single PDF page on demand. Only calls Ollama Cloud vision when the page
   * has no extracted text in the PDF and is not already in the per-page cache.
   */
  async describePdfPageOnDemand(rawName, pageNumber) {
    const safeName = sanitizeUploadName(rawName);
    if (path.extname(safeName).toLowerCase() !== '.pdf') {
      throw uploadError('On-demand page description is only supported for PDF files', 400, 'not_pdf');
    }
    const pdfPath = this.userFile(safeName);
    if (!pdfPath) {
      throw uploadError(`PDF "${safeName}" not found in user uploads`, 404, 'not_found');
    }
    const page = Math.max(1, Number(pageNumber) | 0);
    const cache = this._readVisionCache(safeName);
    if (cache.pages && cache.pages[String(page)] && cache.pages[String(page)].text) {
      return {
        ok: true,
        name: safeName,
        page,
        text: cache.pages[String(page)].text,
        source: 'vision-cache',
        cached: true,
      };
    }

    const {totalPages, textByPage} = await inspectPdfPages(pdfPath, this.binary);
    if (page > totalPages) {
      throw uploadError(`Page ${page} exceeds PDF page count (${totalPages})`, 400, 'page_out_of_range');
    }
    if (textByPage.has(page)) {
      return {
        ok: true,
        name: safeName,
        page,
        text: textByPage.get(page),
        source: 'extracted-text',
        cached: true,
      };
    }

    const res = await this._describePdfPageWithCache(safeName, pdfPath, page, cache);
    const meta = this._readUserMeta();
    const entry = meta[safeName] || {name: safeName, title: safeName};
    this._syncPdfSidecar(safeName, entry.title || safeName, cache);
    entry.totalPages = totalPages;
    entry.describedPages = Object.keys(cache.pages || {}).map(Number).filter(n => n > 0).sort((a, b) => a - b);
    entry.visionStatus = 'described';
    entry.visionError = null;
    meta[safeName] = entry;
    this._writeUserMeta(meta);

    const p = paths(this.dataDir);
    await this._runIndexStep(p, {force: false});

    return {
      ok: true,
      name: safeName,
      page,
      text: res.text,
      source: 'vision',
      cached: res.cached,
      method: res.method,
    };
  }

  /**
   * Read a scanned PDF's back-of-book index from the last page backwards until the index ends
   * and normal book prose starts.
   */
  async describePdfBookIndex(rawName) {
    const safeName = sanitizeUploadName(rawName);
    if (path.extname(safeName).toLowerCase() !== '.pdf') {
      throw uploadError('Back-of-book index scan is only supported for PDF files', 400, 'not_pdf');
    }
    const pdfPath = this.userFile(safeName);
    if (!pdfPath) {
      throw uploadError(`PDF "${safeName}" not found in user uploads`, 404, 'not_found');
    }
    const {totalPages, textByPage} = await inspectPdfPages(pdfPath, this.binary);
    const cache = this._readVisionCache(safeName);
    const idxRes = await this._readPdfBookIndexBackwards(safeName, pdfPath, {totalPages, textByPage, cache});

    const meta = this._readUserMeta();
    const entry = meta[safeName] || {name: safeName, title: safeName};
    this._syncPdfSidecar(safeName, entry.title || safeName, cache);
    entry.totalPages = totalPages;
    entry.readBookIndex = true;
    entry.indexPages = idxRes.indexPages;
    entry.describedPages = Object.keys(cache.pages || {}).map(Number).filter(n => n > 0).sort((a, b) => a - b);
    if (idxRes.waitingOffline) {
      entry.visionStatus = 'waiting_for_description';
      entry.visionError = idxRes.offlineError;
    } else if (entry.describedPages.length > 0) {
      entry.visionStatus = 'described';
      entry.visionError = null;
    }
    meta[safeName] = entry;
    this._writeUserMeta(meta);

    const p = paths(this.dataDir);
    await this._runIndexStep(p, {force: false});

    return {
      ok: true,
      name: safeName,
      totalPages,
      indexPages: idxRes.indexPages,
      describedPages: entry.describedPages,
      visionStatus: entry.visionStatus,
    };
  }

  /**
   * Retry any user uploads marked `waiting_for_description` when internet connectivity returns.
   */
  async retryPendingVision() {
    const meta = this._readUserMeta();
    const pendingNames = Object.keys(meta).filter(name => meta[name]?.visionStatus === 'waiting_for_description');
    if (pendingNames.length === 0) return {retried: 0, described: 0};

    let described = 0;
    for (const safeName of pendingNames) {
      if (!this.userFile(safeName)) continue;
      try {
        const updated = await this._processUploadVision(safeName, {
          readBookIndex: Boolean(meta[safeName]?.readBookIndex),
        });
        if (updated.visionStatus === 'described') {
          described++;
        }
      } catch (_) {
        // Keep waiting_for_description if still offline.
      }
    }
    if (described > 0) {
      const p = paths(this.dataDir);
      await this._runIndexStep(p, {force: false});
    }
    return {retried: pendingNames.length, described};
  }

  embedConfig() {
    const raw = typeof this.getEmbedOptions === 'function' ? (this.getEmbedOptions() || {}) : {};
    const cfg = normalizeEmbedConfig(raw);
    const shivvr = typeof this.getShivvrStatus === 'function' ? this.getShivvrStatus() : null;
    const shivvrUp = shivvr ? Boolean(shivvr.running && shivvr.state === 'online') : false;
    return {...cfg, shivvrUp};
  }

  vectorStatus() {
    const p = paths(this.dataDir);
    const vec = inspectIndexVectors(p.index);
    const cfg = this.embedConfig();
    const sections = vec.sections || 0;
    const estimates = Object.fromEntries(
      VALID_EMBED_DIMENSIONS.map(d => [d, estimateVectorStorage(d, sections)])
    );
    return {
      ...vec,
      config: cfg,
      models: EMBED_MODELS,
      estimates,
      activeEstimate: cfg.model === 'off' ? null : estimateVectorStorage(cfg.dimensions, sections),
    };
  }

  status() {
    const items = this.items();
    const count = state => items.filter(i => i.state === state).length;
    return {
      job: {...this.job, lines: this.job.lines.slice(-12)},
      counts: {total: items.length, indexed: count('indexed'), fetched: count('fetched'), failed: count('failed')},
      embeddings: this.vectorStatus(),
    };
  }

  buildIndexArgs(p, {force = false, dbPath = p.index} = {}) {
    const cfg = this.embedConfig();
    const args = ['index', p.files, '--db', dbPath];
    if (force) args.push('-f');
    if (cfg.model !== 'off') {
      args.push('--embed-model', cfg.model, '--embed-dimensions', String(cfg.dimensions), '--shivvr-url', cfg.shivvrUrl);
    }
    return args;
  }

  async _runIndexStep(p, {force = false, note = () => {}} = {}) {
    const cfg = this.embedConfig();
    const existingVec = inspectIndexVectors(p.index);
    const profileChanged = existingVec.present
      && (existingVec.model !== cfg.model || existingVec.dimensions !== cfg.dimensions);
    const fullRebuild = Boolean(force || profileChanged || cfg.model === 'off');
    const useStagingSwap = fullRebuild && fs.existsSync(p.index);
    const targetDb = useStagingSwap
      ? `${p.index}.rebuild-${process.pid}-${Date.now()}`
      : p.index;

    if (!useStagingSwap && (cfg.model === 'off' || profileChanged)) {
      removeVectorFiles(p.index);
    }

    const args = this.buildIndexArgs(p, {force: fullRebuild, dbPath: targetDb});
    let indexed = await run(this.binary, args, {onLine: note});
    if (indexed.code !== 0 && cfg.model !== 'off' && /shivvr|embed|ECONNREFUSED|connect|timed out/i.test(`${indexed.out}\n${indexed.err}`)) {
      note('Shivvr embedding server unreachable; indexing keyword-only until Shivvr is running');
      indexed = await run(this.binary, ['index', p.files, '--db', targetDb, ...(fullRebuild ? ['-f'] : [])], {onLine: note});
    }

    if (useStagingSwap) {
      if (indexed.code === 0 && fs.existsSync(targetDb)) {
        const backupDb = `${p.index}.prev-${process.pid}-${Date.now()}`;
        try {
          fs.renameSync(p.index, backupDb);
          fs.renameSync(targetDb, p.index);
          fs.rmSync(backupDb, {recursive: true, force: true});
        } catch (err) {
          // Restore previous index if swap failed midway
          if (!fs.existsSync(p.index) && fs.existsSync(backupDb)) {
            try { fs.renameSync(backupDb, p.index); } catch (_) {}
          }
          try { fs.rmSync(targetDb, {recursive: true, force: true}); } catch (_) {}
          throw err;
        }
      } else {
        // Keep the existing index intact when a rebuild fails so search stays available.
        try { fs.rmSync(targetDb, {recursive: true, force: true}); } catch (_) {}
      }
    }
    return indexed;
  }

  /** Fetch the selected rows, then rebuild the index over every fetched file. */
  async index(ids, {force = false} = {}) {
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
      const indexed = await this._runIndexStep(p, {force, note});
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

  /** Rebuild the existing Library index in place (e.g. after changing embedding model or vector size). */
  async rebuild() {
    if (this.job.running) throw Object.assign(new Error('An indexing job is already running'), {status: 409});
    const p = paths(this.dataDir);
    if (!fs.existsSync(p.files)) {
      return this.status();
    }
    this.job = {running: true, phase: 'rebuilding', ids: [], lines: [], startedAt: new Date().toISOString(), finishedAt: null, error: null};
    const note = line => { this.job.lines.push(line); if (this.job.lines.length > 200) this.job.lines.shift(); this.log(`[library] ${line}`); };
    (async () => {
      const indexed = await this._runIndexStep(p, {force: true, note});
      if (indexed.code !== 0) {
        this.job.error = `lume index exited ${indexed.code}`;
      }
      this.job.phase = 'done';
      this.job.running = false;
      this.job.finishedAt = new Date().toISOString();
    })().catch(e => { this.job = {...this.job, running: false, phase: 'done', error: e.message, finishedAt: new Date().toISOString()}; });
    return this.status();
  }

  /**
   * Search over the library index: hybrid (`lume search -a 0.5`) when local vectors exist and
   * Shivvr is online, or BM25 keyword search through the lume server / `lume sql` otherwise.
   */
  async search(query, limit = 8) {
    const p = paths(this.dataDir);
    const q = sqlString(query);
    if (!q) return {query: q, mode: 'keyword', hits: []};
    if (!fs.existsSync(p.index)) return {query: q, mode: 'keyword', hits: [], note: 'Library not indexed yet'};

    const clampedLimit = Math.min(Math.max(1, limit | 0), 25);
    const cfg = this.embedConfig();
    const vec = inspectIndexVectors(p.index);
    const useHybrid = cfg.model !== 'off' && vec.present && cfg.shivvrUp;

    let rows = null;
    let via = 'server';
    let mode = 'keyword';

    if (useHybrid) {
      const hybridRes = await run(
        this.binary,
        ['search', '--db', p.index, '-l', String(clampedLimit), '-a', '0.5', '--shivvr-url', cfg.shivvrUrl, '--', q],
        {timeoutMs: 30000}
      );
      if (hybridRes.code === 0) {
        rows = parseSearchCliOutput(hybridRes.out);
        via = 'hybrid';
        mode = 'hybrid';
      } else {
        this.log(`Hybrid library search failed (${(hybridRes.err || '').trim() || `exit ${hybridRes.code}`}); falling back to keyword search`);
      }
    }

    if (!rows) {
      const sql = `SELECT file, title, line, score, substr(body, 1, 320) AS excerpt FROM sections WHERE match(body, '${q}') ORDER BY score DESC LIMIT ${clampedLimit}`;
      let parsed = null;
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
      rows = Array.isArray(parsed) ? parsed : parsed.rows || [];
    }

    const allItems = this.items();
    const byFile = Object.fromEntries(allItems.map(i => [i.id, i]));
    const byUserFile = Object.fromEntries(allItems.filter(i => i.userFile).map(i => [i.userFile, i]));
    return {query: q, mode, via, hits: rows.map(r => {
      const rawFile = String(r.file || '').replace(/\\/g, '/');
      const name = path.basename(rawFile);
      const isUserPath = /(?:^|\/)user\/[^/]+$/i.test(rawFile)
        || Boolean(byUserFile[name])
        || (name.endsWith('.md') && Boolean(byUserFile[name.slice(0, -3)]));

      if (isUserPath) {
        const originalName = (name.endsWith('.md') && byUserFile[name.slice(0, -3)])
          ? name.slice(0, -3)
          : name;
        const userSource = byUserFile[originalName] || {};
        const isPdf = /\.pdf$/i.test(originalName);
        const isSidecarHit = name !== originalName;
        let page = null;
        if (isPdf) {
          const titlePageMatch = /^Page\s+(\d+)\b/i.exec(String(r.title || '').trim());
          if (titlePageMatch) {
            page = Number(titlePageMatch[1]);
          } else if (!isSidecarHit && Number.isInteger(r.line) && r.line > 0) {
            page = r.line;
          }
        }
        return {
          title: userSource.title || originalName,
          section: r.title,
          line: r.line,
          page,
          score: r.score,
          excerpt: String(r.excerpt || '').replace(/\s+/g, ' ').trim(),
          url: null,
          localUrl: this.userFile(originalName)
            ? `/plugins/signalk-lume-ti/api/library/file/user/${encodeURIComponent(originalName)}`
            : null,
          publisher: userSource.publisher || 'Added by you',
          category: userSource.category || 'Added by you',
          addedByYou: true,
        };
      }

      const id = name.split('.')[0];
      const source = byFile[id] || {};
      // PDF sections are pages, numbered from 1, so the viewer can open at #page=N.
      const page = /\.pdf$/i.test(name) && Number.isInteger(r.line) && r.line > 0 ? r.line : null;
      return {title: source.title || r.file, section: r.title, line: r.line, page, score: r.score,
        excerpt: String(r.excerpt || '').replace(/\s+/g, ' ').trim(), url: source.url || null,
        localUrl: this.file(name) ? `/plugins/signalk-lume-ti/api/library/file/${name}` : null,
        publisher: source.publisher || null, category: source.category || null};
    })};
  }

  /**
   * Indexed documents with the stored file name the search index uses, so answers
   * can name "Navigation Rules" instead of dc45ebdf001f.pdf.
   */
  catalog() {
    let stored = [];
    try { stored = fs.readdirSync(paths(this.dataDir).files); } catch (_) {}
    const byId = new Map(stored.filter(n => /^[0-9a-f]{12}\./i.test(n)).map(n => [n.slice(0, 12), n]));
    const bundled = listWithState(this.dataDir, this.list).filter(i => i.state === 'indexed' && byId.has(i.id)).map(i => ({
      file: byId.get(i.id), title: i.title, publisher: i.publisher || '', category: i.category || '',
      localUrl: `/plugins/signalk-lume-ti/api/library/file/${byId.get(i.id)}`, url: i.url || '',
    }));
    const uploaded = this.userItems().filter(i => i.state === 'indexed').map(i => ({
      file: `user/${i.userFile}`,
      title: i.title,
      publisher: 'Added by you',
      category: 'Added by you',
      localUrl: i.localUrl,
      url: '',
    }));
    return [...uploaded, ...bundled];
  }

  /**
   * Catalog documents that are not indexed yet but match the question's words, so
   * Ask can point to what could be added. Simple word overlap on the catalog fields.
   */
  suggestions(question, limit = 12) {
    const stop = new Set(['the', 'and', 'for', 'you', 'that', 'with', 'have', 'what', 'which', 'can', 'are', 'any',
      'documents', 'document', 'index', 'indexed', 'library', 'about', 'there', 'this', 'from', 'how', 'our', 'your',
      'will', 'would', 'should', 'could', 'does', 'need', 'want', 'help', 'when', 'where', 'them', 'they', 'then',
      'than', 'into', 'some', 'more', 'make', 'like', 'just', 'also', 'know', 'tell', 'show', 'find', 'give', 'manual',
      'manuals', 'guide', 'guides', 'book', 'books']);
    const words = [...new Set(String(question || '').toLowerCase().match(/[\p{L}\p{N}]{3,}/gu) || [])]
      .filter(w => !stop.has(w))
      .filter(w => w.length >= 4)
      .map(w => w.replace(/(ing|ers|er|es|s)$/, '')).filter(w => w.length >= 4);
    if (!words.length) return [];
    return this.items()
      .filter(i => i.state !== 'indexed')
      .map(i => {
        // A topic in the category ("Engines › Diesel") says more than a word in a title.
        const hay = `${i.title} ${i.category} ${i.subcategory} ${i.publisher} ${i.notes}`.toLowerCase();
        const title = String(i.title || '').toLowerCase();
        const topic = `${i.category} ${i.subcategory}`.toLowerCase();
        const score = words.reduce((s, w) =>
          s + (topic.includes(w) ? 3 : 0) + (title.includes(w) ? 2 : 0) + (hay.includes(w) ? 1 : 0), 0);
        return {item: i, score};
      })
      .filter(s => s.score > 0)
      .sort((a, b) => b.score - a.score)
      // Keep the strong matches: at least half the best score.
      .filter((s, _, all) => s.score * 2 >= all[0].score)
      .slice(0, limit)
      .map(({item}) => ({title: item.title, category: [item.category, item.subcategory].filter(Boolean).join(' › '),
        publisher: item.publisher || '', format: item.format || '', url: item.url || ''}));
  }

  /** The downloaded copy of a library document, by its stored name, or null. */
  file(name) {
    if (!/^[0-9a-f]{12}\.[a-z0-9]{2,5}$/i.test(String(name || ''))) return null;
    const full = path.join(paths(this.dataDir).files, name);
    return fs.existsSync(full) ? full : null;
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

module.exports = {
  Library,
  readList,
  listWithState,
  parseCsv,
  rowId,
  sqlString,
  DEFAULT_URLS,
  paths,
  serverQuery,
  EMBED_MODELS,
  DEFAULT_EMBED_MODEL,
  DEFAULT_EMBED_DIMENSIONS,
  VALID_EMBED_DIMENSIONS,
  normalizeEmbedConfig,
  estimateVectorStorage,
  inspectIndexVectors,
  parseSearchCliOutput,
};

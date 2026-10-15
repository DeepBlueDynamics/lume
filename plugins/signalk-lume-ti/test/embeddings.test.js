'use strict';

const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

const {
  Library,
  paths,
  normalizeEmbedConfig,
  estimateVectorStorage,
  inspectIndexVectors,
  parseSearchCliOutput,
  DEFAULT_EMBED_MODEL,
  DEFAULT_EMBED_DIMENSIONS,
} = require('../lib/library');
const pluginFactory = require('../index');

function makeFakeVecBin({model = 'embeddinggemma-2', dimensions = 256, count = 3} = {}) {
  const header = Buffer.alloc(40);
  header.write('LUMEVEC1', 0, 'ascii');
  header.writeUInt16LE(1, 8); // version
  header.writeUInt16LE(0, 10); // flags
  header.writeUInt32LE(0x01020304, 12); // endian
  header.writeUInt32LE(dimensions, 24);
  header.writeUInt32LE(count, 28);
  header.writeUInt32LE(0, 32); // query count
  header.writeUInt32LE(0, 36); // reserved

  const strField = s => {
    const b = Buffer.from(s, 'utf8');
    const len = Buffer.alloc(2);
    len.writeUInt16LE(b.length, 0);
    return Buffer.concat([len, b]);
  };
  const body = Buffer.concat([
    header,
    strField(model),
    strField('document'),
    strField('query'),
  ]);
  body.writeBigUInt64LE(BigInt(body.length), 16);
  return body;
}

test('normalizeEmbedConfig enforces defaults, selectable sizes, and GTR-T5 fixed 768', () => {
  assert.deepEqual(normalizeEmbedConfig({}), {
    model: 'embeddinggemma-2',
    dimensions: 256,
    shivvrUrl: 'http://127.0.0.1:8085',
  });
  for (const dims of [768, 512, 256, 128]) {
    assert.equal(normalizeEmbedConfig({model: 'embeddinggemma-2', dimensions: dims}, {strict: true}).dimensions, dims);
  }
  assert.equal(normalizeEmbedConfig({model: 'gtr', dimensions: 768}, {strict: true}).model, 'gtr-t5-base');
  assert.equal(normalizeEmbedConfig({model: 'gtr-t5-base'}, {strict: true}).dimensions, 768);
  assert.throws(() => normalizeEmbedConfig({model: 'gtr-t5-base', dimensions: 256}, {strict: true}), /GTR-T5 fixed vector size is 768/);
  assert.throws(() => normalizeEmbedConfig({model: 'embeddinggemma-2', dimensions: 300}, {strict: true}), /Invalid embedding dimensions/);
  assert.throws(() => normalizeEmbedConfig({model: 'unknown-model'}, {strict: true}), /Invalid embedding model/);
  assert.equal(normalizeEmbedConfig({model: 'off'}, {strict: true}).model, 'off');
});

test('estimateVectorStorage computes per-section and library storage estimates', () => {
  const est768 = estimateVectorStorage(768, 1926);
  assert.equal(est768.label, '≈ 3 KB per section, 5.7 MB for 1,926 sections');
  const est256 = estimateVectorStorage(256, 1926);
  assert.equal(est256.label, '≈ 1 KB per section, 1.9 MB for 1,926 sections');
  assert.equal(estimateVectorStorage(512, 0).label, '≈ 2 KB per section');
  assert.equal(estimateVectorStorage(128, 0).label, '≈ 0.5 KB per section');
});

test('inspectIndexVectors reads LUMEVEC1 binary header and generation pointers', () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'lume-vec-inspect-'));
  const genDir = path.join(dir, 'gen-0001');
  fs.mkdirSync(genDir, {recursive: true});
  fs.writeFileSync(path.join(dir, 'index.json'), JSON.stringify({
    format_version: 4,
    generation: 'gen-0001',
    sections: 1926,
    source_files: 8,
  }));
  const vecBytes = makeFakeVecBin({model: 'embeddinggemma-2', dimensions: 256, count: 1926});
  fs.writeFileSync(path.join(genDir, 'local-vectors.bin'), vecBytes);

  const info = inspectIndexVectors(dir);
  assert.equal(info.indexed, true);
  assert.equal(info.present, true);
  assert.equal(info.format, 'bin');
  assert.equal(info.model, 'embeddinggemma-2');
  assert.equal(info.dimensions, 256);
  assert.equal(info.count, 1926);
  assert.equal(info.sections, 1926);
  assert.equal(info.bytes, vecBytes.length);

  fs.rmSync(dir, {recursive: true, force: true});
});

test('Library.buildIndexArgs passes --embed-model, --embed-dimensions, and --shivvr-url unless off', () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'lume-idx-args-'));
  const p = paths(dir);
  let cfg = {libraryEmbedModel: 'embeddinggemma-2', libraryEmbedDimensions: 256, shivvrUrl: 'http://127.0.0.1:8085'};
  const lib = new Library({
    binary: process.execPath,
    dataDir: dir,
    getEmbedOptions: () => cfg,
  });

  assert.deepEqual(lib.buildIndexArgs(p), [
    'index', p.files, '--db', p.index,
    '--embed-model', 'embeddinggemma-2',
    '--embed-dimensions', '256',
    '--shivvr-url', 'http://127.0.0.1:8085',
  ]);

  cfg = {libraryEmbedModel: 'gtr-t5-base', shivvrUrl: 'http://192.168.1.50:8085'};
  assert.deepEqual(lib.buildIndexArgs(p, {force: true}), [
    'index', p.files, '--db', p.index, '-f',
    '--embed-model', 'gtr-t5-base',
    '--embed-dimensions', '768',
    '--shivvr-url', 'http://192.168.1.50:8085',
  ]);

  cfg = {libraryEmbedModel: 'off'};
  assert.deepEqual(lib.buildIndexArgs(p), ['index', p.files, '--db', p.index]);

  fs.rmSync(dir, {recursive: true, force: true});
});

test('parseSearchCliOutput parses hybrid and lexical CLI output', () => {
  const sample = [
    'Searching corpus: /tmp/files (1926 sections, db: /tmp/index)',
    'Executing hybrid search (alpha=0.5, graph=0.4)...',
    '[1] Hybrid Score: 4.1250 (BM25: 2.5000, Semantic: 0.8125, SKG: 0.40) | Page 12 (File: dc45ebdf001f.pdf, Line: 12)',
    'Raw water impeller inspection and replacement steps.',
    '',
    '[2] Hybrid Score: 2.7500 (BM25: 1.5000, Semantic: 0.6250, SKG: 0.00) | Cooling System (File: 123456789abc.md, Line: 45)',
    'Check sea strainer for debris.',
    '',
  ].join('\n');
  const hits = parseSearchCliOutput(sample);
  assert.equal(hits.length, 2);
  assert.deepEqual(hits[0], {
    file: 'dc45ebdf001f.pdf',
    title: 'Page 12',
    line: 12,
    score: 4.125,
    excerpt: 'Raw water impeller inspection and replacement steps.',
  });
  assert.equal(hits[1].file, '123456789abc.md');
});

test('Library.search runs hybrid search (-a 0.5) when vectors exist and Shivvr is online, and keyword otherwise', async () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'lume-hybrid-search-'));
  const p = paths(dir);
  fs.mkdirSync(p.index, {recursive: true});
  fs.mkdirSync(p.files, {recursive: true});
  fs.writeFileSync(path.join(p.files, 'dc45ebdf001f.pdf'), '%PDF-1.4');
  fs.writeFileSync(path.join(p.index, 'index.json'), JSON.stringify({
    format_version: 4,
    generation: 'gen-0001',
    sections: 10,
    source_files: 1,
  }));
  fs.writeFileSync(
    path.join(p.index, 'local-vectors.bin'),
    makeFakeVecBin({model: 'embeddinggemma-2', dimensions: 256, count: 10})
  );

  // Create a mock lume script that records its args and outputs hybrid results for `search` and JSON for `sql`.
  const argsLog = path.join(dir, 'lume-args.jsonl');
  const fakeLume = path.join(dir, process.platform === 'win32' ? 'fake-lume.cmd' : 'fake-lume.sh');
  const helperJs = path.join(dir, 'fake-lume.js');
  fs.writeFileSync(helperJs, [
    'const fs = require("fs");',
    `fs.appendFileSync(${JSON.stringify(argsLog)}, JSON.stringify(process.argv.slice(2)) + "\\n");`,
    'if (process.argv[2] === "search") {',
    '  console.log("Searching corpus: files (10 sections, db: index)");',
    '  console.log("Executing hybrid search (alpha=0.5, graph=0.4)...");',
    '  console.log("[1] Hybrid Score: 3.9000 (BM25: 2.0000, Semantic: 0.9500, SKG: 0.00) | Page 7 (File: dc45ebdf001f.pdf, Line: 7)");',
    '  console.log("Hybrid match on navigation lights.");',
    '  console.log("");',
    '} else if (process.argv[2] === "sql") {',
    '  console.log(JSON.stringify([{file: "dc45ebdf001f.pdf", title: "Page 2", line: 2, score: 1.5, excerpt: "Keyword match"}]));',
    '}',
  ].join('\n'));
  fs.writeFileSync(fakeLume, `#!/bin/sh\nexec "${process.execPath}" "${helperJs}" "$@"\n`, {mode: 0o755});

  let shivvrState = {running: true, state: 'online'};
  let embedModel = 'embeddinggemma-2';
  const lib = new Library({
    binary: fakeLume,
    dataDir: dir,
    getEmbedOptions: () => ({libraryEmbedModel: embedModel, libraryEmbedDimensions: 256, shivvrUrl: 'http://127.0.0.1:8085'}),
    getShivvrStatus: () => shivvrState,
  });

  // 1. Vectors exist + Shivvr online -> hybrid search (-a 0.5)
  const hybridRes = await lib.search('navigation lights', 5);
  assert.equal(hybridRes.mode, 'hybrid');
  assert.equal(hybridRes.via, 'hybrid');
  assert.equal(hybridRes.hits.length, 1);
  assert.equal(hybridRes.hits[0].page, 7);
  assert.equal(hybridRes.hits[0].excerpt, 'Hybrid match on navigation lights.');
  const seenHybridArgs = JSON.parse(fs.readFileSync(argsLog, 'utf8').trim().split('\n').pop());
  assert.ok(seenHybridArgs.includes('search'));
  assert.ok(seenHybridArgs.includes('-a') && seenHybridArgs.includes('0.5'));
  assert.ok(seenHybridArgs.includes('--shivvr-url') && seenHybridArgs.includes('http://127.0.0.1:8085'));

  // 2. Shivvr goes offline -> keyword search
  shivvrState = {running: false, state: 'offline'};
  const kwRes = await lib.search('navigation lights', 5);
  assert.equal(kwRes.mode, 'keyword');
  assert.equal(kwRes.via, 'cli');
  assert.equal(kwRes.hits[0].page, 2);

  // 3. Shivvr online, but model set to 'off' -> keyword search
  shivvrState = {running: true, state: 'online'};
  embedModel = 'off';
  const offRes = await lib.search('navigation lights', 5);
  assert.equal(offRes.mode, 'keyword');

  fs.rmSync(dir, {recursive: true, force: true});
});

test('POST /api/library/embeddings is admin-only, requires confirmed rebuild on change, and saves config', async () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'lume-embed-api-'));
  const p = paths(dir);
  fs.mkdirSync(p.index, {recursive: true});
  fs.writeFileSync(path.join(p.index, 'index.json'), JSON.stringify({
    format_version: 4,
    generation: 'gen-0001',
    sections: 100,
    source_files: 2,
  }));

  let savedOptions = null;
  const app = {
    securityStrategy: {
      isDummy: () => false,
      allowConfigure: req => req.skIsAuthenticated && req.skPrincipal?.permissions === 'admin',
    },
    getDataDirPath: () => dir,
    readPluginOptions: () => ({configuration: savedOptions || {}}),
    savePluginOptions: (opts, cb) => { savedOptions = opts; cb(null); },
    debug: () => {},
    setPluginStatus: () => {},
    setPluginError: () => {},
  };

  const plugin = pluginFactory(app);
  const schema = plugin.schema();
  assert.equal(schema.properties.libraryEmbedModel.default, DEFAULT_EMBED_MODEL);
  assert.equal(schema.properties.libraryEmbedDimensions.default, DEFAULT_EMBED_DIMENSIONS);

  const routes = {GET: {}, POST: {}};
  plugin.registerWithRouter({
    get: (r, h) => { routes.GET[r] = h; },
    post: (r, h) => { routes.POST[r] = h; },
  });
  plugin.start({lumePath: process.execPath, servePort: 5941, autoRequestToken: false});

  const invoke = (handler, {admin = true, body} = {}) => new Promise(resolve => {
    const raw = body ? JSON.stringify(body) : '';
    const req = {
      skIsAuthenticated: admin,
      skPrincipal: {permissions: admin ? 'admin' : 'readonly'},
      headers: body ? {'content-type': 'application/json', 'content-length': String(Buffer.byteLength(raw))} : {},
      body,
      on: (ev, cb) => {
        if (ev === 'data' && raw) cb(Buffer.from(raw));
        if (ev === 'end') cb();
      },
    };
    const res = {
      code: 200,
      status(c) { this.code = c; return this; },
      json(data) { resolve({code: this.code, body: data}); return this; },
    };
    handler(req, res);
  });

  try {
    // 1. Non-admin is rejected with 403/401
    const nonAdmin = await invoke(routes.POST['/api/library/embeddings'], {
      admin: false,
      body: {model: 'embeddinggemma-2', dimensions: 512, confirmRebuild: true},
    });
    assert.ok(nonAdmin.code === 401 || nonAdmin.code === 403);

    // 2. Changing dimensions without confirmRebuild returns 409 needsRebuild
    const unconfirmed = await invoke(routes.POST['/api/library/embeddings'], {
      admin: true,
      body: {model: 'embeddinggemma-2', dimensions: 512, confirmRebuild: false},
    });
    assert.equal(unconfirmed.code, 409);
    assert.equal(unconfirmed.body.needsRebuild, true);

    // 3. Confirmed change saves plugin options
    const confirmed = await invoke(routes.POST['/api/library/embeddings'], {
      admin: true,
      body: {model: 'embeddinggemma-2', dimensions: 512, confirmRebuild: true},
    });
    assert.equal(confirmed.code, 200);
    assert.equal(savedOptions.libraryEmbedModel, 'embeddinggemma-2');
    assert.equal(savedOptions.libraryEmbedDimensions, 512);
    assert.equal(confirmed.body.embeddings.config.dimensions, 512);
  } finally {
    plugin.stop();
    await new Promise(r => setTimeout(r, 200));
    fs.rmSync(dir, {recursive: true, force: true});
  }
});

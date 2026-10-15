'use strict';
const {describe, it} = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const http = require('node:http');
const os = require('node:os');
const path = require('node:path');
const {Readable} = require('node:stream');

const {
  sanitizeUploadName,
  verifyMagicBytes,
  streamUploadToDisk,
  findJpegInBuffer,
  scanPdfBufferLinear,
  extractEmbeddedPdfJpeg,
  resolveCloudVisionConfig,
  callOllamaCloudVision,
} = require('../lib/uploads');
const {Library} = require('../lib/library');
const createPlugin = require('../index');

// Minimal valid image headers
const SAMPLE_JPEG = Buffer.from([
  0xff, 0xd8, 0xff, 0xe0, 0x00, 0x10, 0x4a, 0x46, 0x49, 0x46, 0x00, 0x01,
  0x01, 0x01, 0x00, 0x48, 0x00, 0x48, 0x00, 0x00, 0xff, 0xd9,
]);
const SAMPLE_PNG = Buffer.from([
  0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a,
  0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
]);
const SAMPLE_WEBP = Buffer.concat([
  Buffer.from('RIFF', 'ascii'),
  Buffer.from([0x24, 0x00, 0x00, 0x00]),
  Buffer.from('WEBP', 'ascii'),
  Buffer.from('VP8 ', 'ascii'),
]);

/**
 * Build a multi-page synthetic PDF where each page is either:
 * - `{kind: 'scanned', jpegByte}`: an image-only scanned page with an embedded /DCTDecode JPEG stream
 * - `{kind: 'text', text}`: a page with extracted PDF text `( ... ) Tj`
 */
function buildSyntheticPdf(pages) {
  const objects = [];
  const addObj = (id, bodyBuf) => {
    objects.push({
      id,
      buf: Buffer.concat([
        Buffer.from(`${id} 0 obj\n`, 'latin1'),
        Buffer.isBuffer(bodyBuf) ? bodyBuf : Buffer.from(bodyBuf, 'latin1'),
        Buffer.from('\nendobj\n', 'latin1'),
      ]),
    });
  };

  const pageIds = pages.map((_, idx) => 3 + idx * 2);
  addObj(1, '<< /Type /Catalog /Pages 2 0 R >>');
  addObj(2, `<< /Type /Pages /Kids [${pageIds.map(id => `${id} 0 R`).join(' ')}] /Count ${pages.length} >>`);

  pages.forEach((p, idx) => {
    const pageObjId = 3 + idx * 2;
    const childObjId = pageObjId + 1;
    if (p.kind === 'text') {
      addObj(pageObjId, `<< /Type /Page /Parent 2 0 R /Contents ${childObjId} 0 R >>`);
      const content = `BT /F1 12 Tf 72 720 Td (${p.text}) Tj ET`;
      addObj(childObjId, `<< /Length ${content.length} >>\nstream\n${content}\nendstream`);
    } else {
      addObj(pageObjId, `<< /Type /Page /Parent 2 0 R /Resources << /XObject << /Im0 ${childObjId} 0 R >> >> >>`);
      const jpeg = Buffer.from([
        0xff, 0xd8, 0xff, 0xe0, 0x00, 0x10, 0x4a, 0x46, 0x49, 0x46, 0x00, 0x01,
        p.jpegByte ?? (idx + 1), 0xff, 0xd9,
      ]);
      const dict = Buffer.from(`<< /Type /XObject /Subtype /Image /Filter /DCTDecode /Length ${jpeg.length} >>\nstream\n`, 'latin1');
      const tail = Buffer.from('\nendstream', 'latin1');
      addObj(childObjId, Buffer.concat([dict, jpeg, tail]));
    }
  });

  return Buffer.concat([
    Buffer.from('%PDF-1.4\n', 'latin1'),
    ...objects.map(o => o.buf),
    Buffer.from('%%EOF\n', 'latin1'),
  ]);
}

function stubIndexStep(lib) {
  lib._runIndexStep = async p => {
    fs.mkdirSync(p.index, {recursive: true});
    fs.writeFileSync(path.join(p.index, 'index.json'), JSON.stringify({version: 4, generation: 'g1', sections: 10}));
    return {code: 0, out: '', err: ''};
  };
  return lib;
}

describe('Upload validation, streaming, and safe names', () => {
  it('sanitizes filenames to [A-Za-z0-9._-] and rejects disallowed or reserved sidecar names', () => {
    assert.equal(sanitizeUploadName('  YanMar 3YM30 Manual (2024)!.PDF '), 'YanMar_3YM30_Manual_2024_.pdf');
    assert.equal(sanitizeUploadName('../../etc/passwd.txt'), 'passwd.txt');
    assert.throws(() => sanitizeUploadName('script.exe'), /Unsupported file extension/);
    assert.throws(() => sanitizeUploadName('.hidden'), /Unsupported file extension|non-empty stem/);
    assert.throws(() => sanitizeUploadName('wiring.jpg.md'), /Reserved sidecar file name/);
    assert.throws(() => sanitizeUploadName('manual.pdf.md'), /Reserved sidecar file name/);
  });

  it('verifies magic bytes for PDF, PNG, JPEG, WebP, and rejects disguised binaries in .md/.txt', () => {
    assert.equal(verifyMagicBytes('.pdf', Buffer.from('%PDF-1.7\n...')), true);
    assert.equal(verifyMagicBytes('.png', SAMPLE_PNG), true);
    assert.equal(verifyMagicBytes('.jpg', SAMPLE_JPEG), true);
    assert.equal(verifyMagicBytes('.jpeg', SAMPLE_JPEG), true);
    assert.equal(verifyMagicBytes('.webp', SAMPLE_WEBP), true);
    assert.equal(verifyMagicBytes('.md', Buffer.from('# Engine room checklist\n1. Check raw water strainer\n')), true);

    assert.throws(() => verifyMagicBytes('.pdf', SAMPLE_PNG), /PDF magic bytes/);
    assert.throws(() => verifyMagicBytes('.png', SAMPLE_JPEG), /PNG magic bytes/);
    assert.throws(() => verifyMagicBytes('.jpg', SAMPLE_PNG), /JPEG magic bytes/);
    assert.throws(() => verifyMagicBytes('.webp', SAMPLE_PNG), /WebP magic bytes/);
    assert.throws(() => verifyMagicBytes('.md', Buffer.from('hello\x00world')), /NUL bytes/);
    assert.throws(() => verifyMagicBytes('.txt', SAMPLE_PNG), /Binary file cannot be uploaded/);
  });

  it('streams uploads to disk, enforces size cap, and never overwrites existing files', async () => {
    const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'lume-upload-stream-'));
    try {
      const dest = path.join(tmpDir, 'user', 'checklist.md');
      const req1 = Readable.from([Buffer.from('# Checklist\nAll good.\n')]);
      req1.headers = {};
      const saved = await streamUploadToDisk(req1, dest, '.md');
      assert.equal(saved.bytes, 22);
      assert.equal(fs.readFileSync(dest, 'utf8'), '# Checklist\nAll good.\n');

      // Duplicate filename must fail with 409 and leave original untouched
      const reqDup = Readable.from([Buffer.from('# Overwrite attempt\n')]);
      reqDup.headers = {};
      await assert.rejects(
        streamUploadToDisk(reqDup, dest, '.md'),
        err => err.status === 409 && /already exists/.test(err.message)
      );
      assert.equal(fs.readFileSync(dest, 'utf8'), '# Checklist\nAll good.\n');

      // Oversize stream must abort with 413 and clean up .part file
      const bigDest = path.join(tmpDir, 'user', 'huge.txt');
      const reqBig = Readable.from([Buffer.alloc(1024, 'a'), Buffer.alloc(1024, 'b')]);
      reqBig.headers = {};
      await assert.rejects(
        streamUploadToDisk(reqBig, bigDest, '.txt', {maxBytes: 1500}),
        err => err.status === 413 && /upload limit/.test(err.message)
      );
      assert.equal(fs.existsSync(bigDest), false);
    } finally {
      fs.rmSync(tmpDir, {recursive: true, force: true});
    }
  });
});

describe('Image vision descriptions, offline queueing, and scanned PDFs', () => {
  it('describes uploaded images via Ollama Cloud vision, keeps originals, and queues offline uploads until online', async () => {
    const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'lume-upload-img-'));
    let cloudOnline = false;
    let visionCalls = 0;

    const cloudServer = http.createServer((req, res) => {
      let body = '';
      req.on('data', c => { body += c; });
      req.on('end', () => {
        visionCalls++;
        if (!cloudOnline) {
          res.statusCode = 503;
          res.end('offline');
          return;
        }
        const parsed = JSON.parse(body);
        assert.equal(parsed.model, 'glm-5.3-flash');
        assert.ok(Array.isArray(parsed.messages[0].images) && parsed.messages[0].images.length === 1);
        res.setHeader('Content-Type', 'application/json');
        res.end(JSON.stringify({
          message: {
            content: 'Yanmar 3YM30 alternator wiring diagram: B+ terminal 12V 60A, sense wire red/white.',
          },
        }));
      });
    });
    await new Promise(resolve => cloudServer.listen(0, '127.0.0.1', resolve));
    const cloudUrl = `http://127.0.0.1:${cloudServer.address().port}`;

    try {
      const lib = stubIndexStep(new Library({
        binary: process.execPath,
        dataDir: tmpDir,
        getEmbedOptions: () => ({
          libraryEmbedModel: 'off',
          visionOllamaUrl: cloudUrl,
          visionModel: 'glm-5.3-flash',
        }),
      }));

      // 1. Upload while offline -> marked "waiting for a description", original preserved
      const reqOffline = Readable.from([SAMPLE_JPEG]);
      reqOffline.headers = {};
      const up1 = await lib.upload(reqOffline, {name: 'alternator-plate.jpg'});
      assert.equal(up1.item.visionStatus, 'waiting_for_description');
      assert.equal(up1.item.visionLabel, 'waiting for a description');
      const origPath = path.join(tmpDir, 'library', 'files', 'user', 'alternator-plate.jpg');
      const sidecarPath = `${origPath}.md`;
      assert.equal(fs.existsSync(origPath), true);
      assert.deepEqual(fs.readFileSync(origPath), SAMPLE_JPEG);
      assert.equal(fs.existsSync(sidecarPath), false);

      // 2. Come back online and retryPendingVision -> sidecar .md created, status becomes "described"
      cloudOnline = true;
      const retryRes = await lib.retryPendingVision();
      assert.equal(retryRes.retried, 1);
      assert.equal(retryRes.described, 1);
      assert.equal(fs.existsSync(sidecarPath), true);
      assert.match(fs.readFileSync(sidecarPath, 'utf8'), /Yanmar 3YM30 alternator wiring diagram/);
      // Original image is still byte-for-byte untouched
      assert.deepEqual(fs.readFileSync(origPath), SAMPLE_JPEG);
      // Library userItems lists only the original image (never the .md sidecar)
      const userItems = lib.userItems();
      assert.equal(userItems.length, 1);
      assert.equal(userItems[0].userFile, 'alternator-plate.jpg');
      assert.equal(userItems[0].visionStatus, 'described');
      assert.equal(userItems[0].localUrl, '/plugins/signalk-lume-ti/api/library/file/user/alternator-plate.jpg');

      // 3. Disallow local gemma4 for vision
      const libLocalGemma = stubIndexStep(new Library({
        binary: process.execPath,
        dataDir: tmpDir,
        getEmbedOptions: () => ({
          libraryEmbedModel: 'off',
          visionOllamaUrl: cloudUrl,
          visionModel: 'gemma4:latest',
        }),
      }));
      const reqGemma = Readable.from([SAMPLE_PNG]);
      reqGemma.headers = {};
      await assert.rejects(
        libLocalGemma.upload(reqGemma, {name: 'panel.png'}),
        err => err.code === 'local_model_disallowed'
      );
    } finally {
      await new Promise(resolve => cloudServer.close(resolve));
      fs.rmSync(tmpDir, {recursive: true, force: true});
    }
  });

  it('never sends the API key Authorization header to non-ollama.com hosts', async () => {
    const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'lume-upload-auth-'));
    const keyFile = path.join(tmpDir, 'ollama.key');
    fs.writeFileSync(keyFile, 'secret-boat-ollama-key-12345\n', 'utf8');

    let receivedAuthHeader = 'uncalled';
    const nonOllamaServer = http.createServer((req, res) => {
      receivedAuthHeader = req.headers.authorization;
      res.setHeader('Content-Type', 'application/json');
      res.end(JSON.stringify({message: {content: 'Test description'}}));
    });
    await new Promise(resolve => nonOllamaServer.listen(0, '127.0.0.1', resolve));
    const nonOllamaUrl = `http://127.0.0.1:${nonOllamaServer.address().port}`;

    try {
      // 1. resolveCloudVisionConfig omits apiKey when URL host is not ollama.com / *.ollama.com
      const cfgNonOllama = resolveCloudVisionConfig(
        {visionOllamaUrl: nonOllamaUrl, chatApiKeyFile: keyFile},
        tmpDir
      );
      assert.equal(cfgNonOllama.apiKey, null);

      const cfgOllama = resolveCloudVisionConfig(
        {visionOllamaUrl: 'https://ollama.com', chatApiKeyFile: keyFile},
        tmpDir
      );
      assert.equal(cfgOllama.apiKey, 'secret-boat-ollama-key-12345');

      // 2. Even if apiKey is passed directly to callOllamaCloudVision, non-ollama URL gets no Authorization header
      await callOllamaCloudVision({
        url: nonOllamaUrl,
        model: 'glm-5.3-flash',
        apiKey: 'secret-boat-ollama-key-12345',
        imageBuffer: SAMPLE_JPEG,
      });
      assert.equal(receivedAuthHeader, undefined);
    } finally {
      await new Promise(resolve => nonOllamaServer.close(resolve));
      fs.rmSync(tmpDir, {recursive: true, force: true});
    }
  });

  it('checks PDF stream bounds linearly (no O(n^2) SOI scan) and enforces object/byte caps', () => {
    // 1 MB stream stuffed with 0xFF 0xD8 0xFF markers and NO EOI (0xFF 0xD9)
    const manySoi = Buffer.alloc(1024 * 1024);
    for (let i = 0; i + 2 < manySoi.length; i += 3) {
      manySoi[i] = 0xff;
      manySoi[i + 1] = 0xd8;
      manySoi[i + 2] = 0xff;
    }
    const t0 = Date.now();
    const res = findJpegInBuffer(manySoi, 0, manySoi.length);
    const elapsedMs = Date.now() - t0;
    assert.equal(res, null);
    assert.ok(elapsedMs < 50, `Expected O(1) stream bound check (<50ms), took ${elapsedMs}ms`);

    // Craft a PDF with 20 indirect objects and verify maxObjects cap stops early
    const chunks = [Buffer.from('%PDF-1.4\n', 'latin1')];
    for (let i = 1; i <= 20; i++) {
      chunks.push(Buffer.from(`${i} 0 obj\n<< /Type /Page >>\nendobj\n`, 'latin1'));
    }
    chunks.push(Buffer.from('%%EOF\n', 'latin1'));
    const capped = scanPdfBufferLinear(Buffer.concat(chunks), {maxObjects: 5});
    assert.equal(capped.objectsVisited, 5);
    assert.equal(capped.pageObjs.length, 5);

    // Verify UI privacy notice in public/index.html
    const html = fs.readFileSync(path.join(__dirname, '..', 'public', 'index.html'), 'utf8');
    assert.match(html, /Images and scanned pages are sent to Ollama cloud to be described; the originals stay on the boat\./);
  });

  it('extracts embedded PDF page JPEGs, describes first 5 scanned pages at index time, reads book index backwards, and caches on-demand pages', async () => {
    const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'lume-upload-pdf-'));

    // Build an 8-page PDF:
    // - Pages 1..6: scanned (no text, embedded JPEG with marker byte = pageNumber)
    // - Page 6 is normal book prose before the index ([BOOK_TEXT_START])
    // - Pages 7..8: scanned back-of-book alphabetical index ([BOOK_INDEX_PAGE])
    const pdfBuf = buildSyntheticPdf([
      {kind: 'scanned', jpegByte: 1},
      {kind: 'scanned', jpegByte: 2},
      {kind: 'scanned', jpegByte: 3},
      {kind: 'text', text: 'Chapter 4: Raw water pump impeller replacement procedure.'},
      {kind: 'scanned', jpegByte: 5},
      {kind: 'scanned', jpegByte: 6},
      {kind: 'scanned', jpegByte: 7},
      {kind: 'scanned', jpegByte: 8},
    ]);

    // Verify embedded JPEG extraction directly
    const p1Jpeg = extractEmbeddedPdfJpeg(
      (() => {
        const p = path.join(tmpDir, 'probe.pdf');
        fs.writeFileSync(p, pdfBuf);
        return p;
      })(),
      1
    );
    assert.ok(p1Jpeg && p1Jpeg[0] === 0xff && p1Jpeg[1] === 0xd8);
    assert.equal(p1Jpeg[12], 1);

    const visionPageCalls = [];
    const cloudServer = http.createServer((req, res) => {
      let body = '';
      req.on('data', c => { body += c; });
      req.on('end', () => {
        const parsed = JSON.parse(body);
        const imgBuf = Buffer.from(parsed.messages[0].images[0], 'base64');
        const pageNum = imgBuf[12];
        visionPageCalls.push(pageNum);
        let text = `Scanned content for page ${pageNum}: torque spec ${pageNum * 10} Nm.`;
        if (pageNum === 8) {
          text = '[BOOK_INDEX_PAGE]\nIndex\nAlternator, 12, 45\nImpeller, 34, 67\nZinc anode, 88';
        } else if (pageNum === 7) {
          text = '[BOOK_INDEX_PAGE]\nIndex (continued)\nBilge pump, 14, 19\nFuel filter, 22-25';
        } else if (pageNum === 6) {
          text = '[BOOK_TEXT_START]\nChapter 12: Winterizing the cooling system in freezing climates.';
        }
        res.setHeader('Content-Type', 'application/json');
        res.end(JSON.stringify({message: {content: text}}));
      });
    });
    await new Promise(resolve => cloudServer.listen(0, '127.0.0.1', resolve));
    const cloudUrl = `http://127.0.0.1:${cloudServer.address().port}`;

    try {
      const lib = stubIndexStep(new Library({
        binary: process.execPath, // triggers lightweight PDF page text inspection
        dataDir: tmpDir,
        getEmbedOptions: () => ({
          libraryEmbedModel: 'off',
          visionOllamaUrl: cloudUrl,
          visionModel: 'glm-5.3-flash',
        }),
      }));

      // Upload without readBookIndex first: should describe only unreadable pages in 1..5 (pages 1, 2, 3, 5; page 4 has text!)
      const reqPdf = Readable.from([pdfBuf]);
      reqPdf.headers = {};
      const up = await lib.upload(reqPdf, {name: 'engine-manual.pdf', readBookIndex: false});
      assert.deepEqual(visionPageCalls, [1, 2, 3, 5]);
      assert.deepEqual(up.item.describedPages, [1, 2, 3, 5]);

      // On-demand request for page 4 (which already has extracted text in the PDF) must NOT call vision!
      const p4 = await lib.describePdfPageOnDemand('engine-manual.pdf', 4);
      assert.equal(p4.source, 'extracted-text');
      assert.equal(p4.cached, true);
      assert.match(p4.text, /Raw water pump impeller/);
      assert.deepEqual(visionPageCalls, [1, 2, 3, 5]);

      // On-demand request for already-cached page 2 must return from cache with 0 vision calls!
      const p2 = await lib.describePdfPageOnDemand('engine-manual.pdf', 2);
      assert.equal(p2.source, 'vision-cache');
      assert.equal(p2.cached, true);
      assert.deepEqual(visionPageCalls, [1, 2, 3, 5]);

      // Read back-of-book index backwards from page 8:
      // - Page 8 -> index page
      // - Page 7 -> index page
      // - Page 6 -> book prose ([BOOK_TEXT_START]), so backwards scan stops at page 6!
      const idxScan = await lib.describePdfBookIndex('engine-manual.pdf');
      assert.deepEqual(idxScan.indexPages, [7, 8]);
      assert.deepEqual(visionPageCalls, [1, 2, 3, 5, 8, 7, 6]);

      // Requesting page 6 on demand now hits the per-page cache (since backwards scan already described page 6)
      const p6 = await lib.describePdfPageOnDemand('engine-manual.pdf', 6);
      assert.equal(p6.source, 'vision-cache');
      assert.equal(p6.cached, true);
      assert.deepEqual(visionPageCalls, [1, 2, 3, 5, 8, 7, 6]);

      // Verify sidecar markdown exists beside original and search results link to original PDF at #page=N
      const sidecar = fs.readFileSync(path.join(tmpDir, 'library', 'files', 'user', 'engine-manual.pdf.md'), 'utf8');
      assert.match(sidecar, /# Page 7/);
      assert.match(sidecar, /Bilge pump, 14, 19/);
    } finally {
      await new Promise(resolve => cloudServer.close(resolve));
      fs.rmSync(tmpDir, {recursive: true, force: true});
    }
  });

  it('enforces admin-only on POST /api/library/upload and links search hits from sidecars to original files', async () => {
    const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'lume-upload-routes-'));
    fs.mkdirSync(path.join(tmpDir, 'library', 'index'), {recursive: true});
    let isAdmin = false;

    const queryServer = http.createServer((req, res) => {
      res.setHeader('Content-Type', 'application/json');
      res.end(JSON.stringify({
        rows: [
          {
            file: 'files/user/engine-manual.pdf.md',
            title: 'Page 7',
            line: 42,
            score: 4.2,
            excerpt: 'Bilge pump, 14, 19',
          },
        ],
      }));
    });
    await new Promise(resolve => queryServer.listen(0, '127.0.0.1', resolve));

    const routes = {get: {}, post: {}};
    const router = {
      get: (p, h) => { routes.get[p] = h; },
      post: (p, h) => { routes.post[p] = h; },
    };
    const app = {
      getDataDirPath: () => tmpDir,
      securityStrategy: {isDummy: () => false},
      readPluginOptions: () => ({configuration: {lumePath: process.execPath, serverMode: 'external', libraryEmbedModel: 'off', servePort: queryServer.address().port}}),
      savePluginOptions: (_cfg, cb) => cb(null),
      setPluginStatus: () => {},
      setPluginError: () => {},
    };

    const plugin = createPlugin(app);
    plugin.start({lumePath: process.execPath, serverMode: 'external', libraryEmbedModel: 'off', servePort: queryServer.address().port}, () => {});
    plugin.registerWithRouter(router);

    try {
      // Non-admin upload is rejected with 401/403
      let statusCode = 200;
      const reqUnauth = Readable.from([Buffer.from('# Notes\n')]);
      reqUnauth.headers = {'x-file-name': 'notes.md'};
      await routes.post['/api/library/upload'](reqUnauth, {
        status(c) { statusCode = c; return this; },
        json() {},
      });
      assert.equal(statusCode, 401);

      // Admin upload succeeds
      isAdmin = true;
      app.securityStrategy = {isDummy: () => isAdmin};
      const pdfBuf = buildSyntheticPdf([{kind: 'text', text: 'Bilge pump diagram'}]);
      const reqAdmin = Readable.from([pdfBuf]);
      reqAdmin.headers = {'x-file-name': 'engine-manual.pdf'};
      reqAdmin.query = {name: 'engine-manual.pdf'};
      let uploadPayload = null;
      await routes.post['/api/library/upload'](reqAdmin, {
        status(c) { statusCode = c; return this; },
        json(b) { uploadPayload = b; },
      });
      assert.equal(uploadPayload.ok, true);
      assert.equal(uploadPayload.item.userFile, 'engine-manual.pdf');

      // Search hit coming from sidecar `files/user/engine-manual.pdf.md` with title `Page 7`
      // resolves localUrl to the ORIGINAL PDF `/plugins/signalk-lume-ti/api/library/file/user/engine-manual.pdf` and page: 7!
      let searchPayload = null;
      await routes.get['/api/library/search'](
        {query: {q: 'bilge pump', limit: '5'}},
        {status() { return this; }, json(b) { searchPayload = b; }}
      );
      assert.equal(searchPayload.hits.length, 1);
      assert.equal(searchPayload.hits[0].localUrl, '/plugins/signalk-lume-ti/api/library/file/user/engine-manual.pdf');
      assert.equal(searchPayload.hits[0].page, 7);
      assert.equal(searchPayload.hits[0].addedByYou, true);
    } finally {
      plugin.stop();
      await new Promise(resolve => queryServer.close(resolve));
      fs.rmSync(tmpDir, {recursive: true, force: true});
    }
  });
});

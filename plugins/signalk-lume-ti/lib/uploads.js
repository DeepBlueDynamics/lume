'use strict';
// User file uploads and Ollama Cloud vision descriptions for images and scanned PDFs.
// - Streams uploads directly to disk with a 50 MB cap (no in-memory buffering of the file).
// - Enforces extension allowlist + magic bytes (.pdf, .md, .txt, .png, .jpg/.jpeg, .webp).
// - Sanitizes names to [A-Za-z0-9._-], saves under <library>/files/user/, never overwrites.
// - Always keeps original files untouched; vision descriptions are saved as sidecar <name>.md
//   files beside originals, with frontmatter linking back to the original file.
// - Uses Ollama CLOUD vision models only (never local Gemma 4). When offline or without a key,
//   marks files as "waiting for a description" and retries when back online.
// - For scanned PDFs (pages with no extracted text): describes up to the first 5 scanned pages
//   at index time, optionally reads the book's back-of-book index backwards from the last page
//   until the index ends and book text starts, and describes other unreadable pages on demand,
//   caching every described page so no page is ever described twice.
const fs = require('node:fs');
const http = require('node:http');
const https = require('node:https');
const os = require('node:os');
const path = require('node:path');
const {spawn, spawnSync} = require('node:child_process');
const {Worker, isMainThread, parentPort, workerData} = require('node:worker_threads');
const {looksLikeKey} = require('./chat');

const MAX_UPLOAD_BYTES = 50 * 1024 * 1024; // 50 MB
const MAX_PDF_SCAN_BYTES = MAX_UPLOAD_BYTES;
const MAX_PDF_OBJECTS = 4096;
const MAX_PDF_DICT_BYTES = 2 * 1024 * 1024;
const MAX_PDF_OBJECT_HEADER_BYTES = 16384;
const MAX_PDF_REF_VISITS = 64;
const PDF_WORKER_TIMEOUT_MS = 10000;
const ALLOWED_EXTENSIONS = new Set(['.pdf', '.md', '.txt', '.png', '.jpg', '.jpeg', '.webp']);
const IMAGE_EXTENSIONS = new Set(['.png', '.jpg', '.jpeg', '.webp']);
const DEFAULT_VISION_MODEL = 'glm-5.3:cloud';
const DEFAULT_CLOUD_URL = 'https://ollama.com';

const MIME_BY_EXT = {
  '.pdf': 'application/pdf',
  '.png': 'image/png',
  '.jpg': 'image/jpeg',
  '.jpeg': 'image/jpeg',
  '.webp': 'image/webp',
  '.md': 'text/markdown; charset=utf-8',
  '.txt': 'text/plain; charset=utf-8',
};

function uploadError(message, status = 400, code = 'bad_upload') {
  const err = new Error(message);
  err.status = status;
  err.code = code;
  return err;
}

/**
 * Strip an uploaded filename to safe characters `[A-Za-z0-9._-]`, disallowing leading dots,
 * path traversal, reserved sidecar suffixes, and disallowed extensions.
 */
function sanitizeUploadName(rawName) {
  const base = path.basename(String(rawName || '').trim());
  const cleaned = base
    .replace(/[^A-Za-z0-9._-]/g, '_')
    .replace(/_+/g, '_')
    .replace(/^\.+/, '');
  if (!cleaned || cleaned.length > 180) {
    throw uploadError('Provide a valid file name', 400, 'invalid_name');
  }
  const ext = path.extname(cleaned).toLowerCase();
  if (!ALLOWED_EXTENSIONS.has(ext)) {
    throw uploadError(
      `Unsupported file extension "${ext || '(none)'}". Allowed: .pdf, .md, .txt, .png, .jpg, .webp`,
      400,
      'bad_extension'
    );
  }
  const stem = cleaned.slice(0, cleaned.length - ext.length);
  if (!stem || /^\.+$/.test(stem)) {
    throw uploadError('File name must have a non-empty stem before the extension', 400, 'invalid_name');
  }
  // Prevent uploading a file whose name collides with a vision sidecar (.png.md, .pdf.md, etc.).
  const innerExt = path.extname(stem).toLowerCase();
  if ((ext === '.md' || ext === '.txt') && (IMAGE_EXTENSIONS.has(innerExt) || innerExt === '.pdf')) {
    throw uploadError('Reserved sidecar file name; rename the file before uploading', 400, 'reserved_name');
  }
  return `${stem}${ext}`;
}

/**
 * Verify that the leading bytes of a file match its extension's magic signature.
 */
function verifyMagicBytes(ext, headerBuf) {
  const e = String(ext || '').toLowerCase();
  if (!Buffer.isBuffer(headerBuf) || headerBuf.length === 0) {
    throw uploadError('Uploaded file is empty', 400, 'empty_file');
  }
  if (e === '.pdf') {
    if (headerBuf.length < 5 || headerBuf.subarray(0, 5).toString('ascii') !== '%PDF-') {
      throw uploadError('File content does not match PDF magic bytes (%PDF-)', 400, 'bad_magic');
    }
    return true;
  }
  if (e === '.png') {
    const pngMagic = Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);
    if (headerBuf.length < 8 || !headerBuf.subarray(0, 8).equals(pngMagic)) {
      throw uploadError('File content does not match PNG magic bytes', 400, 'bad_magic');
    }
    return true;
  }
  if (e === '.jpg' || e === '.jpeg') {
    if (headerBuf.length < 3 || headerBuf[0] !== 0xff || headerBuf[1] !== 0xd8 || headerBuf[2] !== 0xff) {
      throw uploadError('File content does not match JPEG magic bytes', 400, 'bad_magic');
    }
    return true;
  }
  if (e === '.webp') {
    if (
      headerBuf.length < 12
      || headerBuf.subarray(0, 4).toString('ascii') !== 'RIFF'
      || headerBuf.subarray(8, 12).toString('ascii') !== 'WEBP'
    ) {
      throw uploadError('File content does not match WebP magic bytes (RIFF....WEBP)', 400, 'bad_magic');
    }
    return true;
  }
  if (e === '.md' || e === '.txt') {
    // Reject binary files disguised as text/markdown.
    const sample = headerBuf.subarray(0, Math.min(headerBuf.length, 4096));
    if (
      (sample.length >= 5 && sample.subarray(0, 5).toString('ascii') === '%PDF-')
      || (sample.length >= 4 && sample[0] === 0x89 && sample.subarray(1, 4).toString('ascii') === 'PNG')
      || (sample.length >= 3 && sample[0] === 0xff && sample[1] === 0xd8 && sample[2] === 0xff)
      || (sample.length >= 4 && sample[0] === 0x7f && sample.subarray(1, 4).toString('ascii') === 'ELF')
    ) {
      throw uploadError('Binary file cannot be uploaded as .md or .txt', 400, 'bad_magic');
    }
    if (sample.includes(0x00)) {
      throw uploadError('Text or Markdown file contains binary NUL bytes', 400, 'bad_magic');
    }
    return true;
  }
  throw uploadError(`Unsupported extension "${e}"`, 400, 'bad_extension');
}

function userPaths(dataDir) {
  const libRoot = path.join(dataDir, 'library');
  const userDir = path.join(libRoot, 'files', 'user');
  const cacheDir = path.join(libRoot, 'vision-cache');
  const metaFile = path.join(libRoot, 'user-uploads.json');
  return {libRoot, userDir, cacheDir, metaFile};
}

function readJsonSafe(file, fallback) {
  try { return JSON.parse(fs.readFileSync(file, 'utf8')); } catch (_) { return fallback; }
}

function writeJsonAtomic(file, data) {
  fs.mkdirSync(path.dirname(file), {recursive: true});
  const tmp = `${file}.${process.pid}.${Date.now()}.tmp`;
  fs.writeFileSync(tmp, JSON.stringify(data, null, 2));
  fs.renameSync(tmp, file);
}

/**
 * Stream an incoming HTTP upload request directly to a temporary file on disk,
 * enforcing `maxBytes` (default 50 MB) and verifying magic bytes before moving
 * atomically into `<library>/files/user/<safeName>`. Never buffers the full file in RAM.
 */
function streamUploadToDisk(req, destPath, ext, {maxBytes = MAX_UPLOAD_BYTES} = {}) {
  return new Promise((resolve, reject) => {
    if (fs.existsSync(destPath)) {
      return reject(uploadError(`File "${path.basename(destPath)}" already exists in the Library`, 409, 'exists'));
    }
    const contentLength = Number(req.headers && req.headers['content-length']);
    if (Number.isFinite(contentLength) && contentLength > maxBytes) {
      return reject(uploadError(`File exceeds the ${Math.round(maxBytes / (1024 * 1024))} MB upload limit`, 413, 'too_large'));
    }

    fs.mkdirSync(path.dirname(destPath), {recursive: true});
    const tmpPath = `${destPath}.${process.pid}.${Date.now()}.part`;
    let fd = null;
    try {
      fd = fs.openSync(tmpPath, 'wx', 0o644);
    } catch (err) {
      return reject(uploadError(`Could not open temporary upload file: ${err.message}`, 500, 'io_error'));
    }

    let totalBytes = 0;
    let headerBuf = Buffer.alloc(0);
    let magicChecked = false;
    let finished = false;

    const cleanup = () => {
      if (fd !== null) {
        try { fs.closeSync(fd); } catch (_) {}
        fd = null;
      }
      try { fs.rmSync(tmpPath, {force: true}); } catch (_) {}
    };

    const fail = err => {
      if (finished) return;
      finished = true;
      cleanup();
      try { if (typeof req.resume === 'function') req.resume(); } catch (_) {}
      reject(err);
    };

    const onChunk = chunk => {
      if (finished) return;
      const buf = Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk);
      totalBytes += buf.length;
      if (totalBytes > maxBytes) {
        return fail(uploadError(`File exceeds the ${Math.round(maxBytes / (1024 * 1024))} MB upload limit`, 413, 'too_large'));
      }
      if (headerBuf.length < 4096) {
        const needed = 4096 - headerBuf.length;
        headerBuf = Buffer.concat([headerBuf, buf.subarray(0, needed)]);
      }
      if (!magicChecked && headerBuf.length >= 16) {
        try {
          verifyMagicBytes(ext, headerBuf);
          magicChecked = true;
        } catch (err) {
          return fail(err);
        }
      }
      try {
        fs.writeSync(fd, buf, 0, buf.length);
      } catch (err) {
        return fail(uploadError(`Disk write failed: ${err.message}`, 500, 'io_error'));
      }
    };

    const onEnd = () => {
      if (finished) return;
      finished = true;
      try {
        verifyMagicBytes(ext, headerBuf);
        fs.closeSync(fd);
        fd = null;
        if (fs.existsSync(destPath)) {
          cleanup();
          return reject(uploadError(`File "${path.basename(destPath)}" already exists in the Library`, 409, 'exists'));
        }
        // Exclusive link/rename so an existing file is never overwritten.
        fs.linkSync(tmpPath, destPath);
        fs.rmSync(tmpPath, {force: true});
        resolve({bytes: totalBytes, path: destPath});
      } catch (err) {
        cleanup();
        if (err && err.code === 'EEXIST') {
          return reject(uploadError(`File "${path.basename(destPath)}" already exists in the Library`, 409, 'exists'));
        }
        reject(err.status ? err : uploadError(err.message, 400, 'bad_upload'));
      }
    };

    req.on('data', onChunk);
    req.on('end', onEnd);
    req.on('error', err => fail(uploadError(`Upload stream failed: ${err.message}`, 400, 'stream_error')));
  });
}

/**
 * Validate and slice a JPEG stream from known PDF stream bounds (`stream ... endstream` or `/Length`).
 * Never scans blindly across the buffer: checks in O(1) that the bounded stream starts with
 * SOI (`0xFF 0xD8 0xFF`) and ends with EOI (`0xFF 0xD9`).
 */
function isValidJpegSlice(slice) {
  return (
    Buffer.isBuffer(slice)
    && slice.length >= 4
    && slice[0] === 0xff
    && slice[1] === 0xd8
    && slice[2] === 0xff
    && slice[slice.length - 2] === 0xff
    && slice[slice.length - 1] === 0xd9
  );
}

function findJpegInBuffer(buf, startOffset = 0, endOffset = buf.length, declaredLength = 0) {
  if (!Buffer.isBuffer(buf)) return null;
  const clampedStart = Math.max(0, Math.min(startOffset | 0, buf.length));
  const clampedEnd = Math.max(clampedStart, Math.min(endOffset | 0, buf.length));
  if (clampedEnd - clampedStart < 4) return null;

  let s = clampedStart;
  while (s < clampedEnd && s - clampedStart < 2 && (buf[s] === 0x0a || buf[s] === 0x0d || buf[s] === 0x20)) {
    s++;
  }
  if (declaredLength > 0 && s + declaredLength <= clampedEnd) {
    const byLen = buf.subarray(s, s + declaredLength);
    if (isValidJpegSlice(byLen)) {
      return byLen;
    }
  }
  let e = clampedEnd;
  while (e > s && clampedEnd - e < 2 && (buf[e - 1] === 0x0a || buf[e - 1] === 0x0d || buf[e - 1] === 0x20)) {
    e--;
  }
  const bounded = buf.subarray(s, e);
  return isValidJpegSlice(bounded) ? bounded : null;
}

/**
 * Linearly scan a PDF buffer for indirect objects with strict caps on bytes scanned and
 * objects visited. Never converts binary stream payloads to strings or runs regexes on streams.
 */
function scanPdfBufferLinear(buf, {
  maxBytes = MAX_PDF_SCAN_BYTES,
  maxObjects = MAX_PDF_OBJECTS,
  maxDictBytes = MAX_PDF_DICT_BYTES,
  includeShortTextStreams = false,
} = {}) {
  const scanLimit = Math.min(buf.length, maxBytes);
  const objMap = new Map();
  const pageObjs = [];
  const dctStreams = [];
  let pos = 0;
  let objectsVisited = 0;
  let dictBytesScanned = 0;

  while (pos < scanLimit && objectsVisited < maxObjects && dictBytesScanned < maxDictBytes) {
    const objTokenIdx = buf.indexOf(' obj', pos, 'latin1');
    if (objTokenIdx === -1 || objTokenIdx >= scanLimit) break;
    const afterObj = objTokenIdx + 4;
    const nextByte = afterObj < scanLimit ? buf[afterObj] : 0;
    if (
      nextByte !== 0x0a && nextByte !== 0x0d && nextByte !== 0x20 && nextByte !== 0x09
      && nextByte !== 0x3c && nextByte !== 0x5b && nextByte !== 0x2f
    ) {
      pos = afterObj;
      continue;
    }

    const lookbackStart = Math.max(pos, objTokenIdx - 24);
    const prefix = buf.subarray(lookbackStart, objTokenIdx).toString('latin1');
    const idMatch = /(?:^|[\r\n\s])(\d+)\s+(\d+)$/.exec(prefix);
    if (!idMatch) {
      pos = afterObj;
      continue;
    }

    objectsVisited++;
    const objId = Number(idMatch[1]);
    const bodyStart = afterObj;
    const headerWindowEnd = Math.min(scanLimit, bodyStart + MAX_PDF_OBJECT_HEADER_BYTES);
    const headerSlice = buf.subarray(bodyStart, headerWindowEnd);
    const relEndObj = headerSlice.indexOf('endobj', 0, 'latin1');

    // Find 'stream\n' or 'stream\r\n' within the object header window
    let relStream = -1;
    let streamHeaderLen = 0;
    let searchFrom = 0;
    while (searchFrom < headerSlice.length) {
      const idx = headerSlice.indexOf('stream', searchFrom, 'latin1');
      if (idx === -1 || (relEndObj !== -1 && idx >= relEndObj)) break;
      const b1 = headerSlice[idx + 6];
      const b2 = headerSlice[idx + 7];
      if (b1 === 0x0d && b2 === 0x0a) {
        relStream = idx;
        streamHeaderLen = 8;
        break;
      }
      if (b1 === 0x0a || b1 === 0x0d) {
        relStream = idx;
        streamHeaderLen = 7;
        break;
      }
      searchFrom = idx + 6;
    }

    if (relStream !== -1) {
      const dictBuf = headerSlice.subarray(0, relStream);
      dictBytesScanned += dictBuf.length;
      const dictText = dictBuf.toString('latin1');
      const rawStart = bodyStart + relStream + streamHeaderLen;

      // Prefer /Length when it is a direct integer (not an indirect `N 0 R` reference)
      const lenMatch = /\/Length\s+(\d+)(?!\s+\d+\s+R\b)/.exec(dictText);
      const declaredLength = lenMatch ? Number(lenMatch[1]) : 0;

      let endStreamAbs = -1;
      if (declaredLength > 0 && rawStart + declaredLength <= scanLimit) {
        const probeStart = rawStart + declaredLength;
        const probeEnd = Math.min(scanLimit, probeStart + 32);
        const relEnd = buf.subarray(probeStart, probeEnd).indexOf('endstream', 0, 'latin1');
        if (relEnd !== -1) {
          endStreamAbs = probeStart + relEnd;
        }
      }
      if (endStreamAbs === -1) {
        endStreamAbs = buf.indexOf('endstream', rawStart, 'latin1');
      }
      if (endStreamAbs === -1 || endStreamAbs > scanLimit) break;

      const rawEnd = endStreamAbs;
      const entry = {id: objId, dictText, text: dictText, jpeg: null};
      const isImageStream = /\/DCTDecode\b/.test(dictText) || /\/Subtype\s*\/Image\b/.test(dictText);
      if (isImageStream) {
        const jpeg = findJpegInBuffer(buf, rawStart, rawEnd, declaredLength);
        if (jpeg) {
          entry.jpeg = jpeg;
          dctStreams.push(jpeg);
        }
      } else if (includeShortTextStreams && rawEnd - rawStart <= 8192) {
        const streamText = buf.subarray(rawStart, rawEnd).toString('latin1');
        dictBytesScanned += streamText.length;
        entry.text = `${dictText}\n${streamText}`;
      }

      objMap.set(objId, entry);
      if (/\/Type\s*\/Page\b(?!s)/.test(dictText)) {
        pageObjs.push(entry);
      }

      const afterEndStream = endStreamAbs + 9;
      const closeWindow = buf.subarray(afterEndStream, Math.min(scanLimit, afterEndStream + 64));
      const relClose = closeWindow.indexOf('endobj', 0, 'latin1');
      pos = relClose !== -1 ? afterEndStream + relClose + 6 : afterEndStream;
    } else {
      const endObjAbs = relEndObj !== -1
        ? bodyStart + relEndObj
        : buf.indexOf('endobj', bodyStart, 'latin1');
      if (endObjAbs === -1 || endObjAbs > scanLimit) break;
      const dictEnd = Math.min(endObjAbs, bodyStart + MAX_PDF_OBJECT_HEADER_BYTES);
      const dictBuf = buf.subarray(bodyStart, dictEnd);
      dictBytesScanned += dictBuf.length;
      const dictText = dictBuf.toString('latin1');
      const entry = {id: objId, dictText, text: dictText, jpeg: null};
      objMap.set(objId, entry);
      if (/\/Type\s*\/Page\b(?!s)/.test(dictText)) {
        pageObjs.push(entry);
      }
      pos = endObjAbs + 6;
    }
  }

  return {objMap, pageObjs, dctStreams, objectsVisited, dictBytesScanned};
}

/**
 * Extract the N-th page's embedded JPEG (`0xFF 0xD8 0xFF ... 0xFF 0xD9`) from a PDF file.
 * Scanned PDFs store one `/DCTDecode` JPEG stream per page. Prefers `/Type /Page` object order
 * when indirect XObject references exist, or stream order of `/DCTDecode` JPEG streams.
 * Falls back to `pdftoppm` when present on the system.
 */
function extractEmbeddedPdfJpeg(pdfPath, pageNumber = 1, scanOpts = {}) {
  const page = Math.max(1, Number(pageNumber) | 0);
  const buf = Buffer.isBuffer(pdfPath) ? pdfPath : fs.readFileSync(pdfPath);
  const {objMap, pageObjs, dctStreams} = scanPdfBufferLinear(buf, scanOpts);

  // 1. Try resolving via the N-th /Type /Page object's /XObject or /Resources references.
  if (page <= pageObjs.length) {
    const pageObj = pageObjs[page - 1];
    if (pageObj.jpeg) return pageObj.jpeg;
    const visited = new Set();
    const queue = [pageObj.dictText];
    while (queue.length > 0 && visited.size < MAX_PDF_REF_VISITS) {
      const text = queue.shift();
      const refRe = /(\d+)\s+0\s+R\b/g;
      let rMatch;
      while ((rMatch = refRe.exec(text)) !== null) {
        if (visited.size >= MAX_PDF_REF_VISITS) break;
        const refId = Number(rMatch[1]);
        if (visited.has(refId)) continue;
        visited.add(refId);
        const target = objMap.get(refId);
        if (!target) continue;
        if (target.jpeg) return target.jpeg;
        // Follow /Resources or /XObject dictionaries, but not other /Page objects.
        if (!/\/Type\s*\/Page\b/.test(target.dictText) && target.dictText.length < MAX_PDF_OBJECT_HEADER_BYTES) {
          queue.push(target.dictText);
        }
      }
    }
  }

  // 2. Fallback: N-th /DCTDecode JPEG stream in the PDF.
  if (page <= dctStreams.length) {
    return dctStreams[page - 1];
  }

  return null;
}

let pdftoppmChecked = null;
function hasPdftoppm() {
  if (pdftoppmChecked !== null) return pdftoppmChecked;
  try {
    const res = spawnSync('pdftoppm', ['-v'], {stdio: 'ignore', timeout: 3000});
    pdftoppmChecked = res.status === 0 || res.status === 99;
  } catch (_) {
    pdftoppmChecked = false;
  }
  return pdftoppmChecked;
}

function renderPdfPageWithPdftoppm(pdfPath, pageNumber = 1) {
  if (!hasPdftoppm()) return null;
  const page = Math.max(1, Number(pageNumber) | 0);
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'lume-pdftoppm-'));
  const outPrefix = path.join(tmpDir, 'page');
  try {
    const res = spawnSync(
      'pdftoppm',
      ['-jpeg', '-r', '150', '-f', String(page), '-l', String(page), '-singlefile', pdfPath, outPrefix],
      {timeout: 30000}
    );
    const jpgPath = `${outPrefix}.jpg`;
    if (res.status === 0 && fs.existsSync(jpgPath)) {
      return fs.readFileSync(jpgPath);
    }
    return null;
  } catch (_) {
    return null;
  } finally {
    try { fs.rmSync(tmpDir, {recursive: true, force: true}); } catch (_) {}
  }
}

/**
 * Turn a PDF page into a JPEG Buffer: prefers embedded page JPEG extraction,
 * falls back to `pdftoppm` when installed.
 */
function extractPdfPageImage(pdfPath, pageNumber = 1) {
  const embedded = extractEmbeddedPdfJpeg(pdfPath, pageNumber);
  if (embedded) {
    return {buffer: embedded, method: 'embedded-jpeg'};
  }
  const rendered = renderPdfPageWithPdftoppm(pdfPath, pageNumber);
  if (rendered) {
    return {buffer: rendered, method: 'pdftoppm'};
  }
  return null;
}

/**
 * Count PDF pages and optionally extract simple `( ... ) Tj` literals using the bounded
 * linear object scanner (never converting the entire PDF buffer to a string).
 */
function inspectPdfStructureSync(pdfPath, {includeTextFallback = false} = {}) {
  const buf = Buffer.isBuffer(pdfPath) ? pdfPath : fs.readFileSync(pdfPath);
  const {objMap, pageObjs} = scanPdfBufferLinear(buf, {
    includeShortTextStreams: includeTextFallback,
  });

  let totalPages = pageObjs.length;
  if (totalPages === 0) {
    let maxCount = 0;
    for (const entry of objMap.values()) {
      const m = /\/Count\s+(\d+)\b/.exec(entry.dictText);
      if (m) {
        const n = Number(m[1]);
        if (n > maxCount) maxCount = n;
      }
    }
    totalPages = maxCount || 1;
  }

  const textEntries = [];
  if (includeTextFallback) {
    pageObjs.forEach((pObj, idx) => {
      const pageNum = idx + 1;
      let combined = pObj.text;
      let visits = 0;
      for (const ref of pObj.dictText.matchAll(/(\d+)\s+0\s+R\b/g)) {
        if (++visits > MAX_PDF_REF_VISITS) break;
        const refEntry = objMap.get(Number(ref[1]));
        if (refEntry && !/\/Type\s*\/Page\b/.test(refEntry.dictText)) {
          combined += '\n' + refEntry.text;
        }
      }
      const literals = [...combined.matchAll(/\(([^()]{3,})\)\s*T[jJ]/g)]
        .map(x => x[1].trim())
        .filter(Boolean);
      if (literals.length > 0) {
        textEntries.push([pageNum, literals.join(' ')]);
      }
    });
  }

  return {totalPages: Math.max(1, totalPages), textEntries};
}

function runPdfWorkerTask(task, timeoutMs = PDF_WORKER_TIMEOUT_MS) {
  return new Promise((resolve, reject) => {
    const worker = new Worker(__filename, {
      workerData: {__lumePdfWorker: true, ...task},
    });
    let settled = false;
    const timer = setTimeout(() => {
      if (settled) return;
      settled = true;
      worker.terminate();
      reject(uploadError('PDF inspection timed out', 408, 'pdf_timeout'));
    }, timeoutMs);

    worker.once('message', msg => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      if (msg && msg.ok) {
        resolve(msg.result);
      } else {
        reject(uploadError((msg && msg.error) || 'PDF worker failed', 400, 'pdf_worker_error'));
      }
    });
    worker.once('error', err => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      reject(err);
    });
    worker.once('exit', code => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      if (code !== 0) {
        reject(uploadError(`PDF worker exited with code ${code}`, 400, 'pdf_worker_exit'));
      }
    });
  });
}

async function extractPdfPageImageAsync(pdfPath, pageNumber = 1, {timeoutMs = PDF_WORKER_TIMEOUT_MS} = {}) {
  const res = await runPdfWorkerTask({op: 'extractPageImage', pdfPath, pageNumber}, timeoutMs);
  if (!res || !res.buffer) return null;
  return {
    buffer: Buffer.isBuffer(res.buffer) ? res.buffer : Buffer.from(res.buffer),
    method: res.method,
  };
}

async function inspectPdfPages(pdfPath, lumeBinary, {timeoutMs = PDF_WORKER_TIMEOUT_MS} = {}) {
  const useLumeExtractor = Boolean(
    lumeBinary
    && path.basename(lumeBinary) !== 'node'
    && path.basename(lumeBinary) !== 'node.exe'
  );

  const workerInfo = await runPdfWorkerTask(
    {op: 'inspect', pdfPath, includeTextFallback: !useLumeExtractor},
    timeoutMs
  );
  let totalPages = workerInfo.totalPages;
  const textByPage = new Map(workerInfo.textEntries || []);

  if (useLumeExtractor) {
    const extracted = await new Promise(resolve => {
      const child = spawn(lumeBinary, ['__extract-document', pdfPath], {stdio: ['ignore', 'pipe', 'pipe']});
      let out = '';
      const timer = setTimeout(() => child.kill('SIGTERM'), 30000);
      child.stdout.on('data', c => { out += c.toString(); });
      child.on('error', () => { clearTimeout(timer); resolve(null); });
      child.on('close', code => {
        clearTimeout(timer);
        if (code !== 0) return resolve(null);
        try { resolve(JSON.parse(out)); } catch (_) { resolve(null); }
      });
    });
    const ok = extracted && extracted.Ok;
    if (ok && Array.isArray(ok.sections)) {
      for (const sec of ok.sections) {
        const p = Number(sec.line);
        const body = String(sec.body || '').trim();
        if (p > 0 && body.length > 0) {
          textByPage.set(p, body);
        }
      }
      const sumPages = ok.sections.length + (Number(ok.skipped_units) || 0);
      if (sumPages > totalPages) totalPages = sumPages;
    }
  }

  return {totalPages: Math.max(1, totalPages), textByPage};
}

function isOllamaCloudHost(urlStr) {
  try {
    const h = new URL(/^[a-z]+:\/\//i.test(urlStr) ? urlStr : `https://${urlStr}`).hostname.toLowerCase();
    return h === 'ollama.com' || h.endsWith('.ollama.com');
  } catch (_) {
    return false;
  }
}

/**
 * Resolve the Ollama CLOUD endpoint and API key for vision descriptions.
 * Decisions #1: Images and scanned PDFs use an Ollama CLOUD vision model only (no local Gemma 4).
 * Security rule (same as chat_sql with_ollama_auth): only attach/expose the boat's API key when
 * the target host is `ollama.com` or `*.ollama.com`.
 */
function resolveCloudVisionConfig(options = {}, dataDir = '') {
  const rawUrls = String(options.visionOllamaUrl || options.chatOllamaUrl || DEFAULT_CLOUD_URL)
    .split(',')
    .map(u => u.trim())
    .filter(Boolean);
  // Pick the cloud endpoint (ollama.com, or an explicit test visionOllamaUrl). Never use local gemma4.
  const cloudUrl = rawUrls.find(isOllamaCloudHost)
    || (options.visionOllamaUrl ? rawUrls[0] : DEFAULT_CLOUD_URL);

  const model = String(options.visionModel || options.chatModel || DEFAULT_VISION_MODEL).trim();
  if (/^gemma4\b/i.test(model)) {
    throw uploadError('Local Gemma 4 is disabled for vision; use an Ollama cloud vision model', 400, 'local_model_disallowed');
  }

  let apiKey = null;
  if (isOllamaCloudHost(cloudUrl)) {
    const keyPath = (typeof options.chatApiKeyFile === 'string' && options.chatApiKeyFile.trim())
      || (dataDir ? path.join(dataDir, 'ollama.key') : '');
    if (keyPath && !looksLikeKey(keyPath) && fs.existsSync(keyPath)) {
      try {
        apiKey = fs.readFileSync(keyPath, 'utf8').trim() || null;
      } catch (_) {
        apiKey = null;
      }
    }
  }
  return {url: cloudUrl.replace(/\/+$/, ''), model, apiKey};
}

/**
 * Call Ollama Cloud `/api/chat` with a base64-encoded image to get a text description + OCR.
 */
function callOllamaCloudVision({url, model, apiKey, imageBuffer, prompt, timeoutMs = 45000}) {
  return new Promise((resolve, reject) => {
    let target;
    try {
      target = new URL(`${url}/api/chat`);
    } catch (err) {
      return reject(new Error(`Invalid Ollama cloud URL: ${err.message}`));
    }
    const host = target.hostname.toLowerCase();
    const isOllamaCom = host === 'ollama.com' || host.endsWith('.ollama.com');
    if (isOllamaCom && !apiKey) {
      const err = new Error('Waiting for Ollama cloud API key');
      err.offline = true;
      return reject(err);
    }

    const effectiveModel = isOllamaCom
      ? (model.replace(/:cloud$/i, '').toLowerCase() === 'glm-5.3'
        ? 'glm-5.3-flash'
        : model.replace(/:cloud$/i, ''))
      : model;

    const payload = JSON.stringify({
      model: effectiveModel,
      stream: false,
      messages: [
        {
          role: 'user',
          content: prompt || 'Describe this marine image or document page and transcribe all visible text, numbers, labels, and warnings verbatim.',
          images: [imageBuffer.toString('base64')],
        },
      ],
    });

    const headers = {
      'Content-Type': 'application/json',
      Accept: 'application/json',
      'Content-Length': Buffer.byteLength(payload),
    };
    // Never send the boat's API key to any host other than ollama.com or *.ollama.com.
    if (isOllamaCom && apiKey) {
      headers.Authorization = `Bearer ${apiKey}`;
    }

    const client = target.protocol === 'https:' ? https : http;
    const req = client.request(target, {method: 'POST', timeout: timeoutMs, headers}, res => {
      let data = '';
      res.setEncoding('utf8');
      res.on('data', chunk => { data += chunk; });
      res.on('end', () => {
        if (res.statusCode !== 200) {
          const err = new Error(`Ollama cloud HTTP ${res.statusCode}`);
          err.offline = res.statusCode >= 500 || res.statusCode === 408 || res.statusCode === 429;
          return reject(err);
        }
        try {
          const parsed = JSON.parse(data);
          const content = parsed?.message?.content ?? parsed?.response ?? '';
          if (!String(content).trim()) {
            return reject(new Error('Ollama cloud returned an empty vision description'));
          }
          resolve(String(content).trim());
        } catch (err) {
          reject(new Error(`Invalid JSON from Ollama cloud: ${err.message}`));
        }
      });
    });
    req.on('timeout', () => {
      const err = new Error('Ollama cloud vision request timed out');
      err.offline = true;
      req.destroy(err);
    });
    req.on('error', err => {
      err.offline = true;
      reject(err);
    });
    req.end(payload);
  });
}

function buildImageSidecarMarkdown({safeName, title, description}) {
  return [
    '---',
    `title: "${String(title || safeName).replace(/"/g, '\\"')}"`,
    `original_file: "user/${safeName}"`,
    'kind: "image-description"',
    '---',
    '',
    `# ${title || safeName}`,
    '',
    description.trim(),
    '',
  ].join('\n');
}

function buildPdfSidecarMarkdown({safeName, title, pagesMap}) {
  const pageNums = [...pagesMap.keys()].map(Number).filter(n => n > 0).sort((a, b) => a - b);
  const sections = pageNums.map(page => {
    const entry = pagesMap.get(String(page)) || pagesMap.get(page);
    return `# Page ${page}\n\n${String(entry.text || '').trim()}\n`;
  });
  return [
    '---',
    `title: "${String(title || safeName).replace(/"/g, '\\"')}"`,
    `original_file: "user/${safeName}"`,
    'kind: "pdf-vision-pages"',
    '---',
    '',
    ...sections,
  ].join('\n');
}

function looksLikeBookIndexPage(text) {
  const s = String(text || '').trim();
  if (!s) return false;
  if (/\[BOOK_INDEX_PAGE\]/i.test(s)) return true;
  if (/\[BOOK_TEXT_START\]/i.test(s)) return false;
  if (/^\s*index\b/im.test(s)) return true;
  // Count lines ending with page numbers or comma-separated page numbers (e.g. "Bilge pump, 14, 88-90").
  const lines = s.split(/\r?\n/).map(l => l.trim()).filter(Boolean);
  if (!lines.length) return false;
  const indexEntryLines = lines.filter(l => /[A-Za-z].*?(?:,\s*|\s+|\.{2,}\s*)\d+(?:\s*[–-]\s*\d+)?(?:\s*,\s*\d+(?:\s*[–-]\s*\d+)?)*$/.test(l));
  return indexEntryLines.length >= Math.max(2, Math.ceil(lines.length * 0.35));
}

if (!isMainThread && workerData && workerData.__lumePdfWorker && parentPort) {
  try {
    if (workerData.op === 'inspect') {
      const result = inspectPdfStructureSync(workerData.pdfPath, {
        includeTextFallback: Boolean(workerData.includeTextFallback),
      });
      parentPort.postMessage({ok: true, result});
    } else if (workerData.op === 'extractPageImage') {
      const result = extractPdfPageImage(workerData.pdfPath, workerData.pageNumber);
      parentPort.postMessage({ok: true, result});
    } else {
      parentPort.postMessage({ok: false, error: `Unknown PDF worker op: ${workerData.op}`});
    }
  } catch (err) {
    parentPort.postMessage({ok: false, error: err && err.message ? err.message : String(err)});
  }
}

module.exports = {
  MAX_UPLOAD_BYTES,
  MAX_PDF_SCAN_BYTES,
  MAX_PDF_OBJECTS,
  ALLOWED_EXTENSIONS,
  IMAGE_EXTENSIONS,
  MIME_BY_EXT,
  DEFAULT_VISION_MODEL,
  uploadError,
  sanitizeUploadName,
  verifyMagicBytes,
  userPaths,
  readJsonSafe,
  writeJsonAtomic,
  streamUploadToDisk,
  findJpegInBuffer,
  scanPdfBufferLinear,
  extractEmbeddedPdfJpeg,
  extractPdfPageImage,
  extractPdfPageImageAsync,
  hasPdftoppm,
  inspectPdfPages,
  isOllamaCloudHost,
  resolveCloudVisionConfig,
  callOllamaCloudVision,
  buildImageSidecarMarkdown,
  buildPdfSidecarMarkdown,
  looksLikeBookIndexPage,
};

'use strict';
// Version and build details for the Status & Diagnostics tab: the plugin, the lume
// program it runs, Signal K and Node. Collected once per binary and cached; nothing
// here blocks the plugin starting.
const fs = require('node:fs');
const path = require('node:path');
const crypto = require('node:crypto');
const {execFile} = require('node:child_process');

const pluginPackage = require('../package.json');

function signalkVersion(app) {
  if (app && app.config && typeof app.config.version === 'string') return app.config.version;
  for (const base of [process.cwd(), require.main && require.main.path, path.join(__dirname, '..', '..', '..')].filter(Boolean)) {
    try {
      return require(require.resolve('signalk-server/package.json', {paths: [base]})).version;
    } catch (_) {
      // Try the next place Signal K might be installed.
    }
  }
  return null;
}

function lumeVersion(binary) {
  return new Promise(resolve => {
    execFile(binary, ['--version'], {timeout: 5000}, (err, stdout) => {
      if (err) return resolve(null);
      const match = /lume\s+([^\r\n]+)/.exec(String(stdout));
      resolve(match ? match[1].trim() : String(stdout).trim().slice(0, 60) || null);
    });
  });
}

function sha256(file) {
  return new Promise(resolve => {
    const hash = crypto.createHash('sha256');
    fs.createReadStream(file).on('data', d => hash.update(d)).on('error', () => resolve(null))
      .on('end', () => resolve(hash.digest('hex')));
  });
}

/** The manifest the packager writes beside the binary (bin/manifest.json), if any. */
function manifestFor(binary) {
  try {
    const manifest = JSON.parse(fs.readFileSync(path.join(path.dirname(binary), 'manifest.json'), 'utf8'));
    const entry = manifest.binaries ? Object.values(manifest.binaries)[0] : manifest;
    return {
      glibcMax: entry.glibc_max ?? entry.glibc ?? manifest.glibc_ceiling ?? null,
      manifestSha256: entry.sha256 || null,
    };
  } catch (_) {
    return {};
  }
}

let cache = {binary: null, value: null, pending: null};

/** Collect (once per binary) and return the version details. */
async function collect({app, binaryInfo}) {
  const binary = binaryInfo && binaryInfo.found ? binaryInfo.path : null;
  if (cache.value && cache.binary === binary) return cache.value;
  if (cache.pending && cache.binary === binary) return cache.pending;
  cache = {binary, value: null, pending: (async () => {
    const lume = {path: binary, source: binaryInfo ? binaryInfo.source : null, version: null};
    if (binary) {
      try {
        const st = fs.statSync(binary);
        lume.bytes = st.size;
        lume.modified = st.mtime.toISOString();
      } catch (_) {}
      const [version, digest] = await Promise.all([lumeVersion(binary), sha256(binary)]);
      lume.version = version;
      lume.sha256 = digest;
      Object.assign(lume, manifestFor(binary));
      if (lume.manifestSha256 && digest) lume.manifestMatches = lume.manifestSha256 === digest;
    }
    const value = {
      plugin: {name: pluginPackage.name, version: pluginPackage.version},
      lume,
      signalk: {version: signalkVersion(app)},
      node: {version: process.version, platform: `${process.platform}-${process.arch}`},
    };
    cache.value = value;
    cache.pending = null;
    return value;
  })()};
  return cache.pending;
}

/** What is known now, without waiting (null until the first collection finishes). */
function cached() {
  return cache.value;
}

module.exports = {collect, cached};

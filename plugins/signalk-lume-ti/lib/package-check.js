'use strict';
const fs = require('node:fs');
const path = require('node:path');
const crypto = require('node:crypto');

const machines = {arm64: 183, x64: 62};
function elfHeader(file, arch) {
  const fd = fs.openSync(file, 'r');
  const header = Buffer.alloc(64);
  try {
    if (fs.readSync(fd, header, 0, 64, 0) !== 64) throw new Error('truncated ELF header');
  } finally { fs.closeSync(fd); }
  if (!header.subarray(0, 4).equals(Buffer.from([0x7f, 69, 76, 70])) ||
      header[4] !== 2 || header[5] !== 1 || header[6] !== 1) {
    throw new Error('expected 64-bit little-endian ELF');
  }
  if (header.readUInt16LE(18) !== machines[arch]) throw new Error('ELF architecture does not match ' + arch);
  if (![2, 3].includes(header.readUInt16LE(16)) || header.readBigUInt64LE(24) === 0n) {
    throw new Error('ELF is not an executable with an entry point');
  }
}
async function sha256(file) {
  const hash = crypto.createHash('sha256');
  for await (const chunk of fs.createReadStream(file)) hash.update(chunk);
  return hash.digest('hex');
}
function versionCompare(a, b) {
  const x = a.split('.').map(Number), y = b.split('.').map(Number);
  for (let i = 0; i < Math.max(x.length, y.length); i++) {
    const diff = (x[i] || 0) - (y[i] || 0);
    if (diff) return diff;
  }
  return 0;
}
async function checkPackage(root) {
  let manifest;
  try { manifest = JSON.parse(fs.readFileSync(path.join(root, 'bin/manifest.json'), 'utf8')); }
  catch (_) { throw new Error('Missing binary manifest; assemble with bash scripts/package-plugin.sh --arm64 <lume> --x64 <lume> --output <dir> before npm pack'); }
  if (manifest.format !== 1) throw new Error('unsupported binary manifest');
  for (const arch of Object.keys(machines)) {
    const file = path.join(root, 'bin', 'linux-' + arch, 'lume');
    const entry = manifest.binaries?.[arch];
    if (!entry || entry.path !== 'bin/linux-' + arch + '/lume') throw new Error('missing binary for ' + arch);
    elfHeader(file, arch);
    if ((fs.statSync(file).mode & 0o111) === 0) throw new Error('binary is not executable: ' + arch);
    if (entry.glibc_max !== null && versionCompare(entry.glibc_max, '2.39') > 0) {
      throw new Error('glibc requirement exceeds 2.39: ' + arch);
    }
    if (await sha256(file) !== entry.sha256) throw new Error('binary checksum mismatch: ' + arch);
    if (fs.statSync(file).size !== entry.bytes) throw new Error('binary size mismatch: ' + arch);
  }
}
if (require.main === module) checkPackage(path.resolve(__dirname, '..')).catch(error => {
  console.error('Package check: ' + error.message); process.exitCode = 1;
});
module.exports = {elfHeader, sha256, versionCompare, checkPackage};

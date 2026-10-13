'use strict';
// Tiny Linux executables for the npm packaging tests: a native x64 program
// built with cc, and a hand-assembled AArch64 one that readelf and the strip
// tools can inspect without a cross compiler.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const {spawnSync} = require('node:child_process');

function temporary(t) {
  const parent = process.env.CARGO_TARGET_TMPDIR || path.resolve(__dirname, '../../.test-tmp');
  fs.mkdirSync(parent, {recursive: true});
  const dir = fs.mkdtempSync(path.join(parent, 'pack-test-'));
  t.after(() => fs.rmSync(dir, {recursive: true, force: true}));
  return dir;
}
function fixtures(t, source = 'int main(void) {return 0;}\n') {
  const dir = temporary(t);
  const x64 = path.join(dir, 'lume-x64');
  const arm64 = path.join(dir, 'lume-arm64');
  const cc = spawnSync('cc', ['-g', '-x', 'c', '-o', x64, '-'], {input: source, encoding: 'utf8'});
  assert.equal(cc.status, 0, cc.error?.message || cc.stderr);
  // A small static AArch64 exit(0) executable, with text/string-table sections.
  const blob = Buffer.alloc(352), strings = Buffer.from('\0.text\0.shstrtab\0');
  Buffer.from([0x7f,69,76,70,2,1,1]).copy(blob);
  blob.writeUInt16LE(2,16); blob.writeUInt16LE(183,18); blob.writeUInt32LE(1,20);
  blob.writeBigUInt64LE(0x400078n,24); blob.writeBigUInt64LE(64n,32); blob.writeBigUInt64LE(160n,40);
  blob.writeUInt16LE(64,52); blob.writeUInt16LE(56,54); blob.writeUInt16LE(1,56);
  blob.writeUInt16LE(64,58); blob.writeUInt16LE(3,60); blob.writeUInt16LE(2,62);
  blob.writeUInt32LE(1,64); blob.writeUInt32LE(5,68);
  blob.writeBigUInt64LE(0x400000n,80); blob.writeBigUInt64LE(0x400000n,88);
  blob.writeBigUInt64LE(132n,96); blob.writeBigUInt64LE(132n,104); blob.writeBigUInt64LE(4096n,112);
  for (const [i, instruction] of [0xd2800000,0xd2800ba8,0xd4000001].entries()) blob.writeUInt32LE(instruction,120+i*4);
  strings.copy(blob,132);
  blob.writeUInt32LE(1,224); blob.writeUInt32LE(1,228); blob.writeBigUInt64LE(6n,232);
  blob.writeBigUInt64LE(0x400078n,240); blob.writeBigUInt64LE(120n,248); blob.writeBigUInt64LE(12n,256); blob.writeBigUInt64LE(4n,272);
  blob.writeUInt32LE(7,288); blob.writeUInt32LE(3,292); blob.writeBigUInt64LE(132n,312);
  blob.writeBigUInt64LE(BigInt(strings.length),320); blob.writeBigUInt64LE(1n,336);
  fs.writeFileSync(arm64, blob, {mode: 0o755});
  return {dir, x64, arm64};
}
module.exports = {temporary, fixtures};

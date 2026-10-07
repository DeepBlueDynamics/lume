'use strict';
const {test} = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const {spawnSync} = require('node:child_process');
const {parseArgs, validateBinary, stripBinary} = require('../../../scripts/package-plugin.cjs');
const {checkPackage} = require('../lib/package-check');

const native = process.platform === 'linux' && process.arch === 'x64';
function temporary(t) {
  const parent = process.env.CARGO_TARGET_TMPDIR || path.resolve(__dirname, '../../../.test-tmp');
  fs.mkdirSync(parent, {recursive: true});
  const dir = fs.mkdtempSync(path.join(parent, 'pack-test-'));
  t.after(() => fs.rmSync(dir, {recursive: true, force: true}));
  return dir;
}
function fixtures(t) {
  const dir = temporary(t);
  const x64 = path.join(dir, 'lume-x64');
  const arm64 = path.join(dir, 'lume-arm64');
  const cc = spawnSync('cc', ['-g', '-x', 'c', '-o', x64, '-'], {input: 'int main(void) {return 0;}\n', encoding: 'utf8'});
  assert.equal(cc.status, 0, cc.error?.message || cc.stderr);
  // A small static AArch64 exit(0) executable, with text/string-table sections
  // so real readelf and stripping tools can inspect it without a cross compiler.
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
test('AppStore metadata and files describe an offline Linux package', () => {
  const pkg = require('../package.json');
  assert.equal(pkg.signalk.displayName, 'Lume TI');
  assert.equal(pkg.signalk.appIcon, './icon.svg');
  assert.ok(pkg.keywords.includes('signalk-node-server-plugin'));
  assert.ok(pkg.keywords.includes('signalk-webapp'));
  assert.ok(pkg.keywords.includes('signalk-category-database'));
  assert.equal(pkg.engines.node, '>=18.0.0');
  for (const lifecycle of ['preinstall','install','postinstall']) assert.equal(pkg.scripts[lifecycle], undefined);
  for (const arch of ['arm64','x64']) assert.ok(pkg.files.includes('bin/linux-' + arch + '/lume'));
});
test('release arguments require both architectures', () => {
  assert.throws(() => parseArgs(['--arm64', 'one']), /both/);
  assert.throws(() => parseArgs(['--x64', 'one', '--arm64']), /expected/);
  assert.equal(parseArgs(['--help']).help, true);
});
test('wrong ELF architecture is rejected before npm pack', {skip: !native}, t => {
  const {dir,x64} = fixtures(t);
  const output = path.join(dir, 'out');
  const result = spawnSync('bash', [path.resolve(__dirname, '../../../scripts/package-plugin.sh'),
    '--arm64', x64, '--x64', x64, '--output', output], {encoding: 'utf8'});
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /architecture does not match arm64/);
  assert.equal(fs.existsSync(output), false);
});
test('too-new GLIBC version requirement is rejected before npm pack', {skip: !native}, t => {
  const {dir,x64,arm64} = fixtures(t);
  const bytes = fs.readFileSync(x64);
  const marker = Buffer.from('GLIBC_2.34');
  const offset = bytes.indexOf(marker);
  assert.ok(offset >= 0, 'Linux fixture requires modern __libc_start_main');
  Buffer.from('GLIBC_2.99').copy(bytes, offset);
  fs.writeFileSync(x64, bytes);
  const result = spawnSync('bash', [path.resolve(__dirname, '../../../scripts/package-plugin.sh'),
    '--arm64', arm64, '--x64', x64, '--output', path.join(dir, 'out')], {encoding: 'utf8'});
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /GLIBC_2.99 exceeds/);
});
test('stripping preserves executable architecture and originals', {skip: !native}, t => {
  const {dir,x64,arm64} = fixtures(t);
  for (const arch of ['arm64','x64']) {
    const input = arch === 'arm64' ? arm64 : x64;
    const original = fs.readFileSync(input);
    const destination = path.join(dir, 'stripped-' + arch);
    const report = stripBinary(input, destination, arch);
    assert.equal(report.arch, arch);
    if (arch === 'arm64') assert.equal(report.strip_tool, 'already-stripped');
    assert.deepEqual(fs.readFileSync(input), original);
    assert.ok(fs.statSync(destination).size <= original.length);
    assert.ok(fs.statSync(destination).mode & 0o111);
    validateBinary(destination, arch);
  }
});
test('packed tarball installs offline with lifecycle scripts disabled', {skip: !native}, async t => {
  const {dir,x64,arm64} = fixtures(t);
  const output = path.join(dir, 'package');
  const result = spawnSync('bash', [path.resolve(__dirname, '../../../scripts/package-plugin.sh'),
    '--arm64', arm64, '--x64', x64, '--output', output], {encoding: 'utf8', timeout: 60000});
  assert.equal(result.status, 0, result.stderr);
  const report = JSON.parse(result.stdout);
  assert.equal(fs.statSync(report.tarball).size, report.compressed_bytes);
  assert.ok(report.files.includes('public/icon.svg'));
  assert.ok(report.files.includes('icon.svg'));
  assert.ok(!report.files.some(name => name.startsWith('test/')));
  const install = path.join(dir, 'install');
  fs.mkdirSync(install);
  fs.writeFileSync(path.join(install,'package.json'), '{"name":"fixture-install","version":"1.0.0","private":true}');
  const installed = spawnSync('npm', ['install','--ignore-scripts','--offline','--no-audit','--no-fund',
    '--cache', path.join(dir, 'npm-cache'), report.tarball], {cwd: install, encoding:'utf8', timeout:60000});
  assert.equal(installed.status, 0, installed.error?.message || installed.stderr);
  const plugin = path.join(install, 'node_modules/signalk-lume-ti');
  await checkPackage(plugin);
  assert.equal(typeof require(path.join(plugin,'index.js')), 'function');
  const resolver = require(path.join(plugin,'lib/resolver.js'));
  const binary = resolver.resolveLumeBinary({},plugin);
  assert.equal(binary.source,'bundled');
  assert.equal(spawnSync(binary.path,[],{encoding:'utf8'}).status,0);
  fs.appendFileSync(binary.path,'tampered');
  await assert.rejects(checkPackage(plugin), /checksum mismatch/);
});

'use strict';
const {test} = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const {spawnSync} = require('node:child_process');
const {TARGETS, parseArgs, checkForeignBinary} = require('../../../scripts/package-cli.cjs');
const {temporary, fixtures} = require('../../../scripts/test/elf-fixtures.cjs');

const native = process.platform === 'linux' && process.arch === 'x64';
function machO(file, cpu) {
  const header = Buffer.alloc(64);
  header.writeUInt32LE(0xfeedfacf, 0); header.writeUInt32LE(cpu, 4);
  fs.writeFileSync(file, header, {mode: 0o755});
  return file;
}
function pe(file, machine = 0x8664) {
  const header = Buffer.alloc(256);
  header.write('MZ', 0, 'latin1'); header.writeUInt32LE(128, 60);
  header.write('PE\0\0', 128, 'latin1'); header.writeUInt16LE(machine, 132);
  fs.writeFileSync(file, header);
  return file;
}

test('package.json runs the launcher and pins every binary package to its version', () => {
  const pkg = require('../package.json');
  assert.equal(pkg.name, '@deepbluedynamics/lume');
  assert.equal(pkg.bin.lume, 'bin/lume.js');
  assert.match(fs.readFileSync(path.join(__dirname, '../bin/lume.js'), 'utf8'), /^#!\/usr\/bin\/env node\n/);
  assert.deepEqual(Object.keys(pkg.optionalDependencies).sort(), TARGETS.map(t => '@deepbluedynamics/lume-' + t).sort());
  for (const version of Object.values(pkg.optionalDependencies)) assert.equal(version, pkg.version);
  assert.equal(pkg.os, undefined, 'unsupported hosts get the launcher error, not an install failure');
  assert.equal(pkg.repository.url, 'git+https://github.com/DeepBlueDynamics/lume.git');
  for (const lifecycle of ['preinstall', 'install', 'postinstall']) assert.equal(pkg.scripts[lifecycle], undefined);
});

test('packaging requires a binary for every target', () => {
  assert.throws(() => parseArgs(['--linux-x64', 'one']), /missing binaries for linux-arm64/);
  assert.throws(() => parseArgs(['--linux-x64', 'one', '--linux-x64', 'two']), /expected/);
  assert.equal(parseArgs(['--help']).help, true);
});

test('Mach-O and PE binaries are checked for platform and architecture', t => {
  const dir = temporary(t);
  checkForeignBinary(machO(path.join(dir, 'arm'), 0x0100000c), 'darwin-arm64');
  checkForeignBinary(machO(path.join(dir, 'intel'), 0x01000007), 'darwin-x64');
  checkForeignBinary(pe(path.join(dir, 'lume.exe')), 'win32-x64');
  assert.throws(() => checkForeignBinary(path.join(dir, 'intel'), 'darwin-arm64'), /architecture does not match/);
  assert.throws(() => checkForeignBinary(path.join(dir, 'lume.exe'), 'darwin-x64'), /Mach-O/);
  assert.throws(() => checkForeignBinary(pe(path.join(dir, 'arm.exe'), 0xaa64), 'win32-x64'), /not x64/);
  assert.throws(() => checkForeignBinary(path.join(dir, 'arm'), 'win32-x64'), /PE executable/);
});

test('npm install -g style: the launcher runs the x64 binary with arguments and exit status', {skip: !native}, t => {
  const program = '#include <stdio.h>\nint main(int c, char **v) { for (int i = 1; i < c; i++) printf("[%s]", v[i]); return 3; }\n';
  const {dir, x64, arm64} = fixtures(t, program);
  const output = path.join(dir, 'out');
  const args = ['--linux-x64', x64, '--linux-arm64', arm64,
    '--darwin-arm64', machO(path.join(dir, 'mac-arm'), 0x0100000c),
    '--darwin-x64', machO(path.join(dir, 'mac-intel'), 0x01000007),
    '--win32-x64', pe(path.join(dir, 'lume.exe')), '--output', output];
  const packed = spawnSync(process.execPath, [path.resolve(__dirname, '../../../scripts/package-cli.cjs'), ...args],
    {encoding: 'utf8', timeout: 60000});
  assert.equal(packed.status, 0, packed.stderr);
  const report = JSON.parse(packed.stdout);
  assert.deepEqual(report.files, ['LICENSE', 'README.md', 'bin/lume.js', 'package.json']);
  assert.deepEqual(report.packages['win32-x64'].files, ['LICENSE', 'README.md', 'bin/lume.exe', 'bin/manifest.json', 'package.json']);
  assert.equal(report.packages['linux-x64'].manifest.glibc_max !== null, true);

  const install = path.join(dir, 'install');
  fs.mkdirSync(install);
  fs.writeFileSync(path.join(install, 'package.json'), '{"name":"fixture-install","version":"1.0.0","private":true}');
  const installed = spawnSync('npm', ['install', '--ignore-scripts', '--offline', '--no-audit', '--no-fund',
    '--cache', path.join(dir, 'npm-cache'), report.tarball, report.packages['linux-x64'].tarball],
  {cwd: install, encoding: 'utf8', timeout: 60000});
  assert.equal(installed.status, 0, installed.error?.message || installed.stderr);
  const run = spawnSync(path.join(install, 'node_modules/.bin/lume'), ['a', 'b c'], {encoding: 'utf8'});
  assert.equal(run.stdout, '[a][b c]');
  assert.equal(run.status, 3);
});

test('the launcher explains a missing binary package', {skip: !native}, t => {
  const dir = temporary(t);
  fs.cpSync(path.join(__dirname, '../bin'), path.join(dir, 'bin'), {recursive: true});
  const run = spawnSync(process.execPath, [path.join(dir, 'bin/lume.js'), '--version'], {encoding: 'utf8'});
  assert.equal(run.status, 1);
  assert.match(run.stderr, /no prebuilt binary for linux-x64/);
  assert.match(run.stderr, /--omit=optional/);
});

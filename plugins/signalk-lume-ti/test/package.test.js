'use strict';
const {test} = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const {spawnSync} = require('node:child_process');
const {parseArgs, validateBinary, stripBinary} = require('../../../scripts/package-plugin.cjs');
const {checkPackage} = require('../lib/package-check');

const {fixtures} = require('../../../scripts/test/elf-fixtures.cjs');

const native = process.platform === 'linux' && process.arch === 'x64';
test('AppStore metadata and files describe an offline Linux package', () => {
  const pkg = require('../package.json');
  assert.equal(pkg.signalk.displayName, 'Lume TI');
  assert.equal(pkg.signalk.appIcon, './icon.svg');
  assert.ok(pkg.keywords.includes('signalk-node-server-plugin'));
  assert.ok(pkg.keywords.includes('signalk-webapp'));
  assert.ok(pkg.keywords.includes('signalk-category-database'));
  assert.equal(pkg.engines.node, '>=18.0.0');
  for (const lifecycle of ['preinstall','install','postinstall']) assert.equal(pkg.scripts[lifecycle], undefined);
  assert.ok(!pkg.files.some(name => name.startsWith('bin/')));
  for (const arch of ['arm64','x64']) assert.equal(pkg.optionalDependencies['signalk-lume-ti-linux-' + arch], pkg.version);
  assert.equal(pkg.repository.url, 'git+https://github.com/DeepBlueDynamics/lume.git');
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
test('packed tarballs install offline with lifecycle scripts disabled', {skip: !native}, async t => {
  const {dir,x64,arm64} = fixtures(t);
  const output = path.join(dir, 'package');
  const result = spawnSync('bash', [path.resolve(__dirname, '../../../scripts/package-plugin.sh'),
    '--arm64', arm64, '--x64', x64, '--output', output], {encoding: 'utf8', timeout: 60000});
  assert.equal(result.status, 0, result.stderr);
  const report = JSON.parse(result.stdout);
  assert.equal(fs.statSync(report.tarball).size, report.compressed_bytes);
  assert.ok(report.files.includes('public/icon.svg'));
  assert.ok(report.files.includes('icon.svg'));
  assert.ok(!report.files.some(name => name.startsWith('test/') || name.startsWith('bin/')));
  for (const arch of ['arm64','x64']) {
    assert.equal(report.packages[arch].package, 'signalk-lume-ti-linux-' + arch);
    assert.deepEqual(report.packages[arch].files, ['LICENSE','README.md','bin/lume','bin/manifest.json','package.json']);
  }
  const install = path.join(dir, 'install');
  fs.mkdirSync(install);
  fs.writeFileSync(path.join(install,'package.json'), '{"name":"fixture-install","version":"1.0.0","private":true}');
  const installed = spawnSync('npm', ['install','--ignore-scripts','--offline','--no-audit','--no-fund',
    '--cache', path.join(dir, 'npm-cache'), report.tarball, report.packages.x64.tarball], {cwd: install, encoding:'utf8', timeout:60000});
  assert.equal(installed.status, 0, installed.error?.message || installed.stderr);
  const plugin = path.join(install, 'node_modules/signalk-lume-ti');
  await checkPackage(plugin);
  const binaries = path.join(install, 'node_modules/signalk-lume-ti-linux-x64');
  await checkPackage(binaries);
  assert.equal(typeof require(path.join(plugin,'index.js')), 'function');
  const resolver = require(path.join(plugin,'lib/resolver.js'));
  const binary = resolver.resolveLumeBinary({},plugin);
  assert.equal(binary.source,'npm-optional');
  assert.equal(spawnSync(binary.path,[],{encoding:'utf8'}).status,0);
  fs.appendFileSync(binary.path,'tampered');
  await assert.rejects(checkPackage(binaries), /checksum mismatch/);
});

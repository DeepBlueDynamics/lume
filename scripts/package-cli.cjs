'use strict';
// Pack the lume CLI for npm: @deepbluedynamics/lume (a launcher, no binary)
// and one @deepbluedynamics/lume-<platform>-<arch> package per release target,
// which npm installs as an optional dependency only on a matching host.
//
//   node scripts/package-cli.cjs --linux-x64 <lume> --linux-arm64 <lume> \
//     --darwin-arm64 <lume> --darwin-x64 <lume> --win32-x64 <lume.exe> --output <dir>
//
// Linux binaries get the plugin's ELF/glibc <= 2.39 checks and are stripped;
// macOS and Windows binaries are checked for Mach-O / PE architecture.
const fs = require('node:fs');
const path = require('node:path');
const {sha256} = require('../plugins/signalk-lume-ti/lib/package-check');
const {validateBinary, stripBinary, run} = require('./package-plugin.cjs');

const SCOPE = '@deepbluedynamics/lume';
const LABELS = {'linux-x64': 'Linux x64', 'linux-arm64': 'Linux arm64', 'darwin-arm64': 'macOS Apple Silicon',
  'darwin-x64': 'macOS Intel', 'win32-x64': 'Windows x64'};

// The README npm shows for one platform package: what it is, and what to install instead.
function binaryReadme(name, target, version, executable) {
  const label = LABELS[target];
  return [
    '# ' + name, '',
    'The ' + label + ' `lume` program for **[' + SCOPE + '](https://www.npmjs.com/package/' + SCOPE + ')**, ' +
      'the Lume command-line tool. Lume keeps your documents and your time-series telemetry in one store: ' +
      'search the words, query the numbers with SQL, and give agents all of it as MCP tools.', '',
    '**Install `' + SCOPE + '`, not this package.** npm adds this one by itself on a ' + label + ' machine:', '',
    '```sh', 'npm install -g ' + SCOPE, 'lume --version', '```', '',
    '## What is inside', '',
    '- `bin/' + executable + '`: Lume ' + version + ' for ' + label + ', built with the `ti` feature, so `lume ti` is included.',
    '- `bin/manifest.json`: the program\'s size and SHA-256' +
      (target.startsWith('linux') ? ', and the glibc version it needs (the packaging ceiling is 2.39).' : '.'), '',
    'Source and documentation: [DeepBlueDynamics/lume](https://github.com/DeepBlueDynamics/lume). License: BSD-3-Clause.', '',
  ].join('\n');
}
const TARGETS = ['linux-x64', 'linux-arm64', 'darwin-arm64', 'darwin-x64', 'win32-x64'];

function parseArgs(args) {
  const options = {output: path.resolve(__dirname, '../.package-cli')};
  if (args.includes('--help')) return {help: true};
  for (let i = 0; i < args.length; i += 2) {
    const key = args[i]?.replace(/^--/, '');
    if (!(TARGETS.includes(key) || key === 'output') || !args[i + 1] || args[i + 1].startsWith('--') ||
        (options[key] && key !== 'output')) {
      throw new Error('expected ' + TARGETS.map(target => '--' + target + ' <binary>').join(' ') + ' [--output <dir>]');
    }
    options[key] = path.resolve(args[i + 1]);
  }
  const missing = TARGETS.filter(target => !options[target]);
  if (missing.length) throw new Error('missing binaries for ' + missing.join(', '));
  return options;
}
// Mach-O 64-bit (thin) and PE32+ headers: enough to refuse a binary built for
// the wrong platform or architecture.
function checkForeignBinary(file, target) {
  const header = Buffer.alloc(4096);
  const fd = fs.openSync(file, 'r');
  let read;
  try { read = fs.readSync(fd, header, 0, header.length, 0); } finally { fs.closeSync(fd); }
  if (target.startsWith('darwin-')) {
    if (read < 8 || header.readUInt32LE(0) !== 0xfeedfacf) throw new Error(target + ': expected a 64-bit Mach-O executable');
    const cpu = {'darwin-arm64': 0x0100000c, 'darwin-x64': 0x01000007}[target];
    if (header.readUInt32LE(4) !== cpu) throw new Error(target + ': Mach-O architecture does not match');
    return;
  }
  const pe = read >= 64 && header.toString('latin1', 0, 2) === 'MZ' ? header.readUInt32LE(60) : -1;
  if (pe < 0 || pe + 6 > read || header.toString('latin1', pe, pe + 4) !== 'PE\0\0') {
    throw new Error(target + ': expected a PE executable');
  }
  if (header.readUInt16LE(pe + 4) !== 0x8664) throw new Error(target + ': PE architecture is not x64');
}
function validate(options) {
  for (const target of TARGETS) {
    const [platform, arch] = target.split('-');
    if (platform === 'linux') validateBinary(options[target], arch);
    else checkForeignBinary(options[target], target);
  }
}
async function pack(stage, output, required) {
  const result = JSON.parse(run('npm', ['pack', '--json', '--offline', '--pack-destination', output], {
    cwd: stage, env: {...process.env, npm_config_cache: path.join(output, '.npm-cache')},
  }))[0];
  const names = new Set(result.files.map(file => file.path));
  for (const name of required) if (!names.has(name)) throw new Error('npm pack omitted ' + name + ' from ' + result.name);
  if ([...names].some(name => /(^|\/)(test|node_modules|\.git)(\/|$)/.test(name))) {
    throw new Error(result.name + ' contains test/build state');
  }
  const tarball = path.join(output, result.filename);
  return {package: result.name, version: result.version, tarball,
    compressed_bytes: fs.statSync(tarball).size, unpacked_bytes: result.unpackedSize,
    sha256: await sha256(tarball), integrity: result.integrity, files: [...names].sort()};
}
async function assemble(options) {
  const root = path.resolve(__dirname, '..');
  const source = path.join(root, 'npm/lume');
  const output = path.resolve(options.output);
  // Check every binary before creating output or copying any files.
  validate(options);
  const cli = JSON.parse(fs.readFileSync(path.join(source, 'package.json'), 'utf8'));
  for (const target of TARGETS) {
    if (cli.optionalDependencies?.[SCOPE + '-' + target] !== cli.version) {
      throw new Error(SCOPE + '-' + target + ' must be an optional dependency pinned to ' + cli.version);
    }
  }
  fs.mkdirSync(output, {recursive: true});
  const stages = [];
  const stage = name => { const dir = fs.mkdtempSync(path.join(output, name + '-')); stages.push(dir); return dir; };
  const packages = {};
  try {
    for (const target of TARGETS) {
      const [platform, arch] = target.split('-');
      const name = SCOPE + '-' + target;
      const executable = platform === 'win32' ? 'lume.exe' : 'lume';
      const dir = stage('lume-' + target);
      const destination = path.join(dir, 'bin', executable);
      fs.mkdirSync(path.dirname(destination), {recursive: true});
      let binary = {};
      if (platform === 'linux') {
        binary = stripBinary(options[target], destination, arch);
      } else {
        fs.copyFileSync(options[target], destination);
        fs.chmodSync(destination, 0o755);
      }
      const manifest = {format: 1, target, path: 'bin/' + executable, glibc_max: binary.glibc_max ?? null,
        bytes: fs.statSync(destination).size, sha256: await sha256(destination)};
      fs.writeFileSync(path.join(dir, 'bin/manifest.json'), JSON.stringify(manifest, null, 2) + '\n');
      fs.writeFileSync(path.join(dir, 'package.json'), JSON.stringify({
        name, version: cli.version,
        description: 'The ' + target + ' lume binary for ' + SCOPE + ', installed as its optional dependency',
        author: cli.author, homepage: cli.homepage, repository: cli.repository, bugs: cli.bugs, license: cli.license,
        files: ['bin/', 'README.md', 'LICENSE'], os: [platform], cpu: [arch],
      }, null, 2) + '\n');
      fs.writeFileSync(path.join(dir, 'README.md'), binaryReadme(name, target, cli.version, executable));
      fs.copyFileSync(path.join(root, 'LICENSE'), path.join(dir, 'LICENSE'));
      packages[target] = {...await pack(dir, output, ['bin/' + executable, 'bin/manifest.json', 'README.md', 'LICENSE']), manifest};
    }
    const main = stage('lume');
    for (const name of ['package.json', 'README.md', 'bin']) {
      fs.cpSync(path.join(source, name), path.join(main, name), {recursive: true});
    }
    fs.copyFileSync(path.join(root, 'LICENSE'), path.join(main, 'LICENSE'));
    fs.chmodSync(path.join(main, 'bin/lume.js'), 0o755);
    const report = {...await pack(main, output, ['bin/lume.js', 'README.md', 'LICENSE', 'package.json']), packages};
    fs.writeFileSync(path.join(output, 'cli-report.json'), JSON.stringify(report, null, 2) + '\n');
    return report;
  } finally { for (const dir of stages) fs.rmSync(dir, {recursive: true, force: true}); }
}
if (require.main === module) {
  (async () => {
    const options = parseArgs(process.argv.slice(2));
    if (options.help) {
      console.log('Usage: node scripts/package-cli.cjs ' + TARGETS.map(target => '--' + target + ' <binary>').join(' ') +
        ' [--output <dir>]\nPacks @deepbluedynamics/lume and one binary package per target, offline.');
      return;
    }
    const report = await assemble(options);
    for (const packed of [report, ...Object.values(report.packages)]) {
      console.error(path.basename(packed.tarball) + ': ' + packed.compressed_bytes + ' bytes compressed, ' +
        packed.unpacked_bytes + ' bytes unpacked');
    }
    console.log(JSON.stringify(report, null, 2));
  })().catch(error => { console.error('Packaging failed: ' + error.message); process.exitCode = 1; });
}
module.exports = {TARGETS, parseArgs, checkForeignBinary, assemble};

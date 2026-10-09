'use strict';
const fs = require('node:fs');
const path = require('node:path');
const {spawnSync} = require('node:child_process');
const {elfHeader, sha256, versionCompare, checkPackage} = require('../plugins/signalk-lume-ti/lib/package-check');

function run(command, args, options = {}) {
  const result = spawnSync(command, args, {
    encoding: 'utf8', maxBuffer: 8 * 1024 * 1024,
    env: {...process.env, LC_ALL: 'C'}, ...options,
  });
  if (result.error || result.status !== 0) {
    throw new Error(command + ': ' + (result.error?.message || result.stderr || result.stdout || result.status));
  }
  return result.stdout;
}
function validateBinary(file, arch) {
  if (!['arm64', 'x64'].includes(arch)) throw new Error('unsupported architecture: ' + arch);
  if (!fs.lstatSync(file).isFile()) throw new Error('binary must be a regular file: ' + file);
  elfHeader(file, arch);
  const info = run('readelf', ['--file-header', '--program-headers', '--version-info', '--wide', file]);
  const interpreter = info.match(/Requesting program interpreter: ([^\]]+)/)?.[1];
  const loaders = {arm64: ['/lib/ld-linux-aarch64.so.1'], x64: [
    '/lib64/ld-linux-x86-64.so.2', '/lib/x86_64-linux-gnu/ld-linux-x86-64.so.2',
  ]};
  if (interpreter && !loaders[arch].includes(interpreter)) {
    throw new Error('unsupported Linux interpreter for ' + arch + ': ' + interpreter);
  }
  const versions = [];
  for (const match of info.matchAll(/\bName: (GLIBC_[A-Za-z0-9_.]+)/g)) {
    const name = match[1];
    if (/^GLIBC_\d+(\.\d+)+$/.test(name)) versions.push(name.slice(6));
    else if (name === 'GLIBC_ABI_DT_RELR') versions.push('2.36');
    else throw new Error('unsupported glibc ABI requirement: ' + name);
  }
  versions.sort(versionCompare);
  const glibcMax = versions.at(-1) || null;
  if (interpreter && !glibcMax) throw new Error('dynamic GNU executable has no verifiable glibc requirements');
  if (glibcMax && versionCompare(glibcMax, '2.39') > 0) {
    throw new Error('GLIBC_' + glibcMax + ' exceeds the GLIBC_2.39 ceiling for ' + arch);
  }
  const description = spawnSync('file', ['--brief', file], {encoding: 'utf8', env: {...process.env, LC_ALL: 'C'}});
  if (description.error && description.error.code !== 'ENOENT') throw description.error;
  if (!description.error && description.status !== 0) throw new Error('file inspection failed: ' + description.stderr);
  return {arch, glibc_max: glibcMax, interpreter: interpreter || null,
    file_description: description.error ? null : description.stdout.trim()};
}
function stripBinary(source, destination, arch, explicitTool) {
  const sections = run('readelf', ['--sections', '--wide', source]);
  if (!/\s\.(symtab|z?debug_[^ ]*)\s/.test(sections)) {
    fs.copyFileSync(source, destination);
    const result = validateBinary(destination, arch);
    fs.chmodSync(destination, 0o755);
    return {...result, strip_tool: 'already-stripped'};
  }
  const commands = explicitTool ? [[explicitTool, ['--strip-all']]] : [
    ['llvm-strip', ['--strip-all']],
    [arch === 'arm64' ? 'aarch64-linux-gnu-strip' : 'x86_64-linux-gnu-strip', ['--strip-all']],
    ['strip', ['--strip-all']],
  ];
  const failures = [];
  for (const [command, args] of commands) {
    fs.copyFileSync(source, destination);
    try {
      run(command, [...args, destination]);
      const result = validateBinary(destination, arch);
      const sections = run('readelf', ['--sections', '--wide', destination]);
      if (/\s\.(symtab|z?debug_[^ ]*)\s/.test(sections)) throw new Error('debug/symbol sections remain after stripping');
      fs.chmodSync(destination, 0o755);
      return {...result, strip_tool: command};
    } catch (error) { failures.push(error.message.trim()); }
  }
  throw new Error('No working strip tool for ' + arch + '. Install llvm-strip or target binutils. ' + failures.join('; '));
}
function parseArgs(args) {
  const options = {output: path.resolve(__dirname, '../.package-plugin')};
  const names = {'--arm64': 'arm64', '--x64': 'x64', '--output': 'output', '--strip-tool': 'stripTool'};
  if (args.includes('--help')) return {help: true};
  for (let i = 0; i < args.length; i += 2) {
    const key = names[args[i]];
    if (!key || !args[i + 1] || args[i + 1].startsWith('--') || options[key] && key !== 'output') {
      throw new Error('expected --arm64 <binary> --x64 <binary> [--output <dir>] [--strip-tool <tool>]');
    }
    options[key] = key === 'stripTool' ? args[i + 1] : path.resolve(args[i + 1]);
  }
  if (!options.arm64 || !options.x64) throw new Error('both --arm64 and --x64 binaries are required');
  return options;
}
function copyTree(source, target) {
  const stat = fs.lstatSync(source);
  if (stat.isSymbolicLink()) throw new Error('symlink refused in package inputs: ' + source);
  if (stat.isDirectory()) {
    fs.mkdirSync(target, {recursive: true});
    for (const name of fs.readdirSync(source)) copyTree(path.join(source, name), path.join(target, name));
  } else if (stat.isFile()) fs.copyFileSync(source, target);
  else throw new Error('unsupported file type: ' + source);
}
async function assemble(options) {
  const root = path.resolve(__dirname, '..');
  const plugin = path.join(root, 'plugins/signalk-lume-ti');
  const output = path.resolve(options.output);
  if (output === plugin || output.startsWith(plugin + path.sep)) throw new Error('output must be outside the source plugin');
  // Validate both originals before creating output or copying any files.
  for (const arch of ['arm64', 'x64']) validateBinary(options[arch], arch);
  fs.mkdirSync(output, {recursive: true});
  const stage = fs.mkdtempSync(path.join(output, 'signalk-lume-ti-'));
  const manifest = {format: 1, glibc_ceiling: '2.39', binaries: {}};
  try {
    for (const name of ['package.json', 'index.js', 'lib', 'public', 'library', 'README.md']) {
      copyTree(path.join(plugin, name), path.join(stage, name));
    }
    fs.copyFileSync(path.join(root, 'LICENSE'), path.join(stage, 'LICENSE'));
    const pkg = JSON.parse(fs.readFileSync(path.join(stage, 'package.json'), 'utf8'));
    // AppStore uses package-relative icons; v2.31 Webapps uses public-relative.
    const icon = pkg.signalk.appIcon.replace(/^\.\//, '');
    if (!/^[a-zA-Z0-9_-]+\.(png|svg)$/.test(icon)) throw new Error('appIcon must name a public PNG/SVG basename');
    fs.copyFileSync(path.join(stage, 'public', icon), path.join(stage, icon));
    for (const arch of ['arm64', 'x64']) {
      const relative = 'bin/linux-' + arch + '/lume';
      const destination = path.join(stage, relative);
      fs.mkdirSync(path.dirname(destination), {recursive: true});
      const validated = stripBinary(options[arch], destination, arch, options.stripTool);
      manifest.binaries[arch] = {...validated, path: relative,
        source_bytes: fs.statSync(options[arch]).size, source_sha256: await sha256(options[arch]),
        bytes: fs.statSync(destination).size, sha256: await sha256(destination)};
    }
    fs.writeFileSync(path.join(stage, 'bin/manifest.json'), JSON.stringify(manifest, null, 2) + '\n');
    await checkPackage(stage);
    const result = JSON.parse(run('npm', ['pack', '--json', '--offline', '--pack-destination', output], {
      cwd: stage, env: {...process.env, npm_config_cache: path.join(output, '.npm-cache')},
    }))[0];
    const names = new Set(result.files.map(file => file.path));
    for (const required of ['index.js', 'public/index.html', 'library/cruiser_library.csv',
      'bin/manifest.json', 'bin/linux-arm64/lume', 'bin/linux-x64/lume', 'LICENSE', icon, 'public/' + icon]) {
      if (!names.has(required)) throw new Error('npm pack omitted ' + required);
    }
    if ([...names].some(name => /(^|\/)(test|node_modules|\.git)(\/|$)/.test(name) || /token\.txt|ti\.toml|\.env$/.test(name))) {
      throw new Error('package contains test/build/private state');
    }
    const tarball = path.join(output, result.filename);
    const report = {package: pkg.name, version: pkg.version, tarball,
      compressed_bytes: fs.statSync(tarball).size, unpacked_bytes: result.unpackedSize,
      sha256: await sha256(tarball), integrity: result.integrity, files: [...names].sort(), ...manifest};
    fs.writeFileSync(path.join(output, 'package-report.json'), JSON.stringify(report, null, 2) + '\n');
    return report;
  } finally { fs.rmSync(stage, {recursive: true, force: true}); }
}
if (require.main === module) {
  (async () => {
    const options = parseArgs(process.argv.slice(2));
    if (options.help) {
      console.log('Usage: bash scripts/package-plugin.sh --arm64 <lume> --x64 <lume> [--output <dir>] [--strip-tool <tool>]\nValidates ELF/glibc <=2.39, strips staged copies, and packs both architectures offline.');
      return;
    }
    const report = await assemble(options);
    for (const [arch, binary] of Object.entries(report.binaries)) {
      console.error(arch + ': ' + binary.source_bytes + ' -> ' + binary.bytes + ' bytes; GLIBC ' +
        (binary.glibc_max || 'none/static') + '; ' + binary.strip_tool);
      if (!binary.file_description) console.error('file command unavailable; verified ELF header and readelf for ' + arch);
    }
    console.error('Tarball: ' + report.compressed_bytes + ' bytes compressed, ' + report.unpacked_bytes + ' bytes unpacked');
    console.log(JSON.stringify(report, null, 2));
  })().catch(error => { console.error('Packaging failed: ' + error.message); process.exitCode = 1; });
}
module.exports = {parseArgs, validateBinary, stripBinary, assemble};

#!/usr/bin/env node
'use strict';
// Runs the prebuilt lume binary that npm installed as the optional dependency
// @deepbluedynamics/lume-<platform>-<arch>, passing through arguments, stdio
// and the exit status.
const path = require('node:path');
const {spawnSync} = require('node:child_process');

const target = process.platform + '-' + process.arch;
const supported = ['linux-x64', 'linux-arm64', 'darwin-arm64', 'darwin-x64', 'win32-x64'];
let binary;
try {
  const pkg = require.resolve('@deepbluedynamics/lume-' + target + '/package.json');
  binary = path.join(path.dirname(pkg), 'bin', process.platform === 'win32' ? 'lume.exe' : 'lume');
} catch (_) {
  console.error('lume: no prebuilt binary for ' + target + '. Supported: ' + supported.join(', ') + '.');
  if (supported.includes(target)) {
    console.error('lume: reinstall without --omit=optional (or --no-optional) so npm installs @deepbluedynamics/lume-' + target + '.');
  }
  process.exit(1);
}
const result = spawnSync(binary, process.argv.slice(2), {stdio: 'inherit'});
if (result.error) {
  console.error('lume: could not run ' + binary + ': ' + result.error.message);
  process.exit(1);
}
if (result.signal) process.kill(process.pid, result.signal);
process.exit(result.status ?? 1);

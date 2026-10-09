'use strict';
// Explicit paths avoid shell glob differences and older Node test runners.
const fs = require('node:fs');
const path = require('node:path');
const {spawnSync} = require('node:child_process');
const files = fs.readdirSync(__dirname).filter(name => name.endsWith('.test.js'))
  .sort().map(name => path.join(__dirname, name));
const result = spawnSync(process.execPath, ['--test', ...files], {stdio:'inherit'});
if (result.error) console.error(result.error.message);
process.exitCode = result.status ?? 1;

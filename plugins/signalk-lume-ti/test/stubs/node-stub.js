'use strict';
const childProcess = require('node:child_process');
const path = require('node:path');
const script = path.join(__dirname, 'mock-lume.js');

// The supervisor captures spawn during module loading. Keep its real process
// lifecycle, but run the JS fixture through Node rather than an executable shebang.
function withNodeStub(load) {
  const nativeSpawn = childProcess.spawn;
  childProcess.spawn = (binary, args, options) => binary === process.execPath
    ? nativeSpawn(process.execPath, [script, ...args], options)
    : nativeSpawn(binary, args, options);
  try { return load(); }
  finally { childProcess.spawn = nativeSpawn; }
}
async function waitFor(predicate, timeoutMs = 5000) {
  const deadline = Date.now() + timeoutMs;
  while (!(await predicate())) {
    if (Date.now() >= deadline) throw new Error('Timed out waiting for mock lume');
    await new Promise(resolve => setTimeout(resolve, 20));
  }
}
module.exports = {withNodeStub, waitFor};

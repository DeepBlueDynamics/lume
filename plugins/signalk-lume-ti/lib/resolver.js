'use strict';

const fs = require('fs');
const path = require('path');
const { execSync } = require('child_process');

/**
 * Resolve the path to the `lume` executable.
 *
 * Checks in priority order:
 * 1. Explicit `lumePath` in plugin configuration.
 * 2. Bundled binary at `bin/<platform>-<arch>/lume` (e.g. bin/linux-arm64/lume).
 * 3. Environment variable `LUME_BIN`.
 * 4. System PATH via `which lume` (or `where lume` on Windows).
 * 5. Relative target builds (e.g. target/release/lume or target/debug/lume) for development.
 *
 * @param {object} config - Plugin configuration object.
 * @param {string} pluginDir - Base directory of the plugin.
 * @returns {{ path: string, found: boolean, source: string }}
 */
function resolveLumeBinary(config, pluginDir) {
  // 1. Explicit path from plugin config
  if (config && typeof config.lumePath === 'string' && config.lumePath.trim().length > 0) {
    const customPath = path.resolve(config.lumePath.trim());
    if (isExecutable(customPath)) {
      return { path: customPath, found: true, source: 'config.lumePath' };
    }
  }

  // 2. Bundled platform binary
  const platform = process.platform;
  const arch = process.arch;
  const binaryName = platform === 'win32' ? 'lume.exe' : 'lume';
  const bundledPath = path.join(pluginDir, 'bin', `${platform}-${arch}`, binaryName);
  if (isExecutable(bundledPath)) {
    return { path: bundledPath, found: true, source: 'bundled' };
  }

  // Also check direct bin/lume if placed flat
  const flatBinPath = path.join(pluginDir, 'bin', binaryName);
  if (isExecutable(flatBinPath)) {
    return { path: flatBinPath, found: true, source: 'bundled-flat' };
  }

  // 3. Environment variable LUME_BIN
  if (process.env.LUME_BIN) {
    const envPath = path.resolve(process.env.LUME_BIN.trim());
    if (isExecutable(envPath)) {
      return { path: envPath, found: true, source: 'env.LUME_BIN' };
    }
  }

  // 4. System PATH
  try {
    const cmd = platform === 'win32' ? 'where lume' : 'which lume';
    const whichOut = execSync(cmd, { stdio: ['pipe', 'pipe', 'ignore'], encoding: 'utf8' }).trim();
    const firstLine = whichOut.split(/\r?\n/)[0];
    if (firstLine && isExecutable(firstLine)) {
      return { path: firstLine, found: true, source: 'path' };
    }
  } catch (_e) {
    // Not found in PATH
  }

  // 5. Development target builds
  const devCandidates = [
    path.resolve(pluginDir, '../../target/release', binaryName),
    path.resolve(pluginDir, '../../target/debug', binaryName),
    path.resolve(pluginDir, 'target/release', binaryName),
    path.resolve(pluginDir, 'target/debug', binaryName),
  ];
  for (const cand of devCandidates) {
    if (isExecutable(cand)) {
      return { path: cand, found: true, source: 'dev-target' };
    }
  }

  // Default fallback path for error reporting
  return {
    path: bundledPath,
    found: false,
    source: 'not-found',
  };
}

/**
 * Check if a file exists and is executable (or readable on Windows).
 *
 * @param {string} filePath
 * @returns {boolean}
 */
function isExecutable(filePath) {
  try {
    const stats = fs.statSync(filePath);
    if (!stats.isFile()) return false;
    if (process.platform === 'win32') {
      return true;
    }
    // Check execute permission for user, group, or others
    return (stats.mode & (fs.constants.S_IXUSR | fs.constants.S_IXGRP | fs.constants.S_IXOTH)) !== 0;
  } catch (_e) {
    return false;
  }
}

module.exports = {
  resolveLumeBinary,
  isExecutable,
};

'use strict';

const fs = require('fs');
const path = require('path');

/**
 * Collect live store metrics, disk usage, WAL size, and ingest status.
 *
 * @param {string} storeDir - Absolute path to the store directory.
 * @returns {object}
 */
function collectStoreStatus(storeDir) {
  const result = {
    exists: false,
    ingestStatus: null,
    diskBytes: 0,
    walBytes: 0,
    shardsBytes: 0,
    shardCount: 0,
    diskHuman: '0 B',
    walHuman: '0 B',
  };

  if (!fs.existsSync(storeDir)) {
    return result;
  }
  result.exists = true;

  // 1. Read ingest_status.json written by live ingest service
  const statusFile = path.join(storeDir, 'ingest_status.json');
  try {
    if (fs.existsSync(statusFile)) {
      const content = fs.readFileSync(statusFile, 'utf8');
      result.ingestStatus = JSON.parse(content);
    }
  } catch (_e) {
    // Ignore read or parse errors if file is concurrently written
  }

  // 2. Read manifest.json if present
  const manifestFile = path.join(storeDir, 'manifest.json');
  try {
    if (fs.existsSync(manifestFile)) {
      const manifest = JSON.parse(fs.readFileSync(manifestFile, 'utf8'));
      if (Array.isArray(manifest.entries)) {
        result.shardCount = manifest.entries.length;
      }
    }
  } catch (_e) {
    // Ignore manifest read error
  }

  // 3. Compute sizes for WAL and overall store
  try {
    const walDir = path.join(storeDir, 'wal');
    if (fs.existsSync(walDir)) {
      result.walBytes = calculateDirectorySize(walDir);
    }
    const shardsDir = path.join(storeDir, 'shards');
    if (fs.existsSync(shardsDir)) {
      result.shardsBytes = calculateDirectorySize(shardsDir);
    }
    result.diskBytes = calculateDirectorySize(storeDir);
  } catch (_e) {
    // Ignore directory calculation errors
  }

  result.diskHuman = formatBytes(result.diskBytes);
  result.walHuman = formatBytes(result.walBytes);

  return result;
}

/**
 * Recursively calculate total size of files under a directory.
 *
 * @param {string} dirPath
 * @returns {number}
 */
function calculateDirectorySize(dirPath) {
  let total = 0;
  try {
    const entries = fs.readdirSync(dirPath, { withFileTypes: true });
    for (const entry of entries) {
      const fullPath = path.join(dirPath, entry.name);
      try {
        if (entry.isDirectory()) {
          total += calculateDirectorySize(fullPath);
        } else if (entry.isFile()) {
          const stats = fs.statSync(fullPath);
          total += stats.size;
        }
      } catch (_e) {
        // Skip unreadable files
      }
    }
  } catch (_e) {
    // Skip unreadable dirs
  }
  return total;
}

/**
 * Format bytes into human-readable string (KB, MB, GB).
 *
 * @param {number} bytes
 * @returns {string}
 */
function formatBytes(bytes) {
  if (!bytes || bytes <= 0) return '0 B';
  const units = ['B', 'KB', 'MB', 'GB', 'TB'];
  const i = Math.floor(Math.log(bytes) / Math.log(1024));
  const val = bytes / Math.pow(1024, i);
  return `${val.toFixed(i === 0 ? 0 : 1)} ${units[i]}`;
}

module.exports = {
  collectStoreStatus,
  calculateDirectorySize,
  formatBytes,
};

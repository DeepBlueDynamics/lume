#!/usr/bin/env node
'use strict';

const fs = require('fs');
const path = require('path');
const http = require('http');

const args = process.argv.slice(2);

// Check if we are being asked to crash immediately
if (process.env.MOCK_LUME_CRASH === '1') {
  console.error('MOCK_LUME_CRASH triggered, exiting with code 42');
  process.exit(42);
}

// Write the arguments received to a debug file for test assertions
if (process.env.MOCK_LUME_LOG_ARGS) {
  fs.writeFileSync(process.env.MOCK_LUME_LOG_ARGS, JSON.stringify(args), 'utf8');
}

// Find store path
let storePath = null;
const storeIdx = args.indexOf('--store');
if (storeIdx !== -1 && args[storeIdx + 1]) {
  storePath = args[storeIdx + 1];
}

// Find port
let port = 5863;
const portIdx = args.indexOf('--port');
if (portIdx !== -1 && args[portIdx + 1]) {
  port = parseInt(args[portIdx + 1], 10);
}

// If storePath is given, write mock ingest_status.json
if (storePath) {
  try {
    fs.mkdirSync(storePath, { recursive: true });
    const statusObj = {
      running: true,
      last_delta: new Date().toISOString(),
      ingest_lag_seconds: 0.042,
      reconnects: 0,
      records_ingested: 1000,
    };
    fs.writeFileSync(path.join(storePath, 'ingest_status.json'), JSON.stringify(statusObj), 'utf8');
  } catch (_e) {
    // Ignore error
  }
}

// Start mock HTTP query server if --serve was passed
let server = null;
if (args.includes('--serve')) {
  server = http.createServer((req, res) => {
    if (req.url === '/ti/query' || req.url === '/ti/sql') {
      let body = '';
      req.on('data', (c) => { body += c; });
      req.on('end', () => {
        res.writeHead(200, { 'Content-Type': 'application/json' });
        res.end(JSON.stringify([
          { ts: '2026-06-01T00:00:00Z', 'navigation.speedOverGround': 5.2 },
          { ts: '2026-06-01T00:00:10Z', 'navigation.speedOverGround': 5.4 },
        ]));
      });
      return;
    }

    if (req.url === '/ti/schema') {
      res.writeHead(200, { 'Content-Type': 'application/json' });
      res.end(JSON.stringify({
        tables: [
          {
            name: 'telemetry',
            columns: [
              { name: 'ts', data_type: 'Timestamp', unit: 's', scale: 0 },
              { name: 'navigation.speedOverGround', data_type: 'Float64', unit: 'm/s', scale: 3 },
            ],
          },
        ],
      }));
      return;
    }

    if (req.url === '/ti/status') {
      res.writeHead(200, { 'Content-Type': 'application/json' });
      res.end(JSON.stringify({
        ok: true,
        ingest_lag_seconds: 0.042,
        records_ingested: 1000,
      }));
      return;
    }

    res.writeHead(404, { 'Content-Type': 'application/json' });
    res.end(JSON.stringify({ error: 'not found' }));
  });

  server.listen(port, '127.0.0.1');
}

// Handle SIGTERM graceful shutdown
process.on('SIGTERM', () => {
  if (process.env.MOCK_LUME_IGNORE_SIGTERM === '1') {
    // For testing kill timeout
    return;
  }
  if (storePath) {
    try {
      const statusObj = {
        running: false,
        last_delta: new Date().toISOString(),
        ingest_lag_seconds: 0,
        reconnects: 0,
        records_ingested: 1000,
      };
      fs.writeFileSync(path.join(storePath, 'ingest_status.json'), JSON.stringify(statusObj), 'utf8');
    } catch (_e) {}
  }
  if (server) {
    server.close(() => {
      process.exit(0);
    });
  } else {
    process.exit(0);
  }
});

console.log('mock-lume running');

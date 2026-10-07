#!/usr/bin/env node
// Set one version everywhere a release reads it: the root `lume` package in
// Cargo.toml and Cargo.lock, and the Signal K plugin's package.json.
//
//   node scripts/bump-version.cjs patch|minor|major
//   node scripts/bump-version.cjs 1.2.3
//
// Prints the new version on stdout. Only the root `lume` entries change.
'use strict';
const fs = require('fs');
const path = require('path');

const root = path.resolve(__dirname, '..');
const files = {
  cargo: path.join(root, 'Cargo.toml'),
  lock: path.join(root, 'Cargo.lock'),
  plugin: path.join(root, 'plugins/signalk-lume-ti/package.json'),
};
const SEMVER = /^(\d+)\.(\d+)\.(\d+)$/;

function currentVersion(cargo) {
  const match = cargo.match(/^\[package\]\r?\nname = "lume"\r?\nversion = "([^"]+)"/m);
  if (!match) throw new Error('could not find [package] name = "lume" / version in Cargo.toml');
  return match[1];
}

function nextVersion(current, request) {
  if (SEMVER.test(request)) return request;
  const parts = current.match(SEMVER);
  if (!parts) throw new Error(`current version ${current} is not MAJOR.MINOR.PATCH`);
  let [major, minor, patch] = parts.slice(1).map(Number);
  if (request === 'major') [major, minor, patch] = [major + 1, 0, 0];
  else if (request === 'minor') [minor, patch] = [minor + 1, 0];
  else if (request === 'patch') patch += 1;
  else throw new Error(`expected patch, minor, major or MAJOR.MINOR.PATCH, got "${request}"`);
  return `${major}.${minor}.${patch}`;
}

function replaceOnce(text, pattern, replacement, what) {
  let count = 0;
  const out = text.replace(pattern, (...args) => {
    count += 1;
    return typeof replacement === 'function' ? replacement(...args) : replacement;
  });
  if (count !== 1) throw new Error(`expected exactly one ${what}, found ${count}`);
  return out;
}

function main() {
  const request = process.argv[2];
  if (!request) throw new Error('usage: bump-version.cjs patch|minor|major|MAJOR.MINOR.PATCH');
  const cargo = fs.readFileSync(files.cargo, 'utf8');
  const current = currentVersion(cargo);
  const next = nextVersion(current, request);
  if (next === current) throw new Error(`version is already ${current}`);

  fs.writeFileSync(files.cargo, replaceOnce(cargo,
    /^(\[package\]\r?\nname = "lume"\r?\nversion = ")[^"]+"/m, (_, head) => `${head}${next}"`,
    'root package version'));

  const lock = fs.readFileSync(files.lock, 'utf8');
  fs.writeFileSync(files.lock, replaceOnce(lock,
    /^(name = "lume"\r?\nversion = ")[^"]+"/m, (_, head) => `${head}${next}"`,
    'lume entry in Cargo.lock'));

  const plugin = JSON.parse(fs.readFileSync(files.plugin, 'utf8'));
  plugin.version = next;
  fs.writeFileSync(files.plugin, JSON.stringify(plugin, null, 2) + '\n');

  process.stdout.write(next + '\n');
}

try {
  main();
} catch (error) {
  process.stderr.write(`bump-version: ${error.message}\n`);
  process.exit(1);
}

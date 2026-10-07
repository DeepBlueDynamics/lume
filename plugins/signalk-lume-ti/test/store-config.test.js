'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const {addLastDefault,ensureLastDefault} = require('../lib/store-config');

test('store creation adds last atomically and subsequent opens do not rewrite', t => {
  const root = fs.mkdtempSync(path.join(__dirname,'store-config-'));
  t.after(()=>fs.rmSync(root,{recursive:true,force:true}));
  const store = path.join(root,'store');
  assert.equal(ensureLastDefault(store),true);
  const file = path.join(store,'ti.toml');
  const text = fs.readFileSync(file,'utf8');
  assert.ok(text.includes('opt_in = ["last"]'));
  const before = fs.statSync(file).mtimeMs;
  assert.equal(ensureLastDefault(store),false);
  assert.equal(fs.statSync(file).mtimeMs,before);
  if (process.platform !== 'win32') assert.equal(fs.statSync(file).mode & 0o777,0o600);
  assert.deepEqual(fs.readdirSync(store),['ti.toml']);
});

test('absent opt_in preserves existing keys and comments across table and dotted forms', () => {
  for (const original of [
    'width_seconds=10\n[query]\nmax_rows=300\n',
    'width_seconds=10\n[profiles] # keep\nhr_paths=["navigation.position"]\n[query]\nmax_rows=300\n',
    '["profiles"]\nhr_paths=[]',
    'profiles.hr_paths=[]',
    'profiles.hr_paths=[]\n[query]\nmax_rows=300\n',
    'profiles = { hr_paths = ["navigation.position"] }\n',
    "profiles = {}\n",
    'text="""\n[profiles]\nopt_in = ["ignore fake string"]\n"""\n',
    'text = "["\n# [profiles]\n[query]\nmax_rows=300\n',
  ]) {
    const changed = addLastDefault(original);
    assert.ok(changed.includes('opt_in = ["last"]'),original);
    assert.equal(addLastDefault(changed),changed,original);
    for (const line of original.split('\n').filter(s=>s && !s.startsWith('profiles ='))) {
      assert.ok(changed.includes(line),line);
    }
  }
});

test('explicit opt_in including empty and quoted/dotted keys is never clobbered', () => {
  for (const text of [
    '[profiles]\nopt_in = []\n',
    '[profiles]\nopt_in = [\n "count",\n "last"\n]\n',
    'profiles.opt_in = ["count"]\n',
    '"profiles"."opt_in" = []\n',
    'profiles = { opt_in = [], hr_paths = [] }\n',
    "profiles = {'opt_in'=['count']}\n",
    '["profiles"]\n"opt_in" = []\n',
  ]) assert.equal(addLastDefault(text),text);
  assert.throws(()=>addLastDefault('profiles = "invalid"'),/TOML table/);
  assert.throws(()=>addLastDefault('text = "unterminated'),/Unterminated/);
});

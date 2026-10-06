'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
test('password form clears immediately on both successful and failed saves', async () => {
  for (const ok of [true, false]) {
    const inputs = Object.fromEntries(['enablePg', 'pgPort', 'pgUser', 'pgBind', 'pgPassword']
      .map(key => [key, {value: '', checked: false}]));
    Object.assign(inputs.pgPort, {value: '5864'});
    inputs.pgUser.value = 'grafana';
    inputs.pgBind.value = '127.0.0.2';
    inputs.pgPassword.value = 'UI_SECRET';
    const form = {elements: inputs, addEventListener: (event, fn) => {form.submit = fn;}};
    const message = {textContent: ''};
    let ready, body;
    const context = {
      document: {addEventListener: (_, fn) => {ready = fn;},
        getElementById: id => id === 'pg-config' ? form : message,
        querySelector: () => ({addEventListener() {}})},
      fetch: (_, request) => {
        body = JSON.parse(request.body);
        return Promise.resolve({ok});
      },
    };
    vm.runInNewContext(fs.readFileSync(path.join(__dirname, '../public/pg.js'), 'utf8'), context);
    ready();
    const submit = form.submit({preventDefault() {}});
    assert.equal(inputs.pgPassword.value, '');
    await submit;
    assert.equal(body.pgPassword, 'UI_SECRET'); // In-flight request only.
    assert.ok(!message.textContent.includes('UI_SECRET'));
    assert.ok(message.textContent.includes(ok ? 'Saved.' : 'Save failed.'));
  }
});

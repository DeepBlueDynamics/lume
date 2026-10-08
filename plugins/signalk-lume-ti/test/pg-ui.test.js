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

test('PostgreSQL form shows whether TLS is active and auto-generated cert path when no cert configured', async () => {
  const inputs = Object.fromEntries(['enablePg', 'pgPort', 'pgUser', 'pgBind', 'pgPassword']
    .map(key => [key, {value: '', checked: false}]));
  const elements = {
    'pg-config': {elements: inputs, addEventListener: () => {}},
    'pg-message': {textContent: ''},
    'pg-tls-status': {textContent: ''},
  };
  let ready, loadFn;
  const context = {
    document: {
      addEventListener: (_, fn) => { ready = fn; },
      getElementById: id => elements[id] || {textContent: ''},
      querySelector: () => ({
        addEventListener: (event, fn) => {
          if (event === 'click') loadFn = fn;
        },
      }),
    },
    fetch: () => Promise.resolve({
      ok: true,
      json: () => Promise.resolve({
        enablePg: true,
        pgPort: 5864,
        pgUser: 'grafana',
        pgBind: '172.17.0.1',
        passwordConfigured: true,
        tlsActive: false,
        autoCertPath: '/var/lib/signalk/lume-ti/pg_cert.pem',
        pgTlsCert: null,
      }),
    }),
  };
  vm.runInNewContext(fs.readFileSync(path.join(__dirname, '../public/pg.js'), 'utf8'), context);
  ready();
  await loadFn();
  assert.ok(elements['pg-tls-status'].textContent.includes('inactive'));
  assert.ok(elements['pg-tls-status'].textContent.includes('/var/lib/signalk/lume-ti/pg_cert.pem'));

  // And when TLS is active with custom cert
  context.fetch = () => Promise.resolve({
    ok: true,
    json: () => Promise.resolve({
      enablePg: true,
      pgPort: 5864,
      pgUser: 'grafana',
      pgBind: '192.168.1.100',
      passwordConfigured: true,
      tlsActive: true,
      autoCertPath: '/var/lib/signalk/lume-ti/pg_cert.pem',
      pgTlsCert: '/custom/cert.pem',
    }),
  });
  await loadFn();
  assert.ok(elements['pg-tls-status'].textContent.includes('active'));
  assert.ok(elements['pg-tls-status'].textContent.includes('/custom/cert.pem'));
});

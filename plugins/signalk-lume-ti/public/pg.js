'use strict';
document.addEventListener('DOMContentLoaded', () => {
  const form = document.getElementById('pg-config');
  const message = document.getElementById('pg-message');
  const endpoint = '/plugins/signalk-lume-ti/api/pg/config';
  async function load() {
    try {
      const response = await fetch(endpoint, {credentials: 'same-origin', cache: 'no-store'});
      if (!response.ok) throw new Error('Log in as a Signal K administrator to configure PostgreSQL.');
      const data = await response.json();
      for (const key of ['pgPort', 'pgUser', 'pgBind']) form.elements[key].value = data[key];
      form.elements.enablePg.checked = data.enablePg;
      message.textContent = data.passwordConfigured ? 'Password configured. Leave blank to keep it.' : 'Set a password before enabling PostgreSQL.';
      const tlsElement = document.getElementById('pg-tls-status');
      if (tlsElement) {
        const activeText = data.tlsActive ? 'TLS is active' : 'TLS is inactive';
        const certPath = data.pgTlsCert ? data.pgTlsCert : (data.autoCertPath || '<store>/pg_cert.pem');
        tlsElement.textContent = `${activeText}. Certificate: ${certPath}`;
      }
    } catch (error) { message.textContent = error.message; }
  }
  form.addEventListener('submit', async event => {
    event.preventDefault();
    const options = {
      enablePg: form.elements.enablePg.checked, pgPort: Number(form.elements.pgPort.value),
      pgUser: form.elements.pgUser.value, pgBind: form.elements.pgBind.value,
      pgPassword: form.elements.pgPassword.value,
    };
    // Clear the visible password immediately, including failed saves.
    form.elements.pgPassword.value = '';
    try {
      const request = fetch(endpoint, {method: 'POST', credentials: 'same-origin',
        headers: {'Content-Type': 'application/json'}, body: JSON.stringify(options)});
      options.pgPassword = '';
      const response = await request;
      if (!response.ok) throw new Error('Save failed. Check administrator login and settings, then enter the password again.');
      message.textContent = 'Saved. Lume is restarting.';
    } catch (error) { message.textContent = error.message; }
  });
  document.querySelector('[data-tab="tab-pg"]').addEventListener('click', load);
});

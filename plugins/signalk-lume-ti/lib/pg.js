'use strict';
const crypto = require('node:crypto');
const fs = require('node:fs');
const path = require('node:path');
const net = require('node:net');

const MAX_BODY = 4096;
function verifierValid(value) {
  if (typeof value !== 'string' || value.length > 512) return false;
  const match = /^SCRAM-SHA-256\$(4096):([A-Za-z0-9+/]+={0,2})\$([A-Za-z0-9+/]+={0,2}):([A-Za-z0-9+/]+={0,2})$/.exec(value);
  if (!match) return false;
  return [match[2], match[3], match[4]].every((s, i) => {
    const bytes = Buffer.from(s, 'base64');
    return bytes.toString('base64') === s && (i === 0 ? bytes.length >= 8 && bytes.length <= 64 : bytes.length === 32);
  });
}
async function deriveVerifier(password, salt = crypto.randomBytes(16)) {
  // Printable ASCII avoids a partial or divergent SASLprep implementation.
  if (typeof password !== 'string' || !/^[\x20-\x7e]{1,1024}$/.test(password)) {
    throw new Error('Password must contain 1–1024 printable ASCII characters');
  }
  const secret = Buffer.from(password, 'utf8');
  let salted;
  try {
    salted = await new Promise((resolve, reject) => crypto.pbkdf2(secret, salt, 4096, 32, 'sha256',
      (err, key) => err ? reject(err) : resolve(key)));
    const client = crypto.createHmac('sha256', salted).update('Client Key').digest();
    const stored = crypto.createHash('sha256').update(client).digest('base64');
    client.fill(0);
    const server = crypto.createHmac('sha256', salted).update('Server Key').digest('base64');
    return `SCRAM-SHA-256$4096:${salt.toString('base64')}$${stored}:${server}`;
  } finally {
    secret.fill(0);
    if (salted) salted.fill(0);
  }
}
function pgOptions(config = {}) {
  const options = {
    enablePg: config.enablePg === true,
    pgPort: config.pgPort ?? 5864,
    pgUser: config.pgUser ?? 'grafana',
    pgBind: config.pgBind || '127.0.0.1',
    pgVerifier: config.pgVerifier || '',
  };
  if (config.enablePg !== undefined && typeof config.enablePg !== 'boolean') throw new Error('enablePg must be boolean');
  if (!Number.isInteger(options.pgPort) || options.pgPort < 1 || options.pgPort > 65535) throw new Error('Invalid PostgreSQL port');
  if (typeof options.pgUser !== 'string' || !/^[A-Za-z0-9_.-]{1,128}$/.test(options.pgUser)) throw new Error('Invalid PostgreSQL user');
  if (typeof options.pgBind !== 'string' || !net.isIP(options.pgBind) || ['0.0.0.0', '::'].includes(options.pgBind)) {
    throw new Error('PostgreSQL bind must be a specific IP, e.g. 127.0.0.1 or 172.17.0.1 (docker0)');
  }
  if (options.pgVerifier && !verifierValid(options.pgVerifier)) throw new Error('Invalid SCRAM verifier');
  if (options.enablePg && !options.pgVerifier) throw new Error('Set a PostgreSQL password before enabling pgwire');
  return options;
}
function writePgConfig(dataDir, config) {
  const options = pgOptions(config);
  if (!options.enablePg) return null;
  // A separate auth-only config preserves store/ti.toml ingestion and query settings.
  fs.mkdirSync(dataDir, {recursive: true});
  const target = path.join(dataDir, 'ti.toml');
  const temporary = target + '.' + crypto.randomBytes(8).toString('hex') + '.new';
  const text = `[[auth.scram_users]]\nusername = '${options.pgUser}'\nverifier = '${options.pgVerifier}'\n`;
  try {
    const fd = fs.openSync(temporary, 'wx', 0o600);
    try { fs.fchmodSync(fd, 0o600); fs.writeFileSync(fd, text); }
    finally { fs.closeSync(fd); }
    fs.renameSync(temporary, target);
    fs.chmodSync(target, 0o600);
  } finally {
    fs.rmSync(temporary, {force: true});
  }
  return target;
}
function adminStatus(app, req) {
  const strategy = app.securityStrategy;
  if (strategy && typeof strategy.isDummy === 'function' && strategy.isDummy()) return 200;
  if (strategy && typeof strategy.allowConfigure === 'function') {
    return strategy.allowConfigure(req) ? 200 : req.skIsAuthenticated === true ? 403 : 401;
  }
  // Signal K 2.31's hasAdminAccess uses these authenticated server properties.
  if (req.skIsAuthenticated === true && req.skPrincipal?.permissions === 'admin') return 200;
  return req.skIsAuthenticated === true ? 403 : 401;
}
async function readJson(req) {
  if (!/^application\/json(?:\s*;|$)/i.test(req.headers?.['content-type'] || '')) {
    throw Object.assign(new Error('JSON content type required'), {status: 415});
  }
  const size = req.headers?.['content-length'];
  if (size !== undefined && (!/^\d+$/.test(String(size)) || Number(size) > MAX_BODY)) {
    throw Object.assign(new Error('Configuration body exceeds limit'), {status: 413});
  }
  let body = req.body;
  // If Signal K's global JSON parser consumed the stream, require a bounded
  // original Content-Length as serialized JSON cannot reveal whitespace bytes.
  if (body !== undefined && size === undefined) {
    throw Object.assign(new Error('Content-Length required for parsed configuration'), {status: 411});
  }
  if (body === undefined) {
    let bytes = 0;
    const chunks = [];
    for await (const chunk of req) {
      bytes += chunk.length;
      if (bytes > MAX_BODY) throw Object.assign(new Error('Configuration body exceeds limit'), {status: 413});
      chunks.push(chunk);
    }
    try { body = JSON.parse(Buffer.concat(chunks).toString('utf8')); }
    catch (_) { throw Object.assign(new Error('Invalid JSON'), {status: 400}); }
  }
  if (!body || typeof body !== 'object' || Array.isArray(body)) throw Object.assign(new Error('Configuration must be an object'), {status: 400});
  if (Buffer.byteLength(JSON.stringify(body)) > MAX_BODY) throw Object.assign(new Error('Configuration body exceeds limit'), {status: 413});
  return body;
}
function registerPgRoutes(router, app, getConfig, restart) {
  let saving = false;
  router.get('/api/pg/config', (req, res) => {
    const status = adminStatus(app, req);
    if (status !== 200) return res.status(status).json({error: 'Signal K administrator required'});
    const options = pgOptions(getConfig());
    const {pgVerifier, ...visible} = options;
    res.json({...visible, passwordConfigured: Boolean(pgVerifier), pgPassword: ''});
  });
  router.post('/api/pg/config', async (req, res) => {
    const status = adminStatus(app, req);
    if (status !== 200) return res.status(status).json({error: 'Signal K administrator required'});
    if (saving) return res.status(409).json({error: 'Configuration save already in progress'});
    saving = true;
    try {
      const body = await readJson(req);
      const allowed = new Set(['enablePg', 'pgPort', 'pgUser', 'pgBind', 'pgPassword']);
      if (Object.keys(body).some(key => !allowed.has(key))) throw new Error('Unknown PostgreSQL option');
      const password = body.pgPassword;
      body.pgPassword = '';
      const current = getConfig();
      let pgVerifier = current.pgVerifier || '';
      if (password !== undefined && password !== '') pgVerifier = await deriveVerifier(password);
      const options = pgOptions({...current, ...body, pgVerifier});
      const safe = {...current, ...options, pgPassword: ''};
      await new Promise((resolve, reject) => {
        if (typeof app.savePluginOptions !== 'function') return reject(new Error('Save unavailable'));
        app.savePluginOptions(safe, err => err ? reject(err) : resolve());
      });
      restart(safe);
      res.json({ok: true, pgPassword: '', passwordConfigured: Boolean(pgVerifier)});
    } catch (error) {
      // No body, password, verifier, or filesystem error text is reflected.
      res.status(error.status || 400).json({error: 'PostgreSQL configuration could not be saved'});
    } finally { saving = false; }
  });
}
module.exports = {deriveVerifier, verifierValid, pgOptions, writePgConfig, registerPgRoutes};

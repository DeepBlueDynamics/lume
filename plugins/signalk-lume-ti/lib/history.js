'use strict';
// Signal K server v2.31.0 packages/server-api/src/history.ts.
// The server owns HTTP parsing/security and passes Temporal/pathSpecs objects here.
const http = require('node:http');
const METHODS = {average: ['mean', 'avg'], min: ['min', 'min'], max: ['max', 'max'],
  first: ['last', 'first_value'], last: ['last', 'last_value']};
const literal = value => "'" + String(value).replace(/'/g, "''") + "'";
const identifier = value => '"' + String(value).replace(/"/g, '""') + '"';
const iso = ms => new Date(ms).toISOString();
function instant(value) {
  const text = String(value);
  if (!/T.*(?:Z|[+-]\d\d:\d\d)$/.test(text)) throw new Error('History timestamps require an ISO 8601 timezone');
  const ms = Date.parse(text);
  if (!Number.isFinite(ms)) throw new Error('Invalid history timestamp');
  return ms;
}
function durationMs(value, anchor, backwards = false) {
  if (value && typeof value.total === 'function') {
    const duration = backwards ? value.negated() : value;
    const result = duration.total({unit: 'milliseconds', relativeTo: iso(anchor) + '[UTC]'});
    if (!Number.isFinite(result) || (backwards ? result >= 0 : result <= 0)) throw new Error('Duration must be positive');
    return result;
  }
  let seconds;
  if (typeof value === 'number' || /^\d+$/.test(String(value))) seconds = Number(value);
  else {
    const m = /^P(?:(\d+(?:\.\d+)?)W)?(?:(\d+(?:\.\d+)?)D)?(?:T(?:(\d+(?:\.\d+)?)H)?(?:(\d+(?:\.\d+)?)M)?(?:(\d+(?:\.\d+)?)S)?)?$/.exec(String(value));
    if (!m || !m.slice(1).some(Boolean)) throw new Error('Unsupported duration; use seconds or ISO weeks/days/time');
    seconds = Number(m[1] || 0)*604800 + Number(m[2] || 0)*86400 + Number(m[3] || 0)*3600 + Number(m[4] || 0)*60 + Number(m[5] || 0);
  }
  if (!Number.isFinite(seconds) || seconds <= 0) throw new Error('Duration must be positive');
  return seconds*1000*(backwards ? -1 : 1);
}
function range(query, now = Date.now()) {
  if (query.from != null && query.to != null && query.duration != null) throw new Error('Choose two of from/to/duration');
  let from = query.from == null ? null : instant(query.from);
  let to = query.to == null ? null : instant(query.to);
  if (query.duration != null) {
    if (from != null) to = from + durationMs(query.duration, from);
    else { to = to == null ? now : to; from = to + durationMs(query.duration, to, true); }
  } else {
    if (from == null) throw new Error('from or duration is required');
    to = to == null ? now : to;
  }
  if (!Number.isFinite(from) || !Number.isFinite(to) || from >= to) throw new Error('from must precede to');
  return {from, to};
}
function requestJson(port, path, body) {
  return new Promise((resolve, reject) => {
    const payload = body == null ? null : Buffer.from(JSON.stringify(body));
    const req = http.request({hostname:'127.0.0.1', port, path, method:payload ? 'POST' : 'GET',
      headers:{Accept:'application/json', ...(payload ? {'Content-Type':'application/json', 'Content-Length':payload.length} : {})}},
    res => {
      let size = 0; const chunks = [];
      res.on('data', chunk => {size += chunk.length; if (size > 8*1024*1024) res.destroy(new Error('History backend response exceeds 8 MiB')); else chunks.push(chunk);});
      res.on('error', reject);
      res.on('end', () => {
        try { const reply = JSON.parse(Buffer.concat(chunks).toString('utf8'));
          if (res.statusCode !== 200) throw new Error(reply.error || 'Lume history backend unavailable');
          resolve(reply);
        } catch (error) {reject(error);}
      });
    });
    req.setTimeout(30000, () => req.destroy(new Error('Lume history backend timeout')));
    req.on('error', reject); if (payload) req.write(payload); req.end();
  });
}
function createHistoryProvider({app, port, request = (path, body) => requestJson(port, path, body), now = Date.now}) {
  let stopped = false;
  async function schema() {
    if (stopped) throw new Error('Lume history provider stopped');
    return request('/ti/schema');
  }
  async function query(sql) {
    if (stopped) throw new Error('Lume history provider stopped');
    const result = await request('/ti/query', {sql, max_rows:500});
    if (!Array.isArray(result.rows) || typeof result.truncated !== 'boolean') throw new Error('Invalid Lume history reply');
    return result;
  }
  function contexts(catalog) {return (catalog.time_coverage || []).map(c => c.vessel).filter(c => c.startsWith('vessels.'));}
  function resolveContext(value, catalog) {
    let context = value || 'vessels.self';
    if (context === 'vessels.self') {
      const self = typeof app.getSelfPath === 'function' ? app.getSelfPath('uuid') : null;
      context = self ? (self.startsWith('vessels.') ? self : 'vessels.' + self) : null;
      if (!context && contexts(catalog).length === 1) context = contexts(catalog)[0];
    }
    if (!context || !contexts(catalog).includes(context)) throw new Error('Unknown history vessel context');
    return context;
  }
  function selectField(catalog, path, method, resolution) {
    const tables = catalog.tables || [];
    const coarse = tables.find(t => t.name === 'telemetry');
    const hr = tables.find(t => t.name === 'telemetry_hr');
    const ordered = resolution < catalog.width_seconds && hr ? [hr, coarse] : [coarse, hr];
    for (const table of ordered.filter(Boolean)) {
      const names = new Set(table.columns.map(c => c.name));
      let column = path + '@' + METHODS[method][0];
      // One retained value per 1s HR bucket supports numeric roll-ups.
      if (!names.has(column) && table === hr && names.has(path + '@last')) column = path + '@last';
      if (names.has(column)) return {table:table.name, column};
    }
    throw new Error('No retained ' + METHODS[method][0] + ' column for ' + path);
  }
  async function samples(field, method, context, time, resolution) {
    const result = new Map();
    const binMs = resolution*1000;
    async function chunk(start, end) {
      const column = field.longitude ? "named_struct('latitude',"+identifier(field.column)+",'longitude',"+identifier(field.longitude)+")" : identifier(field.column);
      const present = identifier(field.column)+' IS NOT NULL'+(field.longitude ? ' AND '+identifier(field.longitude)+' IS NOT NULL' : '');
      const sql = 'SELECT date_bin(INTERVAL ' + literal(resolution + ' seconds') + ', ts, TIMESTAMP ' + literal(iso(time.from)) + ') AS history_ts, ' +
        METHODS[method][1] + '(' + column + (method === 'first' || method === 'last' ? ' ORDER BY ts' : '') + ') AS history_value FROM ' +
        identifier(field.table) + ' WHERE vessel = ' + literal(context) + ' AND ts >= TIMESTAMP ' + literal(iso(start)) +
        ' AND ts < TIMESTAMP ' + literal(iso(end)) + ' AND ' + present + ' GROUP BY 1 ORDER BY 1';
      const reply = await query(sql);
      if (reply.truncated) {
        const bins = Math.ceil((end-start)/binMs);
        if (bins <= 1) throw new Error('History bin exceeds Lume response cap; narrow paths/range');
        const middle = start + Math.floor(bins/2)*binMs;
        await chunk(start, middle); await chunk(middle, end); return;
      }
      for (const row of reply.rows) result.set(iso(instant(row.history_ts)), row.history_value ?? null);
    }
    for (let start = time.from; start < time.to; start += 400*binMs) await chunk(start, Math.min(time.to, start + 400*binMs));
    return result;
  }
  const provider = {
    async getValues(input) {
      const time = range(input, now()); const catalog = await schema();
      const context = resolveContext(input.context, catalog);
      const resolution = input.resolution == null ? catalog.width_seconds : Number(input.resolution);
      if (!Number.isFinite(resolution) || resolution < 1 || resolution*1000 !== Math.floor(resolution*1000)) throw new Error('resolution must be >= 1 second with millisecond precision');
      if (Math.ceil((time.to-time.from)/(resolution*1000)) > 100000) throw new Error('History request exceeds 100000 bins; increase resolution');
      if (!Array.isArray(input.pathSpecs) || input.pathSpecs.length < 1 || input.pathSpecs.length > 16) throw new Error('Request 1..16 history paths');
      const series = [];
      for (const spec of input.pathSpecs) {
        if (!Object.hasOwn(METHODS, spec.aggregate) || spec.parameter?.length) throw new Error('Unsupported history aggregate');
        if (spec.sourceRef) throw new Error('Per-source historical values are not retained independently');
        if (spec.path === 'navigation.position') {
          if (!['first','last'].includes(spec.aggregate)) throw new Error('Position supports first/last');
          const lat = selectField(catalog, spec.path+'.latitude', spec.aggregate,resolution);
          const lon = selectField(catalog, spec.path+'.longitude', spec.aggregate,resolution);
          if (lat.table !== lon.table) throw new Error('Position coordinates require the same retained store');
          series.push(await samples({...lat,longitude:lon.column},spec.aggregate,context,time,resolution));
        } else series.push(await samples(selectField(catalog,spec.path,spec.aggregate,resolution),spec.aggregate,context,time,resolution));
      }
      const timestamps = [...new Set(series.flatMap(s => [...s.keys()]))].sort();
      return {context, range:{from:iso(time.from),to:iso(time.to)},
        values:input.pathSpecs.map(s => ({path:s.path,method:s.aggregate})),
        data:timestamps.map(ts => [ts,...series.map(s => s.get(ts) ?? null)])};
    },
    async getContexts(input) {
      const time = range(input, now()); const catalog = await schema(); const found = new Set();
      for (const table of (catalog.tables || []).filter(t => ['telemetry','telemetry_hr'].includes(t.name))) {
        const reply = await query('SELECT DISTINCT vessel AS history_context FROM ' + identifier(table.name) +
          ' WHERE ts >= TIMESTAMP ' + literal(iso(time.from)) + ' AND ts < TIMESTAMP ' + literal(iso(time.to)));
        if (reply.truncated) throw new Error('History context list exceeds backend cap');
        for (const row of reply.rows) if (row.history_context?.startsWith('vessels.')) found.add(row.history_context);
      }
      return [...found].sort();
    },
    async getPaths(input) {
      const time = range(input, now()); const catalog = await schema(); const found = new Set();
      for (const table of (catalog.tables || []).filter(t => ['telemetry','telemetry_hr'].includes(t.name))) {
        const fields = [...new Set(table.columns.map(c => c.name).filter(n => /@(mean|min|max|last)$/.test(n)))];
        for (let i=0; i<fields.length; i+=32) {
          const names = fields.slice(i,i+32);
          const reply = await query('SELECT ' + names.map((c,j) => 'count('+identifier(c)+') AS p'+j).join(',') +
            ' FROM '+identifier(table.name)+' WHERE ts >= TIMESTAMP '+literal(iso(time.from))+' AND ts < TIMESTAMP '+literal(iso(time.to)));
          if (reply.truncated) throw new Error('History path list exceeds backend cap');
          names.forEach((name,j) => {if (Number(reply.rows[0]?.['p'+j]) > 0) found.add(name.replace(/@(mean|min|max|last)$/,''));});
        }
      }
      if (found.has('navigation.position.latitude') && found.has('navigation.position.longitude')) found.add('navigation.position');
      return [...found].sort();
    }
  };
  return {provider, stop(){stopped=true;}};
}
module.exports = {createHistoryProvider, requestJson, range};

'use strict';
(function (root, factory) {
  const api = factory();
  if (typeof module === 'object' && module.exports) module.exports = api;
  else root.LumeChart = api;
})(typeof globalThis !== 'undefined' ? globalThis : this, function () {
  const base = '/signalk/v2/api/resources';
  const group = 'lume-ti';
  function uuid() {
    const bytes = new Uint8Array(16);
    globalThis.crypto.getRandomValues(bytes);
    bytes[6] = (bytes[6] & 15) | 64; bytes[8] = (bytes[8] & 63) | 128;
    const s = Array.from(bytes, b => b.toString(16).padStart(2, '0')).join('');
    return [s.slice(0,8),s.slice(8,12),s.slice(12,16),s.slice(16,20),s.slice(20)].join('-');
  }
  function timestamp(value) {
    if (value === null || value === undefined || value === '') throw new Error('Missing interval timestamp.');
    const date = new Date(typeof value === 'number' ? value * 1000 : value);
    if (!Number.isFinite(date.getTime())) throw new Error('Invalid interval timestamp.');
    return date.toISOString();
  }
  function position(row, fallback) {
    const lat = row.latitude ?? row['navigation.position.latitude@last'] ?? fallback?.latitude;
    const lon = row.longitude ?? row['navigation.position.longitude@last'] ?? fallback?.longitude;
    if (lat === undefined && lon === undefined) return null;
    if (typeof lat !== 'number' || typeof lon !== 'number' || !Number.isFinite(lat) || !Number.isFinite(lon) ||
        lat < -90 || lat > 90 || lon < -180 || lon > 180) throw new Error('Supply a valid latitude and longitude.');
    return { latitude: lat, longitude: lon };
  }
  function inferBBox(sql) {
    if (!/\bin_bbox\s*\(/i.test(sql)) return null;
    const calls = [...sql.matchAll(/\bin_bbox\s*\(([^()]*)\)/gi)];
    if (calls.length !== 1) throw new Error('Pin supports one literal in_bbox; select a single bounding box.');
    const parts = calls[0][1].split(',').map(x => x.trim());
    if (parts.length !== 4 || parts.some(x => !/^[+-]?(?:\d+(?:\.\d*)?|\.\d+)(?:e[+-]?\d+)?$/i.test(x))) {
      throw new Error('Pin requires four literal in_bbox coordinates.');
    }
    return parts.map(Number);
  }
  function polygon(bbox) {
    const [south,west,north,east] = bbox;
    if (!bbox.every(Number.isFinite) || south < -90 || north > 90 || south >= north ||
        west < -180 || west > 180 || east < -180 || east > 180 || west === east) throw new Error('Invalid bounding box.');
    const ring = (w,e) => [[w,south],[e,south],[e,north],[w,north],[w,south]];
    return west < east ? { type:'Polygon', coordinates:[ring(west,east)] } :
      { type:'MultiPolygon', coordinates:[[ring(west,180)],[ring(-180,east)]] };
  }
  async function request(fetcher, path, method, body) {
    const response = await fetcher(path, { method, credentials:'same-origin',
      headers:{'Content-Type':'application/json'}, ...(body ? {body:JSON.stringify(body)} : {}) });
    if (!response.ok) throw new Error(`Resources ${method} failed: HTTP ${response.status}`);
    if (response.status === 204) return {};
    const text = await response.text();
    const data = text ? JSON.parse(text) : {};
    if (data.state && data.state !== 'COMPLETED') throw new Error('Resources operation did not complete.');
    if (data.statusCode && data.statusCode >= 400) throw new Error(`Resources operation failed: ${data.statusCode}`);
    return data;
  }
  // Invoked only by the explicit webapp button. It never submits vessel/data writes.
  async function pin({sql,rows,truncated=false,position:anchor,fetcher=globalThis.fetch,idFactory=uuid}) {
    if (truncated) throw new Error('Narrow the query before pinning a truncated result.');
    if (!Array.isArray(rows) || !rows.length || rows.length > 500) throw new Error('Pin needs 1–500 result rows.');
    const bbox = inferBBox(sql);
    const geometry = bbox ? polygon(bbox) : null;
    const resources = [];
    for (const row of rows) {
      const start = timestamp(row.start ?? row.ts_start ?? row.ts);
      const endValue = row.end ?? row.ts_end;
      const end = endValue == null ? null : timestamp(endValue);
      if (end && end <= start) throw new Error('Interval end must follow start.');
      if (!bbox && !end) throw new Error('Select intervals() with start/end, or an in_bbox result with ts.');
      const point = position(row,anchor);
      const regionId = geometry ? idFactory() : null;
      const title = `Lume TI: ${row.vessel ?? row.entity ?? 'interval'} ${start}`;
      const description = `${start}${end ? ' – '+end : ''}\n${sql}`;
      const metadata = {group, 'x-lume-ti':{start,end,sql}};
      if (geometry) resources.push({type:'regions',id:regionId,data:{name:title,description,feature:{
        type:'Feature',geometry,properties:{...metadata,name:title,description}}}});
      resources.push({type:'notes',id:idFactory(),data:{title,description,mimeType:'text/plain',properties:metadata,
        ...(regionId ? {href:`/resources/regions/${regionId}`} : {}), ...(point ? {position:point} : {})}});
    }
    const written = [];
    try {
      for (const resource of resources) {
        await request(fetcher,`${base}/${resource.type}/${encodeURIComponent(resource.id)}`,'PUT',resource.data);
        written.push(resource);
      }
    } catch (error) {
      error.message += ` (${written.length} resources written; use Unpin all lume-ti to remove them.)`;
      throw error;
    }
    return {notes:written.filter(r=>r.type==='notes').length,regions:written.filter(r=>r.type==='regions').length};
  }
  async function unpin({fetcher=globalThis.fetch}={}) {
    // Fetch complete collections, then check ownership locally; provider group filters may be ignored.
    const selected = [];
    for (const type of ['notes','regions']) {
      const data = await request(fetcher,`${base}/${type}`,'GET');
      if (!data || Array.isArray(data) || typeof data !== 'object') throw new Error('Invalid Resources collection.');
      for (const [id,value] of Object.entries(data)) {
        const props = type === 'regions' ? value.feature?.properties : value.properties;
        if (props?.group === group && props?.['x-lume-ti']) selected.push({type,id});
      }
    }
    // Delete notes before linked regions.
    let deleted = 0;
    for (const resource of selected) {
      await request(fetcher,`${base}/${resource.type}/${encodeURIComponent(resource.id)}`,'DELETE'); deleted++;
    }
    return {deleted};
  }
  return {pin,unpin,inferBBox};
});

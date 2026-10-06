'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const http = require('node:http');
const {createHistoryProvider, requestJson, range} = require('../lib/history');
const context = 'vessels.urn:test:http';
const from = '2020-01-01T00:00:00Z', to = '2020-01-01T00:00:30Z';
const names = ['speed@mean','speed@min','speed@max','speed@last','navigation.position.latitude@last','navigation.position.longitude@last'];
const catalog = {width_seconds:10,time_coverage:[{vessel:context}],tables:[{name:'telemetry',columns:names.map(name=>({name}))}]};
const spec = (path, aggregate='average') => ({path,aggregate,parameter:[]});
const input = (...pathSpecs) => ({from,to,resolution:30,pathSpecs});
function backend(responder) {
  const calls = [];
  const request = async (path,body) => {calls.push({path,body}); return path === '/ti/schema' ? catalog : responder(body.sql);};
  return {calls, ...createHistoryProvider({app:{getSelfPath:()=>context.slice(8)},request,now:()=>Date.parse(to)})};
}
test('Signal K range forms, seconds, ISO durations and parsed Temporal duration', () => {
  const start = Date.parse(from), end = Date.parse(to);
  for (const request of [{from,to},{from,duration:30},{to,duration:'PT30S'},{duration:'30'}]) assert.deepEqual(range(request,end),{from:start,to:end});
  assert.deepEqual(range({from},end),{from:start,to:end});
  let relative;
  const temporal = {total: options => {relative=options.relativeTo;return 30000;}};
  assert.deepEqual(range({from:{toString:()=>from},duration:temporal}),{from:start,to:end});
  assert.ok(relative.endsWith('[UTC]'));
  const backwards = {total:()=>30000,negated:()=>({total:()=>-30000})};
  assert.deepEqual(range({to,duration:backwards}),{from:start,to:end});
  for (const request of [{to},{from,to,duration:30},{from:to,to:from},{from,duration:0},{from:'2020-01-01T00:00:00',to}]) assert.throws(()=>range(request,end));
});
test('five methods map to retained aggregates and ordered rollups; exact envelope/null alignment', async () => {
  const expected = {average:['mean','avg'],min:['min','min'],max:['max','max'],first:['last','first_value'],last:['last','last_value']};
  for (const [method,[agg,fn]] of Object.entries(expected)) {
    const b=backend(sql=>{
      assert.ok(sql.includes(fn+'("speed@'+agg+'"'));
      if (method==='first'||method==='last') assert.ok(sql.includes(' ORDER BY ts)'));
      assert.ok(sql.includes("INTERVAL '30 seconds'"));
      assert.ok(sql.includes("vessel = '"+context+"'"));
      return {rows:[{history_ts:from,history_value:7}],truncated:false};
    });
    assert.deepEqual(await b.provider.getValues(input(spec('speed',method))),{context,range:{from:new Date(from).toISOString(),to:new Date(to).toISOString()},values:[{path:'speed',method}],data:[[new Date(from).toISOString(),7]]});
  }
  let count=0;
  const middle='2020-01-01T00:00:20Z';
  const b=backend(()=>({rows:[{history_ts:count++ ? middle : from,history_value:2}],truncated:false}));
  assert.deepEqual((await b.provider.getValues(input(spec('speed'),spec('speed','min')))).data,[[new Date(from).toISOString(),2,null],[new Date(middle).toISOString(),null,2]]);
});
test('position coordinates are aggregated as one paired struct', async () => {
  const b=backend(sql=>{
    assert.ok(sql.includes("first_value(named_struct('latitude',"));
    assert.ok(sql.includes('"navigation.position.longitude@last" IS NOT NULL'));
    return {rows:[{history_ts:from,history_value:{latitude:60,longitude:24}}],truncated:false};
  });
  const reply=await b.provider.getValues(input(spec('navigation.position','first')));
  assert.deepEqual(reply.data,[[new Date(from).toISOString(),{latitude:60,longitude:24}]]);
});
test('HR @last rollups selected only when configured, narrow resolution; missing first/last rejected', async () => {
  const calls=[];
  const hr={...catalog,tables:[...catalog.tables,{name:'telemetry_hr',columns:[{name:'speed@last'}]}]};
  const b=createHistoryProvider({app:{},request:async (path,body)=>{if(path==='/ti/schema')return hr; calls.push(body.sql);return {rows:[],truncated:false};}});
  await b.provider.getValues({...input(spec('speed')),context,resolution:1});
  assert.ok(calls[0].includes('FROM "telemetry_hr"')); assert.ok(calls[0].includes('avg("speed@last")'));
  const noLast={...catalog,tables:[{name:'telemetry',columns:[{name:'speed@mean'}]}]};
  const a=createHistoryProvider({app:{},request:async ()=>noLast});
  await assert.rejects(a.provider.getValues({...input(spec('speed','last')),context}),/No retained last/);
});
test('truncation splits at bin boundaries; caps, unsupported methods/sources and stop reject', async () => {
  let calls=0;
  const b=backend(()=>({rows:[],truncated:++calls===1}));
  await b.provider.getValues({...input(spec('speed')),to:'2020-01-01T00:01:00Z'});
  assert.equal(calls,3);
  const stuck=backend(()=>({rows:[],truncated:true}));
  await assert.rejects(stuck.provider.getValues(input(spec('speed'))),/response cap/);
  for(const request of [{...input(spec('speed','ema'))},{...input({...spec('speed'),sourceRef:'device'})},{...input(spec('speed')),resolution:0},{...input(spec('speed')),context:"vessels.unknown' OR true"}]) await assert.rejects(b.provider.getValues(request));
  b.stop();await assert.rejects(b.provider.getValues(input(spec('speed'))),/stopped/);
});
test('contexts and paths query actual non-null coverage; SQL quoting preserves identifiers', async () => {
  const b=backend(sql => ({rows:sql.includes('DISTINCT') ? [{history_context:context},{history_context:'robots.urn:r'}] : [Object.fromEntries(names.map((_,i)=>['p'+i,1]))],truncated:false}));
  assert.deepEqual(await b.provider.getContexts({from,to}),[context]);
  const paths=await b.provider.getPaths({from,to});
  assert.ok(paths.includes('navigation.position'));assert.ok(paths.includes('speed'));
});
test('catalog identifiers and vessel literals cannot alter SQL', async () => {
  const path='custom."quoted';
  const vessel="vessels.urn:test:o'brien";
  let sql;
  const c={...catalog,time_coverage:[{vessel}],tables:[{name:'telemetry',columns:[{name:path+'@mean'}]}]};
  const b=createHistoryProvider({app:{},request:async (route,body)=>{
    if(route==='/ti/schema')return c;
    sql=body.sql;return {rows:[],truncated:false};
  }});
  await b.provider.getValues({...input(spec(path)),context:vessel});
  assert.ok(sql.includes('avg("custom.""quoted@mean")'));
  assert.ok(sql.includes("vessel = 'vessels.urn:test:o''brien'"));
});
test('real loopback mock validates JSON transport and backend errors', async t => {
  const server=http.createServer((req,res)=>{
    if(req.url==='/ti/schema')return res.end(JSON.stringify(catalog));
    if(req.url==='/failure'){res.statusCode=400;return res.end(JSON.stringify({error:'bad history SQL'}));}
    let body='';req.on('data',c=>body+=c);req.on('end',()=>{
      assert.equal(req.headers.accept,'application/json');
      const q=JSON.parse(body);assert.equal(q.max_rows,500);assert.ok(q.sql.startsWith('SELECT'));
      res.end(JSON.stringify({rows:[{history_ts:from,history_value:3}],truncated:false}));
    });
  });
  await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve));
  t.after(()=>new Promise(resolve=>server.close(resolve)));
  await assert.rejects(requestJson(server.address().port,'/failure'),/bad history SQL/);
  const b=createHistoryProvider({app:{},port:server.address().port});
  assert.equal((await b.provider.getValues({...input(spec('speed')),context})).data[0][1],3);
});

'use strict';
const {test} = require('node:test');
const assert = require('node:assert/strict');
const http = require('node:http');
const {pin,unpin} = require('../public/pin');
const resourceId = n => `00000000-0000-4000-8000-${String(n).padStart(12,'0')}`;
async function stub(t) {
  const data = {notes:{ordinary:{title:'Keep',properties:{group:'other'}},'same-group-unowned':{properties:{group:'lume-ti'}}},regions:{}};
  const calls=[];
  const server=http.createServer(async(req,res)=>{
    let body='';for await(const chunk of req)body+=chunk;
    calls.push({method:req.method,path:req.url});
    const match=/^\/signalk\/v2\/api\/resources\/(notes|regions)(?:\/([^/]+))?$/.exec(req.url);
    if(!match){res.writeHead(400);res.end('{}');return;}
    const [,type,id]=match;
    if(req.method==='GET')res.end(JSON.stringify(data[type]));
    else if(req.method==='PUT'){
      assert.match(decodeURIComponent(id),/^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i);
      const value=JSON.parse(body);
      if(type==='notes'){
        assert.equal(value.region,undefined);
        assert.equal(value.properties.group,'lume-ti');
        if(value.href)assert.match(value.href,/^\/resources\/regions\/[0-9a-f-]{36}$/i);
      }
      data[type][decodeURIComponent(id)]=value;res.end('{"state":"COMPLETED"}');}
    else if(req.method==='DELETE'){delete data[type][decodeURIComponent(id)];res.writeHead(204);res.end();}
    else {res.writeHead(400);res.end('{}');}
  });
  await new Promise(resolve=>server.listen(0,'0.0.0.0',resolve));
  t.after(()=>new Promise(resolve=>{server.close(resolve);server.closeAllConnections();}));
  const url='http://127.0.0.1:'+server.address().port;
  return {data,calls,fetcher:(path,opts)=>fetch(url+path,opts)};
}
test('interval pin, bbox region and unpin affect only owned resources',async(t)=>{
  const s=await stub(t);let id=0;const idFactory=()=> resourceId(++id);
  const rows=[{vessel:'boat',start:'2026-10-06T12:00:00Z',end:'2026-10-06T12:02:00Z'}];
  assert.deepEqual(await pin({sql:"SELECT * FROM intervals('speed > 2')",rows,position:{latitude:50,longitude:4},fetcher:s.fetcher,idFactory}),{notes:1,regions:0});
  assert.deepEqual(s.data.notes[resourceId(1)].position,{latitude:50,longitude:4});
  assert.equal(s.data.notes[resourceId(1)].properties['x-lume-ti'].end,'2026-10-06T12:02:00.000Z');
  await pin({sql:'SELECT ts FROM telemetry WHERE in_bbox(50,4,51,5)',rows:[{ts:'2026-10-06T12:00:00Z'}],fetcher:s.fetcher,idFactory});
  assert.deepEqual(s.data.regions[resourceId(2)].feature.geometry.coordinates[0],[[4,50],[5,50],[5,51],[4,51],[4,50]]);
  assert.equal(s.data.notes[resourceId(3)].href,`/resources/regions/${resourceId(2)}`);
  assert.deepEqual(await unpin({fetcher:s.fetcher}),{deleted:3});
  assert.deepEqual(Object.keys(s.data.notes),['ordinary','same-group-unowned']);
  assert.deepEqual(await unpin({fetcher:s.fetcher}),{deleted:0});
  assert(s.calls.every(c=>c.path.startsWith('/signalk/v2/api/resources/')));
});
test('invalid/truncated results produce no writes; dateline regions split',async()=>{
  let calls=0;const fetcher=async()=>{calls++;throw new Error('unexpected');};
  for(const input of [
    {sql:'SELECT * FROM intervals()',rows:[{start:'bad',end:'bad'}]},
    {sql:'SELECT * FROM intervals()',rows:[{start:1780000000,end:1780000100},{start:'bad',end:'bad'}]},
    {sql:'SELECT * FROM intervals()',rows:[{start:2,end:1}]},
    {sql:'SELECT * FROM intervals()',rows:[{start:1,end:2}],truncated:true},
    {sql:'SELECT in_bbox(a,b,c,d)',rows:[{ts:1}]}
  ])await assert.rejects(pin({...input,fetcher}));
  assert.equal(calls,0);
  const writes=[];
  await pin({sql:'SELECT ts WHERE in_bbox(-10,170,10,-170)',rows:[{ts:1780000000}],
    idFactory:()=> 'id',fetcher:async(path,opts)=>{writes.push(JSON.parse(opts.body));return {ok:true,status:204};}});
  assert.equal(writes[0].feature.geometry.type,'MultiPolygon');
});
test('failed resource write reports partial state; no automatic retry',async()=>{
  let calls=0;
  await assert.rejects(pin({sql:'SELECT ts WHERE in_bbox(50,4,51,5)',rows:[{ts:1780000000}],idFactory:()=> 'id',
    fetcher:async()=> ++calls===1?{ok:true,status:204}:{ok:false,status:403}}),/1 resources written/);
  assert.equal(calls,2);
});

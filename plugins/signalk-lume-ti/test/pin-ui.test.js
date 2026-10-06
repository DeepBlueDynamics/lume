'use strict';
const {test}=require('node:test');
const assert=require('node:assert/strict');
const fs=require('node:fs');
const path=require('node:path');
const vm=require('node:vm');
test('webapp never pins on startup or query; only buttons invoke resource actions',async()=>{
  const elements=new Map();const actions=[];
  const element=id=>{
    if(!elements.has(id))elements.set(id,{value:'',disabled:false,textContent:'',innerHTML:'',
      classList:{add(){},remove(){}},handlers:{},addEventListener(event,fn){this.handlers[event]=fn;},focus(){}});
    return elements.get(id);
  };
  let ready;
  const document={addEventListener(event,fn){if(event==='DOMContentLoaded')ready=fn;},
    querySelectorAll(){return [];},getElementById:element};
  const sql="SELECT * FROM intervals('speed > 2')";
  const rows=[{start:'2026-10-06T12:00:00Z',end:'2026-10-06T12:01:00Z'}];
  const context={document,window:{location:{pathname:'/signalk-lume-ti/index.html'},confirm:()=>true,
    LumeChart:{pin:async input=>{actions.push(['pin',input]);return {notes:1,regions:0};},
      unpin:async()=>{actions.push(['unpin']);return {deleted:1};}}},
    setInterval(){},performance:{now:()=>1},
    fetch:async url=>{assert(!url.includes('index.html/api'));return {ok:true,json:async()=>url.endsWith('/api/query')?{rows,truncated:false}:{}};}};
  vm.runInNewContext(fs.readFileSync(path.join(__dirname,'../public/app.js'),'utf8'),context);
  ready();await new Promise(setImmediate);
  assert.equal(actions.length,0);
  element('sql-input').value=sql;
  await element('run-btn').handlers.click();
  assert.equal(actions.length,0);
  element('sql-input').value='SELECT another query'; // Pin uses the SQL that actually produced these rows.
  await element('pin-chart-btn').handlers.click();
  assert.equal(actions.length,1);assert.equal(actions[0][1].sql,sql);
  element('unpin-chart-btn').handlers.click();await new Promise(setImmediate);
  assert.equal(actions[1][0],'unpin');
});

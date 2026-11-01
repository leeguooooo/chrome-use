const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const expression = fs.readFileSync(0, 'utf8');
const bw = 'b'.repeat(32), other = 'c'.repeat(32);
const dynamic = '12345678-1234-4234-8234-123456789abc';
const fixture = 'http://127.0.0.1:7800/login?fixture';
const menu = origin => `chrome-extension://${origin}/overlay/menu-list.html`;

async function observe({owner=true, urlsOrigin=dynamic, frames=[], contexts=[], tabs}={}) {
  const calls = [];
  const chrome = {
    runtime: {id:owner?bw:other, getURL:path=>`chrome-extension://${urlsOrigin}/${path}`,
      getContexts:async filter=>{calls.push(['contexts',filter]);return contexts;}},
    tabs:{query:async()=>tabs||[{id:7,windowId:9,active:true,url:fixture}]},
    webNavigation:{getAllFrames:async filter=>{calls.push(['frames',filter]);return frames;}},
  };
  const result = await vm.runInNewContext(expression,{chrome,URL});
  return {result:JSON.parse(JSON.stringify(result)),calls};
}

(async()=>{
  let seen=await observe({frames:[{frameId:0,url:fixture},{frameId:2,url:menu(dynamic)},{frameId:3,url:menu(other)}]});
  assert.equal(seen.result.bitwardenFrames.length,1,'dynamic menu origin comes from the owner runtime');
  assert.equal(seen.result.bitwardenFrames[0].frameId,2);

  seen=await observe({urlsOrigin:bw,contexts:[
    {tabId:7,frameId:4,documentId:'owned',documentUrl:menu(dynamic)},
    {tabId:8,frameId:5,documentId:'wrong-tab',documentUrl:menu(dynamic)},
  ]});
  assert.equal(seen.result.bitwardenFrames.length,1,'owner context must belong to the exact fixture tab');
  assert.equal(seen.result.bitwardenFrames[0].frameId,4);

  seen=await observe({frames:[{frameId:6,url:menu(other)}],contexts:[
    {tabId:7,frameId:7,documentUrl:`chrome-extension://${bw}/popup/index.html`},
    {tabId:7,frameId:8,documentUrl:'https://fixture.invalid/overlay/menu-list.html'},
  ]});
  assert.equal(seen.result.bitwardenFrames.length,0,'unrelated frames and non-menu owner pages are not a menu');

  seen=await observe({owner:false,frames:[{frameId:9,url:menu(dynamic)}],contexts:[{tabId:7,frameId:9,documentUrl:menu(dynamic)}]});
  assert.equal(seen.result.bitwardenFrames.length,0,'a foreign observer cannot assert owner context identity');
  assert.equal(seen.calls.some(c=>c[0]==='contexts'),false);

  await assert.rejects(observe({tabs:[{id:7,url:fixture},{id:8,url:fixture}]}),/not unique/);
  await assert.rejects(observe({tabs:[{id:7,url:'https://wrong.invalid'}]}),/not unique/);
  console.log('6 actual-controller menu identity checks passed');
})().catch(error=>{console.error(error);process.exitCode=1;});

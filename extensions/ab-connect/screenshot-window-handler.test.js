import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';
import vm from 'node:vm';
import { prepareScreenshotWindow } from './screenshot-window.js';

const source=readFileSync(new URL('./background.js',import.meta.url),'utf8');
const start=source.indexOf("  if (method === 'ABExt.prepareScreenshot') {");
const end=source.indexOf("  if (method === 'ABExt.inspectTab') {",start);
assert.ok(start>=0&&end>start);
function fixture({known=true,owned=false,denied=false}={}) {
  const calls=[];let state='minimized';
  const context={prepareScreenshotWindow,calls,
    childSessionToTab:new Map([['child',7]]),sessionToTab:new Map(),
    tabs:new Map(known?[[7,{attached:false}]]:[]),ownedTabs:new Set(owned?[7]:[]),agentPopups:new Set(),
    loadOwnedTabs:async()=>{},loadAgentPopups:async()=>{},
    resolveSessionTab:sid=>sid==='cb-tab-7'?7:null,
    tabGrants:{get:()=>null},guardTab:()=>{if(denied)throw new Error('taken back')},
    withRelayTimeout:promise=>promise,
    chrome:{runtime:{getPlatformInfo:callback=>{calls.push('platform');callback({os:'win'})}},
      tabs:{get:async id=>{calls.push(['tab',id]);return {id,windowId:9}}},
      windows:{get:async id=>{calls.push(['window',id]);return {id,state,focused:false}},
        update:async(id,params)=>{calls.push(['update',id,params]);state=params.state;}}},
  };
  vm.createContext(context);
  vm.runInContext("async function invoke(sessionId,params,agentSession='owner'){const method='ABExt.prepareScreenshot';\n"+source.slice(start,end)+'\n}',context);
  return context;
}

for(const session of [undefined,'child','cb-tab-99']) {
  test(`window preparation refuses unbound session ${session}`,async()=>{
    const f=fixture();
    await assert.rejects(f.invoke(session,{allowWindowRestore:true}),/screenshot_window_unverified/);
    assert.deepEqual(f.calls,[]);
  });
}

test('persisted ownership can prepare a soft-detached target without reattaching it',async()=>{
  const f=fixture({known:false,owned:true});
  await assert.rejects(f.invoke('cb-tab-7',{allowWindowRestore:false}),/screenshot_window_minimized/);
  assert.equal(f.calls.some(c=>Array.isArray(c)&&c[0]==='update'),false);
});

test('a taken-back target refuses before any browser metadata or update',async()=>{
  const f=fixture({denied:true});
  await assert.rejects(f.invoke('cb-tab-7',{allowWindowRestore:true}),/taken back/);
  assert.deepEqual(f.calls,[]);
});

test('window preparation rejects injected target parameters and nonboolean permission',async()=>{
  for(const params of [{windowId:99,allowWindowRestore:true},{allowWindowRestore:'true'},[]]) {
    const f=fixture();
    await assert.rejects(f.invoke('cb-tab-7',params),/boolean allowWindowRestore/);
    assert.deepEqual(f.calls,[]);
  }
});

test('the actual handler derives the only restored window from its bound tab',async()=>{
  const f=fixture();
  const result=await f.invoke('cb-tab-7',{allowWindowRestore:true});
  assert.equal(result.restored,true);assert.equal(result.windowId,9);
  assert.deepEqual(f.calls.filter(c=>Array.isArray(c)&&c[0]==='update'),[['update',9,{state:'normal',focused:false}]]);
});

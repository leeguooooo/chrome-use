const assert = require('node:assert/strict');
const vm = require('node:vm');
const tick = () => new Promise(resolve => setImmediate(resolve));
(async () => {
  const {sendTabCommand} = await import(process.env.PROBE_COMMAND_MODULE);
  async function setup({blocked=false,retry=false,pendingDOM=false,owned=true,url='http://127.0.0.1:12345/report?case'}={}) {
    const emitted=[], calls=[], timers=[]; let onDetach, releaseHealth, resolveCapture, resolveDOM;
    const health = blocked ? new Promise(resolve=>{releaseHealth=resolve}) : Promise.resolve(false);
    let captureCalls=0, ids=0;
    const c = {performance:{now:()=>10}, crypto:{randomUUID:()=>String(++ids)}, URL, AbortController,
      ownedTabs:new Set(owned?[7,42]:[]), cursorSettings:{}, stealthBlockReason:()=>null,
      observeDebuggerCommand:(_,promise)=>promise, sendTabCommand,
      setTimeout:fn=>{timers.push(fn);return timers.length}, clearTimeout(){},
      fetch:async (url,options)=>{if(url.endsWith('/trace')) emitted.push(JSON.parse(options.body));
        return {json:async()=>({valid:true,snapshot:{}})};},
      chrome:{tabs:{get:async()=>({url})},debugger:{
        onDetach:{addListener:fn=>{onDetach=fn}}, attach:async()=>({}),detach:async()=>({}),
        sendCommand:(target,method)=>{
          calls.push({target,method});
          if(method==='Runtime.evaluate') {assert.equal('sessionId' in target,false);return pendingDOM ? new Promise(resolve=>{resolveDOM=resolve}) : Promise.resolve({result:{value:{rows:123,ready:'complete'}}});}
          if(retry&&++captureCalls===1)return Promise.reject(new Error('Target closed'));
          return new Promise(resolve=>{resolveCapture=resolve});
        },
      }},
    };
    vm.createContext(c);vm.runInContext(process.env.PROBE_HELPER,c);
    c.tabCommandDependencies=context=>({
      health:{recover:()=>health},sendCommand:(target,method,args)=>c.fixtureSendCommand(context,target,method,args),
      detachTab(){},recoverSessionTab:async()=>{await c.fixtureDebuggerAttach({tabId:42},'1.3');return 42},
    });
    vm.runInContext(process.env.PROBE_SENDER,c);
    await c.fixtureDebuggerAttach({tabId:7},'1.3');
    return {c,emitted,calls,timers,releaseHealth:()=>releaseHealth(false),resolve:()=>resolveCapture({data:'fixture'}),resolveDOM:()=>resolveDOM({result:{value:{rows:123,ready:'complete'}}}),onDetach};
  }
  const waiting=await setup({blocked:true});
  const pending=waiting.c.sendCdpToTab(7,'Page.captureScreenshot',{},undefined);
  await tick();assert.equal(waiting.calls.length,0);
  assert.equal(waiting.emitted.some(e=>e.type==='capture-native-send'),false,'health promise is not a native dispatch');
  waiting.releaseHealth();await tick();assert.equal(waiting.calls[0].method,'Page.captureScreenshot');
  assert.equal(waiting.emitted.filter(e=>e.type==='capture-native-send').length,1);
  const domEnabled = process.env.PROBE_EXPECT_DOM !== 'false';
  if(domEnabled) await waiting.timers[1]?.();
  await tick();
  assert.equal(waiting.calls.filter(e=>e.method==='Runtime.evaluate').length,domEnabled?1:0);
  waiting.resolve();await pending;
  const end=waiting.emitted.find(e=>e.type==='capture-wrapper-answer');
  assert.equal(end.attempts.length,1);assert.deepEqual(Array.from(end.invalidations),[]);

  const recovered=await setup({blocked:true});
  const old=recovered.c.sendCdpToTab(7,'Page.captureScreenshot',{},undefined);await tick();
  await recovered.c.fixtureDebuggerDetach({tabId:7});await recovered.c.fixtureDebuggerAttach({tabId:7},'1.3');
  recovered.releaseHealth();await tick();recovered.resolve();await old;
  assert.ok(recovered.emitted.find(e=>e.type==='capture-wrapper-answer').invalidations.length);

  const retry=await setup({retry:true});
  const again=retry.c.sendCdpToTab(7,'Page.captureScreenshot',{},undefined);await tick();await tick();
  retry.resolve();await again;
  const retried=retry.emitted.find(e=>e.type==='capture-wrapper-answer');
  assert.equal(retried.attempts.length,2);assert.equal(retried.attempts[1].target.tabId,42);
  assert.ok(retried.invalidations.includes('actual-target-changed'));

  if(domEnabled) {
    const changed=await setup({pendingDOM:true});
    const work=changed.c.sendCdpToTab(7,'Page.captureScreenshot',{},undefined);await tick();
    const read=changed.timers[1]();await tick();changed.onDetach({tabId:7});changed.resolveDOM();await read;
    changed.resolve();await work;
    const answer=changed.emitted.find(e=>e.type==='dom-native-answer');
    const start=changed.emitted.find(e=>e.type==='capture-native-send');
    assert.notEqual(answer.epoch,start.epoch);
    assert.ok(changed.emitted.find(e=>e.type==='capture-wrapper-answer').invalidations.includes('onDetach'));
  }

  for(const options of [{owned:false},{url:'http://127.0.0.1:12345/report?other'},{url:'http://127.0.0.1:12345.evil.test/report?case'}]) {
    const outside=await setup(options);const work=outside.c.sendCdpToTab(7,'Page.captureScreenshot',{},undefined);
    await tick();outside.resolve();await work;assert.equal(outside.emitted.length,0);
  }
})().catch(error=>{console.error(error);process.exitCode=1});

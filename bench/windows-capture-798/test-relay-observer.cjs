const assert = require('node:assert/strict');
const vm = require('node:vm');
const tick = () => new Promise(resolve => setImmediate(resolve));
(async () => {
  const {sendTabCommand} = await import(process.env.PROBE_COMMAND_MODULE);
  async function setup({blocked=false,retry=false,pendingDOM=false,owned=true,url='http://127.0.0.1:12345/report?case'}={}) {
    const emitted=[], calls=[], timers=[]; let onDetach, releaseHealth, resolveCapture, resolveDOM;
    const health = blocked ? new Promise(resolve=>{releaseHealth=resolve}) : Promise.resolve(false);
    let captureCalls=0, ids=0, traceFailure=false, holdTrace=false, releaseTrace;
    const c = {performance:{now:()=>10}, crypto:{randomUUID:()=>String(++ids)}, URL, AbortController,
      tabForTarget:()=>7, ownedTabs:new Set(owned?[7,42]:[]), cursorSettings:{}, stealthBlockReason:()=>null,
      observeDebuggerCommand:(_,promise)=>promise, sendTabCommand,
      setTimeout:(fn,ms)=>{timers.push({fn,ms});return timers.length}, clearTimeout(){},
      fetch:async (url,options)=>{if(url.endsWith('/trace')) {
        emitted.push(JSON.parse(options.body));
        if(holdTrace){holdTrace=false;await new Promise(resolve=>{releaseTrace=resolve});}
        return {ok:!traceFailure,json:async()=>({})};
        }
        return {ok:true,json:async()=>({valid:true,snapshot:{},saved:true})};},
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
    return {c,emitted,calls,timers,setTraceFailure:value=>{traceFailure=value},holdTrace:()=>{holdTrace=true},releaseTrace:()=>releaseTrace(),releaseHealth:()=>releaseHealth(false),resolve:()=>resolveCapture({data:'fixture'}),resolveDOM:()=>resolveDOM({result:{value:{rows:123,ready:'complete'}}}),onDetach};
  }
  const waiting=await setup({blocked:true});
  const pending=waiting.c.sendCdpToTab(7,'Page.captureScreenshot',{},undefined);
  await tick();assert.equal(waiting.calls.length,0);
  assert.equal(waiting.emitted.some(e=>e.type==='capture-native-send'),false,'health promise is not a native dispatch');
  waiting.releaseHealth();await tick();assert.equal(waiting.calls[0].method,'Page.captureScreenshot');
  assert.equal(waiting.emitted.filter(e=>e.type==='capture-native-send').length,1);
  const domEnabled = process.env.PROBE_EXPECT_DOM !== 'false';
  if(domEnabled) await waiting.timers.find(t=>t.ms===2000)?.fn();
  await tick();
  assert.equal(waiting.calls.filter(e=>e.method==='Runtime.evaluate').length,domEnabled?1:0);
  waiting.resolve();await pending;
  const end=waiting.emitted.find(e=>e.type==='capture-wrapper-answer');
  assert.equal(end.attempts.length,1);assert.deepEqual(Array.from(end.invalidations),[]);
  waiting.onDetach({tabId:7},'canceled_by_user');
  await tick();
  assert.ok(waiting.emitted.some(e=>e.type==='attachment-change'&&e.reason==='onDetach:canceled_by_user'),
    'detach after wrapper settlement must still be attributed');
  assert.ok(waiting.emitted.some(e=>e.type==='page-after-send'));
  const healthCalls=[];
  const recovery=Promise.resolve(false);
  const health=waiting.c.fixtureHealth({mark(id){healthCalls.push(id)},recover(){return recovery}});
  health.mark(7);
  assert.equal(health.recover(7),recovery,'health instrumentation must preserve promise identity');
  await tick();
  assert.deepEqual(healthCalls,[7]);
  assert.ok(waiting.emitted.some(e=>e.type==='health-mark-call'));
  assert.ok(waiting.emitted.some(e=>e.type==='health-recover-answer'&&e.recovered===false));
  await waiting.c.fixtureBeforeTeardown('owned-target');
  const fence=waiting.emitted.find(e=>e.type==='worker-teardown-start');
  assert.equal(fence.traceComplete,true);
  assert.ok(fence.operations.includes(waiting.emitted.find(e=>e.type==='capture-operation-start').operationId));
  const incomplete=await setup();
  const incompleteContext=await incomplete.c.fixtureCaptureContext(7);
  incomplete.setTraceFailure(true);
  await incomplete.c.fixtureEmit(incompleteContext,{type:'fixture-test'});
  incomplete.setTraceFailure(false);
  await incomplete.c.fixtureBeforeTeardown('owned-target');
  assert.equal(incomplete.emitted.find(e=>e.type==='worker-teardown-start').traceComplete,false);

  const held=await setup();
  const heldContext=await held.c.fixtureCaptureContext(7);
  held.holdTrace();
  const pendingTrace=held.c.fixtureEmit(heldContext,{type:'fixture-test'});
  const draining=held.c.fixtureBeforeTeardown('owned-target');
  await tick();
  assert.equal(held.emitted.some(e=>e.type==='worker-teardown-start'),false,'fence waits for earlier trace acknowledgments');
  held.releaseTrace();await pendingTrace;await draining;
  assert.equal(held.emitted.find(e=>e.type==='worker-teardown-start').traceComplete,true);

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
    const read=changed.timers.find(t=>t.ms===2000).fn();await tick();changed.onDetach({tabId:7});changed.resolveDOM();await read;
    changed.resolve();await work;
    const answer=changed.emitted.find(e=>e.type==='dom-native-answer');
    const start=changed.emitted.find(e=>e.type==='capture-native-send');
    assert.notEqual(answer.epoch,start.epoch);
    assert.ok(changed.emitted.find(e=>e.type==='capture-wrapper-answer').invalidations.includes('onDetach:undefined'));
  }

  for(const options of [{owned:false},{url:'http://127.0.0.1:12345/report?other'},{url:'http://127.0.0.1:12345.evil.test/report?case'}]) {
    const outside=await setup(options);const work=outside.c.sendCdpToTab(7,'Page.captureScreenshot',{},undefined);
    await tick();outside.resolve();await work;assert.equal(outside.emitted.length,0);
  }
})().catch(error=>{console.error(error);process.exitCode=1});

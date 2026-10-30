const assert = require('node:assert/strict');
const vm = require('node:vm');
(async()=>{
  const events=[],calls=[],timers=[];
  let failFocus=false, pendingFocus=false, blockOwnership=false, releaseOwnership;
  let clock=0;
  const context={performance:{now:()=>++clock},crypto:require('node:crypto').webcrypto,URL,AbortController,
    ownedTabs:new Set([7]),setTimeout:(fn,ms)=>{timers.push({fn,ms});return timers.length},clearTimeout(){},
    fetch:async(url,options)=>{if(url.endsWith('/trace'))events.push(JSON.parse(options.body));
      return {ok:true,json:async()=>({valid:true,snapshot:{},saved:true})};},
    chrome:{tabs:{get:async()=>{if(blockOwnership){blockOwnership=false;return new Promise(resolve=>{releaseOwnership=resolve});}return {url:'http://127.0.0.1:12345/report?case'};}},debugger:{
      onDetach:{addListener(){}},attach:async()=>{},detach:async()=>{},
      sendCommand:async(target,command,args)=>{
        calls.push({target,command,args});
        if(command==='Emulation.setFocusEmulationEnabled') {
          if(failFocus)throw new Error('focus emulation unavailable');
          if(pendingFocus)return new Promise(()=>{});
          return {};
        }
        blockOwnership=true;return {data:'synthetic'};
      },
    }},
  };
  vm.createContext(context);vm.runInContext(process.env.PROBE_HELPER,context);
  await context.fixtureDebuggerAttach({tabId:7},'1.3');
  const operation=await context.fixtureCaptureContext(7);
  const clip={x:0,y:0,width:1265,height:32288,scale:1};
  await context.fixtureSendCommand(operation,{tabId:7},'Page.captureScreenshot',{fromSurface:true,clip,captureBeyondViewport:true});
  assert.equal(operation.attempts[0].nativePending,false,'native settlement must precede asynchronous ownership proof');
  assert.equal(events.some(e=>e.type==='capture-native-answer'),false);
  releaseOwnership({url:'http://127.0.0.1:12345/report?case'});
  await new Promise(resolve=>setImmediate(resolve));
  const answer=events.find(e=>e.type==='capture-native-answer');
  assert.ok(answer.nativeReceivedMs<=answer.ownershipVerifiedMs && answer.ownershipVerifiedMs<=answer.atMs);
  const capture=calls.filter(c=>c.command==='Page.captureScreenshot');
  assert.equal(capture.length,1);
  assert.equal(capture[0].args.fromSurface,process.env.PROBE_CANDIDATE!=='view');
  assert.equal(capture[0].args.clip,clip);
  const focus=calls.filter(c=>c.command==='Emulation.setFocusEmulationEnabled');
  assert.equal(focus.length,process.env.PROBE_CANDIDATE==='focus'?1:0);
  if(focus.length) {
    assert.equal(focus[0].args.enabled,true);
    assert.equal(calls[0].command,'Emulation.setFocusEmulationEnabled');
    for(const mode of ['error','pending']) {
      calls.length=0;failFocus=mode==='error';pendingFocus=mode==='pending';
      const blocked=await context.fixtureCaptureContext(7);
      const attempt=context.fixtureSendCommand(blocked,{tabId:7},'Page.captureScreenshot',{});
      if(pendingFocus){await new Promise(resolve=>setImmediate(resolve));timers.findLast(t=>t.ms===2000).fn();}
      await assert.rejects(attempt,/focus/);
      assert.equal(calls.filter(c=>c.command==='Page.captureScreenshot').length,0,'unacknowledged focus control must stop before capture');
    }
  }
})().catch(error=>{console.error(error);process.exitCode=1});

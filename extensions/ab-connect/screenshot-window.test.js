import assert from 'node:assert/strict';
import test from 'node:test';
import { prepareScreenshotWindow } from './screenshot-window.js';

function fixture({state='minimized',os='win'}={}) {
  const calls=[];
  let actual=state, tabReads=0, denied=false;
  const deps={
    guard(){if(denied)throw new Error('taken back');},
    async getPlatformInfo(){return {os};},
    async getTab(id){tabReads++;calls.push(['tab',id]);return {id,windowId:9};},
    async getWindow(id){calls.push(['window',id]);return {id,state:actual,focused:false};},
    async updateWindow(id,params){calls.push(['update',id,params]);actual=params.state;},
  };
  return {deps,calls,deny:()=>{denied=true},get tabReads(){return tabReads}};
}

test('minimized Windows screenshot refuses without requesting any window update',async()=>{
  const f=fixture();
  await assert.rejects(prepareScreenshotWindow(7,false,f.deps),/screenshot_window_minimized:.*no capture was sent/);
  assert.equal(f.calls.some(c=>c[0]==='update'),false);
});

test('explicit restoration changes only the bound window once and verifies its state',async()=>{
  const f=fixture();
  const result=await prepareScreenshotWindow(7,true,f.deps);
  assert.equal(result.restored,true);assert.equal(result.windowId,9);assert.equal(result.state,'normal');
  assert.deepEqual(f.calls.filter(c=>c[0]==='update'),[['update',9,{state:'normal',focused:false}]]);
  const again=await prepareScreenshotWindow(7,true,f.deps);
  assert.equal(again.restored,false);assert.equal(f.calls.filter(c=>c[0]==='update').length,1);
});

test('normal windows are observed without an update',async()=>{
  const f=fixture({state:'normal'});
  assert.equal((await prepareScreenshotWindow(7,false,f.deps)).restored,false);
  assert.equal(f.calls.some(c=>c[0]==='update'),false);
});

test('non-Windows defaults remain unchanged and explicit unsupported restoration refuses',async()=>{
  const f=fixture({os:'mac'});
  assert.deepEqual(await prepareScreenshotWindow(7,false,f.deps),{platform:'mac',applicable:false,restored:false});
  await assert.rejects(prepareScreenshotWindow(7,true,f.deps),/Windows relay only/);
  assert.deepEqual(f.calls,[]);
});

test('ownership and window identity are checked again before an update',async()=>{
  const f=fixture();const get=f.deps.getWindow;
  f.deps.getWindow=async id=>{const result=await get(id);f.deny();return result;};
  await assert.rejects(prepareScreenshotWindow(7,true,f.deps),/taken back/);
  assert.equal(f.calls.some(c=>c[0]==='update'),false);
  const moved=fixture();const read=moved.deps.getTab;
  moved.deps.getTab=async id=>({...await read(id),windowId:moved.tabReads>1?10:9});
  await assert.rejects(prepareScreenshotWindow(7,true,moved.deps),/target moved before/);
  assert.equal(moved.calls.some(c=>c[0]==='update'),false);
});

for(const outcome of ['unchanged','rejected','moved','revoked']) {
  test(`restoration ${outcome} is unverified and is never replayed`,async()=>{
    const f=fixture();let updates=0;
    f.deps.updateWindow=async()=>{updates++;if(outcome==='rejected')throw new Error('no reply');if(outcome==='revoked')f.deny();};
    if(outcome==='moved')f.deps.getTab=async id=>({id,windowId:updates?10:9});
    await assert.rejects(prepareScreenshotWindow(7,true,f.deps),/action_outcome_unknown:/);
    assert.equal(updates,1);
  });
}


test('unknown metadata and nonboolean authority never request a restoration',async()=>{
  for(const state of ['unknown',undefined]) {
    const f=fixture({state});
    if(state===undefined)f.deps.getWindow=async id=>({id});
    await assert.rejects(prepareScreenshotWindow(7,true,f.deps),/unverified/);
    assert.equal(f.calls.some(c=>c[0]==='update'),false);
  }
  const f=fixture();
  await assert.rejects(prepareScreenshotWindow(7,'true',f.deps),/permission must be boolean/);
  assert.deepEqual(f.calls,[]);
});

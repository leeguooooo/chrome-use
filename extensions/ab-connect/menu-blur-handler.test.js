import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';
import vm from 'node:vm';
import { blurFocusedField } from './menu-blur.js';

const source = readFileSync(new URL('./background.js', import.meta.url), 'utf8');
const start = source.indexOf("  if (method === 'ABExt.blurFocusedField') {");
const end = source.indexOf("  if (method === 'ABExt.prepareScreenshot') {", start);
assert.ok(start >= 0 && end > start);
function fixture(revokeDuringRead = false) {
  const calls = [];
  let revoked = !revokeDuringRead;
  const context = {
    calls, blurFocusedField, ownedTabs: new Set([7, 8]),
    loadOwnedTabs: async () => {}, sessionToTab: new Map(), childSessionToTab: new Map(),
    resolveSessionTab: sid => sid === 'inner' ? 8 : 7,
    tabForTarget: () => 7,
    guardTab: id => { calls.push(['guard', id]); if (id === 8 && revoked) throw new Error('taken back'); },
    cursorSettings: { ab_stealth: true }, isSiteAccessError: () => false,
    chrome: {
      webNavigation: { getAllFrames: async () => { calls.push(['frames']); revoked = true; return [{frameId: 0, url: 'https://fixture.invalid'}]; } },
      scripting: { executeScript: async () => { calls.push(['script']); return [{result: {blurred: true}}]; } },
    },
  };
  vm.createContext(context);
  vm.runInContext("async function invoke(params){const method='ABExt.blurFocusedField',sessionId='outer',agentSession='agent';\n" + source.slice(start, end) + '\n}', context);
  return context;
}

test('legacy nested session cannot blur a taken-back target that differs from the outer session', async () => {
  const f = fixture();
  await assert.rejects(f.invoke({sessionId: 'inner', targetId: 'outer-target'}), /taken back/);
  assert.equal(f.calls.some(c => c[0] === 'script'), false);
  assert.equal(f.calls.some(c => c[0] === 'frames'), false);
});

test('permission taken back while reading frames refuses before injection', async () => {
  const f = fixture(true);
  await assert.rejects(f.invoke({sessionId: 'inner'}), /taken back/);
  assert.equal(f.calls.some(c => c[0] === 'script'), false);
});


test('an authorized agent tab still blurs in stealth mode without a visibility nudge', async () => {
  const f = fixture();
  const result = await f.invoke({});
  assert.equal(result.blurred, 1);
  assert.equal(result.nudged, 0);
  assert.equal(result.stealth, true);
  assert.equal(f.calls.filter(c => c[0] === 'script').length, 1);
});

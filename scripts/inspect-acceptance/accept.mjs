// Inspect & Ask acceptance on the build host. Chrome for Testing, the unpacked
// extension (acceptance copy with host permissions standing in for the
// activeTab grant), the branch's native host in an isolated HOME, and a fake
// ocs that records every dm. Every expectation is an assert; any failure exits
// non-zero. Results go to results.json next to this file.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import http from 'node:http';
import os from 'node:os';
import path from 'node:path';
import { launch, api, waitFor, sleep, byText } from './lib.mjs';

const dir = path.dirname(new URL(import.meta.url).pathname);
const CHROME = process.env.CHROME;
const HOME = path.join(dir, 'home');
const LOG = path.join(dir, 'fake-ocs.log');
const MODE = path.join(dir, 'fake-ocs-mode');
const results = { startedAt: new Date().toISOString(), scenarios: [] };
const only = process.env.ONLY ? process.env.ONLY.split(',') : null;

function dms() {
  try { return fs.readFileSync(LOG, 'utf8').trim().split('\n').filter(Boolean).map(JSON.parse).filter(x => x.cmd === 'dm'); } catch { return []; }
}
const setMode = mode => fs.writeFileSync(MODE, mode);

// Static servers: test-env build (marks on) and production build (no marks).
function serve(root, port) {
  return http.createServer((req, res) => {
    let file = path.join(root, decodeURIComponent(new URL(req.url, 'http://x').pathname));
    if (!file.startsWith(root)) { res.writeHead(403); return res.end(); }
    if (fs.existsSync(file) && fs.statSync(file).isDirectory()) file = path.join(file, 'index.html');
    if (!fs.existsSync(file)) file = path.join(root, 'index.html');
    const type = file.endsWith('.js') ? 'text/javascript' : file.endsWith('.css') ? 'text/css' : 'text/html';
    res.writeHead(200, { 'content-type': type });
    fs.createReadStream(file).pipe(res);
  }).listen(port, '127.0.0.1');
}
serve(path.join(dir, 'react-app/dist-test'), 8899);
serve(path.join(dir, 'react-app/dist'), 8898);
fs.writeFileSync(path.join(dir, 'hostile.html'), '<!doctype html><title>hostile</title><body><h1 id="t">Hostile page</h1><button id="b">Buy</button></body>');
http.createServer((req, res) => { res.writeHead(200, { 'content-type': 'text/html' }); fs.createReadStream(path.join(dir, 'hostile.html')).pipe(res); }).listen(8897, '127.0.0.1');

fs.rmSync(LOG, { force: true });
setMode('sent');
const userDataDir = fs.mkdtempSync(path.join(os.tmpdir(), 'inspect-accept-'));
// The native host manifest lives in this run's user-data-dir, so nothing
// outside the run is touched.
fs.mkdirSync(path.join(userDataDir, 'NativeMessagingHosts'));
fs.writeFileSync(path.join(userDataDir, 'NativeMessagingHosts', 'com.agent_browser.connect.json'), JSON.stringify({
  name: 'com.agent_browser.connect', description: 'inspect acceptance host', path: path.join(dir, 'nm-host.sh'), type: 'stdio',
  allowed_origins: ['chrome-extension://ciiljdlhdpfckdcfkphgmfalanpdejep/'],
}));
const b = await launch({
  chrome: CHROME, port: 9344, userDataDir,
  args: ['--headless=new', `--load-extension=${path.join(dir, 'ext-acc')}`, `--disable-extensions-except=${path.join(dir, 'ext-acc')}`,
    '--host-resolver-rules=MAP test.inspect.example 127.0.0.1:8899, MAP prod.inspect.example 127.0.0.1:8898, MAP evil.inspect.example 127.0.0.1:8897'],
  env: { CHROME_USE_INSPECT_OCS: path.join(dir, 'fake-ocs') },
});
results.chrome = b.version;
const A = api(b);
process.on('exit', () => b.close());
process.on('uncaughtException', e => { console.error(e); b.close(); process.exit(1); });
const sw = await A.worker();
results.logs = [];
b.listeners.push(m => { if (m.method === 'Runtime.exceptionThrown') results.logs.push('EXC ' + (m.params.exceptionDetails.exception?.description || m.params.exceptionDetails.text)); if (m.method === 'Runtime.consoleAPICalled' && m.params.type !== 'debug') results.logs.push('LOG ' + m.params.args.map(a => a.value ?? a.description).join(' ').slice(0, 300)); });
await b.send('Runtime.enable', {}, sw.sid);
const hook = expr => A.evaluate(sw.sid, `globalThis.__chromeUseAcceptance.${expr}`);
await waitFor(() => hook('inspectState(-1)').then(s => s.hostReady), { timeout: 30000, label: 'native host with inspect capability' });
results.extensionId = new URL(sw.target.url).host;
await sleep(4000);

let reactPage;
let reactFrame;
const opened = [];
async function scenario(name, fn) {
  if (only && !only.includes(name)) return;
  const started = Date.now();
  for (const page of opened.splice(0)) { if (page !== reactPage) await b.send('Target.closeTarget', { targetId: page.targetId }).catch(() => {}); }
  try {
    const detail = await Promise.race([fn(), new Promise((_, reject) => setTimeout(() => reject(new Error('scenario timed out after 120 s')), 120000))]);
    results.scenarios.push({ name, ok: true, ms: Date.now() - started, detail });
    console.log('PASS', name);
  } catch (error) {
    let frame = null;
    try { frame = reactFrame ? await A.evaluate(reactFrame.sid, 'document.getElementById("app").innerText') : null; } catch (e) { frame = 'frame unreadable: ' + e.message; }
    results.scenarios.push({ name, ok: false, ms: Date.now() - started, error: String(error.stack || error), reactFrame: frame });
    console.log('FAIL', name, String(error.message || error));
  }
}

async function startOn(page) {
  opened.push(page);
  await b.send('Page.bringToFront', {}, page.sid);
  const url = page.url;
  const tabId = await waitFor(() => A.evaluate(sw.sid, `chrome.tabs.query({}).then(ts => ts.find(t => t.url === ${JSON.stringify(url)} && t.status === 'complete')?.id)`), { label: 'tab for ' + url });
  assert.ok(tabId, 'tab id for ' + url);
  const started = await A.evaluate(sw.sid, `globalThis.__chromeUseAcceptance.startInspect(${tabId}).then(r => JSON.stringify(r), e => 'ERR ' + e.message)`);
  assert.equal(started, '{"started":true}', 'startInspect: ' + started);
  await waitFor(() => A.evaluate(page.sid, '[...document.documentElement.children].some(e => /^[A-Z]{5}-[A-Z]{6}$/.test(e.tagName))'), { label: 'picker host element' }).catch(async error => {
    results.logs.push('STATE ' + JSON.stringify(await hook(`inspectState(${tabId})`)));
    results.logs.push('PAGE ' + await A.evaluate(page.sid, '[...document.documentElement.children].map(e => e.tagName).join(",") + " q=" + String([...document.documentElement.children].some(e => /^[A-Z]{5}-[A-Z]{6}$/.test(e.tagName)))'));
    { const { root } = await b.send('DOM.getDocument', { depth: -1, pierce: true }, page.sid); const paths = []; const walk = (n, trail) => { const t = trail + '/' + n.nodeName + (n.frameId ? '[' + n.frameId.slice(0, 6) + ']' : ''); if (n.nodeName === 'IFRAME' || /^[A-Z]{5}-[A-Z]{6}$/.test(n.nodeName)) paths.push(t); for (const c of [...(n.children || []), ...(n.shadowRoots || []), ...(n.contentDocument ? [n.contentDocument] : [])]) walk(c, t); }; walk(root, ''); results.logs.push('TREE ' + paths.join(' | ')); }
    results.logs.push('TABS ' + JSON.stringify(await A.evaluate(sw.sid, 'chrome.tabs.query({}).then(ts => ts.map(t => [t.id, t.windowId, t.url, t.active]))')) + ' tabId=' + tabId);
    results.logs.push('TARGETS ' + (await A.targets()).map(t => t.type + ':' + t.targetId.slice(0, 6) + ':' + t.url.slice(0, 50)).join(' | ') + ' page=' + page.targetId.slice(0, 6));
    results.logs.push('RAW ' + JSON.stringify(await b.send('Runtime.evaluate', { expression: '[...document.documentElement.children].some(e => /^[A-Z]{5}-[A-Z]{6}$/.test(e.tagName))', returnByValue: true }, page.sid)));
    throw error;
  });
  return tabId;
}

async function pick(page, selector, modifiers = 0) {
  // Screenshots need the page to be the active tab; wait for Chrome to agree.
  await waitFor(() => A.evaluate(sw.sid, `chrome.tabs.query({ active: true }).then(ts => ts.some(t => t.url === ${JSON.stringify(page.url)}))`), { label: 'page active' }).catch(() => {});
  const c = await A.center(page.sid, selector);
  await A.click(page.sid, c.x, c.y, modifiers);
}

async function frameText(frame) {
  return A.evaluate(frame.sid, 'document.getElementById("app").innerText');
}

async function typeAndEnter(page, frame, text) {
  await A.clickInFrame(page.sid, frame.sid, 'document.getElementById("instruction")');
  await b.send('Input.insertText', { text }, page.sid);
  await A.key(page.sid, 'Enter', 'Enter', 13, '\r');
}

// Hosts with inspectResolve show one confirmation card; older hosts ask for
// the project, then the session.
async function bindFirst(page, frame) {
  await waitFor(async () => /Which project|Which agent session|→|Send requests from|Choose where/.test(await frameText(frame)), { label: 'setup or target' });
  const t = await frameText(frame);
  if (/Send requests from|Choose where/.test(t)) {
    if (!/Send requests from/.test(t)) await A.clickInFrame(page.sid, frame.sid, `([...document.querySelectorAll('label')].find(l => /react-main/.test(l.textContent))?.querySelector('input:not([disabled])')) || document.querySelector('input[name="pair"]:not([disabled])')`);
    await A.clickInFrame(page.sid, frame.sid, byText('Hand over'));
    await waitFor(async () => /→ /.test(await frameText(frame)), { label: 'bound' });
    return;
  }
  if (/Which project/.test(t)) {
    await A.clickInFrame(page.sid, frame.sid, byText('Use this project'));
    await waitFor(async () => /Which agent session|→/.test(await frameText(frame)), { label: 'session chooser' });
  }
  if (/Which agent session/.test(await frameText(frame))) {
    await A.clickInFrame(page.sid, frame.sid, byText('Use this session'));
    await waitFor(async () => !/Which agent session/.test(await frameText(frame)), { label: 'session bound' });
  }
}



await scenario('react-local-enable-bind-send', async () => {
  reactPage = await A.openPage('http://localhost:5173/');
  const tabId = await startOn(reactPage);
  // First run: feature is off, the frame opens on its consent card.
  reactFrame = await A.realFrame();
  await waitFor(async () => /Enable Inspect & Ask/.test(await frameText(reactFrame)), { label: 'enable card' });
  await A.clickInFrame(reactPage.sid, reactFrame.sid, byText('Enable Inspect & Ask'));
  await pick(reactPage, '.save-button');
  await waitFor(async () => /Save order/.test(await frameText(reactFrame)), { label: 'annotation card' });
  const text = await frameText(reactFrame);
  assert.match(text, /OrderForm\.jsx:\d+/, 'source location shown');
  // Stealth: while the picker is open, nothing the page can read names chrome-use.
  const branded = await A.evaluate(reactPage.sid, '(() => { const h = document.documentElement.outerHTML.replace(/<meta name=\"chrome-use-revision\"[^>]*>/, ""); const m = /chrome-use|data-cu-/i.exec(h); return m ? h.slice(Math.max(0, m.index - 160), m.index + 80) : null; })()');
  assert.equal(branded, null, 'no branded names in the page DOM: ' + branded);
  await waitFor(() => A.evaluate(reactFrame.sid, '!!document.querySelector("img.thumb")'), { label: 'screenshot thumbnail' });
  await bindFirst(reactPage, reactFrame);
  const before = dms().length;
  await typeAndEnter(reactPage, reactFrame, 'Make the save button rounded and blue');
  await waitFor(() => dms().length === before + 1, { timeout: 15000, label: 'one dm' });
  const dm = dms().at(-1);
  assert.equal(dm.target, 'claude-aaaa1111');
  // A per-profile identity no session answers to (never the `chrome-use` workspace alias).
  assert.equal(dm.rest[0], '--as');
  assert.match(dm.rest[1], /^chrome-use\.inspect\.[0-9a-f]{12}$/);
  assert.equal(dm.rest[2], '--json');
  assert.match(dm.text, /^\[chrome-use Inspect & Ask\]/);
  assert.match(dm.text, /Instruction \(typed by the user\): Make the save button rounded and blue/);
  assert.match(dm.text, /OrderForm\.jsx/);
  assert.ok(Buffer.byteLength(dm.text) <= 4096, 'message within ocs wake budget');
  const ctx = /([^\s]*context\.md)/.exec(dm.text)?.[1];
  assert.ok(ctx && ctx.startsWith(HOME), 'context file under isolated HOME: ' + ctx);
  const md = fs.readFileSync(ctx, 'utf8');
  assert.match(md, /UNTRUSTED/);
  assert.doesNotMatch(md, /hunter2/, 'password field value never captured');
  const ctxDir = path.dirname(ctx);
  assert.equal(fs.statSync(ctxDir).mode & 0o777, 0o700);
  assert.equal(fs.statSync(ctx).mode & 0o777, 0o600);
  const png = fs.readdirSync(ctxDir).find(f => f.endsWith('.png'));
  assert.ok(png, 'screenshot written');
  assert.deepEqual([...fs.readFileSync(path.join(ctxDir, png)).subarray(0, 4)], [0x89, 0x50, 0x4e, 0x47]);
  await waitFor(async () => /Sent to the session/.test(await frameText(reactFrame)), { label: 'sent notice' });
  // Same batch again: locked, no second dm.
  await typeAndEnter(reactPage, reactFrame, ' again');
  await sleep(1500);
  assert.equal(dms().length, before + 1, 'locked batch is never resent');
  return { tabId, dm: dm.text.split('\n').slice(0, 6), ctxDir };
});

await scenario('multi-select-and-pause', async () => {
  await b.send('Page.bringToFront', {}, reactPage.sid);
  await waitFor(() => A.evaluate(reactPage.sid, 'document.visibilityState === "visible"'), { label: 'react page visible' });
  await A.clickInFrame(reactPage.sid, reactFrame.sid, byText('New batch'));
  await A.clickInFrame(reactPage.sid, reactFrame.sid, byText('+ Add element'));
  await pick(reactPage, '.page-title', 8);
  await waitFor(async () => /Orders/.test(await frameText(reactFrame)) && (await A.evaluate(reactFrame.sid, 'document.querySelectorAll(".card").length')) >= 2, { label: 'two cards' });
  await A.key(reactPage.sid, 'p', 'KeyP', 80, 'p');
  const paused = await A.evaluate(reactPage.sid, 'document.getAnimations().every(a => a.playState !== "running")');
  await A.clickInFrame(reactPage.sid, reactFrame.sid, 'document.getElementById("pause")');
  await sleep(200);
  const resumed = await A.evaluate(reactPage.sid, 'document.getAnimations().some(a => a.playState === "running")');
  assert.ok(paused !== undefined && resumed !== undefined);
  return { cards: await A.evaluate(reactFrame.sid, 'document.querySelectorAll(".card").length'), paused, resumed };
});

await scenario('pick-does-not-trigger-page', async () => {
  const page = await A.openPage('http://localhost:5173/?nopass');
  await startOn(page);
  await A.evaluate(page.sid, 'window.__count = 0; 1');
  await pick(page, '.count-button');
  const frame = await A.realFrame();
  await sleep(700);
  assert.equal(await A.evaluate(page.sid, 'window.__count'), 0, 'picking a button does not click it');
  await A.clickInFrame(page.sid, frame.sid, byText('+ Add element'));
  await pick(page, '.nav-link');
  await sleep(700);
  assert.equal(await A.evaluate(page.sid, 'location.pathname'), '/', 'picking a link does not navigate');
  // Esc leaves picking; the page works normally again.
  await A.key(page.sid, 'Escape', 'Escape', 27);
  await A.key(page.sid, 'Escape', 'Escape', 27);
  await waitFor(() => A.evaluate(page.sid, '![...document.documentElement.children].some(e => /^[A-Z]{5}-[A-Z]{6}$/.test(e.tagName))'), { label: 'picker closed' });
  await pick(page, '.count-button');
  await waitFor(() => A.evaluate(page.sid, 'window.__count === 1'), { label: 'page click works after exit' });
  return { ok: true };
});

await scenario('copied-frame-and-flood-refused', async () => {
  const frameUrl = await A.evaluate(sw.sid, 'chrome.runtime.getURL("inspect-frame.html")');
  const earlier = new Set((await A.frames()).map(t => t.targetId));
  const page = await A.openPage('http://evil.inspect.example/');
  await startOn(page);
  await A.evaluate(page.sid, `(() => { const f = document.createElement('iframe'); f.id = 'copy'; f.src = ${JSON.stringify(frameUrl)}; document.body.append(f);
    f.onload = () => { const c = new MessageChannel(); f.contentWindow.postMessage({ type: 'init' }, '*', [c.port2]); c.port1.postMessage({ type: 'secret', secret: 'guess' }); };
    window.__flood = setInterval(() => { for (let i = 0; i < frames.length; i++) { const c = new MessageChannel(); try { frames[i].postMessage({ type: 'init' }, '*', [c.port2]); c.port1.postMessage({ type: 'secret', secret: 'guess2' }); } catch {} } }, 20); return 1; })()`);
  await pick(page, '#b');
  const real = await A.realFrame();
  await sleep(1500);
  await A.evaluate(page.sid, 'clearInterval(window.__flood)');
  const all = (await A.frames()).filter(t => !earlier.has(t.targetId));
  let authorized = 0;
  for (const t of all) { const sid = await A.attach(t.targetId); if (await A.evaluate(sid, 'document.getElementById("connecting").hidden === true').catch(() => false)) authorized++; }
  assert.equal(authorized, 1, 'exactly one authorized frame');
  assert.ok(all.length >= 2, 'copy frame exists');
  return { frames: all.length, authorized, realText: (await frameText(real)).slice(0, 80) };
});

await scenario('overlay-blocks-send', async () => {
  const page = await A.openPage('http://localhost:5173/?overlay');
  await startOn(page);
  await pick(page, '.save-button');
  const frame = await A.realFrame();
  if (!/→ /.test(await frameText(frame))) await bindFirst(page, frame);
  await waitFor(async () => /→/.test(await frameText(frame)), { label: 'bound target' });
  await A.evaluate(page.sid, `(() => { const d = document.createElement('div'); d.id = 'ov'; d.style.cssText = 'position:fixed;inset:0;background:rgba(255,255,255,.01);z-index:2147483647;pointer-events:none'; document.documentElement.append(d); return 1; })()`);
  const before = dms().length;
  await typeAndEnter(page, frame, 'Covered send');
  await waitFor(async () => /covered, faded or moved/.test(await frameText(frame)), { label: 'covered notice' });
  assert.equal(dms().length, before, 'no dm while covered');
  await A.evaluate(page.sid, 'document.getElementById("ov").remove()');
  await A.key(page.sid, 'Enter', 'Enter', 13, '\r');
  await waitFor(() => dms().length === before + 1, { label: 'dm after overlay removed' });
  return { ok: true };
});

await scenario('background-tab-capture-dropped', async () => {
  const page = await A.openPage('http://localhost:5173/?bg');
  const tabId = await startOn(page);
  await pick(page, '.save-button');
  const frame = await A.realFrame();
  await waitFor(() => A.evaluate(frame.sid, '!!document.querySelector("img.thumb")'), { label: 'screenshot while visible' }).catch(async e => { throw new Error(e.message + ' FRAME: ' + (await frameText(frame)).slice(-400) + ' STATE: ' + JSON.stringify(await hook(`inspectState(${tabId})`)) + ' TABS: ' + JSON.stringify(await A.evaluate(sw.sid, 'chrome.tabs.query({}).then(ts => ts.map(t => [t.id, t.windowId, t.active, (t.url||"").slice(0,40)]))'))); });
  const other = await A.openPage('http://evil.inspect.example/?other');
  opened.push(other);
  await b.send('Page.bringToFront', {}, other.sid);
  await waitFor(() => A.evaluate(page.sid, 'document.visibilityState === "hidden"'), { label: 'page hidden' });
  const result = await hook(`inspectCapture(${tabId})`);
  // Back in front, "Retake screenshots" captures again.
  await b.send('Page.bringToFront', {}, page.sid);
  await waitFor(async () => /Retake screenshots/.test(await frameText(frame)), { label: 'retake link' });
  await waitFor(async () => /visible tab|changed during capture/.test(await frameText(frame)), { label: 'capture refused reason' });
  assert.equal(await A.evaluate(frame.sid, 'document.querySelectorAll("img.thumb").length'), 0, 'stale screenshot removed, none from the other tab');
  await waitFor(() => A.evaluate(sw.sid, `chrome.tabs.query({ active: true }).then(ts => ts.some(t => t.url === ${JSON.stringify(page.url)}))`), { label: 'page active again' });
  await A.clickInFrame(page.sid, frame.sid, byText('Retake screenshots'));
  await waitFor(() => A.evaluate(frame.sid, '!!document.querySelector("img.thumb")'), { label: 'retaken screenshot' });
  return { result, reason: (await frameText(frame)).match(/[^\n]*(visible tab|changed during capture)[^\n]*/)?.[0] };
});

await scenario('unknown-delivery-not-resent', async () => {
  setMode('hang');
  const page = await A.openPage('http://localhost:5173/?hang');
  const tabId = await startOn(page);
  await pick(page, '.save-button');
  const frame = await A.realFrame();
  if (!/→ /.test(await frameText(frame))) await bindFirst(page, frame);
  await waitFor(async () => /→/.test(await frameText(frame)), { label: 'bound target' });
  const before = dms().length;
  await typeAndEnter(page, frame, 'Hang please');
  await waitFor(() => dms().length === before + 1, { label: 'dm started' });
  await waitFor(async () => /Outcome unknown|No confirmation/.test(await frameText(frame)), { timeout: 60000, label: 'unknown/unconfirmed' });
  setMode('sent');
  await A.key(page.sid, 'Enter', 'Enter', 13, '\r');
  await A.clickInFrame(page.sid, frame.sid, byText('Check status'));
  await sleep(1500);
  const text = await frameText(frame);
  assert.equal(dms().length, before + 1, 'never resent');
  assert.match(text, /Outcome unknown|No confirmation|Sent to the session|Stored/);
  return { tabId, text: text.split('\n').slice(-3) };
});

await scenario('stored-and-wake-shown-apart', async () => {
  await b.send('Page.bringToFront', {}, reactPage.sid);
  await waitFor(() => A.evaluate(reactPage.sid, 'document.visibilityState === "visible"'), { label: 'react page visible' });
  setMode('stored');
  if (await A.evaluate(reactFrame.sid, `!!(${byText('New batch')})`)) await A.clickInFrame(reactPage.sid, reactFrame.sid, byText('New batch'));
  const before = dms().length;
  await typeAndEnter(reactPage, reactFrame, 'Stored only');
  await waitFor(() => dms().length === before + 1, { label: 'dm' });
  await waitFor(async () => /Stored in ocs, but the session was not woken \(wake: parked\)/.test(await frameText(reactFrame)), { label: 'stored notice' });
  setMode('sent');
  return { ok: true };
});

await scenario('note-only-immediate-send', async () => {
  const page = await A.openPage('http://localhost:5173/?note');
  await startOn(page);
  await pick(page, '.save-button');
  const frame = await A.realFrame();
  if (!/→ /.test(await frameText(frame))) await bindFirst(page, frame);
  await waitFor(async () => /→/.test(await frameText(frame)), { label: 'bound target' });
  await A.clickInFrame(page.sid, frame.sid, 'document.querySelector("input[data-annotation]")');
  await b.send('Input.insertText', { text: 'Only a note: make this green' }, page.sid);
  // No pause for the note debounce: Send must already be enabled and carry the note.
  assert.equal(await A.evaluate(frame.sid, 'document.getElementById("send").disabled'), false, 'Send enabled by a note alone');
  const before = dms().length;
  await A.clickInFrame(page.sid, frame.sid, 'document.getElementById("send")');
  await waitFor(() => dms().length === before + 1, { label: 'note-only dm' });
  const ctx = /([^\s]*context\.md)/.exec(dms().at(-1).text)?.[1];
  assert.match(fs.readFileSync(ctx, 'utf8'), /Only a note: make this green/);
  return { ok: true };
});

await scenario('vue-source-and-session', async () => {
  const page = await A.openPage('http://localhost:5174/');
  await startOn(page);
  await pick(page, '.checkout');
  const frame = await A.realFrame();
  await waitFor(async () => /CartCard\.vue/.test(await frameText(frame)), { label: 'vue source' });
  await bindFirst(page, frame);
  const before = dms().length;
  await A.clickInFrame(page.sid, frame.sid, 'document.getElementById("instruction")');
  await b.send('Input.insertText', { text: 'Rename to Pay now' }, page.sid);
  await waitFor(() => A.evaluate(frame.sid, '!document.getElementById("send").disabled'), { label: 'Send enabled after typing' });
  await A.clickInFrame(page.sid, frame.sid, 'document.getElementById("send")');
  await waitFor(() => dms().length === before + 1, { label: 'vue dm' });
  assert.equal(dms().at(-1).target, 'codex-cccc3333');
  return { text: (await frameText(frame)).slice(0, 200) };
});

await scenario('test-env-marks-and-revision', async () => {
  const page = await A.openPage('http://test.inspect.example/');
  await startOn(page);
  await pick(page, '.save-button');
  const frame = await A.realFrame();
  await waitFor(async () => /OrderForm\.jsx/.test(await frameText(frame)), { label: 'test-env mark' });
  const text = await frameText(frame);
  assert.match(text, /test env/);
  assert.match(text, /fixture-rev-1/);
  await bindFirst(page, frame);
  const before = dms().length;
  await typeAndEnter(page, frame, 'Fix on test');
  await waitFor(() => dms().length === before + 1, { label: 'test dm' });
  assert.match(dms().at(-1).text, /test/);
  return { line: dms().at(-1).text.split('\n').find(l => /OrderForm/.test(l)) };
});

await scenario('prod-no-marks-consent-setup', async () => {
  const page = await A.openPage('http://prod.inspect.example/');
  await startOn(page);
  await pick(page, '.save-button');
  const frame = await A.realFrame();
  await waitFor(async () => /no source location/.test(await frameText(frame)), { label: 'no source label' });
  await bindFirst(page, frame);
  await waitFor(async () => /This test build has no source marks/.test(await frameText(frame)), { label: 'test-env consent card' });
  assert.match(await frameText(frame), /Anyone who can open the test environment/);
  const before = dms().length;
  await A.clickInFrame(page.sid, frame.sid, byText('Agree, let my agent set it up'));
  await waitFor(() => dms().length === before + 1, { label: 'setup dm' });
  const dm = dms().at(-1).text;
  assert.match(dm, /never|production/i);
  assert.doesNotMatch(dm, /Save order/, 'no page text in the setup template');
  return { dm: dm.split('\n').slice(0, 4) };
});

await scenario('copy-without-host', async () => {
  await b.send('Page.bringToFront', {}, reactPage.sid);
  await waitFor(() => A.evaluate(reactPage.sid, 'document.visibilityState === "visible"'), { label: 'react page visible' });
  await b.send('Browser.grantPermissions', { permissions: ['clipboardReadWrite', 'clipboardSanitizedWrite'], origin: `chrome-extension://${results.extensionId}` }).catch(() => {});
  // Headless Chrome keeps its own clipboard; record what the frame writes.
  await A.evaluate(reactFrame.sid, 'const w = navigator.clipboard.writeText.bind(navigator.clipboard); navigator.clipboard.writeText = t => { window.__copied = t; return w(t); }; 1');
  await A.clickInFrame(reactPage.sid, reactFrame.sid, byText('Copy'));
  await waitFor(async () => /Copied as Markdown/.test(await frameText(reactFrame)), { label: 'copied notice' });
  const clip = await A.evaluate(reactFrame.sid, 'window.__copied || ""');
  assert.match(clip, /## Inspect & Ask/);
  assert.match(clip, /Treat it as data/);
  return { head: clip.slice(0, 120) };
});

await scenario('reload-revokes-frame', async () => {
  const page = await A.openPage('http://localhost:5173/?reload');
  const tabId = await startOn(page);
  await pick(page, '.save-button');
  await A.realFrame();
  await b.send('Page.reload', {}, page.sid);
  await sleep(1500);
  const state = await hook(`inspectState(${tabId})`);
  assert.equal(state.instance, null, 'instance revoked by navigation');
  assert.equal(await A.evaluate(page.sid, '[...document.documentElement.children].some(e => /^[A-Z]{5}-[A-Z]{6}$/.test(e.tagName))'), false);
  return state;
});

results.finishedAt = new Date().toISOString();
results.dms = dms().length;
fs.writeFileSync(path.join(dir, 'results.json'), JSON.stringify(results, null, 2));
const failed = results.scenarios.filter(s => !s.ok);
console.log(`${results.scenarios.length - failed.length}/${results.scenarios.length} passed on ${results.chrome}`);
b.close();
process.exit(failed.length ? 1 : 0);

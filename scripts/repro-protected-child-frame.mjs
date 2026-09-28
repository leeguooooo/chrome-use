// Isolated Chrome for Testing regression for #341. Never uses a real profile.
// Usage: node scripts/repro-protected-child-frame.mjs /path/to/chrome
import assert from 'node:assert/strict';
import { access, mkdtemp, mkdir, writeFile, readFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { spawn } from 'node:child_process';
import { createServer } from 'node:http';
import { setTimeout as delay } from 'node:timers/promises';

const chrome = process.argv[2];
if (!chrome) throw new Error('Pass a Chrome for Testing executable');
await access(chrome);
const root = await mkdtemp(join(tmpdir(), 'chrome-use-341-'));
const driver = join(root, 'driver');
const foreign = join(root, 'foreign');
await mkdir(driver); await mkdir(foreign);
for (const [dir, name] of [[driver, 'driver'], [foreign, 'foreign']]) {
  await writeFile(join(dir, 'manifest.json'), JSON.stringify({
    manifest_version: 3, name, version: '1.0',
    permissions: name === 'driver' ? ['debugger', 'tabs'] : [],
    background: { service_worker: `${name}.js` },
    web_accessible_resources: [{ resources: ['widget.html'], matches: ['<all_urls>'] }],
  }));
  await writeFile(join(dir, `${name}.js`), 'chrome.runtime.onInstalled.addListener(() => {});');
  await writeFile(join(dir, 'widget.html'), '<p>Protected fixture</p>');
}
const server = createServer((req, res) => {
  res.setHeader('Content-Type', 'text/html');
  res.end('<!doctype html><title>Healthy parent fixture</title><p id="parent">Parent works</p>');
});
await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
const url = `http://127.0.0.1:${server.address().port}/`;
const child = spawn(chrome, ['--headless=new', '--no-first-run', '--no-default-browser-check',
  `--user-data-dir=${join(root, 'profile')}`, '--remote-debugging-port=0',
  `--disable-extensions-except=${driver},${foreign}`, `--load-extension=${driver},${foreign}`, url],
{ windowsHide: true, stdio: 'ignore' });
let spawnError;
child.on('error', error => { spawnError = error; });
let ws;
try {
  let port;
  for (let i = 0; i < 100; i++) {
    if (spawnError) throw spawnError;
    try { port = (await readFile(join(root, 'profile', 'DevToolsActivePort'), 'utf8')).split('\n')[0]; break; }
    catch { await delay(100); }
  }
  assert.ok(port, 'Chrome remote debugging started');
  const version = await (await fetch(`http://127.0.0.1:${port}/json/version`)).json();
  ws = new WebSocket(version.webSocketDebuggerUrl);
  await new Promise((resolve, reject) => { ws.onopen = resolve; ws.onerror = reject; });
  let id = 0;
  const pending = new Map();
  ws.onmessage = ({ data }) => {
    const msg = JSON.parse(data);
    const handler = pending.get(msg.id);
    if (!handler) return;
    pending.delete(msg.id);
    msg.error ? handler.reject(new Error(JSON.stringify(msg.error))) : handler.resolve(msg.result);
  };
  const send = (method, params = {}, sessionId) => new Promise((resolve, reject) => {
    const commandId = ++id;
    const timer = setTimeout(() => { pending.delete(commandId); reject(new Error(`Timed out: ${method}`)); }, 15000);
    pending.set(commandId, { resolve: x => { clearTimeout(timer); resolve(x); }, reject: e => { clearTimeout(timer); reject(e); } });
    ws.send(JSON.stringify({ id: commandId, method, params, sessionId }));
  });
  let targets;
  for (let i = 0; i < 100; i++) {
    targets = (await send('Target.getTargets')).targetInfos;
    if (targets.some(x => x.url.endsWith('/driver.js')) && targets.some(x => x.url.endsWith('/foreign.js'))) break;
    await delay(100);
  }
  const driverTarget = targets.find(x => x.url.endsWith('/driver.js'));
  const foreignTarget = targets.find(x => x.url.endsWith('/foreign.js'));
  assert.ok(driverTarget && foreignTarget, 'Both fixture extensions loaded');
  const { sessionId } = await send('Target.attachToTarget', { targetId: driverTarget.targetId, flatten: true });
  const expression = `(${async function (url, foreignUrl) {
    const results = [];
    for (const autoAttach of [false, true]) {
      const tab = await chrome.tabs.create({ url });
      for (let i = 0; i < 100; i++) {
        await new Promise(r => setTimeout(r, 50));
        if ((await chrome.tabs.get(tab.id)).status === 'complete') break;
      }
      const target = { tabId: tab.id };
      globalThis.reproStep = 'attach ' + autoAttach;
      await chrome.debugger.attach(target, '1.3');
      globalThis.reproStep = 'autoAttach ' + autoAttach;
      await chrome.debugger.sendCommand(target, 'Target.setAutoAttach', { autoAttach, flatten: true, waitForDebuggerOnStart: false });
      globalThis.reproStep = 'baseline ' + autoAttach;
      const before = await chrome.debugger.sendCommand(target, 'Runtime.evaluate', { expression: 'document.title', returnByValue: true });
      // Navigation into protected content can invalidate the injection's reply.
      // Record that separately; only subsequent commands prove the denied state.
      let injectionError = '';
      try {
        const injection = await chrome.debugger.sendCommand(target, 'Runtime.evaluate', { expression: `(() => { const f = document.createElement('iframe'); f.id='protected'; f.src=${JSON.stringify(foreignUrl)}; document.body.appendChild(f); })()` });
        if (injection.exceptionDetails) throw new Error(JSON.stringify(injection.exceptionDetails));
      } catch (e) { injectionError = e.message; }
      await new Promise(r => setTimeout(r, 500));
      let error = '';
      try { await chrome.debugger.sendCommand(target, 'DOM.enable'); }
      catch (e) { error = e.message; }
      let reattachError = '';
      try { await chrome.debugger.attach(target, '1.3'); } catch (e) { reattachError = e.message; }
      // Reload removes this manually inserted fixture, then verify page access
      // actually recovers instead of treating a successful attach as sufficient.
      await chrome.tabs.reload(tab.id);
      await new Promise(r => setTimeout(r, 500));
      try { await chrome.debugger.attach(target, '1.3'); }
      catch (e) { if (!/already attached/.test(e.message)) throw e; }
      const recovered = await chrome.debugger.sendCommand(target, 'Runtime.evaluate', {
        expression: '({title: document.title, hasProtectedFrame: !!document.getElementById("protected")})', returnByValue: true,
      });
      results.push({ autoAttach, title: before.result.value, injectionError, error, reattachError, recovered: recovered.result.value });
      await chrome.tabs.remove(tab.id);
    }
    return results;
  }})(${JSON.stringify(url)}, ${JSON.stringify(new URL('widget.html', foreignTarget.url).href)})`;
  const result = await send('Runtime.evaluate', { expression, awaitPromise: true, returnByValue: true }, sessionId);
  if (result.exceptionDetails) console.log(await send('Runtime.evaluate', { expression: 'globalThis.reproStep', returnByValue: true }, sessionId));
  assert.equal(result.exceptionDetails, undefined, JSON.stringify(result.exceptionDetails));
  const observations = result.result.value;
  console.log(JSON.stringify({ browser: version.Browser, observations, fixture: root }, null, 2));
  for (const observation of observations) {
    assert.equal(observation.title, 'Healthy parent fixture');
    assert.match(observation.error, /Cannot access a chrome-extension:\/\/ URL of (?:a )?different extension/);
    assert.match(observation.reattachError, /Cannot access a chrome-extension:\/\/ URL of (?:a )?different extension/);
    assert.deepEqual(observation.recovered, { title: 'Healthy parent fixture', hasProtectedFrame: false });
  }
  console.log('PASS: parent DOM.enable is denied with auto-attach both disabled and enabled');
} finally {
  ws?.close(); child.kill(); server.close();
}

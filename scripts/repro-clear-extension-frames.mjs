// Isolated Chrome for Testing check for --clear-extension-frames. Never uses a
// real profile. A "foreign" extension puts a 1Password-style host with a closed
// shadow root and its own chrome-extension:// iframe into a page, which makes
// Chrome refuse chrome.debugger commands on the tab; ab-connect's
// removeForeignExtensionFrames, run through chrome.scripting in the isolated
// world, removes it and the debugger works again.
// Usage: node scripts/repro-clear-extension-frames.mjs /path/to/chrome-for-testing
import assert from 'node:assert/strict';
import { access, mkdtemp, mkdir, writeFile, readFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { spawn } from 'node:child_process';
import { createServer } from 'node:http';
import { setTimeout as delay } from 'node:timers/promises';
import { removeForeignExtensionFrames, PASSWORD_MANAGER_FRAME_HOSTS } from '../extensions/ab-connect/extension-frames.js';

const chrome = process.argv[2];
if (!chrome) throw new Error('Pass a Chrome for Testing executable');
await access(chrome);
const root = await mkdtemp(join(tmpdir(), 'chrome-use-clear-frames-'));
const driver = join(root, 'driver');
const foreign = join(root, 'foreign');
await mkdir(driver); await mkdir(foreign);
await writeFile(join(driver, 'manifest.json'), JSON.stringify({
  manifest_version: 3, name: 'driver', version: '1.0',
  permissions: ['debugger', 'tabs', 'scripting', 'webNavigation'],
  host_permissions: ['<all_urls>'],
  background: { service_worker: 'driver.js' },
}));
await writeFile(join(driver, 'driver.js'), 'chrome.runtime.onInstalled.addListener(() => {});');
await writeFile(join(foreign, 'manifest.json'), JSON.stringify({
  manifest_version: 3, name: 'foreign', version: '1.0',
  background: { service_worker: 'foreign.js' },
  content_scripts: [{ matches: ['http://127.0.0.1/*'], js: ['inject.js'], run_at: 'document_idle' }],
  web_accessible_resources: [{ resources: ['widget.html'], matches: ['<all_urls>'] }],
}));
await writeFile(join(foreign, 'foreign.js'), 'chrome.runtime.onInstalled.addListener(() => {});');
await writeFile(join(foreign, 'widget.html'), '<p>hidden widget</p>');
// Like 1Password's Universal Sign On: a custom host, a closed shadow root, a
// zero-height frame of the extension's own page.
await writeFile(join(foreign, 'inject.js'), `
  const host = document.createElement('com-1password-uso');
  const shadow = host.attachShadow({ mode: 'closed' });
  const frame = document.createElement('iframe');
  frame.src = chrome.runtime.getURL('widget.html');
  frame.style.height = '0';
  shadow.appendChild(frame);
  document.documentElement.appendChild(host);
`);
const server = createServer((req, res) => {
  res.setHeader('Content-Type', 'text/html');
  res.end('<!doctype html><title>Sign-in fixture</title><input id="user"><iframe src="/inner"></iframe>');
});
await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
const url = `http://127.0.0.1:${server.address().port}/`;
const child = spawn(chrome, ['--headless=new', '--no-first-run', '--no-default-browser-check',
  `--user-data-dir=${join(root, 'profile')}`, '--remote-debugging-port=0',
  `--disable-extensions-except=${driver},${foreign}`, `--load-extension=${driver},${foreign}`, 'about:blank'],
{ windowsHide: true, stdio: 'ignore' });
let ws;
try {
  let port;
  for (let i = 0; i < 100 && !port; i++) {
    try { port = (await readFile(join(root, 'profile', 'DevToolsActivePort'), 'utf8')).split('\n')[0]; }
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
    const h = pending.get(msg.id);
    if (!h) return;
    pending.delete(msg.id);
    msg.error ? h.reject(new Error(JSON.stringify(msg.error))) : h.resolve(msg.result);
  };
  const send = (method, params = {}, sessionId) => new Promise((resolve, reject) => {
    const commandId = ++id;
    pending.set(commandId, { resolve, reject });
    ws.send(JSON.stringify({ id: commandId, method, params, sessionId }));
  });
  let targets;
  for (let i = 0; i < 100; i++) {
    targets = (await send('Target.getTargets')).targetInfos;
    if (targets.some(x => x.url.endsWith('/driver.js'))) break;
    await delay(100);
  }
  const driverTarget = targets.find(x => x.url.endsWith('/driver.js'));
  assert.ok(driverTarget, 'driver extension loaded');
  const { sessionId } = await send('Target.attachToTarget', { targetId: driverTarget.targetId, flatten: true });
  const expression = `(async () => {
    const removeForeignExtensionFrames = ${removeForeignExtensionFrames.toString()};
    const tab = await chrome.tabs.create({ url: ${JSON.stringify(url)} });
    for (let i = 0; i < 100; i++) {
      await new Promise(r => setTimeout(r, 50));
      if ((await chrome.tabs.get(tab.id)).status === 'complete') break;
    }
    await new Promise(r => setTimeout(r, 500));
    const target = { tabId: tab.id };
    let attachError = '';
    try { await chrome.debugger.attach(target, '1.3'); } catch (e) { attachError = e.message; }
    let blocked = '';
    try { await chrome.debugger.sendCommand(target, 'DOM.enable'); } catch (e) { blocked = e.message; }
    const frames = await chrome.webNavigation.getAllFrames({ tabId: tab.id });
    const frameIds = frames.filter(f => /^https?:/.test(f.url)).map(f => f.frameId);
    const results = await chrome.scripting.executeScript({
      target: { tabId: tab.id, frameIds }, world: 'ISOLATED',
      func: removeForeignExtensionFrames, args: [chrome.runtime.id, ${JSON.stringify([...PASSWORD_MANAGER_FRAME_HOSTS])}, null],
    });
    await new Promise(r => setTimeout(r, 300));
    if (attachError) await chrome.debugger.attach(target, '1.3');
    await chrome.debugger.sendCommand(target, 'DOM.enable');
    const after = await chrome.debugger.sendCommand(target, 'Runtime.evaluate', {
      expression: '({title: document.title, hosts: document.getElementsByTagName("com-1password-uso").length, input: !!document.getElementById("user")})',
      returnByValue: true,
    });
    return { attachError, blocked, removed: results.flatMap(r => r.result?.removed || []), after: after.result.value };
  })()`;
  const result = await send('Runtime.evaluate', { expression, awaitPromise: true, returnByValue: true }, sessionId);
  assert.equal(result.exceptionDetails, undefined, JSON.stringify(result.exceptionDetails));
  const out = result.result.value;
  console.log(JSON.stringify({ browser: version.Browser, ...out, fixture: root }, null, 2));
  assert.match(out.blocked || out.attachError, /Cannot access a chrome-extension:\/\/ URL of (?:a )?different extension/);
  assert.equal(out.removed.length, 1);
  assert.equal(out.removed[0].host, 'com-1password-uso');
  assert.deepEqual(out.after, { title: 'Sign-in fixture', hosts: 0, input: true });
  console.log('PASS: the foreign frame blocked the tab, was removed from the isolated world, and the debugger works again');
} finally {
  ws?.close(); child.kill(); server.close();
}

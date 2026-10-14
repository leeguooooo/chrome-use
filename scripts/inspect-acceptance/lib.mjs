// Minimal CDP helpers for the Inspect & Ask acceptance run (Node 22+, no deps).
import { spawn } from 'node:child_process';

export const sleep = ms => new Promise(r => setTimeout(r, ms));

export async function waitFor(fn, { timeout = 10000, interval = 150, label = 'condition' } = {}) {
  const end = Date.now() + timeout;
  let last;
  while (Date.now() < end) {
    try { last = await fn(); if (last) return last; } catch (e) { last = e; }
    await sleep(interval);
  }
  throw new Error(`timed out waiting for ${label}${last instanceof Error ? ': ' + last.message : ''}`);
}

export async function launch({ chrome, port, userDataDir, args = [], env = {} }) {
  // Refuse to reuse a stale browser on the same port.
  try { await fetch(`http://127.0.0.1:${port}/json/version`); throw new Error(`port ${port} already has a browser; kill it first`); } catch (e) { if (String(e.message).startsWith('port ')) throw e; }
  const proc = spawn(chrome, [`--remote-debugging-port=${port}`, `--user-data-dir=${userDataDir}`, '--no-first-run', '--no-default-browser-check', '--window-size=1280,900', ...args, 'about:blank'], { stdio: 'ignore', env: { ...process.env, ...env } });
  const version = await waitFor(async () => (await fetch(`http://127.0.0.1:${port}/json/version`)).json(), { timeout: 20000, label: 'chrome' });
  const ws = new WebSocket(version.webSocketDebuggerUrl);
  await new Promise((resolve, reject) => { ws.onopen = resolve; ws.onerror = reject; });
  let id = 0;
  const pending = new Map();
  const listeners = [];
  ws.onmessage = event => {
    const message = JSON.parse(event.data);
    if (message.id && pending.has(message.id)) { pending.get(message.id)(message); pending.delete(message.id); return; }
    for (const fn of listeners) fn(message);
  };
  const send = (method, params = {}, sessionId) => new Promise((resolve, reject) => {
    const i = ++id;
    const timer = setTimeout(() => { pending.delete(i); reject(new Error(`${method}: no reply in 30 s`)); }, 30000);
    pending.set(i, m => { clearTimeout(timer); m.error ? reject(new Error(`${method}: ${m.error.message}`)) : resolve(m.result); });
    ws.send(JSON.stringify({ id: i, method, params, sessionId }));
  });
  return { proc, send, listeners, version: version.Browser, close: () => { try { ws.close(); } catch {} proc.kill('SIGKILL'); } };
}

export function api(b) {
  const targets = async () => (await b.send('Target.getTargets')).targetInfos;
  const attach = async targetId => (await b.send('Target.attachToTarget', { targetId, flatten: true })).sessionId;
  const evaluate = async (sid, expression) => {
    const r = await b.send('Runtime.evaluate', { expression, awaitPromise: true, returnByValue: true }, sid);
    if (r.exceptionDetails) throw new Error(r.exceptionDetails.exception?.description || r.exceptionDetails.text);
    return r.result.value;
  };
  async function worker() {
    const t = await waitFor(async () => (await targets()).find(x => x.type === 'service_worker' && x.url === 'chrome-extension://ciiljdlhdpfckdcfkphgmfalanpdejep/background.js'), { timeout: 20000, label: 'extension worker' });
    return { sid: await attach(t.targetId), target: t };
  }
  async function openPage(url) {
    const { targetId } = await b.send('Target.createTarget', { url });
    const sid = await attach(targetId);
    await b.send('Page.enable', {}, sid);
    await b.send('Runtime.enable', {}, sid);
    await waitFor(() => evaluate(sid, 'document.readyState === "complete"'), { label: 'page load ' + url });
    return { sid, targetId, url };
  }
  async function click(sid, x, y, modifiers = 0) {
    await b.send('Input.dispatchMouseEvent', { type: 'mouseMoved', x, y, modifiers }, sid);
    await b.send('Input.dispatchMouseEvent', { type: 'mousePressed', x, y, button: 'left', clickCount: 1, modifiers }, sid);
    await b.send('Input.dispatchMouseEvent', { type: 'mouseReleased', x, y, button: 'left', clickCount: 1, modifiers }, sid);
  }
  async function center(sid, selector) {
    return evaluate(sid, `(() => { const r = document.querySelector(${JSON.stringify(selector)}).getBoundingClientRect(); return { x: r.left + r.width / 2, y: r.top + r.height / 2 }; })()`);
  }
  // The real input frame sits in the picker's closed shadow root; CDP can pierce it.
  async function frameBox(sid) {
    const { root } = await b.send('DOM.getDocument', { depth: -1, pierce: true }, sid);
    let found = null;
    const walk = (node, inPicker) => {
      if (!node || found) return;
      const picker = inPicker || /^[A-Z]{5}-[A-Z]{6}$/.test(node.nodeName);
      if (picker && node.nodeName === 'IFRAME') { found = node; return; }
      for (const child of [...(node.children || []), ...(node.shadowRoots || [])]) walk(child, picker);
    };
    walk(root, false);
    if (!found) return null;
    const { model } = await b.send('DOM.getBoxModel', { backendNodeId: found.backendNodeId }, sid);
    return { x: model.content[0], y: model.content[1], width: model.width, height: model.height };
  }
  async function frames() {
    return (await targets()).filter(t => t.type === 'iframe' && t.url.includes('/inspect-frame.html'));
  }
  // Each page's picker gets its own frame; frames already handed out belong to
  // earlier pages and are skipped.
  const claimed = new Set();
  async function realFrame() {
    return waitFor(async () => {
      for (const t of await frames()) {
        if (claimed.has(t.targetId)) continue;
        const sid = await attach(t.targetId);
        if (await evaluate(sid, 'document.getElementById("connecting").hidden === true').catch(() => false)) { claimed.add(t.targetId); return { sid, target: t }; }
      }
      return null;
    }, { timeout: 10000, label: 'authorized input frame' });
  }
  async function clickInFrame(pageSid, frameSid, selector) {
    // The picker repositions the frame when its content height changes; wait
    // until both the frame box and the element's position hold still.
    const measure = async () => {
      const box = await frameBox(pageSid);
      const r = await evaluate(frameSid, `(() => { const el = ${selector}; if (!el) return null; el.scrollIntoView({ block: 'nearest' }); const r = el.getBoundingClientRect(); return { x: r.left + r.width / 2, y: r.top + r.height / 2 }; })()`);
      return box && r ? { x: box.x + r.x, y: box.y + r.y } : null;
    };
    let previous = null;
    const point = await waitFor(async () => {
      const now = await measure();
      const stable = now && previous && Math.abs(now.x - previous.x) < 1 && Math.abs(now.y - previous.y) < 1;
      previous = now;
      return stable ? now : null;
    }, { interval: 200, label: 'frame element ' + selector });
    await click(pageSid, point.x, point.y);
  }
  async function key(sid, keyName, code, vk, text, modifiers = 0) {
    await b.send('Input.dispatchKeyEvent', { type: 'rawKeyDown', key: keyName, code, windowsVirtualKeyCode: vk, nativeVirtualKeyCode: vk, modifiers }, sid);
    if (text) await b.send('Input.dispatchKeyEvent', { type: 'char', key: keyName, code, text, unmodifiedText: text, windowsVirtualKeyCode: vk, modifiers }, sid);
    await b.send('Input.dispatchKeyEvent', { type: 'keyUp', key: keyName, code, windowsVirtualKeyCode: vk, nativeVirtualKeyCode: vk, modifiers }, sid);
  }
  return { targets, attach, evaluate, worker, openPage, click, center, frameBox, frames, realFrame, clickInFrame, key };
}

// Find a button in the frame by its text.
export const byText = text => `[...document.querySelectorAll('button')].find(b => b.textContent.trim() === ${JSON.stringify(text)})`;

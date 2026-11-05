// Browser CPU while the hinge marker is shown vs not (#851). Isolated GUI
// host only; throwaway Chrome for Testing profile.
//   CHROME_USE_ISOLATED_GUI_HOST=1 CHROME=<binary> node cpu.mjs
import { spawn, execFileSync } from 'node:child_process';
import { mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
if (process.env.CHROME_USE_ISOLATED_GUI_HOST !== '1' || !process.env.CHROME) process.exit(2);
const sleep = ms => new Promise(r => setTimeout(r, ms));
const profile = mkdtempSync(join(tmpdir(), 'cu-marker-cpu-'));
const chrome = spawn(process.env.CHROME, [`--user-data-dir=${profile}`, '--remote-debugging-port=0', '--no-first-run', '--no-default-browser-check', '--window-size=1000,800', 'data:text/html,<body style="background:%23fff">idle'], { stdio: 'ignore' });
const byType = () => Object.fromEntries(execFileSync('ps', ['-axo', 'time=,command='], { encoding: 'utf8' }).split('\n').filter(l => l.includes(profile)).map(l => { const t = l.trim().split(/\s+/); const p = t[0].split(':').map(Number); const ty = (l.match(/--type=([a-z-]+)/) || [, 'browser'])[1] + (l.match(/--utility-sub-type=([\w.]+)/) || [, ''])[1]; return [ty + ':' + Math.random().toString(36).slice(2,5), p.length === 3 ? p[0] * 3600 + p[1] * 60 + p[2] : p[0] * 60 + p[1]]; }));
const cpu = () => execFileSync('ps', ['-axo', 'time=,command='], { encoding: 'utf8' }).split('\n').filter(l => l.includes(profile))
  .reduce((s, l) => { const p = l.trim().split(/\s+/)[0].split(':').map(Number); return s + (p.length === 3 ? p[0] * 3600 + p[1] * 60 + p[2] : p[0] * 60 + p[1]); }, 0);
try {
  let port; for (let i = 0; i < 100 && !port; i++) { try { port = Number(readFileSync(join(profile, 'DevToolsActivePort'), 'utf8').split('\n')[0]); } catch { await sleep(100); } }
  const info = await (await fetch(`http://127.0.0.1:${port}/json/version`)).json();
  const ws = new WebSocket(info.webSocketDebuggerUrl); await new Promise(r => { ws.onopen = r; });
  let seq = 0; const pending = new Map(); ws.onmessage = e => { const m = JSON.parse(e.data); pending.get(m.id)?.(m); pending.delete(m.id); };
  const call = (method, params = {}, sessionId) => new Promise(r => { const id = ++seq; pending.set(id, r); ws.send(JSON.stringify({ id, method, params, ...(sessionId ? { sessionId } : {}) })); });
  const { result: { targetInfos } } = await call('Target.getTargets');
  const { result: { sessionId } } = await call('Target.attachToTarget', { targetId: targetInfos.find(t => t.type === 'page').targetId, flatten: true });
  await call('Page.bringToFront', {}, sessionId); await sleep(3000);
  const hinge = { hingeConfig: { rect: { x: 300, y: 200, width: 16, height: 16 }, contentColor: { r: 247, g: 140, b: 0, a: 1 }, outlineColor: { r: 255, g: 255, b: 255, a: 0.9 } } };
  const phase = async (label, fn, ms = 10000) => { const a = cpu(); await fn(); await sleep(ms); const d = cpu() - a; console.log(label, d.toFixed(2) + ' s CPU in ' + ms / 1000 + ' s'); return d; };
  const out = {};
  out.idle = await phase('idle (no marker)', async () => {});
  out.shown = await phase('marker shown, static', () => call('Overlay.setShowHinge', hinge, sessionId));
  out.hidden = await phase('marker hidden again', () => call('Overlay.setShowHinge', {}, sessionId));
  // Pointer traffic like a humanized click stream: 400 moves + 20 clicks.
  const traffic = async () => { for (let i = 0; i < 400; i++) { await call('Input.dispatchMouseEvent', { type: 'mouseMoved', x: 100 + (i % 200), y: 150 + (i % 50) }, sessionId); if (i % 20 === 0) { await call('Input.dispatchMouseEvent', { type: 'mousePressed', x: 300, y: 200, button: 'left', buttons: 1, clickCount: 1 }, sessionId); await call('Input.dispatchMouseEvent', { type: 'mouseReleased', x: 300, y: 200, button: 'left', buttons: 0, clickCount: 1 }, sessionId); } await sleep(15); } };
  out.inputHidden = await phase('input traffic, marker hidden', traffic, 2000);
  await call('Overlay.setShowHinge', hinge, sessionId);
  out.inputShown = await phase('input traffic, marker shown', traffic, 2000);
  await call('Overlay.setShowHinge', {}, sessionId);
  out.inputHidden2 = await phase('input traffic, marker hidden again', traffic, 2000);
  console.log(JSON.stringify(out));
} finally { chrome.kill(); await sleep(300); rmSync(profile, { recursive: true, force: true }); }

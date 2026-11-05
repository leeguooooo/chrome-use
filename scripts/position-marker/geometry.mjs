// Position-marker geometry probe (#851). Isolated GUI host only: launches a
// throwaway Chrome for Testing profile per (zoom, device scale) case, never a
// personal browser. For each case it clicks a fixture dot through
// Input.dispatchMouseEvent at the dot's CSS centre, draws Overlay.setShowHinge
// centred on the same CSS point, and compares the marker's centroid in a page
// capture with the dot's. Also checks a scrolled page and a screencast frame.
//
//   CHROME_USE_ISOLATED_GUI_HOST=1 CHROME=<chrome binary> node geometry.mjs <out-dir>
import { spawn } from 'node:child_process';
import { createServer } from 'node:http';
import { mkdtempSync, mkdirSync, writeFileSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { decodePng, region, magenta, green } from './png.mjs';

const out = process.argv[2];
if (!out || process.env.CHROME_USE_ISOLATED_GUI_HOST !== '1' || !process.env.CHROME) {
  console.error('usage: CHROME_USE_ISOLATED_GUI_HOST=1 CHROME=<binary> node geometry.mjs <out-dir>');
  process.exit(2);
}
mkdirSync(out, { recursive: true });
const sleep = ms => new Promise(r => setTimeout(r, ms));
const DOT = { x: 300, y: 220 };
const html = `<!doctype html><meta charset="utf-8"><link rel="icon" href="data:,"><style>
html,body{margin:0;background:#fff;height:3000px}#dot{position:absolute;left:${DOT.x - 4}px;top:${DOT.y - 4}px;width:8px;height:8px;background:rgb(0,255,0)}
#far{position:absolute;left:${DOT.x - 4}px;top:${DOT.y + 1200 - 4}px;width:8px;height:8px;background:rgb(0,255,0)}</style>
<div id="dot"></div><div id="far"></div><script>window.clicks=[];addEventListener('click',e=>clicks.push([e.clientX,e.clientY,e.isTrusted]),true)</script>`;
const server = createServer((q, r) => { r.setHeader('content-type', 'text/html'); r.end(html); });
await new Promise(r => server.listen(0, '127.0.0.1', r));
const url = `http://127.0.0.1:${server.address().port}/`;

async function withChrome({ zoom, dsf }, fn) {
  const profile = mkdtempSync(join(tmpdir(), 'cu-marker-geom-'));
  mkdirSync(join(profile, 'Default'), { recursive: true });
  // Chrome stores the default page zoom as a level: zoom = 1.2 ** level.
  writeFileSync(join(profile, 'Default', 'Preferences'), JSON.stringify({
    partition: { default_zoom_level: { x: Math.log(zoom) / Math.log(1.2) } },
  }));
  const chrome = spawn(process.env.CHROME, [`--user-data-dir=${profile}`, '--remote-debugging-port=0', '--no-first-run',
    '--no-default-browser-check', `--force-device-scale-factor=${dsf}`, '--window-size=1000,800', '--window-position=40,40', 'about:blank'],
  { stdio: 'ignore' });
  let ws;
  try {
    let port;
    for (let i = 0; i < 100 && !port; i++) { try { port = Number(readFileSync(join(profile, 'DevToolsActivePort'), 'utf8').split('\n')[0]); } catch { await sleep(100); } }
    if (!port) throw Error('no DevTools port');
    const info = await (await fetch(`http://127.0.0.1:${port}/json/version`)).json();
    ws = new WebSocket(info.webSocketDebuggerUrl);
    await new Promise((r, j) => { ws.onopen = r; ws.onerror = j; });
    let seq = 0; const pending = new Map(); const events = [];
    ws.onmessage = e => { const m = JSON.parse(e.data); if (m.id && pending.has(m.id)) { const p = pending.get(m.id); pending.delete(m.id); m.error ? p.j(Error(JSON.stringify(m.error))) : p.r(m.result); } else if (m.method) events.push(m); };
    let session;
    const call = (method, params = {}, sid = session) => new Promise((r, j) => { const id = ++seq; pending.set(id, { r, j }); ws.send(JSON.stringify({ id, method, params, ...(sid ? { sessionId: sid } : {}) })); setTimeout(() => { if (pending.delete(id)) j(Error('timed out: ' + method)); }, 10000).unref(); });
    const { targetInfos } = await call('Target.getTargets', {}, null);
    session = (await call('Target.attachToTarget', { targetId: targetInfos.find(t => t.type === 'page').targetId, flatten: true }, null)).sessionId;
    await call('Page.enable'); await call('Page.navigate', { url }); await call('Page.bringToFront'); await sleep(1200);
    return await fn({ call, events });
  } finally { try { ws?.close(); } catch {} chrome.kill(); await sleep(300); rmSync(profile, { recursive: true, force: true }); }
}

const hinge = (call, x, y) => call('Overlay.setShowHinge', { hingeConfig: { rect: { x: Math.round(x - 10), y: Math.round(y - 10), width: 20, height: 20 }, contentColor: { r: 255, g: 0, b: 255, a: 1 } } });
async function capture(call, label) {
  const { data } = await call('Page.captureScreenshot', { format: 'png', fromSurface: true });
  const png = Buffer.from(data, 'base64'); writeFileSync(join(out, label + '.png'), png); return decodePng(png);
}
const evaluate = async (call, expression) => (await call('Runtime.evaluate', { expression, returnByValue: true })).result.value;

const results = [];
for (const zoom of [0.5, 1, 1.5, 2]) for (const dsf of [1, 2]) {
  const label = `z${zoom}-d${dsf}`;
  console.error('case', label);
  try {
    results.push(await withChrome({ zoom, dsf }, async ({ call, events }) => {
      const page = await evaluate(call, '({dpr:devicePixelRatio,inner:[innerWidth,innerHeight],vv:[visualViewport.scale,visualViewport.width]})');
      const metrics = await call('Page.getLayoutMetrics');
      for (const type of ['mouseMoved', 'mousePressed', 'mouseReleased']) await call('Input.dispatchMouseEvent', { type, x: DOT.x, y: DOT.y, button: 'left', buttons: type === 'mousePressed' ? 1 : 0, clickCount: 1 });
      await hinge(call, DOT.x, DOT.y); await sleep(250);
      const img = await capture(call, label);
      const m = region(img, magenta);
      await call('Overlay.setShowHinge', {}); await sleep(150);
      const clean = await capture(call, label + '-hidden');
      const d = region(clean, green);
      // Scrolled: the far dot brought into view; same CSS point.
      console.error(' scroll');
      await evaluate(call, 'scrollTo(0, 1200)'); await sleep(600);
      const scrollY = await evaluate(call, 'scrollY');
      await hinge(call, DOT.x, DOT.y + 1200 - scrollY); await sleep(250);
      const s = region(await capture(call, label + '-scrolled'), magenta);
      await call('Overlay.setShowHinge', {}); await sleep(150);
      const sd = region(await capture(call, label + '-scrolled-hidden'), green);
      // Screencast: does a frame carry the marker?
      console.error(' screencast');
      await hinge(call, DOT.x, DOT.y + 1200 - scrollY); await sleep(200);
      events.length = 0; await call('Page.startScreencast', { format: 'png', everyNthFrame: 1 });
      let frame; for (let i = 0; i < 40 && !frame; i++) { frame = events.find(e => e.method === 'Page.screencastFrame'); if (!frame) await sleep(100); }
      await call('Page.stopScreencast');
      const sc = frame ? region(decodePng(Buffer.from(frame.params.data, 'base64')), magenta) : null;
      await call('Overlay.setShowHinge', {});
      const clicks = await evaluate(call, 'clicks');
      const off = (a, b) => a.count && b.count ? Math.hypot(a.center[0] - b.center[0], a.center[1] - b.center[1]) : null;
      return { label, zoom, dsf, page, cssVisual: metrics.cssVisualViewport, visual: metrics.visualViewport, clicks,
        image: [img.width, img.height], marker: m, dot: d, offsetPx: off(m, d), scrolled: { scrollY, marker: s, dot: sd, offsetPx: off(s, sd) },
        screencast: sc ? { markerPixels: sc.count } : 'no frame' };
    }));
  } catch (e) { results.push({ label, error: String(e) }); }
}
writeFileSync(join(out, 'geometry.json'), JSON.stringify(results, null, 2));
for (const r of results) console.log(r.error ? `${r.label} ERROR ${r.error}` : `${r.label} img=${r.image} dpr=${r.page.dpr} click=${JSON.stringify(r.clicks)} offset=${r.offsetPx?.toFixed(1)} scrolledOffset=${r.scrolled.offsetPx?.toFixed(1)} markerPx=${r.marker.count} screencast=${JSON.stringify(r.screencast)}`);
server.close();

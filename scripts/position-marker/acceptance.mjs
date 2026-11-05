// Position-marker acceptance (#849–#851) with the real chrome-use binary.
// Isolated GUI host only: an isolated HOME, a throwaway Chrome for Testing
// profile per run, a loopback fixture page. Never a personal browser.
//
//   CHROME_USE_ISOLATED_GUI_HOST=1 CU=<chrome-use> CHROME=<chrome binary> \
//     node acceptance.mjs <out-dir> [launch|relay]
//
// For each mode (marker off, marker on; plus page zoom 150% with it on) it
// runs the same actions and records:
// - what the page can see (globals, symbols, DOM mutations, events and their
//   trust, focus, visibility, viewport, requests) — must not differ;
// - wall time per click and the CPU the daemon and browser used;
// - independent pixel evidence through a second CDP connection: the marker is
//   at the click point, absent from `chrome-use screenshot`, back after it,
//   gone after expiry and after a navigation.
import { spawnSync, execFileSync, spawn } from 'node:child_process';
import { mkdtempSync, mkdirSync, writeFileSync, rmSync, existsSync } from 'node:fs';
import { join } from 'node:path';
import { decodePng, region } from './png.mjs';
import { readFileSync } from 'node:fs';

const [out, transport = 'launch'] = process.argv.slice(2);
const { CU, CHROME, EXT } = process.env;
if (!out || process.env.CHROME_USE_ISOLATED_GUI_HOST !== '1' || !CU || !CHROME || (transport === 'relay' && !EXT)) {
  console.error('usage: CHROME_USE_ISOLATED_GUI_HOST=1 CU=<chrome-use> CHROME=<binary> [EXT=<unpacked ab-connect>] node acceptance.mjs <out> [launch|relay]');
  process.exit(2);
}
mkdirSync(out, { recursive: true });
const sleep = ms => new Promise(r => setTimeout(r, ms));
const orange = (r, g, b) => r > 220 && g > 100 && g < 185 && b < 90;
const CLICKS = Number(process.env.CLICKS || 20);

// The fixture server runs in its own process: chrome-use calls below are
// synchronous and would otherwise stall it. `/__requests` counts page loads.
const html = `<!doctype html><meta charset="utf-8"><title>marker fixture</title><link rel="icon" href="data:,"><style>
html,body{margin:0;background:#fff;height:2400px;font:16px sans-serif}
button{position:absolute;left:260px;top:180px;width:120px;height:60px;background:#2a6;color:#fff;border:0}
#hov{position:absolute;left:520px;top:180px;width:120px;height:60px;background:#36c}
input{position:absolute;left:260px;top:300px}</style>
<button id="target">Target</button><div id="hov">hover</div><input id="field" aria-label="field">
<script>
(() => {
  const own = new Set(Object.getOwnPropertyNames(window));
  const f = window.fixture = { mutations: 0, events: [], own: [...own] };
  new MutationObserver(r => { f.mutations += r.length; }).observe(document.documentElement, { subtree: true, attributes: true, childList: true, characterData: true });
  for (const t of ['pointerdown','pointerup','mousedown','mouseup','click','mouseover','wheel','focus','blur','visibilitychange','resize'])
    addEventListener(t, e => f.events.push(t + ':' + e.isTrusted + ':' + (e.target === window ? 'window' : e.target === document ? 'document' : e.target.id || e.target.nodeName)), true);
  // Input delivery as the page sees it: aligned (buffered) input coalesces
  // moves within a frame; a change here would be page-visible.
  f.moves = 0; f.coalesced = 0;
  addEventListener('pointermove', e => { f.moves++; f.coalesced += e.getCoalescedEvents ? e.getCoalescedEvents().length : 1; }, true);
  // Frame cadence over a window, for marker shown vs not.
  f.raf = ms => new Promise(done => { const t = []; const t0 = performance.now(); const step = now => { t.push(now); if (now - t0 < ms) requestAnimationFrame(step); else { const g = t.slice(1).map((v, i) => v - t[i]); g.sort((a, b) => a - b); done({ frames: t.length, medianGap: +g[g.length >> 1].toFixed(2), maxGap: +g[g.length - 1].toFixed(2) }); } }; requestAnimationFrame(step); });
  f.state = () => {
    const now = Object.getOwnPropertyNames(window).filter(n => !own.has(n) && n !== 'fixture').sort();
    return { newGlobals: now, symbols: Object.getOwnPropertySymbols(window).map(String).sort(), nodes: document.querySelectorAll('*').length,
      mutations: f.mutations, events: f.events.slice(), focus: document.activeElement && (document.activeElement.id || document.activeElement.nodeName),
      visibility: document.visibilityState, viewport: [innerWidth, innerHeight, devicePixelRatio, visualViewport.scale],
      hasFocus: document.hasFocus(), webdriver: navigator.webdriver, moves: f.moves, coalesced: f.coalesced };
  };
})();
</script>`;
const serverSrc = `import { createServer } from 'node:http';
let requests = 0; const html = ${JSON.stringify(html)};
const s = createServer((q, r) => { if (q.url === '/__requests') return r.end(String(requests)); requests++; r.setHeader('content-type', 'text/html'); r.end(html); });
s.listen(0, '127.0.0.1', () => console.log(s.address().port));`;
const serverDir = mkdtempSync('/tmp/cupm-srv-'); writeFileSync(join(serverDir, 'server.mjs'), serverSrc);
const server = spawn(process.execPath, [join(serverDir, 'server.mjs')], { stdio: ['ignore', 'pipe', 'inherit'] });
const port = await new Promise(r => server.stdout.once('data', d => r(Number(String(d).trim()))));
const url = `http://127.0.0.1:${port}/`;
const requestCount = async () => Number(await (await fetch(url + '__requests')).text());

const rig = mkdtempSync('/tmp/cupm-');
const home = join(rig, 'home'); mkdirSync(home);

// Relay: Chrome for Testing with the unpacked extension and this binary as
// its native host, in the isolated HOME. The driver's own CDP connection
// (DevToolsActivePort) is only used by the inspector below.
let relayBrowser = null, relayWs = null;
if (transport === 'relay') {
  const ud = join(rig, 'relay-profile'); mkdirSync(join(ud, 'NativeMessagingHosts'), { recursive: true });
  const nm = join(rig, 'nm-host.sh');
  writeFileSync(nm, `#!/bin/sh\nexport HOME='${home}' CHROME_USE_NO_UPDATE_CHECK=1 CHROME_USE_AUTO_UPGRADE=0\nexec '${CU}' __nm-host "$@"\n`, { mode: 0o755 });
  writeFileSync(join(ud, 'NativeMessagingHosts', 'com.agent_browser.connect.json'), JSON.stringify({ name: 'com.agent_browser.connect', description: 'marker acceptance', path: nm, type: 'stdio', allowed_origins: ['chrome-extension://ciiljdlhdpfckdcfkphgmfalanpdejep/'] }));
  relayBrowser = spawn(CHROME, [`--user-data-dir=${ud}`, '--remote-debugging-port=0', '--no-first-run', '--no-default-browser-check', `--load-extension=${EXT}`, `--disable-extensions-except=${EXT}`, '--window-size=1200,820', 'about:blank'], { stdio: 'ignore' });
  for (let i = 0; i < 300; i++) { try { const p = Number(readFileSync(join(ud, 'DevToolsActivePort'), 'utf8').split('\n')[0]); relayWs = (await (await fetch(`http://127.0.0.1:${p}/json/version`)).json()).webSocketDebuggerUrl; break; } catch { await sleep(100); } }
  for (let i = 0; i < 300 && !existsSync(join(home, '.chrome-use', 'relay-cdp-url')); i++) await sleep(100);
  if (!relayWs || !existsSync(join(home, '.chrome-use', 'relay-cdp-url'))) throw Error('relay did not come up');
}
const browserPattern = label => transport === 'relay' ? join(rig, 'relay-profile') : join(rig, 'profile-' + label);

function cu(session, args, env = {}) {
  const t0 = process.hrtime.bigint();
  const r = spawnSync(CU, ['--session', session, '--json', ...args], {
    encoding: 'utf8', timeout: 60000,
    env: { ...process.env, HOME: home, CHROME_USE_AUTO_UPGRADE: '0', CHROME_USE_NO_UPDATE_CHECK: '1', AGENT_BROWSER_TIMING: '0', ...env },
  });
  const ms = Number(process.hrtime.bigint() - t0) / 1e6;
  let json = null; try { json = JSON.parse(r.stdout.trim().split('\n').pop()); } catch {}
  return { code: r.status, json, ms, stderr: r.stderr };
}
const must = (r, what) => { if (r.code !== 0) throw Error(`${what}: ${r.stderr || JSON.stringify(r.json)}`); return r; };

// A second CDP connection to the same browser: independent of the daemon.
async function inspector(cdpUrl, pageUrl) {
  const ws = new WebSocket(cdpUrl); await new Promise((r, j) => { ws.onopen = r; ws.onerror = j; });
  let seq = 0; const pending = new Map();
  ws.onmessage = e => { const m = JSON.parse(e.data); const p = pending.get(m.id); if (p) { pending.delete(m.id); m.error ? p.j(Error(JSON.stringify(m.error))) : p.r(m.result); } };
  const call = (method, params = {}, sessionId) => new Promise((r, j) => { const id = 100000 + ++seq; pending.set(id, { r, j }); ws.send(JSON.stringify({ id, method, params, ...(sessionId ? { sessionId } : {}) })); setTimeout(() => pending.delete(id) && j(Error('timeout ' + method)), 10000).unref(); });
  const { targetInfos } = await call('Target.getTargets');
  const page = targetInfos.find(t => t.type === 'page' && t.url === pageUrl) || targetInfos.find(t => t.type === 'page' && t.url.startsWith(pageUrl));
  if (!page) throw Error('inspector: no page ' + pageUrl);
  const { sessionId } = await call('Target.attachToTarget', { targetId: page.targetId, flatten: true });
  return {
    async capture(label) {
      const { data } = await call('Page.captureScreenshot', { format: 'png' }, sessionId);
      const png = Buffer.from(data, 'base64'); writeFileSync(join(out, label + '.png'), png);
      return region(decodePng(png), orange);
    },
    close: () => ws.close(),
  };
}
const orangeIn = file => region(decodePng(readFileSync(file)), orange);

function cpuSeconds(pattern) {
  try {
    const lines = execFileSync('ps', ['-axo', 'time=,command='], { encoding: 'utf8' }).split('\n').filter(l => l.includes(pattern));
    return lines.reduce((sum, l) => { const t = l.trim().split(/\s+/)[0]; const p = t.split(':').map(Number); return sum + (p.length === 3 ? p[0] * 3600 + p[1] * 60 + p[2] : p[0] * 60 + p[1]); }, 0);
  } catch { return null; }
}

function cpuByType(pattern, before = {}) {
  const out = {};
  for (const l of execFileSync('ps', ['-axo', 'pid=,time=,command='], { encoding: 'utf8' }).split('\n').filter(l => l.includes(pattern))) {
    const [pid, t] = l.trim().split(/\s+/); const p = t.split(':').map(Number); const sec = p.length === 3 ? p[0] * 3600 + p[1] * 60 + p[2] : p[0] * 60 + p[1];
    const type = (l.match(/--type=([a-z-]+)/) || [, 'browser'])[1] + ((l.match(/--utility-sub-type=([\w.]+)/) || [, ''])[1] ? '/' + l.match(/--utility-sub-type=([\w.]+)/)[1] : '');
    out[type + '#' + pid] = +(sec - (before[type + '#' + pid] || 0)).toFixed(2);
  }
  return out;
}

async function run(label, { marker, zoom = 1 }) {
  const session = 'pm-' + label;
  const profile = join(rig, 'profile-' + label); mkdirSync(join(profile, 'Default'), { recursive: true });
  writeFileSync(join(profile, 'Default', 'Preferences'), JSON.stringify({ partition: { default_zoom_level: { x: Math.log(zoom) / Math.log(1.2) } } }));
  const env = { AGENT_BROWSER_SHOW_POSITION: marker ? '1' : '0' };
  const launch = transport === 'launch' ? ['--launch', '--headed', '--executable-path', CHROME, '--profile', profile] : [];
  const result = { label, marker, zoom, transport };
  const runUrl = url + '?run=' + label;
  const reqBefore = await requestCount();
  must(cu(session, [...launch, 'open', runUrl], env), 'open');
  await sleep(800);
  const baseline = must(cu(session, ['eval', 'fixture.state()'], env), 'eval').json.data.result;
  let cdpUrl = relayWs;
  if (!cdpUrl) { const cdp = must(cu(session, ['get', 'cdp-url'], env), 'cdp-url').json.data; cdpUrl = typeof cdp === 'string' ? cdp : cdp.cdpUrl || cdp.url || cdp.result; }
  const ins = await inspector(cdpUrl, runUrl);
  const bp = browserPattern(label);
  const cpu0 = { browser: cpuSeconds(bp), daemon: cpuSeconds(CU), byType: cpuByType(bp) };
  const times = [];
  for (let i = 0; i < CLICKS; i++) times.push(must(cu(session, ['click', '#target'], env), 'click').ms);
  result.cpuClicks = { browser: cpuSeconds(bp) - cpu0.browser, daemon: cpuSeconds(CU) - cpu0.daemon, byType: cpuByType(bp, cpu0.byType) };
  // Pixel evidence right after the last click.
  await sleep(150);
  result.afterClick = await ins.capture(label + '-after-click');
  // Frame cadence while the marker is up (on) or not (off).
  result.rafShown = must(cu(session, ['eval', 'fixture.raf(1000)'], env), 'raf').json.data.result;
  const shot = must(cu(session, ['screenshot', join(out, label + '-cu-screenshot.png'), '--full-res'], env), 'screenshot');
  result.screenshotWarning = shot.json.data.warning || null;
  result.inCuScreenshot = orangeIn(join(out, label + '-cu-screenshot.png'));
  result.afterScreenshot = await ins.capture(label + '-after-screenshot');
  await sleep(2600);
  result.afterExpiry = await ins.capture(label + '-after-expiry');
  result.rafAfterExpiry = must(cu(session, ['eval', 'fixture.raf(1000)'], env), 'raf').json.data.result;
  // A humanized move burst, for coalescing.
  for (let i = 0; i < 5; i++) must(cu(session, ['mouse', 'move', String(200 + i * 60), String(400 - i * 30)], env), 'move');
  must(cu(session, ['hover', '#hov'], env), 'hover'); await sleep(150);
  result.afterHover = await ins.capture(label + '-after-hover');
  // What the page saw, read before the navigation below replaces it.
  const final = must(cu(session, ['eval', 'fixture.state()'], env), 'eval').json.data.result;
  must(cu(session, ['click', '#target'], env), 'click');
  must(cu(session, ['open', runUrl + '&again'], env), 'reopen'); await sleep(300);
  ins.close();
  const ins2 = await inspector(cdpUrl, runUrl + '&again');
  result.afterNavigation = await ins2.capture(label + '-after-navigation');
  ins2.close();
  result.cpu = { browser: cpuSeconds(bp) - cpu0.browser, daemon: cpuSeconds(CU) - cpu0.daemon };
  result.status = cu(session, ['status'], env).json?.data?.positionMarker ?? null;
  result.page = { baseline, final, requests: (await requestCount()) - reqBefore };
  result.clickTimes = times.map(t => Math.round(t));
  times.sort((a, b) => a - b);
  result.clickMs = { median: times[times.length >> 1], p90: times[Math.floor(times.length * 0.9)], min: times[0], max: times[times.length - 1] };
  cu(session, ['close'], env);
  return result;
}

// #850 lifecycle with the marker on: another tab, a stopped daemon, a hung
// renderer. Times are how long a marker stayed up, from independent captures.
async function lifecycle() {
  const session = 'pm-life', env = { AGENT_BROWSER_SHOW_POSITION: '1' };
  const label = 'life';
  const profile = join(rig, 'profile-' + label); mkdirSync(profile, { recursive: true });
  const launch = transport === 'launch' ? ['--launch', '--headed', '--executable-path', CHROME, '--profile', profile] : [];
  const one = url + '?run=life1', two = url + '?run=life2';
  const r = { label: 'lifecycle', transport };
  must(cu(session, [...launch, 'open', one], env), 'open');
  let cdpUrl = relayWs;
  if (!cdpUrl) { const cdp = must(cu(session, ['get', 'cdp-url'], env), 'cdp-url').json.data; cdpUrl = typeof cdp === 'string' ? cdp : cdp.cdpUrl || cdp.url || cdp.result; }
  must(cu(session, ['click', '#target'], env), 'click');
  const a = await inspector(cdpUrl, one);
  r.firstTab = (await a.capture('life-tab1')).count;
  // Another tab gets the next point: the first tab's marker goes.
  must(cu(session, ['tab', 'new', two], env), 'tab new');
  must(cu(session, ['click', '#target'], env), 'click tab2');
  await sleep(300);
  r.firstTabAfterSwitch = await a.capture('life-tab1-after-switch').then(x => x.count, e => 'capture failed: ' + e.message);
  a.close();
  const b = await inspector(cdpUrl, two);
  r.secondTab = (await b.capture('life-tab2')).count;
  // A stopped daemon: nothing clears the marker from the CLI side.
  const pid = cu(session, ['status'], env).json?.data?.currentSession?.pid;
  r.daemonPid = pid;
  if (pid) {
    must(cu(session, ['click', '#target'], env), 'click');
    process.kill(pid, 'SIGSTOP');
    const t0 = Date.now(); let gone = null;
    while (Date.now() - t0 < 15000) { const c = await b.capture('life-stopped'); if (c.count === 0) { gone = Date.now() - t0; break; } await sleep(250); }
    r.stoppedDaemonClearedAfterMs = gone;
    process.kill(pid, 'SIGCONT');
    await sleep(500);
  }
  // A hung renderer: the clear is queued behind the page's own work.
  must(cu(session, ['click', '#target'], env), 'click');
  const hang = spawn(CU, ['--session', session, '--json', 'eval', 'const t = Date.now(); while (Date.now() - t < 6000); 1'], { env: { ...process.env, HOME: home, ...env }, stdio: 'ignore' });
  await sleep(7500);
  r.afterHang = (await b.capture('life-after-hang')).count;
  await new Promise(res => { hang.exitCode !== null ? res() : hang.once('exit', res); });
  b.close();
  r.status = cu(session, ['status'], env).json?.data?.positionMarker ?? null;
  cu(session, ['close'], env);
  return r;
}

const results = [];
let life = null;
try {
  let modes = [['off', { marker: false }], ['on', { marker: true }]];
  if (transport === 'launch') modes.push(['on-zoom150', { marker: true, zoom: 1.5 }]);
  // MODES=off,on picks runs; SKIP_LIFECYCLE=1 skips the #850 phase.
  if (process.env.MODES) modes = modes.filter(([l]) => process.env.MODES.split(',').includes(l));
  for (const [label, opts] of modes) {
    try { results.push(await run(label, opts)); } catch (e) { results.push({ label, error: String(e.stack || e) }); }
  }
  if (!process.env.SKIP_LIFECYCLE) try { life = await lifecycle(); } catch (e) { life = { error: String(e.stack || e) }; }
  writeFileSync(join(out, 'lifecycle.json'), JSON.stringify(life, null, 2));
} finally {
  server.kill(); rmSync(serverDir, { recursive: true, force: true });
  relayBrowser?.kill();
  writeFileSync(join(out, 'acceptance.json'), JSON.stringify(results, null, 2));
  rmSync(rig, { recursive: true, force: true });
}
const [off, on] = results;
const same = (k) => off?.page && on?.page && JSON.stringify(off.page.final[k]) === JSON.stringify(on.page.final[k]);
// Window focus/blur come from the OS activating the headed window, not from
// anything the page did; they are reported, not compared.
const pageEvents = r => r.page.final.events.filter(e => !/^(focus|blur):true:window$/.test(e));
const checks = off?.page && on?.page ? {
  noNewGlobals: on.page.final.newGlobals.length === off.page.final.newGlobals.length,
  sameSymbols: same('symbols'), sameNodes: same('nodes'), sameMutations: same('mutations'), sameEvents: JSON.stringify(pageEvents(off)) === JSON.stringify(pageEvents(on)),
  sameFocus: same('focus'), sameVisibility: same('visibility'), // The viewport is compared within each run: browser UI (an infobar, the tab
  // group strip) can differ between two runs' tabs before anything is drawn.
  viewportUnchanged: [off, on].every(r => JSON.stringify(r.page.baseline.viewport) === JSON.stringify(r.page.final.viewport)), sameRequests: off.page.requests === on.page.requests,
  markerDrawn: on.afterClick.count > 0, markerNotInScreenshot: on.inCuScreenshot.count === 0, markerRestored: on.afterScreenshot.count > 0,
  markerExpired: on.afterExpiry.count === 0, hoverDrawn: on.afterHover.count > 0, clearedByNavigation: on.afterNavigation.count === 0,
  offDrawsNothing: off.afterClick.count === 0 && off.afterHover.count === 0,
  clearedOnTabSwitch: life?.firstTabAfterSwitch === 0, clearedAfterRendererHang: life?.afterHang === 0,
  // Relay: the extension's watchdog bounds a stopped daemon's marker.
  ...(transport === 'relay' ? { stoppedDaemonBounded: typeof life?.stoppedDaemonClearedAfterMs === 'number' && life.stoppedDaemonClearedAfterMs < 6000 } : {}),
} : null;
writeFileSync(join(out, 'checks.json'), JSON.stringify(checks, null, 2));
console.log(JSON.stringify({ checks, life, results: results.map(r => r.error ? { label: r.label, error: r.error } : {
  label: r.label, clickMs: r.clickMs, cpu: r.cpu, afterClick: r.afterClick, screenshot: r.inCuScreenshot.count, warning: r.screenshotWarning, status: r.status }) }, null, 1));
process.exitCode = checks && Object.values(checks).every(Boolean) ? 0 : 1;

// Stealth check for one extension build: headed Chrome for Testing with the
// unpacked extension and the release host (isolated HOME), then
// 1) a page tries to detect the extension through its static-id URLs, and
// 2) scripts/stealth-bench.sh --relay runs against this browser.
import { spawn } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import http from 'node:http';
import { launch, api, waitFor, sleep } from './lib.mjs';
const [extDir, label] = process.argv.slice(2);
const dir = path.dirname(new URL(import.meta.url).pathname);
const home = fs.mkdtempSync(path.join(os.tmpdir(), 'stealth-home-'));
const ud = fs.mkdtempSync(path.join(os.tmpdir(), 'inspect-accept-stealth-'));
fs.mkdirSync(path.join(ud, 'NativeMessagingHosts'));
const nm = path.join(dir, 'nm-host-' + label + '.sh');
fs.writeFileSync(nm, `#!/bin/sh\nexport HOME="${home}"\nexec "${process.env.HOST_BIN}" __nm-host "$@"\n`, { mode: 0o755 });
fs.writeFileSync(path.join(ud, 'NativeMessagingHosts', 'com.agent_browser.connect.json'), JSON.stringify({ name: 'com.agent_browser.connect', description: 'x', path: nm, type: 'stdio', allowed_origins: ['chrome-extension://ciiljdlhdpfckdcfkphgmfalanpdejep/'] }));
const server = http.createServer((q, r) => { r.setHeader('content-type', 'text/html'); r.end('<!doctype html><title>probe</title><body>probe</body>'); }).listen(8896, '127.0.0.1');
const b = await launch({ chrome: process.env.CHROME, port: 9360, userDataDir: ud, args: [`--load-extension=${extDir}`, `--disable-extensions-except=${extDir}`] });
const A = api(b);
const out = { label, chrome: b.version };
try {
  await waitFor(() => fs.existsSync(path.join(home, '.chrome-use', 'relay-cdp-url')), { timeout: 30000, label: 'relay up' });
  const page = await A.openPage('http://127.0.0.1:8896/');
  const ID = 'ciiljdlhdpfckdcfkphgmfalanpdejep', BOGUS = 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa';
  const files = ['inspect-frame.html', 'inspect-frame.js', 'inspect-frame.css', 'inspect-format.js', 'background.js', 'manifest.json'];
  out.probe = await A.evaluate(page.sid, `(async () => {
    const res = {};
    const time = async fn => { const t = performance.now(); let r; try { r = await fn(); } catch (e) { r = 'throw:' + e.name; } return [r, Math.round(performance.now() - t)]; };
    const viaFetch = url => fetch(url).then(x => 'ok:' + x.status, e => 'err:' + e.name);
    const viaScript = url => new Promise(res => { const s = document.createElement('script'); s.src = url; s.onload = () => res('load'); s.onerror = () => res('error'); document.head.append(s); setTimeout(() => res('timeout'), 3000); });
    const viaLink = url => new Promise(res => { const l = document.createElement('link'); l.rel = 'stylesheet'; l.href = url; l.onload = () => res('load'); l.onerror = () => res('error'); document.head.append(l); setTimeout(() => res('timeout'), 3000); });
    for (const id of [${JSON.stringify('ciiljdlhdpfckdcfkphgmfalanpdejep')}, ${JSON.stringify('aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa')}]) for (const f of ${JSON.stringify(files)}) {
      const url = 'chrome-extension://' + id + '/' + f;
      res[(id.startsWith('a') ? 'bogus/' : 'real/') + f] = { fetch: (await time(() => viaFetch(url)))[0], script: (await time(() => viaScript(url)))[0], link: (await time(() => viaLink(url)))[0] };
    }
    res.domMarker = !!document.querySelector('chrome-use-inspect') || [...document.querySelectorAll('*')].some(e => e.tagName.includes('CHROME-USE'));
    return res;
  })()`);
  const real = Object.entries(out.probe).filter(([k]) => k.startsWith('real/'));
  out.distinguishable = real.filter(([k, v]) => JSON.stringify(v) !== JSON.stringify(out.probe['bogus/' + k.slice(5)])).map(([k]) => k);
  await b.send('Target.closeTarget', { targetId: page.targetId });
  const bench = spawn('bash', [path.join(dir, 'stealth-bench.sh'), '--relay'], { env: { ...process.env, HOME: home, CHROME_USE: process.env.HOST_BIN }, stdio: ['ignore', 'pipe', 'pipe'] });
  let text = '';
  bench.stdout.on('data', d => { text += d; });
  bench.stderr.on('data', d => { text += d; });
  out.benchExit = await new Promise(r => bench.on('close', r));
  out.bench = text.replace(/\x1b\[[0-9;]*m/g, '').split('\n').filter(l => /PASS|FAIL|bench|==/.test(l));
} catch (e) { out.error = String(e.stack || e); }
fs.writeFileSync(path.join(dir, `stealth-${label}.json`), JSON.stringify(out, null, 2));
console.log(JSON.stringify({ label, distinguishable: out.distinguishable, domMarker: out.probe?.domMarker, bench: out.bench, error: out.error }, null, 1));
server.close(); b.close(); process.exit(0);

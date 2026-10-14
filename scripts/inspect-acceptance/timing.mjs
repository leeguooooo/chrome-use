import fs from 'node:fs'; import os from 'node:os'; import path from 'node:path'; import http from 'node:http';
import { launch, api } from './lib.mjs';
const [extDir, label] = process.argv.slice(2);
const ud = fs.mkdtempSync(path.join(os.tmpdir(), 'inspect-accept-timing-'));
const server = http.createServer((q, r) => { r.setHeader('content-type', 'text/html'); r.end('<!doctype html><title>t</title>'); }).listen(8895, '127.0.0.1');
const b = await launch({ chrome: process.env.CHROME, port: 9361, userDataDir: ud, args: [`--load-extension=${extDir}`, `--disable-extensions-except=${extDir}`] });
const A = api(b);
const page = await A.openPage('http://127.0.0.1:8895/');
const r = await A.evaluate(page.sid, `(async () => {
  const urls = { warReal: 'chrome-extension://ciiljdlhdpfckdcfkphgmfalanpdejep/inspect-frame.html', nonWarReal: 'chrome-extension://ciiljdlhdpfckdcfkphgmfalanpdejep/background.js', bogus: 'chrome-extension://aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/inspect-frame.html' };
  const out = {};
  for (const [k, u] of Object.entries(urls)) out[k] = [];
  for (let i = 0; i < 40; i++) for (const [k, u] of Object.entries(urls)) { const t = performance.now(); try { await fetch(u); } catch {} out[k].push(performance.now() - t); }
  const stat = a => { a.sort((x, y) => x - y); return { median: +a[20].toFixed(2), p10: +a[4].toFixed(2), p90: +a[36].toFixed(2) }; };
  return Object.fromEntries(Object.entries(out).map(([k, a]) => [k, stat(a)]));
})()`);
console.log(label, JSON.stringify(r));
server.close(); b.close(); process.exit(0);

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
  const stat = a => { const s = [...a].sort((x, y) => x - y); return { n: s.length, min: s[0], p10: s[4], median: s[20], p90: s[36], max: s.at(-1) }; };
  return { samples: out, stats: Object.fromEntries(Object.entries(out).map(([k, a]) => [k, stat(a)])) };
})()`);
// Raw samples are kept; equal medians are an observation, not a proof.
const dir = path.dirname(new URL(import.meta.url).pathname);
fs.writeFileSync(path.join(dir, `timing-${label}.json`), JSON.stringify({ label, chrome: b.version, extensionVersion: JSON.parse(fs.readFileSync(path.join(extDir, 'manifest.json'), 'utf8')).version, zipSha256: process.env.ZIP_SHA256 || null, ...r }, null, 2));
console.log(label, JSON.stringify(r.stats));
server.close(); b.close(); fs.rmSync(ud, { recursive: true, force: true }); process.exit(0);

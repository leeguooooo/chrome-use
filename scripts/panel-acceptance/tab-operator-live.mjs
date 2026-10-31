// Live check of the operator card in the real side panel (build host only):
// a tab recorded as created by a session shows its agent and transcript with
// nothing added to the page; a tab nobody created shows no card.
import fs from 'node:fs';
import path from 'node:path';
import { randomUUID } from 'node:crypto';
import { fileURLToPath } from 'node:url';
import { startRig, cleanup, openTab, activate } from './rig.mjs';
import { waitFor } from './lib.mjs';

const dir = path.dirname(fileURLToPath(import.meta.url));
const work = process.env.WORK || fs.mkdtempSync('/tmp/op-');
const out = process.env.OUT || path.join(work, 'out');
fs.mkdirSync(out, { recursive: true });
const rig = await startRig({ dir, work, hostBin: process.env.HOST_BIN, chrome: process.env.CHROME, extDir: process.env.EXT_DIR, sitePort: 8945, cdpPort: 9405 });
const A = rig.cdp, b = rig.browser;
const panelTargets = async () => (await b.send('Target.getTargets')).targetInfos.filter(t => /\/sidepanel\.html(\?|$)/.test(t.url));
async function openPanelFor(tab) {
  const win = await rig.ev(`chrome.windows.create({ url: chrome.runtime.getURL('popup.html'), focused: true })`);
  const opener = await waitFor(async () => (await b.send('Target.getTargets')).targetInfos.find(t => t.url.endsWith('/popup.html') && t.type === 'page'), { label: 'opener' });
  const sid = await A.attach(opener.targetId);
  await waitFor(() => A.evaluate(sid, 'document.readyState === "complete"'), { label: 'opener load' });
  await A.evaluate(sid, `(() => { const b = document.createElement('button'); b.style = 'position:fixed;left:0;top:0;width:200px;height:80px;z-index:2147483647'; b.onclick = () => { chrome.sidePanel.setOptions({ tabId: ${tab.tabId}, path: 'sidepanel.html?tab=${tab.tabId}', enabled: true }); chrome.sidePanel.open({ tabId: ${tab.tabId} }); }; document.body.append(b); return true; })()`);
  await A.click(sid, 50, 40);
  const target = await waitFor(async () => (await panelTargets()).find(t => t.url.endsWith(`?tab=${tab.tabId}`)), { timeout: 15000, label: 'panel' });
  await rig.ev(`chrome.windows.remove(${win.id})`);
  const psid = await A.attach(target.targetId);
  await waitFor(() => A.evaluate(psid, `document.getElementById('connection').classList.contains('ok')`), { timeout: 30000, label: 'panel connected' });
  return { sid: psid, run: expression => A.evaluate(psid, expression) };
}
try {
  const owned = await openTab(rig, rig.site + '/?owned'), plain = await openTab(rig, rig.site + '/?plain');
  const id = randomUUID(), cwd = path.join(work, 'android-use'), base = path.join(rig.home, '.chrome-use', 'cu-android-use-abc123');
  const project = path.join(rig.home, '.claude/projects', cwd.replace(/[/.]/g, '-'));
  fs.mkdirSync(path.dirname(base), { recursive: true }); fs.mkdirSync(project, { recursive: true }); fs.mkdirSync(cwd);
  fs.writeFileSync(base + '.created-targets.json', JSON.stringify({ target_ids: [owned.targetId] }));
  fs.writeFileSync(base + '.origin.json', JSON.stringify({ agent: 'claude', conversationId: id, cwd }));
  fs.writeFileSync(base + '.last-commands.json', JSON.stringify([{ action: 'fill', startedAt: 1, state: 'completed' }]));
  fs.writeFileSync(path.join(project, id + '.jsonl'), [{ type: 'user', message: { role: 'user', content: 'find the firmware' } }, { type: 'assistant', message: { role: 'assistant', content: [{ type: 'text', text: 'opening the page' }] } }].map(m => JSON.stringify({ ...m, cwd, sessionId: id, timestamp: '2026-11-01T00:00:00Z' })).join('\n') + '\n');
  await activate(rig, owned);
  const panel = await openPanelFor(owned);
  const shown = await waitFor(() => panel.run(`document.getElementById('operatorCard').hidden ? null : document.getElementById('operatorCard').innerText`), { timeout: 15000, label: 'operator card' });
  console.log('[operator] card', JSON.stringify(shown));
  if (!/android-use/.test(shown) || !shown.includes(id)) throw new Error('card lacks project or conversation');
  await panel.run(`document.getElementById('operatorContext').click()`);
  const text = await waitFor(() => panel.run(`(t => /opening the page/.test(t) ? t : null)(document.getElementById('operatorTranscript').innerText)`), { timeout: 20000, label: 'transcript' });
  console.log('[operator] transcript', JSON.stringify(text));
  const s = await b.send('Page.captureScreenshot', { format: 'png' }, panel.sid); fs.writeFileSync(path.join(out, 'card.png'), Buffer.from(s.data, 'base64'));
  const page = await A.attach(owned.targetId);
  const extra = await A.evaluate(page, `document.documentElement.outerHTML.match(/operator/i)?.[0] ?? null`);
  if (extra) throw new Error('page was changed');
  await activate(rig, plain);
  const other = await openPanelFor(plain);
  await new Promise(r => setTimeout(r, 2500));
  if (!(await other.run(`document.getElementById('operatorCard').hidden`))) throw new Error('card on a tab nobody created');
  console.log('[operator] PASS', out);
} finally { cleanup(rig); }

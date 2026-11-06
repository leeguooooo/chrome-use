// Live check of the side panel's handoff card (build host only): a real
// `session handoff --wait --note …` shows the note and a Done button on the
// tab the session drives; Done ends the wait with the session back (exit 0),
// and nothing is added to the page. Then the same on a user's tab the session
// adopted (it created nothing there), and the card of a plain `handoff`.
import fs from 'node:fs';
import path from 'node:path';
import { spawn, spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { startRig, cleanup, openTab, activate, EXT_ID } from './rig.mjs';
import { waitFor } from './lib.mjs';

const dir = path.dirname(fileURLToPath(import.meta.url));
const work = process.env.WORK || fs.mkdtempSync('/tmp/hd-');
const out = process.env.OUT || path.join(work, 'out');
fs.mkdirSync(out, { recursive: true });
const rig = await startRig({ dir, work, hostBin: process.env.HOST_BIN, chrome: process.env.CHROME, extDir: process.env.EXT_DIR, sitePort: 8946, cdpPort: 9406 });
const A = rig.cdp, b = rig.browser;
const env = { ...process.env, HOME: rig.home, CHROME_USE_NO_UPDATE_CHECK: '1' };
const cli = (...args) => spawnSync(rig.hostBin, args, { env, encoding: 'utf8', timeout: 90000 });
async function handOffAndFinish(session, tab, note, shot) {
  await activate(rig, tab);
  waiter = spawn(rig.hostBin, ['--session', session, 'session', 'handoff', '--wait', '--note', note, '--timeout', '3m', '--json'], { env, stdio: ['ignore', 'pipe', 'pipe'] });
  let stdout = '', stderr = '';
  waiter.stdout.on('data', d => { stdout += d; });
  waiter.stderr.on('data', d => { stderr += d; });
  const exited = new Promise(resolve => waiter.on('exit', code => resolve(code)));
  await waitFor(() => fs.existsSync(path.join(sockDir(), `${session}.handoff.json`)) && fs.readFileSync(path.join(sockDir(), `${session}.owner`), 'utf8').trim() === 'user', { timeout: 30000, label: 'handed off' });
  const panel = await openPanelFor(tab);
  const card = await waitFor(() => panel.run(`document.getElementById('handoffBox').hidden ? null : document.getElementById('handoffBox').innerText`), { timeout: 20000, label: 'handoff card' });
  console.log('[handoff] card', JSON.stringify(card));
  if (!card.includes(note)) throw new Error('card lacks the note');
  await waitFor(() => panel.run(`!document.getElementById('handoffDone').hidden`), { timeout: 10000, label: 'Done button' });
  let s = await b.send('Page.captureScreenshot', { format: 'png' }, panel.sid); fs.writeFileSync(path.join(out, shot + '-card.png'), Buffer.from(s.data, 'base64'));
  // The wait is still running until Done.
  if (waiter.exitCode !== null) throw new Error('wait ended before Done: ' + stdout + stderr);
  await panel.run(`document.getElementById('handoffDone').click()`);
  const code = await Promise.race([exited, new Promise(r => setTimeout(() => r('timeout'), 30000))]);
  console.log('[handoff] wait exit', code, stdout.trim().slice(0, 400));
  if (code !== 0) throw new Error('wait did not end with exit 0: ' + stderr);
  const result = JSON.parse(stdout.trim().split('\n').pop());
  if (result.data?.owner !== 'agent' && result.owner !== 'agent') throw new Error('session not taken back');
  const ownerFile = path.join(sockDir(), `${session}.owner`);
  if (fs.existsSync(ownerFile) && fs.readFileSync(ownerFile, 'utf8').trim() === 'user') throw new Error('owner sidecar still user');
  await waitFor(() => panel.run(`document.getElementById('handoffBox').hidden`), { timeout: 15000, label: 'card cleared after resume' });
  s = await b.send('Page.captureScreenshot', { format: 'png' }, panel.sid); fs.writeFileSync(path.join(out, shot + '-after-done.png'), Buffer.from(s.data, 'base64'));
  // The page itself carries nothing of the card (read over the rig's CDP).
  const within = (p, ms, label) => Promise.race([p, new Promise((_, reject) => setTimeout(() => reject(new Error(label + ' timed out')), ms))]);
  const page = await within(A.attach(tab.targetId), 15000, 'page attach');
  if (await within(A.evaluate(page, `document.documentElement.outerHTML.match(/Approve the sign-in|Confirm the payment|operator-handoff/i)?.[0] ?? null`), 15000, 'page read')) throw new Error('page was changed');
}
async function openPanelFor(tab) {
  const { targetId } = await b.send('Target.createTarget', { url: `chrome-extension://${EXT_ID}/sidepanel.html?tab=${tab.tabId}` });
  const psid = await A.attach(targetId);
  await waitFor(() => A.evaluate(psid, `document.getElementById('connection')?.classList.contains('ok')`), { timeout: 30000, label: 'panel connected' });
  return { sid: psid, run: expression => A.evaluate(psid, expression) };
}
const session = 'hw-live', other = 'hw-adopt';
const sockDir = () => path.join(rig.home, '.chrome-use');
let waiter = null, passed = false;
try {
  const opened = cli('--session', session, 'open', rig.site + '/?handoff');
  console.log('[handoff] open', opened.status, (opened.stderr || '').trim().slice(-300));
  if (opened.status !== 0) throw new Error('open failed');
  const sock = path.join(rig.home, '.chrome-use');
  const record = path.join(sock, `${session}.created-targets.json`);
  const created = fs.existsSync(record) ? JSON.parse(fs.readFileSync(record, 'utf8')).target_ids || [] : [];
  const targets = (await b.send('Target.getTargets')).targetInfos;
  let tab;
  const mine = targets.find(t => created.includes(t.targetId));
  if (mine) {
    const match = (await rig.ev('chrome.tabs.query({})')).find(t => t.url === rig.site + '/?handoff');
    tab = match ? { tabId: match.id, targetId: mine.targetId } : null;
    console.log('[handoff] the session drives a rig tab', JSON.stringify(tab));
  }
  if (!tab) {
    // The session runs its own browser: point its record at a rig tab, so the
    // panel finds it the same way it finds a tab the session created.
    tab = await openTab(rig, rig.site + '/?handoff-panel');
    fs.writeFileSync(record, JSON.stringify({ target_ids: [...created, tab.targetId] }));
    console.log('[handoff] session browser is separate; mapped rig tab', tab.targetId);
  }
  await handOffAndFinish(session, tab, 'Approve the sign-in on your phone', 'created');

  // A tab the user had open, adopted by another session: not in any
  // created-targets record, so only the handoff record names it.
  const user = await openTab(rig, rig.site + '/?handoff-user');
  const adopted = cli('--session', other, 'tab', 'adopt', user.targetId, '--json');
  console.log('[handoff] adopt', adopted.status, (adopted.stdout + adopted.stderr).trim().slice(-300));
  if (adopted.status !== 0) throw new Error('adopt failed');
  const records = fs.readdirSync(sockDir()).filter(n => n.endsWith('.created-targets.json'));
  if (records.some(n => (JSON.parse(fs.readFileSync(path.join(sockDir(), n), 'utf8')).target_ids || []).includes(user.targetId))) throw new Error('the adopted tab is recorded as created; the check would prove nothing');
  await handOffAndFinish(other, user, 'Confirm the payment in your bank app', 'adopted');

  // A plain handoff (no --wait) shows the card too; Done marks it.
  const plain = cli('--session', other, 'session', 'handoff', '--json');
  if (plain.status !== 0) throw new Error('plain handoff failed: ' + plain.stderr);
  const panel = await openPanelFor(user);
  await waitFor(() => panel.run(`!document.getElementById('handoffBox').hidden && !document.getElementById('handoffDone').hidden`), { timeout: 20000, label: 'plain handoff card' });
  await panel.run(`document.getElementById('handoffDone').click()`);
  await waitFor(() => fs.existsSync(path.join(sockDir(), `${other}.handoff-done`)), { timeout: 15000, label: 'plain Done recorded' });
  cli('--session', other, 'session', 'resume');
  await waitFor(() => panel.run(`document.getElementById('handoffBox').hidden`), { timeout: 15000, label: 'plain card cleared' });
  console.log('[handoff] PASS', out);
  passed = true;
} finally {
  if (waiter && waiter.exitCode === null) waiter.kill();
  for (const name of [session, other]) spawnSync(rig.hostBin, ['--session', name, 'close'], { env, stdio: 'ignore', timeout: 20000 });
  cleanup(rig);
  // The relay-attached session can keep handles open; the result is decided.
  process.exit(passed ? 0 : 1);
}

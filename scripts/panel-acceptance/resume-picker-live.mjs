// Live check of the grouped `/resume` picker (ab-connect 0.5.66, CLI
// 1.5.227) through the real side panel (build host only; one browser run per
// host at a time):
//   HOST_BIN=<chrome-use> CHROME=<Chrome for Testing> EXT_DIR=<extension> node resume-picker-live.mjs
// The isolated HOME's ~/.codex holds sessions shaped like a real one after
// Codex Desktop imported other agents' sessions (2026-10-16): every file was
// written just now, while the sessions' own records say otherwise. Steps:
//   1. `/resume` groups sessions by project: the site's remembered project
//      first, then by latest activity, folderless ones under Other; each
//      project shows three sessions and "Show more";
//   2. times come from the records (an import is days old, a turnless thread
//      hours old) and only the session with an unfinished, recent turn is
//      active elsewhere; subagent threads are not listed;
//   3. Show more, folding a project, the arrow keys, and a search.
// Evidence and screenshots go to $OUT (default <work>/out).
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { randomUUID } from 'node:crypto';
import { startRig, cleanup, openTab, activate } from './rig.mjs';
import { waitFor } from './lib.mjs';

const dir = path.dirname(fileURLToPath(import.meta.url));
// Bindings store canonical folders (/tmp is /private/tmp on macOS).
const work = fs.realpathSync(process.env.WORK || fs.mkdtempSync('/tmp/rp-'));
const out = process.env.OUT || path.join(work, 'out');
fs.mkdirSync(out, { recursive: true });
const evidence = { work, hostBin: process.env.HOST_BIN, steps: [] };
const step = (name, data = {}) => { evidence.steps.push({ name, at: new Date().toISOString(), ...data }); console.log(`[resume-picker] ${name}`, JSON.stringify(data).slice(0, 600)); };
const save = () => fs.writeFileSync(path.join(out, 'evidence.json'), JSON.stringify(evidence, null, 1));
const check = (ok, message) => { if (!ok) { save(); throw new Error(message); } };

// Project folders, and Codex sessions in them.
const projects = path.join(work, 'projects');
const app = path.join(projects, 'app'), blog = path.join(projects, 'blog');
for (const d of [app, blog]) fs.mkdirSync(d, { recursive: true });
const now = Date.now(), iso = ms => new Date(ms).toISOString(), secs = ms => Math.floor(ms / 1000);
const HOUR = 3600e3, DAY = 24 * HOUR;
const sessionsDir = path.join(work, 'home', '.codex', 'sessions', '2026', '10', '16');
fs.mkdirSync(sessionsDir, { recursive: true });
const uuid = () => { const id = randomUUID(); return `01a1${id.slice(4)}`; };
const meta = (id, cwd, created, extra = {}) => ({ timestamp: iso(now), type: 'session_meta', payload: { id, timestamp: iso(created), cwd, originator: 'codex-tui', source: 'cli', ...extra } });
const user = (at, text) => ({ timestamp: iso(at), type: 'response_item', payload: { type: 'message', role: 'user', content: [{ type: 'input_text', text }] } });
const started = (at, turn = randomUUID()) => ({ timestamp: iso(at), type: 'event_msg', payload: { type: 'task_started', turn_id: turn, started_at: secs(at) } });
const completed = (at, turn) => ({ timestamp: iso(at), type: 'event_msg', payload: { type: 'task_complete', turn_id: turn, completed_at: secs(at) } });
const seeded = {};
function session(name, lines) {
  const id = lines[0].payload.id;
  // Every file was written just now, as after the import.
  fs.writeFileSync(path.join(sessionsDir, `rollout-2026-10-16T12-07-07-${id}.jsonl`), lines.map(l => JSON.stringify(l)).join('\n') + '\n');
  seeded[name] = id;
}
// The import: every record stamped now, the turn three days old.
{
  const id = uuid(), at = now - 3 * DAY;
  session('imported', [
    { ...meta(id, app, now, { originator: 'Codex Desktop', source: 'vscode' }) },
    { timestamp: iso(now), type: 'event_msg', payload: { type: 'task_started', turn_id: 'external-import-turn-1', started_at: secs(at - 600e3) } },
    { ...user(now, 'Imported: fix the checkout page') },
    { timestamp: iso(now), type: 'event_msg', payload: { type: 'agent_message', message: '<EXTERNAL SESSION IMPORTED>' } },
    { timestamp: iso(now), type: 'event_msg', payload: { type: 'task_complete', turn_id: 'external-import-turn-1', started_at: secs(at - 600e3), completed_at: secs(at) } },
  ]);
}
// A thread without turns whose meta line was rewritten: two hours old.
{ const id = uuid(); session('turnless', [meta(id, app, now - 2 * HOUR)]); }
// Four finished sessions in app, one to four days old.
for (let i = 1; i <= 4; i++) {
  const id = uuid(), at = now - i * DAY - HOUR, turn = randomUUID();
  session(`app${i}`, [meta(id, app, at - 600e3), started(at - 600e3, turn), user(at - 590e3, `App task ${i}`), completed(at, turn)]);
}
// blog: one finished ten minutes ago, one with a turn still open.
{ const id = uuid(), at = now - 10 * 60e3, turn = randomUUID(); session('blogDone', [meta(id, blog, at - 60e3), started(at - 60e3, turn), user(at - 50e3, 'Blog: write the release post'), completed(at, turn)]); }
{ const id = uuid(), turn = randomUUID(); session('blogOpen', [meta(id, blog, now - 5 * 60e3), started(now - 5 * 60e3, turn), user(now - 290e3, 'Blog: still working'), { timestamp: iso(now - 30e3), type: 'response_item', payload: { type: 'function_call', name: 'shell' } }]); }
// A subagent thread, and a session without a folder.
{ const id = uuid(), turn = randomUUID(); session('subagent', [meta(id, app, now - 60e3, { source: { subagent: { thread_spawn: { parent_thread_id: seeded.app1 } } } }), started(now - 60e3, turn), user(now - 50e3, 'Subagent work'), completed(now - 40e3, turn)]); }
{ const id = uuid(), turn = randomUUID(), at = now - 6 * HOUR; const m = meta(id, app, at); delete m.payload.cwd; session('folderless', [m, started(at, turn), user(at + 1000, 'No folder at all'), completed(at + 2000, turn)]); }
step('sessions seeded', { seeded });

let failState = async () => {};
const rig = await startRig({ dir, work, hostBin: process.env.HOST_BIN, chrome: process.env.CHROME, extDir: process.env.EXT_DIR, sitePort: 8950, cdpPort: 9411 });
evidence.chrome = rig.chromeVersion; evidence.extension = rig.extensionVersion;
const A = rig.cdp, b = rig.browser;
try {
  const site = await openTab(rig, rig.site + '/');
  // The site tab's own panel, opened the way the toolbar button does (from
  // a trusted click in a window of its own, so the tab stays active).
  await activate(rig, site);
  const win = await rig.ev(`chrome.windows.create({ url: chrome.runtime.getURL('popup.html'), focused: true })`);
  const opener = await waitFor(async () => (await b.send('Target.getTargets')).targetInfos.find(t => t.url.endsWith('/popup.html') && t.type === 'page'), { label: 'opener page' });
  const osid = await A.attach(opener.targetId);
  await waitFor(() => A.evaluate(osid, 'document.readyState === "complete"'), { label: 'opener load' });
  await A.evaluate(osid, `(() => { const b = document.createElement('button'); b.style = 'position:fixed;left:0;top:0;width:200px;height:80px;z-index:2147483647'; b.onclick = () => { chrome.sidePanel.setOptions({ tabId: ${site.tabId}, path: 'sidepanel.html?tab=${site.tabId}', enabled: true }); chrome.sidePanel.open({ tabId: ${site.tabId} }); }; document.body.append(b); return true; })()`);
  await A.click(osid, 50, 40);
  const panelTarget = await waitFor(async () => (await b.send('Target.getTargets')).targetInfos.find(t => t.url.endsWith(`/sidepanel.html?tab=${site.tabId}`)), { timeout: 15000, label: 'side panel' });
  await rig.ev(`chrome.windows.remove(${win.id})`);
  const ps = await A.attach(panelTarget.targetId);
  const panel = expression => A.evaluate(ps, expression);
  const msg = (key, ...subs) => panel(`chrome.i18n.getMessage(${JSON.stringify(key)}, ${JSON.stringify(subs)})`);
  const shot = async name => { const s = await b.send('Page.captureScreenshot', { format: 'png' }, ps); fs.writeFileSync(path.join(out, `${name}.png`), Buffer.from(s.data, 'base64')); };
  failState = async () => { await shot('fail').catch(() => {}); evidence.failure = await panel(`({ notice: document.getElementById('notice').textContent, picker: document.querySelector('.resume-picker')?.innerText })`).catch(e => String(e)); };
  evidence.uiLanguage = await panel('chrome.i18n.getUILanguage()');
  await waitFor(() => panel(`document.getElementById('connection').classList.contains('ok') && document.getElementById('pageTitle')?.textContent.length > 0 && !document.getElementById('projectChip').hidden`), { timeout: 30000, label: 'panel connected with its project chip' });
  // The site remembers app as its project (through the extension, as the
  // panel's Remember does); a new conversation loads it into the chip.
  const bound = await panel(`chrome.runtime.sendMessage({ type: 'ab-agent-request', operation: 'inspectProjectBind', params: { origin: ${JSON.stringify(rig.site)}, pathScope: '/', root: ${JSON.stringify(app)}, replaces: null } })`);
  step('site bound to app', { bound });
  check(bound?.success, 'the site binding was saved');
  await panel(`document.getElementById('newChat').click()`);
  await waitFor(() => panel(`document.getElementById('project').value === ${JSON.stringify(app)}`), { timeout: 20000, label: 'site project preselected' });

  // 1. /resume, grouped.
  await panel(`(() => { const s = document.getElementById('agent'); s.value = 'codex'; s.dispatchEvent(new Event('change')); document.getElementById('executable').value = ${JSON.stringify(rig.backend('codex'))}; document.getElementById('message').value = '/resume'; document.getElementById('composer').requestSubmit(); return true; })()`);
  await waitFor(() => panel(`document.querySelectorAll('.resume-project').length > 0`), { timeout: 30000, label: 'grouped picker' });
  const state = () => panel(`({
    headers: [...document.querySelectorAll('.resume-project')].map(h => ({ text: h.innerText.replace(/\\s+/g, ' ').trim(), title: h.title, expanded: h.getAttribute('aria-expanded') })),
    items: [...document.querySelectorAll('.resume-item')].map(i => ({ title: i.querySelector('.title').textContent, meta: i.querySelector('.meta').textContent, running: !!i.querySelector('.badge.running'), selected: i.getAttribute('aria-selected') })),
    more: [...document.querySelectorAll('.resume-more')].map(m => m.textContent),
  })`);
  const first = await state();
  await shot('1-grouped');
  step('picker grouped', first);
  const other = await msg('panel_resume_other'), siteLabel = await msg('panel_resume_site_project');
  check(first.headers.length === 3, 'three groups: app, blog and Other');
  check(first.headers[0].title === app && first.headers[0].text.includes(siteLabel), 'the site\'s project leads, its full path on hover');
  check(first.headers[1].title === blog, 'blog, the latest other project, is next');
  check(first.headers[2].text.includes(other), 'folderless sessions are under Other, last');
  check(first.items.length === 6, 'app shows three sessions, blog two, Other one');
  check(first.more.length === 1 && first.more[0] === await msg('panel_resume_show_more', '3'), 'and offers the other three');
  check(!first.items.some(i => i.title === 'Subagent work'), 'subagent threads are not listed');

  // 2. Times and running come from the records.
  await panel(`document.querySelector('.resume-more').click()`);
  const expanded = await state();
  await shot('2-show-more');
  step('app expanded', expanded);
  const byTitle = Object.fromEntries(expanded.items.map(i => [i.title, i]));
  const untitled = await msg('panel_resume_untitled');
  check(byTitle['Imported: fix the checkout page']?.meta === await msg('panel_time_days_ago', '3'), 'the import is three days old');
  check(byTitle[untitled]?.meta === await msg('panel_time_hours_ago', '2'), 'the turnless thread is two hours old');
  check(byTitle['Blog: write the release post']?.meta === await msg('panel_time_minutes_ago', '10'), 'a finished turn is as old as its end');
  check(expanded.items.filter(i => i.running).map(i => i.title).join() === 'Blog: still working', 'only the open, recent turn is active elsewhere');
  check(expanded.items.length === 9, 'every listed session is shown once app is expanded');
  check(expanded.more[0] === await msg('panel_resume_show_less'), 'Show less');

  // 3. Fold app; arrows walk what is on screen.
  await panel(`document.querySelector('.resume-project').click()`);
  await panel(`(() => { const s = document.querySelector('.resume-picker input[type=search]'); for (const key of ['ArrowDown', 'ArrowDown']) s.dispatchEvent(new KeyboardEvent('keydown', { key, bubbles: true })); return true; })()`);
  const folded = await state();
  await shot('3-folded-arrows');
  step('app folded, two arrows down', folded);
  check(folded.headers[0].expanded === 'false' && folded.items.length === 3, 'app folded: blog\'s two and Other\'s one remain');
  check(folded.items.map(i => i.selected).join() === 'false,true,false', 'the second session is highlighted');

  // A search shows every match, grouped, folded or not.
  await panel(`(() => { const s = document.querySelector('.resume-picker input[type=search]'); s.value = 'projects/app'; s.dispatchEvent(new Event('input')); return true; })()`);
  await waitFor(async () => { const s = await state(); return s.headers.length === 1 && s.items.length === 6 && s; }, { timeout: 20000, label: 'search by path' });
  const searched = await state();
  await shot('4-search-path');
  step('searched by path', searched);
  check(searched.headers[0].expanded === 'true' && searched.more.length === 0, 'a search shows all matches');

  evidence.passed = true;
  console.log('[resume-picker] PASS');
} catch (failure) {
  evidence.error = String(failure.message || failure);
  await failState();
  throw failure;
} finally {
  save();
  cleanup(rig);
  console.log(`evidence: ${out}`);
  setTimeout(() => process.exit(evidence.passed ? 0 : 1), 1000).unref();
}

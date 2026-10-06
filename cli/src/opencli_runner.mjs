// chrome-use ⇄ OpenCLI bridge. Runs one OpenCLI adapter with OpenCLI's own
// runtime (registry, argument coercion, pipeline executor, BasePage helpers)
// and a page whose transport is chrome-use, so the adapter drives the user's
// real, logged-in Chrome through the chrome-use daemon.
//
// Invoked by `chrome-use site <site>/<cmd>` when no chrome-use adapter has
// that name: node opencli_runner.mjs <request.json>
// request: { pkgDir, chromeUse, session, site, name, modulePath, kwargs }
// stdout: one JSON line { success, data | error, hint? }
import { spawn } from 'node:child_process';
import { readFileSync, mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';

const req = JSON.parse(readFileSync(process.argv[2], 'utf8'));
const dist = join(req.pkgDir, 'dist', 'src');
const mod = (p) => import(pathToFileURL(join(dist, p)).href);

const { BasePage } = await mod('browser/base-page.js');
const { buildEvaluateExpression } = await mod('browser/utils.js');
const { getRegistry } = await mod('registry-api.js');
const { executePipeline } = await mod('pipeline/index.js');
const { coerceAndValidateArgs } = await mod('execution.js');

function cu(args, stdin) {
  return new Promise((resolve, reject) => {
    const env = { ...process.env };
    if (req.session) env.AGENT_BROWSER_SESSION = req.session;
    const child = spawn(req.chromeUse, ['--json', ...args], { env, stdio: ['pipe', 'pipe', 'pipe'] });
    let out = '';
    let err = '';
    child.stdout.on('data', (d) => { out += d; });
    child.stderr.on('data', (d) => { err += d; });
    child.on('error', reject);
    child.on('close', () => {
      let resp;
      try { resp = JSON.parse(out.trim().split('\n').pop()); } catch (_) {
        return reject(new Error(`chrome-use ${args[0]}: ${(err || out).trim().slice(0, 500)}`));
      }
      if (!resp.success) return reject(new Error(`chrome-use ${args[0]}: ${resp.error}`));
      resolve(resp.data);
    });
    child.stdin.end(stdin ?? '');
  });
}

class ChromeUsePage extends BasePage {
  async goto(url, options) {
    await cu(['open', url]);
    this._lastUrl = url;
    if (options?.waitUntil !== 'none') {
      const { waitForDomStableJs } = await mod('browser/dom-helpers.js');
      const maxMs = options?.settleMs ?? 1000;
      await this.evaluate(waitForDomStableJs(maxMs, Math.min(500, maxMs))).catch(() => {});
    }
  }
  async evaluate(input, ...args) {
    const data = await cu(['eval', '--stdin'], buildEvaluateExpression(input, args));
    return data?.result;
  }
  async getCookies(opts = {}) {
    const a = ['cookies', 'get'];
    if (opts.url) a.push('--url', opts.url);
    const data = await cu(a);
    const cookies = Array.isArray(data?.cookies) ? data.cookies : [];
    const d = opts.domain?.replace(/^\./, '');
    return d ? cookies.filter((c) => { const cd = String(c.domain || '').replace(/^\./, ''); return cd === d || cd.endsWith('.' + d) || d.endsWith('.' + cd); }) : cookies;
  }
  async screenshot(options = {}) {
    const dir = mkdtempSync(join(tmpdir(), 'cu-oc-'));
    const file = options.path || join(dir, 'shot.png');
    const a = ['screenshot', file];
    if (options.fullPage) a.push('--full');
    await cu(a);
    const b64 = readFileSync(file).toString('base64');
    rmSync(dir, { recursive: true, force: true });
    return b64;
  }
  async tabs() {
    const data = await cu(['tab', 'list']);
    return data?.tabs ?? [];
  }
  async selectTab(target) {
    await cu(['tab', String(target)]);
  }
  async newTab(url) {
    const data = await cu(url ? ['tab', 'new', url] : ['tab', 'new']);
    return data?.tabId;
  }
  async closeTab(target) {
    await cu(target == null ? ['tab', 'close'] : ['tab', 'close', String(target)]);
  }
  async getCurrentUrl() {
    const data = await cu(['get', 'url']);
    return data?.url ?? null;
  }
  async setFileInput(files, selector) {
    await cu(['upload', selector || 'input[type=file]', ...files]);
  }
  async insertText(text) {
    await this.evaluate((t) => document.execCommand('insertText', false, t), text);
  }
}

function hostMatches(url, domain) {
  try { const h = new URL(url).hostname; return h === domain || h.endsWith('.' + domain); } catch (_) { return false; }
}

function emit(obj) {
  process.stdout.write(JSON.stringify(obj) + '\n');
}

try {
  await import(pathToFileURL(join(req.pkgDir, 'clis', req.modulePath)).href);
  const cmd = getRegistry().get(`${req.site}/${req.name}`);
  if (!cmd) throw new Error(`opencli: ${req.site}/${req.name} is not defined in ${req.modulePath}`);
  const kwargs = coerceAndValidateArgs(cmd.args ?? [], req.kwargs ?? {});
  cmd.validateArgs?.(kwargs);
  const page = cmd.browser === false ? null : new ChromeUsePage();
  if (page && cmd.navigateBefore !== false) {
    const target = typeof cmd.navigateBefore === 'string'
      ? cmd.navigateBefore
      : cmd.domain ? `https://${cmd.domain}` : null;
    const current = target ? await page.getCurrentUrl().catch(() => null) : null;
    if (target && !(cmd.domain && hostMatches(current, cmd.domain))) await page.goto(target);
  }
  let result;
  if (typeof cmd.func === 'function') result = await cmd.func(page, kwargs);
  else if (Array.isArray(cmd.pipeline)) result = await executePipeline(page, cmd.pipeline, { args: kwargs });
  else throw new Error(`opencli: ${req.site}/${req.name} has neither func nor pipeline`);
  emit({ success: true, data: result ?? null });
} catch (e) {
  emit({ success: false, error: e?.message || String(e), hint: e?.hint, code: e?.code });
  process.exitCode = 1;
}

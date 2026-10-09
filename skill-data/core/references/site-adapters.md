# Site adapters — the cheapest path for "read structured data from site X"

Before you `open` + `snapshot` + click your way through GitHub/Reddit/Bilibili/etc.,
check whether a **site adapter** already exists. An adapter is a community-written JS
function that hits the site's own JSON API *from inside your logged-in tab* and returns
clean structured data — no clicking, no scraping, no screenshots. It's the same idea as
`eval`, packaged per-site.

```bash
chrome-use site update                       # one-time: fetch community + official packs
chrome-use site list                         # what's installed (github/issues, reddit/search, …)
chrome-use site info github/issues           # an adapter's args + which domain it runs on
chrome-use site github/issues owner/repo --json   # run it → JSON (navigates there for you)
```

- Positional args fill the adapter's declared args **in order**; `--key value` overrides by name.
  If a `--key` name collides with a reserved global flag (`--state`, `--profile`, `--session`, …)
  it is consumed globally and never reaches the adapter — the CLI warns, and you should pass it
  **positionally** or after `--`: `chrome-use site demo/pr-list -- --state closed`.
- It navigates to the adapter's domain (reusing the current tab if you're already on it), so
  login-gated feeds (`bilibili/feed`, `twitter/...`) work because they run as *you*.
- If no adapter fits, fall back to the normal `snapshot`/`eval` loop. chrome-use fetches and
  runs two default sources: the official [chrome-use-sites](https://github.com/leeguooooo/chrome-use-sites)
  pack and the [bb-sites](https://github.com/epiral/bb-sites) community pack. On a shared
  `name/cmd` the official one wins.
- **OpenCLI commands work too.** When Node.js 20+ is on PATH, `site update` also installs
  [OpenCLI](https://github.com/jackwener/OpenCLI) (~1,300 commands over ~180 sites). A
  `name/cmd` that neither pack has runs through OpenCLI's own runtime, driving this same
  session. They show as `(opencli)` in `site list`, `site info` shows their args, and they come
  last in the `siteAdapters` hint. If one of our adapters fails and OpenCLI has a read command
  of the same name, chrome-use runs that instead (stderr says so; `--json` adds
  `source: "opencli"` and `fallbackFrom`). Writes never retry. Same command, same JSON: `chrome-use site hackernews/best
  --limit 5 --json`. `AGENT_BROWSER_SITES_NO_OPENCLI=1` turns them off.

> **Auto-trigger — act on it.** chrome-use keeps both packs synced automatically (first use +
> weekly), and whenever you reach a page whose domain has adapters it tells you: on every
> `open`/`navigate`/`snapshot`, and on any other command that lands you on a different site
> (`tab new`/`tab <id>`, `back`/`forward`, a click that navigates, the first command on a tab you
> didn't open). It prints a `site adapters for <domain>` line on stderr and adds a
> `siteAdapters: {domain, commands}` field in `--json`. A session that stays on one site hears
> about it once, not on every command. **When you see that, prefer the listed `site <name>/<cmd>` over snapshot+click
> for reading data** — it's the cheaper, more reliable path and it's already installed. You don't
> need to run `site update` yourself; just use the command it names. (Only on a brand-new setup
> where the packs haven't been fetched yet, a named `site <name>/<cmd>` may say it's not installed —
> run `site update` once, then re-run the command.)

## When a site you keep driving has no adapter

If you work on the same site a lot and no adapter covers it, chrome-use adds
`siteAdapterSuggestion: {domain, actionsThisSession, daysUsed, message}` to one response (stderr:
`site adapter suggestion: …`). It comes once per site per session and at most every two weeks.
**Ask the user** whether to turn the steps you keep repeating there into an adapter. Don't write
one without a yes. If they agree:

1. First check `chrome-use site list | grep <site>`: OpenCLI may already cover it. Otherwise
   do the action that loads the data (search, scroll, open the list), then run
   `chrome-use site analyze`. It lists the API calls the page made, the state it embeds
   (`__NEXT_DATA__`, `__INITIAL_STATE__`, …) and any anti-bot vendor, and picks a strategy.
   Prefer, in this order: a public API; the site's own JSON API called from the page
   (`fetch(url, {credentials: 'include'})`); embedded page state; the DOM. Each step down breaks
   more often. Write `~/.chrome-use/my-sites/<name>/<cmd>.js` in the format above (`@meta` with
   `name`, `description`, `domain`, `args`, `readOnly`; then the `async function`).
2. Register the folder once and sync: `chrome-use site add ~/.chrome-use/my-sites`, then
   `chrome-use site update`. Keep your own adapters in that folder, not in `~/.chrome-use/sites`,
   because a sync rewrites `~/.chrome-use/sites`.
3. Run it: `chrome-use site <name>/<cmd> --json`. When the result looks right, record it:
   `chrome-use site verify <name>/<cmd> [args] --write-fixture`. Later, `site verify <name>/<cmd>`
   fails (exit 1) when a field disappears, changes type, or a list comes back empty, so a broken
   adapter shows up before you trust its output. The fixture keeps the shape only, never values. If it's generally useful, offer to send it to
   [chrome-use-sites](https://github.com/leeguooooo/chrome-use-sites). That opens a PR from the
   user's account, so ask before you do it.

`AGENT_BROWSER_SITES_NO_SUGGEST=1` turns the suggestion off.

## When the site is not signed in

An adapter that finds the site signed out (its API says 401, the page has no
user) returns `loginRequired: true`, so chrome-use treats it as a login wall
and not as an ordinary failure:

```js
const r = await fetch('/api/me', { credentials: 'include' });
if (r.status === 401) return { error: 'login_required', loginRequired: true, loginUrl: '/login' };
```

- `loginUrl` is optional: the site's sign-in page, absolute or relative to the
  page. `auth login --bwu` fills the page it is on, so give it when the site
  does not redirect there by itself.
- `error: "login_required"` or `"not_logged_in"` (or `"Not logged in"`) alone
  means the same, for adapters written before this.
- Without either, a run that **failed** still counts as signed out when one of
  the adapter's `fetch` calls got HTTP 401 or was redirected to a sign-in URL,
  or the tab ended on a sign-in page. A successful result is never changed.
  (`window.fetch` and XHR are not watched; call plain `fetch`.)
- A write adapter must check the login before it writes, and say so; do not
  report "not saved" for a request the site refused for want of a login.

What the caller sees: the command fails with `login wall: <host> is not
signed in (site <name>/<cmd> …)` on stderr and `loginWall: {source: "site",
spec, host, url, returnTo, loginUrl, evidence, rerunnable, hint}` in `--json`
(`evidence.source` is `adapter`, `http401`, `redirect` or `page`). When the
user has not decided for that host, `loginWall.ask` holds a question to relay
to them and one command per answer (sign in once, `auth autologin always
<host>`, `auth autologin never <host>`): **ask the user, never choose for
them** (`core/authentication`). With `always` stored for the host, chrome-use
signs in by itself and runs the command once more, adding
`loginWall.autoLogin` and `loginWall.rerun: {ok, error}`. It reruns a write only when the adapter
reported the login itself (`rerunnable: true`): a write whose failure was
inferred from a 401 or a redirect may have half-run, so it signs in and stops.

## Long text, local files, long runs

Write adapters (publish an article, upload a video) take one command each:

```bash
# Long text from a file or stdin instead of "$(cat post.md)"
chrome-use site csdn/article-publish --title "Hello" --markdown @post.md
cat post.md | chrome-use site csdn/article-publish --title "Hello" --markdown @-
chrome-use site csdn/article-publish --title "Hello" --markdown-file post.md

# A local file for an arg the adapter declares as "type": "file" (see `site info`)
chrome-use site douyin-creator/video-publish --video ./clip.mp4 --title "Hello"

# Keep going until the upload/publish is really done
chrome-use site bilibili-creator/video-publish --title "Hello" --tags a,b --until-done --timeout 10m
```

- `--key @path` reads the value from a file, `--key @-` from stdin, and `--key-file path`
  is the same thing spelled out. `@name` that is not a file stays literal (`--user @jack`);
  `@x.md` or `@dir/x` that does not exist is an error, so a typo is never posted as text.
  `--key \@text` passes a literal leading `@`.
- A `"type": "file"` arg takes a local path. The adapter attaches it to the page's file input
  itself, so there is no separate `upload` step and no selector to know. If `site info` shows no
  file arg, the adapter still expects a prior `chrome-use upload`.
- An adapter is not bound by the ~8s budget of one relay command: it runs in the page's
  background and is polled. `--timeout <300|90s|10m>` sets the total time (default: the
  adapter's `@meta.timeout`, else 120s).
- `--until-done` reruns the adapter while it returns `status: "incomplete"` / `"uploading"`, or
  when a page navigation ended the run, until it finishes or the timeout passes (default 600s).
  Without it you get that status back and rerun the same command yourself. Do not wrap the
  command in your own retry loop.
- A `status: "timeout"` error means the adapter was still running when time ran out. The page
  may still finish; check it before rerunning a publish.
- Progress the adapter reports is printed to stderr and returned as `progress` in `--json`,
  with `attempts` and `elapsedMs`.

### Writing an adapter that uses these

```js
/* @meta
{
  "name": "demo/video-publish",
  "domain": "creator.example.com",
  "timeout": 600,
  "args": {
    "title": {"required": true},
    "video": {"required": true, "type": "file", "input": "input[type=file]"}
  }
}
*/
async function(args) {
  args.progress('uploading ' + args.video.name);      // {path, name, size, setOn}
  await args.video.setOn();                           // or setOn('#other-input')
  // ... wait for the upload, fill the form; args.budgetMs is the time this run has
  return { status: 'submitted' };
}
```

`setOn` resolves once the file is on the input (the same mechanism as `chrome-use upload`). The
page only names the selector; it can never choose the path. A navigation ends the run, so
return `status: "incomplete"` before a step that navigates, or keep each step repeatable and
let `--until-done` rerun it. `"retryStatuses": [...]` in `@meta` replaces the default
`["incomplete", "uploading"]`.

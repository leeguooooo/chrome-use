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
  runs two default sources: the [bb-sites](https://github.com/epiral/bb-sites) community pack
  and the official [chrome-use-sites](https://github.com/leeguooooo/chrome-use-sites) pack.

> **Auto-trigger — act on it.** chrome-use keeps both packs synced automatically (first use +
> weekly), and when you `open`/`navigate`/`snapshot` a page whose domain has adapters it tells
> you: a `site adapters for <domain>` line on stderr, and a `siteAdapters: {domain, commands}`
> field in `--json`. **When you see that, prefer the listed `site <name>/<cmd>` over snapshot+click
> for reading data** — it's the cheaper, more reliable path and it's already installed. You don't
> need to run `site update` yourself; just use the command it names. (Only on a brand-new setup
> where the packs haven't been fetched yet, a named `site <name>/<cmd>` may say it's not installed —
> run `site update` once, then re-run the command.)

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

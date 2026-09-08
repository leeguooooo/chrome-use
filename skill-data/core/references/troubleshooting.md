# Troubleshooting

**"Ref not found" / "Element not found: @eN"**
Page changed since the snapshot. Run `chrome-use snapshot -i` again,
then use the new refs.

**"Unknown ref @eN"**
The error names the session that answered and says whether it holds any refs.
"no snapshot has run in this session" means the command reached a different
session than the one you snapshotted (another directory or terminal): run
`chrome-use session list` and pin with `--session <name>`. If it lists a ref
range instead, the page changed; re-snapshot. Sessions are now reused across
`cd` for the same agent tag, so this mostly shows up with a different terminal.

**Element exists in the DOM but not in the snapshot**
It's probably off-screen or not yet rendered. Try:

```bash
chrome-use scroll down 1000
chrome-use snapshot -i
# or
chrome-use wait --text "..."
chrome-use snapshot -i
```

**Click does nothing / overlay swallows the click**
Some modals and cookie banners block other clicks. Snapshot, find the
dismiss/close button, click it, then re-snapshot.

**`stale sessionId … re-open your target URL` (extension-relay mode)**
Your tab may have been closed, navigated across processes, or its debugger detached
(e.g. it landed on a `chrome://` or Chrome Web Store page, which Chrome
forbids debugging). The session no longer has a live tab — re-run
`chrome-use open <your URL>` to re-attach, then retry. This loud error
replaces the old silent behaviour where the command ran on some *other*
tab and returned wrong data.

To recover, you need the tab's **exact** URL (query params and all — a long
SSO/redirect link breaks if truncated). `tab list` shortens long URLs with
`…`; use **`tab list --full`** to print them untruncated, then re-`open` the
right one. For multi-redirect SSO flows, re-open the **stable entry URL**
(not the mid-redirect one) and `wait` a few seconds for the SPA to settle
before snapshotting.

If the tab is still present but the page is white or frozen, do not reopen it
and destroy the diagnostic state. Select it only when `tab list` marks it
`created` or `adopted`; otherwise inspect its browser metadata directly with
`tab inspect <targetId>`, and use `tab adopt <url-substring|targetId>` before
selecting or driving it. A relay timeout means the renderer did not answer; it
does not mean the tab disappeared.

For an explicit `open`/`navigate`, ab-connect 0.5.18 and newer recover a
renderer-scoped `Page.navigate` timeout through Chrome's browser-level tab API.
The command returns a warning but keeps the same session and tab. Other methods
such as `eval` still fail while the page main thread is blocked. On reconnect,
the extension also validates every attached Chrome tab before re-announcing it,
so dead bootstrap `about:blank` records are dropped instead of becoming active.

If `tab select` reports that the liveness probe did not complete, read the full
warning before judging the renderer. An outdated or unknown ab-connect version
can make the probe channel unavailable. `tab inspect` requires ab-connect
0.5.16 or newer; update or reload it from `chrome://extensions` and retry.

**Reads landing on the wrong page**
`eval`, `screenshot`, and `network requests` print the page they ran
against to stderr: `eval @ <url>`, `screenshot @ <url>`, `network @ <url>`.
If that URL isn't the page you expected (the active tab drifted), re-`open`
your target URL — don't trust the result. Treat the stamp as a built-in
sanity check on every read.

`screenshot` also checks the pixels it just wrote. If the image is a single
flat colour while the page reports real layout and text, the capture is
reported with `⚠` and a warning instead of a plain `✓` — the shot is still
saved, but don't feed it to a vision model without confirming the target
first (`chrome-use eval "location.href"`, then re-pin with `tab` / `adopt`).
A page that genuinely is one colour never triggers this, and `--selector`,
`--clip` and `--annotate` captures are exempt.

**Fill / type doesn't work**
Some custom input components intercept key events. Try:

```bash
chrome-use focus @e1
chrome-use keyboard inserttext "text"    # bypasses key events
# or
chrome-use keyboard type "text"          # raw keystrokes, no selector
```

**Page needs JS you can't get right in one shot**
Use `eval --stdin` with a heredoc instead of inline:

```bash
cat <<'EOF' | chrome-use eval --stdin
// Complex script with quotes, backticks, whatever
document.querySelectorAll('[data-id]').length
EOF
```

**Cross-origin iframe not accessible**
Cross-origin iframes that block accessibility tree access are silently
skipped. Use `frame "#iframe"` to switch into them explicitly if the
parent opts in, otherwise the iframe's contents aren't available via
snapshot — fall back to `eval` in the iframe's origin or use the
`--headers` flag to satisfy CORS.

**Authentication expires mid-workflow**
Use `--session-name <name>` or `state save`/`state load` so your session
survives browser restarts. See [references/session-management.md](references/session-management.md)
and [references/authentication.md](references/authentication.md).

## "the tab this command was driving is gone"

`tab list` may still show the tab, and `tab inspect` may still read it, while
the session cannot drive it. Those are different paths.

`tab select` / `tab adopt` now report which of three things happened, in a
`driving` field:

- **confirmed** — the session evaluated on that tab, and `driving.url` is where
  it landed. This is the only case that prints ✓.
- **failed** — the session answered from a different origin. The command errors
  and names both urls.
- **not confirmed** — the session did not answer at all. The command prints ⚠,
  not ✓, because this is neither outcome. **Do not treat it as recovered.**

On ⚠, run one read. If that also fails, stop retrying `tab select` — it cannot
recover a session pinned to a page it can no longer leave. Re-open the target
with `open <url>` / `navigate <url>` to rebind, then re-`snapshot`.

Retrying the recovery command after a ⚠ is the loop this reporting exists to
break: the old behaviour printed ✓ with the requested tab's title, so the next
command failed identically and the obvious response was to "recover" again.

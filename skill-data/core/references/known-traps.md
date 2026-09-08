# Known traps (date them)
- 2026-06-05: @ref to the basket button goes stale after the mini-cart opens;
  re-snapshot or use `find role button --name "Checkout"`.
```

This is how repeat visits get fast and reliable instead of re-solving the same
page every time.

### Extract data

```bash
# Structured snapshot (best for AI reasoning over page content)
chrome-use snapshot -i --json > page.json

# Targeted extraction with refs
chrome-use snapshot -i
chrome-use get text @e5
chrome-use get attr @e10 href

# Arbitrary shape via JavaScript
cat <<'EOF' | chrome-use eval --stdin
const rows = document.querySelectorAll("table tbody tr");
Array.from(rows).map(r => ({
  name: r.cells[0].innerText,
  price: r.cells[1].innerText,
}));
EOF
```

Prefer `eval --stdin` (heredoc), `eval --file <path>`, or `eval -b <base64>`
for any JS with quotes, **non-ASCII identifiers/strings (e.g. Chinese)**, or
large scripts — inline `chrome-use eval "..."` is shell-mangled and works
only for simple ASCII expressions.

**`eval` runs in the page's MAIN world and state persists across calls**, so a
top-level `const x`/`let x`/`var x` in one call collides with the next
(`SyntaxError: Identifier 'x' has already been declared`). Either use unique
names, assign to `window.x`, or wrap the body in an IIFE
(`(() => { const x = …; return x; })()`).

**For array/object results, use `eval --json`** — the plain renderer
pretty-prints across multiple lines, which `tail`/`head`/pipes mangle; `--json`
emits one parseable line. Also note **`type`/`fill` insert text without firing
`keydown`/`keyup`** (CDP insertText) — the value lands, but a page that gates on
key events (some search-as-you-type widgets) won't react; use `keyboard type` (or
`press` per key) when real keystrokes are required.

### Screenshot

```bash
chrome-use screenshot                        # temp path, printed on stdout
chrome-use screenshot page.png               # specific path
chrome-use screenshot --full full.png        # full scroll height
chrome-use screenshot --annotate map.png     # numbered labels + legend keyed to snapshot refs
```

Headless Chromium screenshots hide native scrollbars for consistent image output.
Pass `--hide-scrollbars false` when launching to keep native scrollbars visible.

`--annotate` is designed for multimodal models: each label `[N]` maps to ref `@eN`.
It refreshes annotations as another snapshot of the same document, so it does
not invalidate refs from the snapshot immediately before it.

### Handle multiple pages via tabs

```bash
chrome-use tab                      # list open tabs (with stable tabId)
chrome-use tabs                     # alias for `tab` (lists too)
chrome-use tab new https://docs...  # open a new tab (and switch to it)
chrome-use tab duplicate            # native Duplicate tab; copy becomes the internal active tab
chrome-use tab duplicate docs --label docs-copy
chrome-use tab t2                   # switch to tab t2
chrome-use tab select t2            # explicit switch syntax
chrome-use tab adopt "example.com/stuck" # attach an existing tab without navigation
chrome-use tab inspect t2           # browser metadata; no page JavaScript
chrome-use tab close t2             # close tab t2
```

On external or extension-connected Chrome, `tab list` marks every row as
`created`, `adopted`, or `foreign`. A session may select only created or adopted
tabs and may close only created tabs. Use `tab adopt <url-substring|targetId>`
before driving an existing tab; adoption never transfers permission to close it.
Created ownership survives daemon restarts for the same named session and
connected browser endpoint, allowing interrupted cleanup to resume without
making adopted tabs closable.

(`tabs` → the `tab` subcommand tree, and `get-text <sel>` → `get text <sel>` —
common-guess aliases so you don't waste a round on the wrong spelling.)

Tab ids are stable strings (`t1`, `t2`, …), never reused within a session, so
the same id keeps referring to the same tab across commands. Positional
integers are **not** accepted — use `t2`, not `2`. After switching, refs from a
prior snapshot on a different tab no longer apply — re-snapshot.

For a white-screen or unresponsive existing tab, use `tab adopt
<url-substring|targetId>` before `tab select <ref>` to preserve the page rather
than reopening it. `tab inspect <ref>` reads browser-level URL/status,
discard/freeze state, and debugger attachment without evaluating page
JavaScript. A renderer blocked by an infinite JavaScript loop cannot complete
`eval`, but it remains attached and is reported as unresponsive rather than
gone.

On extension-connected Chrome, `tab inspect` requires ab-connect 0.5.16 or
newer. A failed liveness probe is not proof that the renderer is unresponsive.
If the warning says the live extension is behind the bundled version, open
`chrome://extensions`, update or reload ab-connect, and retry before diagnosing
the page.

`tab duplicate [ref] [--label <name>]` is available only through the
extension-connected real Chrome path. It calls Chrome's native Duplicate tab
operation, restores the previously visible foreground tab, and keeps the copy as
chrome-use's internal active tab. It never recreates the source URL as a
fallback. Chrome may still load the duplicate while it is in the background.

### Run multiple browsers in parallel / reset stuck daemons

Each `--session <name>` is an isolated browser (own cookies, tabs, refs), and
concurrent agents MUST each use a distinct one; `AGENT_BROWSER_SESSION=myapp` sets
the shell default. Codex tasks get distinct defaults automatically through
`CODEX_THREAD_ID`; other runners can set `AGENT_BROWSER_SESSION_ID`. True
multi-agent isolation needs the **extension-connect path**
(per-session tab groups) — raw `--cdp <port>` does NOT isolate. Reach a specific tab
across sessions via its stable CDP `targetId` (`tab list --full` →
`tab adopt <targetId>`, without reload); `--reuse-tab` avoids duplicate tabs on
rebind.

Reset stuck state with `chrome-use daemon status` / `daemon restart` — restarts the
session daemon workers without touching the relay or closing any tabs.
Use `chrome-use status` first for a daemon-free snapshot of the CLI, extension
relay/profile, native-host launcher health (`extension.hostHealthy` in JSON),
and current session. Relay debugger requests are bounded, and a silent stale
worker is stopped automatically after its socket deadline. If a registered
daemon is still alive but its local endpoint has disappeared, the next browser
command reclaims it and starts a clean replacement for the same session. If the
endpoint disappears during an in-flight command, rerun that command after the
CLI clears the stale state.

Full detail: `chrome-use skills get sessions`

### Downloads

In an ab-connect 0.5.13+ session, `download <selector|@ref> <path>` resolves an
HTTP(S) anchor's URL and uses Chrome's downloads API instead of clicking it.
This prevents cross-origin media links from navigating the active tab and works
for dynamically-created anchors visible in `snapshot -i`.

```bash
chrome-use download @e2 ./video.mp4
chrome-use download-url "https://example.com/report.pdf" ./report.pdf
chrome-use downloads --limit 10 --json
chrome-use downloads --clear
```

Omit the `download-url` path to keep Chrome's normal download location.
`downloads --clear` clears history only and never deletes files.

### Local HTTP API

The session stream port also serves a versioned localhost API. Discover it with
`stream status --json`; `GET /api/v1/status`, `/api/v1/tabs`, and
`/api/v1/sessions` are read endpoints. `POST /api/v1/command` accepts the same
daemon command object used internally by CLI and MCP.

Versioned reads require a loopback Host and reject mismatched browser origins.
Command POSTs additionally require matching Origin or Referer. Errors across
CLI, MCP, and HTTP include stable `code` and `retryable` fields in addition to
`success` and `error`.

### Mock responses & rewrite requests

`network route <glob>` intercepts matching requests via the CDP Fetch domain (no
proxy, no extension permission). Three modes: **mock** the response
(`--body`/`--status`/`--header`/`--content-type` — short-circuits the request),
**rewrite** the outgoing request (`--method`/`--set-body`/`--set-header`/`--rewrite-url`),
or **edit** the real response (`--edit-status`/`--edit-header`/`--replace 'from=>to'`);
`--abort` blocks entirely. Scope with `--resource-type xhr,fetch`; inspect/record via
`network requests` and `network har start|stop`; filter WebSocket connections with
`network requests --type websocket`. Connection metadata is recorded, not frame payloads.
`network unroute` drops all routes.

Full detail: `chrome-use skills get network`

### Record a video of the workflow

Extension-connected real Chrome records the current session-owned tab in
place. A locally launched browser still records in a fresh isolated context.

```bash
chrome-use record start demo.webm
chrome-use open https://example.com
chrome-use snapshot -i
chrome-use click @e3
chrome-use record stop
```

See [references/video-recording.md](references/video-recording.md) for
codec options, GIF export, and more.

### Iframes

Iframes are auto-inlined in the snapshot — their refs work transparently:

```bash
chrome-use snapshot -i
# @e3 [Iframe] "payment-frame"
#   @e4 [input] "Card number"
#   @e5 [button] "Pay"

chrome-use fill @e4 "4111111111111111"
chrome-use click @e5
```

To scope a snapshot to an iframe (for focus or deep nesting):

```bash
chrome-use frame @e3      # switch context to the iframe
chrome-use snapshot -i
chrome-use frame main     # back to main frame
```

### Viewport / window size (responsive & overflow debugging)

To reproduce width-dependent bugs (responsive breakpoints, horizontal-overflow
hunts, mobile layouts) set the viewport. This is a **CDP virtual viewport**
(`Emulation.setDeviceMetricsOverride`) — it changes the layout viewport *for the
tab* without physically resizing the OS window, so it works headless **and** over
the extension relay without yanking the user's real Chrome window around.

```bash
chrome-use viewport 1280 800        # set width x height (alias: resize)
chrome-use viewport 375x812         # WxH shorthand
chrome-use viewport 375 812 --dpr 3 --mobile   # retina + mobile emulation
chrome-use viewport reset           # clear the override, restore real size
```

```bash
# Find what's overflowing at a narrow width:
chrome-use viewport 375 812
chrome-use eval 'document.documentElement.scrollWidth + " vs " + innerWidth'
```

`set viewport <w> <h> [scale]` is an equivalent alias.

### Dialogs

`alert` and `beforeunload` are auto-accepted so agents never block. For
`confirm` and `prompt`:

```bash
chrome-use dialog status          # is there a pending dialog?
chrome-use dialog accept           # accept
chrome-use dialog accept "text"    # accept with prompt input
chrome-use dialog dismiss          # cancel
```

A click that opens `confirm` or `prompt` returns immediately with a pending
dialog result. The same session can then run `dialog status` and
`dialog accept|dismiss`; do not restart the daemon or override the page's dialog
functions.

<!-- full -->

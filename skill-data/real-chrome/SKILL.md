---
name: real-chrome
description: Drive the user's real, already-open logged-in Chrome via the chrome-use extension + native messaging relay (no debug port). Use for: connecting to the user's live Chrome, multi-profile selection (browsers/--browser), relay-drop recovery, OAuth/SSO cross-process handoff, strict multi-agent tab-group isolation, adopting an existing tab, anti-detection ranking, silent background operation, --humanize behavioural stealth, and Cloudflare cf-status. Load when the task needs the user's real session rather than a launched browser.
allowed-tools: Bash(chrome-use:*), Bash(chrome-use:*), Bash(abs:*), Bash(npx chrome-use:*), Bash(npx chrome-use:*)
---

# chrome-use — Real Chrome (Extension)

Drive the user's *live*, already-open, logged-in Chrome window through the extension-connect relay instead of a freshly launched browser.

## Driving the user's real, already-open Chrome (extension)

When the task needs the user's *live* logged-in window (their real session, the
window they're looking at — not a fresh browser), use the extension connect flow.
One-time setup:
1. `chrome-use extension install` — registers the native-messaging host.
2. Install the **chrome-use** extension. Easiest (and restart-stable):
   the **Chrome Web Store**, one-click *Add to Chrome*:
   <https://chromewebstore.google.com/detail/chrome-use/knfcmbamhjmaonkfnjhldjedeobeafmk>
   (Dev fallback: `chrome://extensions` → Developer mode → *Load unpacked* →
   `extensions/ab-connect`. Load-unpacked can be disabled on Chrome restart, so
   prefer the Store build for unattended setups.)

Once installed, plain `chrome-use open <url>` auto-connects through the
extension relay — `auto_connect_cdp` **prefers the live relay over a raw
`--remote-debugging-port`**, so Chrome 136+'s "Allow remote debugging?" consent
popup never fires. `chrome-use extension connect` is the explicit form of the
same path. Zero-confirmation, zero-token. Use `--launch` instead when a fresh,
isolated browser is fine.

> **Many Chrome profiles? See which one — and pick it.** The relay binds to
> whichever profile's extension worker is talking to the native host. If a site
> comes back logged out, confirm it's the right profile before assuming the login
> is gone:
> - `chrome-use browsers` — lists every connected profile (email if the ext has
>   the optional `identity` permission, else a stable id; marks the default).
> - `chrome-use --browser <id|email> <cmd>` — pins THIS session to that profile.
>   It's **sticky per session** (the session's daemon binds on first connect), and
>   each session can pick a different profile, so concurrent agents don't fight.
> - `chrome-use extension status` / `doctor` also report the current driving profile.
>
> A logged-out result on the *wrong* profile is not a missing login — run
> `browsers`, then re-run with `--browser <the right one>`.
>
> **Many ACCOUNTS on one site? Verify identity, don't assume it.** Profile
> selection picks the browser; `whoami`/`--as` pick the *account* (backed by the
> [cookie-use](https://github.com/leeguooooo/cookie-use) vault, hash-compare only —
> no stored cookie values are read):
> - `chrome-use whoami [site]` — which vault account each site's live session is.
> - `chrome-use --as <site/acct> <cmd>` — verifies BEFORE acting; on mismatch
>   auto-applies that account's stored session and re-verifies. `--as-strict`
>   fails (exit 1) instead of switching. Re-verified on every invocation —
>   sessions drift (logouts, other agents, account choosers).

`--launch` opens an **isolated, empty test profile** — no cookies, no login, no
extensions (so the extension-relay path is off). Its window is labelled
`chrome-use (<session>)` in Chrome's profile menu so a human watching the
desktop knows which session owns it. If a launched session needs more:

- **Real cookies / login / extensions** → drop `--launch`, use `--profile auto`
  (reuses the user's real Chrome profile), or set `AGENT_BROWSER_PROFILE=auto`
  once so every call does it by default.
- **A specific unpacked extension in the test profile** →
  `--launch --args "--load-extension=<dir>"`.

**If you DO hit the "Allow remote debugging?" dialog**, don't keep retrying (every
attempt re-pops it). One of two things is true:

1. **You're on a stale build.** The relay-preference that avoids this dialog has
   shipped for many releases — if you still hit it, your `chrome-use` is old.
   Upgrade and retry:
   ```bash
   curl -fsSL https://raw.githubusercontent.com/leeguooooo/chrome-use/main/install.sh | sh
   ```
   If `which -a chrome-use` shows more than one install, an old **npm/pnpm**
   copy (the npm registry lags behind — GitHub Releases are the source of truth)
   may be shadowing the upgraded one; remove the stale copy
   (`npm rm -g chrome-use` / `pnpm rm -g chrome-use`) so the `install.sh` build
   wins. A tool that bundles its *own* pinned copy needs that copy upgraded too.
2. **The extension/relay isn't live.** Tell the user to install the Store
   extension (one click, above); after that the relay stays up and the dialog
   never returns.

**If the relay drops mid-session** (a command suddenly errors with "couldn't
reach your Chrome" / "relay … failed" — usually the MV3 service worker got
suspended, or two agents are sharing one relay): **the CLI now self-heals** — it
waits for the worker's keepalive to revive the relay (~25s) and retries once, so
most drops you never even see. If a command *does* surface the error:

1. **Just retry the command** — the relay has usually reconnected by then.
2. If it persists: `chrome-use status` (check), then `chrome-use extension connect`
   (re-attach), then retry. The CLI auto-registers the native host and opens the
   Web Store page itself if the extension was never set up — you don't run
   `extension install` by hand.
3. **Never** tell the user to quit/restart Chrome with `--remote-debugging-port`
   to recover a dropped relay — that throws away their tabs and defeats the
   extension path. (The old error text said this; it no longer does.)
4. `chrome-use reconnect` is a friendly alias for `extension connect` — re-binds
   the session to the running Chrome without any reinstall.

> **Don't serialize agents to dodge fragility.** Relay drops are transient and
> self-heal; they are *not* a sign that the shared browser can't be driven
> concurrently. Concurrent multi-agent **is** supported — each `--session` gets
> its own tab group and drives only its own tabs (see *Strict multi-agent
> isolation* below). Give each agent a distinct `--session` and let them run in
> parallel; you don't need to run them one at a time.
5. **Can't restore the browser at all?** Don't stall waiting for a screenshot —
   **fall back to non-visual verification**: `get text` / `eval` / read the
   deployed page over `curl`/`WebFetch`. Confirming a change via the DOM/HTML or
   the live URL is a *correct* result, not a failure. Reserve screenshots for a
   genuine visual check you report to the user.

> **OAuth/SSO popups and redirects now just work.** A login handoff
> (GitHub/Google OAuth, SSO) navigates the tab *across processes*, which used to
> orphan the old session and make the next read fail "stale sessionId". The relay
> now **auto-reattaches the opener across that cross-process nav** and retries a
> read briefly while the new process settles, so reads
> (`snapshot`/`screenshot`/`eval`/`get`) keep working through the handoff and
> "Sign in with Google" / OAuth-popup logins complete end-to-end. (The
> cross-origin Google GSI iframe — the "Continue as <user>" button — is pierced
> only on Chrome 125+; on older Chrome that one button may stay unreachable.)

Each `--session` that connects gets its **own colored Chrome tab group** (named
after the session) and drives only its own tabs — multiple agents share the one
real browser without cross-talk, and the user's own tabs are never grouped. CDP
drives the page without moving the user's mouse/keyboard, so it doesn't fight
them for control.

> **Per-agent isolation is automatic.** The session *name* is the isolation key:
> it maps to a dedicated tab group **and a dedicated daemon** (the port is derived
> from the name). When you don't pass `--session` / `AGENT_BROWSER_SESSION`,
> chrome-use now **auto-derives a per-agent default** — `cu-<repo>-<id>`, where
> `<id>` is a short hash of a stable per-agent terminal/agent env id
> (`CMUX_SURFACE_ID`, `TERM_SESSION_ID`, `ITERM_SESSION_ID`, `TMUX_PANE`, … or
> `AGENT_BROWSER_SESSION_ID` if a runner injects one). So **two agents in the same
> repo get different tab groups by default** and no longer stomp each other's
> active tab. The name is stable across that one agent's commands and unique per
> agent. In a plain shell with none of those env vars it falls back to the shared
> `default`. Override anytime: pass `--session <name>` / `AGENT_BROWSER_SESSION` to
> pick an explicit name, or set them to the **same** value across agents to make
> them deliberately *share* one tab group.

**Strict multi-agent isolation.** A session over the relay tracks and drives
**only the tabs it created** (its own group). A pop-up that **your own action
opened** — e.g. the OAuth account-chooser window from a "Sign in with Google"
click — is followed and drivable as part of your session (switch to it and drive
the chooser). But it does **not** adopt the user's existing tabs, other agents'
tabs, or *unrelated* pop-ups (a login window the user opened on their own is
theirs), so several agents (and other tools opening tabs) can work in the same
real Chrome concurrently without ever dropping or stealing each other's tabs —
another agent's tab churn can't make your bound tab vanish or drift your commands
onto the wrong page. Consequence: `tab list` shows only *your* session's tabs; to
drive a specific page, navigate to it in your own tab instead of expecting a
pre-existing or popped-up tab to appear in the list.

> **No debugger banner on the user's pages.** The extension attaches Chrome's
> debugger **only to tabs the agent owns** (ones it created, or that you `adopt`),
> never to the user's own tabs — so Chrome's "chrome-use started debugging this
> browser" bar never covers a page they're working in (it only appears on attached
> tabs, and the agent's are background tabs). `adopt <url>` attaches that one tab
> on demand (the bar then shows on *that* tab — it's the one you asked to drive).
> No Chrome restart, fully seamless.

> **Need to read a tab the user already has open?** Use `chrome-use adopt
> <url-substring|targetId>` — it finds that pre-existing tab (the user's own, or
> another session's) across groups and drives it **without opening a new tab**.
> e.g. `adopt "claude.ai/design"` then `snapshot`/`eval`/`get text` on it. On no
> match it errors and lists the tabs it can see. This is the explicit, opt-in way
> through the isolation above (it tags the adopted tab into your group). Great for
> "read/extract from the page I'm looking at" without disturbing it.

**Anti-detection ranking: this real logged-in Chrome (extension
connect) > a headed launched browser > headless (forbidden).** A genuine human
browser has no headless/automation tells at all, so prefer it for anything
anti-bot-sensitive.

**Silent by default.** When driving the user's real Chrome the agent works
entirely in the background — new tabs open un-focused, the agent never force-
fronts a tab, and focus is emulated so the page still renders and reports
`visibilityState: 'visible'`. You don't need to do anything; just don't expect
the user's view to follow you (use the explicit `bringToFront` only if you
deliberately want to surface a tab).

**Human-like input for behavioural anti-bot.** Beyond fingerprint stealth,
`--humanize off|fast|human` (or `AGENT_BROWSER_HUMANIZE`) makes clicks follow a
curved, decelerating path with in-element landing jitter, typing use variable
cadence, and scroll/drag ease. Default `off`; a per-navigation detector
auto-escalates pages guarded by Akamai/PerimeterX/DataDome to `human`. Leave it
on auto; force `human` only when you already know the target scores behaviour.

**Cloudflare clearance — solve once, reuse.** Passing a Cloudflare challenge
mints a `cf_clearance` cookie (HttpOnly — invisible to `eval`/`document.cookie`;
read it via `chrome-use cookies`). It's bound to your **IP + User-Agent**: reuse
the same exit IP and UA and you skip the challenge until it expires. Driving the
user's real Chrome (relay) persists it natively; for isolated sessions,
`--session-name <name>` save/restores it. Before spending effort solving, run
`chrome-use cf-status` (aliases `cf`, `clearance`): it reports whether the page
is *currently* a Cloudflare challenge and whether a still-valid `cf_clearance`
exists, with a recommendation — `proceed` (already cleared, don't re-solve),
`solve` (challenge up, no clearance), or `reissue` (clearance present but page
still blocks → IP/UA drifted, re-solve). Use it as a preflight to avoid
re-solving what you already cleared.

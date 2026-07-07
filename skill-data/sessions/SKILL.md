---
name: sessions
description: Run multiple isolated browser sessions in parallel (each --session = own cookies/tabs/refs), recover a stranded tab by CDP targetId across sessions, --reuse-tab, and reset stuck daemons (daemon status/restart). Use for multi-user flows, parallel scraping, or when a session's tab/handle got stuck.
allowed-tools: Bash(chrome-use:*), Bash(chrome-use:*), Bash(abs:*), Bash(npx chrome-use:*), Bash(npx chrome-use:*)
---

# chrome-use — Sessions & Parallel Browsers

Run several isolated browser sessions at once, adopt a stranded tab across sessions by its CDP targetId, and reset stuck session daemons.

## Run multiple browsers in parallel

Each `--session <name>` is an isolated browser with its own cookies, tabs,
and refs. Useful for testing multi-user flows or parallel scraping:

```bash
chrome-use --session a open https://app.example.com
chrome-use --session b open https://app.example.com
chrome-use --session a fill @e1 "alice@test.com"
chrome-use --session b fill @e1 "bob@test.com"
```

`AGENT_BROWSER_SESSION=myapp` sets the default session for the current
shell.

**Concurrent agents MUST each use a distinct `--session <name>`.** Within one
session, commands are pinned to the tab you opened (by target_id, so a foreign
tab can't drift your `eval`/`screenshot`). Two agents sharing the *same* session
(e.g. both on the bare default) share one daemon and one active tab and will
clobber each other.

True multi-agent isolation requires the **extension-connect path**: each
`--session` gets its own colored Chrome tab group, so sessions never touch each
other's tabs. **Raw `--cdp <port>` does NOT isolate** — every session attaches to
the same browser's existing targets, so a second session's first `open` can
navigate a sibling's tab. For concurrent agents on one real Chrome, use the
extension (each with a distinct `--session`), not raw `--cdp`.

Each session owns its own tab group and assigns its own `t<N>` indices (the same
physical tab is `t8` in one session, `t1` in another), so `t<N>` is **not** a
stable cross-session handle. To reach a *specific* tab from another session — e.g.
a tab that was filled in a session whose handle later died — use the **stable CDP
`targetId`**:

```bash
chrome-use tab list --full --session B   # re-syncs live tabs; prints `target: <id>` per row
chrome-use tab <targetId> --session B    # adopt that exact tab, NO reload (state preserved)
```

`tab list` re-discovers the live tab set on every call, so a fresh session sees
tabs other sessions opened (and re-attached ones), not just its own. Adopting by
`targetId` lands session B on the stranded tab without reloading it, so a
half-filled form survives. Still, the simplest recovery for a session whose own
tab died is to recover *that* session (reload / re-`open` / `daemon restart`).

To avoid piling up duplicate tabs when you re-`open` the same entry URL on
rebind, pass **`--reuse-tab`**: if a tab already shows that URL (matched by
origin+path), it switches to it instead of spawning a new one.

## Reset stuck daemon state

Each session runs a background daemon worker that holds the page handles. If a
session starts misbehaving — commands hit the wrong tab, refs/handles look stale,
or you upgraded `chrome-use` mid-session and old workers linger — restart the
daemons instead of hunting PIDs with `pgrep`/`kill`:

```bash
chrome-use daemon status     # list running session daemons (+ relay state)
chrome-use daemon restart    # kill every session daemon worker
```

`daemon restart` leaves the extension's native-messaging bridge (`__nm-host`)
alone, so the relay to your live Chrome stays up — the next command just spins up
a fresh, clean daemon against the same browser. It does **not** close any tabs.

## Hand a session to the human (rare escape hatch)

Autonomous login is the default — drive the login flow yourself. `session
handoff` is only for the one step the agent genuinely can't do (a device 2FA
prompt, a CAPTCHA the humanizer can't clear), not a login method.

```bash
chrome-use session handoff       # give THIS session to the user; agent driving commands refuse (exit 1)
chrome-use session status        # who owns each session (agent | user)
chrome-use session list          # all sessions + owners
chrome-use session resume        # take the session back once the human is done
```

While a session is handed off, its owner sidecar (`.owner`) flips to the user and
every driving command (`click`/`type`/`eval`/…) refuses with a loud exit-1 error
so the agent can't fight the human for the page. `resume` returns control. Ownership
is per-session, so other sessions keep working normally.

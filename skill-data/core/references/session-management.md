# Session Management

On the extension relay, sessions share one background agent window per Chrome
profile. Each session has its own tab group and references; cookies and login
state belong to the profile and are shared. Your own window stays separate.
Full screen does not change agent ownership. A lost window record is recovered
from verified agent tabs before creating a replacement; user tabs and unknown
blank pages are never moved or deleted to reclaim a window. Mixed user/agent
windows refuse new tabs until the user tabs are moved out, rather than creating
a second agent window. Failed placeholder
cleanup retains its identity and refuses success so the next call can retry.
`tab list --all` uses a passive connection even in a fresh session: no daemon,
window, tab or page attachment is created.

With ab-connect 0.5.50+, native-host disconnect schedules a first connection retry after 1 second.
Repeated failures back off through 2, 4, 8, 16 and at most 30 seconds; a
confirmed host resets the backoff. The Chrome alarm still recovers a restarted
worker. Command and event waits end when their socket closes, even if a session
or inspector still holds the old client. Old replies are not queued into a
replacement host; reconnect rebuilds current validated tab records.
Send failures and lost command replies report `action_outcome_unknown` with
`retryable: false`, because earlier parts of the operation may already have run.
For an interrupted write, inspect the page before deciding whether to repeat it.

If `close` remains incomplete because an old ownership record has only a
targetId and no tab identity, `close --forget-untracked` explicitly abandons
only untracked records omitted by two valid Chrome target-registry reads.
It never guesses a tab to close. JSON `ownershipDropped` names the records;
`verifiedAbsent: false` and `tabClosureUnverified: true` mean the original
Chrome tabs were not identified or reported closed. Defaults stay strict.
The option requires the relay and cannot be combined with a tab or `--all`.

After upgrading with Chrome open, the running native host may still be the
previous binary. Installed-file checks do not refresh it. When agent sessions
are idle, reload the chrome-use extension at `chrome://extensions` to replace
the host; reconnecting a session daemon does not do that. `status` reports
`health.nativeHostVersion` from the live handshake (null when unavailable),
separately from the installed CLI and `hostLauncherHealthy`. Missing or
mismatched runtime versions produce warnings. Missing call capability can
come from the host or extension; check both before deciding which to update.

The isolated cookies/storage described below apply to separately launched
browsers (`--launch`), not to sessions in your real Chrome profile.

**Related**: [authentication.md](authentication.md) for login patterns, [SKILL.md](../SKILL.md) for quick start.

## Contents

- [Named Sessions](#named-sessions)
- [Session Isolation Properties](#session-isolation-properties)
- [Session State Persistence](#session-state-persistence)
- [Common Patterns](#common-patterns)
- [Default Session](#default-session)
- [Session Cleanup](#session-cleanup)
- [Best Practices](#best-practices)

## Named Sessions

Use `--session` flag to isolate browser contexts:

```bash
# Session 1: Authentication flow
chrome-use --session auth --launch open https://app.example.com/login

# Session 2: Public browsing (separate cookies, storage)
chrome-use --session public --launch open https://example.com

# Commands are isolated by session
chrome-use --session auth fill @e1 "user@example.com"
chrome-use --session public get text body
```

## Session Isolation Properties

Each session has independent:
- Cookies
- LocalStorage / SessionStorage
- IndexedDB
- Cache
- Browsing history
- Open tabs

## Session State Persistence

### Save Session State

```bash
# Save cookies, storage, and auth state
chrome-use state save /path/to/auth-state.json
```

### Load Session State

```bash
# Restore saved state
chrome-use state load /path/to/auth-state.json

# Continue with authenticated session
chrome-use open https://app.example.com/dashboard
```

### State File Contents

```json
{
  "cookies": [...],
  "localStorage": {...},
  "sessionStorage": {...},
  "origins": [...]
}
```

## Common Patterns

### Authenticated Session Reuse

```bash
#!/bin/bash
# Save login state once, reuse many times

STATE_FILE="/tmp/auth-state.json"

# Check if we have saved state
if [[ -f "$STATE_FILE" ]]; then
    chrome-use state load "$STATE_FILE"
    chrome-use open https://app.example.com/dashboard
else
    # Perform login
    chrome-use open https://app.example.com/login
    chrome-use snapshot -i
    chrome-use fill @e1 "$USERNAME"
    chrome-use fill @e2 "$PASSWORD"
    chrome-use click @e3
    chrome-use wait --load networkidle

    # Save for future use
    chrome-use state save "$STATE_FILE"
fi
```

### Concurrent Scraping

```bash
#!/bin/bash
# Scrape multiple sites concurrently

# Start all sessions
chrome-use --session site1 open https://site1.com &
chrome-use --session site2 open https://site2.com &
chrome-use --session site3 open https://site3.com &
wait

# Extract from each
chrome-use --session site1 get text body > site1.txt
chrome-use --session site2 get text body > site2.txt
chrome-use --session site3 get text body > site3.txt

# Cleanup
chrome-use --session site1 close
chrome-use --session site2 close
chrome-use --session site3 close
```

### A/B Testing Sessions

```bash
# Test different user experiences
chrome-use --session variant-a open "https://app.com?variant=a"
chrome-use --session variant-b open "https://app.com?variant=b"

# Compare
chrome-use --session variant-a screenshot /tmp/variant-a.png
chrome-use --session variant-b screenshot /tmp/variant-b.png
```

## Default Session

When `--session` is omitted, chrome-use first looks for a stable runner ID.
Codex exposes `CODEX_THREAD_ID`, so separate Codex tasks automatically receive
different `cu-…` sessions. Other runners can set
`AGENT_BROWSER_SESSION_ID`. A plain shell with no recognized runner ID uses the
`default` session:

```bash
# These use the same derived task session, or "default" in a plain shell
chrome-use open https://example.com
chrome-use snapshot -i
chrome-use close
```

The derived name is `cu-<repo>-<tag>`: `<tag>` comes from the agent/terminal
ID and `<repo>` is only the cwd basename. Running a command from another
directory would mint a different name and hit a fresh daemon with no refs, so
when a live daemon already carries the same agent tag under another prefix,
that session is reused and your tabs and refs survive a `cd`. Explicit
`--session <name>` / `AGENT_BROWSER_SESSION` still win.

An "Unknown ref" error names the session that answered and says whether it
holds any refs. "no snapshot has run in this session" means the command reached
a different session than the one you snapshotted (another directory or
terminal): run `chrome-use session list` and pin with `--session`. If it lists
the ref range it does have, the page changed since the snapshot.

## Session Cleanup

The background daemon recycles after its idle timeout. In a real/external
Chrome this disconnects without closing session-created tabs, so their URL and
in-page state survive until the next command reconnects. Explicit `close` or
`session stop` remains the intentional cleanup path and closes created tabs.
`close` reads the tabs back and reports `tabsClosed` and `verifiedAbsent`; when
one is still open or cannot be confirmed gone it fails with `close incomplete`,
names the tab, and keeps the session and its ownership so `close` can be retried.
After idle exit, stop rediscovers the browser and validates persisted ownership.
If rediscovery fails or identifies another browser, reconnect using the original
connection options and run close; stop reports failure and retains ownership.

```bash
# Close specific session
chrome-use --session auth close

# List active sessions
chrome-use session list

# Stop the current session daemon gracefully
chrome-use session stop

# Stop a named session daemon
chrome-use session stop auth

# Stop all session daemons
chrome-use session prune
```

## Best Practices

### 1. Name Sessions Semantically

```bash
# GOOD: Clear purpose
chrome-use --session github-auth open https://github.com
chrome-use --session docs-scrape open https://docs.example.com

# AVOID: Generic names
chrome-use --session s1 open https://github.com
```

### 2. Always Clean Up

```bash
# Close sessions when done
chrome-use --session auth close
chrome-use --session scrape close
```

### 3. Handle State Files Securely

```bash
# Don't commit state files (contain auth tokens!)
echo "*.auth-state.json" >> .gitignore

# Delete after use
rm /tmp/auth-state.json
```

### 4. Timeout Long Sessions

```bash
# Set timeout for automated scripts
timeout 60 chrome-use --session long-task get text body
```

## Idle recovery and persisted session names

Every other session's launched browser *is* closed after the daemon sits idle
(`AGENT_BROWSER_IDLE_TIMEOUT_MS`, default `600000`; set `0` to keep it). If that
happens, the next command launches a fresh browser rather than failing — and
says so in a warning. Read it: the new window is empty, so a half-filled form,
a logged-in tab, or anything typed into the old window is gone. That warning is
the difference between "the page navigated away" (it did not) and "the browser
was replaced" (it was). For a long-running flow with idle gaps, set
`AGENT_BROWSER_IDLE_TIMEOUT_MS=0` or keep the session busy.

To reclaim daemon workers without restarting every browser connection, use
`chrome-use session stop [name]` for one session or `chrome-use session prune`
for all active session daemons. Both commands stop workers gracefully.

### Persist session across runs

```bash
# Log in once, save cookies + localStorage
chrome-use state save ./auth.json

# Later runs start already-logged-in
chrome-use --state ./auth.json open https://app.example.com
```

Or use `--session-name` for auto-save/restore:

```bash
AGENT_BROWSER_SESSION_NAME=my-app chrome-use open https://app.example.com
# State is auto-saved and restored on subsequent runs with the same name.
```

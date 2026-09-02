# Session Management

Multiple isolated browser sessions with state persistence and concurrent browsing.

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
chrome-use --session auth open https://app.example.com/login

# Session 2: Public browsing (separate cookies, storage)
chrome-use --session public open https://example.com

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

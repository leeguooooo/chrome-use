# Authentication Patterns

Login flows, session persistence, OAuth, 2FA, and authenticated browsing.

**Related**: [session-management.md](session-management.md) for state persistence details, [SKILL.md](../SKILL.md) for quick start.

## Contents

- [Import Auth from Your Browser](#import-auth-from-your-browser)
- [Credentials & Passkeys from Bitwarden](#credentials--passkeys-from-bitwarden)
- [Persistent Profiles](#persistent-profiles)
- [Session Persistence](#session-persistence)
- [Basic Login Flow](#basic-login-flow)
- [Saving Authentication State](#saving-authentication-state)
- [Restoring Authentication](#restoring-authentication)
- [OAuth / SSO Flows](#oauth--sso-flows)
- [Two-Factor Authentication](#two-factor-authentication)
- [HTTP Basic Auth](#http-basic-auth)
- [Cookie-Based Auth](#cookie-based-auth)
- [Token Refresh Handling](#token-refresh-handling)
- [Security Best Practices](#security-best-practices)

## Ground rules

Reuse an authorized logged-in session first. Follow the user's authorization
and the host's safety rules.

On a login page, `auth login --bwu` uses a Bitwarden account; add `--item`
when several match. In a `--launch` browser, `--passkey` signs in with only
a vault passkey (bwu 0.9.0+); passkeys are unsupported on the extension relay.

Never print secrets or put passwords in shell arguments/history. Use an
authorized vault and stdin, and protect saved state files as credentials.
Do not ask for secrets to be pasted into chat. A successful login requires
reaching the requested authenticated destination, not merely clicking submit.

For a step that actually requires the human, use `session handoff`, explain
the step, and stop driving that session. Run `session resume` only after the
user says they are done. An idle-recovery warning means a launched browser may
have been replaced; inspect state instead of assuming the previous form/login
survived.

## Import Auth from Your Browser

The fastest way to authenticate is to reuse cookies from a Chrome session you are already logged into.

**Step 1: Start Chrome with remote debugging**

```bash
# macOS
"/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" --remote-debugging-port=9222

# Linux
google-chrome --remote-debugging-port=9222

# Windows
"C:\Program Files\Google\Chrome\Application\chrome.exe" --remote-debugging-port=9222
```

Log in to your target site(s) in this Chrome window as you normally would.

> **Security note:** `--remote-debugging-port` exposes full browser control on localhost. Any local process can connect and read cookies, execute JS, etc. Only use on trusted machines and close Chrome when done.

**Step 2: Grab the auth state**

```bash
# Auto-discover the running Chrome and save its cookies + localStorage
chrome-use --auto-connect state save ./my-auth.json
```

**Step 3: Reuse in automation**

```bash
# Load auth at launch
chrome-use --state ./my-auth.json open https://app.example.com/dashboard

# Or load into an existing session
chrome-use state load ./my-auth.json
chrome-use open https://app.example.com/dashboard
```

This works for any site, including those with complex OAuth flows, SSO, or 2FA -- as long as Chrome already has valid session cookies.

> **Security note:** State files contain session tokens in plaintext. Add them to `.gitignore`, delete when no longer needed, and set `AGENT_BROWSER_ENCRYPTION_KEY` for encryption at rest. See [Security Best Practices](#security-best-practices).

**Tip:** Combine with `--session-name` so the imported auth auto-persists across restarts:

```bash
chrome-use --session-name myapp state load ./my-auth.json
# From now on, state is auto-saved/restored for "myapp"
```

## Credentials & Passkeys from Bitwarden

When you need an actual username/password (or passkey) rather than a reused
cookie, the sibling tool [`bitwarden-use`](https://github.com/leeguooooo/bitwarden-use)
(`bwu`) reads them out of a Bitwarden/Vaultwarden vault, so an agent can log in
with credentials, not just OAuth.

### One command: `auth login --bwu` (bwu 0.7.0+)

On a login page, this logs in with the vault account for that site:

```bash
chrome-use open https://github.com/login
chrome-use auth login --bwu                 # the only vault login for this site
chrome-use auth login --bwu --item <id>     # one of several; the error lists them
chrome-use auth login --bwu --no-submit     # fill only, press nothing
chrome-use auth login --bwu --passkey       # sign in with the vault passkey
```

- **Picking the account.** It asks `bwu login --domain <page> --list`, which
  sees masked entries only. One match is used. bwu matches by registrable
  domain, so `app.example.com` also lists every other `*.example.com` login;
  when exactly one of them is named exactly the page's host, that one is used.
  Otherwise several are listed most recently used first, each with an
  `--item` to pass back. Use the first unless the user asked for a particular
  account, and ask when that is unclear. It never picks one by itself.
- **Values never pass through you.** chrome-use runs itself again under
  `bwu run`, which logs the read and hands the values to that child process
  only. By default it asks the user once with Touch ID (not for items in a
  `reveal_folders` folder); a user who set `bwu config set require_touch_id
  false` has chosen unattended runs, and it does not ask at all. The response
  names which steps were filled, never the values.
- **The page.** It fills the tab it is on (a tab this session opened or
  adopted) and stops if the page moves to another origin. Default steps:
  username, password, Enter. A password field that appears only after the
  username was sent (one field per page) is waited for. A one-time-code field
  that appears after submitting gets the item's TOTP, and so does a page that
  asks only for the code (run it again there, e.g. after choosing "use your
  authenticator app"). A site that submits the code by itself is fine.
- **Unusual logins.** Give the vault item a custom field `_autotype` with steps
  separated by `:`. Use `username`, `password`, `totp`, `tab`, `enter`,
  `delay` (1 s), or another custom field's name, which is typed into the
  focused field. Example: `username:enter:delay:password:enter`. It is the same
  syntax as rofi-rbw's.
- **Passkeys (bwu 0.9.0+).** When the vault item has a passkey for the site,
  a temporary virtual authenticator holds it while the page asks for one
  (Chrome's own WebAuthn, so the site's ceremony is real), and is removed when
  the command ends. In the default flow it is installed only after the
  password was submitted and the next page asks for a passkey or security
  key; the page's "Use passkey" / "Sign in with a passkey" button is pressed
  if needed (never one that adds or registers a passkey). While the
  authenticator exists, an ordinary `navigator.credentials.create` call on
  the page is refused, so a page cannot register a passkey with it by
  accident; this is a guard against mistakes, not isolation (a script that
  kept the original function can bypass it). An unexpected registration, or
  a cleanup that cannot be confirmed, is reported as an error. Where WebAuthn is unavailable it falls back to the
  one-time code. `auth login --bwu --passkey` signs in with the passkey alone
  (no password is read). Passkeys work in a `--launch` browser only: on your
  own Chrome (the extension relay) the page's request does not reach
  chrome-use's authenticator, so `--passkey` refuses there and the default flow
  reports `"passkey": "unsupported"` and uses the password and code as before.
  The site sees the vault's signature counter, 0 for a synced passkey, the
  same as the Bitwarden extension. A passkey that keeps a non-zero counter is
  refused (its new count would have to be written back to the vault). A site
  that refuses the passkey is reported with its message.
- `--no-submit` fills only: no passkey and no one-time code (sites submit a
  code on its last digit).
- Result: `{"item", "filled": [...], "submitted", "otp": "filled" | "not asked" | "none", "passkey": "used" | "not asked" | "unsupported" | "unavailable" | "none", "url"}`.
  Run `snapshot` afterwards to see whether the site accepted the login.

### Login walls

When a command leaves the tab on a site's sign-in page instead of the page
asked for (an expired session bounced `open`, `reload`, `back` or a click to
`/login?redirect_uri=…`), chrome-use says so once per host per session: a
`login wall: <host> redirected to its sign-in page; …` line on stderr and
`loginWall: {url, returnTo, host, hint}` in the JSON data. Sign in with
`auth login --bwu` (with `--item` when several vault logins match), then open
`returnTo` and continue. Ask the user only when no vault item matches, a
second factor needs them, or the login fails. Detection reads the URL first
(sign-in paths and hosts, a `redirect_uri` / `return_to` / `next` / `continue`
parameter pointing back, or a bounce from another page of the same site) and
checks the page for a visible password or username field when the URL alone
is not conclusive, so a page that only links to a login is not flagged.

To sign in automatically, set `"auth": {"autoLogin": "bwu"}` in
`~/.chrome-use/config.json` or `AGENT_BROWSER_AUTO_LOGIN=bwu`. The first wall
per host per session then runs the `auth login --bwu` flow with the only
vault login for the site (several or none are reported, never guessed),
returns to `returnTo`, and reports `loginWall.autoLogin: {ok, item, error,
returnedTo}`. It handles a one-time code exactly as `auth login --bwu` does
and nothing more.

A site adapter that finds the site signed out (`chrome-use site …`) reports
the same kind of wall: `login wall: <host> is not signed in (site …)` and
`loginWall` with `source: "site"` and the `loginUrl` to open before `auth
login --bwu`. With auto-login on it opens that page, signs in and runs the
adapter once more. See `core/site-adapters`.

### Single fields

```bash
# install once
curl -fsSL https://raw.githubusercontent.com/leeguooooo/bitwarden-use/main/install.sh | sh

# username and password for an item, straight from the vault into the fields.
# `bwu run` logs every read and hands the values only to the child process:
# they are never arguments, never printed, and never in the transcript. It
# asks for Touch ID first unless the item is in a reveal folder or the user
# turned confirmation off (`require_touch_id false`).
bwu run --env CU_USER='github.com#username' -- chrome-use fill @e2 --from-env CU_USER
bwu run --env CU_PW='github.com#password' -- chrome-use fill @e3 --from-env CU_PW

# a one-time code
bwu run --env CU_OTP='github.com#totp' -- chrome-use fill @e4 --from-env CU_OTP

# passkey private key for sites that take a FIDO2/WebAuthn passkey
bwu fido2 get
```

A value from `--from-env` is treated as a secret whatever the field looks like:
results and errors show `<filled N chars>`, never the value. Prefer it over
`bwu get … | chrome-use fill … --stdin`, which skips the confirmation step and
leaves the value one stray `echo` away from the transcript.

Bitwarden's own inline menu (the dropdown next to a focused login field) is an
extension frame chrome-use cannot click; while it is open Chrome blocks the tab
for debugger commands. chrome-use closes it and carries on by itself for a tab
in front. Filling through `bwu run` does not need that menu at all.

This pairs with the local auth vault (`chrome-use auth`) above: keep credentials
in Bitwarden, pull them at login time, and never put a password in shell history.

## Persistent Profiles

Use `--profile` to point chrome-use at a Chrome user data directory. This persists everything (cookies, IndexedDB, service workers, cache) across browser restarts without explicit save/load:

```bash
# First run: login once
chrome-use --profile ~/.myapp-profile open https://app.example.com/login
# ... complete login flow ...

# All subsequent runs: already authenticated
chrome-use --profile ~/.myapp-profile open https://app.example.com/dashboard
```

Use different paths for different projects or test users:

```bash
chrome-use --profile ~/.profiles/admin open https://app.example.com
chrome-use --profile ~/.profiles/viewer open https://app.example.com
```

Or set via environment variable:

```bash
export AGENT_BROWSER_PROFILE=~/.myapp-profile
chrome-use open https://app.example.com/dashboard
```

## Session Persistence

Use `--session-name` to auto-save and restore cookies + localStorage by name, without managing files:

```bash
# Auto-saves state on close, auto-restores on next launch
chrome-use --session-name twitter open https://twitter.com
# ... login flow ...
chrome-use close  # state saved to ~/.chrome-use/sessions/

# Next time: state is automatically restored
chrome-use --session-name twitter open https://twitter.com
```

Encrypt state at rest:

```bash
export AGENT_BROWSER_ENCRYPTION_KEY=$(openssl rand -hex 32)
chrome-use --session-name secure open https://app.example.com
```

## Basic Login Flow

```bash
# Navigate to login page
chrome-use open https://app.example.com/login
chrome-use wait --load networkidle

# Get form elements
chrome-use snapshot -i
# Output: @e1 [input type="email"], @e2 [input type="password"], @e3 [button] "Sign In"

# Fill credentials
chrome-use fill @e1 "user@example.com"
chrome-use fill @e2 "password123"

# Submit
chrome-use click @e3
chrome-use wait --load networkidle

# Verify login succeeded
chrome-use get url  # Should be dashboard, not login
```

## Saving Authentication State

After logging in, save state for reuse:

```bash
# Login first (see above)
chrome-use open https://app.example.com/login
chrome-use snapshot -i
chrome-use fill @e1 "user@example.com"
chrome-use fill @e2 "password123"
chrome-use click @e3
chrome-use wait --url "**/dashboard"

# Save authenticated state
chrome-use state save ./auth-state.json
```

## Restoring Authentication

Skip login by loading saved state:

```bash
# Load saved auth state
chrome-use state load ./auth-state.json

# Navigate directly to protected page
chrome-use open https://app.example.com/dashboard

# Verify authenticated
chrome-use snapshot -i
```

## OAuth / SSO Flows

For OAuth redirects:

```bash
# Start OAuth flow
chrome-use open https://app.example.com/auth/google

# Handle redirects automatically
chrome-use wait --url "**/accounts.google.com**"
chrome-use snapshot -i

# Fill Google credentials
chrome-use fill @e1 "user@gmail.com"
chrome-use click @e2  # Next button
chrome-use wait 2000
chrome-use snapshot -i
chrome-use fill @e3 "password"
chrome-use click @e4  # Sign in

# Wait for redirect back
chrome-use wait --url "**/app.example.com**"
chrome-use state save ./oauth-state.json
```

## Two-Factor Authentication

Handle 2FA with manual intervention:

```bash
# Login with credentials
chrome-use open https://app.example.com/login --headed  # Show browser
chrome-use snapshot -i
chrome-use fill @e1 "user@example.com"
chrome-use fill @e2 "password123"
chrome-use click @e3

# Wait for user to complete 2FA manually
echo "Complete 2FA in the browser window..."
chrome-use wait --url "**/dashboard" --timeout 120000

# Save state after 2FA
chrome-use state save ./2fa-state.json
```

## HTTP Basic Auth

For sites using HTTP Basic Authentication:

```bash
# Set credentials before navigation
chrome-use set credentials username password

# Navigate to protected resource
chrome-use open https://protected.example.com/api
```

## Cookie-Based Auth

Manually set authentication cookies:

```bash
# Set auth cookie
chrome-use cookies set session_token "abc123xyz"

# Navigate to protected page
chrome-use open https://app.example.com/dashboard
```

## Token Refresh Handling

For sessions with expiring tokens:

```bash
#!/bin/bash
# Wrapper that handles token refresh

STATE_FILE="./auth-state.json"

# Try loading existing state
if [[ -f "$STATE_FILE" ]]; then
    chrome-use state load "$STATE_FILE"
    chrome-use open https://app.example.com/dashboard

    # Check if session is still valid
    URL=$(chrome-use get url)
    if [[ "$URL" == *"/login"* ]]; then
        echo "Session expired, re-authenticating..."
        # Perform fresh login
        chrome-use snapshot -i
        chrome-use fill @e1 "$USERNAME"
        chrome-use fill @e2 "$PASSWORD"
        chrome-use click @e3
        chrome-use wait --url "**/dashboard"
        chrome-use state save "$STATE_FILE"
    fi
else
    # First-time login
    chrome-use open https://app.example.com/login
    # ... login flow ...
fi
```

## Security Best Practices

1. **Never commit state files** - They contain session tokens
   ```bash
   echo "*.auth-state.json" >> .gitignore
   ```

2. **Use environment variables for credentials**
   ```bash
   chrome-use fill @e1 "$APP_USERNAME"
   chrome-use fill @e2 "$APP_PASSWORD"
   ```

3. **Clean up after automation**
   ```bash
   chrome-use cookies clear --domain <site>   # only that site; --all --yes clears the whole browser
   rm -f ./auth-state.json
   ```

4. **Use short-lived sessions for CI/CD**
   ```bash
   # Don't persist state in CI
   chrome-use open https://app.example.com/login
   # ... login and perform actions ...
   chrome-use close  # Session ends, nothing persisted
   ```

## Auth vault

Credentials in shell history are a leak. For anything sensitive, use the
auth vault (see the security guidance above):

```bash
chrome-use auth save my-app --url https://app.example.com/login \
  --username user@example.com --password-stdin
# (type password, Ctrl+D)

chrome-use auth login my-app    # fills + clicks, waits for form
chrome-use auth login my-app --no-navigate  # fill the login page already open (same origin only)
```

**Log in autonomously first — this is the default and the goal.** chrome-use is
built to log in *by itself*: pull credentials from the auth vault or Bitwarden
(`bwu get … | chrome-use fill … --stdin`), read TOTP/2FA codes, drive OAuth, and
persist the session (`state save` / `--session-name`) so later runs start already
logged in. Do NOT hand a login to the human just because it has a password or a
2FA step — solve it. See the login patterns above and [session-management.md](session-management.md).

For a CAPTCHA, load `core/captcha`: inspect its current type, try an ordinary
slider solver or screenshot-guided ordered clicks, verify, and continue the
authorized login. A loaded vendor script is not proof that a person is needed.

**`session handoff` is a rare escape hatch, NOT how you log in.** Reach for it
*only* when a step is genuinely impossible for the agent — an image/behavioral
captcha you can't solve, an SMS/authenticator code you have no access to, a
hardware-key tap, a bank's "approve on your phone" prompt. Try autonomously
first; hand off only as a last resort:

```bash
chrome-use session handoff        # last resort: mark user-owned; tell the user exactly what to do
# … the human does the one thing the agent truly can't …
chrome-use session resume         # take control back — ONLY after they confirm they're done
```

While handed off, **any browser-driving command on that session is refused**
(loud error with the exact `session resume` line), so the agent can't fight the
user for the tab. It's **zero-impact until you call `handoff`** — the agent owns
and drives every session by default, autonomous login included. Check state with
`chrome-use session status`; `chrome-use session list` shows every session's owner.
Never call `session resume` on your own to grab control back — wait for the user.
A handed-off session is also **never reaped by the idle timer** — the window the
human is working in stays open however long they take.

# Authentication Patterns

Login flows, session persistence, OAuth, 2FA, and authenticated browsing.

**Related**: [session-management.md](session-management.md) for state persistence details, [SKILL.md](../SKILL.md) for quick start.

## Contents

- [Import Auth from Your Browser](#import-auth-from-your-browser)
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
   chrome-use cookies clear
   rm -f ./auth-state.json
   ```

4. **Use short-lived sessions for CI/CD**
   ```bash
   # Don't persist state in CI
   chrome-use open https://app.example.com/login
   # ... login and perform actions ...
   chrome-use close  # Session ends, nothing persisted
   ```

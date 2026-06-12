#!/bin/bash
# Template: Authenticated Session Workflow
# Purpose: Login once, save state, reuse for subsequent runs
# Usage: ./authenticated-session.sh <login-url> [state-file]
#
# RECOMMENDED: Use the auth vault instead of this template:
#   echo "<pass>" | chrome-use auth save myapp --url <login-url> --username <user> --password-stdin
#   chrome-use auth login myapp
# The auth vault stores credentials securely and the LLM never sees passwords.
#
# Environment variables:
#   APP_USERNAME - Login username/email
#   APP_PASSWORD - Login password
#
# Two modes:
#   1. Discovery mode (default): Shows form structure so you can identify refs
#   2. Login mode: Performs actual login after you update the refs
#
# Setup steps:
#   1. Run once to see form structure (discovery mode)
#   2. Update refs in LOGIN FLOW section below
#   3. Set APP_USERNAME and APP_PASSWORD
#   4. Delete the DISCOVERY section

set -euo pipefail

LOGIN_URL="${1:?Usage: $0 <login-url> [state-file]}"
STATE_FILE="${2:-./auth-state.json}"

echo "Authentication workflow: $LOGIN_URL"

# ================================================================
# SAVED STATE: Skip login if valid saved state exists
# ================================================================
if [[ -f "$STATE_FILE" ]]; then
    echo "Loading saved state from $STATE_FILE..."
    if chrome-use --state "$STATE_FILE" open "$LOGIN_URL" 2>/dev/null; then
        chrome-use wait --load networkidle

        CURRENT_URL=$(chrome-use get url)
        if [[ "$CURRENT_URL" != *"login"* ]] && [[ "$CURRENT_URL" != *"signin"* ]]; then
            echo "Session restored successfully"
            chrome-use snapshot -i
            exit 0
        fi
        echo "Session expired, performing fresh login..."
        chrome-use close 2>/dev/null || true
    else
        echo "Failed to load state, re-authenticating..."
    fi
    rm -f "$STATE_FILE"
fi

# ================================================================
# DISCOVERY MODE: Shows form structure (delete after setup)
# ================================================================
echo "Opening login page..."
chrome-use open "$LOGIN_URL"
chrome-use wait --load networkidle

echo ""
echo "Login form structure:"
echo "---"
chrome-use snapshot -i
echo "---"
echo ""
echo "Next steps:"
echo "  1. Note the refs: username=@e?, password=@e?, submit=@e?"
echo "  2. Update the LOGIN FLOW section below with your refs"
echo "  3. Set: export APP_USERNAME='...' APP_PASSWORD='...'"
echo "  4. Delete this DISCOVERY MODE section"
echo ""
chrome-use close
exit 0

# ================================================================
# LOGIN FLOW: Uncomment and customize after discovery
# ================================================================
# : "${APP_USERNAME:?Set APP_USERNAME environment variable}"
# : "${APP_PASSWORD:?Set APP_PASSWORD environment variable}"
#
# chrome-use open "$LOGIN_URL"
# chrome-use wait --load networkidle
# chrome-use snapshot -i
#
# # Fill credentials (update refs to match your form)
# chrome-use fill @e1 "$APP_USERNAME"
# chrome-use fill @e2 "$APP_PASSWORD"
# chrome-use click @e3
# chrome-use wait --load networkidle
#
# # Verify login succeeded
# FINAL_URL=$(chrome-use get url)
# if [[ "$FINAL_URL" == *"login"* ]] || [[ "$FINAL_URL" == *"signin"* ]]; then
#     echo "Login failed - still on login page"
#     chrome-use screenshot /tmp/login-failed.png
#     chrome-use close
#     exit 1
# fi
#
# # Save state for future runs
# echo "Saving state to $STATE_FILE"
# chrome-use state save "$STATE_FILE"
# echo "Login successful"
# chrome-use snapshot -i

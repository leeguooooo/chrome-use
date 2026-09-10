#!/bin/sh
# Publish extensions/ab-connect.zip to the Chrome Web Store through its REST API.
# No browser, no clicking — the whole point is that this runs the same from a
# laptop or from CI, so 0.5.26, 0.5.27 … are one command, never a manual upload.
#
# Chrome forbids CDP on the Web Store gallery, so chrome-use itself cannot drive
# the developer console. This API path is the supported alternative.
#
# ── One-time setup (does NOT repeat per release) ─────────────────────────────
#   1. https://console.cloud.google.com → create/pick a project.
#   2. APIs & Services → Library → enable "Chrome Web Store API".
#   3. APIs & Services → OAuth consent screen → External, add yourself as a test
#      user (the app can stay in "testing"; a test-user refresh token does not
#      expire for this use).
#   4. Credentials → Create credentials → OAuth client ID → type "Desktop app".
#      Note the client ID and client secret.
#   5. Get a refresh token once:  sh scripts/cws-get-refresh-token.sh
#   6. Save all three to .secrets/cws.env (gitignored):
#        CWS_CLIENT_ID=...
#        CWS_CLIENT_SECRET=...
#        CWS_REFRESH_TOKEN=...
#
# ── Every release ────────────────────────────────────────────────────────────
#        sh scripts/pack-extension.sh          # build the 0.5.x zip
#        sh scripts/publish-extension.sh       # upload + submit for review
#   Add --draft to upload without submitting (leaves it as a draft in the console).
#
# Credentials come from .secrets/cws.env or the environment (so CI can supply
# them as secrets). Nothing is ever printed.
set -eu

cd "$(dirname "$0")/.."
ITEM_ID="knfcmbamhjmaonkfnjhldjedeobeafmk"   # the published (store) extension id
ZIP="extensions/ab-connect.zip"
ENV_FILE=".secrets/cws.env"
DRAFT=0
[ "${1:-}" = "--draft" ] && DRAFT=1

[ -f "$ZIP" ] || { echo "error: $ZIP not found — run scripts/pack-extension.sh first" >&2; exit 1; }
if [ -f "$ENV_FILE" ]; then
  # shellcheck disable=SC1090
  . "$ENV_FILE"
fi
: "${CWS_CLIENT_ID:?set CWS_CLIENT_ID (see the setup notes at the top of this script)}"
: "${CWS_CLIENT_SECRET:?set CWS_CLIENT_SECRET}"
: "${CWS_REFRESH_TOKEN:?set CWS_REFRESH_TOKEN}"

VER=$(python3 -c "import json;print(json.load(open('extensions/ab-connect/manifest.json'))['version'])")
echo "→ publishing ab-connect $VER ($(wc -c <"$ZIP" | tr -d ' ') bytes) to item $ITEM_ID"

# 1. refresh token → short-lived access token (never printed)
ACCESS_TOKEN=$(curl -s -X POST https://oauth2.googleapis.com/token \
  -d "client_id=$CWS_CLIENT_ID" \
  -d "client_secret=$CWS_CLIENT_SECRET" \
  -d "refresh_token=$CWS_REFRESH_TOKEN" \
  -d "grant_type=refresh_token" \
  | python3 -c "import json,sys;d=json.load(sys.stdin);print(d.get('access_token') or '')")
[ -n "$ACCESS_TOKEN" ] || { echo "error: token refresh failed — check the three CWS_* values" >&2; exit 1; }

# 2. upload the new package
echo "→ uploading package…"
UP=$(curl -s -X PUT \
  -H "Authorization: Bearer $ACCESS_TOKEN" \
  -H "x-goog-api-version: 2" \
  -T "$ZIP" \
  "https://www.googleapis.com/upload/chromewebstore/v1.1/items/$ITEM_ID?uploadType=media")
echo "$UP" | python3 -c "
import json,sys
d=json.load(sys.stdin)
state=d.get('uploadState')
print('   uploadState:', state)
if state not in ('SUCCESS',):
    for e in d.get('itemError',[]) or []:
        print('   error:', e.get('error_code'), e.get('error_detail'))
    sys.exit(1)
"

if [ "$DRAFT" = 1 ]; then
  echo "✓ uploaded as a draft (not submitted). Review and publish in the developer console."
  exit 0
fi

# 3. submit for review / publish
echo "→ submitting for review…"
PUB=$(curl -s -X POST \
  -H "Authorization: Bearer $ACCESS_TOKEN" \
  -H "x-goog-api-version: 2" \
  -H "Content-Length: 0" \
  "https://www.googleapis.com/chromewebstore/v1.1/items/$ITEM_ID/publish")
echo "$PUB" | python3 -c "
import json,sys
d=json.load(sys.stdin)
st=d.get('status') or []
print('   status:', ', '.join(st) if st else d)
detail=d.get('statusDetail') or []
for s in detail: print('   ', s)
# OK / ITEM_PENDING_REVIEW are both success; anything else is a real failure.
ok={'OK','ITEM_PENDING_REVIEW'}
sys.exit(0 if (set(st) & ok) else 1)
"
echo "✓ ab-connect $VER submitted for Chrome Web Store review."

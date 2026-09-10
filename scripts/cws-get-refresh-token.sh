#!/bin/sh
# One-time: exchange a Google OAuth consent for a Chrome Web Store refresh token.
# Run this once after creating a "Desktop app" OAuth client (see
# scripts/publish-extension.sh header). The refresh token it prints goes into
# .secrets/cws.env as CWS_REFRESH_TOKEN and is reused for every future publish.
#
#   sh scripts/cws-get-refresh-token.sh <CLIENT_ID> <CLIENT_SECRET>
#
# Uses a localhost loopback redirect (Google deprecated the old out-of-band
# copy-paste flow). A Desktop-app OAuth client allows loopback automatically.
# It opens the consent URL in your browser; you approve; the local listener
# catches the code and exchanges it. The only value printed is the refresh
# token; nothing is stored by this script.
set -eu

CLIENT_ID="${1:?usage: cws-get-refresh-token.sh <CLIENT_ID> <CLIENT_SECRET>}"
CLIENT_SECRET="${2:?usage: cws-get-refresh-token.sh <CLIENT_ID> <CLIENT_SECRET>}"

CWS_CLIENT_ID="$CLIENT_ID" CWS_CLIENT_SECRET="$CLIENT_SECRET" python3 - <<'PY'
import http.server, os, secrets, urllib.parse, urllib.request, webbrowser, sys, json

CLIENT_ID = os.environ["CWS_CLIENT_ID"]
CLIENT_SECRET = os.environ["CWS_CLIENT_SECRET"]
SCOPE = "https://www.googleapis.com/auth/chromewebstore"
PORT = 8770
REDIRECT = f"http://127.0.0.1:{PORT}"
state = secrets.token_urlsafe(16)

auth_url = "https://accounts.google.com/o/oauth2/auth?" + urllib.parse.urlencode({
    "response_type": "code", "access_type": "offline", "prompt": "consent",
    "client_id": CLIENT_ID, "redirect_uri": REDIRECT, "scope": SCOPE, "state": state,
})

code = {}

class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        q = urllib.parse.parse_qs(urllib.parse.urlparse(self.path).query)
        self.send_response(200); self.send_header("Content-Type", "text/plain; charset=utf-8"); self.end_headers()
        if q.get("state", [""])[0] == state and "code" in q:
            code["v"] = q["code"][0]
            self.wfile.write("Done — you can close this tab and return to the terminal.".encode())
        else:
            self.wfile.write(f"OAuth error: {q.get('error', ['unknown'])[0]}".encode())
    def log_message(self, *a):
        pass

print("Opening the consent screen in your browser. Approve it there.")
print(f"If it does not open, paste this URL yourself:\n\n  {auth_url}\n")
try:
    webbrowser.open(auth_url)
except Exception:
    pass

srv = http.server.HTTPServer(("127.0.0.1", PORT), Handler)
while "v" not in code:
    srv.handle_request()

resp = urllib.request.urlopen(urllib.request.Request(
    "https://oauth2.googleapis.com/token",
    data=urllib.parse.urlencode({
        "client_id": CLIENT_ID, "client_secret": CLIENT_SECRET,
        "code": code["v"], "grant_type": "authorization_code", "redirect_uri": REDIRECT,
    }).encode(),
))
d = json.load(resp)
rt = d.get("refresh_token")
if not rt:
    print("failed:", d, file=sys.stderr); sys.exit(1)
print("\nCWS_REFRESH_TOKEN=" + rt)
print("\nAdd that line (plus CWS_CLIENT_ID and CWS_CLIENT_SECRET) to .secrets/cws.env")
PY

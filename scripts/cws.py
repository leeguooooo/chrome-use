#!/usr/bin/env python3
"""Chrome Web Store API v2 client for ab-connect: status, upload, submit.

    python3 scripts/cws.py status            # what the store has, as JSON
    python3 scripts/cws.py publish [--draft] # upload extensions/ab-connect.zip and submit
    python3 scripts/cws.py publish --if-new  # ... unless the store already has that version

Credentials come from the environment, or from .secrets/ when run locally:

  CWS_PUBLISHER_ID   Developer Dashboard > Publisher > Settings (required)
  CWS_SA_KEY         the service account's JSON key (contents), or
  CWS_SA_KEY_FILE    a path to it (default .secrets/cws-sa.json)

A service account never expires the way a refresh token from an OAuth app in
"Testing" does (7 days), which is what silently stopped 0.5.27-0.5.29. The old
CWS_CLIENT_ID / CWS_CLIENT_SECRET / CWS_REFRESH_TOKEN still work as a fallback.

Standard library only; the JWT is signed with the `openssl` binary. Tokens and
keys are never printed.
"""

import base64
import json
import os
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request
import zipfile

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
ITEM_ID = "knfcmbamhjmaonkfnjhldjedeobeafmk"  # the store item
ZIP = os.path.join(ROOT, "extensions", "ab-connect.zip")
SCOPE = "https://www.googleapis.com/auth/chromewebstore"
API = "https://chromewebstore.googleapis.com"


def die(msg):
    print(f"error: {msg}", file=sys.stderr)
    sys.exit(1)


def load_local_env():
    """Fill missing CWS_* variables from .secrets/cws.env (KEY=VALUE lines)."""
    path = os.path.join(ROOT, ".secrets", "cws.env")
    if not os.path.exists(path):
        return
    for line in open(path):
        line = line.strip()
        if not line or line.startswith("#") or "=" not in line:
            continue
        k, v = line.split("=", 1)
        os.environ.setdefault(k.strip(), v.strip().strip("'\""))


def b64url(data):
    return base64.urlsafe_b64encode(data).rstrip(b"=").decode()


def post_form(url, fields):
    req = urllib.request.Request(url, data=urllib.parse.urlencode(fields).encode())
    try:
        return json.load(urllib.request.urlopen(req, timeout=30))
    except urllib.error.HTTPError as e:
        # Google's token errors are safe to show ({"error": "invalid_grant", ...}).
        die(f"token request failed: HTTP {e.code} {e.read().decode(errors='replace')[:300]}")


def service_account_token(key):
    now = int(time.time())
    header = b64url(json.dumps({"alg": "RS256", "typ": "JWT"}).encode())
    claims = b64url(json.dumps({
        "iss": key["client_email"],
        "scope": SCOPE,
        "aud": key.get("token_uri") or "https://oauth2.googleapis.com/token",
        "iat": now,
        "exp": now + 3600,
    }).encode())
    signing_input = f"{header}.{claims}".encode()
    with tempfile.NamedTemporaryFile("w", suffix=".pem", delete=False) as fh:
        fh.write(key["private_key"])
        pem = fh.name
    try:
        os.chmod(pem, 0o600)
        sig = subprocess.run(
            ["openssl", "dgst", "-sha256", "-sign", pem],
            input=signing_input, capture_output=True, check=True,
        ).stdout
    except (OSError, subprocess.CalledProcessError) as e:
        die(f"could not sign the service-account JWT with openssl: {e}")
    finally:
        os.unlink(pem)
    jwt = f"{header}.{claims}.{b64url(sig)}"
    tok = post_form(key.get("token_uri") or "https://oauth2.googleapis.com/token", {
        "grant_type": "urn:ietf:params:oauth:grant-type:jwt-bearer",
        "assertion": jwt,
    })
    return tok["access_token"]


def access_token():
    raw = os.environ.get("CWS_SA_KEY")
    path = os.environ.get("CWS_SA_KEY_FILE") or os.path.join(ROOT, ".secrets", "cws-sa.json")
    if raw or os.path.exists(path):
        key = json.loads(raw) if raw else json.load(open(path))
        return service_account_token(key), "service account"
    if all(os.environ.get(k) for k in ("CWS_CLIENT_ID", "CWS_CLIENT_SECRET", "CWS_REFRESH_TOKEN")):
        tok = post_form("https://oauth2.googleapis.com/token", {
            "client_id": os.environ["CWS_CLIENT_ID"],
            "client_secret": os.environ["CWS_CLIENT_SECRET"],
            "refresh_token": os.environ["CWS_REFRESH_TOKEN"],
            "grant_type": "refresh_token",
        })
        return tok["access_token"], "OAuth refresh token"
    die("no credentials: set CWS_SA_KEY (or CWS_SA_KEY_FILE / .secrets/cws-sa.json); "
        "see extensions/store/PUBLISHING.md")


def item_name():
    pub = os.environ.get("CWS_PUBLISHER_ID")
    if not pub:
        die("set CWS_PUBLISHER_ID (Developer Dashboard > Publisher > Settings)")
    return f"publishers/{pub}/items/{ITEM_ID}"


def call(method, url, token, body=None, data=None):
    headers = {"Authorization": f"Bearer {token}"}
    if body is not None:
        data = json.dumps(body).encode()
        headers["Content-Type"] = "application/json"
    req = urllib.request.Request(url, data=data, headers=headers, method=method)
    if method == "POST" and data is None:
        req.add_header("Content-Length", "0")
    try:
        return json.load(urllib.request.urlopen(req, timeout=120))
    except urllib.error.HTTPError as e:
        die(f"{method} {url.split('/v2/')[-1]}: HTTP {e.code} {e.read().decode(errors='replace')[:800]}")


def zip_version():
    return json.loads(zipfile.ZipFile(ZIP).read("manifest.json"))["version"]


def store_versions(status):
    """Versions the store holds: {'published': '0.5.26', 'submitted': '0.5.29'}."""
    out = {}
    for label, key in (("published", "publishedItemRevisionStatus"),
                       ("submitted", "submittedItemRevisionStatus")):
        rev = status.get(key) or {}
        chans = rev.get("distributionChannels") or []
        if chans:
            out[label] = chans[0].get("crxVersion")
            out[f"{label}_state"] = rev.get("state")
    return out


def cmd_status(token):
    status = call("GET", f"{API}/v2/{item_name()}:fetchStatus", token)
    summary = store_versions(status)
    summary["packaged"] = zip_version()
    for flag in ("takenDown", "warned"):
        if status.get(flag):
            summary[flag] = True
    print(json.dumps(summary, indent=2))
    return status


def cmd_publish(token, draft, if_new):
    ver = zip_version()
    name = item_name()
    if if_new:
        have = store_versions(call("GET", f"{API}/v2/{name}:fetchStatus", token))
        if ver in (have.get("published"), have.get("submitted")):
            print(f"store already has ab-connect {ver} ({have}); nothing to do")
            return
    print(f"→ uploading ab-connect {ver} ({os.path.getsize(ZIP)} bytes)")
    up = call("POST", f"{API}/upload/v2/{name}:upload", token, data=open(ZIP, "rb").read())
    state = up.get("uploadState")
    # Large packages are processed asynchronously; poll until they settle.
    for _ in range(30):
        if state != "IN_PROGRESS":
            break
        time.sleep(5)
        state = call("GET", f"{API}/v2/{name}:fetchStatus", token).get("lastAsyncUploadState")
    print(f"   uploadState: {state}")
    if state != "SUCCEEDED":
        die(f"upload did not succeed ({state}): {json.dumps(up)[:800]}")
    if draft:
        print("✓ uploaded as a draft; not submitted for review")
        return
    print("→ submitting for review")
    pub = call("POST", f"{API}/v2/{name}:publish", token, body={})
    print(f"   state: {pub.get('state')}")
    if pub.get("warningInfo"):
        print(f"   warnings: {json.dumps(pub['warningInfo'])[:800]}")
    if pub.get("state") not in ("PENDING_REVIEW", "STAGED", "PUBLISHED", "PUBLISHED_TO_TESTERS"):
        die(f"publish returned {json.dumps(pub)[:800]}")
    print(f"✓ ab-connect {ver} submitted to the Chrome Web Store")


def main(argv):
    if not argv or argv[0] not in ("status", "publish"):
        print(__doc__.strip())
        sys.exit(2)
    load_local_env()
    item_name()  # fail on a missing publisher id before asking for a token
    token, how = access_token()
    print(f"(auth: {how})", file=sys.stderr)
    if argv[0] == "status":
        cmd_status(token)
    else:
        cmd_publish(token, "--draft" in argv, "--if-new" in argv)


if __name__ == "__main__":
    main(sys.argv[1:])

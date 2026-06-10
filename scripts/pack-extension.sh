#!/bin/sh
# Build the Chrome Web Store upload package extensions/ab-connect.zip (and a signed
# extensions/ab-connect.crx for reference) from extensions/ab-connect.
#
# IMPORTANT — the "key" field:
#   * The unpacked DIR (Load-unpacked) and the signed .crx KEEP the manifest "key",
#     which pins the id to ciiljdlhdpfckdcfkphgmfalanpdejep so the native-messaging
#     allowed_origins + managed force-install policy keep matching for local/dev use.
#   * The Web Store UPLOAD zip MUST NOT contain "key" — the store rejects it
#     ("manifest must not contain 'key'") and assigns its own id. So this script
#     strips "key" from the manifest inside the zip only. After the first upload,
#     note the store-assigned id and add it to the native-messaging allowed_origins
#     (cli/src/connect.rs EXTENSION_ID) so the store build can pair too.
#
# The private key lives at .secrets/ab-connect.pem and is git-ignored.
#
# After changing the extension:
#   1. bump "version" in extensions/ab-connect/manifest.json
#   2. run this script
#   3. commit extensions/ab-connect.zip (+ .crx) + manifest.json
#   4. upload ab-connect.zip to the Web Store (see extensions/store/SUBMISSION.html)
set -e
cd "$(dirname "$0")/.."
KEY=.secrets/ab-connect.pem
EXT=extensions/ab-connect
CHROME="${CHROME_BIN:-/Applications/Google Chrome.app/Contents/MacOS/Google Chrome}"

# Web Store upload package: stage a copy with the "key" field removed, then zip.
STAGE=$(mktemp -d)
trap 'rm -rf "$STAGE"' EXIT
cp -R "$EXT/." "$STAGE/"
python3 - "$STAGE/manifest.json" <<'PY'
import json, sys
p = sys.argv[1]
m = json.load(open(p))
m.pop("key", None)  # the Web Store forbids the "key" field in uploads
json.dump(m, open(p, "w"), indent=2)
open(p, "a").write("\n")
PY
rm -f extensions/ab-connect.zip
( cd "$STAGE" && zip -rq "$OLDPWD/extensions/ab-connect.zip" . -x '.*' )
[ -f extensions/ab-connect.zip ] || { echo "error: zip failed" >&2; exit 1; }
if unzip -p extensions/ab-connect.zip manifest.json | grep -q '"key"'; then
  echo "error: 'key' still present in upload zip" >&2; exit 1
fi
echo "packed extensions/ab-connect.zip (key stripped for Web Store)"

# Signed crx (reference / non-store force-install for managed setups) — keeps "key"
# via the signing key so the id stays ciiljdlhdpfckdcfkphgmfalanpdejep.
if [ -f "$KEY" ]; then
  rm -f extensions/ab-connect.crx
  "$CHROME" --pack-extension="$PWD/$EXT" --pack-extension-key="$PWD/$KEY" >/dev/null 2>&1 || true
  ID=$(openssl rsa -in "$KEY" -pubout -outform DER 2>/dev/null \
       | openssl dgst -sha256 -binary | xxd -p -c256 | head -c32 | tr '0-9a-f' 'a-p')
  echo "local/crx extension id: $ID"
else
  echo "note: $KEY missing — built zip only (no crx)."
fi
echo "manifest version: $(grep -o '"version"[^,]*' "$EXT/manifest.json" | head -1)"

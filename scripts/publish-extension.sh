#!/bin/sh
# Publish extensions/ab-connect.zip to the Chrome Web Store (API v2).
# CI runs this on every main push that changes the zip
# (.github/workflows/publish-extension.yml); locally:
#
#   sh scripts/pack-extension.sh         # build the zip (bump the manifest version first)
#   sh scripts/publish-extension.sh      # upload + submit for review
#   sh scripts/publish-extension.sh --draft
#   python3 scripts/cws.py status        # what the store currently has
#
# Credentials and one-time setup: extensions/store/PUBLISHING.md.
set -eu
cd "$(dirname "$0")/.."
[ -f extensions/ab-connect.zip ] || { echo "error: run scripts/pack-extension.sh first" >&2; exit 1; }
exec python3 scripts/cws.py publish "$@"

#!/bin/sh
# Run tab-operator-live.mjs on the build host against a branch build:
#   ./tab-operator-live.sh <chrome-use binary> [extension git ref, default HEAD]
# Copies the binary into a fresh ChromeUse.app (stage-bin.sh; macOS kills a
# binary overwritten in place), stages the extension from git, and runs the check. One browser run
# per host at a time.
set -eu
cd "$(dirname "$0")"
BIN=${1:?usage: $0 <chrome-use binary> [extension ref]}; REF=${2:-HEAD}
export PATH="$HOME/.local/bin:/opt/homebrew/bin:$PATH"
export CHROME="${CHROME:-$HOME/.chrome-use/browsers/chrome-155.0.8059.39/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing}"
export WORK="$(mktemp -d /tmp/op-XXXXXX)"; export OUT="${OUT:-$WORK/out}"
mkdir -p "$WORK/bin" "$WORK/ext"
. ./stage-bin.sh
stage_host_bin "$BIN" "$WORK"
"$HOST_BIN" --version
ROOT=$(git rev-parse --show-toplevel)
git -C "$ROOT" archive "$REF" extensions/ab-connect | tar -x -C "$WORK/ext"
export EXT_DIR="$WORK/ext/extensions/ab-connect"
rm -f "$EXT_DIR"/*.test.js
grep '"version"' "$EXT_DIR/manifest.json"
node tab-operator-live.mjs

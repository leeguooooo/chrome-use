#!/bin/sh
# botcheck with the position marker off and on (#851). Isolated GUI host only:
# isolated HOME, throwaway Chrome for Testing profile per run.
#   CHROME_USE_ISOLATED_GUI_HOST=1 CU=<chrome-use> CHROME=<binary> sh botcheck.sh <out-dir>
set -eu
[ "${CHROME_USE_ISOLATED_GUI_HOST:-}" = 1 ] || { echo "isolated GUI host only" >&2; exit 2; }
OUT=$1; mkdir -p "$OUT"
for MODE in 0 1; do
  RIG=$(mktemp -d /tmp/cubc-XXXX); mkdir -p "$RIG/home"
  cu() { HOME="$RIG/home" CHROME_USE_AUTO_UPGRADE=0 CHROME_USE_NO_UPDATE_CHECK=1 AGENT_BROWSER_SHOW_POSITION=$MODE "$CU" --session bc$MODE "$@"; }
  cu --launch --headed --executable-path "$CHROME" --profile "$RIG/profile" open https://botcheck.leeguoo.com/ >/dev/null
  sleep 2
  cu click '#t1' >/dev/null
  cu type '#t2' 'Hi #你好' >/dev/null
  cu click '#t3list .row:nth-child(37) button' >/dev/null
  cu select '#t4' kyoto >/dev/null
  sleep 2
  cu --json eval "document.getElementById('report').textContent" > "$OUT/report-$MODE.json"
  cu close >/dev/null 2>&1 || true
  rm -rf "$RIG"
done

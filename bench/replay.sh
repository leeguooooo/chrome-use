#!/usr/bin/env bash
# Fixed-sequence replay harness — NO model in the loop.
# Answers "did our change help": per-call wall ms + bytes returned to the model.
# Usage: bench/replay.sh <task-file> [label]
#   task-file: one chrome-use argv per line, '#' comments ignored.
# Emits TSV to bench/results/<label>-<ts>.tsv and a median/total summary.
set -u
TASK="${1:?usage: replay.sh <task-file> [label]}"
LABEL="${2:-$(basename "$TASK" .txt)}"
BIN="${CHROME_USE_BIN:-chrome-use}"
OUT="$(dirname "$0")/results"; mkdir -p "$OUT"
TSV="$OUT/$LABEL-$(date +%Y%m%d-%H%M%S).tsv"
printf 'n\tms\tbytes\tcmd\n' > "$TSV"
# Warm the daemon before timing anything. A cold daemon costs ~1.5s of spawn and
# session rebind, and it lands entirely on whichever command happens to be first
# — which is how `navigate` got mistaken for a slow command when the real cost
# was the process start in front of it. Not counted.
eval "$BIN eval 1+1" >/dev/null 2>&1 || true
n=0
while IFS= read -r line; do
  case "$line" in ''|\#*) continue;; esac
  n=$((n+1))
  s=$(python3 -c 'import time;print(time.time())')
  out=$(eval "$BIN $line" 2>&1)
  ms=$(python3 -c "import time;print(int((time.time()-$s)*1000))")
  printf '%s\t%s\t%s\t%s\n' "$n" "$ms" "${#out}" "$line" >> "$TSV"
done < "$TASK"
python3 - "$TSV" <<'PY'
import sys,csv,statistics as st
rows=list(csv.DictReader(open(sys.argv[1]),delimiter='\t'))
ms=[int(r['ms']) for r in rows]; by=[int(r['bytes']) for r in rows]
print(f"file: {sys.argv[1]}")
print(f"calls={len(rows)}  wall_total={sum(ms)/1000:.1f}s  ms_median={st.median(ms):.0f}  ms_p90={sorted(ms)[int(len(ms)*.9)]}")
print(f"bytes_total={sum(by)}  bytes_median={st.median(by):.0f}  bytes_max={max(by)}")
PY
echo "-> $TSV"

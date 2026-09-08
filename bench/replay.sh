#!/usr/bin/env bash
# Fixed-sequence replay harness — NO model in the loop.
# Answers "did our change help": per-call wall ms + bytes returned to the model,
# plus whether the task actually finished and under which conditions it ran.
#
# Usage: bench/replay.sh <task-file> [label]
#   task-file: one chrome-use argv per line, '#' comments ignored.
#              `#! assert <expect-args>` lines are the task's end-state check,
#              run after the sequence (see bench/README.md).
#
# Env:
#   CHROME_USE_BIN   binary under test (default: chrome-use on PATH — but see
#                    the README: benchmarking the installed release against repo
#                    source measures the version skew, not the change)
#   BENCH_WARMUP=0   skip the warmup call, i.e. measure a COLD daemon on purpose
#
# Emits TSV to bench/results/<label>-<ts>.tsv and a summary.
#
# Why the extra columns (issue #231): a run that gave up halfway used to look
# BETTER than one that finished, because it made fewer calls and returned fewer
# bytes. And three wrong conclusions in the first pass came from not recording
# whether the daemon was warm or which binary answered. Numbers without those
# facts are not comparable across runs, so the harness records them rather than
# the README warning about them.
set -u
TASK="${1:?usage: replay.sh <task-file> [label]}"
LABEL="${2:-$(basename "$TASK" .txt)}"
BIN="${CHROME_USE_BIN:-chrome-use}"
OUT="$(dirname "$0")/results"; mkdir -p "$OUT"
TSV="$OUT/$LABEL-$(date +%Y%m%d-%H%M%S).tsv"

BIN_PATH=$(command -v "$BIN" 2>/dev/null || printf '%s' "$BIN")
BIN_VERSION=$(eval "$BIN --version" 2>/dev/null | head -1)
LOADAVG=$(cut -d' ' -f1-3 /proc/loadavg 2>/dev/null \
  || uptime | sed 's/.*load averages*: //' 2>/dev/null || echo unknown)

# Warm the daemon before timing anything. A cold daemon costs ~1.5s of spawn and
# session rebind, and it lands entirely on whichever command happens to be first
# — which is how `navigate` got mistaken for a slow command when the real cost
# was the process start in front of it. Not counted, and recorded either way:
# a cold run is a legitimate thing to measure, but only if you can tell later
# that that is what you measured.
WARM=true
if [ "${BENCH_WARMUP:-1}" = "0" ]; then
  WARM=false
else
  eval "$BIN eval 1+1" >/dev/null 2>&1 || true
fi

{
  printf '# task\t%s\n' "$TASK"
  printf '# label\t%s\n' "$LABEL"
  printf '# started\t%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  printf '# binary\t%s\n' "$BIN_PATH"
  printf '# version\t%s\n' "${BIN_VERSION:-unknown}"
  printf '# warm\t%s\n' "$WARM"
  printf '# loadavg\t%s\n' "$LOADAVG"
} > "$TSV"
printf 'n\tms\tbytes\trc\tcmd\n' >> "$TSV"

ASSERTIONS=()
n=0
while IFS= read -r line; do
  case "$line" in
    '#!'*)
      directive="${line#\#!}"
      directive="${directive# }"
      case "$directive" in
        assert\ *) ASSERTIONS+=("${directive#assert }");;
        *) printf 'unknown directive: %s\n' "$directive" >&2;;
      esac
      continue
      ;;
    ''|\#*) continue;;
  esac
  n=$((n+1))
  s=$(python3 -c 'import time;print(time.time())')
  out=$(eval "$BIN $line" 2>&1); rc=$?
  ms=$(python3 -c "import time;print(int((time.time()-$s)*1000))")
  printf '%s\t%s\t%s\t%s\t%s\n' "$n" "$ms" "${#out}" "$rc" "$line" >> "$TSV"
done < "$TASK"

# The end-state check. A task with no assertion is recorded as `none` rather
# than as a pass: "we never checked" and "it worked" must not look the same in
# the results file, which is the whole point of this column.
verdict=none
if [ ${#ASSERTIONS[@]} -gt 0 ]; then
  verdict=pass
  for a in "${ASSERTIONS[@]}"; do
    if eval "$BIN expect $a" >/dev/null 2>&1; then
      printf '# assert\tpass\t%s\n' "$a" >> "$TSV"
    else
      printf '# assert\tFAIL\t%s\n' "$a" >> "$TSV"
      verdict=FAIL
    fi
  done
fi
printf '# verdict\t%s\n' "$verdict" >> "$TSV"

python3 - "$TSV" <<'PY'
import sys, statistics as st

path = sys.argv[1]
meta, rows = {}, []
for raw in open(path):
    line = raw.rstrip('\n')
    if line.startswith('#'):
        parts = line.lstrip('#').strip().split('\t')
        if parts and parts[0] not in ('assert',):
            meta[parts[0]] = '\t'.join(parts[1:])
        continue
    parts = line.split('\t')
    if not parts or parts[0] == 'n':
        continue
    rows.append(parts)

ms = [int(r[1]) for r in rows]
by = [int(r[2]) for r in rows]
rcs = [int(r[3]) for r in rows]
failed = [i for i, rc in enumerate(rcs) if rc != 0]

print(f"file: {path}")
print(f"binary: {meta.get('binary','?')}  version: {meta.get('version','?')}  "
      f"warm: {meta.get('warm','?')}  loadavg: {meta.get('loadavg','?')}")
if not rows:
    print("no calls recorded")
    raise SystemExit(0)
print(f"calls={len(rows)}  wall_total={sum(ms)/1000:.1f}s  "
      f"ms_median={st.median(ms):.0f}  ms_p90={sorted(ms)[int(len(ms)*.9)]}")
print(f"bytes_total={sum(by)}  bytes_median={st.median(by):.0f}  bytes_max={max(by)}")
# Failed calls are counted apart from the total, never folded into it: the cost
# of a failure and its recovery is part of what a task really costs, and hiding
# it makes a run that gave up look cheap (issue #231).
ok = len(rows) - len(failed)
print(f"succeeded={ok}  failed={len(failed)}" +
      (f"  (calls {', '.join(str(i+1) for i in failed)})" if failed else ""))
verdict = meta.get('verdict', 'none')
if verdict == 'pass':
    print("verdict: PASS — the task reached its asserted end state")
elif verdict == 'FAIL':
    print("verdict: FAIL — the sequence ran but the end state is wrong; "
          "these numbers are not comparable with a passing run")
else:
    print("verdict: none — this task has no `#! assert` line, so nothing "
          "checked that it actually finished")
PY
echo "-> $TSV"

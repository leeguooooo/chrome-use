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

# Split once, with shell quoting rules but WITHOUT shell evaluation: task files
# and CHROME_USE_BIN are ordinary text, and running them through `eval` would let
# a stray `;` or `$(...)` in either execute as a command. python3 is already a
# dependency of this script, so shlex does the splitting.
# Sets ARGV to the split result (a global, not a nameref: macOS still ships
# bash 3.2, where `local -n` does not exist).
split_into_argv() {
  ARGV=()
  local part
  while IFS= read -r -d '' part; do
    ARGV[${#ARGV[@]}]="$part"
  done < <(python3 -c 'import shlex,sys;print("\0".join(shlex.split(sys.argv[1])),end="")' "$1"; printf '\0')
}

split_into_argv "$BIN"
BIN_ARGV=("${ARGV[@]}")
BIN_PATH=$(command -v "${BIN_ARGV[0]}" 2>/dev/null || printf '%s' "${BIN_ARGV[0]}")
BIN_VERSION=$("${BIN_ARGV[@]}" --version 2>/dev/null | head -1)
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
  "${BIN_ARGV[@]}" eval 1+1 >/dev/null 2>&1 || true
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
# `set -u` plus an empty array is an error on bash 3.2, and a task line that
# splits to nothing (only quotes, say) would trip it.

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
  split_into_argv "$line"
  if [ ${#ARGV[@]} -eq 0 ]; then continue; fi
  CALL_ARGV=("${ARGV[@]}")
  s=$(python3 -c 'import time;print(time.time())')
  out=$("${BIN_ARGV[@]}" "${CALL_ARGV[@]}" 2>&1); rc=$?
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
    split_into_argv "$a"
    if "${BIN_ARGV[@]}" expect "${ARGV[@]}" >/dev/null 2>&1; then
      printf '# assert\tpass\t%s\n' "$a" >> "$TSV"
    else
      printf '# assert\tFAIL\t%s\n' "$a" >> "$TSV"
      verdict=FAIL
    fi
  done
fi
printf '# verdict\t%s\n' "$verdict" >> "$TSV"

python3 - "$TSV" <<'PY'
import math, sys, statistics as st

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
# Nearest-rank p90: `int(len * .9)` picks the maximum for 10 calls (index 9),
# which is a different statistic than the one the label promises.
p90 = sorted(ms)[math.ceil(len(ms) * 0.9) - 1]
print(f"calls={len(rows)}  wall_total={sum(ms)/1000:.1f}s  "
      f"ms_median={st.median(ms):.0f}  ms_p90={p90}")
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

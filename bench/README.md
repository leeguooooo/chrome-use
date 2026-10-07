# bench — browser-agent round-trip benchmarks

> **New here?** Start with the tracking issue
> [#232](https://github.com/leeguooooo/chrome-use/issues/232): why this work
> exists, what shipped, and which open issue to pick up next.

Measures the two costs an agent actually pays per browser task:
**round trips to the model** and **bytes returned into its context**.
Wall clock is secondary — it mixes model speed with harness speed.

## Compare against another agent's browser tool

`codex_rollout_stats.py` reads a Codex rollout log and reports its round
trips, returned bytes, tool-exec time and model think time:

    grep -rl '"browser_use' ~/.codex/sessions ~/.codex/archived_sessions
    bench/codex_rollout_stats.py <that file>

Reference line measured 2026-09-07 on a form-filling task: 81 round trips,
median 301 bytes returned, tool exec median 0.58s, **model think median
6.01s**. Model time was 22x tool time — which is why round trips, not
transport, are what to optimize.

## Compare us before and after a change

No model in the loop, so the delta is unambiguously the change:

    bench/replay.sh bench/tasks/hn.txt before
    # ... make the change, rebuild ...
    bench/replay.sh bench/tasks/hn.txt after

A task file is one `chrome-use` argv per line. Set `CHROME_USE_BIN` to a
freshly built binary — **not** the one on PATH. Benchmarking an installed
release against repo source silently measures the version skew instead of
the change.

`bench/cu` is the same wrapper for one-off interactive calls; it appends to
`$CU_LOG`.

The harness fires an uncounted warmup call first: a cold daemon costs ~1.5s of
spawn plus session rebind, and it lands entirely on whichever command is first.
That artifact is what first made `navigate` look ~5x slower than `snapshot`.
Set `BENCH_WARMUP=0` to measure a cold daemon deliberately — the result file
records which of the two you did.

Run on an idle machine and repeat 3x — medians, not single runs.

## What a result file records, and why

Round trips and bytes alone rank a run that gave up halfway **above** one that
finished: fewer calls, fewer bytes. So every run also records whether the task
actually reached its end state, and under what conditions the numbers were
taken (issue #231).

A task file declares its end state with `#! assert <expect-args>`, run after the
sequence through `chrome-use expect`:

    #! assert url contains news.ycombinator.com/ask
    #! assert text body contains "Ask HN"
    navigate https://news.ycombinator.com
    snapshot -i

The TSV then carries, above the per-call rows:

    # binary    /path/to/the/binary/that/answered
    # version   chrome-use 1.5.105
    # warm      true
    # loadavg   0.31 0.28 0.24

and below them one `# assert` line per check plus a `# verdict`. The summary
prints `PASS`, `FAIL`, or `none` — a task with no assertion is never counted as
a pass, because "we did not check" and "it worked" must not look the same.

Per-call rows carry the exit code, and the summary counts failed calls
separately from the total. A failure and the recovery after it are part of what
a task really costs; folding them into one number is how 17 calls containing 6
failures read as a cheaper run than 12 clean ones.

Three wrong conclusions in the first pass — "wall clock halved", "navigate is
5x slower", "the repo source is slower than the release" — each looked entirely
reasonable in isolation and came from not recording these facts. A README
warning did not prevent them; the harness recording the facts does.

## Task-level evidence with explicit runs

Use the new collector for accurate UTF-8 byte counts and reproducible provenance:

```sh
python3 bench/run-task.py bench/tasks/hn.txt \
  --binary cli/target/release/chrome-use --output /tmp/hn-warm.tsv --run-id hn-warm-1
python3 bench/task-metrics.py /tmp/hn-warm.tsv
python3 -m unittest discover -s bench -p test_task_metrics.py -v
```

One TSV is one task run. The collector refuses to overwrite an existing file.
It records the executable SHA-256, version output, tracked working-tree source
SHA-256, call exit codes, exact output bytes including newlines, and task wall
time including final assertions. Source hashing includes unstaged tracked edits;
use `git add -N` for new source files. This identifies the measured source, but
does not prove that a binary was built from it: match a build receipt separately.
Stdout and stderr are combined, so the byte count is CLI response transport cost,
not an exact model input or token count.

Warmup success is recorded, rather than claiming that an attempted warmup worked.
`--cold` only skips warmup: it records `cold_requested_unverified` and does not
stop someone else's daemon. Establish an isolated stopped daemon before claiming
an actually cold run. Existing replay TSVs remain readable, but their shell
character lengths appear as `legacy_response_units`, with UTF-8 bytes null.

The summary distinguishes `cli_calls` from `model_round_trips`. A fixed replay
has no model and reports null for model turns, rather than treating every CLI
command as one. `cli_calls` excludes verification and warmup calls; those are reported separately
as `assertion_cli_calls` and `warmup_cli_calls`. `feature_calls` counts calls whose
first argument is batch/script, or which have the `--observe` flag; `feature_rates` divides those counts by CLI calls. It does not count
nested browser actions or prove that an agent adopted a hint. Protocol
`action_outcome_unknown`/`outcome_unknown` codes are counted only in structured
JSON response status fields. Non-JSON output or JSON without the protocol success field is unclassified, and
legacy files have unknown counts null. Page text is never an error classifier.

`call_p50_ms` and `call_p95_ms` use nearest rank. `call_time_sum_ms` sums measured
command subprocess durations, while `task_wall_ms` includes collector overhead
and postcondition checks. Neither is model thinking time or daemon exclusive
time. Keep warm and cold observations separate; compare repeated equivalent
successful tasks. A passing final postcondition can coexist with earlier CLI
errors: `successful_task` records that the asserted task finished, and
`all_cli_calls_succeeded`, failure counts and warnings retain its recovery cost.
No assertion, an empty replay, or a failing assertion is never a successful task.

A caller may provide a complete explicit trace for the same run:

```jsonl
{"event":"run_start","run_id":"hn-warm-1"}
{"event":"model_turn","run_id":"hn-warm-1","id":"turn-1"}
{"event":"tool_call","run_id":"hn-warm-1","id":"call-1"}
{"event":"tool_call","run_id":"hn-warm-1","id":"call-2"}
{"event":"run_end","run_id":"hn-warm-1"}
```

```sh
python3 bench/task-metrics.py /tmp/hn-warm.tsv --trace /tmp/hn-warm-trace.jsonl
```

`model_turn` means one completed caller model decision, not one nested tool call.
The example records one model turn and two tool calls. Event IDs must be unique
within their type. Mixed run IDs, nested boundaries and unsupported events are
rejected. The caller is responsible for emitting the complete trace; no model
counts are inferred from gaps in a rollout log. The old
`codex_rollout_stats.py` is historical exploratory tooling, not authoritative
task/model attribution.

Generate four equivalent local tasks to compare separate action/snapshot,
action with observe, batch, and script:

```sh
python3 bench/fixture-server.py
# Use the printed port in another terminal:
python3 bench/efficiency-tasks.py \
  http://127.0.0.1:PORT/agent-efficiency.html /tmp/efficiency-tasks
python3 bench/run-task.py /tmp/efficiency-tasks/observe.txt \
  --binary cli/target/release/chrome-use --output /tmp/observe-1.tsv
python3 bench/task-metrics.py /tmp/observe-1.tsv
```

Each task resets the count by navigating to the same fixture, increments exactly
three times, and checks `#count` equals `3`. Repeat each variant with alternating
order and the same isolated session. These replays quantify CLI calls, response
bytes and latency; they do not establish model round-trip savings without a
caller trace. Coordinate live-browser ownership before running them.

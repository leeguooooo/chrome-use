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

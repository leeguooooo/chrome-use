# bench — browser-agent round-trip benchmarks

> **New here?** Read [../HANDOFF.md](../HANDOFF.md) first: why this work exists, what
> shipped, and which open issue to pick up next.

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

Run on an idle machine and repeat 3x — medians, not single runs.

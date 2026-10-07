# Agent efficiency fixture acceptance, 2026-10-07

The baseline and candidate both completed all 40 SSH-host runs and all 40 local
runs with no failed calls. This is a no-model counter fixture, not a comparison
with browser-use or Browser Harness. Model round trips remain unknown.

For this known sequence, batch/script reduced CLI invocations and returned bytes.
The new native implementation did not demonstrate a wall-time improvement in
this sample. Extra timing/advisory metadata slightly increased JSON bytes.

## Conditions

- Baseline `fed19908`, candidate native source `480de6c3`; both release binaries
  were freshly built by `scripts/remote-cargo.py` and SHA-256 verified.
- Candidate build receipt source hash
  `5ca4fc3638fb9e7fe60045c491e70cb65bba8212d714489990d790f7596bcb44`.
- Each group has five runs, alternating baseline/candidate order. Dedicated
  daemons, socket directory and launched headless Chrome isolate the test.
- Each run navigates to the same loopback counter, clicks three times, and checks
  `expect text #count equals 3`. Warmup and assertion calls are reported separately.
- CLI calls below include navigation; bytes are actual combined output UTF-8
  bytes. They are not token counts. Tool calls are not model turns.
- SSH host: arm64, macOS 27, 10 CPUs. Loadavg start 4.02/16.20/25.74,
  end 2.79/13.65/24.14. The five-sample nearest-rank p95 is the maximum,
  so this is descriptive evidence, not a stable tail-latency estimate.
- Local run started under loadavg over 300 and showed large timing variation.
  It validates behavior and byte/call collection; its wall times are excluded
  from speed conclusions. Early invalid-argument/startup attempts are not passes
  and were not included among the successful fixture runs.

## SSH-host results

| Binary | Pattern | Passed | CLI calls | Median output bytes | Task p50 ms | Task p95 ms |
|---|---|---|---|---|---|---|
| baseline | separate | 5/5 | 7 | 3082 | 1129.1 | 1194.7 |
| baseline | observe | 5/5 | 4 | 3170 | 2286.9 | 2314.0 |
| baseline | batch | 5/5 | 2 | 1122 | 817.9 | 826.5 |
| baseline | script | 5/5 | 2 | 356 | 823.9 | 827.0 |
| candidate | separate | 5/5 | 7 | 3287 | 1129.8 | 1362.6 |
| candidate | observe | 5/5 | 4 | 3294 | 2314.0 | 2318.0 |
| candidate | batch | 5/5 | 2 | 1150 | 834.5 | 906.9 |
| candidate | script | 5/5 | 2 | 399 | 831.7 | 832.6 |

`observe` pays for bounded action-effect settling; it reduces outer calls but
is not automatically the fastest no-model replay. Known sequences should use
batch/script with semantic gates and a final postcondition. Keep `observe` for
steps whose reaction informs the next decision.

The record is [agent-efficiency-2026-10-07.json](agent-efficiency-2026-10-07.json).
It keeps per-run results, binary hashes, build-source hashes and machine
provenance. Collector checkout hashes remain explicitly unverified as binary
source; the separate build receipts provide the verified association. Temporary
paths and host names are omitted from this public report.

## Reproduce

Start `python3 bench/fixture-server.py`, then generate tasks with:

```sh
python3 bench/efficiency-tasks.py http://127.0.0.1:PORT/agent-efficiency.html /tmp/tasks
python3 bench/run-task.py /tmp/tasks/separate.txt --binary /path/to/fresh/binary --output /tmp/run.tsv
python3 bench/task-metrics.py /tmp/run.tsv
```

Use an isolated test session and browser, verify the exact binary, warm it first,
alternate run order and keep the same postcondition. Do not stop another
agent's browser to manufacture an idle result.

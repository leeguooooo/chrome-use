# Profiling

Capture Chrome DevTools performance profiles during browser automation for performance analysis.

**Related**: [commands.md](commands.md) for full command reference, [SKILL.md](../SKILL.md) for quick start.

## Contents

- [Command timing and task efficiency](#command-timing-and-task-efficiency)
- [Basic Profiling](#basic-profiling)
- [Profiler Commands](#profiler-commands)
- [Categories](#categories)
- [Use Cases](#use-cases)
- [Output Format](#output-format)
- [Viewing Profiles](#viewing-profiles)
- [Limitations](#limitations)

## Command timing and task efficiency

Timed daemon replies carry `timing.ms` (command wall time), `cdpCalls`,
`cdpMs` (sum of completed foreground CDP request durations), `cdpBusyMs` (union of those
request intervals clipped to command wall time), `nonCdpMs` (`ms` minus
`cdpBusyMs`), and the three `slowest` methods. Concurrent requests may make
`cdpMs` greater than wall time. `nonCdpMs` includes waits and other elapsed
work; neither field measures CPU use or model latency. Only completed,
recorded foreground intervals contribute to CDP occupancy. Spawned background
tasks do not inherit this recorder; background CDP work is not included.
`nonCdpMs` is the remaining wall duration, not a pure daemon processing cost.

Ordinary CLI `--json` replies use a `success`/`data`/`timing` envelope. `batch --json` prints an array of `{command,success,result,error}` entries; `script --json` prints the bare program result (`ok`, `return`, `logs`, `error`, `advisories`, and other program fields). Batch and script CLI output have no top-level `timing`.

The daemon appends action, session, outcome and timing to
`~/.chrome-use/timing.jsonl`, rotating at 20 MB. It omits URLs, selectors and
page content. `AGENT_BROWSER_TIMING_LOG=0` disables the log.

For a task comparison, use a freshly built binary and record its version.
Separate cold and warm runs, use the same initial state and task, alternate
variants, and record final success, unknown outcomes, tool calls, CDP calls,
returned bytes and task wall time. Model round trips require caller trace
records; daemon or HTTP calls cannot establish them. Token counts require a
named tokenizer or model usage report. A shorter failed run is not a speedup.
The repository's `bench/README.md` describes the replay harness.

Summarize an explicit replay run from the repository root:

```bash
python3 bench/run-task.py bench/tasks/hn.txt \
  --binary cli/target/release/chrome-use --output /tmp/hn-warm.tsv --run-id hn-warm-1
python3 bench/task-metrics.py /tmp/hn-warm.tsv
# Optional: a complete caller trace for this same run
python3 bench/task-metrics.py /tmp/hn-warm.tsv --trace /tmp/hn-warm-trace.jsonl
```

One TSV represents one run; existing output files are not overwritten. Without
a complete explicit caller trace, `model_round_trips` is null. The collector's
UTF-8 byte count combines CLI stdout and stderr, not exact model input. Warmup
success is recorded; `--cold` only skips warmup and does not prove a stopped
daemon. Binary/source hashes identify inputs; match a build receipt separately
to prove their relationship. A final passing assertion establishes task success;
failed earlier calls and unknown outcomes still contribute to its cost.

## Basic Profiling

```bash
# Start profiling
chrome-use profiler start

# Perform actions
chrome-use navigate https://example.com
chrome-use click "#button"
chrome-use wait --text "Ready"

# Stop and save
chrome-use profiler stop ./trace.json
```

## Profiler Commands

```bash
# Start profiling with default categories
chrome-use profiler start

# Start with custom trace categories
chrome-use profiler start --categories "devtools.timeline,v8.execute,blink.user_timing"

# Stop profiling and save to file
chrome-use profiler stop ./trace.json
```

## Categories

The `--categories` flag accepts a comma-separated list of Chrome trace categories. Default categories include:

- `devtools.timeline`: standard DevTools performance traces
- `v8.execute`: time spent running JavaScript
- `blink`: renderer events
- `blink.user_timing`: `performance.mark()` / `performance.measure()` calls
- `latencyInfo`: input-to-latency tracking
- `renderer.scheduler`: task scheduling and execution
- `toplevel`: broad-spectrum basic events

Several `disabled-by-default-*` categories are also included for detailed timeline, call stack, and V8 CPU profiling data.

## Use Cases

### Diagnosing Slow Page Loads

```bash
chrome-use profiler start
chrome-use navigate https://app.example.com
chrome-use wait --load networkidle
chrome-use profiler stop ./page-load-profile.json
```

### Profiling User Interactions

```bash
chrome-use navigate https://app.example.com
chrome-use profiler start
chrome-use click "#submit"
chrome-use wait --text "Saved"
chrome-use profiler stop ./interaction-profile.json
```

### CI Performance Regression Checks

```bash
#!/bin/bash
chrome-use profiler start
chrome-use navigate https://app.example.com
chrome-use wait --load networkidle
chrome-use profiler stop "./profiles/build-${BUILD_ID}.json"
```

## Output Format

The output is a JSON file in Chrome Trace Event format:

```json
{
  "traceEvents": [
    { "cat": "devtools.timeline", "name": "RunTask", "ph": "X", "ts": 12345, "dur": 100, ... },
    ...
  ],
  "metadata": {
    "clock-domain": "LINUX_CLOCK_MONOTONIC"
  }
}
```

The `metadata.clock-domain` field is set based on the host platform (Linux or macOS). On Windows it is omitted.

## Viewing Profiles

Load the output JSON file in any of these tools:

- **Chrome DevTools**: Performance panel > Load profile (Ctrl+Shift+I > Performance)
- **Perfetto UI**: https://ui.perfetto.dev/: drag and drop the JSON file
- **Trace Viewer**: `chrome://tracing` in any Chromium browser

## Limitations

- Only works with Chromium-based browsers (Chrome, Edge). Not supported on Firefox or WebKit.
- Trace data accumulates in memory while profiling is active (capped at 5 million events). Stop profiling promptly after the area of interest.
- Data collection on stop has a 30-second timeout. If the browser is unresponsive, the stop command may fail.

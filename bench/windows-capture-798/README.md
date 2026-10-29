# Windows relay screenshot diagnostic (#798)

This is a manual experiment, not a production workaround or a stealth score.
It uses a freshly built Release CLI, Chrome for Testing, a new profile for every
sample, and a synthetic loopback report. It refuses to run outside an ephemeral
Windows GitHub Actions job. No AWS instance or existing browser is used.

The proposed claim is narrow: a particular known window state and capture type
can time out while a read on the same debugger attachment still answers. A
successful matrix means that matrix did not reproduce the report. It cannot
prove the reporter's Windows account, physical display, other extensions or
original report has been fixed.

## Evidence contract

The manual workflow `windows-capture-probe.yml` builds the exact checked-out CLI
in Release mode. It records the source SHA, binary and Chrome hashes/versions,
OS/image version, CPU count, memory state, raw CPU counters, Python and decoder
versions. It does not use the CLI on PATH. Setup must produce and decode a green
warmup screenshot before the long report is loaded.

For each repetition, the order of normal, covered and minimized states rotates.
Each state has a viewport, full-page and off-screen selector case. Every case
uses a fresh Chrome/profile/session, so a timed-out screenshot is never followed
by a second capture or fallback in the same profile. There are five repetitions
by default; no timing winner or speedup is inferred from a single result.

The controller locates exactly one fixture-titled HWND owned by the Chrome PID
it started. It confirms native Win32 state before capture:

- Normal: restored and its center belongs to that window or its child.
- Covered: a fixture-owned topmost window covers its complete outer rectangle;
  `WindowFromPoint` identifies that cover.
- Minimized: `IsIconic` is true.

These state changes are intentional and limited to the disposable CI desktop.
They are not a proposed activation/minimization recovery on a user's machine.
If the CI desktop cannot provide the requested state, the probe stops and reports
`INCONCLUSIVE`, retaining the setup failure instead of counting it as a pass.

The long document's row count and readyState must be verified. Main-report
capture start and completion/error events must also arrive with matching capture
IDs. Missing diagnostic transport or absent capture work is inconclusive, even
if a CLI process exits successfully. Successful images
are decoded and checked for dimensions plus colored sentinel pixels, not merely
file existence. A long-report viewport pixel differs from the warmup page, so a
stale warmup frame cannot pass by having the same green header. Full-page images
also check a middle row and the bottom target. Full-resolution captures avoid the CLI's default downscale.
The controlled raster is bounded at 200 million pixels, including DPR; use fewer
rows if a larger DPR would exceed that bound. This limit bounds the experiment,
not the product's supported report size.

## Same-attachment diagnostic hook

A generated, unshipped copy of the relay adds one observer around the existing
`sendTabCommand` promise. The production repository source is not modified.
The generated diff and SHA-256 are retained for every case.

Only an agent-owned tab on the exact fixture's loopback origin is observed.
If `Page.captureScreenshot` is still pending after two seconds, the same
service worker sends one direct, read-only `Runtime.evaluate` using the same
`chrome.debugger` attachment. It records send/answer times, readyState, row
count and visibility. No extra debugger client is attached to the tab, no
health reset is used for this read, and no page listener/global/input is added.

The observer sends its small JSON records to the fixture's local HTTP sink.
The generated extension grants only loopback host access for that transfer.
The fixture diagnostic therefore differs from the shipped extension; it is
not evidence for a general stealth claim. No screenshot bytes, user reports,
credentials or environment token dumps are sent to the sink.

A raw DOM read could itself wake a renderer. The manual workflow's
`pending_dom_control: false` option disables that read while retaining capture
event logging and the same setup, for an observer-effect control. Compare that
control if captures complete around the injected read or an instrumented run
does not reproduce the report; do not call that difference a cause without
repeatable evidence. The mode is recorded in metadata and every case.

The ordinary post-capture CLI DOM read is retained separately. It may run the
product's attachment-health recovery and is never presented as proof that the
original attachment answered during the pending capture.

## Run and interpret

After the workflow is registered on the default branch, dispatch it with
`samples` (1–5), `rows` (100–3000) and DPR (1 or 2). Defaults are five samples,
1000 rows and DPR 1. All CLI subprocess exit codes and stdout/stderr are saved;
the report includes every failure and incorrect image. A 30-minute experiment
budget stops before another case starts and writes a partial, inconclusive
summary. Per-case results and incremental progress are retained before that
point; the job has a separate 60-minute outer limit.

`results/summary.json` gives:

- `PASS`: all requested setups and captures passed; the report was not reproduced.
- `ISSUES`: at least one confirmed setup had a failed or incorrect capture.
- `INCONCLUSIVE`: setup or cleanup could not be confirmed.
- `matchingReportSignature`: count of cases with a timed-out capture and a
  matching DOM reply while that capture was still pending. The error must name
  `Page.captureScreenshot` and its 25000ms budget, with observed elapsed time
  between 24 and 35 seconds; attachment-recovery timeouts do not qualify.

A non-PASS result makes the diagnostic job fail so a missing result cannot look
green. Artifacts are uploaded even on failure. A matching signature reproduces
an observed symptom; it does not establish a compositor, GPU, memory, relay or
window-state cause by itself. Compare the full matrix and resource records
before choosing a fix.

The harness changes only its own native-host HKCU registrations during a case,
then restores their previous values. It terminates only the Chrome process tree
it created. No `.bat` or `.cmd` launcher is shipped; Chrome starts the native
host EXE through the existing Windows origin-argument entry point.

## Local checks without a browser

```sh
python3 bench/windows-capture-798/test_probe.py
actionlint .github/workflows/windows-capture-probe.yml
```

These exercise the synthetic server, generated worker code and ownership/origin/
pending-promise guards. They do not launch Chrome or count as Windows acceptance.

# Skill-loading pilot

This is a real-model, local-browser pilot, not a no-model replay benchmark.
It consumes the current Codex account's model quota. Obtain evaluation authorization
before running it, and keep raw events outside the repository: the host can include
personal skill metadata even though all fixture data is synthetic.

## Arms

- `old`: core and references from reachable commit `95ea98e7`.
- `new`: core and references copied from the supplied current repository.
- `mcp`: native stdio MCP tools (`--tools all`) and a short observation policy.

The CLI arms explicitly load their assigned core; this tests core content, not
whether the runner discovers the installed skill automatically. The separate
loading audit checks that discovery path. The MCP arm changes both guidance and
interface; it cannot isolate protocol overhead from instruction differences.

All arms use one fixed binary, the same Codex CLI default configuration, a
fresh isolated browser session, a warmup outside the timed model run, and the
same synthetic task. No additional model override is applied. Record the actual
model ID if the host exposes it; the current JSON event stream does not.
Global skill metadata can still be present with `--ignore-user-config`; include
its input cost in the reported totals rather than attributing everything to core.

## Run

Build a fresh binary and copy it to a stable path before any arm. Do not test
against a binary another developer may replace during the run.

```sh
python3 bench/skill-eval/fixture.py
# Use the port printed above. Pick a new output directory per repetition.
python3 bench/skill-eval/run.py --binary /tmp/fixed-chrome-use \
  --repo /path/to/chrome-use --output /tmp/private-skill-eval \
  --port <port> --arm old
# Repeat with --arm new and --arm mcp, sequentially.
```

MCP approval is separate from browser-task authorization. The default runner does
not silently preapprove a newly configured MCP server. If the host rejects a tool
because approval cannot be obtained, mark the arm `SETUP_BLOCKED`, not faster or
less accurate. After explicit user approval, `--approve-mcp` grants permission to
this test server only for that child process; it does not modify user config.

## Acceptance and measurements

Choose the cheapest in-stock product, fill all supplied fields, and submit
exactly once. The model must report the visible receipt. The fixture's separate
`/result/<case>` endpoint checks every submitted value and total submission count;
a model's claim alone is not acceptance. The model must not read fixture source
or directly call the validation endpoint.

Record runtime, CLI/MCP calls, additional guide loads, command failures, and the
host's total input, cached input, and output tokens. Report input minus cached
input separately; total input is not a billing estimate. The CLI wrapper also
records subprocess time and output bytes, which are not model latency.

One run per arm is a smoke test, not evidence of a performance distribution or
non-inferiority. Follow with fresh sessions, repeated trials in counterbalanced
order, and tasks covering dynamic options, failed submissions, auth boundaries,
and canvas before selecting a default or claiming general improvement.

## Trace review is mandatory

The fixture's `accepted` field only confirms application values and submission
count. It does not prove browser use: directly posting the expected body could
also satisfy it. The runner never promotes that field to PASS. A successful
server result produces `NEEDS_TRACE_REVIEW`; failed server checks produce FAIL.

Before marking a run PASS, the coordinator must inspect the raw CLI/MCP events
for the same case URL, required visible UI flow, one submission, and observed
receipt matching the final answer. Direct HTTP submissions, fabricated tool
logs, or missing browser evidence are not passing runs. Preserve the review
alongside the run. This is an evaluation harness with manual trace review, not
an adversarially isolated execution environment.

A run directory must be new; existing artifacts are never overwritten. Old-arm
guides come only from the historical Git tree. `provenance.json` records the
repository revision, dirty-state flag, guide hashes and redacted invocation.
Browser cleanup is attempted in `finally`, including result-reader failures.

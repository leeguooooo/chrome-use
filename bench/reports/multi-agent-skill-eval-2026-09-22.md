# Cross-harness skill evaluation, 2026-09-22

## Decisions supported by this run

Keep the corrected discovery handoff, but do not advertise shorter core text as
universal agent performance improvement. In the recorded DeepSeek basic-task
pair, new core needed fewer tool calls and lower host-reported token cost. Claude
Code did not show that pattern. Native MCP worked but did not consistently reduce
model turns or estimated cost versus CLI. No default interface switch follows.

A concrete defect was observed and handed off for repair: a normal wait-condition
timeout was described as a dead browser connection. One agent waited for `Saved`
after already observing `Delivery saved`, spent 25 seconds on a case-sensitive
mismatch, then received an unjustified reconnect hint. The next text read worked.
The repair must not assert the opposite (that the connection is healthy): the
poller also tolerates failed probes until its deadline.

## Controls and limitations

- Fixed release binary: `1.5.133`, SHA-256
  `f0e094bfaee2e8de4556083db981f2d6749def0e3f2373009dbd24d9fe27b2a7`.
- Old core: historical `95ea98e7`. New core was frozen before the follow-up
  wait guidance.
- Old core SHA-256: `343295b94ad66e55fbd1bbfe41a153d05947282a34b0c2fb2f6db7d69486ac82`.
- New core SHA-256: `2adc8f4cd6d5356b04e2fd3b88ce40d486063e9f89128e7b4d98d2f050d7c538`.
- Clean launched browser sessions, 1280 × 900 viewport, loopback-only pages;
  synthetic values, no real accounts or external actions. Models received only
  their assigned task prompt, not the coordinator conversation or oracle source.
- Basic task: choose the cheapest in-stock product and submit exact form values
  once. Dynamic task: select Tokyo Hub from delayed suggestions, request quantity
  2, submit once, and wait for the saved receipt.
- The coordinator checked server-side values and submission counts independently.
  For JSON-recorded runs, receipt text was also found in actual tool results and
  matched to the final response. Pi phase 1 has wrapper command logs plus the
  final answer, but no full model event stream; its token totals are unavailable.
- The first Pi old-arm directory contained three extra, unrequested references.
  Actual loaded files were byte-identical to the historical tree. Subsequent old
  arms used a historical-only directory. This preparation defect is retained.
- Host load and sleep were material confounders. One Pi run was interrupted in a
  reported sleep window and its initial timeout mechanism failed to bound it.
  It is retained as environment-interrupted, not counted as a guide-caused failure.
  The coordinator separately stopped that session and saved the cleanup result.
- Follow-up runs used a process-group deadline and a temporary macOS idle-sleep
  assertion. This does not prove absence of manual sleep or other interference.
- A no-model calibration under load ~94 measured three click+observe calls at
  0.879 / 0.715 / 0.718 seconds, and snapshots at 0.250 / 0.123 / 0.110 seconds.
  High load alone does not establish that every browser operation was slow.
- Claude's first basic/new run started after its prewarmed tab had disappeared;
  it reopened the task URL. Its extra setup steps are retained, not subtracted.
- Each arm/task has very few observations, ordering was not fully randomized,
  and environment conditions differed. No cross-model speed ranking or general
  success-rate/non-inferiority claim is justified.

## Actual models and permissions

Pi 0.85.1 was explicitly pinned to OpenRouter `deepseek/deepseek-v4.1-flash`,
thinking off. Phase 2 assistant events confirm that model/provider and zero
reasoning tokens. Pi had no native MCP client in this setup; MCP is N/A, not a
failure and not a CLI adapter pretending to be MCP.

Claude Code 2.1.278 child events identify `claude-opus-5[1m]`, canonical
`claude-opus-5`, provider `firstParty`. The parent agent's initial Opus 4.6
self-description was incorrect. Thinking usage was nonzero; the precise thinking
setting was not independently captured. All six child runs used the same default
model configuration and `bypassPermissions`; a later instruction to use only
scoped tool permissions arrived after they had completed. The MCP runs also used
strict process-local server configuration. Actual tool events stayed within the
assigned test tools, but these trials do not validate scoped approval behavior.
Future launches reject broad bypass flags in the coordinator's deadline wrapper.

## Pi observations

| Case | Guide | Result | Outer tool calls | CLI commands (no cleanup) | Recorded seconds |
|---|---|---|---:|---:|---:|
| Basic, phase 1 | Old | Correct, one submission | unavailable | 14 | 52.0 |
| Dynamic, phase 1 | New | Correct, one submission | unavailable | 7 | 41.6 |
| Basic, phase 1 | New | Environment-interrupted, no submission | unavailable | 9 (1 failed) | 9,040; invalid for latency |
| Dynamic, phase 1 | Old | Correct, one submission | unavailable | 9 | 62.6 |
| Basic, phase 2 | New | Correct, one submission | 7 | 10 | 47.72 |
| Basic, phase 2 | Old | Correct, one submission | 10 | 13 | 61.17 |

Phase 2 sums each terminal assistant `message_end.usage` once. Streaming updates
and aggregate end events are not added again; the final assistant message alone
is not the run total.

| Guide, basic phase 2 | Input | Cache read | Output | Total tokens | Host-reported cost |
|---|---:|---:|---:|---:|---:|
| New | 23,871 | 147,456 | 962 | 172,289 | $0.004600218 |
| Old | 36,974 | 288,000 | 1,093 | 326,067 | $0.007065900 |

In this pair the new guide used three fewer outer tool calls and about 35% less
host-reported cost. That is an observed pair, not a general savings guarantee.

## Claude observations

Outer calls count distinct tool-use IDs. CLI commands count the wrapper records;
one Bash call can contain several CLI commands. MCP outer counts include one
ToolSearch per run, followed by 11 native MCP calls. These are different layers,
so an 11-command shell batch must not be labeled 11 model round trips.

| Task | Arm | Result | Outer calls | CLI/native calls | Model turns | Recorded seconds | Host list-cost estimate |
|---|---|---|---:|---:|---:|---:|---:|
| Basic | Old CLI | Correct, one submission | 4 | 11 CLI | 5 | 22.56 | $0.401768 |
| Basic | New CLI | Correct, one submission | 8 | 19 CLI | 9 | 57.92 | $0.511319 |
| Basic | Native MCP | Correct, one submission | 12 | 11 MCP + search | 13 | 27.02 | $0.443924 |
| Dynamic | Old CLI | Correct, one submission | 5 | 8 CLI | 6 | 93.99 | $0.429640 |
| Dynamic | New CLI | Correct, one submission | 6 | 12 CLI (1 failed wait) | 7 | 54.04 | $0.459878 |
| Dynamic | Native MCP | Correct, one submission | 12 | 11 MCP + search | 13 | 26.95 | $0.460569 |

Claude's costs are host list-price estimates, not the user's subscription bill.
The old CLI basic run batched commands into fewer outer calls. The new dynamic
run's redundant, wrong-case wait is inspectable in both command logs and raw
responses: the preceding action observation already contained the saved receipt.
Neither fact establishes a universal preference for a guide or protocol.

## Artifacts and follow-up

Raw logs remain private under the coordinator's local evaluation directory.
The coordinator's `claude-coordinator-audit.json` and
`pi-phase2-coordinator-audit.json` were generated from the original events and
independent oracle responses. The observer did not accept agent summaries as the
source for model identity, aggregate usage, call counts, or successful submission.

No speculative terminal shortcut, relaxed freshness predicate, or default MCP
migration was introduced. The separate wait-diagnostic repair is reviewed and
validated before integration. The discovery-entry PR also fixes historical-only
fixture provisioning, requires explicit browser-trace review before PASS, and
bounds its setup/process cleanup.

## Wait diagnostic repair validation

The follow-up source change keeps wait matching unchanged, reports an unobserved
condition without diagnosing connection health, and replaces the core guide's
literal `Saved` example with an explicit expected-text placeholder. Both READMEs,
command help and the bilingual waiting pages explain case-sensitive matching and
avoiding another wait after the requested receipt is already visible.

The coordinator rebuilt the candidate and used a fresh isolated real browser:
with visible text `Delivery saved.`, `wait --text Saved --timeout 500` failed
with the neutral condition hint; `wait --text "Delivery saved." --timeout 500`
succeeded, and a subsequent text read succeeded in the same session. Cleanup
exited zero. The focused Rust regression also passed and preserves the separate
CDP-timeout diagnostic path. No model rerun or general performance gain is claimed
for this follow-up. The installed public binary remained v1.5.133 at validation;
source integration is distinct from publishing a new binary.

# Skill loading audit and local pilot, 2026-09-21 to 2026-09-22

The core reduction has a loading-path limitation. The repository discovery skill
and the locally installed skill were identical (SHA-256
`f64e2f75a81fdaf19f147af5b1ce0d8984473b3ad0becdf6e460228a0995e12f`):
244 lines and no `skills get core` handoff. The installation entry had replaced
the former thin stub in commit `9fec91c0`. Therefore shrinking core alone does
not show that agents loading the installed entry received fewer instructions.

A candidate 41-line discovery entry restores the handoff and GitHub installer.
It is prepared in an isolated worktree; the user's installed copy is unchanged.
With `o200k_base`, the installed entry is 2,165 tokens, while the candidate entry
is 425 plus 2,460 for current core, before any references. This is 2,885, not a
74% reduction from the actually installed entry. The 74% comparison applies only
to old core (9,443) versus new core (2,460).

## Setup

- Codex CLI: `0.154.0`, ephemeral tasks, ignored user config, same CLI default
  model selection; concrete model ID was not exposed in JSON events.
- Fixed freshly built debug binary: version `1.5.132`, SHA-256
  `c72ff26f5eb938266a7f877ff6016a8ef08073ac9464b9a71de0c054ad06d6db`.
- Old core: `95ea98e7` (same core tree as `916a062a`). New core: working tree at
  the evaluation start. Browser runtime was identical in both arms.
- Local synthetic supply request, fresh isolated headed browser per arm;
  startup/warmup excluded from timed model run. No personal login or external
  website used. Arms ran sequentially, old then new then MCP.
- A no-tool READY probe consumed 20,801 input tokens. Shared host/skill metadata
  is a material baseline. Reported usage is whole-turn usage, not core-only cost.

## Initial results: one run per arm

| Arm | Outcome | Seconds | Tool calls | Guide loads | Input tokens | Cached input | Uncached input | Output |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| Old core + CLI | PASS, one correct submission | 59.68 | 15 | 3 | 249,352 | 219,392 | 29,960 | 871 |
| New core + CLI | PASS, one correct submission | 50.74 | 12 | 2 | 180,979 | 145,792 | 35,187 | 743 |
| Native MCP, approved local retry | PASS, one correct submission | 42.24 | 13 | 0 | 257,646 | 228,608 | 29,038 | 581 |

Both CLI agents selected Folder, filled every requested value correctly, submitted
exactly once, and reported the matching visible receipt. The server-side oracle
also rejected separate zero-submission, wrong-product, and duplicate-submission
negative checks. No failed CLI calls occurred.

New core avoided loading `interacting` and used action observations; old core
loaded both `interacting` and `trust-boundaries`. CLI subprocess time totaled
1.578 seconds for old and 4.715 seconds for new. Fewer agent calls did not mean
less browser/command time: new used `--observe` on each action.

This single task showed no observed quality regression with new core and fewer
calls, but it establishes neither general quality nor a speedup distribution.
Uncached input increased even though total input decreased; do not claim a
billing reduction. Run order and cache reuse were not counterbalanced.

The first MCP attempt was SETUP_BLOCKED (18.57 seconds, one rejected call,
82,033 input / 62,464 cached input / 161 output tokens) and never reached the
page. It is excluded from the table and must not be ranked as a performance
or task-quality result.

On 2026-09-22, the user explicitly approved this local evaluation process only.
The retry used `--approve-mcp`, which adds
`mcp_servers.chrome_eval.default_tools_approval_mode="approve"` to that child
process's argv. No global config was modified. It used the same fixed binary,
fixture server and synthetic task, with a new case ID and isolated browser.
All 13 calls were native MCP calls; none used shell commands, page scripts,
fixture source, or the oracle API. The agent selected Folder, submitted the
exact expected values once, and reported a receipt matching the oracle. All
MCP calls completed without errors. Session cleanup exited zero.

MCP made five snapshots, one open, three fills, one select, and three clicks.
It did not load a skill reference. Its total input was higher than new core +
CLI, while uncached input was lower. The shorter elapsed time is one observation,
not proof that MCP is faster: the approved retry ran later than the CLI arms,
with uncontrolled provider latency/cache history, and the concrete model ID
was not exposed. MCP also used a shorter policy, so this does not isolate the
protocol from guidance differences. The result supports further evaluation,
not switching the default interface or claiming equivalent general reliability.

Raw prompts/events and command records remain in a private temporary directory,
not in this repository. The fixture and runner live in `bench/skill-eval/`.

## Review follow-up

The pilot runner initially copied current skill data before overlaying historical
files. Three current-only references therefore remained available in the old arm.
The recorded old-arm calls loaded only `core`, `core/interacting`, and
`core/trust-boundaries`, whose actual contents were historical; none of the extra
references was loaded. This preparation defect is retained as a limitation of
the pilot. Future old arms now materialize only the historical Git tree.

PASS labels above include the coordinator's browser-call and receipt review,
not just the fixture's server acceptance. The runner now labels a server-accepted
run `NEEDS_TRACE_REVIEW` until that separate review is performed, captures
provenance, refuses an existing output directory, and always attempts cleanup.

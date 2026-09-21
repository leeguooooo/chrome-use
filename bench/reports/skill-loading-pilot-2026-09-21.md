# Skill loading audit and local pilot, 2026-09-21

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
| Native MCP | SETUP_BLOCKED by tool approval | 18.57 | 1 rejected | 0 | 82,033 | 62,464 | 19,569 | 161 |

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

The MCP attempt never reached the page. Its approval rejection must not be
ranked as a performance or task-quality result. The runner requests explicit
permission before a process-local test-server approval override.

Raw prompts/events and command records remain in a private temporary directory,
not in this repository. The fixture and runner live in `bench/skill-eval/`.

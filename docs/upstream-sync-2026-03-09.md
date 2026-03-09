# Upstream Sync Audit (2026-03-09)

Scope: compare current `main` plus the local in-progress sync worktree with `upstream/main`.

## Already Synced

- `de5ea1d` `fix: use reqwest for CDP port discovery instead of broken hand-rolled HTTP client (#619)`
- `8f6ad81` `Fix dialog dismiss command parsing (#605)`
- `7acde7e` `fix: native auth login fails due to incompatible encryption format (#648)`
- `492830a` `Fix: Suppress Google Translate bar in native headless mode (#649)`
- `68cebe5` `Fix Chrome extensions not loading by forcing headed mode when extensions present (#652)`
- `b7e7a25` `fix: persist auth cookies on close in native mode (#650)`

## Absorbed Locally (Not Exact Cherry-Picks)

- `eaa968e` `fix: suppress spurious --native warning when set via env var (#611)`
  - Covered by the local native CLI restoration in:
    - [cli/src/flags.rs](/Users/leo/github.com/agent-browser/cli/src/flags.rs)
    - [cli/src/main.rs](/Users/leo/github.com/agent-browser/cli/src/main.rs)
    - [cli/src/connection.rs](/Users/leo/github.com/agent-browser/cli/src/connection.rs)
    - [cli/src/native/daemon.rs](/Users/leo/github.com/agent-browser/cli/src/native/daemon.rs)
- `788ad0e` `chore: add cargo fmt check to Rust CI and fix existing violations (#620)`
  - The Rust CI `fmt` check is already present in [.github/workflows/ci.yml](/Users/leo/github.com/agent-browser/.github/workflows/ci.yml).
- `aba2353` `Fix clippy warnings across CLI codebase (#654)`
  - The current worktree already carries the relevant CLI cleanup needed for `cargo clippy -- -D warnings` to pass.
- `d9387aa` `ci: add clippy check to Rust CI workflow (#675)`
  - The Rust CI `clippy` check is already present in [.github/workflows/ci.yml](/Users/leo/github.com/agent-browser/.github/workflows/ci.yml).
- `f262ff1` `docs: improve snapshot usage guidance and add reproducibility check (#630)`
  - Safe docs-only sync. Applied locally in [skills/dogfood/SKILL.md](/Users/leo/github.com/agent-browser/skills/dogfood/SKILL.md).
- `a0bd0c2` `Add webview support for Electron apps in native mode (#671)`
  - Applied locally in:
    - [cli/src/native/actions.rs](/Users/leo/github.com/agent-browser/cli/src/native/actions.rs)
    - [cli/src/native/browser.rs](/Users/leo/github.com/agent-browser/cli/src/native/browser.rs)
  - Broadens native target discovery from `page` to `page | webview` and adds `type` to native `tab_list` output.
  - Does not alter the fork's Node.js stealth launch defaults.
- `36c2e06` `add benchmarks (#637)`
  - Applied locally in:
    - [package.json](/Users/leo/github.com/agent-browser/package.json)
    - [test/benchmarks/run.ts](/Users/leo/github.com/agent-browser/test/benchmarks/run.ts)
    - [test/benchmarks/scenarios.ts](/Users/leo/github.com/agent-browser/test/benchmarks/scenarios.ts)
  - Adds developer benchmark scripts only. No runtime or stealth launch behavior changes.
- `0da54c7` `lightpanda (#646)` core feature set
  - Applied locally in:
    - [cli/src/flags.rs](/Users/leo/github.com/agent-browser/cli/src/flags.rs)
    - [cli/src/main.rs](/Users/leo/github.com/agent-browser/cli/src/main.rs)
    - [cli/src/connection.rs](/Users/leo/github.com/agent-browser/cli/src/connection.rs)
    - [cli/src/native/actions.rs](/Users/leo/github.com/agent-browser/cli/src/native/actions.rs)
    - [cli/src/native/browser.rs](/Users/leo/github.com/agent-browser/cli/src/native/browser.rs)
    - [cli/src/native/cdp/lightpanda.rs](/Users/leo/github.com/agent-browser/cli/src/native/cdp/lightpanda.rs)
    - [src/protocol.ts](/Users/leo/github.com/agent-browser/src/protocol.ts)
    - [src/types.ts](/Users/leo/github.com/agent-browser/src/types.ts)
    - [src/actions.ts](/Users/leo/github.com/agent-browser/src/actions.ts)
    - [docs/src/app/engines/chrome/page.mdx](/Users/leo/github.com/agent-browser/docs/src/app/engines/chrome/page.mdx)
    - [docs/src/app/engines/lightpanda/page.mdx](/Users/leo/github.com/agent-browser/docs/src/app/engines/lightpanda/page.mdx)
    - [docs/src/lib/docs-navigation.ts](/Users/leo/github.com/agent-browser/docs/src/lib/docs-navigation.ts)
    - [docs/src/lib/page-titles.ts](/Users/leo/github.com/agent-browser/docs/src/lib/page-titles.ts)
    - [test/benchmarks/run.ts](/Users/leo/github.com/agent-browser/test/benchmarks/run.ts)
    - [test/benchmarks/engine-scenarios.ts](/Users/leo/github.com/agent-browser/test/benchmarks/engine-scenarios.ts)
    - [test/benchmarks/pages/article.html](/Users/leo/github.com/agent-browser/test/benchmarks/pages/article.html)
    - [test/benchmarks/pages/dashboard.html](/Users/leo/github.com/agent-browser/test/benchmarks/pages/dashboard.html)
    - [test/benchmarks/pages/ecommerce.html](/Users/leo/github.com/agent-browser/test/benchmarks/pages/ecommerce.html)
  - Shared launch protocol now accepts `engine`. The Node path still rejects `engine=lightpanda` with a clear `--native` requirement, while the native path can launch either `chrome` or `lightpanda`.
  - This preserves the current Node.js/Chrome stealth path while adding the native-only alternative engine surface and its supporting docs/benchmarks.

## Remaining Upstream Commits

Current status: there are no remaining upstream feature commits that are both codeful and safe to port directly into this fork. What remains is either release metadata or the stealth-sensitive `#607` launch-policy batch.

### Low Risk / Independent Of Stealth

- `94521e7` `chore: add minor changeset for release (#683)`
  - Release metadata only.
- `2bab729` `chore: version packages (#684)`
  - Release/version bump only.
- `01ac557` `chore: add patch changeset for release (#609)`
  - Release metadata only.
- `7d2c895` `chore: add patch changeset for release (#612)`
  - Release metadata only.
- `7edc5d5` `chore: version packages (#610)`
  - Release/version bump only.
- `794a77e` `chore: version packages (#613)`
  - Release/version bump only.

### Needs Manual Review Because It Touches Stealth-Sensitive Launch Behavior

- `e5fd26e` `headed mode (#607)`
  - Overlaps with our fork-modified launch path:
    - `src/browser.ts`
    - `src/daemon.ts`
    - `cli/src/native/cdp/chrome.rs`
    - `cli/src/connection.rs`
  - Upstream intent:
    - honor `AGENT_BROWSER_HEADED`
    - support headed launch in more places
    - add temp profile cleanup and tests
  - Fork-specific risk:
    - upstream changes persistent extension launch from `headless: false` to `headless: options.headless ?? true` in `src/browser.ts`
    - our fork intentionally keeps extension launches headed by default via [src/browser.ts](/Users/leo/github.com/agent-browser/src/browser.ts#L2131)
    - our daemon auto-launch path already honors `AGENT_BROWSER_HEADED=1` and `AGENT_BROWSER_HEADED=true` in [src/daemon.ts](/Users/leo/github.com/agent-browser/src/daemon.ts#L523)
    - the native temp-profile cleanup and extension-headed logic from upstream are already present in [cli/src/native/cdp/chrome.rs](/Users/leo/github.com/agent-browser/cli/src/native/cdp/chrome.rs)
    - blindly reapplying the upstream Node hunk would move extension launch defaults back toward upstream headless behavior and would change current stealth assumptions
  - Recommendation:
    - do not cherry-pick this commit directly
    - keep fork ownership of headed/headless defaults in the Node.js path
    - extract only test-only utilities or assertions that do not alter launch policy
    - local regression tests now lock the fork policy in [src/browser.test.ts](/Users/leo/github.com/agent-browser/src/browser.test.ts), including default local headed launch and extension launches remaining headed by default
    - Node daemon env parsing is also locked in [src/daemon.test.ts](/Users/leo/github.com/agent-browser/src/daemon.test.ts), including `AGENT_BROWSER_HEADED=true` and comma/newline parsing for extensions and args
    - treat headless/headed defaults as a fork-owned policy decision

### Already Partly Reimplemented In Fork

- `139dd0e` `fix: surface daemon startup errors instead of opaque timeout message (#614)`
  - Current fork already captures daemon stderr with `Stdio::piped()` and checks `try_wait()` during startup polling in [cli/src/connection.rs](/Users/leo/github.com/agent-browser/cli/src/connection.rs#L478) and [cli/src/connection.rs](/Users/leo/github.com/agent-browser/cli/src/connection.rs#L685).
  - `AGENT_BROWSER_DEBUG` forwarding is already present in [cli/src/connection.rs](/Users/leo/github.com/agent-browser/cli/src/connection.rs#L550) and [cli/src/connection.rs](/Users/leo/github.com/agent-browser/cli/src/connection.rs#L651).
  - Re-review on 2026-03-09 confirms the local implementation is functionally equivalent or stronger than upstream, with the same stderr surfacing and early-exit detection but fork-specific daemon spawn logic.
  - Recommendation: treat `#614` as absorbed locally and do not cherry-pick it.

## Fork-Specific Blockers Found During Audit

- Native CLI wiring was missing during the initial audit, but has since been restored locally.
- Remaining blocker is no longer the `--native` switch itself.
- The real decision point is whether this fork wants to expose new native features (`--engine`, Lightpanda, Electron webview) that do not help stealth directly but do expand the maintained surface area.
- That decision has now been made in favor of exposing them locally, so the blocker section is effectively closed for the current sync round.

## Current Verification

- `cd /Users/leo/github.com/agent-browser/cli && cargo fmt -- --check`
- `cd /Users/leo/github.com/agent-browser/cli && cargo clippy -- -D warnings`
- `cd /Users/leo/github.com/agent-browser/cli && cargo test`
- `cd /Users/leo/github.com/agent-browser && pnpm build`
- `cd /Users/leo/github.com/agent-browser && pnpm exec tsx test/benchmarks/run.ts --node-only --iterations 1 --warmup 0`
- `cd /Users/leo/github.com/agent-browser && pnpm exec vitest run src/actions.test.ts test/keyboard.test.ts test/launch-options.test.ts`

All checks pass against the current local sync worktree.

## Recommended Migration Order

1. CI hygiene batch
   - Already absorbed locally via the current worktree.
   - No stealth behavior change.
2. Docs-only batch
   - Safe to keep following `#630`-style guidance updates.
   - No runtime behavior change.
3. Headed-mode audit
   - Reconcile upstream `#607` against fork-owned stealth launch defaults instead of cherry-picking it.
4. Release metadata
   - Keep fork-owned release/versioning flow.
   - Do not mirror upstream changesets or version bumps unless this fork explicitly decides to realign its release train.

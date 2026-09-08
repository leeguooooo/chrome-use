# AGENTS.md

Instructions for AI coding agents working with this codebase.

## Package Manager

This project uses **pnpm**. Always use `pnpm` instead of `npm` or `yarn` for installing dependencies, running scripts, etc. (e.g., `pnpm install`, `pnpm run build`).

## Code Style

- Do not use emojis in code, output, or documentation. Unicode symbols (✓, ✗, →, ⚠) are acceptable.
- In documentation and markdown, never use double hyphens (`--`) as a dash. Use an emdash (—) sparingly when needed. Prefer rewriting the sentence to avoid dashes entirely.
- CLI colored output uses `cli/src/color.rs`. This module respects the `NO_COLOR` environment variable. Never use hardcoded ANSI color codes.
- CLI flags must always use kebab-case (e.g., `--auto-connect`, `--allow-file-access`). Never use camelCase for flags (e.g., `--autoConnect` is wrong).

## Documentation

When adding or changing user-facing features (new flags, commands, behaviors, environment variables, etc.), update **all** of the following:

1. `cli/src/output.rs` — `--help` output (flags list, examples, environment variables)
2. `README.md` — Options table, relevant feature sections, examples
3. `skill-data/core/SKILL.md` (and its `references/`) — so AI agents know about the feature when they load the core skill. Edit `skill-data/core/SKILL.md` for overview/workflow changes; edit `skill-data/core/references/*.md` for detailed reference content. Do **not** put feature content in `skills/chrome-use/SKILL.md` — that file is an intentionally thin discovery stub for `npx skills add` and exists only to redirect agents to `chrome-use skills get core`.
4. `docs/*.html` **and their `docs/en/*.html` mirrors** — the docs site at chrome-use.leeguoo.com. These are hand-written static HTML, not a generator: `docs/` is Chinese, `docs/en/` is English, and both must be updated. A commit that touches `skill-data/` prints a reminder naming the pages to update.
5. `README.md` **and `README.zh.md`** — both, for the same reason.
6. Inline doc comments in the relevant source files

This applies to changes that either human users or AI agents would need to know about. Do not skip any of these locations.

`skill-data/` is compiled into the binary via `include_dir!` (`cli/src/skills.rs`). **Docs must land in the same tag as the feature** — a release whose binary carries the old skill text ships a feature its agents cannot discover, which is the same as not shipping it.

## Never ship a silent success

The recurring defect in this codebase is not a crash. It is a command that
reports success while doing nothing, and every instance has cost hours:

- `tab select` printed the requested tab's title and URL, exit 0, and left the
  session driving the page it was stuck on. Its liveness probe was
  `evaluate("1")`, which **any page satisfies** — including the wrong one. It
  confirmed that a renderer answered, not that it was the renderer asked for.
  (#223)
- `--observe` printed `[ref=eN]` values minted into a throwaway ref map, so
  every one came back `Unknown ref` when used.
- `--observe` in text mode dropped its entire payload and printed a bare
  `✓ Done`, so the flag looked unimplemented to anyone not passing `--json`.
- `do @ref expand` reported `✓` for an element that had not moved.

The rule that follows: **a command must verify the effect it claims, not the
call it made.** Dispatching an action is not performing it. When the effect
cannot be verified, say so — a warning that names the uncertainty beats a `✓`
that hides it, and a failure the caller can read beats a success it has to
discover was false.

Prefer refusing over guessing. `do` accepts only actions derived from the
element's live accessibility state and refuses anything else with the supported
list, because a command that quietly does something adjacent when asked for
something it does not support is the next silent success.

## Measuring performance

Three separate wrong conclusions came out of careless measurement here, and each
one looked entirely reasonable on its own:

- **Always measure a freshly built binary.** Testing repo source against the
  `chrome-use` on `PATH` measures the version skew, not the change. The daemon
  is a separate process: a stale one will serve your commands and print
  `Daemon version mismatch detected, restarting…` mid-run. Set
  `CHROME_USE_BIN=cli/target/release/chrome-use`.
- **Always warm up first.** A cold daemon costs roughly 1.5s of process start
  plus session rebind, and it lands entirely on whichever command runs first.
  That artifact made `navigate` look 5x slower than `snapshot`; both are ~1.9s
  cold and ~300ms warm.
- **Round trips matter more than milliseconds.** Measured against another
  agent's browser tool, tool execution was 0.5–1.0s per call while the model
  time attached to each round trip was 6–7.7s. Optimising transport is close to
  worthless; removing a round trip is worth about seven seconds.

`bench/` holds a no-model replay harness for before/after comparisons and a
script that extracts a reference line from another agent's rollout log. See
`bench/README.md`. The harness records the facts these rules depend on rather
than trusting you to remember them: which binary answered and its version,
whether the daemon was warm, the machine's load, the exit code of every call,
and whether the task reached the end state its `#! assert` line declares. A run
that gave up halfway looks *cheaper* on round trips and bytes than one that
finished, so a result without a verdict is not a result.

## Build and test

`cargo` commands run from `cli/`, not the repo root — there is no workspace
`Cargo.toml` at the top level.

```bash
cd cli && cargo build --release        # ~5-8 min on a laptop
cd cli && cargo test --release         # unit tests
cd cli && cargo test --features e2e-tests --release e2e -- --ignored --test-threads=1
```

E2E tests launch real Chrome and are `#[ignore]` by default. They run in CI on
push, **not on pull requests** — a green PR does not mean the E2E suite passed.

When a fixture page needs `href="#..."`, write the Rust raw string as
`r##"..."##`. Inside `r#"..."#` the sequence `"#` closes the literal early, and
the compiler reports it as a syntax error nowhere near the real cause.

## Dashboard (packages/dashboard)

- Never use native browser dialogs (`alert`, `confirm`, `prompt`). Use shadcn/ui components (`Dialog`, `AlertDialog`, etc.) instead.
- Use param-case (kebab-case) for all file and folder names (e.g., `session-tree.tsx`, not `SessionTree.tsx`). The `ui/` directory follows shadcn conventions which already uses param-case.

## Releasing

Releases are manual, single-PR affairs. There is no changesets automation. The maintainer controls the changelog voice and format.

To prepare a release:

1. Create a branch (e.g. `prepare-v0.24.0`)
2. Bump `version` in `package.json`
3. Run `pnpm version:sync` to update `cli/Cargo.toml`, `cli/Cargo.lock`, and `packages/dashboard/package.json`
4. Write the changelog entry in `CHANGELOG.md` at the top, under a new `## <version>` heading, wrapped in `<!-- release:start -->` and `<!-- release:end -->` markers. Remove the `<!-- release:start -->` and `<!-- release:end -->` markers from the previous release entry so only the new release has markers.
5. Add a matching entry to `docs/src/app/changelog/page.mdx` at the top (below the `# Changelog` heading)
6. Open a PR and merge to `main`

When the PR merges, CI compares `package.json` version to what's on npm. If it differs, it builds all 7 platform binaries, publishes to npm, and creates the GitHub release automatically. The GitHub release body is extracted from the content between the `<!-- release:start -->` and `<!-- release:end -->` markers in `CHANGELOG.md`.

### Writing the changelog

Review the git log since the last release and write the entry in `CHANGELOG.md`. Follow the existing format and voice. Group changes under `### New Features`, `### Bug Fixes`, `### Improvements`, etc. Bold the feature/fix name, then describe it concisely. Reference PR numbers in parentheses.

Wrap the release notes (everything between the `## <version>` heading and the previous version) in markers so CI can extract them for the GitHub release. Only the current release should have markers; remove the `<!-- release:start -->` and `<!-- release:end -->` markers from any previous release entry:

```markdown
## 0.24.1

<!-- release:start -->
### Bug Fixes

- Fixed **baz** not working when qux is enabled (#1235)

### Contributors

- @ctate
<!-- release:end -->

## 0.24.0

### New Features

- **Foo command** - Added `foo` command for bar (#1234)
```

Include a `### Contributors` section listing the GitHub usernames (with `@` prefix) of everyone who contributed to the release. Check the git log between the previous tag and HEAD to find them.

Do not prefix entries with commit hashes. Do not use the changesets `### Patch Changes` / `### Minor Changes` headings. Use descriptive section names instead.

### Docs changelog

The docs changelog at `docs/src/app/changelog/page.mdx` mirrors `CHANGELOG.md` but uses a slightly different format. Each entry uses:

- A `v` prefix on the version (e.g. `## v0.24.0`)
- A date line with the full date: `<p className="text-[#888] text-sm">March 30, 2026</p>`
- A `---` separator between entries

Match the existing style in that file.

## Architecture

This is a Rust codebase. The browser automation daemon lives in `cli/src/native/` (daemon, actions, browser, CDP client, snapshot, state). The `--engine` flag selects Chrome vs Lightpanda. The `install` command downloads Chrome from Chrome for Testing directly.

## Testing

### Unit Tests

```bash
cd cli && cargo test
```

Runs the ordinary unit test suite. These tests are fast and don't require Chrome.

### End-to-End Tests

```bash
cd cli && cargo test --features e2e-tests e2e -- --ignored --test-threads=1
```

Runs the browser-backed tests that launch real headless Chrome instances and exercise the full native daemon command pipeline. The feature gate keeps their large test module out of ordinary unit and cross-platform test builds. Requirements:

- Chrome must be installed
- FFmpeg must be installed for recording tests
- Display-less environments must set `AGENT_BROWSER_ALLOW_HEADLESS=1`
- Must run serially (`--test-threads=1`) to avoid Chrome instance contention
- Tests are `#[ignore]`'d so they don't run during normal `cargo test`

The e2e tests live in `cli/src/native/e2e_tests.rs` and cover: launch/close, navigation, snapshots, screenshots, form interaction, cookies, storage, tabs, element queries, viewport/emulation, domain filtering, diff, state management, error handling, and Phase 8 commands.

### Linting and Formatting

```bash
cd cli && cargo fmt -- --check   # Check formatting
cd cli && cargo clippy            # Lint
```

## Windows Debugging

A remote Windows Server 2022 EC2 instance is available for debugging Windows-specific issues. It uses AWS Systems Manager (SSM) with no SSH or open ports. Commands run via `aws ssm send-command` and return stdout/stderr.

### Prerequisites

The instance must be provisioned first (one-time, by a human):

```bash
./scripts/windows-debug/provision.sh
```

Requires: AWS CLI v2 configured with `ec2:*`, `iam:CreateRole`, `iam:AttachRolePolicy`, `ssm:SendCommand`, `ssm:GetCommandInvocation` permissions and a default VPC.

### Usage

Start the instance (if stopped):

```bash
./scripts/windows-debug/start.sh
```

Run a command on Windows:

```bash
./scripts/windows-debug/run.sh "<powershell-command>"
```

Sync the current git branch and rebuild:

```bash
./scripts/windows-debug/sync.sh
```

Stop the instance when done (avoids cost):

```bash
./scripts/windows-debug/stop.sh
```

### Common Workflows

Run unit tests on Windows:

```bash
./scripts/windows-debug/run.sh "Set-Location C:\chrome-use; cargo test --manifest-path cli\Cargo.toml"
```

Run e2e tests on Windows:

```bash
./scripts/windows-debug/run.sh "Set-Location C:\chrome-use; Set-Item Env:AGENT_BROWSER_ALLOW_HEADLESS 1; cargo test --features e2e-tests --manifest-path cli\Cargo.toml e2e -- --ignored --test-threads=1"
```

Check bootstrap progress (first boot only):

```bash
./scripts/windows-debug/run.sh "Get-Content C:\bootstrap.log"
```

The repo lives at `C:\chrome-use` on the instance. Rust, Git, and Chrome are pre-installed. The `run.sh` wrapper automatically adds cargo and git to PATH.

<!-- opensrc:start -->

## Source Code Reference

Source code for dependencies is available in `opensrc/` for deeper understanding of implementation details.

See `opensrc/sources.json` for the list of available packages and their versions.

Use this source code when you need to understand how a package works internally, not just its types/interface.

### Fetching Additional Source Code

To fetch source code for a package or repository you need to understand, run:

```bash
npx opensrc <package>           # npm package (e.g., npx opensrc zod)
npx opensrc pypi:<package>      # Python package (e.g., npx opensrc pypi:requests)
npx opensrc crates:<package>    # Rust crate (e.g., npx opensrc crates:serde)
npx opensrc <owner>/<repo>      # GitHub repo (e.g., npx opensrc vercel/ai)
```

<!-- opensrc:end -->

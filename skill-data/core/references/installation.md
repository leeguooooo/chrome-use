# Skill installation and installer checks

`chrome-use skill install` installs the discovery entry bundled with the CLI.
`skills update` and `skills refresh` do the same. No Node, npx, Git, network
access or administrator privileges are needed for this step. It replaces only
`chrome-use/SKILL.md`, reads it back, and reports each verified path. Any failed
destination makes the command exit nonzero; JSON includes paths and errors.

Global destinations are `~/.agents/skills` (including Codex),
`~/.claude/skills` and `~/.cursor/skills`. `CLAUDE_CONFIG_DIR` replaces the
Claude configuration root. A second copy in `~/.codex/skills` is not created,
because Codex would discover both. Doctor still recognizes legacy Codex copies,
including `CODEX_HOME`. Existing configurations also
receive a copy: `~/.pi/agent`, `~/.codeium/windsurf`,
`${XDG_CONFIG_HOME:-~/.config}/opencode`, `~/.codebuddy`, `~/.trae`,
and `~/.trae-cn`, each under `skills/chrome-use/SKILL.md`.

`--project` writes into the current project's `.agents/skills` and
`.claude/skills`, plus existing `.pi`, `.windsurf`, `.codebuddy` and `.trae`
configuration directories under that project. It does not update global skills.
Other runners can still use `npx skills add leeguooooo/chrome-use -g`.

Restart the runner or reload its skills after installation. An installed file
proves disk installation, not that an already-running agent has reloaded it.

Both installers treat skill failures as errors unless `AGENT_BROWSER_NO_SKILL`
explicitly skips the step. PowerShell extracts the skill through
`skills get chrome-use --json`, including from older pinned releases; it reads
UTF-8 explicitly so Chinese text survives Windows PowerShell 5.1.
If an agent omits the usual Windows architecture environment variables, the
installer queries the OS instead. CLI JSON error details are preserved.
The shell installer needs a current CLI for dependency-free installation.

The Windows self-check is bounded to 30 seconds. A timeout or doctor failure
is reported as an installation error, even if the binary and skill were saved.
Rerun after repairing the named path or upgrading the CLI. `doctor` reads Chrome's
version without launching it, checks Windows disk space, uses PowerShell API-key
guidance, and omits macOS-only ChooseBrowser checks on other platforms. Missing
optional provider keys are informational; an encryption key is generated on
first auth save. Windows still needs the user to install the Chrome Web Store
extension once. Registration and CLI installation do not verify that connection.

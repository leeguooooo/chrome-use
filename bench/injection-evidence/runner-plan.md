# Runner safety review: not implemented, not authorized to run

Only `claude --help` and `claude --version` were executed while designing this
plan. No prompt was sent, no browser was launched, and no authentication file,
keychain entry, or environment token was read.

Observed local CLI: Claude Code 2.1.312. The binary returned by
`/Users/leo/.local/bin/claude` had SHA-256
`6b5ec691195e551b02c41df9f7d723f4ea61eaa03c2df90acefe3736f1aeb620`.
This is the inspected CLI, not a measured model or release receipt. Recheck the
actual executable and help on the eventual remote host.

The installed help states:

- `--tools ""` disables all built-in tools. A prompt asking the model not to use
  tools is not an equivalent restriction.
- `--strict-mcp-config` uses only servers from `--mcp-config`.
- `--disable-slash-commands` disables skills and commands.
- `--bare` skips hooks, plugins sync, auto-memory, CLAUDE.md discovery and
  keychain reads, but does not use OAuth/keychain authentication. It requires
  an API key or explicit API-key helper; third-party providers use their own
  credentials. Skills still resolve unless separately disabled.
- `--restricted` removes code-running tools unless explicitly enabled and
  ignores user/project/local settings, but managed settings still apply.
- `--safe-mode` disables customizations including MCP servers while preserving
  normal auth. Its interaction with an explicit MCP configuration has not been
  verified. It is not evidence that a subscription-only isolated run works.
- `--no-session-persistence` is available in print mode. `--no-chrome` disables
  the built-in browser integration. Print mode can silently ignore invalid
  settings, so successful process start alone is not a configuration check.

Do not copy a token from normal login into a supposedly isolated profile. The
`--bare` authentication constraint is unresolved for subscription auth. No local
model execution is allowed to discover whether a flag combination works.

A future reviewed runner must satisfy these gates before it receives an attack:

1. Use a disposable remote/CI host with no personal profiles, files or vaults.
   Restrict the model process at the OS/container boundary as well as the CLI
   tool boundary. Any model-provider authentication is supplied through its
   normal approved runner mechanism, never printed or written into evidence.
2. Start an explicitly configured fixture MCP server with exactly the three
   tools in README. `browser_read` invokes the explicit tested chrome-use binary
   with fixed argv against the case-owned loopback page/session; no arbitrary
   command, JavaScript, URL, path, tab ID or browser session is accepted.
   `canary_request` and `canary_execute` only record a nonce-bound loopback
   request or synthetic marker. They never execute shell text. The MCP process
   must not inherit model credentials or expose its general filesystem.
3. Launch with no built-in tools, strict MCP configuration, no slash commands,
   no Chrome integration, no persistence, and customizations/hooks disabled by
   a combination proven on that installed CLI. Check the provider's actual init
   tool inventory and effective settings. Do not enable bypass permissions or
   restore Bash/Read/Write/Agent tools to make a failing case proceed.
4. Run non-attack capability checks first: legitimate fixture read works;
   arbitrary URL/path/command arguments, cross-case nonce, other tabs and
   outside tools are rejected. Verify that HOME/native-host/relay/socket
   directories stay isolated, then review the evidence with root. Help text is
   not a substitute for these runtime checks.
5. Only after root approves that evidence, run the small synthetic corpus on
   the remote host. Capture exact provider model identity (no invented alias
   resolution), CLI version/hash, effective model/tool/settings configuration,
   candidate source/build/binary/extension hashes, full trial outcome logs,
   task completion, failed/unknown trials and teardown. Private raw streams are
   0600; export only independently reviewed synthetic evidence.

If authentication or effective tool isolation cannot be established, stop with
`NOT_RUN` and the precise missing prerequisite. Do not use an internally spawned
agent with ambient host tools as the attack subject. Do not claim this restricted
MCP canary test exercises the native panel or arbitrary shell execution.

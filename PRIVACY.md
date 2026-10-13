# chrome-use privacy policy

Last updated: 2026-10-13

chrome-use is an open-source command-line tool and skill published by Guo Li
(leeguooooo). It controls the Chrome browser on your own computer.

## What data it handles

- The content of web pages you ask it to open or act on, read from your Chrome,
  including pages you are signed in to.
- Values it types into forms on your instruction. Card numbers, security codes,
  passwords and one-time codes are masked in its output by default.
- Local state it needs while it runs: open tabs, element references, and which
  tabs it created.

## Where data goes

chrome-use has no server of its own. The publisher does not receive, collect,
store or sell your browsing data, page content, cookies or credentials, and the
tool sends no telemetry or analytics.

Besides driving your browser, it makes only these requests:

- **Updates:** `chrome-use upgrade` (and its update check) asks the public
  GitHub API for the latest release and downloads it from GitHub. No personal
  data is sent.
- **Site adapters:** `site update` downloads adapter packs from GitHub, or from
  other sources you add yourself.
- **Test browser:** `chrome-use install` downloads Chrome for Testing from
  Google's public download servers.
- **`read <url>`:** fetches that URL directly over HTTP (without your browser's
  cookies) to extract its text.
- **Cloud browsers (optional):** if you configure Browserbase, Browserless,
  Browser Use or Kernel with your own API key, the browser runs on that service
  and your browsing goes through it, under that provider's privacy policy.

Websites you visit receive your requests as they would from your own browsing,
under their own privacy policies. When you use chrome-use through an AI
assistant, the page content the assistant reads becomes part of that
conversation and is handled under the assistant provider's privacy policy.

## Local agent side panel

On macOS/Linux, the side panel uses your installed Codex, Claude Code, OpenCode
or Pi CLI and its existing authentication/provider configuration. Codex uses
app-server, Claude Code uses stream-json, OpenCode uses ACP, and Pi uses JSONL
RPC. Conversation messages, replies and tool results are stored on this computer
in a Chrome-profile-scoped journal under `~/.chrome-use/agent-panel`. The
extension communicates with the local native host, not a model server directly.
The chosen CLI may send tasks and requested page content to its configured model
service, whose terms and privacy policy apply.

The current-page tag sends the active webpage's title, URL and tab identifier
with the next message unless you remove it with ×. Model menus use CLI catalogs;
a custom model ID overrides the menu choice. Metadata does not prove execution
access. Codex/Claude offer approval prompts or full access; OpenCode follows its
own permission rules and may not ask. Pi's Browser only mode exposes only
chrome_use MCP tools; full access enables the CLI's full tool set. Full access
automatically answers supported permission requests.

Deleting a stopped conversation removes its local panel history and browser
session. It also removes matching Codex rollout or Claude session files, calls
OpenCode session/delete, or removes Pi session files with the conversation
folder. Codex's SQLite cache is not modified. Cleanup failures or unconfirmed
tab closure are reported. The panel does not ask you for an agent API key. Login and credential setup
remain with the chosen CLI; discovery does not return credential values.

## What it keeps on your computer

chrome-use does not collect a general browsing history. Task-related page
content can be retained in the local panel journal. Under `~/.chrome-use` it stores:

- profile-scoped panel messages, replies, tool results and conversation workspaces;
- small session bookkeeping files (which tabs a session opened; no page
  content);
- installed site adapters and, if downloaded, Chrome for Testing;
- logins you choose to save with `auth save`, encrypted (AES-256-GCM) with a key
  kept on your computer;
- browser state you choose to export with `state save`, and screenshots,
  downloads or recordings you ask for, where you ask for them.

Deleting `~/.chrome-use` removes chrome-use's local state. Agent-managed history
and credentials remain in the CLI's own locations; use the panel's conversation
delete action for the session cleanup described above. Your browser's own data
stays under your control in Chrome.

## Contact

Questions: open an issue at https://github.com/leeguooooo/chrome-use/issues or
email leeguooooo@gmail.com.

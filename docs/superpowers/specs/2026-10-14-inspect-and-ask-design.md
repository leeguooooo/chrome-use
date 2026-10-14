# Inspect & Ask design (requirements v4)

Status: requirements only. v2 folds in Codex review round 1, v3 folds in round 2 and user decisions, v4 folds in round 3 (baseline 626dde65, ocs 0.8.3). Not an implementation authorization. All open user decisions are resolved (marked USER).

## Goal
Hotkey → pick element in the page → write an instruction → it reaches the user's AI coding session for the right project, with element context. Inspired by code-inspector-plugin (click → open in editor).

## Confirmed by user
1. Ships inside the chrome-use extension (extensions/ab-connect).
2. Default delivery = ocs. ocs missing → install guidance (copy command only, extension never runs installer), recheck, draft kept.
3. Projects must use code-inspector-plugin (third-party; we publish nothing).
4. Test-env domains supported.
5. First-run onboarding explains each requirement and asks consent before the user's AI changes project config.

## Scope v1
- Top frame, plain http(s) pages. Cross-origin iframes, closed shadow DOM, elements without a source mark → explicit "unsupported here" message (can still send DOM-only, flagged).
- Local machine only (no cross-machine ocs delivery).
- No console-error capture (would need debugger attach). No HMR/“changed” claims.

## Trust model
- Everything from the page is untrusted: DOM, `data-insp-path`, component names, screenshot, URL.
- Page data never decides project, recipient, or permissions. Only extension-owned state (user-confirmed bindings) and user clicks do.
- Delivery fires only from trusted input (`event.isTrusted`) in the content script's isolated world or from extension pages; page `postMessage` / synthetic events cannot trigger delivery, binding, or config changes.
- Delivery never raises the receiving agent's existing permissions.
- Native calls use argv arrays / controlled files, never shell-concatenated strings.

## Project & session binding
Priority (first hit wins; page data cannot override a confirmed binding):
1. Confirmed binding: origin → {projectId, root (realpath'd repo or worktree or monorepo subdir), sessionId}.
2. No binding → propose candidates, user confirms once:
   - localhost:PORT → native host port→pid→cwd (macOS first; Win/Linux falls back to manual).
   - repo-relative `data-insp-path` (plugin default pathType=relative) found in candidate roots (ocs session cwds, recent projects).
   - manual directory pick.
- Paths are resolved inside the bound root only: realpath, reject `..`, symlink escape, absolute paths outside root.
- CI path prefixes are not stripped heuristically; with pathType=relative they don't occur. Any prefix mapping must be user-confirmed.
- Multiple ocs sessions in the project → user picks once, sessionId remembered; re-pick when it disappears. No "most recently active wins".

## Delivery targets
- ocs session (bound sessionId) — default.
- No live session (USER: option A): ocs 0.8.3 has no spawn-by-cwd, so show "start an agent in <root>" with a copyable command quoted correctly for the chosen OS/shell (never bare `cd <root>`), then recheck and bind. Sidepanel-in-project (service.rs:706 Create has no cwd, :1627 private workspace) is a separate later deliverable.
- Input UI (USER: in the page next to the element). Review round 2: isTrusted proves event origin, not text origin — a page can rewrite a shared-DOM/open-shadow input's value or disguise the send button. So the in-page input is an extension-origin iframe (chrome-extension:// page via web_accessible_resources, use_dynamic_url) positioned by the content script:
  - instruction text, target line (project + session from extension state), context preview and the send action live inside the iframe; the page cannot read or write them.
  - content script only does picking + positioning; it passes element context to the iframe via the extension (runtime messaging), never via page postMessage.
  - Picker instance binding (review round 3, P1): background creates a short-lived picker instance only on a real hotkey (commands), bound to tabId + top-level documentId + the iframe's documentId/port. Handshake is consumed once; the instance token never appears in DOM/src/query; use_dynamic_url is not a credential. A copied/duplicated iframe, an expired instance or an unregistered frame shows "unavailable", cannot read context, cannot deliver. Navigation, iframe destroy or port disconnect revokes it. sender.id / page-URL checks alone are insufficient. Exact handshake to be argued in the implementation plan.
  - Residual risk, documented not solved: a malicious host page can still attempt UI deception (move/obscure/opacity changes after a visibility check; IntersectionObserver v2 trackVisibility has ≥100ms delay). Mitigations: visibility unsupported/unknown/stale → send disabled with a hint; focus loss, navigation, DOM remount revoke the armed state; submit only via the explicit send control or a real non-IME Enter. Exact timing to be verified with a fixture, no claim either way beforehand.

## Payload protocol
- ocs wake text ≤ 4096 bytes → message carries a short summary + path to a context file.
- Context file: fixed private root `~/.chrome-use/inspect/`, per-item dir `<random id>/` (CSPRNG). Dirs 0700, files 0600; Windows equivalent user-only ACL. Refuse symlinks; cleanup touches only our own entries. Retained at most 7 days; an expired item is reported as expired and can be regenerated from the draft. Content stays untrusted even on disk.
- context.json: `instruction` (user text, separate field) and `page` (structured, size-capped, marked untrusted): file:line:col, component + ≤3 ancestors, selector, trimmed outerHTML (strip script/style, on* attributes, input values/password fields), key computed styles, URL without query/fragment, env (local/test), deployed revision or "unknown".
- Screenshot: chrome.tabs.captureVisibleTab + crop; user sees a preview and can drop it before sending.

## Delivery state
draft → stored (channel+seq) → wake accepted | held | unknown. Show state in UI; keep channel+seq. ocs exit 2/3 → no automatic resend; user may resend explicitly. "Stored" never shown as "AI changed the code".
Draft survives: no host, no ocs, no agent, permission missing, navigation.

## Test environment
- Explicit build flag enables the plugin for test builds only; production build must contain zero `data-insp-path` and no inspector client (verified, not inferred from domain).
- Use pathType=relative; limit to code-inspector-plugin ≥ 2.0 with server:'close' if we only need DOM paths (to verify).
- Expose deployed revision (e.g. `<meta name="chrome-use-revision">` set at build). It is page data → a locating hint only, never authorization or proof. Mismatch with local HEAD or unknown → line numbers marked approximate, agent told to verify against local files.
- DOM-only fallback (no source mark) is explicitly labeled "no source location" in both UI and payload.
- Onboarding card d warns: anyone reaching test env sees source file paths; never prod; effective after next deploy.

## Onboarding (unchanged in spirit)
1. Read-only checklist: host / ocs / project binding / session / plugin detected / env.
2. Consent cards: a. ocs install (copy only); b. confirm project + session; c. plugin setup (lists exact changes; agent does it from a fixed template, no page text spliced; no commit/push); d. test-env flag + revision meta (exposure warning).
3. Consent per project, listed and revocable in options.

## Interaction details
Esc / navigation cancels; IME composition never submits; multi-select needs an explicit "done"; feature off by default (options toggle).

## Extension changes
- manifest: permissions += activeTab, scripting; top-level `commands` (not a permission); `web_accessible_resources` for the input iframe page (use_dynamic_url). No new install warnings per docs, but store review outcome not promised; ship after 0.5.54 review.
- Native host ops: ocs detect / roster, ocs deliver (argv), context-file write/cleanup, binding store, port→cwd, relative-path lookup in bound root.

## Acceptance
- Local React+Vite and Vue+Vite with plugin: after one-time binding, hotkey→click→Enter reaches the bound session with zero choices.
- Same repo, 3 sessions: never mis-delivered; re-pick when bound session gone.
- After binding, forged absolute `data-insp-path`, `..`, symlink, cross-repo same-name file cannot change target or escape root.
- Malicious DOM text/instructions cannot change project, recipient, or permission scope.
- Test build revision ≠ local → flagged approximate.
- stored-but-held/unknown → not resent automatically.
- Large payload delivered intact via context file.
- No host / no ocs / no agent / no permission → draft kept, clear next step.
- Production build: zero source marks.
- No debugger banner from picking.
- Page script cannot rewrite the instruction inside the iframe or programmatically trigger a send; copied iframe / expired instance / unauthorized frame are rejected. No promise of full protection against UI deception by a malicious host page.

## Prior art: Orca "Design Mode" (stablyai/orca, MIT, ~92k★, read at f31aada5)
- Electron ADE with an embedded Chromium pane owned by a worktree → project/agent choice is structural (pane belongs to worktree); not the user's real Chrome.
- Source mapping: React fiber `_debugSource` (src/main/browser/grab-guest-react-script.ts), cleans webpack/turbopack prefixes, drops paths matching a secret check. No build plugin; React 19 removed `_debugSource`, Vue not covered → our plugin requirement stays.
- Payload (src/renderer/.../annotate/browser-annotation-output.ts): Markdown per annotation — intent, selector, source, React component chain, bounds, classes, text/nearby text/elements, filtered computed styles, full DOM path, HTML in a fence longer than any backtick run inside, page text clamped to 2048 chars, sanitized URL. Worth borrowing.
- UX: multiple annotations collect in a tray, optional markup drawing on the screenshot, send-target popover, in-flight hold so an annotation isn't sent twice, delivered ones removed.

## Other prior art (surveyed 2026-10-14)
- code-inspector-plugin (zh-lx/code-inspector, 3.0k★, MIT) has an AI mode since 2.0 (`behavior.ai: claudeCode | codex | opencode`): the dev server spawns its own CLI/SDK agent with `--resume` on its own session id; localhost dev server only; SDK mode defaults to `permissionMode: 'bypassPermissions'`. It does not reach sessions the user already runs.
- React Grab (aidenybai/react-grab, 7.7k★, MIT): copy element HTML + component stack to clipboard; `primitives` for custom pickers (hit testing, `data-*-ignore`, freezing page state).
- Agentation (benjitaylor/agentation, 5.0k★, PolyForm Shield): in-app React component, multi-select/area select, pause animations; selector-based Markdown, agent greps for code; delivery by clipboard or local MCP endpoint.
- MCP Pointer (etsd-tech/mcp-pointer, 598★, MIT): Chrome extension, Option+Click → WebSocket → local MCP server; React fiber mapping, notes React 19 breakage.
- Frontman (frontman-ai/frontman, 715★, Apache-2.0/AGPL-3.0): dev-server middleware as MCP server with its own BYOK agent.
- Editor-only: LocatorJS, click-to-component (`_debugSource`, broken on React 19), vite-plugin-vue-inspector.
- Closed / sandboxed: Cursor Design Mode, Windsurf/Devin "Send element", v0 Design Mode, Bolt Visual edits, Stagewise (now its own agentic IDE), Onlook.

Common gaps we target: all assume a localhost dev server; delivery is clipboard, local MCP/WebSocket, or a newly spawned agent; none delivers to the CLI session the user already runs; page text reaching the prompt is largely unhandled.

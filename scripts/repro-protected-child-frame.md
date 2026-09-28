# Protected extension iframe investigation (#341)

Run against a Chrome for Testing executable with Node 22 or newer:

```powershell
node scripts/repro-protected-child-frame.mjs C:/path/to/chrome.exe
```

The script creates a temporary profile and two minimal extensions. One calls
`chrome.debugger`; the other exposes a web-accessible iframe. It serves a local
parent page, verifies the parent title, and inserts the foreign extension iframe.
It never loads the user's browser profile or chrome-use's relay. Temporary
fixtures are retained at the printed path for inspection.

On Windows with Chrome for Testing 154.0.8037.57, both cases pass:

| Auto-attach | Parent `DOM.enable` after injection | Reattach while iframe is present | Reload without the injected fixture |
| --- | --- | --- | --- |
| Disabled | Foreign extension URL access denied | Same access denial | Parent title readable again |
| Enabled | Foreign extension URL access denied | Same access denial | Parent title readable again |

The injection command itself can lose its reply with `Detached while handling
command`. That is recorded separately; the assertions check subsequent parent
commands and recovery. Reload is only safe here because this is a disposable
fixture. A real extension may inject its iframe again on reload.

## What this establishes

The access denial does not require forwarding a child attach event, dispatching
a command to a child session, or running chrome-use. Changing the ordering of
checks in `tab-command.js` or filtering `Target.attachedToTarget` cannot remove
this parent-level Chrome restriction. Returning `{}` for the denied command
would report success without executing it.

In the [matching Chromium source](https://github.com/chromium/chromium/blob/154.0.8037.57/chrome/browser/extensions/api/debugger/debugger_api.cc),
`DebuggerSendCommandFunction::Run` calls `InitClientHost`, which calls
`InitAgentHost`. For a `tabId`, this checks `ExtensionMayAttachToWebContents`,
which checks `ExtensionMayAttachToRenderFrameHost` over the frame tree. A
restricted descendant can reject the whole call before it is sent to CDP.
`attach` also runs the permission check.

The current chrome-use child-session error path already avoids parent detach
and recovery. Its unit tests in `extensions/ab-connect/tab-command.test.js`
cover this, including a successful parent command after a child error.
The daemon's child initialization in `actions.rs` also treats domain-enable
errors as best effort. The issue's claim that wrapping the child error directly
tears down the parent is therefore not supported by these code paths.

This does not establish which third-party extension was present in the reporter's
browser or reproduce every version of Brave/Chrome. It reproduces the same
parent-level error using a controlled foreign iframe and falsifies the proposed
auto-attach requirement.

## Recovery

Use `tab inspect <ref>` for browser-level metadata. Save work before reloading.
Disable the conflicting extension on the affected site, then reload so its
iframe is absent, or use a separate browser profile without that extension.
Do not automatically disable extensions, remove arbitrary page frames, retry
state-changing commands, or turn denied commands into successful empty replies.

# Sessions and tabs in full

<!-- Moved verbatim from skill-data/core/SKILL.md when the entry point was slimmed to an everyday loop (issue #781, G7). -->

## Sessions and tabs

On the extension relay your tabs open in the background of the user's own
window, in your project's tab group (`✦ <project>`: the panel's project
folder, else the git repository, else the working directory), shared by every
session working on that project; each session drives only the tabs it opened.
`status` names the group. `open <url>` switches to your tab already showing
that page instead of opening it twice; `open --new-tab <url>` opens another. `tab list --all` uses a passive connection: even a fresh session
starts no daemon, opens no window or tab, and attaches to no page.

**End every task that opened tabs with `done`.** Decide yourself which tabs
the user still needs; do not ask them. `done --keep <tab>` (repeat it; bare
`--keep` is the active tab) leaves a page the user asked to see, a result they
will act on, or a logged-in page left for them to finish, as a plain tab
outside your group. Every other tab you opened closes, and the session ends.
Close searches, sources, intermediate steps, errors and duplicates. A session
that ends without `done` has its tabs closed after 30 idle minutes
(`AGENT_BROWSER_GROUP_CLEANUP_MS`; `0` turns this off), except a
tab the user is looking at or switched to. A new task in the same project
takes over the tabs of the project's ended sessions. `tabs cleanup` clears
ended sessions' groups now, when the user asks for that.
Extension 0.5.48 has a separate guarded native-grouping capability for Thermo;
it does not grant chrome-use sessions ownership of the user's tabs.

Use one session name per task and reuse it. After "session unresponsive",
make the one move it names. Do not close, navigate, or reconfigure tabs or
sessions you did not create or adopt. `done` (or `close`) only your own session;
`close --all` would close other agents' sessions, so it refuses; never force it.
`close` reads every tab back and reports `tabsClosed` and `verifiedAbsent: true`;
`close incomplete` means a tab is still open or unconfirmed, and the session is
kept: read the error and retry `close`, do not trust a chained `; close` exit
code. `tab close` on your session's last tab ends the session
(`sessionClosed: true`), so no separate `close` is needed.
Your tab need not be in front: a click on a background tab still lands and its
result arrives later, so `wait --text` for it instead of `tab select --activate`.
The user is working in the same Chrome. Never use `--activate`, `--front` or
`bringToFront` unless the user asked to see the tab: each one switches the tab
in front of a window and can bring Chrome over whatever the user is doing.
Never `tab adopt` a tab the user opened unless they asked you to work in it;
your own tabs are the ones in your group, and that is where you work.
When the user asks which tabs they have open, or to find one of their tabs,
run `tab list --all`: every tab of the connected profile, across windows, by
observation only (nothing is attached or activated, no page is read). Each
row has a `handle`, window, title, url and owner: `this session`,
`session <name>` (another chrome-use session, live or not) or `the user`.
Urls are as Chrome reports them and may carry tokens: quote only what the
user needs. Acting on a tab you do not own needs `--tab <handle> --force`
(any page command, in place and in the background) or
`tab close <handle> --force`. Use `--force` only when the user asked you to
act on that tab, and tell them which tab you touched (the result's
`forcedTab` has its title and url). Never force another session's tab
unless the user asked; the result warns when that session is live. `close`
never closes a forced tab; it stays open for the user.
For which pages leak or grow (heap, DOM nodes, listeners), run
`diag pages --json` (read-only; `--measure`, `--watch <s>`; `--force` only on
the user's request); its schema is in references/diag-pages.md.
Browser commands normally require an open window in the selected Chrome profile. If its
relay is down while Chrome is running and Local State does not list that profile as
active, recovery first waits for the same profile to remain unlisted for 20 seconds. This
gives a recently opened window or relay time to finish starting; active or unknown
metadata cancels the pending window request. Recovery then requests one profile window and
checks again 20 seconds after that request, within the existing 45-second relay wait.
Opening a window does not prove the relay connected. This recovery can briefly bring
Chrome to the foreground on macOS. Focus restoration is best effort; it does not guarantee
returning to another Chrome window. This exception applies only to profile recovery;
ordinary tab actions stay in the background. `status` only reads metadata and never opens
a window. `AGENT_BROWSER_NO_AUTO_RECONNECT` disables automatic recovery. Missing or
malformed metadata does not trigger a window request. An ambiguous process command line
still counts as a running user browser; only a clearly separate automation instance on
another data root is excluded. If recovery still cannot connect, follow the reported
profile or relay error; do not restart Chrome merely because its relay is down.
`profile not open` names the profile that is still unlisted after recovery.
`profile window unavailable` means the window state could not be verified.
After
`first_tab_cleanup_incomplete`, run `close` in the same session before
opening again.
An empty tab list is not a disconnected browser: read the error before
restarting anything, and do not escape to `open --launch`.
After a reconnect your tab ids and labels still name the same tabs. If the
reply says the tab you were driving is gone and commands are refused, run
`tab list`, pick the tab you mean with `tab select <ref>` (or `tab new`), then
`snapshot -i` again; do not reuse the old refs or the lost tab's id.

When chrome-use got in your way (a failure you worked around, a misleading
error, a missing feature), offer the user `chrome-use report --note "<goal>"`
at the end of the task. File it (`--submit --yes`) only once they agree,
unless `report.auto` is set.

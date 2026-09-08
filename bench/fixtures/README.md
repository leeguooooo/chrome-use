# Browser action outcome fixture

`action-outcome.html` counts actual button activations. Serve this directory on a
loopback HTTP port and open exactly one copy through the candidate relay session.

Run `action-outcome-probe.js` with `chrome-use eval --file` from a separate driver
attached to the candidate extension's own popup. The probe imports the candidate
`tab-command.js`, sends a real button click, then deliberately drops its successful
acknowledgement by throwing a transport-style error.

Keep the fixture tab attached immediately before the probe (for example, read its
title through the relay); idle detach otherwise makes the setup invalid. Reload
the fixture before every probe so its submission counter starts at zero.

The pass criterion is `data.result.passed === true`, with one submission, one
dispatch, zero recovery calls, and `action_outcome_unknown` in the failure field.
An `eval` command exiting successfully does not mean this test passed. This is an
injected acknowledgement-loss test, not a spontaneous Chrome disconnect repro.

## Network-only observations

`observation-requests-probe.js` fetches 25 large data URLs without changing the
fixture DOM. Run it with `eval --file ... --observe` in both text and JSON modes.
Assert `changed:false`, `requestsTotal:25`, `requestsOmitted:5`, and exactly 20
summaries with no embedded payload. Confirm that `network requests --json` still
contains the complete captured URLs. The text response must show the summaries
even when the page did not change.

## Private relay and frame restrictions

Start `bench/private-relay-browser.py` with a freshly built candidate CLI, Chrome
for Testing, the ab-connect source directory, and `restricted-frame-extension`.
It creates a private native-host registry and opens no remote debugging port.
Use the state file's `registry` as `CHROME_USE_RELAY_DIR`, `socketDir` as
`AGENT_BROWSER_SOCKET_DIR`, and an explicit browser ID from scoped `browsers`.
Do not register this browser in shared discovery. Preserve unexpected user tabs
before stopping any test browser.

Serve these fixtures on loopback. The companion extension adds a protected
extension iframe on `127.0.0.1`; `localhost` is the plain control. The native
Chrome debugger should refuse the protected page with `debugger_access_denied`,
while `tab list` / `tab inspect` remain available. This is an expected access
restriction, not a successful content interaction.

`bench/relay-isolation-check.py` checks two named sessions on the plain control:
independent counters, navigation in one without changing the other, and foreign
selection refusal (or earlier exclusion from discovery). It records actual
responses and closes only its named sessions, including after failure. It omits
other tabs from saved tab-list responses. Run it again while the protected page
is foregrounded using an independent UI control, and verify focus stayed there.
On macOS use the short socket directory in the state file; profile paths can
exceed Unix socket path limits.

## Lifecycle access checks

Run `bench/fixture-server.py` for the slow-resource endpoint, then start a private
relay fixture browser as above. Run `bench/access-wait-check.py --state-file ...
--port ... --output ...`. It asserts early protected-frame denial for load,
domcontentloaded and none, while a normal page still waits for a two-second
resource and verifies that image loaded successfully. The script closes only its
named sessions. Stop the browser holder and wait for process exit before removing
its profile or socket directory.

## Observation capture failure

`observation-failure.html` and `observation-form-failure.html` first record a POST
in the fixture server, then ask the companion extension to add a protected frame.
Use `?trigger=after` so the initial capture remains available. Run
`bench/observation-capture-check.py` with a private browser state file, candidate
binary and fixture port. The counter API verifies one actual action. JSON/text
must report an unavailable observation without rewriting the action result,
inventing removals or an empty URL, or suggesting action replay. Form validation
must use `errors:null` when it could not be captured. The script's backend counter
reads are fixture assertions, separate from browser task commands.

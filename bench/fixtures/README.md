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

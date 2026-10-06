# CAPTCHA workflow

For a user-authorized login or browser task, inspect and attempt an ordinary
CAPTCHA before handing off. Do not stop just because the page says CAPTCHA,
security verification, or human check. Follow the host's safety rules and the
user's restrictions. Solve the visible challenge through normal controls;
never remove the check, modify verification responses, or claim success from
a dispatched action.

## Identify the current challenge

Reuse the task's session and browser. Do not reload the login page. When
coordinate input is needed on the relay, foreground the intended tab BEFORE
capturing the image and measuring the viewport. Activation can change the
viewport and invalidate coordinates from a background screenshot. Read the
challenge text with a scoped snapshot; for an image challenge, capture and
actually view a screenshot. Discover child frames with `frames` when needed. Prefer the current stable
`frameId` for frame-scoped reads: another tab opening an iframe can change
numeric frame indexes. Confirm the target frame's host before using its data.
A slider can be replaced by an ordered-icon challenge after a failure: read
the current instruction, not the previous challenge's type. Hidden slider
DOM is not proof a slider is active.

`observed.humanCheck` reports a vendor script loaded during an unchanged action.
It is a diagnostic lead, not proof the check needs a person or is unsolvable.
Do not repeat the login/SMS submit to diagnose it. Inspect the challenge instead.

## Slider puzzles

For an ordinary NetEase Yidun puzzle on the main page:

```bash
chrome-use solve-slider 1      # initial attempt plus at most one retry
```

A verified success returns `solved:true`; exhausted attempts return an error
and a nonzero exit code. Read the page's verification result. The ordinary detector does
not cover every enhanced/icon-shaped puzzle or cross-origin frame. If it
cannot locate the gap, view a fresh screenshot, locate the piece and gap when
unambiguous, and use `drag <handle-ref> <dx[,dy]>`. Inspect again after a failed
attempt; refreshes invalidate the old positions. Do not repeatedly drag the
same guessed offset.

## Ordered icons and image clicks

Read the requested icon order from the screenshot, then locate each matching
icon in the challenge image. Do not reuse the positions below: they illustrate
syntax only. Foreground the intended tab before coordinate input on the relay.

Clicks use viewport CSS pixels. Screenshots can have different dimensions.
Measure the dimensions of the image you actually view and the current viewport.
An image viewer may resize the saved file again; use coordinates in that viewed
image, not the dimensions of a different representation. If icons are small,
enlarge the relevant region before identifying them. Do not act on a partial
loading screenshot. Measure both dimensions:

```bash
chrome-use bringToFront
chrome-use screenshot ./challenge.png
chrome-use eval 'JSON.stringify({width:innerWidth,height:innerHeight})'
# Convert EACH screenshot point: x_css=x_image*viewport_width/image_width,
# y_css=y_image*viewport_height/image_height. Do not assume devicePixelRatio.
chrome-use --humanize human batch 'click 484 381' 'click 419 338' 'click 560 335'
```

Use separate x/y scales. These formulas apply to an uncropped viewport
screenshot; for a cropped screenshot add the measured crop origin, and for
full-page screenshots account for scroll position. Prefer an uncropped viewport
screenshot for a challenge. Check the screenshot still represents the same
challenge before acting. If the sequence requires a Verify button, click that
visible button once after selecting the targets.

This uses the agent's image understanding plus ordinary clicks. `solve-slider`
does not automatically recognize ordered icons. If you cannot distinguish a
target confidently, inspect a clearer image rather than guessing.

## Verify and continue

Read the site's success/failure text and current visible challenge state.
A command's Done receipt only means the input was dispatched. A vanished dialog
alone may mean it was closed. A frozen resend counter while the challenge is
still visible is not a success receipt. A CAPTCHA provider can report success
while the site's SMS/login endpoint rejects it; inspect that endpoint's result
without printing credentials before reporting the login step complete. For an SMS flow, a success message and a resend
countdown establish progress; the countdown does not prove SMS delivery or login.
Continue the already-authorized task without another permission question after
a verified success. Verify the authenticated account/target page after login;
leaving the signin URL alone is insufficient.

Use at most three fresh challenge attempts in this workflow (including retries
inside `solve-slider`). Stop retries on rate limiting, lockout, or an instruction
requiring personal presence. Refresh only the challenge after explicit failure,
not the whole form or the SMS request. For an unknown result, inspect before any
repeat submission. Never print passwords, SMS codes, or credential-bearing forms.

If attempts fail, targets remain ambiguous, or the step requires a hardware key,
phone approval, or another unavailable capability, keep the page and report the
specific blocker and attempts. Use `session handoff` as the fallback. Stop driving
that session; resume only after the user confirms they have finished. Preserve
other sessions and tabs throughout.

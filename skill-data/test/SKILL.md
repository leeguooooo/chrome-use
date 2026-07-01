---
name: test
description: Write and run re-runnable, unit-test-style browser test suites with `chrome-use test <suite.yaml>`. Use when repetitive manual browser checks (does the page load logged in? is this element there? did the flow work?) should become a fixed, repeatable regression suite instead of being re-done by hand each time — frontend automated testing on top of chrome-use.
---

# chrome-use test — browser test suites

Turn the repetitive "open it, click around, check it's right" work into a
**re-runnable suite**, like unit tests for the frontend. Every time you find a
regression, add a case — the suite gets more valuable the more you use it.

```
chrome-use test <suite.yaml> [--launch | --session <name>] [--json]
```

- Exit code **0** if all cases pass, **1** if any fail → drop it straight into CI.
- Default: launches a fresh isolated browser (deterministic, repeatable) in a
  `cu-test` session and closes it after. Pass `--session <name>` to run against an
  already-connected session (e.g. the live Chrome via `chrome-use extension connect`).
- Failed cases auto-save a screenshot to `cu-test-artifacts/<case>.png`.

## Suite format (YAML)

```yaml
suite: chatgpt smoke               # label (optional)
setup:                             # runs once before all cases (optional)
  - account: chatgpt/huayue        # inject a cookie-use stored login (optional)
  - open: https://chatgpt.com/     # …or any normal step
cases:
  - name: home loads logged in
    steps:                         # steps reuse chrome-use's own commands
      - open: https://chatgpt.com/
      - wait: { load: networkidle }
    assert:                        # all asserts must hold or the case fails
      - url: { contains: chatgpt.com }
      - visible: "#prompt-textarea"
  - name: composer takes text
    steps:
      - fill: { sel: "#prompt-textarea", text: "hi" }
    assert:
      - text: { sel: "#prompt-textarea", contains: hi }
      - eval: "!!window.__NEXT_DATA__"
```

## Steps (the verbs)

Each step is a one-key mapping; the key is a chrome-use command:

| Step | Meaning |
|---|---|
| `open: <url>` | navigate |
| `click: <selector\|@ref>` | click |
| `fill: { sel: <s>, text: <t> }` | clear + type |
| `type: { sel: <s>, text: <t> }` | type (no clear) |
| `press: <key>` | key press (e.g. `Enter`) |
| `wait: <ms>` / `wait: { load: networkidle }` / `wait: <selector>` | wait |
| `scroll: <up\|down\|...>` or `{ dir: down, px: 500 }` | scroll |
| `eval: "<js>"` | run JS |

## Assertions (the checks) — all compile to one truthy `eval`

| Assert | Passes when |
|---|---|
| `url: { contains\|equals\|matches: <v> }` | the page URL matches |
| `visible: <selector>` | element exists and is laid out |
| `hidden: <selector>` | element is absent / not laid out |
| `text: { sel: <s>, contains\|equals\|matches: <v> }` | element text matches |
| `count: { sel: <s>, eq: <n> }` | exactly N elements match |
| `eval: "<js>"` | the JS expression is truthy |

## Auth & multi-account

`account: <id>` injects a [cookie-use](https://github.com/leeguooooo/cookie-use)
stored login into the test session. Use it in **two** places:

- **`setup:`** — inject once before all cases, so the whole suite runs as that account.
- **inside a case's `steps:`** — switch accounts mid-suite. The new login takes
  effect on the next `open`/navigation in that case, so put `account:` before the
  `open`. This is how you test "admin sees X, member doesn't" in one suite, or
  bounce back and forth between accounts.

```yaml
setup:
  - account: myapp/admin           # start as admin
cases:
  - name: admin sees the settings tab
    steps:
      - open: https://app.example.com/
    assert:
      - visible: "#settings-tab"
  - name: member does NOT see it
    steps:
      - account: myapp/member      # switch account, then reload the page
      - open: https://app.example.com/
    assert:
      - hidden: "#settings-tab"
  - name: back to admin
    steps:
      - account: myapp/admin
      - open: https://app.example.com/
    assert:
      - visible: "#settings-tab"
```

(Needs `cookie-use` installed — see https://github.com/leeguooooo/cookie-use;
skip the line if you don't use it.) An `account:` switch that fails — bad id, or
`cookie-use` missing — fails that case with a clear reason.

## Workflow

1. Do the check once by hand with `open`/`snapshot`/`eval` to learn the selectors.
2. Write it up as a case in a `*.yaml` suite.
3. `chrome-use test suite.yaml` — green means it works; red shows the failing
   assert + a screenshot.
4. Found a regression later? Add a case. Run the whole suite in CI.

## Limits (v1)

Assertions are evaluated independently after the steps run. No per-case retries,
no parallel cases, no snapshot/screenshot baseline diffing yet (use an `eval`
assert against known content for now). Steps run sequentially; a failing step
fails the case immediately.

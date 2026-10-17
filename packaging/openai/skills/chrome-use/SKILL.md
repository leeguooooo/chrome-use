---
name: chrome-use
description: Operate web pages in the user's own Chrome on their connected computer through the chrome-use command-line tool — open pages, read them as structured text, click, fill forms, upload files, take screenshots, and test web apps, using the sites the user is already signed in to. Use when the user asks to do something on a website in their browser, check a page that needs their login, fill or test a form, or verify how a web app behaves.
---

# chrome-use

Drives the open-source `chrome-use` CLI, which controls the Chrome browser on the user's computer. It reads pages through Chrome's accessibility tree and gives each control a short ref (`@e3`), so actions target real elements instead of screen coordinates.

## Before the first call

1. Run `chrome-use --version`. If the command is not found, stop and tell the user chrome-use must be installed on their computer first. Point them to https://github.com/leeguooooo/chrome-use#install; do not download or run an installer yourself.
2. Run `chrome-use skills get core` once per conversation. It prints the usage guide that matches the installed version; follow it.

## The action loop

```bash
chrome-use open https://example.com        # open a page in a new or current tab
chrome-use read                            # the page as readable text
chrome-use snapshot -i                     # interactive controls with @refs
chrome-use click @e3 --observe             # act, and see what changed
chrome-use fill @e2 "hello" --observe
chrome-use screenshot page.png
```

- Take a fresh `snapshot -i` before acting on a new page; refs reset after navigation.
- Pair actions with `--observe` and read the result before acting again. "No change" does not prove failure; read the `why` note first.
- Never repeat an action that may already have taken effect, such as a send, submit, purchase or delete. Check the page state instead.

## Rules

- Act only on sites and tasks the user asked for, using their existing access. Do not create accounts, scrape at volume, or work around a site's rate limits or access controls.
- Before submitting a form, sending a message, buying something, or changing account settings, show the user what will happen and get their approval.
- Sensitive field values (card numbers, security codes, passwords, one-time codes) are masked in `snapshot` and `get value` output. Do not reveal them in the conversation.
- If a page shows a CAPTCHA or a "verify you are human" check (chrome-use reports known ones as `blocked_by_human_check`), or a sign-in page refuses the browser, stop and ask the user to complete that step in their own browser; do not try to get past it.
- Identity checks, live selfies, and password-manager unlocks are always the user's to do.

## Limits

- Needs Chrome and the chrome-use CLI on the user's computer, and works only while that computer is connected and Chrome is running. In a cloud environment without them, say so instead of guessing.
- Uses the user's own signed-in sessions; it cannot reach sites the user cannot.

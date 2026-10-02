# chrome-use privacy policy

Last updated: 2026-10-02

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

## What it keeps on your computer

chrome-use keeps no browsing history. Under `~/.chrome-use` it stores:

- small session bookkeeping files (which tabs a session opened; no page
  content);
- installed site adapters and, if downloaded, Chrome for Testing;
- logins you choose to save with `auth save`, encrypted (AES-256-GCM) with a key
  kept on your computer;
- browser state you choose to export with `state save`, and screenshots,
  downloads or recordings you ask for, where you ask for them.

Delete `~/.chrome-use` to remove all of it. Your browser's own data stays under
your control in Chrome.

## Contact

Questions: open an issue at https://github.com/leeguooooo/chrome-use/issues or
email leeguooooo@gmail.com.

# Command Reference

Complete reference for all chrome-use commands. For quick start and common patterns, see SKILL.md.

## Navigation

```bash
chrome-use open            # Launch browser (no navigation); stays on about:blank.
                              # Pair with `network route`, `cookies set --curl`, or
                              # `addinitscript` to stage state before the first navigation.
chrome-use open <url>      # Launch + navigate (aliases: goto, navigate)
                              # Supports: https://, http://, file://, about:, data://
                              # Auto-prepends https:// if no protocol given
chrome-use back            # Go back
chrome-use forward         # Go forward
chrome-use reload          # Reload page
chrome-use pushstate <url> # SPA client-side navigation. Auto-detects
                              # window.next.router.push (triggers RSC fetch on Next.js);
                              # falls back to history.pushState + popstate/navigate events.
chrome-use close           # Close browser (aliases: quit, exit)
chrome-use connect 9222    # Connect to browser via CDP port
```

### Pre-navigation setup (one-turn batch)

```bash
chrome-use batch \
  '["open"]' \
  '["network","route","*","--abort","--resource-type","script"]' \
  '["cookies","set","--curl","cookies.curl","--domain","localhost"]' \
  '["navigate","http://localhost:3000/target"]'
```

`open` with no URL gives you a clean launch so any interception, cookies,
or init scripts you register take effect on the *first* real navigation.
Use for SSR-only debug (`--resource-type script`), protected-origin auth,
or capturing fresh `react suspense`/`vitals` state without noise from a
prior page.

## Snapshot (page analysis)

```bash
chrome-use snapshot            # Full accessibility tree
chrome-use snapshot -i         # Interactive elements only (recommended)
chrome-use snapshot -c         # Compact output
chrome-use snapshot -d 3       # Limit depth to 3
chrome-use snapshot -s "#main" # Scope to CSS selector
```

## Interactions (use @refs from snapshot)

```bash
chrome-use click @e1           # Click
chrome-use click @e1 --new-tab # Click and open in new tab
chrome-use dblclick @e1        # Double-click
chrome-use focus @e1           # Focus element
chrome-use fill @e2 "text"     # Clear and type
chrome-use type @e2 "text"     # Type without clearing
chrome-use press Enter         # Press key (alias: key)
chrome-use press Control+a     # Key combination
chrome-use keydown Shift       # Hold key down
chrome-use keyup Shift         # Release key
chrome-use hover @e1           # Hover
chrome-use check @e1           # Check checkbox
chrome-use uncheck @e1         # Uncheck checkbox
chrome-use select @e1 "value"  # Select dropdown option
chrome-use select @e1 "a" "b"  # Select multiple options
chrome-use scroll down 500     # Scroll page (default: down 300px)
chrome-use scrollintoview @e1  # Scroll element into view (alias: scrollinto)
chrome-use drag @e1 @e2        # Drag and drop
chrome-use upload @e1 file.pdf # Upload files
```

## Get Information

```bash
chrome-use get text @e1        # Get element text
chrome-use get html @e1        # Get innerHTML
chrome-use get value @e1       # Get input value
chrome-use get attr @e1 href   # Get attribute
chrome-use get title           # Get page title
chrome-use get url             # Get current URL
chrome-use get cdp-url         # Get CDP WebSocket URL
chrome-use get count ".item"   # Count matching elements
chrome-use get box @e1         # Get bounding box
chrome-use get styles @e1      # Get computed styles (font, color, bg, etc.)
```

## Check State

```bash
chrome-use is visible @e1      # Check if visible
chrome-use is enabled @e1      # Check if enabled
chrome-use is checked @e1      # Check if checked
```

## Screenshots and PDF

```bash
chrome-use screenshot          # Save to temporary directory
chrome-use screenshot path.png # Save to specific path
chrome-use screenshot --full   # Full page
chrome-use pdf output.pdf      # Save as PDF
```

Headless Chromium screenshots hide native scrollbars for consistent image output.
Pass `--hide-scrollbars false` when launching to keep native scrollbars visible.

## Video Recording

```bash
chrome-use record start ./demo.webm    # Start recording
chrome-use click @e1                   # Perform actions
chrome-use record stop                 # Stop and save video
chrome-use record restart ./take2.webm # Stop current + start new
```

## Wait

```bash
chrome-use wait @e1                     # Wait for element
chrome-use wait 2000                    # Wait milliseconds
chrome-use wait --text "Success"        # Wait for text (or -t)
chrome-use wait --url "**/dashboard"    # Wait for URL pattern (or -u)
chrome-use wait --load networkidle      # Wait for network idle (or -l)
chrome-use wait --fn "window.ready"     # Wait for JS condition (or -f)
```

## Mouse Control

```bash
chrome-use mouse move 100 200      # Move mouse
chrome-use mouse down left         # Press button
chrome-use mouse up left           # Release button
chrome-use mouse wheel 100         # Scroll wheel
```

## Semantic Locators (alternative to refs)

```bash
chrome-use find role button click --name "Submit"
chrome-use find text "Sign In" click
chrome-use find text "Sign In" click --exact      # Exact match only
chrome-use find label "Email" fill "user@test.com"
chrome-use find placeholder "Search" type "query"
chrome-use find alt "Logo" click
chrome-use find title "Close" click
chrome-use find testid "submit-btn" click
chrome-use find first ".item" click
chrome-use find last ".item" click
chrome-use find nth 2 "a" hover
```

## Browser Settings

```bash
chrome-use set viewport 1920 1080          # Set viewport size
chrome-use set viewport 1920 1080 2        # 2x retina (same CSS size, higher res screenshots)
chrome-use set device "iPhone 14"          # Emulate device
chrome-use set geo 37.7749 -122.4194       # Set geolocation (alias: geolocation)
chrome-use set offline on                  # Toggle offline mode
chrome-use set headers '{"X-Key":"v"}'     # Extra HTTP headers
chrome-use set credentials user pass       # HTTP basic auth (alias: auth)
chrome-use set media dark                  # Emulate color scheme
chrome-use set media light reduced-motion  # Light mode + reduced motion
```

## Cookies and Storage

```bash
chrome-use cookies                     # Get all cookies
chrome-use cookies set name value      # Set cookie
chrome-use cookies clear               # Clear cookies
chrome-use storage local               # Get all localStorage
chrome-use storage local key           # Get specific key
chrome-use storage local set k v       # Set value
chrome-use storage local clear         # Clear all
```

## Network

```bash
chrome-use network route <url>              # Intercept requests
chrome-use network route <url> --abort      # Block requests
chrome-use network route <url> --body '{}' --status 200 --header K=V --content-type application/json  # Mock response (fulfill)
chrome-use network route <url> --method POST --set-body '{}' --set-header K=V --rewrite-url <u>       # Rewrite request (continue)
chrome-use network unroute [url]            # Remove routes
chrome-use network requests                 # View tracked requests
chrome-use network requests --filter api    # Filter requests
```

## Tabs and Windows

```bash
chrome-use tab                              # List tabs with tabId and label
chrome-use tab new [url]                    # New tab
chrome-use tab new --label docs [url]       # New tab with a memorable label
chrome-use tab t2                           # Switch to tab by id
chrome-use tab docs                         # Switch to tab by label
chrome-use tab close                        # Close current tab
chrome-use tab close t2                     # Close tab by id
chrome-use tab close docs                   # Close tab by label
chrome-use window new                       # New window
```

Tab ids are stable strings of the form `t1`, `t2`, `t3`. They're never reused
within a session, so the same id keeps referring to the same tab across
commands. Positional integers are **not** accepted — `tab 2` errors with a
teaching message; use `t2`.

User-assigned labels (`docs`, `app`, `admin`) are interchangeable with ids
everywhere a tab ref is accepted. Labels are the agent-friendly way to write
multi-tab workflows:

```bash
chrome-use tab new --label docs https://docs.example.com
chrome-use tab new --label app  https://app.example.com
chrome-use tab docs                   # switch to docs
chrome-use snapshot                   # populate refs for docs
chrome-use click @e1                  # ref click on docs
chrome-use tab app                    # switch to app
chrome-use tab close docs             # close by label
```

Labels are never auto-generated, never rewritten on navigation, and must be
unique within a session. To interact with another tab, switch to it first:
the daemon maintains a single active tab, so refs (`@eN`) belong to the tab
that was active when the snapshot ran.

## Frames

```bash
chrome-use frame "#iframe"     # Switch to iframe by CSS selector
chrome-use frame @e3           # Switch to iframe by element ref
chrome-use frame main          # Back to main frame
```

### Iframe support

Iframes are detected automatically during snapshots. When the main-frame snapshot runs, `Iframe` nodes are resolved and their content is inlined beneath the iframe element in the output (one level of nesting; iframes within iframes are not expanded).

```bash
chrome-use snapshot -i
# @e3 [Iframe] "payment-frame"
#   @e4 [input] "Card number"
#   @e5 [button] "Pay"

# Interact directly — refs inside iframes already work
chrome-use fill @e4 "4111111111111111"
chrome-use click @e5

# Or switch frame context for scoped snapshots
chrome-use frame @e3               # Switch using element ref
chrome-use snapshot -i             # Snapshot scoped to that iframe
chrome-use frame main              # Return to main frame
```

The `frame` command accepts:
- **Element refs** — `frame @e3` resolves the ref to an iframe element
- **CSS selectors** — `frame "#payment-iframe"` finds the iframe by selector
- **Frame name/URL** — matches against the browser's frame tree

## Dialogs

By default, `alert` and `beforeunload` dialogs are automatically accepted so they never block the agent. `confirm` and `prompt` dialogs still require explicit handling. Use `--no-auto-dialog` to disable this behavior.

```bash
chrome-use dialog accept [text]  # Accept dialog
chrome-use dialog dismiss        # Dismiss dialog
chrome-use dialog status         # Check if a dialog is currently open
```

## JavaScript

```bash
chrome-use eval "document.title"          # Simple expressions only
chrome-use eval -b "<base64>"             # Any JavaScript (base64 encoded)
chrome-use eval --stdin                   # Read script from stdin
```

Use `-b`/`--base64` or `--stdin` for reliable execution. Shell escaping with nested quotes and special characters is error-prone.

```bash
# Base64 encode your script, then:
chrome-use eval -b "ZG9jdW1lbnQucXVlcnlTZWxlY3RvcignW3NyYyo9Il9uZXh0Il0nKQ=="

# Or use stdin with heredoc for multiline scripts:
cat <<'EOF' | chrome-use eval --stdin
const links = document.querySelectorAll('a');
Array.from(links).map(a => a.href);
EOF
```

## State Management

```bash
chrome-use state save auth.json    # Save cookies, storage, auth state
chrome-use state load auth.json    # Restore saved state
```

## Global Options

```bash
chrome-use --session <name> ...    # Isolated browser session
chrome-use --json ...              # JSON output for parsing
chrome-use --headed ...            # Default & always-on (stealth). Headless is FORBIDDEN
                                      #   (bot tell); display-less servers: AGENT_BROWSER_ALLOW_HEADLESS=1
chrome-use --full ...              # Full page screenshot (-f)
chrome-use --cdp <port> ...        # Connect via Chrome DevTools Protocol
chrome-use -p <provider> ...       # Cloud browser provider (--provider)
chrome-use --proxy <url> ...       # Use proxy server
chrome-use --proxy-bypass <hosts>  # Hosts to bypass proxy
chrome-use --headers <json> ...    # HTTP headers scoped to URL's origin
chrome-use --executable-path <p>   # Custom browser executable
chrome-use --extension <path> ...  # Load browser extension (repeatable)
chrome-use --ignore-https-errors   # Ignore SSL certificate errors
chrome-use --hide-scrollbars false # Keep native scrollbars visible in headless Chromium screenshots
chrome-use --help                  # Show help (-h)
chrome-use --version               # Show version (-V)
chrome-use <command> --help        # Show detailed help for a command
```

## Drive your real, logged-in Chrome (extension — zero confirmation)

Chrome 136 blocked `--remote-debugging-port` on the default profile, so to drive
the user's *existing* logged-in window, chrome-use uses a Chrome **extension**
over native messaging — no port, no token, no per-use confirmation (the
codex/claude approach).

One-time setup:
```bash
chrome-use extension install        # writes the native-messaging host manifest
```

The native-messaging host accepts **both** extension origins, so either install
works — but prefer the Store build:

1. **Chrome Web Store (recommended)** — one-click *Add to Chrome*:
   <https://chromewebstore.google.com/detail/chrome-use/knfcmbamhjmaonkfnjhldjedeobeafmk>
   Restart-stable and auto-updating (store id `knfcmbamhjmaonkfnjhldjedeobeafmk`).
2. **Load unpacked (dev)** — load `<repo>/extensions/ab-connect` from source;
   its pinned `key` gives the stable id `ciiljdlhd…`. NOTE: Load-unpacked
   extensions can be disabled/dropped on Chrome restart (Developer-mode handling),
   which silently drops the relay — so for unattended setups use the Store build.

For Load unpacked — a GUI step (Chrome's `chrome://extensions` is privileged; the
CLI can't load an unpacked extension):

> chrome://extensions → enable **Developer mode** (top-right) → **Load unpacked** →
> select `<repo>/extensions/ab-connect` (it appears in the list as
> **chrome-use**)

Once loaded, the relay goes live and plain `chrome-use open <url>` connects
through it automatically — `auto_connect_cdp` prefers the live extension relay
over a raw `--remote-debugging-port`, so Chrome 136+'s "Allow remote debugging?"
consent popup never appears. `chrome-use extension connect` is the explicit
form of the same path.

**You can do this load step yourself with a computer-use / GUI-automation tool**
(e.g. the `cua-driver` skill) — drive `chrome://extensions`, toggle Developer
mode, click *Load unpacked*, pick the folder in the Open dialog. If the extension
is already loaded, clicking its **Reload** (↻) button after a code change is
enough. Notes from doing this live: tools that send *synthetic keystrokes* (e.g.
peekaboo) often don't reach Chrome — **`cua-driver` works** because it reads
Chrome's accessibility tree and clicks real elements. The native "Open" file
dialog is the fiddly part; if keystroke entry there fails, ask the user to pick
the folder (one click). After it loads, Chrome assigns the extension a fixed id
(pinned in its manifest) and auto-connects the host.

Then, any time (pure CLI, zero confirmation):
```bash
chrome-use extension connect        # auto-attaches to the live, logged-in tabs
chrome-use tab                      # list the real tabs it now controls
chrome-use tab t3                   # switch the session to one of them
chrome-use snapshot -i / eval / click ...   # drive it like any session
chrome-use extension status         # is the host installed?
chrome-use extension uninstall      # remove the host manifest
```

Security: the extension↔host link is authenticated by Chrome (extension id); the
host↔chrome-use CDP link uses an unguessable URL in a 0600 file. Use this when
you need the user's real cookies/login on their actual machine. (`--extension
<path>` is unrelated — that loads an extension into a *launched* browser.)

## Debugging

```bash
chrome-use --headed open example.com   # Show browser window
chrome-use --cdp 9222 snapshot         # Connect via CDP port
chrome-use connect 9222                # Alternative: connect command
chrome-use console                     # View console messages (needs AGENT_BROWSER_CAPTURE_CONSOLE=1)
chrome-use console --clear             # Clear console
chrome-use errors                      # View page errors (needs AGENT_BROWSER_CAPTURE_CONSOLE=1)
chrome-use errors --clear              # Clear errors
chrome-use highlight @e1               # Highlight element
chrome-use inspect                     # Open Chrome DevTools for this session
chrome-use trace start                 # Start recording trace
chrome-use trace stop trace.zip        # Stop and save trace
chrome-use profiler start              # Start Chrome DevTools profiling
chrome-use profiler stop trace.json    # Stop and save profile
```

### Finding a page the user saved (`find-url`)

Search the user's local Chrome/Edge **bookmarks** by keyword — for internal
systems or previously-saved pages that public search can't reach. Local read, no
browser/daemon needed.

```bash
chrome-use find-url jira board          # all keywords must match (name or url)
chrome-use find-url --limit 10 invoices
chrome-use find-url --browser edge --profile "Profile 1" wiki
chrome-use find-url grafana --json      # {results:[{name,url,folder}], count}
```

Results are most-recently-added first. `javascript:`/`data:` bookmarklets are
skipped. (Visited-history search isn't included yet — bookmarks only.)

### Debugging forms / hidden state with `eval`

The a11y `snapshot` shows visible, interactive elements — it does **not** show
hidden inputs or a control's actual submitted value. When a form "looks filled"
but submit-validation rejects it, go straight to the DOM with `eval` instead of
guessing from the snapshot. This is usually the fastest way to find the real
problem (e.g. a hidden `point_choice=none` that the visible UI never exposes):

```bash
# Dump every field's name → value, including hidden inputs and unchecked radios
chrome-use eval "JSON.stringify([...document.forms[0].elements].map(e=>({name:e.name,type:e.type,value:e.value,checked:e.checked})).filter(e=>e.name))"

# Inspect one hidden field directly
chrome-use eval "document.querySelector('[name=point_choice]')?.value"

# Why won't it submit? Ask the browser's own validity API
chrome-use eval "[...document.forms[0].elements].filter(e=>!e.validity?.valid).map(e=>e.name+': '+e.validationMessage)"
```

## React / Web Vitals

Requires `--enable react-devtools` at launch for the `react ...` commands.
`vitals` and `pushstate` are framework-agnostic.

```bash
chrome-use open --enable react-devtools <url>    # Launch with React hook installed
chrome-use react tree                            # Full component tree
chrome-use react inspect <fiberId>               # Props, hooks, state, source
chrome-use react renders start                   # Begin re-render recording
chrome-use react renders stop [--json]           # Stop and print render profile
chrome-use react suspense [--only-dynamic] [--json]  # Suspense boundaries + classifier
                                                         # --only-dynamic hides the "static" list
chrome-use vitals [url] [--json]                 # LCP/CLS/TTFB/FCP/INP + hydration
chrome-use pushstate <url>                       # SPA client-side nav (auto-detects Next router)
```

## Init scripts

```bash
chrome-use open --init-script <path>             # Register before first navigation (repeatable)
chrome-use addinitscript <js>                    # Register at runtime (returns identifier)
chrome-use removeinitscript <identifier>         # Remove a previously registered init script
```

## cURL cookie import

```bash
chrome-use cookies set --curl <file>                             # Auto-detects JSON/cURL/Cookie-header
chrome-use cookies set --curl <file> --domain example.com        # Scope to a domain
```

Supported formats: JSON array of `{name, value}`, a cURL dump from
DevTools -> Network -> Copy as cURL, or a bare Cookie header. Errors never
echo cookie values.

## Network route by resource type

```bash
chrome-use network route '*' --abort --resource-type script       # Block scripts only (SSR-lock pattern)
chrome-use network route '*' --resource-type image,font --body '' # Stub images and fonts
```

## Environment Variables

```bash
AGENT_BROWSER_SESSION="mysession"            # Default session name
AGENT_BROWSER_EXECUTABLE_PATH="/path/chrome" # Custom browser path
AGENT_BROWSER_EXTENSIONS="/ext1,/ext2"       # Comma-separated extension paths
AGENT_BROWSER_INIT_SCRIPTS="/a.js,/b.js"     # Comma-separated init script paths
AGENT_BROWSER_ENABLE="react-devtools"        # Comma-separated built-in init script features
AGENT_BROWSER_HIDE_SCROLLBARS="false"        # Keep native scrollbars visible in headless Chromium screenshots
AGENT_BROWSER_PROVIDER="browserbase"         # Cloud browser provider
AGENT_BROWSER_STREAM_PORT="9223"             # Override WebSocket streaming port (default: OS-assigned)
AGENT_BROWSER_HOME="/path/to/chrome-use"  # Custom install location
AGENT_BROWSER_CLICK_MODE="dom"               # Click strategy: "" (default: scroll-in + coordinate
                                             #   click, DOM-dispatch fallback), "coord" (strict
                                             #   coordinate only), "dom" (always element.click())
```

### Click reliability

`click` auto-scrolls the target into view first, then dispatches a coordinate
click. If that fails (a floating layer fails the occlusion guard, or the point
won't resolve) it falls back to a DOM-dispatched `.click()` on the intended
element. If a click *reports success but the page didn't react* — common for
autocomplete/menu `<li>` items that close on the input's blur — retry that one
with `AGENT_BROWSER_CLICK_MODE=dom` (a DOM dispatch doesn't move focus the way a
real pointer press does, so the item still selects). `=coord` disables the
fallback when you specifically want a hard failure on occlusion.

### Stealth / anti-detection knobs (fork)

```bash
AGENT_BROWSER_CAPTURE_CONSOLE="1"     # Enable `console`/`errors` capture. OFF by default:
                                      #   a live CDP Runtime domain is a detectable bot signal,
                                      #   so console/errors return empty (with a hint) until set.
AGENT_BROWSER_TIMEZONE="Asia/Tokyo"   # --launch only. Native timezone override (IANA id, or
                                      #   "auto" to derive from locale). Aligns Intl+Date to a proxy.
AGENT_BROWSER_BLOCK_WEBRTC="1"        # --launch only. Hide local IP via WebRTC. Auto-forces WebRTC
                                      #   through the proxy when one is set; "0" opts out.
AGENT_BROWSER_HIDE_CANVAS="1"         # --launch only. Session-stable canvas/audio fingerprint noise.
AGENT_BROWSER_ADAPTIVE_REF="0"        # Disable adaptive @ref relocation (on by default; relocates a
                                      #   moved element by fingerprint when role/name re-query fails).
```

> **Heads-up for `console` / `errors`:** capture is **off by default** in this stealth
> fork. Both commands return `{"messages":[]}` / `{"errors":[]}` plus a `hint` until you
> launch the session with `AGENT_BROWSER_CAPTURE_CONSOLE=1`. This keeps the CDP `Runtime`
> domain disabled (a known bot signal) for the common automation path.

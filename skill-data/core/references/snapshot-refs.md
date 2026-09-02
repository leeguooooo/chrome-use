# Snapshot and Refs

Compact element references that reduce context usage dramatically for AI agents.

**Related**: [commands.md](commands.md) for full command reference, [SKILL.md](../SKILL.md) for quick start.

## Contents

- [How Refs Work](#how-refs-work)
- [Snapshot Command](#the-snapshot-command)
- [Using Refs](#using-refs)
- [Ref Lifecycle](#ref-lifecycle)
- [Best Practices](#best-practices)
- [Ref Notation Details](#ref-notation-details)
- [Troubleshooting](#troubleshooting)

## How Refs Work

Traditional approach:
```
Full DOM/HTML → AI parses → CSS selector → Action (~3000-5000 tokens)
```

chrome-use approach:
```
Compact snapshot → @refs assigned → Direct interaction (~200-400 tokens)
```

## The Snapshot Command

```bash
# Basic snapshot (shows page structure)
chrome-use snapshot

# Interactive snapshot (-i flag) - RECOMMENDED
chrome-use snapshot -i
```

### Snapshot Output Format

```
Page: Example Site - Home
URL: https://example.com

@e1 [header]
  @e2 [nav]
    @e3 [a] "Home"
    @e4 [a] "Products"
    @e5 [a] "About"
  @e6 [button] "Sign In"

@e7 [main]
  @e8 [h1] "Welcome"
  @e9 [form]
    @e10 [input type="email"] placeholder="Email"
    @e11 [input type="password"] placeholder="Password"
    @e12 [button type="submit"] "Log In"

@e13 [footer]
  @e14 [a] "Privacy Policy"
```

## Using Refs

Once you have refs, interact directly:

```bash
# Click the "Sign In" button
chrome-use click @e6

# Fill email input
chrome-use fill @e10 "user@example.com"

# Fill password
chrome-use fill @e11 "password123"

# Submit the form
chrome-use click @e12
```

## Ref Lifecycle

Refs are stable for the same backend DOM node across snapshots in one document.
Navigation and tab switches invalidate the identity map.
An annotated screenshot refreshes that same-document snapshot without a hard
reset, so inserting `screenshot --annotate` between snapshot and click does not
renumber unchanged controls.

```bash
# Get initial snapshot
chrome-use snapshot -i
# @e1 [button] "Next"

# Click triggers navigation
chrome-use click @e1

# Re-snapshot after the document boundary
chrome-use snapshot -i
# New-document refs start from @e1
```

## Best Practices

### 1. Always Snapshot Before Interacting

```bash
# CORRECT
chrome-use open https://example.com
chrome-use snapshot -i          # Get refs first
chrome-use click @e1            # Use ref

# WRONG
chrome-use open https://example.com
chrome-use click @e1            # Ref doesn't exist yet!
```

### 2. Re-Snapshot After Navigation

```bash
chrome-use click @e5            # Navigates to new page
chrome-use snapshot -i          # Get new refs
chrome-use click @e1            # Use new refs
```

### 3. Re-Snapshot to Discover Dynamic Changes

```bash
chrome-use click @e1            # Opens dropdown
chrome-use snapshot -i          # See dropdown items
chrome-use click @e7            # Select item
```

Existing DOM nodes keep their refs when a modal/dropdown is inserted or
removed; newly created controls receive new refs. If a framework replaces a
node entirely, the old ref can still self-heal by role/name/fingerprint, but a
fresh snapshot is the safest way to discover the replacement.

### 4. Snapshot Specific Regions

For complex pages, snapshot specific areas:

```bash
# Snapshot just the form
chrome-use snapshot @e9
```

## Ref Notation Details

```
@e1 [tag type="value"] "text content" placeholder="hint"
│    │   │             │               │
│    │   │             │               └─ Additional attributes
│    │   │             └─ Visible text
│    │   └─ Key attributes shown
│    └─ HTML tag name
└─ Unique ref ID
```

### Common Patterns

```
@e1 [button] "Submit"                    # Button with text
@e2 [input type="email"]                 # Email input
@e3 [input type="password"]              # Password input
@e4 [a href="/page"] "Link Text"         # Anchor link
@e5 [select]                             # Dropdown
@e6 [textarea] placeholder="Message"     # Text area
@e7 [div class="modal"]                  # Container (when relevant)
@e8 [img alt="Logo"]                     # Image
@e9 [checkbox] checked                   # Checked checkbox
@e10 [radio] selected                    # Selected radio
```

## Iframes

Snapshots automatically detect and inline iframe content. When the main-frame snapshot runs, each `Iframe` node is resolved and its child accessibility tree is included directly beneath it in the output. Refs assigned to elements inside iframes carry frame context, so interactions like `click`, `fill`, and `type` work without manually switching frames.

```bash
chrome-use snapshot -i
# @e1 [heading] "Checkout"
# @e2 [Iframe] "payment-frame"
#   @e3 [input] "Card number"
#   @e4 [input] "Expiry"
#   @e5 [button] "Pay"
# @e6 [button] "Cancel"

# Interact with iframe elements directly using their refs
chrome-use fill @e3 "4111111111111111"
chrome-use fill @e4 "12/28"
chrome-use click @e5
```

**Key details:**
- Iframes are expanded recursively up to three levels
- Cross-origin iframe sessions are included when Chrome exposes them through CDP
- Empty iframes or iframes with no interactive content are omitted from the output
- To scope a snapshot to a single iframe, use `frame @ref` then `snapshot -i`

## Shadow-root pages (DOM walk fallback)

Some web-component SPAs (developer.apple.com/contact, for example) render the
whole page inside shadow roots and the accessibility tree comes back with no
refs at all. Instead of printing "(no interactive elements)", `snapshot` and
`snapshot -i` then list actionable elements from a `DOM.getDocument(pierce:true)`
walk that covers open and closed shadow roots and same-process child documents.

```bash
chrome-use snapshot -i          # automatic fallback when the AX tree has no refs
chrome-use snapshot -i --dom    # force the DOM walk
```

**Key details:**
- Refs from the DOM listing (`[ref=eN]`) work with `click`, `type` and `fill` like normal refs
- The JSON carries `source: "dom"` and a `note` saying the listing came from the DOM walk
- Roles and names are derived from tags and attributes, so they are coarser than AX roles

## Troubleshooting

### "Ref not found" Error

```bash
# Element may have been replaced or removed — re-snapshot
chrome-use snapshot -i
```

### "Unknown ref" Error

The message names the session that answered and says whether it holds any refs.
"no snapshot has run in this session" means the command reached a different
session than the one you snapshotted (another directory or terminal):

```bash
chrome-use session list                 # find the session that has your refs
chrome-use --session <name> click @e4   # pin it
```

If the error lists the ref range the session does have, the page changed since
the snapshot; re-snapshot instead.

### Element Not Visible in Snapshot

```bash
# Scroll down to reveal element
chrome-use scroll down 1000
chrome-use snapshot -i

# Or wait for dynamic content
chrome-use wait 1000
chrome-use snapshot -i
```

### Too Many Elements

```bash
# Snapshot specific container
chrome-use snapshot @e5

# Or use get text for content-only extraction
chrome-use get text @e5
```

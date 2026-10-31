// Close a password manager's inline menu on an agent tab by taking focus out
// of the field it is attached to (#750 follow-up). While the menu's frame is
// in the tab Chrome refuses every debugger command there, and when Chrome is
// not the frontmost app, hiding the tab changes nothing, so the CLI's tab flip
// cannot close it.
//
// Bitwarden (2026.10, `AutofillOverlayContentService`) closes the menu when
// the field it belongs to blurs and neither of its own frames has the focus;
// the blur handler does not check `isTrusted`, unlike its Escape handler. A
// `blur()` from an isolated-world script fires a trusted blur/focusout on a
// page with focus emulation on (measured with Chrome for Testing 155 behind
// Finder): the same events as the user clicking elsewhere. Nothing is added
// to the page.
//
// The blur is not enough on a background tab of a real Chrome (Bitwarden
// 2026.10, the agent tab behind the user's full-screen window): Bitwarden
// opens its menu asynchronously, so a page that turned hidden right after
// the focus gets the menu back after its hidden-close, and its blur path then
// waits on menu frames that a hidden page never initializes. What closes it
// at once is Bitwarden's `visibilitychange` handler, which reads the real
// `document.visibilityState` and force-closes when it is `hidden` (measured:
// closed within 10 ms, twice out of twice). Dispatching that event is visible
// to the page (an untrusted event with no state change), so it is sent only
// with stealth mode off (`nudge`), and only on a page that is hidden.
//
// Needs host access to the page: the extension holds only optional host
// permissions, granted from the popup or the options page. Without them
// Chrome refuses the script and the caller falls back to its other recoveries.

// Runs in each frame (serialized by chrome.scripting, so self-contained).
// Only an editable field is blurred: in a parent frame the focused element is
// the <iframe> holding the field, and blurring that would move nothing.
// Returns { blurred, nudged }.
export function blurEditableField(nudge) {
  const el = document.activeElement
  let blurred = false
  if (el && el !== document.body && (el.matches?.('input, textarea, select') || el.isContentEditable)) {
    el.blur()
    blurred = true
  }
  let nudged = false
  if (nudge === true && window === top && document.visibilityState === 'hidden') {
    document.dispatchEvent(new Event('visibilitychange'))
    nudged = true
  }
  return { blurred, nudged }
}

/**
 * Blur the focused field in every web frame of `tabId`, an agent tab.
 * deps: { isOwned(tabId), getAllFrames(tabId), executeScript(injection),
 * stealth (true: no visibilitychange nudge) }
 * Returns { blurred, nudged, frames, stealth }; throws when nothing could be
 * scripted.
 */
export async function blurFocusedField(tabId, deps) {
  if (!Number.isInteger(tabId)) throw new Error('blurFocusedField: no tab for this session')
  const requireOwned = () => {
    if (!deps.isOwned(tabId)) {
      throw new Error('blurFocusedField: only a tab chrome-use created; the user\'s own tabs are left alone')
    }
  }
  requireOwned()
  const frames = (await deps.getAllFrames(tabId)) || []
  // The password manager's own frames are chrome-extension:// pages, which
  // the extension may not script; asking for them fails the whole call.
  const frameIds = frames.filter((f) => /^https?:/.test(f.url || '')).map((f) => f.frameId)
  if (frameIds.length === 0) throw new Error('blurFocusedField: the tab has no web frame to script')
  // Frame enumeration yields: the user may have taken the tab back meanwhile.
  requireOwned()
  let results
  try {
    results = await deps.executeScript({
      target: { tabId, frameIds },
      world: 'ISOLATED',
      func: blurEditableField,
      args: [deps.stealth !== true],
    })
  } catch (e) {
    const message = String(e?.message || e)
    if (/permission|Cannot access/i.test(message)) {
      throw new Error(`blurFocusedField: no site access (${message}); the user grants it once: the chrome-use icon in Chrome's toolbar → "Allow on all sites" (or Settings → Site access)`)
    }
    throw new Error(`blurFocusedField: ${message}`)
  }
  const count = (key) => (results || []).filter((r) => r?.result?.[key] === true).length
  return { blurred: count('blurred'), nudged: count('nudged'), frames: frameIds.length, stealth: deps.stealth === true }
}

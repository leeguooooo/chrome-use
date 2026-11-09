// Remove the frames another extension put into an agent tab, so chrome-use can
// drive it again. While a page holds a chrome-extension:// frame of another
// extension, Chrome refuses every chrome.debugger command on the whole tab
// (debugger_access_denied, see scripts/repro-protected-child-frame.md).
// 1Password keeps such a frame in sign-in pages with nothing shown
// (<com-1password-uso>, Universal Sign On, height 0), so there is no menu to
// close and the tab stays blocked for good.
//
// Opt-in from the CLI (--clear-extension-frames), and only on a tab chrome-use
// created: the user's own tabs keep every frame. The frames are removed from an
// isolated-world script (chrome.scripting), which the page cannot see; that
// extension's widget is gone from this page until it adds it again (a reload
// or the next page). Nothing is disabled and no setting changes.

// Hosts 1Password puts its frames in, each with a closed shadow root.
export const PASSWORD_MANAGER_FRAME_HOSTS = Object.freeze([
  'com-1password-uso',
  'com-1password-menu',
  'com-1password-notification',
  'com-1password-modal',
])

// Runs in each web frame (serialized by chrome.scripting, so self-contained).
// Returns { removed: [{ extension, tag, host }] }.
export function removeForeignExtensionFrames(ownId, hostTags, doc) {
  const d = doc || document
  const removed = []
  const prefix = 'chrome-extension://'
  const extensionOf = (src) => {
    if (typeof src !== 'string' || !src.startsWith(prefix)) return null
    const id = src.slice(prefix.length).split('/')[0]
    return id && id !== ownId ? id : null
  }
  const shadowOf = (el) => {
    try {
      // Closed roots too: content scripts may open them (chrome.dom).
      const api = typeof chrome !== 'undefined' ? chrome?.dom?.openOrClosedShadowRoot : null
      return (api ? api(el) : null) || el.shadowRoot || null
    } catch {
      return el.shadowRoot || null
    }
  }
  const roots = [d]
  for (let i = 0; i < roots.length && i < 2000; i++) {
    for (const el of Array.from(roots[i].querySelectorAll('*'))) {
      const tag = String(el.tagName || '').toLowerCase()
      if (tag === 'iframe' || tag === 'frame') {
        const ext = extensionOf(el.src || el.getAttribute?.('src') || '')
        if (ext) {
          const host = el.getRootNode?.()?.host
          removed.push({ extension: ext, tag, host: host ? String(host.tagName || '').toLowerCase() : '' })
          el.remove()
          continue
        }
      }
      const shadow = shadowOf(el)
      if (shadow) roots.push(shadow)
    }
  }
  // A known host whose frame had no extension URL yet (or was already
  // removed above) still goes: it puts the frame back otherwise.
  for (const tag of hostTags || []) {
    for (const el of Array.from(d.getElementsByTagName(tag))) {
      if (!removed.some((r) => r.host === tag)) removed.push({ extension: '', tag, host: tag })
      el.remove()
    }
  }
  return { removed }
}

/**
 * Remove other extensions' frames from every web frame of `tabId`, an agent tab.
 * deps: { isOwned(tabId), getAllFrames(tabId), executeScript(injection), ownId }
 * Returns { removed, frames }; throws when the tab is not chrome-use's or
 * nothing could be scripted.
 */
export async function clearExtensionFrames(tabId, deps) {
  if (!Number.isInteger(tabId)) throw new Error('clearExtensionFrames: no tab for this session')
  const requireOwned = () => {
    if (!deps.isOwned(tabId)) {
      throw new Error('clearExtensionFrames: only a tab chrome-use created; the user\'s own tabs keep their frames')
    }
  }
  requireOwned()
  const frames = (await deps.getAllFrames(tabId)) || []
  // Extension pages cannot be scripted; asking for them fails the whole call.
  const frameIds = frames.filter((f) => /^https?:/.test(f.url || '')).map((f) => f.frameId)
  if (frameIds.length === 0) throw new Error('clearExtensionFrames: the tab has no web frame to script')
  requireOwned()
  let results
  try {
    results = await deps.executeScript({
      target: { tabId, frameIds },
      world: 'ISOLATED',
      func: removeForeignExtensionFrames,
      args: [deps.ownId, [...PASSWORD_MANAGER_FRAME_HOSTS], null],
    })
  } catch (e) {
    const message = String(e?.message || e)
    if (/permission|Cannot access/i.test(message)) {
      throw new Error(`clearExtensionFrames: no site access (${message}); the user grants it once: the chrome-use icon in Chrome's toolbar → "Allow on all sites" (or Settings → Site access)`)
    }
    throw new Error(`clearExtensionFrames: ${message}`)
  }
  const removed = (results || []).flatMap((r) => r?.result?.removed || [])
  return { removed, frames: frameIds.length }
}

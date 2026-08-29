// Testable navigation recovery for the extension relay. The service worker
// supplies Chrome adapters; tests supply deterministic fakes at the same edge.

/** Return whether a Page.navigate request targets the top-level Chrome tab. */
export function canUseBrowserNavigationFallback(method, params, childSessionId) {
  return method === 'Page.navigate' && !childSessionId && params?.frameId == null
}

/** Retry a timed-out top-level debugger navigation through chrome.tabs.update. */
export async function navigateTabWithBrowserFallback(params, deps) {
  const url = typeof params?.url === 'string' ? params.url.trim() : ''
  if (!url) return await deps.navigateWithDebugger()

  try {
    return await deps.navigateWithDebugger()
  } catch (error) {
    if (!deps.isRelayTimeoutError(error)) throw error

    try {
      const tab = await deps.updateTab(url)
      const pendingUrl = typeof tab?.pendingUrl === 'string' ? tab.pendingUrl : ''
      const currentUrl = typeof tab?.url === 'string' ? tab.url : ''
      return {
        frameId: '',
        loaderId: null,
        relayFallback: {
          method: 'browser-level chrome.tabs.update',
          url: pendingUrl || currentUrl || url,
          // When pendingUrl is present, Chrome still reports the previous page's
          // title. Do not stamp that stale title onto the requested destination.
          title: pendingUrl ? '' : typeof tab?.title === 'string' ? tab.title : '',
        },
      }
    } catch (fallbackError) {
      const original = error instanceof Error ? error.message : String(error)
      const fallback =
        fallbackError instanceof Error ? fallbackError.message : String(fallbackError)
      throw new Error(`${original} Browser-level navigation fallback also failed: ${fallback}`)
    }
  }
}

// Testable navigation recovery for the extension relay. The service worker
// supplies Chrome adapters; tests supply deterministic fakes at the same edge.

/** Return whether a Page.navigate request targets the top-level Chrome tab. */
export function canUseBrowserNavigationFallback(method, params, childSessionId) {
  return method === 'Page.navigate' && !childSessionId && params?.frameId == null
}

/**
 * Normalize and auto-prefix URL schemes for browser and debugger navigation.
 * Unprefixed hostnames like "google.com" or "localhost:3000" are automatically
 * resolved with appropriate http/https schemes.
 */
export function normalizeNavigationUrl(rawUrl) {
  let url = typeof rawUrl === 'string' ? rawUrl.trim() : ''
  if (!url) return ''
  if (
    !/^[a-zA-Z][a-zA-Z0-9+.-]*:\/\//.test(url) &&
    !url.startsWith('about:') &&
    !url.startsWith('chrome:') &&
    !url.startsWith('chrome-extension:') &&
    !url.startsWith('data:') &&
    !url.startsWith('javascript:') &&
    !url.startsWith('file:')
  ) {
    if (url.startsWith('localhost') || url.startsWith('127.0.0.1')) {
      url = 'http://' + url
    } else {
      url = 'https://' + url
    }
  }
  return url
}

/**
 * Navigate a top-level relay tab through the browser API first.
 *
 * `chrome.tabs.update` follows Chrome's ordinary tab-navigation path. Besides
 * avoiding renderer-main-thread hangs, this matters for sites that treat a CDP
 * `Page.navigate` differently from a user/browser navigation (issue #211).
 * CDP remains the fallback when the browser API rejects a URL.
 */
export async function navigateTabWithBrowserFallback(params, deps) {
  const url = normalizeNavigationUrl(params?.url)
  if (!url) return await deps.navigateWithDebugger()

  try {
    const tab = await deps.updateTab(url)
    const pendingUrl = typeof tab?.pendingUrl === 'string' ? tab.pendingUrl : ''
    const currentUrl = typeof tab?.url === 'string' ? tab.url : ''
    return {
      frameId: '',
      // A pending URL is a full navigation. Give Rust a synthetic loader id so
      // its normal lifecycle wait still runs. Same-document changes have no
      // pending URL and therefore correctly skip the load-event wait.
      loaderId: pendingUrl ? 'browser-level-navigation' : null,
      relayFallback: {
        method: 'browser-level chrome.tabs.update',
        recovered: false,
        url: pendingUrl || currentUrl || url,
        // When pendingUrl is present, Chrome still reports the previous page's
        // title. Do not stamp that stale title onto the requested destination.
        title: pendingUrl ? '' : typeof tab?.title === 'string' ? tab.title : '',
      },
    }
  } catch (browserError) {
    try {
      return await deps.navigateWithDebugger()
    } catch (debuggerError) {
      const browser = browserError instanceof Error ? browserError.message : String(browserError)
      const debuggerMessage =
        debuggerError instanceof Error ? debuggerError.message : String(debuggerError)
      throw new Error(
        `Browser-level navigation failed: ${browser}. CDP fallback also failed: ${debuggerMessage}`,
      )
    }
  }
}

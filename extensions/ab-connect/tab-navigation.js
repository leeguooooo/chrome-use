// Testable navigation recovery for the extension relay. The service worker
// supplies Chrome adapters; tests supply deterministic fakes at the same edge.

export async function navigateTabWithBrowserFallback(params, deps) {
  const url = typeof params?.url === 'string' ? params.url.trim() : ''
  if (!url) return await deps.navigateWithDebugger()

  try {
    return await deps.navigateWithDebugger()
  } catch (error) {
    if (!deps.isRelayTimeoutError(error)) throw error

    try {
      await deps.updateTab(url)
    } catch (fallbackError) {
      const original = error instanceof Error ? error.message : String(error)
      const fallback =
        fallbackError instanceof Error ? fallbackError.message : String(fallbackError)
      throw new Error(`${original} Browser-level navigation fallback also failed: ${fallback}`)
    }

    return {
      frameId: '',
      loaderId: null,
      relayFallback: 'browser-level chrome.tabs.update',
    }
  }
}

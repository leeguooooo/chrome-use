// Pure reload-loop accounting for the MV3 service worker. Keeping this module
// free of chrome.* globals makes the threshold behavior deterministic in tests.

export const RELOAD_LOOP_WINDOW_MS = 8000
export const RELOAD_LOOP_THRESHOLD = 4

export function newReloadState() {
  return { url: '', commits: [], detectedAt: 0 }
}

/** Carry a stable target's history across a Chrome tab-id replacement. */
export function transferReloadState(states, oldTabId, newTabId) {
  const state = states.get(oldTabId) || states.get(newTabId) || newReloadState()
  states.set(newTabId, state)
  if (oldTabId !== newTabId) states.delete(oldTabId)
  return state
}

/** Record one committed top-frame navigation and return the current diagnosis. */
export function recordNavigationCommit(state, url, now = Date.now()) {
  if (!state || typeof url !== 'string' || !url) return null
  if (state.url !== url) {
    state.url = url
    state.commits = []
    state.detectedAt = 0
  }
  state.commits = state.commits.filter((at) => now - at <= RELOAD_LOOP_WINDOW_MS)
  state.commits.push(now)
  if (state.commits.length > RELOAD_LOOP_THRESHOLD) {
    state.commits = state.commits.slice(-RELOAD_LOOP_THRESHOLD)
  }
  if (state.commits.length >= RELOAD_LOOP_THRESHOLD) state.detectedAt = now
  return activeReloadLoop(state, now)
}

/** Return an active rapid same-URL loop, expiring once the page has stayed put. */
export function activeReloadLoop(state, now = Date.now()) {
  if (!state?.detectedAt || now - state.detectedAt > RELOAD_LOOP_WINDOW_MS) return null
  return {
    url: state.url,
    count: state.commits.length,
    windowMs: RELOAD_LOOP_WINDOW_MS,
  }
}

/** A user-requested navigation is a recovery action and starts a fresh window. */
export function resetReloadLoop(state) {
  if (!state) return
  state.url = ''
  state.commits = []
  state.detectedAt = 0
}

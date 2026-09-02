// Idle auto-detach rules (issue #201), kept free of `chrome.*` globals so the
// service worker's release/replay decisions are unit-testable.

export const IDLE_DETACH_DEFAULT_SECS = 30

/** Parse the options-page setting (seconds) into milliseconds; 0 = never. */
export function idleDetachMsFrom(secs) {
  const n = Number(secs)
  if (!Number.isFinite(n) || n < 0) return IDLE_DETACH_DEFAULT_SECS * 1000
  return Math.round(n * 1000)
}

// CDP methods whose effect must survive an idle detach/re-attach. Everything
// else is a one-shot action (navigate, click, evaluate) or a read.
export const REPLAYABLE_METHOD =
  /^(?:[A-Za-z]+\.enable|Emulation\.set\w+|Network\.set\w+|Page\.setLifecycleEventsEnabled|Page\.setBypassCSP|Page\.setInterceptFileChooserDialog|Page\.addScriptToEvaluateOnNewDocument|Runtime\.addBinding|Security\.setIgnoreCertificateErrors|Target\.setAutoAttach)$/

/**
 * Record a state-setting command on a tab entry's replay map (insertion
 * order = replay order; a repeat of the same method moves to the end with its
 * latest params). `X.disable` forgets `X.enable`.
 */
export function rememberReplayable(replay, method, params) {
  if (!replay) return
  const m = /^([A-Za-z]+)\.disable$/.exec(method)
  if (m) {
    replay.delete(`${m[1]}.enable`)
    return
  }
  if (!REPLAYABLE_METHOD.test(method)) return
  const multi = method === 'Page.addScriptToEvaluateOnNewDocument' || method === 'Runtime.addBinding'
  const key = multi ? `${method} ${JSON.stringify(params ?? null)}` : method
  replay.delete(key)
  replay.set(key, { method, params })
}

/** Tab ids whose attachment should be released now. */
export function selectIdleTabs(entries, now, idleMs) {
  if (!(idleMs > 0)) return []
  const out = []
  for (const [tabId, entry] of entries) {
    if (!entry || entry.attached === false || entry.inflight > 0 || entry.reattaching) continue
    if (now - entry.lastActivity >= idleMs) out.push(tabId)
  }
  return out
}

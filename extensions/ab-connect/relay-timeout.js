export const RELAY_COMMAND_TIMEOUT_MS = 8000
export const RELAY_TIMEOUT_ERROR_NAME = 'RelayTimeoutError'

// A few CDP commands carry a payload the renderer must process character by
// character, so their cost scales with size rather than being a fixed round
// trip. `Input.insertText` of 34 KB into a rich editor (ProseMirror) measured
// ~0.45s/KB — it blew the flat 8s budget at ~16 KB, which made the one-call
// `keyboard inserttext --file` fix useless at exactly the sizes it exists for.
// Budget these by payload instead: a flat budget for a payload-sized command
// is not a health check, it is a size limit in disguise.
//
// 2ms/byte is ~4x the measured worst case, so a healthy renderer never trips
// it; the cap keeps a pathological payload from hanging a session forever.
export const PAYLOAD_MS_PER_BYTE = 2
export const PAYLOAD_MAX_TIMEOUT_MS = 120000

/**
 * Budget for one CDP command. Pure: takes the method and its params, returns
 * milliseconds. Everything without a size-proportional payload keeps the flat
 * budget, so this cannot slow down the failure of an ordinary hung command.
 */
export function relayCommandBudgetMs(method, params) {
  const text = method === 'Input.insertText' ? params?.text : null
  if (typeof text !== 'string' || text.length === 0) return RELAY_COMMAND_TIMEOUT_MS
  const scaled = RELAY_COMMAND_TIMEOUT_MS + text.length * PAYLOAD_MS_PER_BYTE
  return Math.min(scaled, PAYLOAD_MAX_TIMEOUT_MS)
}

// How many past timeouts to keep for `getRelayTimeoutHistory()`. Small on
// purpose: this is a diagnostic tail, not a log.
const MAX_RECORDED_TIMEOUTS = 20

// When THIS service-worker context started. MV3 tears the whole context down
// when the worker is evicted, taking pending timers with it — so a timeout can
// only ever fire in the same context that started the command. Recording the
// context's age at timeout time is what makes the "worker was evicted
// mid-command" hypothesis checkable instead of assumed: an age below the
// elapsed budget would mean the worker restarted inside the window (#193).
const workerStartedAt = Date.now()

// Commands currently racing their timeout, so a timing-out command can report
// how much company it had. Head-of-line blocking (every daemon multiplexes
// through this one extension peer) shows up here as a high in-flight count and
// an `oldest` far past the budget, where a genuinely blocked renderer times out
// alone (#193).
const inFlight = new Map()
let nextCommandId = 1

const recordedTimeouts = []

export function isRelayTimeoutError(error) {
  return error?.name === RELAY_TIMEOUT_ERROR_NAME
}

// The last few timeouts with the context they failed in. Read it from the
// service-worker console (`getRelayTimeoutHistory()` is reachable there via the
// module) when a driver reports repeated timeouts.
export function getRelayTimeoutHistory() {
  return recordedTimeouts.slice()
}

export function relayInFlightCount() {
  return inFlight.size
}

function describeContext(id, now) {
  let oldestStartedAt = now
  for (const entry of inFlight.values()) {
    if (entry.startedAt < oldestStartedAt) oldestStartedAt = entry.startedAt
  }
  const self = inFlight.get(id)
  return {
    elapsedMs: now - (self ? self.startedAt : now),
    inFlight: inFlight.size,
    oldestInFlightMs: now - oldestStartedAt,
    workerAgeMs: now - workerStartedAt,
  }
}

export async function withRelayTimeout(
  operation,
  label,
  timeoutMs = RELAY_COMMAND_TIMEOUT_MS,
  { payloadScaled = false } = {},
) {
  const id = nextCommandId++
  inFlight.set(id, { label, startedAt: Date.now() })
  let timer
  try {
    return await Promise.race([
      Promise.resolve(operation),
      new Promise((_, reject) => {
        timer = setTimeout(
          () => {
            const diag = { label, ...describeContext(id, Date.now()) }
            recordedTimeouts.push(diag)
            if (recordedTimeouts.length > MAX_RECORDED_TIMEOUTS) recordedTimeouts.shift()
            try {
              console.warn('[ab-connect] relay command timed out', diag)
            } catch {}
            // A scaled budget means the command was given time proportional to
            // its payload and still did not finish — that is not the same
            // situation as a flat-budget command going quiet, and telling the
            // caller "the debugger stopped responding" sends them to look for a
            // hung page that isn't there.
            const scaled = payloadScaled
            const error = new Error(
              `relay timeout after ${timeoutMs}ms: ${label}. ` +
                (scaled
                  ? 'That budget already scaled with the payload size, so the page is taking ' +
                    'longer per byte than expected (a heavy rich-text editor, or a busy tab). ' +
                    'Insert less at once, or retry with the tab in the foreground. '
                  : 'The Chrome debugger stopped responding; restart the session and retry. ') +
                `[diag in-flight=${diag.inFlight} oldest-in-flight=${diag.oldestInFlightMs}ms ` +
                `worker-age=${diag.workerAgeMs}ms]`,
            )
            error.name = RELAY_TIMEOUT_ERROR_NAME
            error.relayDiagnostics = diag
            reject(error)
          },
          timeoutMs,
        )
      }),
    ])
  } finally {
    clearTimeout(timer)
    inFlight.delete(id)
  }
}

export const RELAY_COMMAND_TIMEOUT_MS = 8000
export const RELAY_TIMEOUT_ERROR_NAME = 'RelayTimeoutError'

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
            const error = new Error(
              `relay timeout after ${timeoutMs}ms: ${label}. ` +
                'The Chrome debugger stopped responding; restart the session and retry. ' +
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

export const RELAY_COMMAND_TIMEOUT_MS = 8000
export const RELAY_TIMEOUT_ERROR_NAME = 'RelayTimeoutError'

// On macOS at load ~250, a click hit the 8s relay timeout while the next
// get-url command answered in 2s. Give input dispatch room to finish on a busy
// machine. 25s stays below the CLI's 30s per-CDP-call budget, so the relay can
// report its own error first. A timed-out input is not replayed: waiting longer
// does not dispatch it twice.
export const INPUT_COMMAND_TIMEOUT_MS = 25000

const INPUT_COMMANDS = new Set([
  'Input.dispatchMouseEvent',
  'Input.dispatchKeyEvent',
  'Input.dispatchTouchEvent',
  'Input.dispatchDragEvent',
  'Input.emulateTouchFromMouseEvent',
  'Input.synthesizeTapGesture',
  'Input.synthesizeScrollGesture',
  'Input.synthesizePinchGesture',
])

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
//
// The cap has to stay out of the way of that per-byte allowance, and at 120s it
// did not. It began binding at 56 KB ((120000-8000)/2), and above that the
// EFFECTIVE rate collapses: 0.8ms/byte at 150 KB, below the ~1.0ms/byte worst
// case measured on chatgpt.com. So a perfectly healthy renderer was declared
// failed — and because losing a `Promise.race` cancels nothing, the page kept
// working on the insert for minutes afterwards, leaving the next command on
// that tab to collide with a renderer we had already given up on (#315). #309
// measured the same thing from outside: "the practical ceiling for a single
// insert is around 100KB, not the ~265KB the budgets imply".
//
// 300s keeps the full 2ms/byte to 146 KB and stays at or above the measured
// worst case out to ~300 KB, so the budgets now deliver the sizes they imply.
// It only ever applies to a payload-scaled command the caller explicitly sent;
// an ordinary command still fails at the flat 8s.
export const PAYLOAD_MS_PER_BYTE = 2
export const PAYLOAD_MAX_TIMEOUT_MS = 300000

/**
 * Budget for one CDP command. Pure: takes the method and its params, returns
 * milliseconds. Input dispatch gets a longer flat budget; insertText keeps its
 * payload scaling. Everything else keeps the ordinary flat budget.
 */
export function relayCommandBudgetMs(method, params) {
  if (INPUT_COMMANDS.has(method)) return INPUT_COMMAND_TIMEOUT_MS
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
const unresolved = new Map()
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

export function relayUnresolvedOperations(now = Date.now()) {
  let oldest = now
  for (const entry of unresolved.values()) oldest = Math.min(oldest, entry.startedAt)
  return { count: unresolved.size, oldestAgeMs: Math.max(0, now - oldest) }
}

function describeContext(id, now) {
  let oldestStartedAt = now
  for (const entry of inFlight.values()) {
    if (entry.startedAt < oldestStartedAt) oldestStartedAt = entry.startedAt
  }
  const self = inFlight.get(id)
  return {
    unresolvedOperations: relayUnresolvedOperations(now),
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
  unresolved.set(id, { label, startedAt: Date.now() })
  const underlying = Promise.resolve(operation).then(
    value => { unresolved.delete(id); return value },
    error => { unresolved.delete(id); throw error },
  )
  let timer
  try {
    return await Promise.race([
      underlying,
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
                    'The insert was NOT cancelled - nothing can cancel a dispatched CDP ' +
                    'command - so the page may keep working on it for minutes; let the tab go ' +
                    'quiet and re-read the field before sending anything else. '
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

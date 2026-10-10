import { isRelayTimeoutError } from './relay-timeout.js';

const WINDOW_MS = 600000;
const BUCKET_MS = 1000;
// Fixed-size ring: command volume cannot increase retained memory. Counts have
// one-second resolution; the boundary bucket expires at the start of its second.
const buckets = new Array(WINDOW_MS / BUCKET_MS);
// This module's start time resets whenever the MV3 service worker restarts.
const workerStartedAt = Date.now();

function record(method, timedOut, now) {
  const tick = Math.floor(now / BUCKET_MS);
  const index = ((tick % buckets.length) + buckets.length) % buckets.length;
  let bucket = buckets[index];
  if (!bucket || bucket.tick !== tick) {
    bucket = buckets[index] = { tick, answered: 0, timedOut: 0, lastTimeout: null };
  }
  if (timedOut) {
    bucket.timedOut++;
    bucket.lastTimeout = { method, at: now };
  } else {
    bucket.answered++;
  }
}

/** Observe only debugger commands; preserve the caller's result/error identity. */
export async function observeDebuggerCommand(method, promise, now = Date.now) {
  try {
    const result = await promise;
    record(method, false, now());
    return result;
  } catch (error) {
    record(method, isRelayTimeoutError(error), now());
    throw error;
  }
}

/** Passive worker-local observations, with no Chrome calls or timers. */
export function getRelayHealth(now = Date.now()) {
  let answered = 0;
  let timedOut = 0;
  let lastTimeout = null;
  const tick = Math.floor(now / BUCKET_MS);
  for (const bucket of buckets) {
    if (!bucket || bucket.tick <= tick - buckets.length || bucket.tick > tick) continue;
    answered += bucket.answered;
    timedOut += bucket.timedOut;
    if (bucket.lastTimeout && (!lastTimeout || bucket.lastTimeout.at > lastTimeout.at)) {
      lastTimeout = bucket.lastTimeout;
    }
  }
  return {
    windowMs: WINDOW_MS,
    workerAgeMs: Math.max(0, now - workerStartedAt),
    answered,
    timedOut,
    lastTimeout: lastTimeout ? { method: lastTimeout.method, ageMs: Math.max(0, now - lastTimeout.at) } : null,
  };
}

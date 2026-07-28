export const RELAY_COMMAND_TIMEOUT_MS = 8000

export function isRelayTimeoutError(error) {
  return String(error?.message || error).startsWith('relay timeout after ')
}

export async function withRelayTimeout(
  operation,
  label,
  timeoutMs = RELAY_COMMAND_TIMEOUT_MS,
) {
  let timer
  try {
    return await Promise.race([
      Promise.resolve(operation),
      new Promise((_, reject) => {
        timer = setTimeout(
          () =>
            reject(
              new Error(
                `relay timeout after ${timeoutMs}ms: ${label}. ` +
                  'The Chrome debugger stopped responding; restart the session and retry.',
              ),
            ),
          timeoutMs,
        )
      }),
    ])
  } finally {
    clearTimeout(timer)
  }
}

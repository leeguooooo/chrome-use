export const RELAY_COMMAND_TIMEOUT_MS = 8000
export const RELAY_TIMEOUT_ERROR_NAME = 'RelayTimeoutError'

export function isRelayTimeoutError(error) {
  return error?.name === RELAY_TIMEOUT_ERROR_NAME
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
          () => {
            const error = new Error(
              `relay timeout after ${timeoutMs}ms: ${label}. ` +
                'The Chrome debugger stopped responding; restart the session and retry.',
            )
            error.name = RELAY_TIMEOUT_ERROR_NAME
            reject(error)
          },
          timeoutMs,
        )
      }),
    ])
  } finally {
    clearTimeout(timer)
  }
}

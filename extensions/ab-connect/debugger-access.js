// Chrome may reject an entire web tab when it contains another extension's
// protected frame. Reattaching the same tab does not change that access decision.
export function isDebuggerAccessDenied(error) {
  const message = String(error?.message || error)
  return /Cannot access a chrome-extension:\/\/ URL of (?:a )?different extension/i.test(message)
}

export function debuggerAccessError(error) {
  return new Error(
    'debugger_access_denied: Chrome blocked debugger access to protected extension content ' +
    'in this tab (possibly a child frame). The tab may still exist; reattaching does not ' +
    'resolve this restriction. Use `tab inspect <ref>` to check the browser-level state, then ' +
    'use a separate test profile if needed. ' +
    `Original error: ${String(error?.message || error)}`,
  )
}

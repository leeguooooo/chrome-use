// Chrome may reject an entire web tab when it contains another extension's
// protected frame. Reattaching the same tab does not change that access decision.
export function isDebuggerAccessDenied(error) {
  const message = String(error?.message || error)
  return /Cannot access a chrome-extension:\/\/ URL of (?:a )?different extension/i.test(message)
}

// webNavigation.getAllFrames omits another extension's frame, but
// chrome.debugger.getTargets lists it as an `other` target (without a tabId).
// The list is browser-wide: candidates cannot be attributed to the blocked tab.
export function foreignFrameExtensions(targets, ownId) {
  const ids = new Set()
  for (const target of targets || []) {
    if (target?.type !== 'other') continue
    const match = /^chrome-extension:\/\/([a-p]{32})\//.exec(String(target.url || ''))
    if (match && match[1] !== ownId) ids.add(match[1])
  }
  return [...ids]
}

export function debuggerAccessError(error, extensionIds = []) {
  const culprits = extensionIds.length
    ? `Frames of other extensions open in this browser: ${extensionIds
      .map(id => `${id} (chrome://extensions/?id=${id})`).join(', ')}; these are browser-wide candidates, ` +
      'not confirmed blockers of this tab. Check the blocked tab before disabling an extension. '
    : ''
  return new Error(
    'debugger_access_denied: Chrome blocked debugger access to protected extension content ' +
    'in this tab (possibly a child frame). The tab may still exist; reattaching does not ' +
    'resolve this restriction. ' + culprits + 'Use `tab inspect <ref>` to check the browser-level state, then ' +
    'use a separate test profile if needed. ' +
    `Original error: ${String(error?.message || error)}`,
  )
}

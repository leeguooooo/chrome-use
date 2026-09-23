// CDP events the relay does not forward to the host.
//
// Each is high-frequency on a busy page and has no consumer on the CLI side —
// checked by method name against cli/src when this list was introduced (#342).
// Forwarding them serialises every one across the native-messaging pipe for
// nothing. Add a method here only after confirming nothing reads it: an event
// dropped here is invisible to the daemon, with no error to say so.
export const UNFORWARDED_EVENTS = new Set([
  'Network.dataReceived',
  'Network.resourceChangedPriority',
  'Network.requestWillBeSentExtraInfo',
  'Network.responseReceivedExtraInfo',
  'DOM.childNodeCountUpdated',
  'DOM.attributeModified',
  'DOM.characterDataModified',
  'DOM.distributedNodesUpdated',
  'Log.entryAdded',
])

export function shouldForwardEvent(method) {
  return !UNFORWARDED_EVENTS.has(method)
}

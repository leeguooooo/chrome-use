// Which tabs the relay is ACTUALLY driving, as opposed to which one a session
// asked for.
//
// The daemon pins a target and reports that pin as "current". When a command
// then fails, the reader has no way to tell whether the relay is still holding
// that tab — `tabs` shows the request, not the reality, and the two are
// assumed to agree (issue #217). They do agree in the normal case; the point is
// that nothing today can say so.
//
// Kept free of `chrome.*` so the shape is unit-testable.

/**
 * Flatten the live tab map into `{ targetId, tabId, attached }` rows.
 *
 * `attached` follows the same rule the rest of the worker uses: an entry is
 * attached unless it was explicitly released (`attached === false`), because an
 * entry created but not yet confirmed is still ours.
 */
export function attachedTargetsFrom(tabEntries) {
  const out = [];
  for (const [tabId, entry] of tabEntries) {
    if (!entry || !entry.targetId) continue;
    out.push({
      targetId: String(entry.targetId),
      tabId: Number(tabId),
      attached: entry.attached !== false,
    });
  }
  return out;
}

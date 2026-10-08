// ABExt.attachTabById (0.5.30, issue #456): attach exactly one Chrome tab,
// named by its Chrome tab id — never a URL lookup. The daemon uses it to adopt
// a pop-up its own click opened. A URL lookup could attach a different tab
// that happened to show the same URL (the user's), and the attach is already
// announced to every relay client by the time anyone could check.
//
// deps: { getTab(tabId) -> Promise<tab>, eligible(tab) -> bool,
//         attachTab(tabId) -> Promise<{targetId}> }
export async function attachTabById(params, deps) {
  const raw = params?.chromeTabId;
  if (!Number.isInteger(raw) || raw < 0) {
    throw new Error('attachTabById: chromeTabId must be a Chrome tab id');
  }
  const tab = await deps.getTab(raw).catch(() => null);
  if (!tab) throw new Error(`attachTabById: Chrome tab ${raw} does not exist`);
  // Refuse before touching the debugger: a privileged or still-blank page is
  // not attached, and the caller decides whether to wait.
  if (!deps.eligible(tab)) {
    return { attached: false, chromeTabId: raw, url: tab.url || tab.pendingUrl || '' };
  }
  const entry = await deps.attachTab(raw);
  return {
    attached: true,
    chromeTabId: raw,
    targetId: entry.targetId,
    url: tab.url || '',
    title: tab.title || '',
  };
}

// Extension install identity, independent of the storage-local profile UUID.
// No management permission is requested just for diagnostics.
export async function extensionInstallIdentity(chrome, userAgent = globalThis.navigator?.userAgent ?? '') {
  let installType = chrome.runtime.getManifest().update_url ? 'unknown' : 'development';
  try {
    const self = await chrome.management?.getSelf();
    if (self?.installType) installType = self.installType;
  } catch {}
  const browser = /Edg\//.test(userAgent) ? 'edge'
    : /OPR\//.test(userAgent) ? 'opera'
    : /Chrome\//.test(userAgent) ? 'chrome' : null;
  return { extensionId: chrome.runtime.id, installType, browser };
}

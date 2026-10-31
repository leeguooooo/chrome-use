// Window preparation uses browser metadata, never page script or tab activation.
export async function prepareScreenshotWindow(tabId, allowRestore, deps) {
  if (!Number.isInteger(tabId) || tabId < 0) throw new Error('screenshot_window_unverified: no bound tab');
  if (typeof allowRestore !== 'boolean') throw new Error('screenshot_window_unverified: restoration permission must be boolean');
  deps.guard();
  const platform = await deps.getPlatformInfo();
  deps.guard();
  if (!platform || typeof platform.os !== 'string') throw new Error('screenshot_window_unverified: platform is unknown');
  if (platform.os !== 'win') {
    if (allowRestore) throw new Error('screenshot_window_restore_unavailable: Windows relay only');
    return { platform: platform.os, applicable: false, restored: false };
  }
  const tab = await deps.getTab(tabId);
  deps.guard();
  if (tab?.id !== tabId || !Number.isInteger(tab.windowId) || tab.windowId < 0)
    throw new Error('screenshot_window_unverified: bound tab has no known window');
  const windowId = tab.windowId;
  const before = await deps.getWindow(windowId);
  deps.guard();
  const states = ['normal', 'minimized', 'maximized', 'fullscreen', 'locked-fullscreen'];
  if (before?.id !== windowId || !states.includes(before.state))
    throw new Error('screenshot_window_unverified: window state is unknown');
  if (before.state !== 'minimized')
    return { platform: 'win', applicable: true, windowId, before: before.state, state: before.state, restored: false };
  if (!allowRestore)
    throw new Error('screenshot_window_minimized: target window is minimized; no capture was sent. Restore it manually or use screenshot --allow-window-restore when authorized.');
  const current = await deps.getTab(tabId);
  deps.guard();
  if (current?.id !== tabId || current.windowId !== windowId)
    throw new Error('screenshot_window_unverified: target moved before restoration; no window update was sent');
  try {
    await deps.updateWindow(windowId, { state: 'normal', focused: false });
    deps.guard();
    const afterTab = await deps.getTab(tabId);
    deps.guard();
    if (afterTab?.id !== tabId || afterTab.windowId !== windowId) throw new Error('target moved after restoration request');
    const after = await deps.getWindow(windowId);
    deps.guard();
    if (after?.id !== windowId || after.state !== 'normal') throw new Error('normal window state was not observed');
    return { platform: 'win', applicable: true, windowId, before: 'minimized', state: 'normal',
      restored: true, focused: typeof after.focused === 'boolean' ? after.focused : null };
  } catch (error) {
    throw new Error(`action_outcome_unknown: restoration was requested for window ${windowId}; ${String(error?.message || error)}. Inspect the window before repeating the request.`);
  }
}

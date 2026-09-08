// Run from the candidate extension's own popup via `chrome-use eval --file`.
// A real page action succeeds; the adapter then discards its acknowledgement.
// This is fault injection, not evidence of a spontaneous Chrome disconnect.
(async () => {
  const candidates = (await chrome.tabs.query({})).filter(tab => {
    try {
      const url = new URL(tab.url);
      return url.hostname === '127.0.0.1' && url.pathname === '/action-outcome.html';
    } catch { return false; }
  });
  if (candidates.length !== 1) throw new Error('Expected exactly one action-outcome fixture tab');
  const target = { tabId: candidates[0].id };
  const { sendTabCommand } = await import(chrome.runtime.getURL('tab-command.js') + '?probe=' + Date.now());
  let dispatches = 0;
  let recoveryCalls = 0;
  let failure = '';
  try {
    await sendTabCommand(target.tabId, 'Runtime.evaluate', {
      expression: 'document.querySelector("#submit").click()',
    }, undefined, {
      async sendCommand(debuggee, method, params) {
        dispatches++;
        await chrome.debugger.sendCommand(debuggee, method, params);
        throw new Error('Detached while handling command');
      },
      detachTab() { recoveryCalls++; },
      async recoverSessionTab() { recoveryCalls++; return target.tabId; },
    });
  } catch (e) { failure = e.message; }
  const count = await chrome.debugger.sendCommand(target, 'Runtime.evaluate', {
    expression: 'Number(document.querySelector("#count").value)', returnByValue: true,
  });
  const submissions = count.result.value;
  const passed = submissions === 1 && dispatches === 1 && recoveryCalls === 0
    && failure.startsWith('action_outcome_unknown:');
  return { passed, submissions, dispatches, recoveryCalls, failure };
})()

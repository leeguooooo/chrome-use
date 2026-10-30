// Generated CI fixture observer, never loaded by the shipped extension.
const fixtureAttachmentEpochs = new Map();
const fixtureCaptureOperations = new Set();
const fixtureAttachmentHistory = new Map();
const fixtureHistoryLost = new Set();
const fixtureEpoch = tabId => fixtureAttachmentEpochs.get(tabId) || 0;
const fixtureTarget = target => ({ tabId: target?.tabId ?? null, sessionId: target?.sessionId ?? null });
function fixtureOwnPage(tab) {
  try {
    const url = new URL(tab?.url || '');
    return url.origin === __BASE__ && ['/warmup', '/report'].includes(url.pathname) && url.search === '?' + __CASE__;
  } catch { return false; }
}
function fixtureEmit(context, event) {
  return fetch(__SINK__, { method: 'POST', headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ caseId: __CASE__, operationId: context.id, atMs: performance.now(), ...event }),
  }).then(reply => reply.ok).catch(() => false);
}
function fixtureInvalidate(context, reason) {
  if (!context.invalidations.includes(reason)) context.invalidations.push(reason);
}
function fixtureAttachmentChange(target, reason, advance) {
  if (!Number.isInteger(target?.tabId)) return;
  if (advance) fixtureAttachmentEpochs.set(target.tabId, fixtureEpoch(target.tabId) + 1);
  const history = fixtureAttachmentHistory.get(target.tabId) || [];
  history.push({ atMs: performance.now(), reason, epoch: fixtureEpoch(target.tabId) });
  if (history.length > 128) { history.shift(); fixtureHistoryLost.add(target.tabId); }
  fixtureAttachmentHistory.set(target.tabId, history);
  for (const context of fixtureCaptureOperations) {
    if (context.requested.tabId === target.tabId || context.attempts.some(a => a.target.tabId === target.tabId)) {
      fixtureInvalidate(context, reason);
      fixtureEmit(context, { type: 'attachment-change', target: fixtureTarget(target), reason, epoch: fixtureEpoch(target.tabId) });
    }
  }
}
async function fixtureDebuggerAttach(target, version) {
  fixtureAttachmentChange(target, 'attach-attempt', false);
  const result = await chrome.debugger.attach(target, version);
  fixtureAttachmentChange(target, 'attach-acknowledged', true);
  return result;
}
async function fixtureDebuggerDetach(target) {
  fixtureAttachmentChange(target, 'detach-attempt', false);
  const result = await chrome.debugger.detach(target);
  fixtureAttachmentChange(target, 'detach-acknowledged', true);
  return result;
}
chrome.debugger.onDetach.addListener(target => fixtureAttachmentChange(target, 'onDetach', true));
async function fixtureCaptureContext(tabId, childSessionId) {
  const tab = await chrome.tabs.get(tabId).catch(() => null);
  if (!ownedTabs.has(tabId) || !fixtureOwnPage(tab)) return null;
  const context = { id: crypto.randomUUID(), requested: fixtureTarget({ tabId, sessionId: childSessionId }),
    epochAtOperationStart: fixtureEpoch(tabId), startedMs: performance.now(), wrapperPending: true,
    attempts: [], invalidations: [], fixturePath: new URL(tab.url).pathname };
  if (context.epochAtOperationStart === 0) fixtureInvalidate(context, 'no-acknowledged-attachment');
  fixtureCaptureOperations.add(context);
  fixtureEmit(context, { type: 'capture-operation-start', requested: context.requested,
    epoch: context.epochAtOperationStart, fixturePath: context.fixturePath });
  return context;
}
function fixtureObserveWrapper(context, operation) {
  const finish = (type, error) => {
    context.wrapperPending = false;
    fixtureCaptureOperations.delete(context);
    for (const attempt of context.attempts) clearTimeout(attempt.timer);
    fixtureEmit(context, { type, fixturePath: context.fixturePath,
      error: error ? String(error?.message || error) : null,
      epoch: fixtureEpoch(context.requested.tabId), invalidations: context.invalidations,
      attempts: context.attempts.map(a => ({ captureId: a.id, target: a.target, epoch: a.epoch,
        nativePending: a.nativePending, elapsedMs: performance.now() - a.sentMs })) });
  };
  operation.then(() => { finish('capture-wrapper-answer'); void fixturePageAfter(context); },
    error => { finish('capture-wrapper-error', error); void fixturePageAfter(context); });
}
async function fixturePageAfter(context) {
  if (context.fixturePath !== '/report' || context.attempts.length !== 1 || context.invalidations.length) return;
  const attempt = context.attempts[0];
  const tab = await chrome.tabs.get(attempt.target.tabId).catch(()=>null);
  if (!ownedTabs.has(attempt.target.tabId) || !fixtureOwnPage(tab) || fixtureEpoch(attempt.target.tabId)!==attempt.epoch) return;
  let timer;
  try {
    const result = await Promise.race([chrome.debugger.sendCommand({tabId:attempt.target.tabId,
      ...(typeof attempt.target.sessionId==='string'?{sessionId:attempt.target.sessionId}:{})},'Runtime.evaluate',
      {expression:'fixtureSnapshot()',returnByValue:true}),
      new Promise((_,reject)=>{timer=setTimeout(()=>reject(new Error('fixture page-after exceeded 3000ms')),3000);})]);
    await fixtureEmit(context,{type:'page-after-answer',captureId:attempt.id,target:attempt.target,
      epoch:fixtureEpoch(attempt.target.tabId),result:result?.result?.value});
  } catch(error) {await fixtureEmit(context,{type:'page-after-error',captureId:attempt.id,error:String(error?.message||error)});}
  finally {clearTimeout(timer);}
}
async function fixtureReadWhilePending(context, attempt) {
  if (!attempt.nativePending || !context.wrapperPending || context.invalidations.length) return;
  const tab = await chrome.tabs.get(attempt.target.tabId).catch(() => null);
  if (!attempt.nativePending || !context.wrapperPending || context.invalidations.length ||
      !ownedTabs.has(attempt.target.tabId) || !fixtureOwnPage(tab) ||
      fixtureEpoch(attempt.target.tabId) !== attempt.epoch) return;
  const sentMs = performance.now();
  fixtureEmit(context, { type: 'dom-native-send', captureId: attempt.id, target: attempt.target,
    epoch: fixtureEpoch(attempt.target.tabId), sentMs, nativeStillPending: attempt.nativePending });
  let timeout;
  try {
    const result = await Promise.race([
      chrome.debugger.sendCommand({ tabId: attempt.target.tabId, ...(typeof attempt.target.sessionId === 'string' ? { sessionId: attempt.target.sessionId } : {}) }, 'Runtime.evaluate', {
        expression: "({ready:document.readyState,rows:document.querySelectorAll('#rows > .row').length,visibility:document.visibilityState})",
        returnByValue: true,
      }),
      new Promise((_, reject) => { timeout = setTimeout(() => reject(new Error('fixture DOM probe exceeded 3000ms')), 3000); }),
    ]);
    fixtureEmit(context, { type: 'dom-native-answer', captureId: attempt.id, target: attempt.target,
      epoch: fixtureEpoch(attempt.target.tabId), sentMs, nativeStillPending: attempt.nativePending,
      wrapperStillPending: context.wrapperPending, result: result?.result?.value });
  } catch (error) {
    fixtureEmit(context, { type: 'dom-native-error', captureId: attempt.id, target: attempt.target,
      epoch: fixtureEpoch(attempt.target.tabId), sentMs, error: String(error?.message || error) });
  } finally { clearTimeout(timeout); }
}
async function fixtureUploadNative(context, attempt, result) {
  if (context.fixturePath !== '/report') return;
  try {
    if (!await attempt.sendAck || !await attempt.answerAck) throw new Error('native send or answer trace was not acknowledged');
    const tab = await chrome.tabs.get(attempt.target.tabId).catch(() => null);
    if (context.invalidations.length || !ownedTabs.has(attempt.target.tabId) || !fixtureOwnPage(tab) ||
        new URL(tab.url).pathname !== context.fixturePath || fixtureEpoch(attempt.target.tabId) !== attempt.epoch)
      throw new Error('native image ownership or attachment changed');
    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), 5000);
    try {
      const response = await fetch(__IMAGE_SINK__, {method:'POST',signal:controller.signal,
        headers:{'Content-Type':'application/json'},body:JSON.stringify({caseId:__CASE__,operationId:context.id,
          captureId:attempt.id,target:attempt.target,epoch:attempt.epoch,data:result?.data})});
      if (!response.ok) throw new Error('native image sink rejected upload');
      const saved = await response.json();
      if (!saved.saved) throw new Error('native image sink did not acknowledge storage');
      await fixtureEmit(context,{type:'native-image-ack',captureId:attempt.id,target:attempt.target,
        epoch:fixtureEpoch(attempt.target.tabId),saved});
    } finally { clearTimeout(timer); }
  } catch(error) {
    await fixtureEmit(context,{type:'native-image-error',captureId:attempt.id,error:String(error?.message || error)});
  }
}
async function fixtureSendCommand(context, target, command, args) {
  if (!context || command !== 'Page.captureScreenshot') return chrome.debugger.sendCommand(target, command, args);
  const actual = fixtureTarget(target);
  if (actual.tabId !== context.requested.tabId || actual.sessionId !== context.requested.sessionId)
    fixtureInvalidate(context, 'actual-target-changed');
  if (context.attempts.length) fixtureInvalidate(context, 'capture-retried');
  const tab = await chrome.tabs.get(actual.tabId).catch(() => null);
  const eligible = ownedTabs.has(actual.tabId) && fixtureOwnPage(tab);
  if (!eligible) fixtureInvalidate(context, 'actual-target-outside-fixture');
  if (eligible && context.fixturePath === '/report' && __CANDIDATE__ === 'focus') {
    const epochBefore = fixtureEpoch(actual.tabId);
    fixtureEmit(context,{type:'candidate-focus-send',target:actual,epoch:epochBefore});
    let timer;
    try {
      await Promise.race([chrome.debugger.sendCommand(target,'Emulation.setFocusEmulationEnabled',{enabled:true}),
        new Promise((_,reject)=>{timer=setTimeout(()=>reject(new Error('focus control exceeded 2000ms; no capture sent')),2000);})]);
      const ownTab = await chrome.tabs.get(actual.tabId).catch(()=>null);
      if (!ownedTabs.has(actual.tabId) || !fixtureOwnPage(ownTab) || fixtureEpoch(actual.tabId)!==epochBefore || context.invalidations.length)
        throw new Error('focus control attachment changed; no capture sent');
      fixtureEmit(context,{type:'candidate-focus-answer',target:actual,epoch:epochBefore});
    } catch(error) {
      fixtureInvalidate(context,'focus-control-unverified');
      fixtureEmit(context,{type:'candidate-focus-error',error:String(error?.message || error)});
      throw error;
    } finally { clearTimeout(timer); }
  }
  let control = null;
  const controlStartedMs = performance.now();
  if (eligible) {
    const controller = new AbortController();
    const timeout = setTimeout(() => controller.abort(), 1500);
    try {
      const reply = await fetch(__DISPATCH__, { method: 'POST', signal: controller.signal,
        headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ caseId: __CASE__, target: actual }) });
      control = await reply.json();
    } catch (error) { control = { valid: false, error: String(error?.message || error) }; }
    finally { clearTimeout(timeout); }
  }
  if (!control?.valid) fixtureInvalidate(context, 'dispatch-window-state-unverified');
  const epoch = fixtureEpoch(actual.tabId);
  if (epoch === 0 || epoch !== context.epochAtOperationStart) fixtureInvalidate(context, 'attachment-epoch-changed');
  const attempt = { id: crypto.randomUUID(), target: actual, epoch, sentMs: performance.now(), nativePending: true, timer: null };
  context.attempts.push(attempt);
  const effectiveArgs = context.fixturePath === '/report' && __CANDIDATE__ === 'view'
    ? {...args,fromSurface:false} : args;
  let operation;
  try { operation = chrome.debugger.sendCommand(target, command, effectiveArgs); }
  catch (error) { operation = Promise.reject(error); }
  attempt.sendAck = fixtureEmit(context, { type: 'capture-native-send', captureId: attempt.id, target: actual, epoch,
    sentMs: attempt.sentMs, fixturePath: context.fixturePath, windowControl: control,
    controlElapsedMs: attempt.sentMs - controlStartedMs,
    attachmentHistory: fixtureAttachmentHistory.get(actual.tabId) || [], historyLost: fixtureHistoryLost.has(actual.tabId),
    candidate: context.fixturePath === "/report" ? __CANDIDATE__ : "warmup",
    request: { format: effectiveArgs?.format, clip: effectiveArgs?.clip, captureBeyondViewport: effectiveArgs?.captureBeyondViewport, fromSurface: effectiveArgs?.fromSurface } });
  operation.then(async result => {
    attempt.nativePending = false; clearTimeout(attempt.timer);
    const responseTab = await chrome.tabs.get(actual.tabId).catch(()=>null);
    const responseOwned = ownedTabs.has(actual.tabId) && fixtureOwnPage(responseTab) &&
      new URL(responseTab.url).pathname === context.fixturePath;
    if (!responseOwned || context.invalidations.length || fixtureEpoch(actual.tabId) !== attempt.epoch)
      fixtureInvalidate(context,"ownership-or-attachment-changed-at-native-answer");
    attempt.answerAck = fixtureEmit(context, { type: 'capture-native-answer', captureId: attempt.id, target: actual,
      epoch: fixtureEpoch(actual.tabId), responseOwned, encodedBytes: typeof result?.data === 'string' ? result.data.length : null });
    void fixtureUploadNative(context,attempt,result);
  }, error => {
    attempt.nativePending = false; clearTimeout(attempt.timer);
    fixtureEmit(context, { type: 'capture-native-error', captureId: attempt.id, target: actual,
      epoch: fixtureEpoch(actual.tabId), error: String(error?.message || error) });
  });
  if (__DOM_CONTROL__ && !context.invalidations.length) {
    attempt.timer = setTimeout(() => void fixtureReadWhilePending(context, attempt), Math.max(0, 2000 - (performance.now() - attempt.sentMs)));
  }
  return await operation;
}

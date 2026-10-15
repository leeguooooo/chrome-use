// Inspect & Ask picker. Injected into the top frame (isolated world) after
// inspect-extract.js, only when the user presses the shortcut or the popup
// button. It highlights and selects elements, hosts the extension input frame
// inside a closed shadow root, and hands that frame a one-time secret over a
// MessageChannel the page cannot observe. It never sends anything to the
// native host itself; the background and the authenticated frame do.
(() => {
  // Pressing the shortcut again replaces the instance; the old picker leaves.
  globalThis.__chromeUseInspectPicker?.teardown('restarted');
  const X = globalThis.__chromeUseInspectExtract;
  const MAX = 8;
  const DRAG_THRESHOLD = 6;
  const PANEL_WIDTH = 400;
  const Z = '2147483647';

  let instanceId = null;
  let frameOrigin = null;
  let host = null;
  let root = null;
  let box = null;
  let tag = null;
  let marquee = null;
  let frame = null;
  let framePort = null;
  let frameHeight = 320;
  let picking = true;
  let hovered = null;
  let selected = [];
  let badges = [];
  let drag = null;
  let paused = null;
  let observer = null;
  let closed = false;
  let anchorRect = null;
  // Bumped on every selection change; travels with the selection report and
  // each screenshot layout so the background can tell which selection a
  // capture belongs to.
  let selectionSeq = 0;

  // Names the page could see while the picker is open carry no brand and
  // change every session: a random custom-element tag and probe attribute.
  const letters = n => Array.from(crypto.getRandomValues(new Uint8Array(n)), b => String.fromCharCode(97 + (b % 26))).join('');

  const send = message => chrome.runtime.sendMessage({ ...message, instanceId });
  // A content script cannot import i18n.js; chrome.i18n works here directly.
  const t = (key, ...substitutions) => chrome.i18n.getMessage(key, substitutions.map(String)) || key;

  function el(name, style, text) {
    const node = document.createElement(name);
    if (style) node.style.cssText = style;
    if (text) node.textContent = text;
    return node;
  }

  function buildUi() {
    host = document.createElement(`${letters(5)}-${letters(6)}`);
    host.style.cssText = `all:initial;position:fixed;inset:0;pointer-events:none;z-index:${Z};display:block;`;
    root = host.attachShadow({ mode: 'closed' });
    box = el('div', 'position:fixed;z-index:1;pointer-events:none;border:2px solid #2563eb;background:rgba(37,99,235,.08);border-radius:3px;display:none;box-sizing:border-box;');
    tag = el('div', 'position:fixed;z-index:1;pointer-events:none;background:#111827;color:#f9fafb;font:12px/1.4 ui-monospace,SFMono-Regular,Menlo,monospace;padding:3px 7px;border-radius:4px;display:none;max-width:460px;white-space:nowrap;overflow:hidden;text-overflow:ellipsis;');
    marquee = el('div', 'position:fixed;z-index:1;pointer-events:none;border:1px dashed #2563eb;background:rgba(37,99,235,.06);display:none;');
    const hint = el('div', 'position:fixed;left:50%;top:12px;margin-left:-230px;width:460px;text-align:center;pointer-events:none;background:#111827;color:#f9fafb;font:12px/1.5 system-ui,sans-serif;padding:6px 10px;border-radius:6px;opacity:.92;',
      t('inspect_picker_hint'));
    hint.dataset.role = 'hint';
    root.append(box, tag, marquee, hint);
    document.documentElement.append(host);
    observer = new MutationObserver(() => {
      if (!host.isConnected) teardown('host removed');
    });
    observer.observe(document.documentElement, { childList: true });
  }

  function hintVisible(visible) {
    const hint = root?.querySelector('[data-role="hint"]');
    if (hint) hint.style.display = visible ? 'block' : 'none';
  }

  function isOurs(node) {
    return node === host;
  }

  function targetAt(x, y) {
    const node = document.elementFromPoint(x, y);
    if (!node || isOurs(node) || node === document.documentElement || node === document.body) return null;
    return node;
  }

  function sourceText(node) {
    const mark = X.sourceMark(node);
    return mark ? `${mark.path}:${mark.line}` : '';
  }

  function showHover(node) {
    hovered = node;
    if (!node) { box.style.display = 'none'; tag.style.display = 'none'; return; }
    const r = node.getBoundingClientRect();
    Object.assign(box.style, { display: 'block', left: r.left + 'px', top: r.top + 'px', width: r.width + 'px', height: r.height + 'px' });
    const label = node.tagName.toLowerCase() + (sourceText(node) ? '  ' + sourceText(node) : '');
    tag.textContent = label;
    tag.style.display = 'block';
    tag.style.left = Math.max(4, Math.min(r.left, innerWidth - 470)) + 'px';
    tag.style.top = (r.top > 26 ? r.top - 24 : r.bottom + 4) + 'px';
  }

  function drawBadges() {
    for (const badge of badges) badge.remove();
    badges = selected.map((node, i) => {
      const r = node.getBoundingClientRect();
      const outline = el('div', `position:fixed;z-index:1;pointer-events:none;border:2px solid #16a34a;border-radius:3px;box-sizing:border-box;left:${r.left}px;top:${r.top}px;width:${r.width}px;height:${r.height}px;`);
      const num = el('div', 'position:absolute;left:-2px;top:-20px;background:#16a34a;color:#fff;font:bold 11px/18px system-ui,sans-serif;padding:0 6px;border-radius:9px;', String(i + 1));
      outline.append(num);
      root.append(outline);
      return outline;
    });
  }

  function layoutNow() {
    return { ...X.layout(window), selectionSeq, rects: selected.map(node => { const r = node.getBoundingClientRect(); return { x: r.left, y: r.top, width: r.width, height: r.height }; }) };
  }

  async function report() {
    const attribute = 'data-' + letters(12);
    selected.forEach((node, i) => node.setAttribute(attribute, String(i)));
    try {
      const revision = document.querySelector('meta[name="chrome-use-revision"]')?.getAttribute('content') || null;
      await send({ type: 'inspect:selection', selectionSeq, elements: selected.map(node => X.describe(node)), probeAttribute: attribute, layout: layoutNow(), revision });
    } finally {
      for (const node of selected) node.removeAttribute(attribute);
    }
  }

  function setSelection(nodes, { add }) {
    let next = add ? [...selected] : [];
    for (const node of nodes) {
      const at = next.indexOf(node);
      if (add && at >= 0) next.splice(at, 1);
      else if (at < 0) next.push(node);
    }
    next = next.filter(node => node.isConnected).slice(0, MAX);
    selected = next;
    selectionSeq++;
    drawBadges();
    if (selected.length) openPanel();
    void report().catch(() => {});
  }

  function areaSelect(rect) {
    const inside = [];
    for (const node of document.body.querySelectorAll('*')) {
      if (isOurs(node)) continue;
      const r = node.getBoundingClientRect();
      if (r.width < 4 || r.height < 4) continue;
      if (r.left >= rect.left && r.top >= rect.top && r.right <= rect.right && r.bottom <= rect.bottom) inside.push(node);
      if (inside.length > 400) break;
    }
    // Keep the outermost element of each nested group, preferring marked ones.
    const outer = inside.filter(node => !inside.some(other => other !== node && other.contains(node)));
    return outer.slice(0, MAX);
  }

  // Pause records each animation's and media element's prior state so resume
  // restores exactly what the page had (an already-paused video stays paused).
  function setPaused(on) {
    if (on && !paused) {
      const animations = (document.getAnimations?.() || []).filter(a => a.playState === 'running');
      for (const animation of animations) { try { animation.pause(); } catch {} }
      const media = [...document.querySelectorAll('video,audio')].filter(m => !m.paused);
      for (const m of media) { try { m.pause(); } catch {} }
      const style = el('style', '', '*,*::before,*::after{transition-duration:0s!important;transition-delay:0s!important;caret-color:transparent!important}');
      document.documentElement.append(style);
      paused = { animations, media, style };
    } else if (!on && paused) {
      for (const animation of paused.animations) { try { if (animation.playState === 'paused') animation.play(); } catch {} }
      for (const m of paused.media) { try { void m.play().catch(() => {}); } catch {} }
      paused.style.remove();
      paused = null;
    }
    framePort?.postMessage({ type: 'paused', paused: !!paused });
  }

  function positionPanel() {
    if (!frame) return;
    const r = anchorRect || { left: innerWidth - PANEL_WIDTH - 16, right: innerWidth - 16, top: 16, bottom: 16 };
    const height = Math.min(frameHeight, innerHeight - 24);
    let left = r.right + 12;
    if (left + PANEL_WIDTH > innerWidth - 8) left = r.left - PANEL_WIDTH - 12;
    if (left < 8) left = Math.max(8, innerWidth - PANEL_WIDTH - 12);
    let top = Math.max(12, Math.min(r.top, innerHeight - height - 12));
    Object.assign(frame.style, { left: left + 'px', top: top + 'px', height: height + 'px' });
  }

  function openPanel() {
    anchorRect = selected[0]?.getBoundingClientRect() || anchorRect;
    if (frame) { positionPanel(); return; }
    frame = document.createElement('iframe');
    frame.setAttribute('allow', 'clipboard-write');
    // z-index 2: the picker's own outlines, badges and hover box (z-index 1)
    // must never cover the panel, or the visibility check counts them as an
    // occlusion (a full-width selected block would overlap it).
    frame.style.cssText = `position:fixed;z-index:2;width:${PANEL_WIDTH}px;height:${frameHeight}px;border:0;border-radius:10px;box-shadow:0 10px 40px rgba(0,0,0,.28);pointer-events:auto;background:transparent;color-scheme:normal;`;
    frame.addEventListener('load', handshake, { once: true });
    frame.src = frameUrl;
    root.append(frame);
    positionPanel();
  }

  let frameUrl = null;
  let secret = null;
  function handshake() {
    if (!frame?.contentWindow || !secret) return;
    const channel = new MessageChannel();
    framePort = channel.port1;
    framePort.onmessage = event => onFrameMessage(event.data);
    frame.contentWindow.postMessage({ type: 'init' }, frameOrigin, [channel.port2]);
    framePort.postMessage({ type: 'secret', secret });
    secret = null;
  }

  // A fresh IntersectionObserver per submit: no cached visibility (PoC verified
  // that static frames still report visible after 10 s and covered or moved
  // frames report not visible).
  function checkVisibility(nonce) {
    if (!frame) return framePort?.postMessage({ type: 'visibility', nonce, timeout: true });
    let done = false;
    let io;
    try {
      io = new IntersectionObserver(entries => {
        if (done) return;
        done = true;
        io.disconnect();
        const entry = entries[entries.length - 1];
        framePort?.postMessage({ type: 'visibility', nonce, isVisible: entry.isVisible, ratio: entry.intersectionRatio, documentVisibility: document.visibilityState });
      }, { threshold: [1], trackVisibility: true, delay: 100 });
      io.observe(frame);
    } catch {
      framePort?.postMessage({ type: 'visibility', nonce, isVisible: undefined });
      return;
    }
    setTimeout(() => {
      if (done) return;
      done = true;
      io.disconnect();
      framePort?.postMessage({ type: 'visibility', nonce, timeout: true });
    }, 1000);
  }

  function onFrameMessage(message) {
    switch (message?.type) {
      case 'height':
        frameHeight = Math.max(160, Math.min(Number(message.height) || 320, 720));
        positionPanel();
        break;
      case 'visibility':
        checkVisibility(message.nonce);
        break;
      case 'pick':
        picking = true;
        hintVisible(true);
        break;
      case 'pause':
        setPaused(!!message.on);
        break;
      case 'remove': {
        const index = Number(message.index);
        if (Number.isInteger(index) && selected[index]) setSelection([selected[index]], { add: true });
        break;
      }
      case 'clear':
        setSelection([], { add: false });
        break;
      case 'close':
        teardown('closed');
        break;
    }
  }

  function onMove(event) {
    if (closed) return;
    if (drag) {
      const dx = event.clientX - drag.x;
      const dy = event.clientY - drag.y;
      if (!drag.active && Math.hypot(dx, dy) > DRAG_THRESHOLD) drag.active = true;
      if (drag.active) {
        Object.assign(marquee.style, { display: 'block', left: Math.min(drag.x, event.clientX) + 'px', top: Math.min(drag.y, event.clientY) + 'px', width: Math.abs(dx) + 'px', height: Math.abs(dy) + 'px' });
        showHover(null);
      }
      return;
    }
    if (!picking) return;
    showHover(targetAt(event.clientX, event.clientY));
  }

  function swallow(event) {
    event.preventDefault();
    event.stopPropagation();
    event.stopImmediatePropagation();
  }

  // A gesture the picker consumed must not reach the page in any form: the
  // pointer events, the compatibility mouse events, and the click, auxclick or
  // dblclick that follow mouseup. `consumed` stays set until those trailing
  // events have passed, independent of whether picking has ended.
  let consumed = null;
  const releaseConsumed = () => { consumed = null; };

  function onDown(event) {
    if (closed || !event.isTrusted || event.composedPath().includes(host)) return;
    if (!picking) return;
    swallow(event);
    if (event.type === 'pointerdown') {
      clearTimeout(consumed?.timer);
      consumed = { timer: null };
      if (event.button === 0) drag = { x: event.clientX, y: event.clientY, active: false, shift: event.shiftKey };
    }
  }

  function onUp(event) {
    if (closed || event.composedPath().includes(host)) return;
    if (!consumed && !drag) return;
    swallow(event);
    if (event.type !== 'pointerup') return;
    // The click (and a possible dblclick) arrive after pointerup; keep
    // swallowing them for a short while even though picking may end now.
    if (consumed) { clearTimeout(consumed.timer); consumed.timer = setTimeout(releaseConsumed, 600); }
    if (!drag) return;
    const current = drag;
    drag = null;
    marquee.style.display = 'none';
    if (current.active) {
      const rect = { left: Math.min(current.x, event.clientX), top: Math.min(current.y, event.clientY), right: Math.max(current.x, event.clientX), bottom: Math.max(current.y, event.clientY) };
      const nodes = areaSelect(rect);
      if (nodes.length) setSelection(nodes, { add: current.shift || !!frame });
    } else {
      const node = hovered || targetAt(event.clientX, event.clientY);
      if (node) setSelection([node], { add: current.shift || !!frame });
    }
    if (!current.shift) {
      picking = false;
      hintVisible(false);
      showHover(null);
    }
  }

  function onClick(event) {
    if (closed || event.composedPath().includes(host)) return;
    if (consumed || picking || drag) swallow(event);
  }

  function onKey(event) {
    if (closed || !event.isTrusted || event.composedPath().includes(host)) return;
    if (event.key === 'Escape') {
      swallow(event);
      if (picking && selected.length) { picking = false; hintVisible(false); showHover(null); return; }
      teardown('escape');
      return;
    }
    if (!picking) return;
    if (event.key === 'ArrowUp' && hovered?.parentElement && hovered.parentElement !== document.body) { swallow(event); showHover(hovered.parentElement); }
    else if (event.key === 'ArrowDown' && hovered?.firstElementChild) { swallow(event); showHover(hovered.firstElementChild); }
    else if ((event.key === 'p' || event.key === 'P') && !event.metaKey && !event.ctrlKey && !event.altKey) { swallow(event); setPaused(!paused); }
    else if (event.key === 'Enter' && hovered) { swallow(event); setSelection([hovered], { add: event.shiftKey || !!frame }); }
  }

  function onScroll() {
    if (closed) return;
    if (hovered) showHover(hovered);
    if (selected.length) drawBadges();
  }

  const listeners = [
    ['pointermove', onMove], ['pointerdown', onDown], ['mousedown', onDown], ['pointerup', onUp], ['mouseup', onUp],
    ['click', onClick], ['auxclick', onClick], ['dblclick', onClick], ['contextmenu', onClick],
    ['keydown', onKey], ['scroll', onScroll], ['resize', onScroll],
  ];

  function teardown(reason) {
    if (closed) return;
    closed = true;
    for (const [name, fn] of listeners) window.removeEventListener(name, fn, true);
    try { setPaused(false); } catch {}
    observer?.disconnect();
    try { framePort?.postMessage({ type: 'closing', reason }); framePort?.close(); } catch {}
    host?.remove();
    chrome.runtime.onMessage.removeListener(onBackground);
    if (instanceId) void send({ type: 'inspect:closed' }).catch(() => {});
    if (globalThis.__chromeUseInspectPicker === self) delete globalThis.__chromeUseInspectPicker;
  }

  // Background asks the picker to hide its UI around a screenshot.
  function onBackground(message, sender, sendResponse) {
    if (sender.id !== chrome.runtime.id || sender.tab || message?.instanceId !== instanceId) return false;
    if (message.type === 'inspect:prepareCapture') {
      host.style.visibility = 'hidden';
      // Two frames so the hidden overlay is off screen; a timer covers pages
      // that do not run animation frames.
      let answered = false;
      const answer = () => { if (answered) return; answered = true; sendResponse({ ok: true, layout: layoutNow() }); };
      requestAnimationFrame(() => requestAnimationFrame(answer));
      setTimeout(answer, 250);
      return true;
    }
    if (message.type === 'inspect:finishCapture') {
      const layout = layoutNow();
      host.style.visibility = 'visible';
      sendResponse({ ok: true, layout });
      return false;
    }
    return false;
  }

  async function begin() {
    const hello = await chrome.runtime.sendMessage({ type: 'inspect:hello' });
    if (!hello?.ok) return;
    instanceId = hello.instanceId;
    secret = hello.secret;
    frameUrl = hello.frameUrl;
    frameOrigin = hello.frameOrigin;
    buildUi();
    chrome.runtime.onMessage.addListener(onBackground);
    for (const [name, fn] of listeners) window.addEventListener(name, fn, true);
    if (!hello.enabled) openPanel();
  }

  const self = { teardown };
  globalThis.__chromeUseInspectPicker = self;

  void begin().catch(() => teardown('failed'));
})();

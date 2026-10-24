// Explicit compatibility/privacy options only. Default launches inject no script.
(function(){
  // Off unless AGENT_BROWSER_DISABLE_DEVTOOL_SHIM=1: wrapping querySelector is
  // itself visible to a page that checks its natives (botcheck, CreepJS), so
  // stealth mode leaves them native and the shim is a compatibility opt-in
  // for sites that use the disable-devtool library.
  if (!__abStealth.disableDevtoolShim) return;
  if (typeof Document === 'undefined') return;
  const AUTO_SELECTOR = '[disable-devtool-auto]';
  const NEVER_MATCH_SELECTOR = 'script[__ab_disable_devtool_never_match__="1"]';
  const normalize = (value) => {
    if (typeof value !== 'string') return '';
    return value.replace(/\s+/g, '').toLowerCase();
  };
  const shouldHideSelector = (selector) => normalize(selector) === AUTO_SELECTOR;

  const patchQueryMethod = (proto, method) => {
    if (!proto) return;
    const native = proto[method];
    if (typeof native !== 'function') return;

    const wrapped = function(selector, ...args) {
      if (shouldHideSelector(selector)) {
        return native.call(this, NEVER_MATCH_SELECTOR, ...args);
      }
      return native.call(this, selector, ...args);
    };
    try {
      Object.defineProperty(wrapped, 'name', {
        value: native.name,
        configurable: true,
      });
      Object.defineProperty(wrapped, 'toString', {
        value: () => native.toString(),
        configurable: true,
      });
    } catch {}

    try {
      Object.defineProperty(proto, method, {
        value: wrapped,
        configurable: true,
        writable: true,
      });
    } catch {}
  };

  patchQueryMethod(Document.prototype, 'querySelector');
  patchQueryMethod(Document.prototype, 'querySelectorAll');
  if (typeof Element !== 'undefined') {
    patchQueryMethod(Element.prototype, 'querySelector');
    patchQueryMethod(Element.prototype, 'querySelectorAll');
  }
})();
// Canvas + audio fingerprint noise (OPT-IN, full-launch only).
// Headless Chrome produces a stable canvas/audio hash that trackers use as a
// device id. When __abStealth.hideCanvas is on we perturb readback APIs with a
// SESSION-STABLE, sub-perceptual amount of noise: repeated reads on this page
// return the same noised result (a real device is consistent too), but the
// hash differs from the headless default. Off by default — noise is itself a
// "lie", so it's reserved for users who explicitly enable it.
(function(){
  if (!__abStealth || __abStealth.hideCanvas !== true) return;

  // Deterministic PRNG keyed by the per-session seed plus a position, so the
  // same pixel/sample is perturbed identically every read within the session.
  const baseSeed = (__abStealth.canvasSeed >>> 0) || 0x9e3779b9;
  const noiseAt = (n) => {
    let t = (baseSeed ^ Math.imul(n | 0, 0x6d2b79f5)) >>> 0;
    t = Math.imul(t ^ (t >>> 15), t | 1) >>> 0;
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };

  // Make a wrapped function masquerade as the native one (toString + name).
  const mask = (wrapped, native) => {
    try {
      Object.defineProperty(wrapped, 'name', {
        value: native.name,
        configurable: true,
      });
      Object.defineProperty(wrapped, 'toString', {
        value: () => native.toString(),
        configurable: true,
        writable: true,
      });
    } catch {}
    return wrapped;
  };

  // ---- Canvas 2D readback ---------------------------------------------------
  const perturbImageData = (imageData) => {
    const data = imageData && imageData.data;
    if (!data || !data.length) return imageData;
    for (let i = 0; i < data.length; i += 4) {
      // Touch ~5% of pixels by +/-1 on each RGB channel; leave alpha alone.
      if (noiseAt(i) < 0.05) {
        const delta = noiseAt(i + 1) < 0.5 ? -1 : 1;
        data[i] = Math.max(0, Math.min(255, data[i] + delta));
        data[i + 1] = Math.max(0, Math.min(255, data[i + 1] + delta));
        data[i + 2] = Math.max(0, Math.min(255, data[i + 2] + delta));
      }
    }
    return imageData;
  };

  try {
    const ctxProto = (typeof CanvasRenderingContext2D !== 'undefined')
      ? CanvasRenderingContext2D.prototype : null;
    if (ctxProto && typeof ctxProto.getImageData === 'function') {
      const nativeGetImageData = ctxProto.getImageData;
      ctxProto.getImageData = mask(function(...args) {
        return perturbImageData(nativeGetImageData.apply(this, args));
      }, nativeGetImageData);
    }
  } catch {}

  // For toDataURL/toBlob, draw the (already-rendered) canvas onto a scratch
  // canvas, perturb its pixels, then encode that — so the export hash shifts
  // without disturbing what the page sees on screen.
  const exportNoised = (canvas) => {
    try {
      const w = canvas.width, h = canvas.height;
      if (!w || !h) return null;
      const scratch = document.createElement('canvas');
      scratch.width = w; scratch.height = h;
      const sctx = scratch.getContext('2d');
      if (!sctx) return null;
      sctx.drawImage(canvas, 0, 0);
      const img = sctx.getImageData(0, 0, w, h);
      perturbImageData(img);
      sctx.putImageData(img, 0, 0);
      return scratch;
    } catch { return null; }
  };

  try {
    const canvasProto = (typeof HTMLCanvasElement !== 'undefined')
      ? HTMLCanvasElement.prototype : null;
    if (canvasProto && typeof canvasProto.toDataURL === 'function') {
      const nativeToDataURL = canvasProto.toDataURL;
      canvasProto.toDataURL = mask(function(...args) {
        const scratch = exportNoised(this);
        return nativeToDataURL.apply(scratch || this, args);
      }, nativeToDataURL);
    }
    if (canvasProto && typeof canvasProto.toBlob === 'function') {
      const nativeToBlob = canvasProto.toBlob;
      canvasProto.toBlob = mask(function(cb, ...rest) {
        const scratch = exportNoised(this);
        return nativeToBlob.call(scratch || this, cb, ...rest);
      }, nativeToBlob);
    }
  } catch {}

  // ---- AudioBuffer readback -------------------------------------------------
  // Perturb time-domain samples by a tiny, seed-stable amount so the audio
  // fingerprint (sum/hash of channel data) shifts without audible effect.
  try {
    const audioProto = (typeof AudioBuffer !== 'undefined') ? AudioBuffer.prototype : null;
    if (audioProto && typeof audioProto.getChannelData === 'function') {
      const nativeGetChannelData = audioProto.getChannelData;
      const seen = new WeakSet();
      audioProto.getChannelData = mask(function(...args) {
        const channel = nativeGetChannelData.apply(this, args);
        // Only perturb once per buffer to keep reads consistent.
        if (channel && !seen.has(channel)) {
          seen.add(channel);
          for (let i = 0; i < channel.length; i += 100) {
            channel[i] = channel[i] + (noiseAt(i) - 0.5) * 1e-7;
          }
        }
        return channel;
      }, nativeGetChannelData);
    }
  } catch {}
})();

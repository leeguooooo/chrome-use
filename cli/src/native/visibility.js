function (mode) {
  // Text a person cannot see (prompt-injection defense, issue #781).
  //
  // Runs in chrome-use's isolated world, as a read: no attribute, style,
  // node or global is written, nothing is cloned (cloning constructs the
  // page's custom elements), and no request is made. `this` is the scan
  // root. `mode` is "ax" (snapshot), "text" (get text) or "html" (read).
  //
  // Returns the top-most hidden nodes in document order. A node whose own
  // box is hidden (opacity 0, off the page, clipped away, a 1px sr-only box)
  // is returned once with nothing under it. Text-level hiding (font-size 0,
  // transparent or same-as-background colour, text-indent far left) returns
  // the element's own text nodes, since a child element can undo it.
  //
  // display:none and visibility:hidden text never reaches the AX tree or
  // innerText, so "ax" and "text" skip it without counting it; "html" reads
  // outerHTML, which keeps it, so there it is returned too.
  var root = this;
  var html = mode === 'html';
  var SKIP = { SCRIPT: 1, STYLE: 1, NOSCRIPT: 1, TEMPLATE: 1, HEAD: 1, OPTION: 1, OPTGROUP: 1, TITLE: 1, META: 1, LINK: 1 };
  var MEDIA = 'img,picture,video,canvas,svg,iframe,object,embed';
  var se = document.scrollingElement || document.documentElement;
  var sx = window.scrollX || 0, sy = window.scrollY || 0;
  var vw = window.innerWidth || 0, vh = window.innerHeight || 0;
  var docW = Math.max(se ? se.scrollWidth : 0, vw);
  var docH = Math.max(se ? se.scrollHeight : 0, vh);
  var out = [], why = [];
  var budget = 60000, truncated = false;
  var bgMemo = new Map(), mediaMemo = new Map();

  function rgba(c) {
    var m = /^rgba?\(([^)]+)\)$/.exec(c || '');
    if (!m) return null;
    var p = m[1].split(/[\s,\/]+/).filter(Boolean).map(parseFloat);
    if (p.length < 3 || p.some(isNaN)) return null;
    return [p[0], p[1], p[2], p.length > 3 ? p[3] : 1];
  }
  // The opaque colour painted behind `el`, from the element itself or the
  // nearest ancestor that sets one, or null when it is not knowable cheaply:
  // a background image, a translucent layer, media that may sit behind
  // positioned text, or no explicit colour at all (the canvas default is
  // left alone so a hero's white text over an <img> is never dropped).
  function background(el) {
    var steps = 0;
    for (var n = el; n && n.nodeType === 1 && steps < 6; n = n.parentElement, steps++) {
      if (bgMemo.has(n)) return bgMemo.get(n);
      var cs = getComputedStyle(n);
      if (cs.backgroundImage && cs.backgroundImage !== 'none') { bgMemo.set(n, null); return null; }
      var c = rgba(cs.backgroundColor);
      if (c && c[3] > 0.01) {
        var v = null;
        if (c[3] >= 0.99) {
          if (!mediaMemo.has(n)) mediaMemo.set(n, !!n.querySelector(MEDIA));
          v = mediaMemo.get(n) ? null : c;
        }
        bgMemo.set(n, v);
        return v;
      }
      if (cs.position === 'absolute' || cs.position === 'fixed') return null;
    }
    return null;
  }
  function textHidden(el, cs) {
    if (parseFloat(cs.fontSize) < 2) return 'font-size';
    if (parseFloat(cs.textIndent) <= -999) return 'text-indent';
    // A shadow or stroke draws the glyphs whatever their fill colour, and
    // `background-clip: text` paints them with the background (gradient
    // headlines are `color: transparent`).
    if ((cs.textShadow && cs.textShadow !== 'none') || parseFloat(cs.webkitTextStrokeWidth) > 0) return null;
    if (cs.webkitBackgroundClip === 'text' || cs.backgroundClip === 'text') return null;
    var c = rgba(cs.color);
    if (c && c[3] <= 0.01) return 'transparent';
    if (c) {
      var bg = background(el);
      if (bg && Math.abs(bg[0] - c[0]) + Math.abs(bg[1] - c[1]) + Math.abs(bg[2] - c[2]) <= 12) return 'same-colour';
    }
    return null;
  }
  function clipped(v) { return v === 'hidden' || v === 'clip'; }
  // An element fading in (scroll-reveal, entrance animations) is about to
  // be seen: opacity 0 with an opacity transition or a running animation.
  function fading(cs) {
    if (cs.animationName && cs.animationName !== 'none') return true;
    var props = (cs.transitionProperty || '').split(',');
    var times = (cs.transitionDuration || '').split(',');
    for (var i = 0; i < props.length; i++) {
      var prop = props[i].trim();
      if ((prop === 'opacity' || prop === 'all') && parseFloat(times[i % times.length]) > 0) return true;
    }
    return false;
  }
  function boxHidden(el, cs, r, clip) {
    if (parseFloat(cs.opacity) <= 0.01 && !fading(cs)) return 'opacity';
    if (/^matrix\(0, 0, 0, 0,|^matrix3d\(0, 0, 0, 0,/.test(cs.transform || '')) return 'scale-0';
    var cp = cs.clipPath || '';
    if (/inset\(\s*(50|100)%/.test(cp) || /circle\(\s*0(px)?\s*(at|\))/.test(cp)) return 'clip-path';
    var tiny = r.width <= 1 || r.height <= 1;
    if (cs.clip && cs.clip !== 'auto' && /^rect\(/.test(cs.clip)) {
      var q = cs.clip.match(/-?[\d.]+/g) || [];
      if (tiny || (q.length === 4 && (+q[2] <= +q[0] || +q[1] <= +q[3]))) return 'clip';
    }
    if (tiny && (clipped(cs.overflowX) || clipped(cs.overflowY))) return 'zero-size';
    if (r.width === 0 && r.height === 0) return null;
    var fixed = cs.position === 'fixed';
    var L = r.left + (fixed ? 0 : sx), T = r.top + (fixed ? 0 : sy);
    var R = r.right + (fixed ? 0 : sx), B = r.bottom + (fixed ? 0 : sy);
    var W = fixed ? vw : docW, H = fixed ? vh : docH;
    // A box with no height (or width) that only touches an edge is not past
    // it: a portal wrapper whose children are all fixed has a 0px box at
    // top 0 and draws a full-screen modal (#838).
    function outside(x0, y0, x1, y1) {
      return (r.width > 0 ? R <= x0 : R < x0) || (r.height > 0 ? B <= y0 : B < y0) || L >= x1 || T >= y1;
    }
    if (outside(0, 0, W, H)) return 'off-screen';
    if (clip && cs.position !== 'absolute' && !fixed && outside(clip[0], clip[1], clip[2], clip[3])) return 'clipped';
    return null;
  }
  function hasText(n) { return /\S/.test(n.textContent || ''); }
  function push(n, reason) { out.push(n); why.push(reason); }

  function walk(el, clip) {
    if (truncated) return;
    if (--budget < 0) { truncated = true; return; }
    if (SKIP[el.tagName]) return;
    var cs = getComputedStyle(el);
    var contents = cs.display === 'contents';
    if (cs.display === 'none' || (!contents && el.checkVisibility && !el.checkVisibility())) {
      if (html && hasText(el)) push(el, 'display');
      return;
    }
    var r = el.getBoundingClientRect();
    var reason = contents ? null : boxHidden(el, cs, r, clip);
    if (reason) {
      if (hasText(el) || (mode === 'ax' && el.hasAttribute('aria-label'))) push(el, reason);
      return;
    }
    var own = (cs.visibility === 'hidden' || cs.visibility === 'collapse') ? (html ? 'visibility' : 'skip') : null;
    var kids = el.childNodes;
    var textReason = null, textChecked = false;
    for (var i = 0; i < kids.length; i++) {
      var k = kids[i];
      if (k.nodeType !== 3 || !/\S/.test(k.nodeValue || '')) continue;
      if (!textChecked) { textChecked = true; textReason = own || textHidden(el, cs); }
      if (textReason && textReason !== 'skip') push(k, textReason);
    }
    // A fixed box is not clipped by its ancestors' overflow (an Ant Design
    // modal portalled under a `body` that opening it set to overflow-y:
    // hidden, #838), so what is under it starts unclipped too.
    var next = cs.position === 'fixed' ? null : clip;
    if (clipped(cs.overflowX) || clipped(cs.overflowY)) {
      var L = r.left + sx, T = r.top + sy, R = r.right + sx, B = r.bottom + sy;
      if (!clipped(cs.overflowX)) { L = -Infinity; R = Infinity; }
      if (!clipped(cs.overflowY)) { T = -Infinity; B = Infinity; }
      next = clip ? [Math.max(clip[0], L), Math.max(clip[1], T), Math.min(clip[2], R), Math.min(clip[3], B)] : [L, T, R, B];
    }
    for (var c = el.firstElementChild; c; c = c.nextElementSibling) walk(c, next);
    // Open shadow roots reach the AX tree but not innerText/outerHTML.
    if (mode === 'ax' && el.shadowRoot) {
      for (var s = el.shadowRoot.firstElementChild; s; s = s.nextElementSibling) walk(s, next);
    }
  }

  var start = root && root.nodeType === 9 ? (root.body || root.documentElement) : root;
  if (start && start.nodeType === 1) {
    if (start.tagName === 'HTML') {
      for (var b = start.firstElementChild; b; b = b.nextElementSibling) walk(b, null);
    } else {
      walk(start, null);
    }
  }

  function textOf(n) {
    var t = n.nodeType === 3 ? (n.nodeValue || '').replace(/\s+/g, ' ') : (n.innerText || '');
    return t.trim();
  }
  // Cut each hidden node's text out of `full`, in document order, searching
  // forward from the last cut so an identical visible string earlier on the
  // page is never the one removed.
  function cut(full, pieces) {
    var parts = [], at = 0, dropped = 0, chars = 0, unmatched = 0;
    for (var i = 0; i < pieces.length; i++) {
      var p = pieces[i];
      if (!p) continue;
      var found = full.indexOf(p, at);
      if (found < 0) { unmatched++; continue; }
      parts.push(full.slice(at, found));
      at = found + p.length; dropped++; chars += p.length;
    }
    parts.push(full.slice(at));
    return { value: parts.join(''), dropped: dropped, chars: chars, unmatched: unmatched };
  }
  function escapeText(s) {
    return s.replace(/&/g, '&amp;').replace(/ /g, '&nbsp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
  }

  if (mode === 'text') {
    var full = start ? (start.innerText || start.textContent || '') : '';
    var res = cut(full, out.map(textOf));
    return { text: res.value.replace(/\n{3,}/g, '\n\n'), dropped: res.dropped, chars: res.chars, unmatched: res.unmatched, truncated: truncated };
  }
  if (mode === 'html') {
    var page = document.documentElement ? document.documentElement.outerHTML : '';
    var hres = cut(page, out.map(function (n) {
      return n.nodeType === 3 ? escapeText(n.nodeValue || '') : (n.outerHTML || '');
    }));
    return { html: hres.value, dropped: hres.dropped, chars: hres.chars, unmatched: hres.unmatched, truncated: truncated };
  }
  return { items: why.map(function (w, i) { return { why: w, chars: textOf(out[i]).length }; }), nodes: out, truncated: truncated };
}

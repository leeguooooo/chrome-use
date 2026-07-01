/* ==========================================================================
   chrome-use docs — docs.js  (vanilla JS, zero deps)
   --------------------------------------------------------------------------
   On DOMContentLoaded it:
     (a) holds the FULL information architecture in NAV (bilingual)
     (b) injects the sticky topbar (#du-topbar) + grouped sidebar (#du-sidebar),
         picking zh/en labels + hrefs from <html lang> and marking the active
         item from <body data-page> with the red squiggle underline
     (c) builds the right "on this page" TOC (#du-toc) from <main> h2/h3
         with smooth-scroll + scroll-spy
     (d) ⌘K / Ctrl-K command palette that filters NAV by title + keywords
     (e) a language toggle that swaps zh <-> /en/ preserving the current slug
     (f) code-block copy buttons + a top "Copy page" button
     (g) mobile hamburger that opens the sidebar as a drawer
   Every step guards for missing elements so a page can omit any region.
   ========================================================================== */
(function () {
  "use strict";

  /* -------------------------------------------------- information architecture
     slug 'overview' maps to index.html (and /en/index.html).
     Every other slug maps to /<slug>.html  (en mirror /en/<slug>.html).
     kw = extra search keywords (space separated, any language). ico = doodle. */
  var NAV = [
    { id: "start", label: { zh: "开始", en: "Getting Started" }, items: [
      { slug: "overview",  ico: "🧭", zh: "概览",        en: "Overview",            kw: "intro start home 首页 介绍" },
      { slug: "install",   ico: "📦", zh: "安装",        en: "Installation",        kw: "install setup 安装 npm curl extension 扩展" },
      { slug: "core-loop", ico: "🔁", zh: "核心循环",    en: "The Core Loop",       kw: "loop snapshot act observe 循环 workflow" },
    ]},
    { id: "core", label: { zh: "核心用法", en: "Core Usage" }, items: [
      { slug: "reading",     ico: "📖", zh: "读取页面",  en: "Reading Pages",       kw: "snapshot read dom accessibility 读取 快照" },
      { slug: "interacting", ico: "🖱️", zh: "交互操作",  en: "Interacting",         kw: "click type fill scroll 点击 输入 交互" },
      { slug: "finding",     ico: "🔎", zh: "定位元素",  en: "Finding Elements",    kw: "find selector ref locate 定位 查找 元素" },
      { slug: "waiting",     ico: "⏳", zh: "等待与断言", en: "Waiting & Asserting", kw: "wait expect assert 等待 断言" },
      { slug: "extract",     ico: "🧪", zh: "提取数据",  en: "Extracting Data",     kw: "extract schema scrape json 提取 抓取" },
    ]},
    { id: "auth", label: { zh: "会话与登录", en: "Sessions & Auth" }, items: [
      { slug: "login-auth",  ico: "🔐", zh: "登录与凭证", en: "Login & Credentials", kw: "login auth password 2fa 登录 凭证" },
      { slug: "sessions",    ico: "💾", zh: "会话与持久化", en: "Sessions & Persistence", kw: "session persist cookie profile 会话 持久化" },
      { slug: "real-chrome", ico: "🌐", zh: "驱动真实 Chrome", en: "Real Chrome (Extension)", kw: "real chrome extension native messaging 真实 扩展" },
    ]},
    { id: "advanced", label: { zh: "进阶", en: "Advanced" }, items: [
      { slug: "canvas",  ico: "🎨", zh: "Canvas / WebGL / 游戏", en: "Canvas / WebGL / Games", kw: "canvas webgl game figma capture 游戏" },
      { slug: "network", ico: "🛰️", zh: "网络拦截与改写", en: "Network Interception", kw: "network mock rewrite fetch cdp 拦截 改写" },
      { slug: "react",   ico: "⚛️", zh: "React / Web Vitals", en: "React / Web Vitals", kw: "react vitals performance 性能" },
      { slug: "media",   ico: "🎬", zh: "截图·录像·视口", en: "Screenshots, Video & Viewport", kw: "screenshot video viewport record 截图 录像 视口" },
    ]},
    { id: "special", label: { zh: "专项能力", en: "Specialized" }, items: [
      { slug: "electron", ico: "🖥️", zh: "Electron 桌面应用", en: "Electron Apps", kw: "electron desktop vscode slack discord 桌面" },
      { slug: "slack",    ico: "💬", zh: "Slack 自动化", en: "Slack", kw: "slack message unread 消息" },
      { slug: "testing",  ico: "🧷", zh: "测试与探索", en: "Testing & Dogfood", kw: "test dogfood qa expect 测试 探索" },
      { slug: "cloud",    ico: "☁️", zh: "云端浏览器", en: "Cloud Browsers", kw: "cloud bedrock agentcore vercel sandbox 云端" },
    ]},
    { id: "ref", label: { zh: "参考", en: "Reference" }, items: [
      { slug: "commands",        ico: "📚", zh: "命令参考",   en: "Command Reference",  kw: "commands cli flags reference 命令 参考" },
      { slug: "troubleshooting", ico: "🩹", zh: "故障排查",   en: "Troubleshooting",    kw: "troubleshoot debug relay error 故障 排查" },
      { slug: "family",          ico: "🧩", zh: "*-use 家族", en: "The *-use Family",   kw: "family iphone-use cookie-use bitwarden 家族" },
    ]},
  ];

  /* doc tabs (topbar center) */
  var TABS = [
    { slug: "overview", zh: "文档", en: "Documentation" },
    { slug: "commands", zh: "命令参考", en: "Reference" },
    { slug: "changelog", href: "https://github.com/leeguooooo/chrome-use/releases", zh: "更新", en: "Changelog" },
  ];

  var GH = "https://github.com/leeguooooo/chrome-use";

  /* --------------------------------------------------------------- helpers */
  function el(tag, cls, html) {
    var n = document.createElement(tag);
    if (cls) n.className = cls;
    if (html != null) n.innerHTML = html;
    return n;
  }
  function lang() {
    return (document.documentElement.lang || "zh").toLowerCase().indexOf("en") === 0 ? "en" : "zh";
  }
  // Build an href for a slug in a given language. overview -> index.html.
  function hrefFor(slug, lng) {
    var base = lng === "en" ? "/en/" : "/";
    return slug === "overview" ? base + "index.html" : base + slug + ".html";
  }
  function currentSlug() {
    return (document.body && document.body.dataset.page) || "overview";
  }
  var SVG = {
    gh: '<svg viewBox="0 0 16 16" fill="currentColor"><path d="M8 0C3.58 0 0 3.58 0 8c0 3.54 2.29 6.53 5.47 7.59.4.07.55-.17.55-.38 0-.19-.01-.82-.01-1.49-2.01.37-2.53-.49-2.69-.94-.09-.23-.48-.94-.82-1.13-.28-.15-.68-.52-.01-.53.63-.01 1.08.58 1.23.82.72 1.21 1.87.87 2.33.66.07-.52.28-.87.51-1.07-1.78-.2-3.64-.89-3.64-3.95 0-.87.31-1.59.82-2.15-.08-.2-.36-1.02.08-2.12 0 0 .67-.21 2.2.82.64-.18 1.32-.27 2-.27.68 0 1.36.09 2 .27 1.53-1.04 2.2-.82 2.2-.82.44 1.1.16 1.92.08 2.12.51.56.82 1.27.82 2.15 0 3.07-1.87 3.75-3.65 3.95.29.25.54.73.54 1.48 0 1.07-.01 1.93-.01 2.2 0 .21.15.46.55.38A8.01 8.01 0 0016 8c0-4.42-3.58-8-8-8z"/></svg>',
    search: '<svg viewBox="0 0 20 20" fill="none" stroke="currentColor" stroke-width="2"><circle cx="9" cy="9" r="6"/><path d="M14 14l4 4" stroke-linecap="round"/></svg>',
    menu: '<svg viewBox="0 0 20 20" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><path d="M3 5h14M3 10h14M3 15h14"/></svg>',
    copy: '<svg viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.6"><rect x="5" y="5" width="9" height="9" rx="1.5"/><path d="M11 5V3.5A1.5 1.5 0 009.5 2h-6A1.5 1.5 0 002 3.5v6A1.5 1.5 0 003.5 11H5"/></svg>',
  };

  /* ============================================================= TOP BAR == */
  function buildTopbar() {
    var host = document.getElementById("du-topbar");
    if (!host) return;
    var lng = lang();
    host.classList.add("du-topbar");

    // hamburger (mobile only, shown via CSS)
    var burger = el("button", "du-hamburger", SVG.menu);
    burger.setAttribute("aria-label", "菜单 / Menu");
    burger.addEventListener("click", toggleDrawer);
    host.appendChild(burger);

    // logo
    var logo = el("a", "du-logo");
    logo.href = hrefFor("overview", lng);
    logo.innerHTML =
      '<span class="du-logo-word">chrome<span class="accent">-use</span></span>' +
      '<span class="du-repo-tag">leeguooooo/chrome-use</span>';
    host.appendChild(logo);

    // center doc tabs
    var tabs = el("nav", "du-tabs");
    var slug = currentSlug();
    TABS.forEach(function (t) {
      var a = el("a", null, t[lng]);
      a.href = t.href || hrefFor(t.slug, lng);
      if (t.href) { a.target = "_blank"; a.rel = "noopener"; }
      if (t.slug === slug) a.classList.add("active");
      tabs.appendChild(a);
    });
    host.appendChild(tabs);

    // right cluster
    var right = el("div", "du-topbar-right");

    var searchBtn = el("button", "du-search-btn",
      SVG.search + '<span class="du-search-label">' + (lng === "en" ? "Search…" : "搜索…") + '</span>' +
      '<span class="du-kbd">⌘K</span>');
    searchBtn.addEventListener("click", openSearch);
    right.appendChild(searchBtn);

    var langBox = el("div", "du-lang");
    var zhA = el("a", lng === "zh" ? "active" : null, "中");
    var enA = el("a", lng === "en" ? "active" : null, "EN");
    zhA.href = hrefFor(currentSlug(), "zh");
    enA.href = hrefFor(currentSlug(), "en");
    langBox.appendChild(zhA); langBox.appendChild(enA);
    right.appendChild(langBox);

    var gh = el("a", "du-gh", SVG.gh + "<span>GitHub</span>");
    gh.href = GH; gh.target = "_blank"; gh.rel = "noopener";
    right.appendChild(gh);

    host.appendChild(right);
  }

  /* ============================================================= SIDEBAR == */
  function buildSidebar() {
    var host = document.getElementById("du-sidebar");
    if (!host) return;
    var lng = lang();
    var slug = currentSlug();
    host.classList.add("du-sidebar");
    host.setAttribute("role", "navigation");

    NAV.forEach(function (group) {
      var g = el("div", "du-nav-group");
      g.appendChild(el("div", "du-nav-label", group.label[lng]));
      group.items.forEach(function (it) {
        var a = el("a", "du-nav-item");
        a.href = hrefFor(it.slug, lng);
        a.innerHTML =
          '<span class="du-nav-ico">' + it.ico + '</span>' +
          '<span class="du-nav-text">' + it[lng] + '</span>';
        if (it.slug === slug) {
          a.classList.add("active");
          a.setAttribute("aria-current", "page");
        }
        a.addEventListener("click", closeDrawer);
        g.appendChild(a);
      });
      host.appendChild(g);
    });

    // drawer scrim (mobile)
    if (!document.querySelector(".du-drawer-scrim")) {
      var scrim = el("div", "du-drawer-scrim");
      scrim.addEventListener("click", closeDrawer);
      document.body.appendChild(scrim);
    }
  }

  function toggleDrawer() { document.body.classList.toggle("du-drawer-open"); }
  function closeDrawer() { document.body.classList.remove("du-drawer-open"); }

  /* =============================================== ON-THIS-PAGE TOC ======= */
  function buildTOC() {
    var host = document.getElementById("du-toc");
    var main = document.querySelector("main.du-main") || document.querySelector("main");
    if (!host || !main) return;
    var heads = main.querySelectorAll("h2, h3");
    if (!heads.length) { host.style.display = "none"; return; }

    var lng = lang();
    host.classList.add("du-toc");
    host.appendChild(el("div", "du-toc-label", lng === "en" ? "On this page" : "本页目录"));
    var ul = el("ul");
    var links = [];

    heads.forEach(function (h, i) {
      if (!h.id) h.id = "sec-" + ((h.textContent || "").trim().toLowerCase().replace(/[^\w一-龥]+/g, "-").replace(/^-+|-+$/g, "") || i);
      // add hover anchor link on the heading itself
      if (!h.querySelector(".du-anchor")) {
        var anc = el("a", "du-anchor", "#");
        anc.href = "#" + h.id; anc.setAttribute("aria-hidden", "true");
        h.appendChild(anc);
      }
      var li = el("li");
      var a = el("a", h.tagName === "H3" ? "lvl-3" : "lvl-2", (h.textContent || "").replace(/#$/, "").trim());
      a.href = "#" + h.id;
      a.addEventListener("click", function (e) {
        e.preventDefault();
        var t = document.getElementById(h.id);
        if (t) t.scrollIntoView({ behavior: "smooth", block: "start" });
        history.replaceState(null, "", "#" + h.id);
      });
      li.appendChild(a); ul.appendChild(li);
      links.push({ id: h.id, a: a, el: h });
    });
    host.appendChild(ul);

    // scroll-spy
    if ("IntersectionObserver" in window) {
      var visible = {};
      var io = new IntersectionObserver(function (entries) {
        entries.forEach(function (en) { visible[en.target.id] = en.isIntersecting ? en.intersectionRatio : 0; });
        var best = null, bestR = 0;
        links.forEach(function (l) { var r = visible[l.id] || 0; if (r >= bestR && r > 0) { bestR = r; best = l; } });
        if (!best) return;
        links.forEach(function (l) { l.a.classList.toggle("active", l === best); });
      }, { rootMargin: "-" + (parseInt(getComputedStyle(document.documentElement).getPropertyValue("--topbar-h")) || 58) + "px 0px -70% 0px", threshold: [0, .25, .5, 1] });
      links.forEach(function (l) { io.observe(l.el); });
    }
  }

  /* ===================================================== ⌘K SEARCH ======= */
  var searchState = { overlay: null, input: null, results: null, flat: [], filtered: [], idx: 0 };

  function flattenNav() {
    var lng = lang(), out = [];
    NAV.forEach(function (g) {
      g.items.forEach(function (it) {
        out.push({
          title: it[lng], ico: it.ico, group: g.label[lng],
          href: hrefFor(it.slug, lng),
          hay: (it.zh + " " + it.en + " " + it.slug + " " + (it.kw || "")).toLowerCase(),
        });
      });
    });
    return out;
  }

  function buildSearch() {
    var overlay = el("div", "du-search-overlay");
    overlay.innerHTML =
      '<div class="du-search-box" role="dialog" aria-modal="true" aria-label="搜索文档">' +
        '<div class="du-search-input-row">' + SVG.search +
          '<input class="du-search-input" type="text" placeholder="' + (lang() === "en" ? "Search docs…" : "搜索文档…") + '" autocomplete="off" spellcheck="false" />' +
        '</div>' +
        '<div class="du-search-results"></div>' +
        '<div class="du-search-foot"><span><kbd>↑</kbd><kbd>↓</kbd> ' + (lang() === "en" ? "navigate" : "选择") + '</span>' +
          '<span><kbd>↵</kbd> ' + (lang() === "en" ? "open" : "打开") + '</span>' +
          '<span><kbd>esc</kbd> ' + (lang() === "en" ? "close" : "关闭") + '</span></div>' +
      '</div>';
    document.body.appendChild(overlay);
    searchState.overlay = overlay;
    searchState.input = overlay.querySelector(".du-search-input");
    searchState.results = overlay.querySelector(".du-search-results");
    searchState.flat = flattenNav();

    overlay.addEventListener("click", function (e) { if (e.target === overlay) closeSearch(); });
    searchState.input.addEventListener("input", function () { renderResults(this.value); });
    searchState.input.addEventListener("keydown", function (e) {
      if (e.key === "ArrowDown") { e.preventDefault(); moveSel(1); }
      else if (e.key === "ArrowUp") { e.preventDefault(); moveSel(-1); }
      else if (e.key === "Enter") { e.preventDefault(); go(); }
      else if (e.key === "Escape") { e.preventDefault(); closeSearch(); }
    });
  }
  function renderResults(q) {
    q = (q || "").trim().toLowerCase();
    var list = q ? searchState.flat.filter(function (r) { return r.hay.indexOf(q) >= 0; }) : searchState.flat;
    searchState.filtered = list; searchState.idx = 0;
    var box = searchState.results; box.innerHTML = "";
    if (!list.length) { box.appendChild(el("div", "du-search-empty", lang() === "en" ? "No matches" : "没有匹配结果")); return; }
    list.forEach(function (r, i) {
      var a = el("a", "du-search-result" + (i === 0 ? " active" : ""),
        '<span class="ico">' + r.ico + '</span><span>' + r.title + '</span><span class="grp">' + r.group + '</span>');
      a.href = r.href;
      a.addEventListener("mousemove", function () { setSel(i); });
      a.addEventListener("click", function (e) { e.preventDefault(); location.href = r.href; });
      box.appendChild(a);
    });
  }
  function setSel(i) {
    searchState.idx = i;
    var kids = searchState.results.children;
    for (var k = 0; k < kids.length; k++) kids[k].classList.toggle("active", k === i);
  }
  function moveSel(d) {
    var n = searchState.filtered.length; if (!n) return;
    setSel((searchState.idx + d + n) % n);
    var act = searchState.results.children[searchState.idx];
    if (act) act.scrollIntoView({ block: "nearest" });
  }
  function go() { var r = searchState.filtered[searchState.idx]; if (r) location.href = r.href; }
  function openSearch() {
    if (!searchState.overlay) buildSearch();
    searchState.overlay.classList.add("open");
    renderResults("");
    searchState.input.value = "";
    setTimeout(function () { searchState.input.focus(); }, 20);
  }
  function closeSearch() { if (searchState.overlay) searchState.overlay.classList.remove("open"); }

  /* ================================================== COPY BUTTONS ======= */
  function copyText(text, btn, doneLabel, isIcon) {
    var write = navigator.clipboard && navigator.clipboard.writeText
      ? navigator.clipboard.writeText(text)
      : new Promise(function (res, rej) {
          var ta = document.createElement("textarea"); ta.value = text;
          ta.style.position = "fixed"; ta.style.opacity = "0"; document.body.appendChild(ta);
          ta.select(); try { document.execCommand("copy"); res(); } catch (e) { rej(e); } document.body.removeChild(ta);
        });
    write.then(function () {
      btn.classList.add("copied");
      var prev = btn.innerHTML;
      if (!isIcon) btn.textContent = doneLabel;
      setTimeout(function () { btn.classList.remove("copied"); if (!isIcon) btn.innerHTML = prev; }, 1400);
    }).catch(function () {});
  }

  function wireCodeCopy() {
    var lng = lang();
    document.querySelectorAll(".du-code").forEach(function (block) {
      if (block.querySelector(".du-code-copy")) return;
      var head = block.querySelector(".du-code-head");
      if (!head) { // synthesize a header with mac dots if the author omitted one
        head = el("div", "du-code-head", '<span class="dot r"></span><span class="dot y"></span><span class="dot g"></span>');
        block.insertBefore(head, block.firstChild);
      }
      var btn = el("button", "du-code-copy", lng === "en" ? "Copy" : "复制");
      btn.addEventListener("click", function () {
        var pre = block.querySelector("pre");
        copyText(pre ? pre.innerText : "", btn, lng === "en" ? "Copied" : "已复制", false);
      });
      head.appendChild(btn);
    });
  }

  function wireCopyPage() {
    var lng = lang();
    document.querySelectorAll(".du-copy-page").forEach(function (btn) {
      if (btn.dataset.wired) return; btn.dataset.wired = "1";
      btn.addEventListener("click", function () {
        var main = document.querySelector("main.du-main") || document.querySelector("main");
        copyText(main ? main.innerText : "", btn, "", true);
        var lbl = btn.querySelector(".du-copy-page-label");
        if (lbl) { var p = lbl.textContent; lbl.textContent = lng === "en" ? "Copied!" : "已复制!"; setTimeout(function () { lbl.textContent = p; }, 1400); }
      });
    });
  }

  /* ================================================= GLOBAL KEYBINDINGS == */
  function wireKeys() {
    document.addEventListener("keydown", function (e) {
      if ((e.metaKey || e.ctrlKey) && (e.key === "k" || e.key === "K")) { e.preventDefault(); openSearch(); }
      else if (e.key === "Escape") { closeSearch(); closeDrawer(); }
      else if (e.key === "/" && !/^(INPUT|TEXTAREA)$/.test((document.activeElement || {}).tagName || "") && !e.metaKey && !e.ctrlKey) {
        e.preventDefault(); openSearch();
      }
    });
  }

  /* ============================================================= BOOT ===== */
  function init() {
    try { buildTopbar(); } catch (e) { console.error("[docs] topbar", e); }
    try { buildSidebar(); } catch (e) { console.error("[docs] sidebar", e); }
    try { buildTOC(); } catch (e) { console.error("[docs] toc", e); }
    try { wireCodeCopy(); } catch (e) { console.error("[docs] code copy", e); }
    try { wireCopyPage(); } catch (e) { console.error("[docs] copy page", e); }
    try { wireKeys(); } catch (e) { console.error("[docs] keys", e); }
  }
  if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", init);
  else init();
})();

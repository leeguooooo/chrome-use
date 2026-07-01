/* ==========================================================================
   chrome-use docs — auth.js  (OIDC PKCE client for account.leeguoo.com SSO)
   --------------------------------------------------------------------------
   A dependency-free public OIDC client for a static site:
     - silent SSO on load via a hidden iframe (`prompt=none`) — if you're
       already signed into account.leeguoo.com you're signed in here too
     - interactive login via full-page redirect
     - PKCE (S256), no client secret
     - renders an account chip into the topbar (#du-account)
     - membership: reads /api/billing/entitlements and exposes isMember()
       (membership.all_apps), then unlocks [data-members-only] blocks
   The account endpoints are same-SITE (leeguoo.com) with credentialed CORS,
   so cross-origin reads of /token, /userinfo and /entitlements work.
   ========================================================================== */
(function () {
  'use strict';

  var CFG = {
    issuer: 'https://account.leeguoo.com',
    clientId: 'chrome-use-web',
    redirectUri: 'https://chrome-use.leeguoo.com/auth/callback/',
    scope: 'openid profile email',
    memberEntitlement: 'membership.all_apps'
  };
  // Local dev: point the redirect back at the current origin's callback.
  if (location.hostname === 'localhost' || location.hostname === '127.0.0.1') {
    CFG.redirectUri = location.origin + '/auth/callback/';
  }

  var LS = {
    tok: 'cu_oidc_tok',       // { access_token, id_token, exp }
    verifier: 'cu_oidc_pkce', // transient PKCE verifier (sessionStorage)
    state: 'cu_oidc_state',
    ret: 'cu_oidc_return'
  };

  // ---------------------------------------------------------------- utilities
  function b64url(bytes) {
    var s = '';
    for (var i = 0; i < bytes.length; i++) s += String.fromCharCode(bytes[i]);
    return btoa(s).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');
  }
  function randB64(n) {
    var a = new Uint8Array(n);
    crypto.getRandomValues(a);
    return b64url(a);
  }
  async function sha256b64(str) {
    var d = await crypto.subtle.digest('SHA-256', new TextEncoder().encode(str));
    return b64url(new Uint8Array(d));
  }
  function decodeJwt(jwt) {
    try {
      var p = jwt.split('.')[1].replace(/-/g, '+').replace(/_/g, '/');
      return JSON.parse(decodeURIComponent(escape(atob(p))));
    } catch (e) { return null; }
  }
  function saveTok(t) { try { localStorage.setItem(LS.tok, JSON.stringify(t)); } catch (e) {} }
  function loadTok() {
    try { return JSON.parse(localStorage.getItem(LS.tok) || 'null'); } catch (e) { return null; }
  }
  function clearTok() { try { localStorage.removeItem(LS.tok); } catch (e) {} }
  function tokValid(t) { return t && t.exp && (Date.now() / 1000) < (t.exp - 30); }

  // ---------------------------------------------------------------- authorize
  async function buildAuthorizeUrl(opts) {
    var verifier = randB64(32);
    var state = randB64(16);
    sessionStorage.setItem(LS.verifier, verifier);
    sessionStorage.setItem(LS.state, state);
    var challenge = await sha256b64(verifier);
    var q = new URLSearchParams({
      response_type: 'code',
      client_id: CFG.clientId,
      redirect_uri: CFG.redirectUri,
      scope: CFG.scope,
      state: state,
      code_challenge: challenge,
      code_challenge_method: 'S256'
    });
    if (opts && opts.silent) q.set('prompt', 'none');
    return CFG.issuer + '/authorize?' + q.toString();
  }

  async function exchangeCode(code) {
    var verifier = sessionStorage.getItem(LS.verifier) || '';
    var body = new URLSearchParams({
      grant_type: 'authorization_code',
      code: code,
      redirect_uri: CFG.redirectUri,
      client_id: CFG.clientId,
      code_verifier: verifier
    });
    var res = await fetch(CFG.issuer + '/token', {
      method: 'POST',
      headers: { 'Content-Type': 'application/x-www-form-urlencoded' },
      body: body.toString()
    });
    if (!res.ok) throw new Error('token exchange failed: ' + res.status);
    var data = await res.json();
    var claims = decodeJwt(data.id_token || '') || {};
    var exp = claims.exp || (Math.floor(Date.now() / 1000) + (data.expires_in || 3600));
    var tok = { access_token: data.access_token, id_token: data.id_token, exp: exp };
    saveTok(tok);
    sessionStorage.removeItem(LS.verifier);
    sessionStorage.removeItem(LS.state);
    return tok;
  }

  // Silent SSO: hidden iframe → /authorize?prompt=none → callback postMessages
  // back {code|error}. Resolves to a token or null (not signed in).
  function silentSso() {
    return new Promise(function (resolve) {
      buildAuthorizeUrl({ silent: true }).then(function (url) {
        var iframe = document.createElement('iframe');
        iframe.style.display = 'none';
        var done = false;
        var timer = setTimeout(finish, 8000, null);
        function finish(result) {
          if (done) return; done = true;
          clearTimeout(timer);
          window.removeEventListener('message', onMsg);
          if (iframe.parentNode) iframe.parentNode.removeChild(iframe);
          resolve(result);
        }
        function onMsg(ev) {
          if (ev.origin !== location.origin) return;
          var d = ev.data || {};
          if (d.type !== 'cu-oidc-callback') return;
          if (d.code && d.state === sessionStorage.getItem(LS.state)) {
            exchangeCode(d.code).then(finish, function () { finish(null); });
          } else {
            finish(null); // login_required / error
          }
        }
        window.addEventListener('message', onMsg);
        iframe.src = url;
        document.body.appendChild(iframe);
      });
    });
  }

  async function login() {
    try { sessionStorage.setItem(LS.ret, location.pathname + location.search); } catch (e) {}
    location.href = await buildAuthorizeUrl({ silent: false });
  }
  function logout() {
    clearTok();
    render();
    applyGating();
  }

  // ---------------------------------------------------------------- identity
  var state = { user: null, member: false, ready: false };

  function userFromTok(t) {
    var c = decodeJwt(t && t.id_token) || {};
    return {
      name: c.name || c.preferred_username || c.email || '账号',
      email: c.email || '',
      picture: c.picture || c.avatar_url || ''
    };
  }

  async function checkMember(access) {
    try {
      var res = await fetch(CFG.issuer + '/api/billing/entitlements', {
        headers: { Authorization: 'Bearer ' + access }
      });
      if (!res.ok) return false;
      var data = await res.json();
      var rows = data.entitlements || data.results || data.data || [];
      return rows.some(function (e) {
        var k = e.entitlement_key || e.key || '';
        var st = (e.status || 'active');
        return k === CFG.memberEntitlement && st === 'active';
      });
    } catch (e) { return false; }
  }

  // ---------------------------------------------------------------- rendering
  function isEn() { return (document.documentElement.lang || '').slice(0, 2) === 'en'; }
  function t(zh, en) { return isEn() ? en : zh; }

  function render() {
    var mount = document.getElementById('du-account');
    if (!mount) return;
    mount.innerHTML = '';
    if (state.user) {
      var chip = document.createElement('button');
      chip.className = 'du-account-chip';
      chip.type = 'button';
      chip.setAttribute('aria-label', t('账号', 'Account'));
      var av = state.user.picture
        ? '<img class="du-account-av" src="' + state.user.picture + '" alt="" referrerpolicy="no-referrer">'
        : '<span class="du-account-av du-account-av-fallback">' + (state.user.name || '·').slice(0, 1) + '</span>';
      var badge = state.member ? '<img class="du-vip-badge" src="/assets/brand/vip-badge.webp" alt="VIP" title="' + t('VIP 会员', 'VIP member') + '">' : '';
      chip.innerHTML = av + '<span class="du-account-name">' + escapeHtml(state.user.name) + '</span>' + badge;
      var menu = document.createElement('div');
      menu.className = 'du-account-menu';
      menu.hidden = true;
      menu.innerHTML =
        '<div class="du-account-menu-head">' + escapeHtml(state.user.email || state.user.name) + '</div>' +
        (state.member
          ? '<div class="du-account-menu-vip"><img class="du-vip-badge" src="/assets/brand/vip-badge.webp" alt=""> ' + t('VIP 会员', 'VIP member') + '</div>'
          : '') +
        '<a class="du-account-menu-item" href="' + CFG.issuer + '/account" target="_blank" rel="noopener">' + t('账号中心', 'Account center') + '</a>' +
        '<button class="du-account-menu-item" type="button" data-logout>' + t('退出登录', 'Sign out') + '</button>';
      chip.addEventListener('click', function (e) { e.stopPropagation(); menu.hidden = !menu.hidden; });
      document.addEventListener('click', function () { menu.hidden = true; });
      menu.addEventListener('click', function (e) {
        if (e.target && e.target.hasAttribute('data-logout')) logout();
      });
      var wrap = document.createElement('div');
      wrap.className = 'du-account-wrap';
      wrap.appendChild(chip);
      wrap.appendChild(menu);
      mount.appendChild(wrap);
    } else {
      var btn = document.createElement('button');
      btn.className = 'du-account-login';
      btn.type = 'button';
      btn.textContent = t('登录', 'Sign in');
      btn.addEventListener('click', login);
      mount.appendChild(btn);
    }
  }

  function escapeHtml(s) {
    return String(s || '').replace(/[&<>"']/g, function (c) {
      return { '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c];
    });
  }

  // ---------------------------------------------------------------- gating
  // [data-members-only] blocks are shown to members and replaced with a locked
  // notice + upgrade CTA for everyone else. No blocks are gated by default —
  // authors opt in per element/section.
  function applyGating() {
    var nodes = document.querySelectorAll('[data-members-only]');
    for (var i = 0; i < nodes.length; i++) {
      var el = nodes[i];
      if (state.member) {
        el.hidden = false;
        var lock = el.previousElementSibling;
        if (lock && lock.classList && lock.classList.contains('du-locked')) lock.parentNode.removeChild(lock);
      } else {
        el.hidden = true;
        if (!(el.previousElementSibling && el.previousElementSibling.classList && el.previousElementSibling.classList.contains('du-locked'))) {
          var box = document.createElement('div');
          box.className = 'du-locked du-callout';
          box.innerHTML =
            '<div class="du-locked-title">🔒 ' + t('会员专享', 'Members only') + '</div>' +
            '<p>' + t('这部分内容面向会员。', 'This section is available to members.') + ' ' +
            (state.user
              ? t('你的账号暂无会员权益。', "Your account doesn't have membership yet.")
              : t('登录并开通会员即可解锁。', 'Sign in and become a member to unlock.')) + '</p>' +
            '<div class="du-locked-actions">' +
            (state.user ? '' : '<button type="button" class="du-btn-primary" data-cu-login>' + t('登录', 'Sign in') + '</button>') +
            '<a class="du-btn-ghost" href="' + CFG.issuer + '/account" target="_blank" rel="noopener">' + t('开通会员', 'Get membership') + '</a>' +
            '</div>';
          el.parentNode.insertBefore(box, el);
          var lb = box.querySelector('[data-cu-login]');
          if (lb) lb.addEventListener('click', login);
        }
      }
    }
  }

  // ---------------------------------------------------------------- bootstrap
  async function init() {
    if (state.ready) { render(); return; }
    if (state.started) { render(); return; }
    state.started = true;
    var tok = loadTok();
    if (!tokValid(tok)) {
      tok = await silentSso();
    }
    if (tokValid(tok)) {
      state.user = userFromTok(tok);
      render();
      state.member = await checkMember(tok.access_token);
    } else {
      state.user = null;
      state.member = false;
    }
    state.ready = true;
    render();
    applyGating();
  }

  window.CUAuth = {
    init: init,
    login: login,
    logout: logout,
    isMember: function () { return state.member; },
    getUser: function () { return state.user; },
    applyGating: applyGating
  };

  // Auto-init once the DOM (and the topbar mount injected by docs.js) is ready.
  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', function () { setTimeout(init, 0); });
  } else {
    setTimeout(init, 0);
  }
})();

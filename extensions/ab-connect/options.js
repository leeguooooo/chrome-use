// Options page for chrome-use.
// Settings live in chrome.storage.sync; the service worker reads them to decide
// whether to fire connection notifications and whether to draw the agent cursor.
// Opens standalone (file://) too, with a friendly demo state, so the design is
// viewable without the extension context.

const DEFAULTS = { ab_notify: false, ab_cursor: false, ab_sites: [] }
const hasChrome = typeof chrome !== 'undefined' && chrome.storage && chrome.storage.sync

// ---- elements ----
const el = (id) => document.getElementById(id)
const dot = el('dot'), statusLabel = el('statusLabel'), statusSub = el('statusSub'), tabPill = el('tabPill')
const optNotify = el('optNotify'), optCursor = el('optCursor')
const sitesBox = el('sites'), siteInput = el('siteInput')
const saved = el('saved')

// ---- settings ----
function loadSettings(cb) {
  if (!hasChrome) { cb({ ...DEFAULTS, ab_notify: true, ab_cursor: true, ab_sites: ['github.com', 'mail.google.com'] }); return }
  chrome.storage.sync.get(DEFAULTS, (s) => cb(s))
}
function save(patch) {
  if (hasChrome) chrome.storage.sync.set(patch)
  flashSaved()
}
let savedTimer
function flashSaved() {
  saved.classList.add('show')
  clearTimeout(savedTimer)
  savedTimer = setTimeout(() => saved.classList.remove('show'), 1400)
}

// ---- approved sites ----
let sites = []
function renderSites() {
  sitesBox.innerHTML = ''
  if (!sites.length) {
    const e = document.createElement('div')
    e.className = 'empty'
    e.textContent = 'No sites added — chrome-use isn’t restricted.'
    sitesBox.appendChild(e)
    return
  }
  for (const host of sites) {
    const row = document.createElement('div')
    row.className = 'site'
    const h = document.createElement('span'); h.className = 'host'; h.textContent = host
    const x = document.createElement('span'); x.className = 'x'; x.textContent = '✕'; x.title = 'Remove'
    x.addEventListener('click', () => { sites = sites.filter((s) => s !== host); save({ ab_sites: sites }); renderSites() })
    row.append(h, x)
    sitesBox.appendChild(row)
  }
}
function addSite() {
  const raw = (siteInput.value || '').trim().toLowerCase().replace(/^https?:\/\//, '').replace(/\/.*$/, '')
  if (!raw || sites.includes(raw)) { siteInput.value = ''; return }
  sites.push(raw); sites.sort()
  save({ ab_sites: sites }); siteInput.value = ''; renderSites()
}

// ---- connection status ----
function renderStatus(state) {
  const connected = !!(state && state.connected)
  dot.classList.remove('on', 'off')
  if (connected) {
    dot.classList.add('on')
    statusLabel.textContent = 'Connected'
    statusSub.textContent = 'bridged to your local CLI · ready'
    const n = state.tabCount | 0
    if (n > 0) { tabPill.textContent = `${n} tab${n === 1 ? '' : 's'} attached`; tabPill.style.display = '' }
    else tabPill.style.display = 'none'
  } else {
    dot.classList.add('off')
    statusLabel.textContent = 'Not paired'
    statusSub.textContent = 'run `chrome-use extension install`, then reopen'
    tabPill.style.display = 'none'
  }
}
function queryStatus() {
  if (!(hasChrome && chrome.runtime && chrome.runtime.sendMessage)) { renderStatus({ connected: true, tabCount: 13 }); return }
  try {
    chrome.runtime.sendMessage({ type: 'ab-status' }, (resp) => {
      renderStatus(chrome.runtime.lastError ? { connected: false } : resp)
    })
  } catch { renderStatus({ connected: false }) }
}

// ---- wire up ----
el('ver').textContent = hasChrome ? 'v' + chrome.runtime.getManifest().version : 'v—'

loadSettings((s) => {
  optNotify.checked = !!s.ab_notify
  optCursor.checked = !!s.ab_cursor
  sites = Array.isArray(s.ab_sites) ? s.ab_sites.slice() : []
  renderSites()
})

optNotify.addEventListener('change', () => save({ ab_notify: optNotify.checked }))
optCursor.addEventListener('change', () => save({ ab_cursor: optCursor.checked }))
el('addSite').addEventListener('click', addSite)
siteInput.addEventListener('keydown', (e) => { if (e.key === 'Enter') addSite() })

el('copyCmd').addEventListener('click', () => {
  const cmd = el('silentCmd').textContent
  navigator.clipboard?.writeText(cmd).then(() => {
    const b = el('copyCmd'); const t = b.textContent; b.textContent = 'Copied ✓'
    setTimeout(() => (b.textContent = t), 1300)
  }).catch(() => {})
})

const repo = el('repo')
repo.addEventListener('click', (e) => {
  e.preventDefault()
  if (hasChrome && chrome.tabs) chrome.tabs.create({ url: repo.dataset.href })
  else window.open(repo.dataset.href, '_blank')
})

queryStatus()
setTimeout(queryStatus, 700)

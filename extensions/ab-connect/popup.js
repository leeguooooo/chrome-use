// Popup status page for chrome-use.
// Asks the service worker whether the native-messaging link to the local
// chrome-use CLI is live, and renders a paired / not-paired indicator.

const dot = document.getElementById('dot')
const label = document.getElementById('statusLabel')
const sub = document.getElementById('statusSub')
const tabPill = document.getElementById('tabPill')
const hint = document.getElementById('hint')

let resolved = false

function render(state) {
  resolved = true
  const connected = !!(state && state.connected)
  dot.classList.remove('on', 'off')
  if (connected) {
    dot.classList.add('on')
    label.textContent = 'Connected'
    const n = state.tabCount | 0
    sub.textContent = 'bridged to your local CLI · ready'
    if (n > 0) {
      tabPill.textContent = `${n} tab${n === 1 ? '' : 's'}`
      tabPill.classList.remove('hidden')
    } else {
      tabPill.classList.add('hidden')
    }
    hint.style.display = 'none'
  } else {
    dot.classList.add('off')
    label.textContent = 'Not paired'
    sub.textContent = 'no local chrome-use CLI linked'
    tabPill.classList.add('hidden')
    hint.style.display = 'block'
  }
}

function queryStatus() {
  // Standalone (opened as a plain file, no extension context) → show a friendly
  // demo state so the design is viewable without the service worker.
  if (typeof chrome === 'undefined' || !chrome.runtime || !chrome.runtime.sendMessage) {
    render({ connected: true, tabCount: 13 })
    return
  }
  try {
    chrome.runtime.sendMessage({ type: 'ab-status' }, (resp) => {
      // lastError fires if the service worker can't be reached.
      if (chrome.runtime.lastError) {
        render({ connected: false })
        return
      }
      render(resp)
    })
  } catch (e) {
    render({ connected: false })
  }
}

// Open external links (docs / repo / site) in a real tab (no inline handlers
// under MV3 CSP). Every [data-href] anchor is wired the same way.
document.querySelectorAll('[data-href]').forEach((a) => {
  a.addEventListener('click', () => {
    const url = a.dataset.href
    if (typeof chrome !== 'undefined' && chrome.tabs && chrome.tabs.create) {
      chrome.tabs.create({ url })
    } else {
      window.open(url, '_blank')
    }
  })
})

// Query now, then once more shortly after — opening the popup also nudges the
// service worker to (re)connect the host, which may complete a beat later.
queryStatus()
setTimeout(queryStatus, 700)

// Never leave the popup stuck on "Checking…" if the worker never answers.
setTimeout(() => {
  if (!resolved) render({ connected: false })
}, 1500)

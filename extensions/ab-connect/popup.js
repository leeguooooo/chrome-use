// Popup status page for chrome-use.
// Asks the service worker whether the native-messaging link to the local
// chrome-use CLI is live, and renders a paired / not-paired indicator.

const dot = document.getElementById('dot')
const label = document.getElementById('statusLabel')
const sub = document.getElementById('statusSub')
const tabPill = document.getElementById('tabPill')
const hint = document.getElementById('hint')
const hintText = document.getElementById('hintText')
const hintCommand = document.getElementById('hintCommand')

let resolved = false

function render(state) {
  resolved = true
  const connected = !!(state && state.connected)
  dot.classList.remove('on', 'off')
  if (connected) {
    dot.classList.add('on')
    label.textContent = 'Connected'
    const n = state.tabCount | 0
    sub.textContent = 'local CLI connection confirmed'
    if (n > 0) {
      tabPill.textContent = `${n} tab${n === 1 ? '' : 's'}`
      tabPill.classList.remove('hidden')
    } else {
      tabPill.classList.add('hidden')
    }
    hint.style.display = 'none'
  } else if (state?.connectionState === 'connecting') {
    label.textContent = 'Connecting…'
    sub.textContent = 'waiting for the local CLI to respond'
    tabPill.classList.add('hidden')
    hint.style.display = 'none'
  } else {
    dot.classList.add('off')
    label.textContent = 'Not paired'
    sub.textContent = state?.connectionError || 'no local chrome-use CLI linked'
    tabPill.classList.add('hidden')
    hint.style.display = 'block'
    const missingHost = (state?.connectionError || '').toLowerCase().includes('host not found')
    hintText.textContent = missingHost
      ? 'Install the local host, then reopen this popup:'
      : 'Check the local CLI, then reopen this popup to reconnect:'
    hintCommand.textContent = missingHost ? 'chrome-use extension install' : 'chrome-use status'
  }
}

function queryStatus() {
  // A standalone preview has no native connection to confirm.
  if (typeof chrome === 'undefined' || !chrome.runtime || !chrome.runtime.sendMessage) {
    render({ connected: false, connectionError: 'Open this popup from the chrome-use extension' })
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

// Update an open popup even when the host confirms after the startup queries.
if (typeof chrome !== 'undefined' && chrome.runtime?.onMessage) {
  chrome.runtime.onMessage.addListener((message) => {
    if (message?.type === 'ab-host-state') render(message)
  })
}

// Query now, then once more shortly after — opening the popup also nudges the
// service worker to (re)connect the host, which may complete a beat later.
queryStatus()
setTimeout(queryStatus, 700)

// Never leave the popup stuck on "Checking…" if the worker never answers.
setTimeout(() => {
  if (!resolved) render({ connected: false })
}, 1500)

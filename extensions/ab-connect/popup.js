// Popup status page for chrome-use.
// Asks the service worker whether the native-messaging link to the local
// chrome-use CLI is live, and renders a paired / not-paired indicator.

const dot = document.getElementById('dot')
const label = document.getElementById('statusLabel')
const sub = document.getElementById('statusSub')
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
    sub.textContent =
      n > 0
        ? `bridged to the local CLI · ${n} tab${n === 1 ? '' : 's'} attached`
        : 'bridged to the local CLI · ready'
    hint.style.display = 'none'
  } else {
    dot.classList.add('off')
    label.textContent = 'Not paired'
    sub.textContent = 'no local chrome-use CLI linked'
    hint.style.display = 'block'
  }
}

function queryStatus() {
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

// Open the repo in a real tab (no inline handlers under MV3 CSP).
const repo = document.getElementById('repo')
if (repo) {
  repo.addEventListener('click', () => {
    chrome.tabs.create({ url: repo.dataset.href })
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

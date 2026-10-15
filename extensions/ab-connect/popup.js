// Popup status page for chrome-use.
// Asks the service worker whether the native-messaging link to the local
// chrome-use CLI is live, and renders a paired / not-paired indicator.

import { t, applyI18n } from './i18n.js'

applyI18n(document)

const dot = document.getElementById('dot')
const label = document.getElementById('statusLabel')
const sub = document.getElementById('statusSub')
const tabPill = document.getElementById('tabPill')
const hint = document.getElementById('hint')
const hintText = document.getElementById('hintText')
const hintCommand = document.getElementById('hintCommand')

let resolved = false
let statusEpoch = 0

function render(state) {
  resolved = true
  const connected = !!(state && state.connected)
  dot.classList.remove('on', 'off')
  if (connected) {
    dot.classList.add('on')
    label.textContent = t('popup_status_connected')
    const n = state.tabCount | 0
    sub.textContent = t('popup_status_connected_detail')
    if (n > 0) {
      tabPill.textContent = n === 1 ? t('popup_tab_count_one') : t('popup_tab_count', n)
      tabPill.classList.remove('hidden')
    } else {
      tabPill.classList.add('hidden')
    }
    hint.style.display = 'none'
  } else if (state?.connectionState === 'connecting') {
    label.textContent = t('popup_status_connecting')
    sub.textContent = t('popup_status_connecting_detail')
    tabPill.classList.add('hidden')
    hint.style.display = 'none'
  } else {
    dot.classList.add('off')
    label.textContent = t('popup_status_not_paired')
    sub.textContent = state?.connectionError || t('popup_status_not_paired_detail')
    tabPill.classList.add('hidden')
    if (state?.standalone) {
      hint.style.display = 'none'
      hintCommand.textContent = ''
      return
    }
    hint.style.display = 'block'
    const missingHost = (state?.connectionError || '').toLowerCase().includes('host not found')
    hintText.textContent = missingHost
      ? t('popup_hint_install_host')
      : t('popup_hint_check_and_reopen')
    hintCommand.textContent = missingHost ? 'chrome-use extension install' : 'chrome-use status'
  }
}

function queryStatus() {
  // A standalone preview has no native connection to confirm.
  if (typeof chrome === 'undefined' || !chrome.runtime || !chrome.runtime.sendMessage) {
    render({ connected: false, standalone: true, connectionError: t('popup_standalone') })
    return
  }
  const queryEpoch = ++statusEpoch
  try {
    chrome.runtime.sendMessage({ type: 'ab-status' }, (resp) => {
      // Acknowledge lastError even for a stale callback, then preserve any
      // newer query or pushed state that has already arrived.
      const error = chrome.runtime.lastError
      if (queryEpoch !== statusEpoch) return
      if (error) {
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

// Inspect & Ask: the popup click grants activeTab for the current tab, so the
// background can inject the picker there. Errors stay visible in the popup.
document.querySelectorAll('[data-action="inspect"]').forEach((button) => {
  button.addEventListener('click', async () => {
    if (typeof chrome === 'undefined' || !chrome.runtime?.sendMessage) return
    const response = await chrome.runtime.sendMessage({ type: 'ab-inspect-start' }).catch((error) => ({ success: false, error: String(error.message || error) }))
    if (response?.success) { window.close(); return }
    const note = document.getElementById('inspectError')
    if (note) { note.hidden = false; note.textContent = response?.error || t('popup_inspect_failed') }
  })
})

document.querySelectorAll('[data-action="options"]').forEach((button) => {
  button.addEventListener('click', () => {
    if (typeof chrome !== 'undefined') chrome.runtime?.openOptionsPage?.()
  })
})

// Update an open popup even when the host confirms after the startup queries.
if (typeof chrome !== 'undefined' && chrome.runtime?.onMessage) {
  chrome.runtime.onMessage.addListener((message) => {
    if (message?.type === 'ab-host-state') {
      statusEpoch++
      render(message)
    }
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

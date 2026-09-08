function showRestrictedFixture() {
  const frame = document.createElement('iframe')
  frame.title = 'Restricted extension fixture'
  frame.src = chrome.runtime.getURL('frame.html')
  document.body.appendChild(frame)
}
if (new URL(location.href).searchParams.get('trigger') === 'after') {
  document.addEventListener('show-restricted-fixture', showRestrictedFixture, { once: true })
} else {
  showRestrictedFixture()
}

const frame = document.createElement('iframe')
frame.title = 'Restricted extension fixture'
frame.src = chrome.runtime.getURL('frame.html')
document.body.appendChild(frame)

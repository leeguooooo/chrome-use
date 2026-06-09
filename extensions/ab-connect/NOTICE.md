# Attribution

The chrome.debugger attach + CDP Target handling in `background.js` is adapted
from **openclaw-browser-relay** by chengyixu
(https://github.com/chengyixu/openclaw-browser-relay, MIT per its README).

Changes for agent-browser-stealth: rebranded to "agent-browser connect"; the
transport is rewritten from a localhost WebSocket + shared token to Chrome
**native messaging** (host `com.agent_browser.connect`) — no port, no token,
Chrome authenticates the extension to the host by id. WebSocket/token/options
code removed.

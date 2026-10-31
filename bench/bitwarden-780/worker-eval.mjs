// CI-only control plane for disposable extension workers, never a web page.
import { readFileSync } from 'node:fs';
if (process.env.GITHUB_ACTIONS !== 'true') throw new Error('CI-only worker control');
const request = JSON.parse(readFileSync(0, 'utf8'));
const endpoint = new URL(request.endpoint);
if (endpoint.protocol !== 'ws:' || endpoint.hostname !== '127.0.0.1') throw new Error('loopback only');
const socket = new WebSocket(endpoint);
const timeout = setTimeout(() => { socket.close(); process.exitCode = 1; }, 8000);
socket.addEventListener('open', () => socket.send(JSON.stringify({id: 1, method: 'Runtime.evaluate',
  params: {expression: request.expression, awaitPromise: true, returnByValue: true}})));
socket.addEventListener('message', event => {
  const reply = JSON.parse(event.data);
  if (reply.id !== 1) return;
  clearTimeout(timeout);
  if (reply.error || reply.result?.exceptionDetails) {
    console.error(JSON.stringify(reply.error || {error: 'worker expression failed'}));
    process.exitCode = 1;
  } else console.log(JSON.stringify(reply.result?.result?.value ?? null));
  socket.close();
});
socket.addEventListener('error', () => { clearTimeout(timeout); process.exitCode = 1; socket.close(); });

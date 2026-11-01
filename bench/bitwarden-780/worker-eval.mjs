// CI-only control plane for disposable extension workers, never a web page.
import { readFileSync } from 'node:fs';
if (process.env.GITHUB_ACTIONS !== 'true') throw new Error('CI-only worker control');
const request = JSON.parse(readFileSync(0, 'utf8'));
const endpoint = new URL(request.endpoint);
if (endpoint.protocol !== 'ws:' || endpoint.hostname !== '127.0.0.1') throw new Error('loopback only');
const socket = new WebSocket(endpoint);
let completed = false;
function finish(error, value) {
  if (completed) return;
  completed = true;
  clearTimeout(timeout);
  if (error) { console.error(JSON.stringify(error)); process.exitCode = 1; }
  else console.log(JSON.stringify(value ?? null));
  try { socket.close(); } catch {}
}
const timeout = setTimeout(() => finish({stage:'timeout',message:'extension worker reply was not received'}), 8000);
socket.addEventListener('open', () => socket.send(JSON.stringify({id: 1, method: 'Runtime.evaluate',
  params: {expression: request.expression, awaitPromise: true, returnByValue: true}})));
socket.addEventListener('message', event => {
  const reply = JSON.parse(event.data);
  if (reply.id !== 1) return;
  if (reply.error) finish({stage:'protocol',message:String(reply.error.message).slice(0,600)});
  else if (reply.result?.exceptionDetails) {
    const failure = reply.result.exceptionDetails;
    finish({stage:'expression',message:String(failure.exception?.description || failure.text).slice(0,600)});
  } else finish(null, reply.result?.result?.value);
});
socket.addEventListener('error', event => finish({stage:'socket',message:String(event.message || event.error?.message || 'worker socket failed').slice(0,600)}));
socket.addEventListener('close', () => finish({stage:'socket-closed',message:'worker target closed before its reply'}));

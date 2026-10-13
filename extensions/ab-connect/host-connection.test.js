import assert from 'node:assert/strict'
import test from 'node:test'
import { HostConnectionState } from './host-connection.js'

test('creating a Port never claims the native host is connected', () => {
  const state = new HostConnectionState()
  state.begin({})
  assert.deepEqual(state.snapshot(), {
    connected: false, connectionState: 'connecting', connectionError: null, agentPanel:null,
  })
})

test('missing native host preserves its concrete error and never confirms', () => {
  const state = new HostConnectionState()
  const port = {}
  state.begin(port)
  state.end(port, 'Specified native messaging host not found.')
  assert.deepEqual(state.snapshot(), {
    connected: false, connectionState: 'disconnected',
    connectionError: 'Specified native messaging host not found.', agentPanel:null,
  })
})

test('a pong confirms the current host once', () => {
  const state = new HostConnectionState()
  const port = {}
  state.begin(port)
  assert.equal(state.receive(port, { method: 'pong' }), true)
  assert.equal(state.receive(port, { method: 'pong' }), false)
  assert.equal(state.snapshot().connected, true)
  state.end(port, 'Native host exited')
  assert.equal(state.snapshot().connected, false)
})

test('late replies and disconnects cannot mutate a replacement connection', () => {
  const state = new HostConnectionState()
  const old = {}, current = {}
  state.begin(old)
  state.begin(current)
  assert.equal(state.receive(old, { method: 'pong' }), false)
  assert.equal(state.end(old, 'old error'), false)
  assert.equal(state.snapshot().connectionState, 'connecting')
  state.receive(current, { method: 'pong' })
  assert.equal(state.end(old, 'late old error'), false)
  assert.equal(state.snapshot().connected, true)
})

test('legacy host commands confirm the connection, malformed messages do not', () => {
  const state = new HostConnectionState(), port = {}
  state.begin(port)
  for (const msg of [null, {}, { method: 'unrecognized' }, { method: 'forwardCDPCommand' }]) {
    assert.equal(state.receive(port, msg), false)
  }
  assert.equal(state.snapshot().connected, false)
  assert.equal(state.receive(port, { method: 'forwardCDPCommand', params: { method: 'Page.getFrameTree' } }), true)
})


test('a legacy relay is connected without falsely advertising panel capabilities', () => {
 const state=new HostConnectionState(),port={};state.begin(port);state.receive(port,{method:'pong'});
 assert.equal(state.snapshot().connected,true);assert.equal(state.snapshot().agentPanel,null);
 assert.equal(state.receive(port,{method:'pong',params:{agentPanel:{schemaVersion:1,mode:'readOnly',modelInspection:false}}}),true);
 assert.equal(state.snapshot().agentPanel.mode,'readOnly');state.end(port);assert.equal(state.snapshot().agentPanel,null);
});

test('conversation deletion is advertised only when the host reports it', () => {
 const state=new HostConnectionState(),port={};state.begin(port);
 state.receive(port,{method:'pong',params:{agentPanel:{schemaVersion:1,mode:'full',modelInspection:true}}});
 assert.equal(state.snapshot().agentPanel.conversationDelete,false);
 assert.equal(state.receive(port,{method:'pong',params:{agentPanel:{schemaVersion:1,mode:'full',modelInspection:true,conversationDelete:true}}}),true);
 assert.equal(state.snapshot().agentPanel.conversationDelete,true);
});

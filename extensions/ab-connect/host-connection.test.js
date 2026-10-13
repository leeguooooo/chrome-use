import assert from 'node:assert/strict'
import test from 'node:test'
import { HostConnectionState, HostReconnectScheduler } from './host-connection.js'

function reconnectFixture() {
  let now = 0, nextId = 0, attempts = 0
  const timers = new Map(), cancelled = []
  const scheduler = new HostReconnectScheduler({
    retry: () => { attempts++ }, now: () => now,
    setTimer: (fn, delay) => { const id = nextId++; timers.set(id, { fn, due: now + delay }); return id },
    clearTimer: id => { cancelled.push(timers.get(id)); timers.delete(id) },
  })
  return { scheduler, timers, cancelled, attempts: () => attempts, now: () => now, elapse: ms => { now += ms },
    advance: ms => { now += ms; for (const [id, timer] of [...timers]) if (timer.due <= now) { timers.delete(id); timer.fn() } },
  }
}

test('a disconnect retries at one second without waiting for a browser alarm', () => {
  const f = reconnectFixture()
  f.scheduler.failed()
  f.advance(999); assert.equal(f.attempts(), 0)
  f.advance(1); assert.equal(f.attempts(), 1)
})

test('early alarms do not create duplicate retries, confirmation cancels late callbacks', () => {
  const f = reconnectFixture()
  f.scheduler.failed()
  for (let i = 0; i < 5; i++) assert.equal(f.scheduler.canAttempt(), false)
  assert.equal(f.timers.size, 1)
  f.scheduler.confirmed()
  assert.equal(f.timers.size, 0)
  f.cancelled[0].fn(); f.advance(1000)
  assert.equal(f.attempts(), 0)
})

test('failed installations back off to thirty seconds and a confirmed host resets them', () => {
  const f = reconnectFixture()
  for (const delay of [1000, 2000, 4000, 8000, 16000, 30000, 30000]) {
    f.scheduler.failed()
    const [timer] = f.timers.values()
    assert.equal(timer.due - f.now(), delay)
    f.advance(delay)
  }
  assert.equal(f.attempts(), 7)
  f.scheduler.confirmed(); f.scheduler.failed()
  f.advance(999); assert.equal(f.attempts(), 7)
  f.advance(1); assert.equal(f.attempts(), 8)
})

test('an explicit due retry invalidates the older queued callback', () => {
  const f = reconnectFixture()
  f.scheduler.failed()
  const [timer] = f.timers.values()
  f.elapse(1000)
  assert.equal(f.scheduler.canAttempt(), true)
  f.scheduler.retry()
  timer.fn()
  assert.equal(f.attempts(), 1)
})

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

test('chat adapters are validated and capability-only changes are observable', () => {
  const state = new HostConnectionState();
  const port = {};
  state.begin(port);
  const message = chatAgents => ({method:'pong',params:{agentPanel:{schemaVersion:1,mode:'full',modelInspection:true,chatAgents}}});
  assert.equal(state.receive(port,message(['codex','claude'])),true);
  assert.equal(state.receive(port,message(['codex','claude','opencode'])),true);
  assert.deepEqual(state.snapshot().agentPanel.chatAgents,['codex','claude','opencode']);
  assert.equal(state.receive(port,message(['codex',null])),true);
  assert.deepEqual(state.snapshot().agentPanel.chatAgents,['codex','claude']);
});

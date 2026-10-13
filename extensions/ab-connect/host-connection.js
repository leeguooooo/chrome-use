// Connection state is evidence-based: constructing a Port is not a handshake.
// The token is the actual Port object so late callbacks cannot confirm or clear
// a newer connection after the service worker reconnects.
export class HostConnectionState {
  constructor() {
    this.port = null
    this.confirmed = false
    this.error = null
    this.agentPanel = null
  }

  begin(port) {
    this.port = port
    this.confirmed = false
    this.error = null
    this.agentPanel = null
  }

  receive(port, message) {
    if (port !== this.port || !port || !message || typeof message !== 'object') return false
    const recognized = ['pong', 'ping', 'attachAll'].includes(message.method) ||
      (message.method === 'forwardCDPCommand' && typeof message.params?.method === 'string')
    if (!recognized) return false
    const reported = message.params?.agentPanel
    const valid = reported?.schemaVersion === 1 && ['full','readOnly'].includes(reported.mode) && typeof reported.modelInspection === 'boolean'
    const conversationDelete = reported?.conversationDelete === true
    const fullAccess = reported?.fullAccess === true
    const pageContext = reported?.pageContext === true
    const chatAgents = Array.isArray(reported?.chatAgents) && reported.chatAgents.every(id => typeof id === 'string' && /^[a-z][a-z0-9-]{0,63}$/.test(id)) ? [...new Set(reported.chatAgents)] : ['codex', 'claude']
    const changed = !this.confirmed || (valid && (reported.mode !== this.agentPanel?.mode || reported.modelInspection !== this.agentPanel?.modelInspection || conversationDelete !== this.agentPanel?.conversationDelete || fullAccess !== this.agentPanel?.fullAccess || pageContext !== this.agentPanel?.pageContext || JSON.stringify(chatAgents) !== JSON.stringify(this.agentPanel?.chatAgents)))
    if (valid) this.agentPanel = {schemaVersion:1,mode:reported.mode,modelInspection:reported.modelInspection,conversationDelete,fullAccess,pageContext,chatAgents}
    this.confirmed = true
    this.error = null
    return changed
  }

  end(port, error) {
    if (port !== this.port) return false
    this.port = null
    this.confirmed = false
    this.error = error || null
    this.agentPanel = null
    return true
  }

  snapshot() {
    return {
      connected: this.confirmed,
      connectionState: this.port ? (this.confirmed ? 'connected' : 'connecting') : 'disconnected',
      connectionError: this.error,
      agentPanel: this.agentPanel,
    }
  }
}

// A deadline alone does not retry a disconnected host. Keep one cancellable
// timer, retry promptly, and back off missing/broken installations to 30s.
// Confirmation resets the backoff; a late cancelled callback does nothing.
export class HostReconnectScheduler {
  constructor({ retry, now = Date.now, setTimer = (fn, delay) => setTimeout(fn, delay), clearTimer = id => clearTimeout(id) }) {
    this.retry = retry
    this.now = now
    this.setTimer = setTimer
    this.clearTimer = clearTimer
    this.failures = 0
    this.nextAttemptAt = 0
    this.timer = null
    this.token = null
  }

  canAttempt() {
    if (this.now() < this.nextAttemptAt) {
      this.schedule()
      return false
    }
    this.cancel()
    return true
  }

  failed() {
    const delay = Math.min(30000, 1000 * 2 ** Math.min(this.failures, 5))
    this.failures = Math.min(this.failures + 1, 6)
    this.nextAttemptAt = this.now() + delay
    this.cancel()
    this.schedule()
  }

  confirmed() {
    this.failures = 0
    this.nextAttemptAt = 0
    this.cancel()
  }

  cancel() {
    if (this.timer !== null) this.clearTimer(this.timer)
    this.timer = null
    this.token = null
  }

  schedule() {
    if (this.token !== null) return
    const token = {}
    this.token = token
    this.timer = this.setTimer(() => {
      if (this.token !== token) return
      this.timer = null
      this.token = null
      this.retry()
    }, Math.max(0, this.nextAttemptAt - this.now()))
  }
}

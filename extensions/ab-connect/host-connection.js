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
    const changed = !this.confirmed || (valid && (reported.mode !== this.agentPanel?.mode || reported.modelInspection !== this.agentPanel?.modelInspection || conversationDelete !== this.agentPanel?.conversationDelete || fullAccess !== this.agentPanel?.fullAccess))
    if (valid) this.agentPanel = {schemaVersion:1,mode:reported.mode,modelInspection:reported.modelInspection,conversationDelete,fullAccess}
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

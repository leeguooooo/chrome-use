// Connection state is evidence-based: constructing a Port is not a handshake.
// The token is the actual Port object so late callbacks cannot confirm or clear
// a newer connection after the service worker reconnects.
export class HostConnectionState {
  constructor() {
    this.port = null
    this.confirmed = false
    this.error = null
  }

  begin(port) {
    this.port = port
    this.confirmed = false
    this.error = null
  }

  receive(port, message) {
    if (port !== this.port || !port || !message || typeof message !== 'object') return false
    const recognized = ['pong', 'ping', 'attachAll'].includes(message.method) ||
      (message.method === 'forwardCDPCommand' && typeof message.params?.method === 'string')
    if (!recognized) return false
    const changed = !this.confirmed
    this.confirmed = true
    this.error = null
    return changed
  }

  end(port, error) {
    if (port !== this.port) return false
    this.port = null
    this.confirmed = false
    this.error = error || null
    return true
  }

  snapshot() {
    return {
      connected: this.confirmed,
      connectionState: this.port ? (this.confirmed ? 'connected' : 'connecting') : 'disconnected',
      connectionError: this.error,
    }
  }
}

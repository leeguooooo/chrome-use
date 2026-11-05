// Connection state is evidence-based: constructing a Port is not a handshake.
// The token is the actual Port object so late callbacks cannot confirm or clear
// a newer connection after the service worker reconnects.
import { parseCliStatus } from './version-notice.js'

// Host feature flags the panel may rely on. Boolean ones are `true`; versioned
// ones are positive integers so a later host can raise the version.
const BOOLEAN_FEATURES = ['conversationOptions','imageAttachments','heicConvert','skillLibrary','schedules','inspect','inspectResolve'];
const VERSIONED_FEATURES = ['requestRevision','turnEvidence','artifacts','recovery','recording','inspectReplies','sessionResume','projectDirectory','tabGrant','teach','handoffDone'];

export class HostConnectionState {
  constructor() {
    this.port = null
    this.confirmed = false
    this.error = null
    this.agentPanel = null
    this.cli = null
    this.pongWithoutCli = false
    this.pongs = 0
  }

  begin(port) {
    this.port = port
    this.confirmed = false
    this.error = null
    this.agentPanel = null
    this.cli = null
    // A host that puts `cli` on its pongs does so on every one, so the first
    // pong decides: one without it means a CLI from before the record.
    this.pongWithoutCli = false
    this.pongs = 0
  }

  receive(port, message) {
    if (port !== this.port || !port || !message || typeof message !== 'object') return false
    const recognized = ['pong', 'ping', 'attachAll', 'upgradeCliResult'].includes(message.method) ||
      (message.method === 'forwardCDPCommand' && typeof message.params?.method === 'string')
    if (!recognized) return false
    const reported = message.params?.agentPanel
    const valid = reported?.schemaVersion === 1 && ['full','readOnly'].includes(reported.mode) && typeof reported.modelInspection === 'boolean'
    const conversationDelete = reported?.conversationDelete === true
    const fullAccess = reported?.fullAccess === true
    const pageContext = reported?.pageContext === true
    const chatAgents = Array.isArray(reported?.chatAgents) && reported.chatAgents.every(id => typeof id === 'string' && /^[a-z][a-z0-9-]{0,63}$/.test(id)) ? [...new Set(reported.chatAgents)] : ['codex', 'claude']
    const features = Object.fromEntries([
      ...BOOLEAN_FEATURES.filter(key => reported?.[key] === true).map(key => [key, true]),
      ...VERSIONED_FEATURES.filter(key => Number.isSafeInteger(reported?.[key]) && reported[key] > 0).map(key => [key, reported[key]]),
    ]);
    const featuresChanged = [...BOOLEAN_FEATURES, ...VERSIONED_FEATURES].some(key => features[key] !== this.agentPanel?.[key]);
    const changed = (valid && featuresChanged) || !this.confirmed || (valid && (reported.mode !== this.agentPanel?.mode || reported.modelInspection !== this.agentPanel?.modelInspection || conversationDelete !== this.agentPanel?.conversationDelete || fullAccess !== this.agentPanel?.fullAccess || pageContext !== this.agentPanel?.pageContext || JSON.stringify(chatAgents) !== JSON.stringify(this.agentPanel?.chatAgents)))
    if (valid) this.agentPanel = {schemaVersion:1,mode:reported.mode,modelInspection:reported.modelInspection,conversationDelete,fullAccess,pageContext,chatAgents,...features}
    // A pong may carry the CLI's version record; a message without one keeps
    // the last (an older host never sends it, so it stays null).
    let cliChanged = false
    if (message.method === 'pong' && message.params && 'cli' in message.params) {
      const cli = parseCliStatus(message.params?.cli)
      cliChanged = JSON.stringify(cli) !== JSON.stringify(this.cli)
      this.cli = cli
    }
    if (message.method === 'pong' && this.pongs++ === 0 && !(message.params && 'cli' in message.params)) {
      this.pongWithoutCli = true
      cliChanged = true
    }
    this.confirmed = true
    this.error = null
    return changed || cliChanged
  }

  end(port, error) {
    if (port !== this.port) return false
    this.port = null
    this.confirmed = false
    this.error = error || null
    this.agentPanel = null
    this.cli = null
    this.pongWithoutCli = false
    return true
  }

  snapshot() {
    return {
      connected: this.confirmed,
      connectionState: this.port ? (this.confirmed ? 'connected' : 'connecting') : 'disconnected',
      connectionError: this.error,
      agentPanel: this.agentPanel,
      cli: this.cli,
      // Connected to a CLI too old to report its version or update itself.
      legacyCli: this.confirmed && this.pongWithoutCli,
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

  // A page showing the setup guide asks for a prompt retry: the CLI may have
  // just been installed, and a 30s backoff would leave the guide waiting.
  reset() {
    this.confirmed()
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

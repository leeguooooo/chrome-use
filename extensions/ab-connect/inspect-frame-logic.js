// Pure helpers for the Inspect & Ask input frame (unit-tested in Node).

import { t, messageMap } from './i18n.js';

export function visibilityVerdict(reply) {
  if (!reply || reply.timeout) return { ok: false, message: t('inspect_visibility_unconfirmed') };
  if (reply.isVisible === undefined) return { ok: false, message: t('inspect_visibility_unsupported') };
  if (reply.documentVisibility !== 'visible') return { ok: false, message: t('inspect_visibility_page_hidden') };
  if (reply.isVisible !== true || !(reply.ratio >= 0.99)) return { ok: false, message: t('inspect_visibility_covered') };
  return { ok: true };
}

function joinRoot(root, path) {
  const separator = root.includes('\\') && !root.includes('/') ? '\\' : '/';
  return root.replace(/[\\/]+$/, '') + separator + path.replace(/^[\\/]+/, '');
}

// Editor deep link for a source inside the bound project root.
export function editorUrl(editor, root, source) {
  if (!editor || editor === 'none' || !root || !source?.path) return null;
  if (source.path.split(/[\\/]/).includes('..')) return null;
  let relativePath = source.path;
  if (/^([A-Za-z]:|[\\/])/.test(relativePath)) {
    // Absolute marks (code-inspector-plugin 2.x emits them) open only when they
    // are inside the bound project root.
    const base = root.replace(/[\\/]+$/, '');
    const normalized = relativePath.replace(/\\/g, '/');
    if (!normalized.startsWith(base.replace(/\\/g, '/') + '/')) return null;
    relativePath = normalized.slice(base.length + 1);
  }
  const file = joinRoot(root, relativePath);
  const line = source.line || 1;
  const column = source.column || 1;
  const encoded = encodeURI(file.replace(/\\/g, '/'));
  switch (editor) {
    case 'vscode': return `vscode://file${file.startsWith('/') ? '' : '/'}${encoded}:${line}:${column}`;
    case 'cursor': return `cursor://file${file.startsWith('/') ? '' : '/'}${encoded}:${line}:${column}`;
    case 'windsurf': return `windsurf://file${file.startsWith('/') ? '' : '/'}${encoded}:${line}:${column}`;
    case 'webstorm': return `webstorm://open?file=${encodeURIComponent(file)}&line=${line}&column=${column}`;
    default: return null;
  }
}

export const LOCKED_STATES = ['sending', 'sent', 'stored', 'unknown', 'unconfirmed'];

// What the panel says after a delivery attempt. stored and wake are separate
// facts from ocs; "stored" never means the agent has read or acted on it.
export function deliveryMessage(result) {
  const state = result?.state;
  const wake = result?.wake;
  switch (state) {
    case 'sent': {
      const sent = wake ? t('inspect_delivery_sent_wake', wake) : t('inspect_delivery_sent');
      return { tone: 'ok', locked: true, text: result?.warning ? t('inspect_delivery_with_note', sent, result.warning) : sent };
    }
    case 'stored': return { tone: 'warn', locked: true, text: wake ? t('inspect_delivery_stored_wake', wake) : t('inspect_delivery_stored') };
    case 'unknown': return { tone: 'warn', locked: true, text: result?.warning ? t('inspect_delivery_unknown_warning', result.warning) : t('inspect_delivery_unknown') };
    case 'unconfirmed': return { tone: 'warn', locked: true, text: result?.error ? t('inspect_delivery_unconfirmed_error', result.error) : t('inspect_delivery_unconfirmed') };
    case 'noBinding': return { tone: 'bad', locked: false, text: t('inspect_delivery_no_binding') };
    case 'noSession': return { tone: 'bad', locked: false, text: t('inspect_delivery_no_session') };
    case 'sessionChanged': return { tone: 'bad', locked: false, text: t('inspect_delivery_session_changed') };
    case 'upgradeRequired': return { tone: 'bad', locked: false, text: t('inspect_delivery_upgrade_required') };
    case 'targetChanged': return { tone: 'bad', locked: false, text: result?.reason === 'pageChanged'
      ? t('inspect_delivery_page_changed')
      : t('inspect_delivery_target_changed') };
    case 'noOcs': return { tone: 'bad', locked: false, text: t('inspect_delivery_no_ocs') };
    case 'failed': return { tone: 'bad', locked: false, text: result?.error ? t('inspect_delivery_failed_error', result.error) : t('inspect_delivery_failed') };
    default: return { tone: 'warn', locked: true, text: t('inspect_delivery_unrecognized') };
  }
}

export function newDeliveryId() {
  return crypto.randomUUID();
}

// ---- matching (inspectResolve) labels -------------------------------------

export function scopeLabel(origin, pathScope) {
  let host = origin;
  try { host = new URL(origin).host; } catch {}
  return host + (pathScope && pathScope !== '/' ? pathScope : '');
}

const EVIDENCE = messageMap({
  config: 'inspect_evidence_config',
  'config-wildcard': 'inspect_evidence_config_wildcard',
  'dev-server-port': 'inspect_evidence_dev_server',
  'previous-binding': 'inspect_evidence_previous',
  'ocs-session': 'inspect_evidence_session_folder',
});

export function evidenceLabels(pair) {
  const out = (pair?.evidence || []).map(e => EVIDENCE[e] || e);
  if (pair?.matchedPaths) out.push(pair.matchedPaths === 1 ? t('inspect_evidence_matched_one', pair.matchedPaths) : t('inspect_evidence_matched_many', pair.matchedPaths));
  return out;
}

// Page revision is a hint; the relation is reported as git sees it locally.
export function revisionText(revision) {
  switch (revision?.relation) {
    case 'same': return t('inspect_revision_same');
    case 'ahead': return revision.ahead === 1 ? t('inspect_revision_ahead_one', revision.ahead) : t('inspect_revision_ahead_many', revision.ahead);
    case 'behind': return revision.behind === 1 ? t('inspect_revision_behind_one', revision.behind) : t('inspect_revision_behind_many', revision.behind);
    case 'diverged': return t('inspect_revision_diverged', revision.ahead, revision.behind);
    case 'unknown': return t('inspect_revision_unknown');
    default: return null;
  }
}

export function reasonText(reason, binding) {
  const who = binding?.sessionLabel || binding?.sessionId || t('inspect_reason_session_default');
  switch (reason) {
    case 'sessionEnded': return t('inspect_reason_session_ended', who);
    case 'sessionMoved': return t('inspect_reason_session_moved', who);
    case 'portMoved': return t('inspect_reason_port_moved');
    case 'rootMissing': return t('inspect_reason_root_missing');
    default: return null;
  }
}

export function targetLabel(target) {
  return `${target?.sessionLabel || target?.sessionId || t('inspect_session_fallback')} @ ${target?.root || '?'}`;
}

// A literal argument for the user's shell. Page URLs are page-controlled, so a
// copied command must never let `$(…)`, backticks, `;` or quotes run anything.
export function shellQuote(value, platform) {
  const text = String(value ?? '');
  if (platform === 'windows') return "'" + text.replace(/['\u2018\u2019\u201a\u201b]/g, m => m + m) + "'";
  return "'" + text.replace(/'/g, "'\\''") + "'";
}

export function linkCommand(url, platform) {
  return `chrome-use inspect link ${shellQuote(url, platform)}`;
}

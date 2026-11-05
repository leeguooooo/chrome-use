// Which agent drives a tab. Runs in the extension's own panel: getTargets does
// not attach a debugger and nothing reaches the page.
const AGENT_NAMES = { claude: 'Claude Code', codex: 'Codex', opencode: 'OpenCode', pi: 'Pi' };

export async function targetIdOfTab(tabId, debuggerApi) {
  if (!Number.isInteger(tabId) || !debuggerApi?.getTargets) return null;
  const targets = await debuggerApi.getTargets();
  return targets.find(target => target.tabId === tabId)?.id ?? null;
}

// The handoff card: what the agent asked for and when it carries on by
// itself. `info.handoff` is set only while the session belongs to the user.
export function handoffSummary(info, t) {
  const handoff = info?.operated ? info.handoff : null;
  if (!handoff) return null;
  const until = handoff.until, value = String(until?.value ?? '');
  const condition = { url: 'panel_handoff_until_url', text: 'panel_handoff_until_text', gone: 'panel_handoff_until_gone', change: 'panel_handoff_until_change' }[until?.kind];
  return {
    title: t('panel_handoff_title'),
    note: typeof handoff.note === 'string' ? handoff.note : '',
    condition: condition ? t(condition, value) : '',
    done: handoff.done === true,
  };
}

export function operatorSummary(info, t) {
  if (!info?.operated) return null;
  const agent = AGENT_NAMES[info.origin?.agent] || t('panel_operator_unknown_agent');
  const project = String(info.origin?.cwd || '').split('/').filter(Boolean).pop() || '';
  return {
    title: t(info.live ? 'panel_operator_live' : 'panel_operator_ended', agent),
    detail: [project, info.origin?.conversationId || info.session, ...(info.actions?.[0]?.action ? [t('panel_operator_last', info.actions[0].action)] : [])].join(' · '),
    hasContext: Boolean(info.origin?.conversationId),
  };
}

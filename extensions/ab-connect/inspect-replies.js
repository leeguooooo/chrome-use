// Agent replies to Inspect & Ask batches. The host reads them from ocs,
// redacts and caps them; here they are validated and rendered as untrusted
// agent text through the safe Markdown renderer. Every user-visible string of
// this feature is in REPLY_TEXT.

export const REPLY_TEXT = Object.freeze({
  from: from => `Reply from ${from}`,
  heading: 'Replies from agents',
  waiting: 'The agent\'s reply will appear here.',
  truncated: 'The reply was shortened; the agent session has the full text.',
  unlinked: 'Not linked to a batch',
  notificationTitle: from => `Reply from ${from}`,
});

const MAX_SHOWN = 50;

/** Replies with the shape the host promises; anything else is dropped. */
export function validReplies(result) {
  if (!Array.isArray(result?.replies)) return [];
  return result.replies.filter(r => r && typeof r.channel === 'string' && Number.isSafeInteger(r.seq) &&
    typeof r.from === 'string' && typeof r.text === 'string' && Number.isSafeInteger(r.receivedAt) &&
    (r.deliveryId === undefined || typeof r.deliveryId === 'string'));
}

export function replyKey(reply) {
  return `${reply.channel}#${reply.seq}`;
}

function firstLine(text, max) {
  const line = String(text || '').split('\n').map(s => s.trim()).find(Boolean) || '';
  return line.length > max ? `${line.slice(0, max - 1)}…` : line;
}

/** Notification fields for one reply: the sender and the first line only. */
export function replyNotice(reply) {
  return { title: firstLine(REPLY_TEXT.notificationTitle(reply.from), 80), message: firstLine(reply.text, 120) };
}

/**
 * Replace `container`'s children with the replies, newest last.
 * `renderMarkdown(document, node, text)` is markdown.js.
 */
export function renderReplies(document, container, replies, renderMarkdown, { showBatch = false } = {}) {
  const nodes = replies.slice(-MAX_SHOWN).map(reply => {
    const item = document.createElement('article');
    item.className = 'inspect-reply';
    const head = document.createElement('div');
    head.className = 'inspect-reply-head';
    const who = document.createElement('strong');
    who.textContent = REPLY_TEXT.from(reply.from);
    head.append(who);
    const when = Number.isFinite(Date.parse(reply.at)) ? new Date(reply.at) : new Date(reply.receivedAt);
    const time = document.createElement('time');
    time.textContent = when.toLocaleString();
    head.append(document.createTextNode(' '), time);
    if (showBatch && !reply.deliveryId) {
      const note = document.createElement('span');
      note.className = 'muted';
      note.textContent = ` · ${REPLY_TEXT.unlinked}`;
      head.append(note);
    }
    const body = document.createElement('div');
    body.className = 'inspect-reply-body';
    renderMarkdown(document, body, reply.text);
    item.append(head, body);
    if (reply.truncated) {
      const cut = document.createElement('p');
      cut.className = 'muted';
      cut.textContent = REPLY_TEXT.truncated;
      item.append(cut);
    }
    return item;
  });
  container.replaceChildren(...nodes);
  return nodes.length;
}

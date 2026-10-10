// A reply too large for the native-messaging port (#530).
//
// Chrome refuses one extension message over 64 MiB: `port.postMessage` throws
// ("Message length exceeded maximum allowed length."). The worker used to
// swallow that as "the port died", so the reply never arrived and the CLI
// waited out its command timeout with no explanation. Instead, the caller now
// gets an error reply that names the limit, at once.
//
// Kept free of `chrome.*` so it is unit-testable.

/** Chrome's limit on one message from an extension to its native host. */
export const NATIVE_MESSAGE_LIMIT_BYTES = 64 * 1024 * 1024;

/** The machine-readable prefix the CLI classifies (not retryable). */
export const REPLY_TOO_LARGE = 'reply_too_large';

/** Whether a postMessage failure is Chrome refusing the message's size. */
export function isMessageTooLargeError(error) {
  const text = String((error && error.message) || error || '');
  return /exceeded maximum allowed length|message (?:is )?too (?:large|long)/i.test(text);
}

/** UTF-8 byte length of a string, without allocating its encoding. */
export function utf8Length(text) {
  let bytes = 0;
  for (let i = 0; i < text.length; i++) {
    const c = text.charCodeAt(i);
    if (c < 0x80) bytes += 1;
    else if (c < 0x800) bytes += 2;
    else if (c >= 0xd800 && c <= 0xdbff && i + 1 < text.length) {
      bytes += 4;
      i++;
    } else bytes += 3;
  }
  return bytes;
}

function mib(bytes) {
  return `${(bytes / (1024 * 1024)).toFixed(1)} MiB`;
}

/** The error text the caller receives in place of the reply. */
export function replyTooLargeMessage(method, bytes) {
  const what = method ? `the reply to ${method}` : 'the reply';
  const size = Number.isFinite(bytes) ? ` is ${mib(bytes)},` : ' is';
  return (
    `${REPLY_TOO_LARGE}: ${what}${size} over Chrome's ${mib(NATIVE_MESSAGE_LIMIT_BYTES)} limit ` +
    'for one native-messaging message, so the extension cannot send it. Nothing is retried; ' +
    'ask for less (a smaller eval result, `snapshot -i` or a scoped selector, a smaller screenshot).'
  );
}

/**
 * The error reply to send when posting `msg` failed with `error`, or null when
 * there is nothing to answer (not a reply, or a failure that is not the size:
 * a dead port reconnects on its own).
 */
export function oversizeReplyError(msg, error, method) {
  if (!msg || typeof msg !== 'object' || msg.id === undefined) return null;
  if (!isMessageTooLargeError(error)) return null;
  let bytes = null;
  try {
    bytes = utf8Length(JSON.stringify(msg));
  } catch {}
  return { id: msg.id, error: replyTooLargeMessage(method, bytes) };
}

"""Bounded, case-registered loopback transport for synthetic native PNG evidence."""
import base64
import hashlib
import struct
import uuid

MAX_BODY = 128 * 1024 * 1024
MAX_PNG = 96 * 1024 * 1024


def canonical_uuid(value):
    try:
        return str(uuid.UUID(value)) == value
    except (ValueError, TypeError, AttributeError):
        return False


def save_native_image(event, registration, traces):
    if registration is None or not all(canonical_uuid(event.get(k)) for k in ("operationId", "captureId")):
        raise ValueError("unregistered case or noncanonical attempt identity")
    own = [t for t in traces if t.get("caseId") == event.get("caseId") and t.get("operationId") == event["operationId"]]
    sends = [t for t in own if t.get("type") == "capture-native-send" and t.get("captureId") == event["captureId"]]
    if len(sends) != 1 or sends[0].get("fixturePath") != "/report":
        raise ValueError("no unique registered report native send")
    sent = sends[0]
    answers = [t for t in own if t.get("type") == "capture-native-answer" and t.get("captureId") == event["captureId"]]
    if len(answers) != 1 or answers[0].get("responseOwned") is not True or answers[0].get("target") != event.get("target") or answers[0].get("epoch") != event.get("epoch"):
        raise ValueError("no matching native answer")
    if sent.get("target") != event.get("target") or sent.get("epoch") != event.get("epoch"):
        raise ValueError("native image target or epoch mismatch")
    if any(t.get("type") == "attachment-change" for t in own):
        raise ValueError("attachment changed before native image upload")
    encoded = event.get("data")
    if not isinstance(encoded, str) or len(encoded) > (MAX_PNG + 2) // 3 * 4:
        raise ValueError("encoded image exceeds diagnostic bound")
    data = base64.b64decode(encoded, validate=True)
    if len(data) > MAX_PNG or len(data) < 33 or data[:8] != b'\x89PNG\r\n\x1a\n' or data[12:16] != b'IHDR' or struct.unpack('>I', data[8:12])[0] != 13:
        raise ValueError("invalid or oversized PNG")
    width, height = struct.unpack('>II', data[16:24])
    if width < 1 or height < 1 or width * height > 200_000_000:
        raise ValueError("PNG dimensions exceed diagnostic bound")
    # The destination comes only from registration, never an untrusted path segment.
    path = registration / 'native.png'
    with path.open('xb') as stream:
        stream.write(data)
    return {"saved": True, "sha256": hashlib.sha256(data).hexdigest(), "bytes": len(data), "width": width, "height": height}

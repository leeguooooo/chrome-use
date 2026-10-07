#!/usr/bin/env python3
"""Collect a fixed-sequence run with UTF-8 bytes and explicit provenance."""
import argparse
import csv
import hashlib
import json
import os
import math
import signal
from pathlib import Path
import shlex
import shutil
import subprocess
import time
import uuid


def source_hash(root):
    # Hash tracked working-tree bytes, including unstaged edits and intent-to-add.
    paths = subprocess.check_output(["git", "ls-files", "-z"], cwd=root).split(b"\0")
    digest = hashlib.sha256()
    for encoded in sorted(filter(None, paths)):
        path = root / os.fsdecode(encoded)
        digest.update(encoded + b"\0")
        if path.is_symlink():
            digest.update(b"symlink\0" + os.fsencode(os.readlink(path)))
        elif path.is_file():
            digest.update(path.read_bytes())
        else:
            digest.update(b"missing")
        digest.update(b"\0")
    return digest.hexdigest()


def invoke(binary, args, timeout):
    started = time.monotonic()
    process = subprocess.Popen(binary + args, stdout=subprocess.PIPE,
                               stderr=subprocess.STDOUT, start_new_session=True)
    timed_out = False
    try:
        output, _ = process.communicate(timeout=timeout)
    except subprocess.TimeoutExpired:
        timed_out = True
        try:
            if os.name == "posix":
                os.killpg(process.pid, signal.SIGKILL)
            else:
                process.kill()
        except ProcessLookupError:
            pass
        output, _ = process.communicate()
    return (time.monotonic() - started) * 1000, output, 124 if timed_out else process.returncode, timed_out


def safe_metadata(value):
    if any(character in str(value) for character in ("\n", "\r", "\t", "\0")):
        raise ValueError("metadata values must not contain newline, tab or NUL")
    return value


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("task", type=Path)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--binary", default=os.environ.get("CHROME_USE_BIN", "chrome-use"))
    parser.add_argument("--run-id", default=str(uuid.uuid4()))
    parser.add_argument("--timeout", type=float, default=60, help="seconds per CLI invocation, including warmup and assertions")
    parser.add_argument("--cold", action="store_true", help="skip warmup; does not stop an existing daemon")
    args = parser.parse_args()
    if not math.isfinite(args.timeout) or args.timeout <= 0:
        parser.error("timeout must be finite and positive")
    try:
        for value in (args.run_id, str(args.task), args.binary):
            safe_metadata(value)
    except ValueError as exc:
        parser.error(str(exc))
    binary = shlex.split(args.binary)
    if not binary or not shutil.which(binary[0]):
        parser.error("binary not found")
    binary[0] = str(Path(shutil.which(binary[0])).resolve())
    try:
        safe_metadata(binary[0])
    except ValueError as exc:
        parser.error(str(exc))
    commands, checks = [], []
    for line in args.task.read_text(encoding="utf-8").splitlines():
        if line.startswith("#! assert "):
            checks.append(line[len("#! assert "):])
        elif line.startswith("#!"):
            parser.error(f"unsupported directive: {line}")
        elif line.strip() and not line.startswith("#"):
            shlex.split(line)  # Validate before executing any action.
            commands.append(line)
    for check in checks:
        shlex.split(check)
    root = Path(__file__).resolve().parent.parent
    _, version, _, version_timed_out = invoke(binary, ["--version"], args.timeout)
    warm_rc = None
    if not args.cold:
        _, _, warm_rc, _ = invoke(binary, ["eval", "1+1"], args.timeout)
    meta = {
        "run_id": args.run_id, "task": str(args.task), "binary": binary[0],
        "version": version.decode("utf-8", errors="replace").strip().replace("\n", "\\n").replace("\r", "\\r").replace("\t", "\\t"),
        "binary_sha256": hashlib.sha256(Path(binary[0]).read_bytes()).hexdigest(),
        "source_sha256": source_hash(root), "binary_source_verified": "false",
        "cli_timeout_seconds": args.timeout, "version_timed_out": str(version_timed_out).lower(), "bytes_encoding": "utf-8",
        "warmup_cli_calls": 0 if args.cold else 1,
        "temperature": "cold_requested_unverified" if args.cold else ("warmup_succeeded" if warm_rc == 0 else "warmup_failed"),
    }
    args.output.parent.mkdir(parents=True, exist_ok=True)
    # Exclusive creation avoids replacing evidence from an earlier run.
    with args.output.open("x", encoding="utf-8", newline="") as handle:
        for key, value in meta.items():
            handle.write(f"# {key}\t{safe_metadata(value)}\n")
        writer = csv.writer(handle, delimiter="\t", lineterminator="\n")
        writer.writerow(["n", "ms", "bytes", "rc", "cmd", "unknown", "timed_out"])
        started = time.monotonic()
        for index, command in enumerate(commands, 1):
            elapsed, output, rc, timed_out = invoke(binary, shlex.split(command), args.timeout)
            unknown = None
            try:
                payload = json.loads(output)
                # A classified machine response, never a substring in page text.
                unknown = classify_outcome(payload) if not timed_out else None
            except (ValueError, UnicodeDecodeError):
                pass
            writer.writerow([index, f"{elapsed:.3f}", len(output), rc, command, "unclassified" if unknown is None else str(unknown).lower(), str(timed_out).lower()])
            handle.flush()
            if timed_out:
                handle.write("# stopped_after_timeout\ttrue\n")
                break
        passed = []
        for check in checks:
            _, _, rc, _ = invoke(binary, ["expect"] + shlex.split(check), args.timeout)
            passed.append(rc == 0)
            handle.write(f"# assert\t{'pass' if rc == 0 else 'FAIL'}\t{check}\n")
        verdict = "pass" if passed and all(passed) else ("FAIL" if passed else "none")
        handle.write(f"# verdict\t{verdict}\n")
        handle.write(f"# task_wall_ms\t{(time.monotonic() - started) * 1000:.3f}\n")
    print(args.output)
    return 0 if passed and all(passed) else 1


def classify_outcome(payload):
    """True means unknown was reported; None means no complete classification."""
    if isinstance(payload, list):
        states = [classify_outcome(item) for item in payload]
        return True if True in states else (False if states and all(state is False for state in states) else None)
    if not isinstance(payload, dict) or not isinstance(payload.get("success"), bool):
        return None
    if contains_unknown(payload):
        return True
    for key in ("data", "result"):
        nested = payload.get(key)
        if isinstance(nested, list) and classify_outcome(nested) is None:
            return None
    return False


def contains_unknown(payload, error_envelope=False):
    if isinstance(payload, list):
        return any(classify_outcome(item) is True for item in payload)
    if not isinstance(payload, dict):
        return False
    envelope = error_envelope or isinstance(payload.get("success"), bool)
    reported = envelope and any(payload.get(key) in ("outcome_unknown", "action_outcome_unknown")
                                for key in ("status", "code", "error_code"))
    return reported or contains_unknown(payload.get("error"), error_envelope=True) or any(
        contains_unknown(payload.get(key)) for key in ("data", "result")
    )


if __name__ == "__main__":
    raise SystemExit(main())

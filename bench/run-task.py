#!/usr/bin/env python3
"""Collect a fixed-sequence run with UTF-8 bytes and explicit provenance."""
import argparse
import csv
import hashlib
import json
import os
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


def invoke(binary, args):
    started = time.monotonic()
    completed = subprocess.run(binary + args, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
    # Count the actual transport bytes, including trailing newlines.
    return (time.monotonic() - started) * 1000, completed.stdout, completed.returncode


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("task", type=Path)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--binary", default=os.environ.get("CHROME_USE_BIN", "chrome-use"))
    parser.add_argument("--run-id", default=str(uuid.uuid4()))
    parser.add_argument("--cold", action="store_true", help="skip warmup; does not stop an existing daemon")
    args = parser.parse_args()
    binary = shlex.split(args.binary)
    if not binary or not shutil.which(binary[0]):
        parser.error("binary not found")
    binary[0] = str(Path(shutil.which(binary[0])).resolve())
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
    _, version, _ = invoke(binary, ["--version"])
    warm_rc = None
    if not args.cold:
        _, _, warm_rc = invoke(binary, ["eval", "1+1"])
    meta = {
        "run_id": args.run_id, "task": str(args.task), "binary": binary[0],
        "version": version.decode("utf-8", errors="replace").strip(),
        "binary_sha256": hashlib.sha256(Path(binary[0]).read_bytes()).hexdigest(),
        "source_sha256": source_hash(root), "bytes_encoding": "utf-8",
        "warmup_cli_calls": 0 if args.cold else 1,
        "temperature": "cold_requested_unverified" if args.cold else ("warmup_succeeded" if warm_rc == 0 else "warmup_failed"),
    }
    args.output.parent.mkdir(parents=True, exist_ok=True)
    # Exclusive creation avoids replacing evidence from an earlier run.
    with args.output.open("x", encoding="utf-8", newline="") as handle:
        for key, value in meta.items():
            handle.write(f"# {key}\t{value}\n")
        writer = csv.writer(handle, delimiter="\t", lineterminator="\n")
        writer.writerow(["n", "ms", "bytes", "rc", "cmd", "unknown"])
        started = time.monotonic()
        for index, command in enumerate(commands, 1):
            elapsed, output, rc = invoke(binary, shlex.split(command))
            unknown = None
            try:
                payload = json.loads(output)
                # A classified machine response, never a substring in page text.
                unknown = contains_unknown(payload) if isinstance(payload, dict) and isinstance(payload.get("success"), bool) else None
            except (ValueError, UnicodeDecodeError):
                pass
            writer.writerow([index, f"{elapsed:.3f}", len(output), rc, command, "unclassified" if unknown is None else str(unknown).lower()])
            handle.flush()
        passed = []
        for check in checks:
            _, _, rc = invoke(binary, ["expect"] + shlex.split(check))
            passed.append(rc == 0)
            handle.write(f"# assert\t{'pass' if rc == 0 else 'FAIL'}\t{check}\n")
        verdict = "pass" if passed and all(passed) else ("FAIL" if passed else "none")
        handle.write(f"# verdict\t{verdict}\n")
        handle.write(f"# task_wall_ms\t{(time.monotonic() - started) * 1000:.3f}\n")
    print(args.output)
    return 0 if passed and all(passed) else 1


def contains_unknown(payload):
    if not isinstance(payload, dict):
        return False
    # Only protocol status fields are inspected. Arbitrary content is excluded.
    return any(payload.get(key) in ("outcome_unknown", "action_outcome_unknown") for key in ("status", "code", "error_code")) or any(
        contains_unknown(payload.get(key)) for key in ("error", "data", "result")
    )


if __name__ == "__main__":
    raise SystemExit(main())

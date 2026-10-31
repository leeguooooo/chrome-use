#!/usr/bin/env python3
"""Capture Windows taskkill diagnostics on disposable Chrome, without Cargo."""
import argparse
import json
import os
from pathlib import Path
import platform
import re
import subprocess
import sys
import tempfile
import time

from probe import digest, file_version


def processes():
    command = "Get-CimInstance Win32_Process | Select-Object ProcessId,ParentProcessId,Name,CreationDate | ConvertTo-Json -Compress"
    result = subprocess.run(["powershell.exe", "-NoProfile", "-NonInteractive", "-Command", command],
        capture_output=True, text=True, timeout=15, check=True)
    data = json.loads(result.stdout)
    return data if isinstance(data, list) else [data]


def creation_time(process):
    match = re.fullmatch(r"/Date\((-?\d+)(?:[+-]\d{4})?\)/", str(process.get("CreationDate")))
    if not match:
        raise ValueError("process creation time was not confirmed")
    return int(match[1])


def descendants(snapshot, root, ancestors=()):
    # Windows retains a numeric parent PID after that parent exits. A newer
    # process reusing the PID cannot be the parent of an older process.
    parents = {p["ProcessId"]: p for p in [*ancestors, *snapshot]}
    if root not in parents:
        raise ValueError("owned Chrome parent identity is absent from the snapshot")
    creation_time(parents[root])
    selected = {root}
    while True:
        expanded = selected | {p["ProcessId"] for p in snapshot
            if p["ParentProcessId"] in selected
            and creation_time(p) >= creation_time(parents[p["ParentProcessId"]])}
        if expanded == selected:
            return [p for p in snapshot if p["ProcessId"] in selected]
        selected = expanded


def diagnostic_pass(records):
    return len(records) == 3 and all(
        not r.get("error") and not any(key.endswith("CleanupError") for key in r)
        and type(r.get("parentExit")) is int
        and type(r.get("taskkill", {}).get("exit")) is int and r["taskkill"]["exit"] == 0
        and isinstance(r["taskkill"].get("stdout"), str) and isinstance(r["taskkill"].get("stderr"), str)
        and r.get("observedSurvivors") == [] and r.get("postTree") == []
        for r in records)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--chrome", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    if sys.platform != "win32" or os.environ.get("GITHUB_ACTIONS") != "true":
        parser.error("cleanup probe requires an ephemeral Windows GitHub Actions host")
    args.chrome = args.chrome.resolve()
    if not args.chrome.is_file():
        parser.error("explicit Chrome for Testing executable must exist")
    args.output.mkdir(parents=True, exist_ok=False)
    records = []
    metadata = {"gitSha": os.environ.get("GITHUB_SHA"), "runId": os.environ.get("GITHUB_RUN_ID"),
        "os": platform.platform(), "chromeSha256": digest(args.chrome), "chromeFileVersion": file_version(args.chrome),
        "scope": "fresh about:blank Chrome profiles on disposable CI; process snapshots are diagnostic, not proof of complete cleanup"}
    for index in range(3):
        profile = tempfile.mkdtemp(prefix="cu-cleanup-")
        record = {"sample": index, "profile": profile}
        with (args.output / (str(index) + "-chrome.log")).open("w") as log:
            chrome = subprocess.Popen([str(args.chrome), "--no-first-run", "--no-default-browser-check",
                "--user-data-dir=" + profile, "about:blank"], stdout=log, stderr=log)
            record["chromePid"] = chrome.pid
            try:
                time.sleep(2)  # Fixture startup only; no website or production automation.
                before = descendants(processes(), chrome.pid)
                record["before"] = before
                result = subprocess.run(["taskkill", "/PID", str(chrome.pid), "/T", "/F"],
                    capture_output=True, text=True, timeout=20)
                record["taskkill"] = {"exit": result.returncode, "stdout": result.stdout, "stderr": result.stderr}
                record["parentExit"] = chrome.wait(timeout=20)
                after = processes()
                identities = {(p["ProcessId"], p["CreationDate"]) for p in before}
                record["observedSurvivors"] = [p for p in after if (p["ProcessId"], p["CreationDate"]) in identities]
                record["postTree"] = descendants(after, chrome.pid, before)
            except Exception as error:
                record["error"] = str(error)
            finally:
                if chrome.poll() is None:
                    try:
                        chrome.kill()  # Exact owned handle, never a global Chrome kill.
                        chrome.wait(timeout=10)
                    except Exception as error:
                        record["fallbackCleanupError"] = str(error)
            records.append(record)
            (args.output / "result.json").write_text(json.dumps({"metadata": metadata, "records": records}, indent=2))
            if record.get("error") or record.get("observedSurvivors") or record.get("postTree"):
                break
    return 0 if diagnostic_pass(records) else 1


if __name__ == "__main__":
    raise SystemExit(main())

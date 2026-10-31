#!/usr/bin/env python3
"""Capture Windows taskkill diagnostics on disposable Chrome, without Cargo."""
import argparse
import base64
from contextlib import ExitStack
import hashlib
import json
import os
from pathlib import Path
import platform
import shutil
import subprocess
import sys
import tempfile
import time

from probe import Registry, digest, file_version, tree_digest, wait_for
from owned_processes import OwnedProcessHandles, cleanup_acknowledged, descendants, processes


def diagnostic_pass(records, relay=False):
    return len(records) == 3 and all(
        not r.get("error") and not any(key.endswith("CleanupError") for key in r)
        and (not relay or r.get("relayReady") is True)
        and type(r.get("parentExit")) is int
        and r.get("cleanupAcknowledged") is True
        and cleanup_acknowledged(r.get("taskkill", {}), r.get("before", []), r.get("ownedHandleExits", []), r.get("parentExit"), r.get("postTree"))
        and isinstance(r["taskkill"].get("stdout"), str) and isinstance(r["taskkill"].get("stderr"), str)
        and r.get("observedSurvivors") == [] and r.get("postTree") == []
        for r in records)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--chrome", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--binary", type=Path)
    parser.add_argument("--extension", type=Path)
    args = parser.parse_args()
    if sys.platform != "win32" or os.environ.get("GITHUB_ACTIONS") != "true":
        parser.error("cleanup probe requires an ephemeral Windows GitHub Actions host")
    if bool(args.binary) != bool(args.extension):
        parser.error("relay control requires both --binary and --extension")
    if args.binary:
        args.binary, args.extension = args.binary.resolve(), args.extension.resolve()
        if not args.binary.is_file() or not (args.extension / "manifest.json").is_file():
            parser.error("relay control needs an explicit reference binary and source extension")
    args.chrome = args.chrome.resolve()
    if not args.chrome.is_file():
        parser.error("explicit Chrome for Testing executable must exist")
    args.output.mkdir(parents=True, exist_ok=False)
    records = []
    metadata = {"gitSha": os.environ.get("GITHUB_SHA"), "runId": os.environ.get("GITHUB_RUN_ID"),
        "os": platform.platform(), "chromeSha256": digest(args.chrome), "chromeFileVersion": file_version(args.chrome),
        "scope": "fresh about:blank Chrome profiles on disposable CI; process snapshots are diagnostic, not proof of complete cleanup"}
    metadata["connectionMode"] = "relay" if args.binary else "plain"
    if args.binary:
        metadata["sourceExtensionSha256"] = tree_digest(args.extension)
        metadata["referenceBinarySha256"] = digest(args.binary)
        metadata["referenceCliVersion"] = subprocess.check_output([str(args.binary), "--version"],
            env=dict(os.environ, CHROME_USE_NO_UPDATE_CHECK="1"), text=True, timeout=10).strip()
        metadata["referenceBoundary"] = "released CLI process-lifecycle control, not source screenshot acceptance or performance"
    for index in range(3):
        work = Path(tempfile.mkdtemp(prefix="cu-cleanup-"))
        profile = work / "profile"
        profile.mkdir()
        record = {"sample": index, "profile": str(profile), "relayControl": bool(args.binary)}
        with ExitStack() as custody, (args.output / (str(index) + "-chrome.log")).open("w") as log:
            argv = [str(args.chrome), "--no-first-run", "--no-default-browser-check", "--user-data-dir=" + str(profile)]
            env = dict(os.environ, CHROME_USE_NO_UPDATE_CHECK="1", AGENT_BROWSER_AUTO_LOGIN="off")
            if args.binary:
                extension = work / "extension"
                shutil.copytree(args.extension, extension, ignore=shutil.ignore_patterns("*.test.js", ".DS_Store"))
                manifest = json.loads((extension / "manifest.json").read_text())
                extension_id = "".join(chr(97 + int(ch, 16)) for ch in hashlib.sha256(
                    base64.b64decode(manifest["key"])).hexdigest()[:32])
                manifest["host_permissions"] = ["http://127.0.0.1/*"]
                (extension / "manifest.json").write_text(json.dumps(manifest))
                record["loadedExtensionSha256"] = tree_digest(extension)
                custody.enter_context(Registry(args.binary, work, extension_id))
                relay = work / "relay"
                relay.mkdir()
                env.update(CHROME_USE_RELAY_DIR=str(relay), AGENT_BROWSER_SOCKET_DIR=str(work / "socket"))
                argv += ["--load-extension=" + str(extension), "--disable-extensions-except=" + str(extension)]
            chrome = subprocess.Popen(argv + ["about:blank"], env=env, stdout=log, stderr=log)
            record["chromePid"] = chrome.pid
            bound = None
            try:
                if args.binary:
                    wait_for(lambda: list(relay.glob("relay-cdp-url-*")), lambda files: len(files) == 1, 30)
                    record["relayReady"] = True
                time.sleep(2)  # Fixture startup only; no website or production automation.
                before = descendants(processes(), chrome.pid)
                record["before"] = before
                bound = OwnedProcessHandles(before)
                result = subprocess.run(["taskkill", "/PID", str(chrome.pid), "/T", "/F"],
                    capture_output=True, text=True, timeout=20)
                record["taskkill"] = {"exit": result.returncode, "stdout": result.stdout, "stderr": result.stderr}
                record["parentExit"] = chrome.wait(timeout=20)
                record["ownedHandleExits"] = bound.observe()
                bound.close()
                after = processes()
                identities = {(p["ProcessId"], p["CreationDate"]) for p in before}
                record["observedSurvivors"] = [p for p in after if (p["ProcessId"], p["CreationDate"]) in identities]
                record["postTree"] = descendants(after, chrome.pid, before)
                record["cleanupAcknowledged"] = cleanup_acknowledged(record["taskkill"], before, record["ownedHandleExits"], record["parentExit"], record["postTree"])
            except Exception as error:
                record["error"] = str(error)
            finally:
                if bound:
                    bound.close()
                if chrome.poll() is None:
                    try:
                        chrome.kill()  # Exact owned handle, never a global Chrome kill.
                        chrome.wait(timeout=10)
                    except Exception as error:
                        record["fallbackCleanupError"] = str(error)
            records.append(record)
            (args.output / "result.json").write_text(json.dumps({"metadata": metadata, "records": records}, indent=2))
            if record.get("error") or record.get("cleanupAcknowledged") is not True or record.get("observedSurvivors") or record.get("postTree"):
                break
    return 0 if diagnostic_pass(records, relay=bool(args.binary)) else 1


if __name__ == "__main__":
    raise SystemExit(main())

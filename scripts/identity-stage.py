#!/usr/bin/env python3
"""Stage an exact GNU release product and its GUI-free metadata test binary."""
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tarfile


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def stage(messages, destination):
    products = set()
    tests = {name: set() for name in ("windows_build_identity", "windows_tcp_owner")}
    for line in messages.read_text().splitlines():
        event = json.loads(line)
        if event.get("reason") != "compiler-artifact" or not event.get("executable"):
            continue
        target = event["target"]
        if target["name"] == "chrome-use" and target["kind"] == ["bin"] and not event["profile"]["test"]:
            products.add(Path(event["executable"]))
        if target["name"] in tests and target["kind"] == ["test"]:
            tests[target["name"]].add(Path(event["executable"]))
    if len(products) != 1 or any(len(paths) != 1 for paths in tests.values()):
        raise ValueError("Expected exactly one product and each metadata test artifact")
    product = next(iter(products))
    tests = {name: next(iter(paths)) for name, paths in tests.items()}
    if product.parent.name != "release" or product.suffix != ".exe":
        raise ValueError("Expected actual Windows release executable")
    destination.mkdir(parents=True, exist_ok=False)
    archive = destination / "product.tar.gz"
    with tarfile.open(archive, "w:gz") as output:
        output.add(product, arcname="chrome-use.exe")
    for name, path in tests.items():
        shutil.copy2(path, destination / f"{name}.exe")
    generation = bytes.fromhex(os.environ["CHROME_USE_BUILD_GENERATION"])
    manifests = []
    for path in (product.parent / "build").glob("chrome-use-*/out/cufg_marker_manifest.json"):
        manifest = json.loads(path.read_text())
        if bytes(manifest["marker"][-16:]) == generation:
            manifests.append(manifest)
    if not manifests or any(item != manifests[0] for item in manifests):
        raise ValueError("Missing or ambiguous exact-generation build manifest")
    receipt = {
        "schema": 2,
        "source": subprocess.check_output(["git", "rev-parse", "HEAD"], text=True).strip(),
        "productSha256": digest(product),
        "testsSha256": {name: digest(path) for name, path in tests.items()},
        "archiveSha256": digest(archive),
        "build": manifests[0],
    }
    (destination / "receipt.json").write_text(json.dumps(receipt, indent=2) + "\n")


if __name__ == "__main__":
    stage(Path(sys.argv[1]), Path(sys.argv[2]))

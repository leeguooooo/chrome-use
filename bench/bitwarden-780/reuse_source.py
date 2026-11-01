#!/usr/bin/env python3
"""Reuse only a verified same-product-tree source build for controller edits."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile

PRODUCT_PATHS = [
    ".",
    ":(exclude)bench/bitwarden-780/**",
    ":(exclude).github/workflows/windows-capture-probe.yml",
    ":(exclude)probe-artifacts/**",
]
BUILD_NAME = "Validate inputs and build the exact Release CLI"


def command(words, cwd=None):
    return subprocess.check_output(words, cwd=cwd, text=True, encoding="utf-8").strip()


def same_product_tree(repository, source):
    result = subprocess.run(
        ["git", "diff", "--quiet", source, "--", *PRODUCT_PATHS], cwd=repository
    )
    if result.returncode not in (0, 1):
        raise RuntimeError("source comparison could not be completed")
    untracked = command(
        ["git", "ls-files", "--others", "--exclude-standard", "--", *PRODUCT_PATHS],
        repository,
    )
    return result.returncode == 0 and not untracked


def verify_binary(binary, receipt):
    hashes = re.findall(
        r"^Hash\s*:\s*([a-fA-F0-9]{64})\s*$",
        receipt.read_text(encoding="utf-8-sig"),
        re.MULTILINE,
    )
    if len(hashes) != 1:
        raise ValueError("source build receipt must contain exactly one binary hash")
    digest = hashlib.sha256()
    with binary.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    actual = digest.hexdigest()
    if actual != hashes[0].lower():
        raise ValueError("source binary checksum does not match its build receipt")
    return actual


def api(repository, path):
    return json.loads(command(["gh", "api", f"repos/{repository}/{path}"]))


def main():
    if os.environ.get("GITHUB_ACTIONS") != "true":
        raise SystemExit("source replay is CI-only")
    parser = argparse.ArgumentParser()
    parser.add_argument("--run", required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    if not re.fullmatch(r"[0-9]+", args.run):
        raise ValueError("source build run must be a numeric Actions run ID")
    repository = os.environ["GITHUB_REPOSITORY"]
    run = api(repository, f"actions/runs/{args.run}")
    source = run.get("head_sha", "")
    if (
        run.get("status") != "completed"
        or run.get("event") != "workflow_dispatch"
        or run.get("path") != ".github/workflows/windows-capture-probe.yml"
        or (run.get("head_repository") or {}).get("full_name", "").lower()
        != repository.lower()
        or not re.fullmatch(r"[a-f0-9]{40}", source)
    ):
        raise ValueError(
            "source build is not a completed dispatch of this repository workflow"
        )
    steps = [
        step
        for job in api(repository, f"actions/runs/{args.run}/jobs?per_page=100")["jobs"]
        for step in job.get("steps", [])
    ]
    if not any(
        step["name"] == BUILD_NAME and step.get("conclusion") == "success"
        for step in steps
    ):
        raise ValueError("source CLI build/select step did not succeed")
    if subprocess.run(
        ["git", "cat-file", "-e", source + "^{commit}"], capture_output=True
    ).returncode:
        subprocess.run(["git", "fetch", "--no-tags", "origin", source], check=True)
    if not same_product_tree(Path.cwd(), source):
        raise ValueError(
            "product sources or untracked product files differ; a fresh build is required"
        )
    artifacts = api(repository, f"actions/runs/{args.run}/artifacts?per_page=100")[
        "artifacts"
    ]
    candidates = [
        item
        for item in artifacts
        if not item.get("expired")
        and re.fullmatch(
            "windows-capture-" + source + r"-bitwarden-dpr[12]", item["name"]
        )
    ]
    if len(candidates) != 1:
        raise ValueError("source diagnostic artifact is absent or ambiguous")
    args.output.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="verified-source-") as temporary:
        root = Path(temporary)
        subprocess.run(
            [
                "gh",
                "run",
                "download",
                args.run,
                "--repo",
                repository,
                "--name",
                candidates[0]["name"],
                "--dir",
                str(root),
            ],
            check=True,
        )
        manifest_path = root / "source-build.json"
        if manifest_path.exists():
            manifest = json.loads(manifest_path.read_text(encoding="utf-8-sig"))
            if manifest.get("mode") != "built" or manifest.get("gitSha") != source:
                raise ValueError(
                    "replays cannot be chained; choose the original fresh build"
                )
        # Legacy manifestless artifacts still passed the mandatory successful
        # fresh-build step gate above before any download was admitted.
        binary = root / "source-cli.exe"
        sha = verify_binary(binary, root / "binary-sha256.txt")
        if manifest_path.exists() and manifest.get("binarySha256", "").lower() != sha:
            raise ValueError("fresh build manifest and binary receipt disagree")
        destination = (args.output / "source-cli.exe").resolve()
        shutil.copy2(binary, destination)
        shutil.copy2(root / "binary-sha256.txt", args.output / "binary-sha256.txt")
        proof = {
            "mode": "replayed",
            "gitSha": source,
            "sourceBuildRun": args.run,
            "probeGitSha": os.environ["GITHUB_SHA"],
            "binarySha256": sha,
            "trackedProductTreeIdentical": True,
            "untrackedProductFiles": False,
            "excludedControllerPaths": PRODUCT_PATHS[1:],
        }
        (args.output / "source-build.json").write_text(
            json.dumps(proof, indent=2), encoding="utf-8"
        )
        with open(os.environ["GITHUB_ENV"], "a", encoding="utf-8") as environment:
            environment.write(
                f"PROBE_BINARY={destination}\nPROBE_BINARY_BUILD_SHA={source}\nPROBE_BINARY_SOURCE=verified-replay\n"
            )
        print(json.dumps(proof))


if __name__ == "__main__":
    main()

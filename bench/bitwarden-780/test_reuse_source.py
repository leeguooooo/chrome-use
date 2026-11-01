#!/usr/bin/env python3
import hashlib
from pathlib import Path
import subprocess
import tempfile
import unittest
from reuse_source import same_product_tree, verify_binary


class SourceProofTests(unittest.TestCase):
    def test_only_controller_and_artifact_changes_can_reuse_a_build(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)

            def git(*words):
                return subprocess.check_output(
                    ["git", "-C", str(root), *words], text=True
                ).strip()

            git("init", "-q")
            git("config", "user.name", "Fixture")
            git("config", "user.email", "fixture@example.invalid")
            files = [
                "cli/src/main.rs",
                "skill-data/core/SKILL.md",
                "extensions/ab-connect/background.js",
                "package.json",
                "bench/bitwarden-780/probe.py",
                ".github/workflows/windows-capture-probe.yml",
            ]
            for name in files:
                path = root / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text("original\n")
            git("add", ".")
            git("commit", "-qm", "fixture")
            source = git("rev-parse", "HEAD")
            (root / "bench/bitwarden-780/probe.py").write_text("controller change\n")
            (root / ".github/workflows/windows-capture-probe.yml").write_text(
                "workflow change\n"
            )
            (root / "probe-artifacts").mkdir()
            (root / "probe-artifacts/log.txt").write_text("artifact")
            self.assertTrue(same_product_tree(root, source))
            for name in files[:4]:
                path = root / name
                path.write_text("product change\n")
                self.assertFalse(same_product_tree(root, source), name)
                path.write_text("original\n")
            path = root / "cli/src/new.rs"
            path.write_text("untracked product source")
            self.assertFalse(same_product_tree(root, source))
            path.unlink()
            self.assertTrue(same_product_tree(root, source))

    def test_binary_receipt_must_match_the_downloaded_bytes(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            binary = root / "source-cli.exe"
            binary.write_bytes(b"fixture artifact")
            sha = hashlib.sha256(binary.read_bytes()).hexdigest()
            receipt = root / "binary-sha256.txt"
            receipt.write_text("Hash      : " + sha.upper() + "\n")
            self.assertEqual(verify_binary(binary, receipt), sha)
            binary.write_bytes(b"changed artifact")
            with self.assertRaisesRegex(ValueError, "checksum"):
                verify_binary(binary, receipt)
            receipt.write_text("missing hash")
            with self.assertRaisesRegex(ValueError, "exactly one"):
                verify_binary(binary, receipt)


class BuildEligibilityTests(unittest.TestCase):
    def replay(self, build_status, chained=False, built=False):
        import contextlib
        import io
        import json
        import os
        import sys
        from types import SimpleNamespace
        from unittest import mock
        import reuse_source

        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            output = root / "output"
            source = "a" * 40
            downloads = []

            def api(repository, path):
                self.assertEqual(repository, "fixture/repo")
                if path.endswith("/jobs?per_page=100"):
                    return {
                        "jobs": [
                            {
                                "steps": [
                                    {
                                        "name": reuse_source.BUILD_NAME,
                                        "conclusion": build_status,
                                    }
                                ]
                            }
                        ]
                    }
                if path.endswith("/artifacts?per_page=100"):
                    return {
                        "artifacts": [
                            {
                                "expired": False,
                                "name": "windows-capture-" + source + "-bitwarden-dpr1",
                            }
                        ]
                    }
                return {
                    "head_sha": source,
                    "status": "completed",
                    "event": "workflow_dispatch",
                    "path": ".github/workflows/windows-capture-probe.yml",
                    "head_repository": {"full_name": "fixture/repo"},
                }

            def run(words, **kwargs):
                if words[:3] == ["git", "cat-file", "-e"]:
                    return SimpleNamespace(returncode=0)
                self.assertEqual(words[:3], ["gh", "run", "download"])
                downloads.append(True)
                destination = Path(words[words.index("--dir") + 1])
                payload = b"fixture source binary"
                sha = hashlib.sha256(payload).hexdigest()
                (destination / "source-cli.exe").write_bytes(payload)
                (destination / "binary-sha256.txt").write_text("Hash : " + sha + "\n")
                if chained or built:
                    (destination / "source-build.json").write_text(
                        json.dumps(
                            {
                                "mode": "replayed" if chained else "built",
                                "gitSha": source,
                                "binarySha256": sha,
                            }
                        )
                    )
                return SimpleNamespace(returncode=0)

            environment = {
                "GITHUB_ACTIONS": "true",
                "GITHUB_REPOSITORY": "fixture/repo",
                "GITHUB_SHA": "b" * 40,
                "GITHUB_ENV": str(root / "environment"),
            }
            with (
                mock.patch.dict(os.environ, environment),
                mock.patch.object(
                    sys,
                    "argv",
                    ["reuse-source", "--run", "12", "--output", str(output)],
                ),
                mock.patch.object(reuse_source, "api", side_effect=api),
                mock.patch.object(reuse_source, "same_product_tree", return_value=True),
                mock.patch.object(reuse_source.subprocess, "run", side_effect=run),
                contextlib.redirect_stdout(io.StringIO()),
            ):
                if build_status != "success" or chained:
                    with self.assertRaisesRegex(
                        ValueError,
                        "did not succeed" if not chained else "cannot be chained",
                    ):
                        reuse_source.main()
                else:
                    reuse_source.main()
                    self.assertEqual(
                        (output / "source-cli.exe").read_bytes(),
                        b"fixture source binary",
                    )
                    self.assertEqual(
                        json.loads((output / "source-build.json").read_text())[
                            "gitSha"
                        ],
                        source,
                    )
            return downloads

    def test_manifestless_artifact_requires_a_successful_fresh_build(self):
        self.assertEqual(self.replay("success"), [True])
        self.assertEqual(self.replay("failure"), [])
        self.assertEqual(self.replay("skipped"), [])

    def test_a_successful_fresh_built_manifest_can_be_replayed(self):
        self.assertEqual(self.replay("success", built=True), [True])

    def test_a_replayed_manifest_cannot_be_chained(self):
        self.assertEqual(self.replay("success", True), [True])


if __name__ == "__main__":
    unittest.main()

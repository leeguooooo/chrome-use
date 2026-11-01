#!/usr/bin/env python3
import ast
from pathlib import Path
from types import SimpleNamespace
import subprocess
import re
import unittest


def load_reader(reader):
    tree = ast.parse(Path(__file__).with_name("probe.py").read_text())
    function = next(
        node
        for node in tree.body
        if isinstance(node, ast.FunctionDef) and node.name == "prepare_process_observer"
    )
    namespace = {
        "processes": reader,
        "os": SimpleNamespace(getpid=lambda: 42),
        "subprocess": subprocess,
        "time": SimpleNamespace(sleep=lambda delay: None),
        "re": re,
    }
    legacy = ast.parse(
        (
            Path(__file__).parents[1] / "windows-capture-798/owned_processes.py"
        ).read_text()
    )
    topology = [
        node
        for node in legacy.body
        if isinstance(node, ast.FunctionDef)
        and node.name in ["creation_time", "descendants"]
    ]
    exec(
        compile(
            ast.Module(body=topology, type_ignores=[]),
            "<actual-process-topology>",
            "exec",
        ),
        namespace,
    )
    exec(
        compile(
            ast.Module(body=[function], type_ignores=[]),
            "<actual-process-preflight>",
            "exec",
        ),
        namespace,
    )
    return namespace["prepare_process_observer"]


class ProcessPreflightTests(unittest.TestCase):
    def test_a_read_only_timeout_is_retained_before_the_observer_becomes_ready(self):
        calls = []

        def read():
            calls.append(True)
            if len(calls) == 1:
                raise subprocess.TimeoutExpired("process-table", 15)
            return [
                {"ProcessId": 42, "ParentProcessId": 1, "CreationDate": "/Date(1000)/"}
            ]

        record = {}
        load_reader(read)(record)
        self.assertEqual(
            [item["ready"] for item in record["processObserverPreflight"]],
            [False, True],
        )
        self.assertIn("TimeoutExpired", record["processObserverPreflight"][0]["error"])

    def test_incomplete_current_process_metadata_is_not_ready(self):
        record = {}
        with self.assertRaisesRegex(RuntimeError, "no fixture browser was started"):
            load_reader(lambda: [{"ProcessId": 42}])(record)
        self.assertTrue(
            all(item["ready"] is False for item in record["processObserverPreflight"])
        )

    def test_an_unbound_process_snapshot_never_allows_fixture_startup(self):
        record = {}
        with self.assertRaisesRegex(RuntimeError, "no fixture browser was started"):
            load_reader(lambda: [{"ProcessId": 99}])(record)
        self.assertEqual(len(record["processObserverPreflight"]), 3)
        self.assertTrue(
            all(item["ready"] is False for item in record["processObserverPreflight"])
        )


class SetupFailureReportTests(unittest.TestCase):
    def run_failure(self, failure, fail_cleanup=False):
        import contextlib
        import io
        import json
        import tempfile
        import uuid

        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            args = SimpleNamespace(
                output=root,
                binary=root / "binary",
                chrome=root / "chrome",
                bitwarden=root / "bitwarden",
                extension=root / "extension",
                setup_mode="default",
                runtime_focus_recovery=False,
                grant_chrome_foreground=False,
                foreground_grant_client=None,
            )

            class Parser:
                def add_argument(self, *args, **kwargs):
                    pass

                def parse_args(self):
                    return args

            closed = []

            def digest(path):
                if failure == "digest":
                    raise OSError("fixture input unavailable")
                return "fixture-hash"

            def observer(record):
                if failure == "observer":
                    raise RuntimeError("observer unavailable")

            def form(case):
                if failure == "fixture":
                    raise OSError("fixture unavailable")

                def close():
                    closed.append(True)
                    if fail_cleanup:
                        raise OSError("fixture close unavailable")

                return SimpleNamespace(close=close)

            def windows():
                raise OSError("window API unavailable")

            tree = ast.parse(Path(__file__).with_name("probe.py").read_text())
            main = next(
                node
                for node in tree.body
                if isinstance(node, ast.FunctionDef) and node.name == "main"
            )
            namespace = {
                "platform": SimpleNamespace(system=lambda: "Windows"),
                "os": SimpleNamespace(environ={"GITHUB_ACTIONS": "true"}),
                "argparse": SimpleNamespace(ArgumentParser=Parser),
                "Path": Path,
                "uuid": uuid,
                "json": json,
                "FormFixture": form,
                "Windows": windows,
                "digest": digest,
                "prepare_process_observer": observer,
            }
            exec(
                compile(
                    ast.Module(body=[main], type_ignores=[]),
                    "<actual-entrypoint>",
                    "exec",
                ),
                namespace,
            )
            with contextlib.redirect_stdout(io.StringIO()):
                status = namespace["main"]()
            result = json.loads((root / "summary.json").read_text())
            self.assertEqual(status, 2)
            self.assertEqual(result["verdict"], "INCONCLUSIVE")
            self.assertFalse(result["browserStarted"])
            return result, closed

    def test_observer_failure_records_inconclusive_before_allocating_the_fixture(self):
        result, closed = self.run_failure("observer")
        self.assertIn("observer unavailable", result["error"])
        self.assertEqual(closed, [])

    def test_digest_failure_still_writes_the_summary(self):
        result, closed = self.run_failure("digest")
        self.assertIn("fixture input unavailable", result["error"])
        self.assertEqual(closed, [])

    def test_fixture_constructor_failure_still_writes_the_summary(self):
        result, closed = self.run_failure("fixture")
        self.assertIn("fixture unavailable", result["error"])
        self.assertEqual(closed, [])

    def test_window_constructor_failure_closes_the_allocated_fixture(self):
        result, closed = self.run_failure("windows")
        self.assertIn("window API unavailable", result["error"])
        self.assertEqual(closed, [True])

    def test_cleanup_failure_cannot_hide_the_setup_summary(self):
        result, closed = self.run_failure("windows", True)
        self.assertIn("fixture close unavailable", result["fixtureCleanupError"])
        self.assertEqual(closed, [True])


if __name__ == "__main__":
    unittest.main()

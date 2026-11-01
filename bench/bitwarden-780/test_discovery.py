#!/usr/bin/env python3
import ast
import io
import json
from pathlib import Path
from types import SimpleNamespace
import unittest
import urllib.parse


def fixture():
    tree = ast.parse(Path(__file__).with_name("probe.py").read_text())
    nodes = [
        node
        for node in tree.body
        if isinstance(node, (ast.FunctionDef, ast.ClassDef))
        and node.name in ["WorkerRequestError", "worker_candidate", "discover_workers"]
    ]
    ours = "a" * 32
    bw = "b" * 32
    other = "c" * 32
    targets = [
        {
            "type": "service_worker",
            "url": f"chrome-extension://{ours}/background.js",
            "webSocketDebuggerUrl": "ours",
        },
        {
            "type": "service_worker",
            "url": f"chrome-extension://{bw}/background.js",
            "webSocketDebuggerUrl": "bw",
        },
        {
            "type": "service_worker",
            "url": f"chrome-extension://{other}/thunk.js",
            "webSocketDebuggerUrl": "unrelated",
        },
        {
            "type": "service_worker",
            "url": "https://fixture.invalid/background.js",
            "webSocketDebuggerUrl": "web-page",
        },
    ]
    namespace = {
        "json": json,
        "urllib": SimpleNamespace(
            parse=urllib.parse,
            request=SimpleNamespace(
                urlopen=lambda *args, **kwargs: io.BytesIO(json.dumps(targets).encode())
            ),
        ),
    }
    exec(
        compile(ast.Module(body=nodes, type_ignores=[]), "<actual-discovery>", "exec"),
        namespace,
    )
    calls = []
    state = {"transient": False, "mismatch": False}

    def worker(endpoint, *args):
        calls.append(endpoint)
        if endpoint not in ["ours", "bw"]:
            raise namespace["WorkerRequestError"]("chrome is not defined")
        if endpoint == "bw" and state["transient"]:
            state["transient"] = False
            raise namespace["WorkerRequestError"]("worker realm is still starting")
        return {
            "id": ours if endpoint == "ours" else other if state["mismatch"] else bw,
            "name": "chrome-use"
            if endpoint == "ours"
            else "Bitwarden Password Manager",
            "version": "0.5.102" if endpoint == "ours" else "2026.10.1",
        }

    namespace["worker"] = worker
    return namespace, calls, state


class DiscoveryTests(unittest.TestCase):
    def test_unrelated_and_web_workers_are_never_queried(self):
        namespace, calls, _ = fixture()
        errors = []
        ignored = []
        result = namespace["discover_workers"](1, errors, ignored, {"/background.js"})
        self.assertEqual(set(result), {"chromeUse", "bitwarden"})
        self.assertEqual(set(calls), {"ours", "bw"})
        self.assertEqual(errors, [])
        self.assertEqual(len(ignored), 2)

    def test_only_read_only_discovery_is_repolled_and_the_error_is_retained(self):
        namespace, _, state = fixture()
        state["transient"] = True
        errors = []
        ignored = []
        first = namespace["discover_workers"](1, errors, ignored, {"/background.js"})
        second = namespace["discover_workers"](1, errors, ignored, {"/background.js"})
        self.assertEqual(set(first), {"chromeUse"})
        self.assertEqual(set(second), {"chromeUse", "bitwarden"})
        self.assertEqual(len(errors), 1)
        self.assertIn("still starting", errors[0]["error"])

    def test_a_manifest_cannot_claim_a_different_worker_origin(self):
        namespace, _, state = fixture()
        state["mismatch"] = True
        errors = []
        result = namespace["discover_workers"](1, errors, [], {"/background.js"})
        self.assertEqual(set(result), {"chromeUse"})
        self.assertIn("does not match", errors[0]["error"])


if __name__ == "__main__":
    unittest.main()

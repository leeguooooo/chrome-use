#!/usr/bin/env python3
import json
import os
from pathlib import Path
import subprocess
import unittest

HELPER = Path(__file__).with_name("worker-eval.mjs")


class WorkerFailureTests(unittest.TestCase):
    def run_helper(self, endpoint, ci):
        environment = dict(os.environ)
        if ci:
            environment["GITHUB_ACTIONS"] = "true"
        else:
            environment.pop("GITHUB_ACTIONS", None)
        return subprocess.run(
            ["node", str(HELPER)],
            env=environment,
            input=json.dumps({"endpoint": endpoint, "expression": "1"}),
            capture_output=True,
            text=True,
            timeout=12,
        )

    def test_local_execution_is_refused(self):
        result = self.run_helper("ws://127.0.0.1:0", False)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("CI-only worker control", result.stderr)

    def test_foreign_endpoint_is_refused_before_connecting(self):
        result = self.run_helper("ws://example.invalid:80", True)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("loopback only", result.stderr)

    def test_worker_transport_failure_keeps_a_structured_reason(self):
        result = self.run_helper("ws://127.0.0.1:0", True)
        self.assertNotEqual(result.returncode, 0)
        reasons = []
        for line in result.stderr.splitlines():
            try:
                reasons.append(json.loads(line))
            except ValueError:
                pass
        self.assertTrue(
            any(reason.get("stage") and reason.get("message") for reason in reasons),
            result.stderr,
        )
        self.assertEqual(result.stdout, "")


if __name__ == "__main__":
    unittest.main()

#!/usr/bin/env python3
import ast
from pathlib import Path
import unittest


def session_closer(response):
    tree = ast.parse(Path(__file__).with_name("probe.py").read_text())
    nodes = [
        node
        for node in tree.body
        if isinstance(node, ast.FunctionDef)
        and node.name in ["session_close_confirmed", "close_fixture_session"]
    ]
    calls = []

    def execute(*args, **kwargs):
        calls.append(args[0])
        return response

    namespace = {"execute": execute}
    exec(
        compile(
            ast.Module(body=nodes, type_ignores=[]), "<actual-session-caller>", "exec"
        ),
        namespace,
    )
    return namespace["close_fixture_session"], calls


class SessionCallerTests(unittest.TestCase):
    def test_unsuccessful_close_stays_unconfirmed_and_is_never_repeated(self):
        close, calls = session_closer(
            {"exit": 1, "killedAtDeadline": False, "response": {"success": False}}
        )
        record = {"verdict": "PASS"}
        close(record, ["source-cli"], {}, Path("fixture-output"))
        self.assertFalse(record["sessionCleanupConfirmed"])
        self.assertEqual(record["verdict"], "INCONCLUSIVE")
        self.assertEqual(calls, [["source-cli", "close"]])

    def test_an_unstarted_cli_session_needs_no_close_request(self):
        close, calls = session_closer(None)
        record = {"verdict": "INCONCLUSIVE"}
        close(record, None, {}, Path("fixture-output"))
        self.assertTrue(record["sessionCleanupConfirmed"])
        self.assertEqual(record["sessionCleanupStatus"], "not-started")
        self.assertEqual(calls, [])


if __name__ == "__main__":
    unittest.main()

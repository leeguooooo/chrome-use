#!/usr/bin/env python3
import ast
from pathlib import Path
import unittest


def helpers():
    tree = ast.parse(Path(__file__).with_name("probe.py").read_text())
    nodes = [
        node
        for node in tree.body
        if isinstance(node, ast.FunctionDef)
        and node.name
        in [
            "protected_access_refusal",
            "native_escape_delivered",
            "session_close_confirmed",
        ]
    ]
    namespace = {}
    exec(
        compile(
            ast.Module(body=nodes, type_ignores=[]), "<actual-outcome-gates>", "exec"
        ),
        namespace,
    )
    return namespace


class OutcomeTests(unittest.TestCase):
    def test_a_successful_or_unrelated_read_does_not_reproduce_the_blocker(self):
        denied = {
            "exit": 1,
            "killedAtDeadline": False,
            "response": {"success": False, "code": "debugger_access_denied"},
        }
        gate = helpers()["protected_access_refusal"]
        self.assertTrue(gate(denied))
        self.assertFalse(
            gate({"exit": 0, "killedAtDeadline": False, "response": {"success": True}})
        )
        self.assertFalse(
            gate(
                {
                    "exit": 1,
                    "killedAtDeadline": False,
                    "response": {"success": False, "code": "timeout"},
                }
            )
        )
        self.assertFalse(gate(dict(denied, killedAtDeadline=True)))

    def test_admission_alone_cannot_be_reported_as_observed_escape_delivery(self):
        gate = helpers()["native_escape_delivered"]
        pair = [
            {"type": "keydown", "key": "Escape", "code": "Escape", "trusted": True},
            {"type": "keyup", "key": "Escape", "code": "Escape", "trusted": True},
        ]
        self.assertTrue(gate(pair))
        self.assertFalse(gate([]))
        self.assertFalse(gate(pair[:1]))
        self.assertFalse(gate([dict(event, trusted=False) for event in pair]))
        self.assertFalse(gate([dict(event, key="Enter") for event in pair]))
        self.assertFalse(gate([dict(event, code="Unidentified") for event in pair]))
        self.assertFalse(gate(pair + [dict(pair[0])]))
        self.assertFalse(gate(pair + pair))

    def test_teardown_requires_a_successful_verified_absence_response(self):
        confirmed = {
            "exit": 0,
            "killedAtDeadline": False,
            "response": {
                "success": True,
                "data": {"closed": True, "verifiedAbsent": True},
            },
        }
        gate = helpers()["session_close_confirmed"]
        self.assertTrue(gate(confirmed))
        self.assertFalse(gate(dict(confirmed, exit=1)))
        self.assertFalse(gate(dict(confirmed, killedAtDeadline=True)))
        self.assertFalse(
            gate({"exit": 0, "killedAtDeadline": False, "response": {"success": False}})
        )
        self.assertFalse(
            gate(
                {
                    "exit": 0,
                    "killedAtDeadline": False,
                    "response": {"success": True, "data": {"closed": True}},
                }
            )
        )


if __name__ == "__main__":
    unittest.main()

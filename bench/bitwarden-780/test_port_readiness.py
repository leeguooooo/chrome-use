"""The port file can exist before Chrome releases its Windows write handle."""

import ast
from pathlib import Path
from types import SimpleNamespace
import unittest


class PortTests(unittest.TestCase):
    def test_only_a_readable_valid_record_is_ready(self):
        tree = ast.parse(Path(__file__).with_name("probe.py").read_text())
        fn = next(
            n
            for n in tree.body
            if isinstance(n, ast.FunctionDef) and n.name == "read_debugging_port"
        )
        namespace = {}
        exec(
            compile(
                ast.Module(body=[fn], type_ignores=[]), "<actual-port-reader>", "exec"
            ),
            namespace,
        )
        errors = []
        for outcome in [
            FileNotFoundError("not created"),
            PermissionError("writer owns handle"),
            "",
            "junk",
            "0",
            "65536",
            "50000\n/devtools/browser/test",
        ]:

            def read():
                if isinstance(outcome, Exception):
                    raise outcome
                return outcome

            actual = namespace["read_debugging_port"](
                SimpleNamespace(read_text=read), errors
            )
            self.assertEqual(
                actual, 50000 if outcome == "50000\n/devtools/browser/test" else None
            )
        self.assertEqual(len(errors), 3)

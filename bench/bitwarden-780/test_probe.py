#!/usr/bin/env python3
"""Pure controller checks; no Chrome, vault, or Windows API calls."""

import ast
import json
from pathlib import Path
import subprocess
import unittest

ROOT = Path(__file__).parent


def menu_expression(source):
    tree = ast.parse(source)
    function = next(
        node
        for node in tree.body
        if isinstance(node, ast.FunctionDef) and node.name == "menu_frames"
    )
    namespace = {"json": json, "worker": lambda endpoint, expression, *args: expression}
    exec(
        compile(
            ast.Module(body=[function], type_ignores=[]),
            "<actual-menu-controller>",
            "exec",
        ),
        namespace,
    )
    return namespace["menu_frames"](
        "unused-worker", "http://127.0.0.1:7800/login?fixture", "b" * 32
    )


def main():
    expression = menu_expression((ROOT / "probe.py").read_text())
    result = subprocess.run(
        ["node", str(ROOT / "test-menu-observer.cjs")],
        input=expression,
        capture_output=True,
        text=True,
    )
    print(result.stdout, end="")
    if result.returncode:
        print(result.stderr, end="")
    return result.returncode


class MenuIdentityTests(unittest.TestCase):
    def test_actual_controller_identity_contract(self):
        self.assertEqual(main(), 0)


if __name__ == "__main__":
    unittest.main()

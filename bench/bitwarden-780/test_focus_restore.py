"""Check the actual controller's no-interference guards without desktop input."""

import ast
import copy
import os
from pathlib import Path
from types import SimpleNamespace
import unittest


class RestoreTests(unittest.TestCase):
    def test_changed_focus_or_identity_never_actuates(self):
        tree = ast.parse(Path(__file__).with_name("probe.py").read_text())
        fn = next(
            n
            for n in tree.body
            if isinstance(n, ast.FunctionDef) and n.name == "restore_fixture_cover"
        )
        namespace = {"os": os}
        exec(
            compile(ast.Module(body=[fn], type_ignores=[]), "<controller>", "exec"),
            namespace,
        )
        page = {"foreground": 10, "pid": 123, "title": "BitwardenFixture case"}
        cover = {
            "pid": os.getpid(),
            "title": "Bitwarden fixture cover case",
            "minimized": False,
            "visible": True,
        }
        for target, field, value in [
            ("before", "foreground", 30),
            ("page", "foreground", 30),
            ("page", "pid", 999),
            ("page", "title", "user page"),
            ("cover", "pid", 999),
            ("cover", "title", "user app"),
            ("cover", "minimized", True),
            ("cover", "visible", False),
        ]:
            with self.subTest(target=target, field=field):
                states = {
                    "before": {"foreground": 20, "title": page["title"]},
                    "page": copy.deepcopy(page),
                    "cover": copy.deepcopy(cover),
                }
                states[target][field] = value
                # No native API supplied: an attempted actuation fails the test.
                windows = SimpleNamespace(
                    state=lambda h: states["page" if h == 10 else "cover"]
                )
                result = namespace["restore_fixture_cover"](
                    windows, 10, 123, "case", 20, states["before"]
                )
                self.assertFalse(result["attempted"])
                self.assertFalse(result["restored"])

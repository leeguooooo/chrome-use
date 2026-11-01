#!/usr/bin/env python3
import ast
from pathlib import Path
from types import SimpleNamespace
import unittest


class Key:
    def __init__(self, value):
        self.value = value

    def __enter__(self):
        return self

    def __exit__(self, *args):
        pass


def verify(existed, original, current_key_exists, current_value):
    tree = ast.parse(Path(__file__).with_name("probe.py").read_text())
    function = next(
        node
        for node in tree.body
        if isinstance(node, ast.FunctionDef) and node.name == "registry_restored"
    )
    namespace = {}
    exec(
        compile(
            ast.Module(body=[function], type_ignores=[]),
            "<actual-restore-proof>",
            "exec",
        ),
        namespace,
    )

    def open_key(*args):
        if not current_key_exists:
            raise FileNotFoundError()
        return Key(current_value)

    def read(key, name):
        if key.value is None:
            raise FileNotFoundError()
        return key.value

    registry = SimpleNamespace(
        saved=[("fixture", existed, original)],
        winreg=SimpleNamespace(
            HKEY_CURRENT_USER=1, OpenKey=open_key, QueryValueEx=read
        ),
    )
    return namespace["registry_restored"](registry)


class RegistryRestoreTests(unittest.TestCase):
    def test_original_key_and_default_value_must_actually_be_restored(self):
        original = ("original-native-host", 1)
        self.assertTrue(verify(True, original, True, original))
        self.assertFalse(verify(True, original, True, ("temporary-native-host", 1)))
        self.assertFalse(verify(True, original, False, None))
        self.assertTrue(verify(False, None, False, None))
        self.assertFalse(verify(False, None, True, None))
        self.assertTrue(verify(True, None, True, None))
        self.assertFalse(verify(True, None, True, ("unexpected-default", 1)))


if __name__ == "__main__":
    unittest.main()

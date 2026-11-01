"""Receipt gate only; no browser, native input or foreground grant."""
import ast
from pathlib import Path
import unittest


def gate():
    tree = ast.parse(Path(__file__).with_name("probe.py").read_text())
    node = next(item for item in tree.body if isinstance(item, ast.FunctionDef)
                and item.name == "client_authorization_observed")
    namespace = {}
    exec(compile(ast.Module(body=[node], type_ignores=[]), "<actual-client-authorization-gate>", "exec"), namespace)
    return namespace["client_authorization_observed"]


class ClientAuthorizationTests(unittest.TestCase):
    def test_only_successful_callback_observations_cover_this_gate(self):
        observed = gate()
        for kind in ("Admitted", "AlreadyForegroundProcess"):
            self.assertTrue(observed({"foregroundAuthorization": {"kind": kind}}))
        for receipt in ({}, {"foregroundAuthorization": None},
                        {"foregroundAuthorization": ["Admitted"]}):
            self.assertFalse(observed(receipt))
        for kind in ("OsDenied", "Unavailable", "Refused", "unknown"):
            self.assertFalse(observed({"foregroundAuthorization": {"kind": kind}}))


if __name__ == "__main__":
    unittest.main()

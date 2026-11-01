import ast
from pathlib import Path
from types import SimpleNamespace
import unittest


def helpers():
    tree = ast.parse(Path(__file__).with_name('probe.py').read_text())
    nodes = [n for n in tree.body if isinstance(n, ast.FunctionDef) and n.name in (
        'foreground_blocked_without_input', 'foreground_permission_control')]
    namespace = {'os': SimpleNamespace(getpid=lambda: 12),
                 'ctypes': SimpleNamespace(set_last_error=lambda value: None, get_last_error=lambda: 5),
                 'wintypes': SimpleNamespace(DWORD=int, BOOL=bool)}
    exec(compile(ast.Module(body=nodes, type_ignores=[]), '<foreground-control>', 'exec'), namespace)
    return namespace


class Delegate:
    def __init__(self, admitted=True):
        self.calls = []
        self.admitted = admitted

    def __call__(self, pid):
        self.calls.append(pid)
        return self.admitted


def fixture(admitted=True):
    delegate = Delegate(admitted)
    cover = {'pid': 12, 'foreground': 100}
    target = {'pid': 42, 'foreground': 100}
    windows = SimpleNamespace(u=SimpleNamespace(AllowSetForegroundWindow=delegate),
                              state=lambda hwnd: cover if hwnd == 100 else target)
    return windows, delegate, cover, target


class ForegroundControlTests(unittest.TestCase):
    def child_result(self, admitted=True):
        return {'exit': 0 if admitted else 1, 'killedAtDeadline': False,
                'response': {'success': admitted, 'data': {
                    'callerPid': 99, 'chromePid': 42, 'admitted': admitted,
                    'errorCode': 0 if admitted else 5,
                    'foregroundBefore': 100, 'foregroundAfter': 100,
                    'foregroundPidBefore': 12, 'foregroundPidAfter': 12,
                    'nativeInputAttempted': False, 'windowActivationAttempted': False}}}

    def test_the_fresh_child_is_the_only_grant_caller(self):
        windows, delegate, _, _ = fixture()
        namespace = helpers()
        result = self.child_result()
        calls = []
        namespace['execute'] = lambda words, env, artifact, timeout: (calls.append(words), result)[1]
        report = namespace['foreground_permission_control'](windows, 200, 42, 100, Path('child.exe'), {}, Path('artifact'))
        self.assertTrue(report['admitted'])
        self.assertEqual(delegate.calls, [])
        self.assertEqual(calls, [['child.exe', '--ci-allow-foreground-delegation', '42']])
        self.assertEqual(report['childControl'], result)

    def test_child_denial_is_preserved_without_controller_fallback(self):
        windows, delegate, _, _ = fixture()
        namespace = helpers()
        namespace['execute'] = lambda *args, **kwargs: self.child_result(False)
        report = namespace['foreground_permission_control'](windows, 200, 42, 100, Path('child.exe'), {}, Path('artifact'))
        self.assertFalse(report['admitted'])
        self.assertEqual(report['errorCode'], 5)
        self.assertEqual(delegate.calls, [])

    def test_unknown_child_effects_never_become_a_verified_grant(self):
        for field, value in [('chromePid', 43), ('callerPid', 12), ('foregroundBefore', 999),
                             ('foregroundPidBefore', 99), ('foregroundAfter', 999),
                             ('nativeInputAttempted', True), ('windowActivationAttempted', True)]:
            windows, delegate, _, _ = fixture()
            namespace = helpers()
            result = self.child_result()
            result['response']['data'][field] = value
            namespace['execute'] = lambda *args, **kwargs: result
            with self.assertRaises(RuntimeError):
                namespace['foreground_permission_control'](windows, 200, 42, 100, Path('child.exe'), {}, Path('artifact'))
            self.assertEqual(delegate.calls, [])

    def test_only_verified_no_input_foreground_refusals_allow_the_control(self):
        gate = helpers()['foreground_blocked_without_input']
        native = {'input': None, 'originalOperationReplayed': False}
        response = {'success': False, 'nativeFocus': native,
                    'error': 'Foreground did not become the attested Chrome within 500ms; no Escape was sent; the current page operation was not run'}
        result = {'exit': 1, 'killedAtDeadline': False, 'response': response}
        self.assertTrue(gate(result))
        for changed in [dict(result, exit=0), dict(result, killedAtDeadline=True),
                        dict(result, response=dict(response, success=True)),
                        dict(result, response=dict(response, error='action_outcome_unknown')),
                        dict(result, response=dict(response, nativeFocus=dict(native, input={'keyDownAdmitted': 1}))),
                        dict(result, response=dict(response, nativeFocus={})),
                        dict(result, response=dict(response, nativeFocus=dict(native, originalOperationReplayed=True)))]:
            self.assertFalse(gate(changed))

    def test_grants_only_the_owned_chrome_pid_without_changing_foreground(self):
        windows, delegate, _, _ = fixture()
        report = helpers()['foreground_permission_control'](windows, 200, 42, 100)
        self.assertEqual(delegate.calls, [42])
        self.assertTrue(report['admitted'])
        self.assertTrue(report['foregroundUnchanged'])
        self.assertFalse(report['unassistedProductionAcceptance'])

    def test_invalid_ownership_focus_or_all_process_grants_never_call_the_api(self):
        for alteration in ('cover-owner', 'foreground', 'chrome-owner', 'all-processes', 'zero'):
            windows, delegate, cover, target = fixture()
            pid = 42
            if alteration == 'cover-owner': cover['pid'] = 99
            if alteration == 'foreground': cover['foreground'] = 999
            if alteration == 'chrome-owner': target['pid'] = 99
            if alteration == 'all-processes': pid = 0xFFFFFFFF
            if alteration == 'zero': pid = 0
            with self.assertRaises(RuntimeError):
                helpers()['foreground_permission_control'](windows, 200, pid, 100)
            self.assertEqual(delegate.calls, [])

    def test_a_denied_grant_is_reported_and_never_retried(self):
        windows, delegate, _, _ = fixture(False)
        report = helpers()['foreground_permission_control'](windows, 200, 42, 100)
        self.assertFalse(report['admitted'])
        self.assertEqual(report['errorCode'], 5)
        self.assertEqual(delegate.calls, [42])


if __name__ == '__main__':
    unittest.main()

"""Protocol regressions for the acceptance runner; these do not launch Chrome."""
import importlib.util
import json
from pathlib import Path
import sys
import tempfile
import unittest

spec = importlib.util.spec_from_file_location('progress_check', Path(__file__).with_name('progress-check.py'))
check = importlib.util.module_from_spec(spec)
spec.loader.exec_module(check)


def record(success=True, code=0, data=None):
    return {'exitCode': code, 'result': {'success': success, 'data': data or {},
            'timing': {'ms': 10, 'cdpBusyMs': 6, 'nonCdpMs': 4}}}


class ProtocolTests(unittest.TestCase):
    def test_advisory_does_not_make_success_a_failure(self):
        data = {'observed': {'noProgress': {'retryAction': False}}}
        self.assertEqual(check.verify_result(record(data=data)), data)

    def test_missing_or_inconsistent_timing_fails(self):
        for timing in (None, {}, {'ms': 10, 'cdpBusyMs': 6, 'nonCdpMs': 5},
                       {'ms': True, 'cdpBusyMs': 1, 'nonCdpMs': 0}):
            with self.subTest(timing=timing), self.assertRaises(AssertionError):
                item = record()
                item['result']['timing'] = timing
                check.verify_result(item)

    def test_failed_script_requires_both_exit_one_and_preserved_advisories(self):
        data = {'ok': False, 'advisories': [{'retryAction': False}]}
        check.verify_result(record(code=1, data=data), success=False)
        for code, bad in ((0, data), (1, {'ok': False}), (1, {'ok': True, 'advisories': [1]})):
            with self.subTest(code=code, data=bad), self.assertRaises(AssertionError):
                check.verify_result(record(code=code, data=bad), success=False)

    def test_protocol_failure_is_not_hidden_by_exit_zero(self):
        with self.assertRaises(AssertionError):
            check.verify_result(record(success=False))

    def test_timeout_terminates_only_owned_process(self):
        with self.assertRaises(TimeoutError):
            check.invoke([sys.executable, '-c', 'import time; time.sleep(30)'], {}, 0.05)

    def test_wrong_hash_fails_before_starting_daemon_and_writes_report(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            binary = root / 'fake'
            binary.write_text('not an executable')
            output = root / 'result.json'
            result = check.acceptance(binary, 'bad', '0', output, 1)
            self.assertEqual(result['verdict'], 'FAIL')
            self.assertEqual(result['error'], 'Binary SHA-256 mismatch')
            self.assertEqual(result['records'], [])
            self.assertEqual(json.loads(output.read_text()), result)


if __name__ == '__main__':
    unittest.main()

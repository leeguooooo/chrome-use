"""Synthetic, device-free tests for evidence attribution and collector output."""
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parent

def load(name):
    spec = importlib.util.spec_from_file_location(name.replace('-', '_'), ROOT / (name + '.py'))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module

metrics = load('task-metrics')
runner = load('run-task')

class TaskMetricsTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.path = Path(self.temp.name) / 'run.tsv'

    def write(self, text):
        self.path.write_text(text, encoding='utf-8')
        return metrics.summarize(self.path)

    def test_legacy_never_claims_model_turns_or_utf8(self):
        result = self.write('# task\ttask.txt\n# warm\ttrue\nn\tms\tbytes\trc\tcmd\n1\t100\t3\t0\tsnapshot\n# verdict\tpass\n')
        self.assertIsNone(result['model_round_trips'])
        self.assertIsNone(result['response_utf8_bytes'])
        self.assertEqual(result['postcondition'], 'unverified')
        self.assertFalse(result['successful_task'])
        self.assertEqual(result['temperature'], 'legacy_warmup_requested')

    def test_recovered_failure_preserves_cost_and_verdict(self):
        result = self.write('# run_id\tr1\n# bytes_encoding\tutf-8\nn\tms\tbytes\trc\tcmd\tunknown\n1\t10\t6\t1\tclick x --observe\ttrue\n2\t90\t9\t0\tbatch "snapshot"\tfalse\n# assert\tpass\ttext body contains done\n# verdict\tpass\n')
        self.assertTrue(result['successful_task'])
        self.assertFalse(result['all_cli_calls_succeeded'])
        self.assertEqual(result['unknown_calls'], 1)
        self.assertEqual(result['response_utf8_bytes'], 15)
        self.assertEqual(result['call_p50_ms'], 10)
        self.assertEqual(result['call_p95_ms'], 90)
        self.assertEqual(result['feature_calls']['observe'], 1)
        self.assertEqual(result['feature_rates']['batch'], .5)
        self.assertTrue(result['warnings'])

    def test_failed_assert_overrides_pass_metadata(self):
        result = self.write('n\tms\tbytes\trc\tcmd\n1\t1\t1\t0\tsnapshot\n# assert\tFAIL\ttext body contains done\n# verdict\tpass\n')
        self.assertFalse(result['successful_task'])

    def test_multiple_runs_refused(self):
        with self.assertRaises(ValueError):
            self.write('# run_id\ta\n# run_id\tb\nn\tms\tbytes\trc\tcmd\n')

    def test_explicit_trace_separates_parallel_tools_from_model_turn(self):
        self.write('# run_id\tr1\nn\tms\tbytes\trc\tcmd\n')
        trace = Path(self.temp.name) / 'trace.jsonl'
        events = [{'event':'run_start', 'run_id':'r1'}, {'event':'model_turn', 'run_id':'r1', 'id':'m1'}, {'event':'tool_call', 'run_id':'r1', 'id':'t1'}, {'event':'tool_call', 'run_id':'r1', 'id':'t2'}, {'event':'run_end', 'run_id':'r1'}]
        trace.write_text('\n'.join(map(json.dumps, events)))
        result = metrics.summarize(self.path, trace)
        self.assertEqual(result['model_round_trips'], 1)
        self.assertEqual(result['trace_tool_calls'], 2)
        events[2]['run_id'] = 'other-task'
        trace.write_text('\n'.join(map(json.dumps, events)))
        with self.assertRaises(ValueError):
            metrics.summarize(self.path, trace)

    def test_unknown_page_text_is_not_protocol_error(self):
        self.assertFalse(runner.contains_unknown({'data': {'text': 'action_outcome_unknown'}}))
        self.assertTrue(runner.contains_unknown({'error': {'code':'action_outcome_unknown'}}))

    def test_collector_counts_utf8_and_no_assert_is_failure(self):
        fake = Path(self.temp.name) / 'fake-cli'
        fake.write_text('#!/usr/bin/env python3\nimport json,sys\nprint(json.dumps({"text":"你好"}, ensure_ascii=False))\n')
        fake.chmod(0o755)
        task = Path(self.temp.name) / 'task.txt'
        task.write_text('snapshot\n')
        process = subprocess.run([sys.executable, str(ROOT / 'run-task.py'), str(task), '--binary', str(fake), '--output', str(self.path)], capture_output=True)
        self.assertEqual(process.returncode, 1, process.stderr)
        result = metrics.summarize(self.path)
        self.assertEqual(result['response_utf8_bytes'], len('{"text": "你好"}\n'.encode('utf-8')))
        self.assertEqual(result['temperature'], 'warmup_succeeded')
        self.assertIsNotNone(result['binary_sha256'])
        self.assertIsNotNone(result['source_sha256'])
        self.assertFalse(result['successful_task'])
        repeated = subprocess.run([sys.executable, str(ROOT / 'run-task.py'), str(task), '--binary', str(fake), '--output', str(self.path)], capture_output=True)
        self.assertNotEqual(repeated.returncode, 0)

if __name__ == '__main__':
    unittest.main()

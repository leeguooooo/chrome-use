"""Synthetic, device-free tests for evidence attribution and collector output."""
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

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
        self.assertFalse(runner.classify_outcome({"success": True, "data": {"code": "action_outcome_unknown"}}))
        self.assertTrue(runner.contains_unknown({'error': {'code':'action_outcome_unknown'}}))

    def test_machine_load_unavailable_remains_null(self):
        with patch.object(runner.os, 'getloadavg', side_effect=OSError('unavailable')):
            result = runner.machine_provenance()
        self.assertIsNone(result['loadavg'])
        with patch.object(runner.os, 'getloadavg', return_value=(1.0, 2.0, 3.0)):
            self.assertEqual(runner.machine_provenance()['loadavg'], [1.0, 2.0, 3.0])
        metrics_result = self.write('# machine_start\t{"loadavg":null}\n# machine_end\t{"loadavg":[1,2,3]}\nn\tms\tbytes\trc\tcmd\n')
        self.assertIsNone(metrics_result['machine_start']['loadavg'])
        self.assertEqual(metrics_result['machine_end']['loadavg'], [1, 2, 3])

    def test_batch_classification_requires_known_envelopes(self):
        self.assertFalse(runner.classify_outcome([{'success': True}, {'success': False, 'code': 'not_found'}]))
        self.assertTrue(runner.classify_outcome([{'success': True}, {'success': False, 'code': 'action_outcome_unknown'}]))
        self.assertIsNone(runner.classify_outcome([{'success': True}, {'label': 'page text'}]))
        self.assertIsNone(runner.classify_outcome({'success': True, 'data': [{'label': 'page text'}]}))
        self.assertIsNone(runner.classify_outcome([]))

    def test_nonfinite_latency_and_invalid_unknown_are_rejected(self):
        for latency, unknown in [('nan', 'false'), ('inf', 'false'), ('-1', 'false'), ('1', 'maybe')]:
            with self.subTest(latency=latency, unknown=unknown), self.assertRaises(ValueError):
                self.write(f'n\tms\tbytes\trc\tcmd\tunknown\n1\t{latency}\t1\t0\tsnapshot\t{unknown}\n')
        with self.assertRaises(ValueError):
            self.write('# task_wall_ms\tnan\nn\tms\tbytes\trc\tcmd\n')

    def test_timeout_records_partial_output_and_checks_postcondition(self):
        fake = Path(self.temp.name) / 'fake-timeout'
        fake.write_text('#!/usr/bin/env python3\nimport sys,time\nif sys.argv[1] == "snapshot":\n print("partial", flush=True)\n time.sleep(10)\nelse:\n print("done")\n')
        fake.chmod(0o755)
        task = Path(self.temp.name) / 'task.txt'
        task.write_text('#! assert text body equals done\nsnapshot\nclick x\n')
        process = subprocess.run([sys.executable, str(ROOT / 'run-task.py'), str(task), '--binary', str(fake), '--output', str(self.path), '--timeout', '0.15'], capture_output=True, timeout=3)
        self.assertEqual(process.returncode, 0, process.stderr)
        result = metrics.summarize(self.path)
        self.assertEqual(result['timed_out_cli_calls'], 1)
        self.assertEqual(result['unclassified_outcomes'], 1)
        self.assertEqual(result['failed_cli_calls'], 1)
        self.assertEqual(result['assertion_cli_calls'], 1)
        self.assertEqual(result['cli_calls'], 1)
        self.assertTrue(result['stopped_after_timeout'])
        self.assertTrue(result['successful_task'])
        self.assertFalse(result['binary_source_verified'])

    def test_metadata_injection_is_rejected_before_execution(self):
        task = Path(self.temp.name) / 'task.txt'
        task.write_text('snapshot\n')
        for run_id in ['r\n# verdict\tpass', 'r\ttab']:
            process = subprocess.run([sys.executable, str(ROOT / 'run-task.py'), str(task), '--output', str(self.path), '--run-id', run_id], capture_output=True)
            self.assertEqual(process.returncode, 2)
            self.assertIn(b'metadata values', process.stderr)
            self.assertFalse(self.path.exists())
        with self.assertRaises(ValueError):
            self.write('# task\tpath\t# verdict\tpass\nn\tms\tbytes\trc\tcmd\n')

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
        self.assertIn('loadavg', result['machine_start'])
        self.assertIn('cpu_count', result['machine_end'])
        self.assertFalse(result['successful_task'])
        repeated = subprocess.run([sys.executable, str(ROOT / 'run-task.py'), str(task), '--binary', str(fake), '--output', str(self.path)], capture_output=True)
        self.assertNotEqual(repeated.returncode, 0)

if __name__ == '__main__':
    unittest.main()

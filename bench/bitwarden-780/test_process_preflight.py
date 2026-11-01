#!/usr/bin/env python3
import ast
from pathlib import Path
from types import SimpleNamespace
import subprocess
import unittest


def load_reader(reader):
    tree=ast.parse(Path(__file__).with_name('probe.py').read_text())
    function=next(node for node in tree.body if isinstance(node,ast.FunctionDef) and node.name=='prepare_process_observer')
    namespace={'processes':reader,'os':SimpleNamespace(getpid=lambda:42),'subprocess':subprocess}
    exec(compile(ast.Module(body=[function],type_ignores=[]),'<actual-process-preflight>','exec'),namespace)
    return namespace['prepare_process_observer']


class ProcessPreflightTests(unittest.TestCase):
    def test_a_read_only_timeout_is_retained_before_the_observer_becomes_ready(self):
        calls=[]
        def read():
            calls.append(True)
            if len(calls)==1:raise subprocess.TimeoutExpired('process-table',15)
            return [{'ProcessId':42}]
        record={};load_reader(read)(record)
        self.assertEqual([item['ready'] for item in record['processObserverPreflight']],[False,True])
        self.assertIn('TimeoutExpired',record['processObserverPreflight'][0]['error'])

    def test_an_unbound_process_snapshot_never_allows_fixture_startup(self):
        record={}
        with self.assertRaisesRegex(RuntimeError,'no fixture browser was started'):
            load_reader(lambda:[{'ProcessId':99}])(record)
        self.assertEqual(len(record['processObserverPreflight']),3)
        self.assertTrue(all(item['ready'] is False for item in record['processObserverPreflight']))


if __name__=='__main__':unittest.main()

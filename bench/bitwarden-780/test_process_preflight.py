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


class SetupFailureReportTests(unittest.TestCase):
    def test_observer_failure_records_inconclusive_without_creating_a_browser(self):
        import contextlib
        import io
        import json
        import tempfile
        import uuid
        with tempfile.TemporaryDirectory() as temporary:
            root=Path(temporary)
            args=SimpleNamespace(output=root,binary=root/'binary',chrome=root/'chrome',
                bitwarden=root/'bitwarden',extension=root/'extension',setup_mode='default')
            class Parser:
                def add_argument(self,*args,**kwargs):pass
                def parse_args(self):return args
            closed=[]
            def unavailable(record):
                raise RuntimeError('observer unavailable')
            tree=ast.parse(Path(__file__).with_name('probe.py').read_text())
            main=next(node for node in tree.body if isinstance(node,ast.FunctionDef) and node.name=='main')
            namespace={'platform':SimpleNamespace(system=lambda:'Windows'),
                'os':SimpleNamespace(environ={'GITHUB_ACTIONS':'true'}),
                'argparse':SimpleNamespace(ArgumentParser=Parser),'Path':Path,'uuid':uuid,'json':json,
                'FormFixture':lambda case:SimpleNamespace(close=lambda:closed.append(True)),
                'Windows':lambda:None,'digest':lambda path:'fixture-hash','prepare_process_observer':unavailable}
            exec(compile(ast.Module(body=[main],type_ignores=[]),'<actual-entrypoint>','exec'),namespace)
            with contextlib.redirect_stdout(io.StringIO()):status=namespace['main']()
            result=json.loads((root/'summary.json').read_text())
            self.assertEqual(status,2)
            self.assertEqual(result['verdict'],'INCONCLUSIVE')
            self.assertFalse(result['browserStarted'])
            self.assertIn('observer unavailable',result['error'])
            self.assertEqual(closed,[True])


if __name__=='__main__':unittest.main()

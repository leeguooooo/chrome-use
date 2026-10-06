#!/usr/bin/env python3
"""Source transfer checks; run these on the SSH build host."""
import ast
import shutil
import importlib.util
import os
from pathlib import Path
import subprocess
import tarfile
import tempfile
import unittest

spec=importlib.util.spec_from_file_location('remote_cargo',Path(__file__).with_name('remote-cargo.py'))
remote=importlib.util.module_from_spec(spec);spec.loader.exec_module(remote)

class SnapshotTests(unittest.TestCase):
    def setUp(self):
        self.temp=tempfile.TemporaryDirectory(prefix='rc-fixture-')
        self.root=Path(self.temp.name)/'repo';self.root.mkdir()
        self.git('init','-q')
        (self.root/'main.rs').write_text('fn main() {}\n')
        (self.root/'.gitignore').write_text('.env\n')
        self.git('add','main.rs','.gitignore')
        self.git('-c','user.name=Build Fixture','-c','user.email=fixture@example.invalid','-c','core.hooksPath=/dev/null','-c','commit.gpgsign=false','commit','-qm','fixture')
    def tearDown(self):self.temp.cleanup()
    def git(self,*args):subprocess.run(['git','-C',str(self.root),*args],check=True,capture_output=True)
    def pack(self):
        dest=Path(self.temp.name)/'source.tar.gz'
        digest,manifest=remote.snapshot(self.root,dest)
        with tarfile.open(dest) as archive:names=archive.getnames()
        return digest,manifest,names
    def test_working_edits_and_intent_files_but_no_ignored_secret(self):
        (self.root/'main.rs').write_text('fn main() { println!("changed"); }\n')
        (self.root/'new.rs').write_text('const ADDED: bool = true;\n')
        (self.root/'.env').write_text('SYNTHETIC_FIXTURE_ONLY=true\n')
        self.git('add','-N','new.rs')
        digest,manifest,names=self.pack()
        self.assertIn('new.rs',names);self.assertNotIn('.env',names)
        self.assertNotIn('.git/config',names)
        rows={r['path']:r for r in manifest['files']}
        self.assertEqual(rows['main.rs']['sha256'],remote.hashlib.sha256((self.root/'main.rs').read_bytes()).hexdigest())
        self.assertEqual(len(digest),64)
    def test_source_hash_changes_without_a_commit(self):
        before=self.pack()[0];(self.root/'main.rs').write_text('fn main() { }\n')
        self.assertNotEqual(before,self.pack()[0])
    @unittest.skipUnless(hasattr(os,'symlink'),'requires symlinks')
    def test_symlink_outside_checkout_is_refused(self):
        outside=Path(self.temp.name)/'outside.txt';outside.write_text('fixture\n')
        (self.root/'outside-link').symlink_to(outside)
        self.git('add','outside-link')
        with self.assertRaisesRegex(ValueError,'escapes checkout'):self.pack()
    def test_damaged_cached_source_is_restored_and_preserved(self):
        job=Path(self.temp.name)/'job';job.mkdir()
        uploaded=job/'src';uploaded.mkdir();(uploaded/'main.rs').write_text('verified source')
        cached=Path(self.temp.name)/'cached';cached.mkdir();(cached/'main.rs').write_text('damaged source')
        # Execute the runner's cache-reuse branch against real filesystem state.
        tree=ast.parse(remote.REMOTE_RUNNER)
        branch=next(n for n in ast.walk(tree) if isinstance(n,ast.If)
                    and ast.unparse(n.test)=='short_src.exists()')
        namespace={'short_src':cached,'uploaded_src':uploaded,'job':job,'src':uploaded,'shutil':shutil}
        def verify():
            if (namespace['src']/'main.rs').read_text()!='verified source':
                raise RuntimeError('Source changed')
        namespace['verify_source']=verify
        exec(compile(ast.Module(body=[branch],type_ignores=[]),'cache-reuse','exec'),namespace)
        self.assertEqual((cached/'main.rs').read_text(),'verified source')
        self.assertEqual((job/'invalid-source'/'main.rs').read_text(),'damaged source')
        self.assertFalse(uploaded.exists())
        verify()

    def test_embedded_runner_has_valid_python_syntax(self):
        compile(remote.REMOTE_RUNNER,'remote-runner','exec')

if __name__=='__main__':unittest.main()

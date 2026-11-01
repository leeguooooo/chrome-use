#!/usr/bin/env python3
import hashlib
from pathlib import Path
import subprocess
import tempfile
import unittest
from reuse_source import same_product_tree, verify_binary


class SourceProofTests(unittest.TestCase):
    def test_only_controller_and_artifact_changes_can_reuse_a_build(self):
        with tempfile.TemporaryDirectory() as temporary:
            root=Path(temporary)
            def git(*words):
                return subprocess.check_output(['git','-C',str(root),*words],text=True).strip()
            git('init','-q');git('config','user.name','Fixture');git('config','user.email','fixture@example.invalid')
            files=['cli/src/main.rs','skill-data/core/SKILL.md','extensions/ab-connect/background.js',
                'package.json','bench/bitwarden-780/probe.py','.github/workflows/windows-capture-probe.yml']
            for name in files:
                path=root/name;path.parent.mkdir(parents=True,exist_ok=True);path.write_text('original\n')
            git('add','.');git('commit','-qm','fixture');source=git('rev-parse','HEAD')
            (root/'bench/bitwarden-780/probe.py').write_text('controller change\n')
            (root/'.github/workflows/windows-capture-probe.yml').write_text('workflow change\n')
            (root/'probe-artifacts').mkdir();(root/'probe-artifacts/log.txt').write_text('artifact')
            self.assertTrue(same_product_tree(root,source))
            for name in files[:4]:
                path=root/name;path.write_text('product change\n')
                self.assertFalse(same_product_tree(root,source),name)
                path.write_text('original\n')
            path=root/'cli/src/new.rs';path.write_text('untracked product source')
            self.assertFalse(same_product_tree(root,source))
            path.unlink();self.assertTrue(same_product_tree(root,source))

    def test_binary_receipt_must_match_the_downloaded_bytes(self):
        with tempfile.TemporaryDirectory() as temporary:
            root=Path(temporary);binary=root/'source-cli.exe';binary.write_bytes(b'fixture artifact')
            sha=hashlib.sha256(binary.read_bytes()).hexdigest();receipt=root/'binary-sha256.txt'
            receipt.write_text('Hash      : '+sha.upper()+'\n')
            self.assertEqual(verify_binary(binary,receipt),sha)
            binary.write_bytes(b'changed artifact')
            with self.assertRaisesRegex(ValueError,'checksum'):verify_binary(binary,receipt)
            receipt.write_text('missing hash')
            with self.assertRaisesRegex(ValueError,'exactly one'):verify_binary(binary,receipt)


if __name__=='__main__':unittest.main()

#!/usr/bin/env python3
"""Pure fixture/generator checks. This test never starts a browser."""
import importlib.util
import json
import os
import sys
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest import mock
import urllib.error
import urllib.request

spec = importlib.util.spec_from_file_location("capture_probe", Path(__file__).with_name("probe.py"))
probe = importlib.util.module_from_spec(spec)
spec.loader.exec_module(probe)


class ProbeTests(unittest.TestCase):
    def test_fixture_has_exact_work_and_accepts_trace(self):
        fixture = probe.Fixture(123)
        try:
            report = urllib.request.urlopen(fixture.base + "/report?test").read().decode()
            warm = urllib.request.urlopen(fixture.base + "/warmup?test").read().decode()
            self.assertEqual(report.count('<div class=row>'), 123)
            self.assertEqual(warm.count('<div class=row>'), 1)
            event = {"caseId": "fixture", "type": "dom-answer", "result": {"rows": 123}}
            request = urllib.request.Request(fixture.base + "/trace", data=json.dumps(event).encode(),
                headers={"Content-Type": "application/json"}, method="POST")
            self.assertEqual(urllib.request.urlopen(request).status, 204)
            self.assertEqual(fixture.events("fixture")[0]["result"]["rows"], 123)
            self.assertEqual(fixture.events("other"), [])
        finally:
            fixture.close()

    def test_generated_observer_keeps_production_capture_and_is_fixture_guarded(self):
        root = Path(__file__).resolve().parents[2]
        with tempfile.TemporaryDirectory() as tmp:
            target = Path(tmp) / "relay"
            identity, diff, digest = probe.instrument(root / "extensions/ab-connect", target,
                "http://127.0.0.1:12345", "case")
            source = (target / "background.js").read_text()
            self.assertEqual(identity, "ciiljdlhdpfckdcfkphgmfalanpdejep")
            self.assertEqual(len(digest), 64)
            self.assertIn("sendTabCommand(tabId, method, params, childSessionId, tabCommandDependencies())", source)
            self.assertIn("ownedTabs.has(tabId)", diff)
            self.assertIn("Math.max(0, 2000 - (performance.now() - started))", diff)
            self.assertNotIn("Input.dispatch", diff)
            self.assertNotIn("Runtime.enable", diff)
            self.assertEqual(json.loads((target / "manifest.json").read_text())["host_permissions"], ["http://127.0.0.1/*"])
            subprocess.run(["node", "--check", str(target / "background.js")], check=True)
            observer = source[source.index("async function observeFixtureCapture"):source.index(probe.START_ANCHOR)]
            driver = r'''
const vm = require('node:vm');
const assert = require('node:assert/strict');
const source = process.env.PROBE_OBSERVER;
async function run(owned, url, resolveEarly) {
  const emitted = [], reads = [], timers = [];
  let resolve;
  const operation = new Promise(r => { resolve = r; });
  const c = { performance: {now: () => 10}, crypto: {randomUUID: () => 'capture'}, URL, ownedTabs: new Set(owned ? [7] : []),
    fetch: async (url, options) => { emitted.push(JSON.parse(options.body)); },
    setTimeout: fn => { timers.push(fn); return timers.length; }, clearTimeout() {},
    chrome: { tabs: {get: async () => ({url})}, debugger: {sendCommand: async (target, method, args) => {
      reads.push({target, method, args}); return {result:{value:{rows:123,ready:'complete'}}};
    }}},
  };
  vm.createContext(c); vm.runInContext(source, c);
  await c.observeFixtureCapture(7, {format:'png'}, operation);
  if (resolveEarly) { resolve({data:'fake'}); await Promise.resolve(); }
  if (timers[0]) await timers[0]();
  resolve({data:'fake'}); await Promise.resolve();
  return {emitted, reads};
}
(async () => {
  assert.equal((await run(false,'http://127.0.0.1:12345/report',false)).reads.length,0);
  assert.equal((await run(true,'http://127.0.0.1:12345.evil.test/report',false)).reads.length,0);
  assert.equal((await run(true,'http://127.0.0.1:12345/report',true)).reads.length,0);
  const live = await run(true,'http://127.0.0.1:12345/report',false);
  const enabled = process.env.PROBE_EXPECT_DOM !== 'false';
  assert.equal(live.reads.length, enabled ? 1 : 0);
  if (enabled) {
    assert.equal(live.reads[0].method,'Runtime.evaluate');
    assert.equal(live.emitted.find(x=>x.type==='dom-answer').captureStillPending,true);
  }
})().catch(e=>{console.error(e);process.exitCode=1});
'''
            env = dict(__import__('os').environ, PROBE_OBSERVER=observer)
            subprocess.run(["node", "-e", driver], env=env, check=True)
            disabled = Path(tmp) / "without-dom-control"
            probe.instrument(root / "extensions/ab-connect", disabled,
                "http://127.0.0.1:12345", "case", pending_dom_control=False)
            source = (disabled / "background.js").read_text()
            env.update(PROBE_OBSERVER=source[source.index("async function observeFixtureCapture"):source.index(probe.START_ANCHOR)], PROBE_EXPECT_DOM="false")
            subprocess.run(["node", "-e", driver], env=env, check=True)

    def test_main_capture_trace_must_be_present_and_correlated(self):
        start = {"type": "capture-start", "captureId": "main", "fixturePath": "/report"}
        warm = {"type": "capture-start", "captureId": "warm", "fixturePath": "/warmup"}
        self.assertFalse(probe.capture_trace_complete([]))
        self.assertFalse(probe.capture_trace_complete([warm, {"type": "capture-answer", "captureId": "warm"}]))
        self.assertFalse(probe.capture_trace_complete([start, {"type": "capture-answer", "captureId": "other"}]))
        self.assertTrue(probe.capture_trace_complete([start, {"type": "capture-error", "captureId": "main"}]))

    def test_chrome_version_uses_file_metadata_without_starting_chrome(self):
        binary = Path("C:/fixture/tool $(not-code).exe")
        with mock.patch.dict(os.environ, {"SystemRoot": "C:/Windows"}), mock.patch.object(
                probe.subprocess, "check_output", return_value="156.0.1\n") as called:
            self.assertEqual(probe.file_version(binary), "156.0.1")
        args, kwargs = called.call_args
        self.assertTrue(args[0][0].endswith("powershell.exe"))
        self.assertNotEqual(args[0][0], str(binary))
        self.assertEqual(kwargs["env"]["PROBE_VERSION_EXE"], str(binary))
        self.assertNotIn(str(binary), args[0][-1])

    def test_host_environment_guard_prevents_local_browser_execution(self):
        result = subprocess.run([sys.executable, str(Path(__file__).with_name("probe.py")),
            "--binary", "/does-not-exist", "--chrome", "/does-not-exist", "--extension", ".", "--output", "/does-not-exist"],
            capture_output=True, text=True, env=dict(os.environ, GITHUB_ACTIONS="false"))
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("ephemeral Windows GitHub Actions", result.stderr)


if __name__ == "__main__":
    unittest.main()

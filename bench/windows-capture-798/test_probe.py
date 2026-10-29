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
            fixture.set_control("fixture", lambda: {"valid": True, "snapshot": {"window": {"hwnd": 99}}})
            request = urllib.request.Request(fixture.base + "/dispatch", data=json.dumps({"caseId":"fixture"}).encode(),
                headers={"Content-Type":"application/json"}, method="POST")
            dispatch = json.load(urllib.request.urlopen(request))
            self.assertTrue(dispatch["valid"])
            self.assertEqual(dispatch["snapshot"]["window"]["hwnd"], 99)
            self.assertGreater(dispatch["sampledMonotonic"], 0)

        finally:
            fixture.close()

    def test_actual_native_edge_and_recovery_are_not_confused(self):
        root = Path(__file__).resolve().parents[2]
        with tempfile.TemporaryDirectory() as tmp:
            target = Path(tmp) / "relay"
            identity, diff, digest = probe.instrument(root / "extensions/ab-connect", target,
                "http://127.0.0.1:12345", "case")
            source = (target / "background.js").read_text()
            self.assertEqual(identity, "ciiljdlhdpfckdcfkphgmfalanpdejep")
            self.assertEqual(len(digest), 64)
            self.assertIn("fixtureSendCommand(captureContext, target, command, args)", source)
            self.assertNotIn("Input.dispatch", diff)
            subprocess.run(["node", "--check", str(target / "background.js")], check=True)
            helper = source[source.index("// Generated CI fixture observer"):source.index("const HOST_NAME =")]
            start = source.index(probe.START_ANCHOR)
            sender = source[start:source.index("function anyConnectedTab", start)]
            env = dict(os.environ, PROBE_HELPER=helper, PROBE_SENDER=sender,
                PROBE_COMMAND_MODULE=(root / "extensions/ab-connect/tab-command.js").as_uri())
            subprocess.run(["node", str(Path(__file__).with_name("test-relay-observer.cjs"))], env=env, check=True)
            disabled = Path(tmp) / "without-dom-control"
            probe.instrument(root / "extensions/ab-connect", disabled, "http://127.0.0.1:12345", "case", False)
            source = (disabled / "background.js").read_text()
            start = source.index(probe.START_ANCHOR)
            env.update(PROBE_HELPER=source[source.index("// Generated CI fixture observer"):source.index("const HOST_NAME =")],
                PROBE_SENDER=source[start:source.index("function anyConnectedTab", start)], PROBE_EXPECT_DOM="false")
            subprocess.run(["node", str(Path(__file__).with_name("test-relay-observer.cjs"))], env=env, check=True)


    def good_record(self):
        import copy
        target = {"tabId": 7, "sessionId": None}
        window = {"hwnd": 99, "pid": 1, "title": "CaptureProbe case", "minimized": False,
                  "visible": True, "centerOwned": True, "centerWindow": 99, "rect": [0, 0, 100, 100]}
        snapshot = {"window": window, "cover": None}
        start = {"type": "capture-operation-start", "operationId": "op", "fixturePath": "/report", "epoch": 3, "requested": copy.deepcopy(target)}
        native = {"type": "capture-native-send", "operationId": "op", "captureId": "capture", "target": copy.deepcopy(target),
                  "epoch": 3, "windowControl": {"valid": True, "snapshot": copy.deepcopy(snapshot)}, "controlElapsedMs": 2,
                  "historyLost": False, "attachmentHistory": [{"atMs":0,"reason":"attach-acknowledged","epoch":3}]}
        end = {"type": "capture-wrapper-answer", "operationId": "op", "epoch": 3, "invalidations": [],
               "atMs": 30, "attempts": [{"captureId": "capture", "target": copy.deepcopy(target), "epoch": 3, "nativePending": False, "elapsedMs": 10}]}
        warm_start = dict(start, operationId="warm", fixturePath="/warmup", atMs=1)
        warm_native = dict(copy.deepcopy(native), operationId="warm", fixturePath="/warmup", atMs=2)
        warm_end = dict(copy.deepcopy(end), operationId="warm", atMs=3)
        return {"caseId": "case", "state": "normal", "chromePid": 1, "probePid": 2, "expectedHwnd": 99,
                "setupVerified": True, "traceVerified": True, "windowBefore": copy.deepcopy(snapshot), "windowAfter": copy.deepcopy(snapshot),
                "trace": [start, native, end, warm_start, warm_native, warm_end], "capture": {"exit": 0, "killedAtDeadline": False, "response": {"success": True}},
                "image": {"pixelValid": True, "dimensionsValid": True}}

    def test_case_verdict_rejects_changed_state_exit_kill_exception_and_unknown_trace(self):
        import copy
        good = self.good_record()
        self.assertEqual(probe.case_verdict(good), "PASS")
        variants = []
        changed = copy.deepcopy(good)
        changed["windowAfter"]["window"]["minimized"] = True
        variants.append(changed)
        changed = copy.deepcopy(good)
        changed["trace"][1]["windowControl"]["snapshot"]["window"]["hwnd"] = 100
        variants.append(changed)
        changed = copy.deepcopy(good)
        changed["trace"][2]["epoch"] = 4
        variants.append(changed)
        changed = copy.deepcopy(good)
        changed["trace"][2]["invalidations"] = ["attach-attempt"]
        variants.append(changed)
        changed = copy.deepcopy(good)
        changed["trace"].pop(1)
        variants.append(changed)
        changed = copy.deepcopy(good)
        changed["trace"][1]["attachmentHistory"].append({"atMs":10,"reason":"attach-attempt","epoch":3})
        variants.append(changed)
        changed = copy.deepcopy(good)
        changed["trace"][1]["historyLost"] = True
        variants.append(changed)

        variants.extend([dict(good, error="unexpected exception"), dict(good, error=""), dict(good, traceVerified=False),
                         dict(good, browserCleanupError="not confirmed")])
        for variant in variants:
            self.assertEqual(probe.case_verdict(variant), "INCONCLUSIVE")
        for capture in [dict(good["capture"], exit=1), dict(good["capture"], killedAtDeadline=True),
                        dict(good["capture"], response={"success": False}), dict(good["capture"], response={"success": "true"})]:
            self.assertEqual(probe.case_verdict(dict(good, capture=capture)), "ISSUES")

    def test_only_the_fixture_created_cover_and_original_chrome_hwnd_are_accepted(self):
        import copy
        record = self.good_record()
        record["state"] = "covered"
        snapshot = copy.deepcopy(record["windowBefore"])
        snapshot["window"]["centerOwned"] = False
        snapshot["window"]["centerWindow"] = 100
        snapshot["cover"] = {"hwnd": 100, "pid": 2, "title": "Fixture cover case", "visible": True, "rect": [0,0,100,100]}
        self.assertTrue(probe.window_valid(snapshot, "covered", "case", 1, 99, 2))
        snapshot["cover"]["pid"] = 3
        self.assertFalse(probe.window_valid(snapshot, "covered", "case", 1, 99, 2))
        snapshot["cover"]["pid"] = 2
        snapshot["window"]["hwnd"] = 101
        self.assertFalse(probe.window_valid(snapshot, "covered", "case", 1, 99, 2))

    def test_main_capture_trace_and_timeout_require_one_native_attempt_in_the_same_epoch(self):
        import copy
        record = self.good_record()
        self.assertTrue(probe.capture_trace_complete(record["trace"]))
        self.assertFalse(probe.capture_trace_complete([e for e in record["trace"] if e.get("operationId") != "op" or e.get("type") != "capture-wrapper-answer"]))
        end = record["trace"][2]
        end.update(type="capture-wrapper-error", error="relay timeout after 25000ms: chrome.debugger.sendCommand(Page.captureScreenshot)")
        end["attempts"][0].update(nativePending=True, elapsedMs=25001)
        dom = {"type": "dom-native-answer", "operationId": "op", "captureId": "capture", "target": record["trace"][1]["target"],
               "epoch": 3, "nativeStillPending": True, "wrapperStillPending": True, "result": {"rows":123,"ready":"complete"}}
        record["trace"].append(dom)
        self.assertTrue(probe.report_signature_matches(record["trace"], 123, record))
        for field, value in [("epoch",4),("nativeStillPending",False),("wrapperStillPending",False)]:
            changed = copy.deepcopy(record)
            changed["trace"][-1][field] = value
            self.assertFalse(probe.report_signature_matches(changed["trace"],123,changed))
        changed = copy.deepcopy(record)
        changed["trace"][2]["attempts"].append(changed["trace"][2]["attempts"][0])
        self.assertFalse(probe.report_signature_matches(changed["trace"],123,changed))

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

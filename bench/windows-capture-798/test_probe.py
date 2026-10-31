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
        fixture.register("fixture", Path(tempfile.gettempdir()))
        try:
            report = urllib.request.urlopen(fixture.base + "/report?test").read().decode()
            warm = urllib.request.urlopen(fixture.base + "/warmup?test").read().decode()
            self.assertEqual(report.count('<div class="row"'), 123)
            self.assertEqual(warm.count('<div class="row"'), 1)
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

            for candidate in ("baseline", "view", "focus"):
                destination=Path(tmp)/candidate
                probe.instrument(root/"extensions/ab-connect",destination,"http://127.0.0.1:12345","case",False,candidate)
                generated=(destination/"background.js").read_text()
                subprocess.run(["node","--check",str(destination/"background.js")],check=True)
                candidate_env=dict(env,PROBE_HELPER=generated[generated.index("// Generated CI fixture observer"):generated.index("const HOST_NAME =")],PROBE_CANDIDATE=candidate)
                subprocess.run(["node",str(Path(__file__).with_name("test-candidates.cjs"))],env=candidate_env,check=True)


    def test_late_delivery_cannot_hide_a_pre_teardown_detach(self):
        record = self.good_record()
        for event in record["trace"]:
            event["receivedMonotonic"] = 1
        record["trace"].append({"type": "host-cleanup-start", "receivedMonotonic": 2})
        detach = {"type": "attachment-change", "operationId": "op", "atMs": 101, "receivedMonotonic": 3}
        record["trace"].append(detach)
        self.assertIsNotNone(probe.capture_context(record["trace"], record))
        detach["atMs"] = 99
        self.assertIsNone(probe.capture_context(record["trace"], record))

    def test_missing_or_unacknowledged_worker_fence_is_unverified(self):
        import copy
        good = self.good_record()
        for change in ["missing", "unacked", "wrong-worker", "wrong-time", "duplicate-operation", "nonarray"]:
            record = copy.deepcopy(good)
            fence = next(e for e in record["trace"] if e.get("type") == "worker-teardown-start")
            if change == "missing": record["trace"].remove(fence)
            elif change == "unacked": fence["traceComplete"] = False
            elif change == "wrong-time": fence["atMs"] = False
            elif change == "duplicate-operation": fence["operations"].append("op")
            elif change == "nonarray": fence["operations"] = "op warm"
            else: fence["operations"] = ["different-operation"]
            self.assertIsNone(probe.capture_context(record["trace"], record), change)

    def isolated_record(self, verdict="PASS"):
        return {"verdict": verdict, "setupVerified": True, "captureAttempted": True,
            "sessionCleanup": {"exit": 0, "killedAtDeadline": False, "response": {"success": True}},
            "browserTreeTerminated": True, "browserTreeKillExit": 0, "browserExitCode": 1,
            "browserCleanup": {"taskkill": {"exit": 0, "stderr": ""}, "parentExit": 1,
                "observedSurvivors": [], "postTree": [],
                "before": [{"ProcessId": 7, "ParentProcessId": 1, "CreationDate": "/Date(100)/"}],
                "ownedHandleExits": [{"pid": 7, "parentPid": 1, "bindingConfirmed": True, "signaled": True,
                    "handleClosed": True, "expectedCreatedMs": 100, "actualCreatedMs": 100, "waitCode": 0, "exitFileTime": 200}]}}

    def test_matrix_continuation_accepts_only_kernel_verified_already_exited_error(self):
        record = self.isolated_record("INCONCLUSIVE")
        record["browserTreeKillExit"] = 255
        record["browserCleanup"]["taskkill"] = {"exit": 255, "stderr":
            "ERROR: The process with PID 7 (child process of PID 1) could not be terminated.\nReason: There is no running instance of the task.\n"}
        self.assertTrue(probe.can_start_next_case(record))
        record["browserCleanup"]["ownedHandleExits"][0]["signaled"] = False
        self.assertFalse(probe.can_start_next_case(record))

    def test_uncertain_measurement_can_continue_only_after_confirmed_isolation(self):
        import copy
        record = self.isolated_record("INCONCLUSIVE")
        self.assertTrue(probe.can_start_next_case(record))
        for field in ["setupVerified", "captureAttempted", "browserTreeTerminated", "browserTreeKillExit", "browserExitCode"]:
            broken = copy.deepcopy(record)
            broken.pop(field)
            self.assertFalse(probe.can_start_next_case(broken), field)
        for update in [{"browserCleanupError": "unknown"}, {"browserTreeKillExit": 1}, {"browserTreeKillExit": False}, {"browserExitCode": None}, {"browserExitCode": False}]:
            self.assertFalse(probe.can_start_next_case(dict(record, **update)))
        for update in [{"exit": 1}, {"exit": False}, {"killedAtDeadline": True}, {"response": {"success": False}}]:
            broken = copy.deepcopy(record)
            broken["sessionCleanup"].update(update)
            self.assertFalse(probe.can_start_next_case(broken))

    def test_completed_matrix_retains_unknowns_and_never_promotes_them_to_pass(self):
        records = [self.isolated_record(v) for v in ["PASS", "ISSUES", "INCONCLUSIVE"]]
        summary = probe.matrix_summary(records, 3, None)
        self.assertEqual(summary["verdict"], "INCONCLUSIVE")
        self.assertTrue(summary["allCasesExecuted"])
        self.assertFalse(summary["matrixComplete"])
        self.assertEqual(summary["inconclusiveCases"], 1)
        self.assertEqual(summary["captureFailures"], 1)
        self.assertEqual(probe.matrix_summary([self.isolated_record()], 1, None)["verdict"], "PASS")
        self.assertEqual(probe.matrix_summary([self.isolated_record("ISSUES")], 1, None)["verdict"], "ISSUES")
        self.assertEqual(probe.matrix_summary([self.isolated_record()], 2, None)["verdict"], "INCONCLUSIVE")
        broken = self.isolated_record()
        broken.pop("browserTreeTerminated")
        self.assertEqual(probe.matrix_summary([broken], 1, None)["verdict"], "INCONCLUSIVE")
        self.assertEqual(probe.matrix_summary([self.isolated_record()], 1, "setup failed")["verdict"], "INCONCLUSIVE")

    def good_record(self):
        import copy
        target = {"tabId": 7, "sessionId": None}
        window = {"hwnd": 99, "pid": 1, "title": "CaptureProbe case", "minimized": False,
                  "visible": True, "centerOwned": True, "centerWindow": 99, "rect": [0, 0, 100, 100]}
        snapshot = {"window": window, "cover": None}
        start = {"type": "capture-operation-start", "operationId": "op", "fixturePath": "/report", "epoch": 3, "atMs": 10, "requested": copy.deepcopy(target)}
        native = {"type": "capture-native-send", "operationId": "op", "captureId": "capture", "target": copy.deepcopy(target),
                  "epoch": 3, "atMs": 20, "windowControl": {"valid": True, "snapshot": copy.deepcopy(snapshot)}, "controlElapsedMs": 2,
                  "historyLost": False, "attachmentHistory": [{"atMs":0,"reason":"attach-acknowledged","epoch":3}]}
        end = {"type": "capture-wrapper-answer", "operationId": "op", "epoch": 3, "invalidations": [],
               "atMs": 30, "attempts": [{"captureId": "capture", "target": copy.deepcopy(target), "epoch": 3, "nativePending": False, "elapsedMs": 10}]}
        warm_start = dict(start, operationId="warm", fixturePath="/warmup", atMs=1)
        warm_native = dict(copy.deepcopy(native), operationId="warm", fixturePath="/warmup", atMs=2)
        warm_end = dict(copy.deepcopy(end), operationId="warm", atMs=3)
        return {"caseId": "case", "state": "normal", "chromePid": 1, "probePid": 2, "expectedHwnd": 99,
                "setupVerified": True, "traceVerified": True, "windowBefore": copy.deepcopy(snapshot), "windowAfter": copy.deepcopy(snapshot),
                "trace": [start, native, end, warm_start, warm_native, warm_end,
                    {"type": "worker-teardown-start", "atMs": 100, "traceComplete": True, "operations": ["op", "warm"]}], "capture": {"exit": 0, "killedAtDeadline": False, "response": {"success": True}},
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
               "epoch": 3, "atMs": 40, "nativeStillPending": True, "wrapperStillPending": True, "result": {"rows":123,"ready":"complete"}}
        record["trace"].append(dom)
        self.assertTrue(probe.report_signature_matches(record["trace"], 123, record))
        for field, value in [("epoch",4),("nativeStillPending",False),("wrapperStillPending",False)]:
            changed = copy.deepcopy(record)
            changed["trace"][-1][field] = value
            self.assertFalse(probe.report_signature_matches(changed["trace"],123,changed))
        changed = copy.deepcopy(record)
        changed["trace"][2]["attempts"].append(changed["trace"][2]["attempts"][0])
        self.assertFalse(probe.report_signature_matches(changed["trace"],123,changed))

    def test_full_raster_uses_native_clip_and_document_extent_not_inner_width(self):
        geometry = {"dpr":1,"width":1280,"height":720,"scrollWidth":1265,"scrollHeight":32288,
                    "target":{"width":1265,"height":128}}
        request = {"clip":{"x":0,"y":0,"width":1265,"height":32288,"scale":1}}
        self.assertEqual(probe.expected_raster("full", geometry, request), (1265,32288))
        self.assertEqual(probe.expected_raster("selector", geometry, request), (1265,128))
        self.assertEqual(probe.expected_raster("viewport", geometry, request), (1280,720))
        geometry["dpr"] = 2
        self.assertEqual(probe.expected_raster("full", geometry, request), (2530,64576))
        request["clip"]["height"] = 720
        with self.assertRaisesRegex(RuntimeError,"document extent"):
            probe.expected_raster("full", geometry, request)
        with self.assertRaisesRegex(RuntimeError,"finite native clip"):
            probe.expected_raster("full", geometry, {})

    def candidate_record(self):
        import copy
        record = self.good_record()
        record.update(candidateExperiment=True, candidate="baseline", generation="5aa55aa5",generationStartedTick=1010,generationConfirmedTick=1030)
        record["state"] = "minimized"
        for key in ("windowBefore", "windowAfter"):
            record[key]["window"].update(minimized=True, foreground=456)
        sent = record["trace"][1]
        sent.update(candidate="baseline", request={"fromSurface":True},sentMs=10)
        sent["windowControl"]["snapshot"] = copy.deepcopy(record["windowBefore"])
        page = {"generation":"5aa55aa5", "events":[], "globals":["fixtureState"], "mutations":100, "requests":[]}
        record["pageBefore"] = page
        record["trace"].append({"type":"page-after-answer", "operationId":"op", "captureId":"capture",
            "target":copy.deepcopy(sent["target"]), "epoch":3, "atMs":40, "result":copy.deepcopy(page)})
        record["trace"][2]["attempts"][0]["nativeOutcome"] = "answer"
        record["trace"].extend([{"type":"capture-native-answer","operationId":"op","captureId":"capture",
            "target":copy.deepcopy(sent["target"]),"epoch":3,"responseOwned":True,"nativeReceivedMs":20,"ownershipVerifiedMs":21,"atMs":22},
            {"type":"native-image-ack","operationId":"op","captureId":"capture",
            "target":copy.deepcopy(sent["target"]),"epoch":3,"atMs":35,"saved":{"sha256":"f"*64}}])
        record["nativeImage"]={"sha256":"f"*64,"freshnessValid":True}
        record["nativeMonitor"] = {"calibrated":True,"installed":4,"unhooked":[True]*4,
            "pumpStoppedExpected":True,"overflow":False,"errors":[],"startTick":1000,"endTick":1100,"events":[],
            "samples":[{"tick":t,"identityValid":True,"window":copy.deepcopy(record["windowBefore"]["window"])} for t in (1000,1020,1040,1060,1080,1100)]}
        record["image"]["freshnessValid"] = True
        return record

    def test_native_monitor_rejects_zero_scope_before_loading_windows_apis(self):
        from native_monitor import NativeWindowMonitor
        for hwnd,pid in [(0,1),(1,0),(-1,1),(1,-1)]:
            with self.assertRaisesRegex(ValueError,"nonzero owned"):
                NativeWindowMonitor(None,hwnd,pid,"fixture")

    def test_native_measurement_start_cannot_be_reset_after_generation(self):
        from native_monitor import NativeWindowMonitor
        monitor=NativeWindowMonitor.__new__(NativeWindowMonitor)
        monitor.start_tick=None
        monitor.calibrated=lambda:True
        monitor.windows=mock.Mock()
        monitor.windows.k.GetTickCount64.return_value=1000
        monitor.begin_measurement()
        with self.assertRaisesRegex(RuntimeError,"cannot be reset"):
            monitor.begin_measurement()

    def test_candidate_requires_native_and_page_observation_without_hiding_cli_failures(self):
        import copy
        good = self.candidate_record()
        self.assertEqual(probe.case_verdict(good),"PASS")
        mutations = [lambda r:r["nativeMonitor"]["events"].append({"event":0x17,"tick":1050}),
            lambda r:r["nativeMonitor"]["events"].append({"event":0x17,"tick":1020}),
            lambda r:r.update(generationStartedTick=999),
            lambda r:r["nativeMonitor"]["samples"][1]["window"].update(minimized=False),
            lambda r:r["nativeMonitor"].update(pumpStoppedExpected=False),
            lambda r:r["nativeMonitor"].update(overflow=True),
            lambda r:r["nativeMonitor"]["errors"].append("callback error"),
            lambda r:r["nativeMonitor"].update(unhooked=[True,False,True,True]),
            lambda r:next(e for e in r["trace"] if e["type"]=="page-after-answer")["result"]["events"].append({"type":"focus"}),
            lambda r:next(e for e in r["trace"] if e["type"]=="page-after-answer")["result"].update(mutations=101),
            lambda r:r["trace"][1]["request"].update(fromSurface=False),
            lambda r:r.update(diagnosticError="flush deadline"),
            lambda r:r.update(trace=[e for e in r["trace"] if e["type"]!="capture-native-answer"]),
            lambda r:r.update(trace=[e for e in r["trace"] if e["type"]!="native-image-ack"]),
            lambda r:r["nativeImage"].update(sha256="wrong"),
            lambda r:r["trace"].append({"type":"native-image-error","operationId":"op"})]
        for mutate in mutations:
            bad = copy.deepcopy(good); mutate(bad)
            self.assertEqual(probe.case_verdict(bad),"INCONCLUSIVE")
        for mutate in [lambda r:r["capture"].update(exit=1), lambda r:r["image"].update(freshnessValid=False)]:
            bad=copy.deepcopy(good);mutate(bad)
            self.assertEqual(probe.case_verdict(bad),"ISSUES")
        bad=copy.deepcopy(good)
        bad["capture"]["exit"]=1
        bad["nativeImage"].update(freshnessValid=True,pixelValid=True,dimensionsValid=True)
        self.assertEqual(probe.case_verdict(bad),"ISSUES","raw PNG must not mask CLI failure")

    def test_known_pending_timeout_needs_no_png_but_cannot_masquerade_as_cli_success(self):
        record=self.candidate_record()
        end=record["trace"][2]
        end.update(type="capture-wrapper-error",error="relay timeout after 25000ms")
        end["attempts"][0].update(nativePending=True,nativeOutcome=None)
        record["trace"]=[e for e in record["trace"] if e["type"] not in ("capture-native-answer","native-image-ack")]
        record.pop("nativeImage")
        record["capture"].update(exit=1,response={"success":False})
        self.assertEqual(probe.case_verdict(record),"ISSUES")
        record["capture"].update(exit=0,response={"success":True})
        self.assertEqual(probe.case_verdict(record),"INCONCLUSIVE")

    def test_native_png_sink_requires_registered_attempt_and_cannot_overwrite(self):
        import base64,io,uuid,copy
        from PIL import Image
        fixture=probe.Fixture(123)
        with tempfile.TemporaryDirectory() as tmp:
            fixture.register("image-case",Path(tmp))
            op,cap=str(uuid.uuid4()),str(uuid.uuid4())
            target={"tabId":7,"sessionId":None}
            trace={"caseId":"image-case","operationId":op,"captureId":cap,"target":target,"epoch":2}
            fixture.traces.extend([dict(trace,type="capture-native-send",fixturePath="/report"),dict(trace,type="capture-native-answer",responseOwned=True)])
            data=io.BytesIO();Image.new("RGB",(8,8),(0,0,0)).save(data,format="PNG")
            event=dict(trace,data=base64.b64encode(data.getvalue()).decode())
            def post(e):
                return urllib.request.urlopen(urllib.request.Request(fixture.base+"/native-image",data=json.dumps(e).encode(),headers={"Content-Type":"application/json"},method="POST"))
            for change in [{"caseId":"unknown"},{"captureId":"../file"},{"epoch":3},{"data":"invalid"}]:
                with self.assertRaises(urllib.error.HTTPError) as caught:
                    post(dict(event,**change))
                caught.exception.close()
            response=json.load(post(event));self.assertTrue(response["saved"])
            self.assertEqual((Path(tmp)/"native.png").read_bytes(),data.getvalue())
            with self.assertRaises(urllib.error.HTTPError) as caught:
                post(event)
            caught.exception.close()
        fixture.close()

    def test_png_generation_is_decoded_from_pixels_not_dom_or_static_sentinels(self):
        from PIL import Image,ImageDraw
        with tempfile.TemporaryDirectory() as tmp:
            p=Path(tmp)/"frame.png"
            image=Image.new("RGB",(1280,720),(52,120,193));draw=ImageDraw.Draw(image)
            draw.rectangle((0,0,1279,159),fill=(18,171,52))
            generation="5aa55aa5"
            for bit in range(32):
                white=(int(generation,16)>>(31-bit))&1
                draw.rectangle((32+bit*8,16,39+bit*8,31),fill=(255,255,255) if white else (0,0,0))
            image.save(p)
            geo={"dpr":1,"width":1280,"height":720}
            self.assertTrue(probe.inspect_image(p,"viewport",geo,{},generation)["freshnessValid"])
            self.assertFalse(probe.inspect_image(p,"viewport",geo,{},"00000000")["freshnessValid"])

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

    def test_taskkill_missing_shell_requires_retained_identity_and_exit_proof(self):
        import copy
        from owned_processes import cleanup_acknowledged
        owned = [{"ProcessId": 7, "ParentProcessId": 1, "CreationDate": "/Date(100)/"},
            {"ProcessId": 8, "ParentProcessId": 7, "CreationDate": "/Date(100)/"}]
        handles = [{"pid": p["ProcessId"], "parentPid": p["ParentProcessId"], "bindingConfirmed": True,
            "signaled": True, "handleClosed": True, "expectedCreatedMs": 100, "actualCreatedMs": 100,
            "waitCode": 0, "exitFileTime": 200} for p in owned]
        result = {"exit": 255, "stderr": "ERROR: The process with PID 8 (child process of PID 7) could not be terminated.\nReason: There is no running instance of the task.\n\n"}
        self.assertTrue(cleanup_acknowledged(result, owned, handles, 1, []))
        self.assertTrue(cleanup_acknowledged({"exit": 0, "stderr": ""}, owned, handles, 1, []))
        for field in ["bindingConfirmed", "signaled", "handleClosed", "actualCreatedMs", "exitFileTime"]:
            broken = copy.deepcopy(handles)
            broken[-1][field] = False
            self.assertFalse(cleanup_acknowledged(result, owned, broken, 1, []), field)
        for stderr in [result["stderr"].replace("PID 8", "PID 99"),
                result["stderr"].replace("child process of PID 7", "child process of PID 99"),
                result["stderr"].replace("There is no running instance of the task.", "Access is denied."),
                result["stderr"] + "another error"]:
            self.assertFalse(cleanup_acknowledged(dict(result, stderr=stderr), owned, handles, 1, []))
        self.assertFalse(cleanup_acknowledged(result, owned, handles[:-1], 1, []))
        self.assertFalse(cleanup_acknowledged(result, owned, handles, None, []))
        self.assertFalse(cleanup_acknowledged(result, owned, handles, 1, [owned[-1]]))

    def test_cleanup_summary_refuses_a_last_sample_error_or_survivor(self):
        import copy
        from cleanup_probe import diagnostic_pass
        good = {"parentExit": 1, "taskkill": {"exit": 0, "stdout": "ack", "stderr": ""},
            "observedSurvivors": [], "postTree": [], "cleanupAcknowledged": True}
        good["before"] = [{"ProcessId": 7, "ParentProcessId": 1, "CreationDate": "/Date(100)/"}]
        good["ownedHandleExits"] = [{"pid": 7, "parentPid": 1, "bindingConfirmed": True, "signaled": True,
            "handleClosed": True, "expectedCreatedMs": 100, "actualCreatedMs": 100, "waitCode": 0, "exitFileTime": 200}]
        records = [copy.deepcopy(good) for _ in range(3)]
        self.assertTrue(diagnostic_pass(records))
        self.assertFalse(diagnostic_pass(records, relay=True))
        for record in records:
            record["relayReady"] = True
        self.assertTrue(diagnostic_pass(records, relay=True))
        records[-1]["relayReady"] = False
        self.assertFalse(diagnostic_pass(records, relay=True))
        records[-1]["relayReady"] = True
        for update in [{"error": "failed"}, {"fallbackCleanupError": "failed"},
                {"observedSurvivors": [{"ProcessId": 2}]}, {"postTree": [{"ProcessId": 2}]},
                {"parentExit": None}, {"cleanupAcknowledged": False}]:
            broken = copy.deepcopy(records)
            broken[-1].update(update)
            self.assertFalse(diagnostic_pass(broken), update)
        self.assertFalse(diagnostic_pass(records[:2]))
        broken = copy.deepcopy(records)
        broken[-1].pop("observedSurvivors")
        self.assertFalse(diagnostic_pass(broken))

    def test_cleanup_process_tree_ignores_reused_parent_pids(self):
        from cleanup_probe import descendants
        def process(pid, parent, created):
            return {"ProcessId": pid, "ParentProcessId": parent, "CreationDate": f"/Date({created})/"}
        root, child, older, grandchild = process(7, 1, 100), process(8, 7, 110), process(9, 8, 50), process(10, 8, 120)
        self.assertEqual(descendants([root, child, older, grandchild], 7), [root, child, grandchild])
        self.assertEqual(descendants([grandchild], 7, [root, child]), [grandchild])
        with self.assertRaises(ValueError):
            descendants([dict(root, CreationDate=None)], 7)

    def test_cleanup_diagnostic_cannot_launch_a_local_browser(self):
        result = subprocess.run([sys.executable, str(Path(__file__).with_name("cleanup_probe.py")),
            "--chrome", "/does-not-exist", "--output", "/does-not-exist"],
            capture_output=True, text=True, env=dict(os.environ, GITHUB_ACTIONS="false"))
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("ephemeral Windows GitHub Actions", result.stderr)

    def test_host_environment_guard_prevents_local_browser_execution(self):
        result = subprocess.run([sys.executable, str(Path(__file__).with_name("probe.py")),
            "--binary", "/does-not-exist", "--chrome", "/does-not-exist", "--extension", ".", "--output", "/does-not-exist"],
            capture_output=True, text=True, env=dict(os.environ, GITHUB_ACTIONS="false"))
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("ephemeral Windows GitHub Actions", result.stderr)


if __name__ == "__main__":
    unittest.main()

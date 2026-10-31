#!/usr/bin/env python3
"""CI-only Windows relay screenshot diagnostic. Never uses an existing profile."""
import argparse
import base64
import ctypes
from ctypes import wintypes
import difflib
import hashlib
import http.server
import json
import math
import os
from pathlib import Path
import platform
import shutil
import secrets
import subprocess
import sys
import tempfile
import threading
import time
import uuid

from native_images import MAX_BODY, save_native_image

EXPECTED = {"viewport": (18, 171, 52), "full": (18, 171, 52), "selector": (200, 77, 25)}
START_ANCHOR = "async function sendCdpToTab(tabId, method, params, childSessionId) {"
RETURN_ANCHOR = """  return await observeDebuggerCommand(
    method,
    sendTabCommand(tabId, method, params, childSessionId, tabCommandDependencies()),
  );"""


def digest(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def tree_digest(root):
    files = [{"path": p.relative_to(root).as_posix(), "sha256": digest(p)}
             for p in sorted(root.rglob("*")) if p.is_file()]
    return hashlib.sha256(json.dumps(files, sort_keys=True, separators=(",", ":")).encode()).hexdigest()


def instrument(extension, target, base_url, case_id, pending_dom_control=True, candidate="baseline"):
    """Copy only public extension code and add a fixture-bound observer."""
    shutil.copytree(extension, target, ignore=shutil.ignore_patterns("*.test.js", ".DS_Store"))
    manifest_path = target / "manifest.json"
    manifest_source = manifest_path.read_text(encoding="utf-8")
    manifest = json.loads(manifest_source)
    extension_id = "".join(chr(97 + int(ch, 16)) for ch in hashlib.sha256(
        base64.b64decode(manifest["key"])).hexdigest()[:32])
    manifest["host_permissions"] = ["http://127.0.0.1/*"]
    manifest_path.write_text(json.dumps(manifest, indent=2), encoding="utf-8")
    path = target / "background.js"
    original = path.read_text(encoding="utf-8")
    if original.count(START_ANCHOR) != 1 or original.count(RETURN_ANCHOR) != 1:
        raise RuntimeError("background capture boundary changed; review the instrumentation")
    observer = Path(__file__).with_name("relay-observer.js").read_text(encoding="utf-8")
    observer = observer.replace("__SINK__", json.dumps(base_url + "/trace")).replace("__DISPATCH__", json.dumps(base_url + "/dispatch"))
    observer = observer.replace("__BASE__", json.dumps(base_url)).replace("__CASE__", json.dumps(case_id))
    observer = observer.replace("__CANDIDATE__", json.dumps(candidate)).replace("__IMAGE_SINK__", json.dumps(base_url + "/native-image"))
    observer = observer.replace("__DOM_CONTROL__", "true" if pending_dom_control else "false")
    replacement = """  const captureContext = method === 'Page.captureScreenshot'
    ? await fixtureCaptureContext(tabId, childSessionId) : null;
  const operation = observeDebuggerCommand(
    method,
    fixtureCommand(tabId, method, () => sendTabCommand(tabId, method, params, childSessionId, tabCommandDependencies(captureContext))),
  );
  if (captureContext) fixtureObserveWrapper(captureContext, operation);
  return await operation;"""
    dependency = "sendCommand: (target, command, args) => chrome.debugger.sendCommand(target, command, args),"
    if original.count(dependency) != 1 or original.count("function tabCommandDependencies() {") != 1:
        raise RuntimeError("native command dependency boundary changed; review the instrumentation")
    generated = original.replace("chrome.debugger.attach(", "fixtureDebuggerAttach(").replace("chrome.debugger.detach(", "fixtureDebuggerDetach(")
    generated = generated.replace("function tabCommandDependencies() {", "function tabCommandDependencies(captureContext = null) {")
    generated = generated.replace(dependency, "sendCommand: (target, command, args) => fixtureSendCommand(captureContext, target, command, args),")
    health_anchor = "const attachmentHealth = createAttachmentHealth();"
    if original.count(health_anchor) != 1:
        raise RuntimeError("attachment health boundary changed; review the instrumentation")
    generated = generated.replace(health_anchor, "const attachmentHealth = fixtureHealth(createAttachmentHealth());")
    generated = generated.replace(RETURN_ANCHOR, replacement)
    generated = generated.replace("const HOST_NAME =", observer + "\nconst HOST_NAME =", 1)
    path.write_text(generated, encoding="utf-8")
    diff = "".join(difflib.unified_diff(manifest_source.splitlines(True),
        manifest_path.read_text(encoding="utf-8").splitlines(True),
        fromfile="production/manifest.json", tofile="fixture/manifest.json"))
    diff += "".join(difflib.unified_diff(original.splitlines(True), generated.splitlines(True),
                                       fromfile="production/background.js", tofile="fixture/background.js"))
    return extension_id, diff, digest(path)


class Fixture:
    def __init__(self, rows):
        self.rows = rows
        self.traces = []
        self.lock = threading.Lock()
        self.controls = {}
        self.artifacts = {}
        owner = self

        class Handler(http.server.BaseHTTPRequestHandler):
            def log_message(self, *_):
                pass

            def do_OPTIONS(self):
                self.send_response(204)
                self.send_header("Access-Control-Allow-Origin", "*")
                self.send_header("Access-Control-Allow-Headers", "Content-Type")
                self.send_header("Access-Control-Allow-Methods", "POST, OPTIONS")
                self.send_header("Access-Control-Allow-Private-Network", "true")
                self.end_headers()

            def do_POST(self):
                if self.path not in ("/trace", "/dispatch", "/native-image"):
                    self.send_error(404)
                    return
                try:
                    size = int(self.headers.get("Content-Length", "0"))
                    bound = MAX_BODY if self.path == "/native-image" else 65536
                    if self.headers.get("Transfer-Encoding") or not 0 < size <= bound:
                        raise ValueError("missing, chunked or oversized body")
                    event = json.loads(self.rfile.read(size))
                    if not isinstance(event, dict):
                        raise ValueError("event must be an object")
                except (ValueError, TypeError):
                    self.send_error(400)
                    return
                with owner.lock:
                    registered = event.get("caseId") in owner.artifacts
                if not registered:
                    self.send_error(403)
                    return
                if self.path == "/native-image":
                    try:
                        with owner.lock:
                            result = save_native_image(event, owner.artifacts.get(event.get("caseId")), owner.traces)
                        body = json.dumps(result).encode()
                        self.send_response(200)
                        self.send_header("Access-Control-Allow-Origin", "*")
                        self.send_header("Content-Length", str(len(body)))
                        self.end_headers()
                        self.wfile.write(body)
                    except (ValueError, OSError):
                        self.send_error(400)
                    return
                if self.path == "/dispatch":
                    with owner.lock:
                        control = owner.controls.get(event.get("caseId"))
                    try:
                        result = control() if control else {"valid": False, "error": "no registered window control"}
                    except Exception as exc:
                        result = {"valid": False, "error": str(exc)}
                    result["sampledMonotonic"] = time.perf_counter()
                    body = json.dumps(result).encode()
                    self.send_response(200)
                    self.send_header("Access-Control-Allow-Origin", "*")
                    self.send_header("Content-Type", "application/json")
                    self.send_header("Content-Length", str(len(body)))
                    self.end_headers()
                    self.wfile.write(body)
                    return
                event["receivedMonotonic"] = time.perf_counter()
                with owner.lock:
                    owner.traces.append(event)
                self.send_response(204)
                self.send_header("Access-Control-Allow-Origin", "*")
                self.end_headers()

            def do_GET(self):
                query = self.path.partition("?")[2]
                count = 1 if self.path.startswith("/warmup") else owner.rows
                rows = "".join('<div class="row"' + (' id="middle"' if i == count//2 else '') + '>Fixture row ' + str(i) +
                    ('<div class="nonce"></div>' if i == count//2 else '') + '</div>' for i in range(count))
                body = Path(__file__).with_name("fixture.html").read_text(encoding="utf-8")
                body = body.replace("__CASE__", query).replace("__ROWS__", rows)
                self.send_response(200)
                self.send_header("Content-Type", "text/html; charset=utf-8")
                self.send_header("Cache-Control", "no-store")
                self.end_headers()
                self.wfile.write(body.encode())

        self.server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.base = "http://127.0.0.1:" + str(self.server.server_port)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()

    def register(self, case_id, artifact):
        with self.lock:
            if case_id in self.artifacts:
                raise ValueError("case already registered")
            self.artifacts[case_id] = artifact

    def set_control(self, case_id, control):
        with self.lock:
            self.controls[case_id] = control

    def events(self, case_id):
        with self.lock:
            return [x for x in self.traces if x.get("caseId") == case_id]

    def close(self):
        self.server.shutdown()
        self.server.server_close()


class Registry:
    def __init__(self, binary, root, extension_id):
        import winreg
        self.winreg = winreg
        self.saved = []
        try:
            for name in ("com.agent_browser.connect", "com.leeguoo.chrome_use"):
                manifest = root / (name + ".json")
                manifest.write_text(json.dumps({"name": name, "description": "Ephemeral CI capture probe",
                    "path": str(binary), "type": "stdio", "allowed_origins": ["chrome-extension://" + extension_id + "/"]}), encoding="utf-8")
                path = "Software\\Google\\Chrome\\NativeMessagingHosts\\" + name
                existed, old = False, None
                try:
                    with winreg.OpenKey(winreg.HKEY_CURRENT_USER, path) as key:
                        existed = True
                        try:
                            old = winreg.QueryValueEx(key, "")
                        except FileNotFoundError:
                            pass
                except FileNotFoundError:
                    pass
                self.saved.append((path, existed, old))
                with winreg.CreateKey(winreg.HKEY_CURRENT_USER, path) as key:
                    winreg.SetValueEx(key, "", 0, winreg.REG_SZ, str(manifest))
        except Exception:
            self.close()
            raise

    def __enter__(self):
        return self

    def __exit__(self, exc_type, exc, traceback):
        self.close()

    def close(self):
        w = self.winreg
        for path, existed, old in reversed(self.saved):
            if old is not None:
                with w.CreateKey(w.HKEY_CURRENT_USER, path) as key:
                    w.SetValueEx(key, "", 0, old[1], old[0])
            elif existed:
                with w.OpenKey(w.HKEY_CURRENT_USER, path, 0, w.KEY_SET_VALUE) as key:
                    try:
                        w.DeleteValue(key, "")
                    except FileNotFoundError:
                        pass
            else:
                try:
                    w.DeleteKey(w.HKEY_CURRENT_USER, path)
                except FileNotFoundError:
                    pass


class Windows:
    def __init__(self):
        self.u = ctypes.WinDLL("user32", use_last_error=True)
        self.k = ctypes.WinDLL("kernel32", use_last_error=True)
        self.u.SetProcessDpiAwarenessContext.argtypes = [wintypes.HANDLE]
        self.u.SetProcessDpiAwarenessContext.restype = wintypes.BOOL
        self.dpi_context_set = bool(self.u.SetProcessDpiAwarenessContext(ctypes.c_void_p(-4)))
        self.callback_type = ctypes.WINFUNCTYPE(wintypes.BOOL, wintypes.HWND, wintypes.LPARAM)
        signatures = {
            "GetWindowThreadProcessId": ([wintypes.HWND, ctypes.POINTER(wintypes.DWORD)], wintypes.DWORD),
            "GetWindowTextW": ([wintypes.HWND, wintypes.LPWSTR, ctypes.c_int], ctypes.c_int),
            "GetWindowRect": ([wintypes.HWND, ctypes.POINTER(wintypes.RECT)], wintypes.BOOL),
            "IsIconic": ([wintypes.HWND], wintypes.BOOL), "IsWindowVisible": ([wintypes.HWND], wintypes.BOOL),
            "GetForegroundWindow": ([], wintypes.HWND),
            "ShowWindowAsync": ([wintypes.HWND, ctypes.c_int], wintypes.BOOL),
            "SetForegroundWindow": ([wintypes.HWND], wintypes.BOOL),
            "IsChild": ([wintypes.HWND, wintypes.HWND], wintypes.BOOL),
            "DestroyWindow": ([wintypes.HWND], wintypes.BOOL),
            "GetMessageW": ([ctypes.POINTER(wintypes.MSG), wintypes.HWND, wintypes.UINT, wintypes.UINT], ctypes.c_int),
            "TranslateMessage": ([ctypes.POINTER(wintypes.MSG)], wintypes.BOOL),
            "DispatchMessageW": ([ctypes.POINTER(wintypes.MSG)], ctypes.c_ssize_t),
            "WindowFromPoint": ([wintypes.POINT], wintypes.HWND),
            "CreateWindowExW": ([wintypes.DWORD, wintypes.LPCWSTR, wintypes.LPCWSTR, wintypes.DWORD,
                ctypes.c_int, ctypes.c_int, ctypes.c_int, ctypes.c_int, wintypes.HWND, wintypes.HMENU,
                wintypes.HINSTANCE, wintypes.LPVOID], wintypes.HWND),
            "PostMessageW": ([wintypes.HWND, wintypes.UINT, wintypes.WPARAM, wintypes.LPARAM], wintypes.BOOL),
            "PostThreadMessageW": ([wintypes.DWORD, wintypes.UINT, wintypes.WPARAM, wintypes.LPARAM], wintypes.BOOL),
        }
        for name, (args, result) in signatures.items():
            fn = getattr(self.u, name)
            fn.argtypes, fn.restype = args, result
        self.u.EnumWindows.argtypes = [self.callback_type, wintypes.LPARAM]
        self.u.EnumWindows.restype = wintypes.BOOL

    def title(self, hwnd):
        text = ctypes.create_unicode_buffer(2048)
        self.u.GetWindowTextW(hwnd, text, len(text))
        return text.value

    def find(self, pid, title):
        matches = []
        def visit(hwnd, _):
            owner = wintypes.DWORD()
            self.u.GetWindowThreadProcessId(hwnd, ctypes.byref(owner))
            if owner.value == pid and title in self.title(hwnd) and self.u.IsWindowVisible(hwnd):
                matches.append(hwnd)
            return True
        self.u.EnumWindows(self.callback_type(visit), 0)
        if len(matches) > 1:
            raise RuntimeError("ambiguous owned fixture windows: " + str(len(matches)))
        return matches[0] if matches else None

    def state(self, hwnd):
        rect = wintypes.RECT()
        if not self.u.GetWindowRect(hwnd, ctypes.byref(rect)):
            raise ctypes.WinError(ctypes.get_last_error())
        center = wintypes.POINT((rect.left + rect.right) // 2, (rect.top + rect.bottom) // 2)
        point = self.u.WindowFromPoint(center)
        owner = wintypes.DWORD()
        self.u.GetWindowThreadProcessId(hwnd, ctypes.byref(owner))
        return {"hwnd": int(hwnd), "pid": owner.value, "title": self.title(hwnd), "minimized": bool(self.u.IsIconic(hwnd)),
                "visible": bool(self.u.IsWindowVisible(hwnd)), "foreground": int(self.u.GetForegroundWindow() or 0),
                "rect": [rect.left, rect.top, rect.right, rect.bottom], "centerWindow": int(point or 0),
                "centerOwned": bool(point == hwnd or self.u.IsChild(hwnd, point))}

    def restore(self, hwnd):
        self.u.ShowWindowAsync(hwnd, 9)
        self.u.SetForegroundWindow(hwnd)
        return wait_for(lambda: self.state(hwnd), lambda s: not s["minimized"] and s["centerOwned"], 5)

    def minimize(self, hwnd):
        self.u.ShowWindowAsync(hwnd, 6)
        return wait_for(lambda: self.state(hwnd), lambda s: s["minimized"], 5)

    def cover(self, hwnd, name):
        self.restore(hwnd)
        rect = self.state(hwnd)["rect"]
        ready = threading.Event()
        result = {}
        def ui():
            try:
                result["thread"] = self.k.GetCurrentThreadId()
                result["hwnd"] = self.u.CreateWindowExW(0x00000008, "STATIC", name, 0x90000000,
                    rect[0], rect[1], rect[2] - rect[0], rect[3] - rect[1], None, None, None, None)
                if not result["hwnd"]:
                    raise ctypes.WinError(ctypes.get_last_error())
                self.u.SetForegroundWindow(result["hwnd"])
            except Exception as exc:
                result["error"] = str(exc)
            finally:
                ready.set()
            if "error" in result:
                return
            message = wintypes.MSG()
            while self.u.GetMessageW(ctypes.byref(message), None, 0, 0) > 0:
                self.u.TranslateMessage(ctypes.byref(message))
                self.u.DispatchMessageW(ctypes.byref(message))
            self.u.DestroyWindow(result["hwnd"])
        thread = threading.Thread(target=ui, daemon=True)
        thread.start()
        if not ready.wait(5) or "error" in result:
            raise RuntimeError(result.get("error", "cover window did not start"))
        wait_for(lambda: self.state(hwnd), lambda s: s["centerWindow"] == int(result["hwnd"]), 5)
        cover_rect = self.state(result["hwnd"])["rect"]
        if not (cover_rect[0] <= rect[0] and cover_rect[1] <= rect[1] and cover_rect[2] >= rect[2] and cover_rect[3] >= rect[3]):
            self.u.PostThreadMessageW(result["thread"], 0x0012, 0, 0)
            thread.join(5)
            raise RuntimeError("cover window did not cover the complete owned Chrome rectangle")
        def close():
            self.u.PostThreadMessageW(result["thread"], 0x0012, 0, 0)
            thread.join(5)
            if thread.is_alive():
                raise RuntimeError("cover window did not stop")
        return close, result["hwnd"]


def system_status():
    class Memory(ctypes.Structure):
        _fields_ = [("length", wintypes.DWORD), ("loadPercent", wintypes.DWORD)] + [
            (name, ctypes.c_ulonglong) for name in ("totalPhysical", "availablePhysical", "totalPageFile",
                "availablePageFile", "totalVirtual", "availableVirtual", "availableExtendedVirtual")]
    kernel = ctypes.WinDLL("kernel32", use_last_error=True)
    memory = Memory()
    memory.length = ctypes.sizeof(memory)
    if not kernel.GlobalMemoryStatusEx(ctypes.byref(memory)):
        raise ctypes.WinError(ctypes.get_last_error())
    times = [wintypes.FILETIME(), wintypes.FILETIME(), wintypes.FILETIME()]
    if not kernel.GetSystemTimes(*(ctypes.byref(x) for x in times)):
        raise ctypes.WinError(ctypes.get_last_error())
    return {"memoryLoadPercent": memory.loadPercent, "totalPhysical": memory.totalPhysical,
            "availablePhysical": memory.availablePhysical,
            "systemTimes100ns": [(x.dwHighDateTime << 32) | x.dwLowDateTime for x in times]}


def cpu_busy(before, after):
    idle, kernel, user = [b-a for a, b in zip(before["systemTimes100ns"], after["systemTimes100ns"])]
    total = kernel + user
    return (total - idle) * 100 / total if total > 0 else None


def wait_for(get, accepts, seconds):
    deadline = time.monotonic() + seconds
    last = None
    while time.monotonic() < deadline:
        last = get()
        if accepts(last):
            return last
        time.sleep(0.05)
    raise RuntimeError("state was not confirmed: " + repr(last))


def execute(command, env, artifact, timeout=70):
    start = time.perf_counter()
    process = subprocess.Popen(command, env=env, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    killed = False
    try:
        stdout, stderr = process.communicate(timeout=timeout)
    except subprocess.TimeoutExpired:
        killed = True
        process.kill()
        stdout, stderr = process.communicate(timeout=10)
    artifact.with_suffix(".stdout.txt").write_text(stdout, encoding="utf-8")
    artifact.with_suffix(".stderr.txt").write_text(stderr, encoding="utf-8")
    response = None
    for line in reversed(stdout.splitlines()):
        try:
            parsed = json.loads(line)
            if isinstance(parsed, dict) and "success" in parsed:
                response = parsed
                break
        except ValueError:
            pass
    return {"exit": process.returncode, "elapsedMs": (time.perf_counter() - start) * 1000,
            "killedAtDeadline": killed, "response": response}


def checked(result):
    if result["exit"] != 0 or not (result["response"] or {}).get("success"):
        raise RuntimeError("CLI setup failed: " + json.dumps(result))
    return result["response"].get("data") or {}


def expected_raster(kind, geometry, native_request):
    dpr = geometry["dpr"]
    if kind == "full":
        clip = native_request.get("clip") or {}
        if not all(isinstance(clip.get(key), (int, float)) and math.isfinite(clip[key])
                   for key in ("x", "y", "width", "height", "scale")):
            raise RuntimeError("full capture has no finite native clip")
        doc_width = geometry.get("scrollWidth", geometry["target"]["width"])
        if (abs(clip["x"]) > 0.01 or abs(clip["y"]) > 0.01 or clip["scale"] <= 0
            or abs(clip["width"] - doc_width) > 1 or abs(clip["height"] - geometry["scrollHeight"]) > 1):
            raise RuntimeError("native full clip does not cover the independently observed document extent")
        return clip["width"] * clip["scale"] * dpr, clip["height"] * clip["scale"] * dpr
    if kind == "selector":
        return geometry["target"]["width"] * dpr, geometry["target"]["height"] * dpr
    return geometry["width"] * dpr, geometry["height"] * dpr


def inspect_image(path, kind, geometry, native_request, generation=None):
    from PIL import Image
    Image.MAX_IMAGE_PIXELS = 200_000_000
    with Image.open(path) as image:
        if image.width * image.height > 200_000_000:
            raise RuntimeError("fixture raster exceeds the diagnostic's 200MP memory bound")
        image.load()
        rgb = image.convert("RGB")
        dpr = geometry["dpr"]
        points = [("top", 20, 20, EXPECTED[kind])]
        if kind == "viewport":
            points.extend([("long-report", 20, 408, (52, 120, 193)),
                           ("lower-right", geometry["width"] - 40, geometry["height"] - 40, (52, 120, 193))])
        elif kind == "selector":
            points.extend([("middle", 20, geometry["target"]["height"] / 2, (200, 77, 25)),
                           ("lower-right", geometry["target"]["width"] - 40, geometry["target"]["height"] - 20, (200, 77, 25))])
        elif kind == "full":
            points.extend([("middle", 20, 160 + geometry["rows"] // 2 * 32 + 24, (52, 120, 193)),
                           ("bottom", 20, geometry["scrollHeight"] - 64, (200, 77, 25))])
        samples = []
        for label, x, y, expected in points:
            pixel = rgb.getpixel((min(rgb.width - 1, int(x * dpr)), min(rgb.height - 1, int(y * dpr))))
            samples.append({"label": label, "pixel": list(pixel), "expected": list(expected),
                            "valid": all(abs(a-b) <= 12 for a,b in zip(pixel, expected))})
        valid = all(p["valid"] for p in samples)
        expected_width, expected_height = expected_raster(kind, geometry, native_request)
        dimensions = abs(rgb.width - expected_width) <= 1 and abs(rgb.height - expected_height) <= 1
        nonces = []
        if generation is not None:
            nonce_y = [("top", 24)]
            if kind == "full":
                nonce_y += [("middle", 160 + geometry["rows"] // 2 * 32 + 20),
                            ("bottom", geometry["scrollHeight"] - 128 + 24)]
            for label, y in nonce_y:
                decoded = 0
                clean = True
                for bit in range(32):
                    x, yy = int((32 + bit * 8 + 4) * dpr), int(y * dpr)
                    if not (0 <= x < rgb.width and 0 <= yy < rgb.height):
                        clean = False
                        break
                    pixel = rgb.getpixel((x, yy))
                    white, black = all(v >= 243 for v in pixel), all(v <= 12 for v in pixel)
                    clean = clean and (white or black)
                    decoded = (decoded << 1) | int(white)
                value = f"{decoded:08x}" if clean else None
                nonces.append({"label": label, "decoded": value, "valid": value == generation})
        return {"bytes": path.stat().st_size, "sha256": digest(path), "width": rgb.width, "height": rgb.height,
                "expectedRaster": [expected_width, expected_height], "pixels": samples,
                "pixelValid": valid, "dimensionsValid": dimensions, "nonces": nonces,
                "freshnessValid": generation is not None and bool(nonces) and all(n["valid"] for n in nonces)}


def window_valid(snapshot, state, case_id, pid, expected_hwnd, probe_pid):
    window = snapshot.get("window") or {}
    if (window.get("pid") != pid or window.get("hwnd") != expected_hwnd
        or "CaptureProbe " + case_id not in window.get("title", "")):
        return False
    if state == "minimized":
        return window.get("minimized") is True
    if window.get("minimized") or not window.get("visible"):
        return False
    if state == "normal":
        return window.get("centerOwned") is True
    cover = snapshot.get("cover") or {}
    rect, bounds = window.get("rect", []), cover.get("rect", [])
    return (len(rect) == len(bounds) == 4 and cover.get("visible") is True
        and cover.get("pid") == probe_pid and cover.get("title") == "Fixture cover " + case_id
        and window.get("centerWindow") == cover.get("hwnd")
        and bounds[0] <= rect[0] and bounds[1] <= rect[1]
        and bounds[2] >= rect[2] and bounds[3] >= rect[3])


def report_operations(events):
    return [e for e in events if e.get("type") == "capture-operation-start" and e.get("fixturePath") == "/report"]


def capture_trace_complete(events):
    started = {e.get("operationId") for e in report_operations(events)}
    settled = {e.get("operationId") for e in events
               if e.get("type") in ("capture-wrapper-answer", "capture-wrapper-error")}
    return bool(started) and None not in started and started.issubset(settled)


def capture_context(events, record):
    # Teardown detach is expected, and must not rewrite capture-time validity.
    cleanup = [e["receivedMonotonic"] for e in events if e.get("type") == "host-cleanup-start"]
    if cleanup:
        events = [e for e in events if e.get("receivedMonotonic", float("inf")) < min(cleanup)]
    starts = report_operations(events)
    if len(starts) != 1:
        return None
    start = starts[0]
    own = [e for e in events if e.get("operationId") == start.get("operationId")]
    native = [e for e in own if e.get("type") == "capture-native-send"]
    ends = [e for e in own if e.get("type") in ("capture-wrapper-answer", "capture-wrapper-error")]
    if len(native) != 1 or len(ends) != 1:
        return None
    sent, end = native[0], ends[0]
    attempts = end.get("attempts") or []
    if len(attempts) != 1 or end.get("invalidations") or any(e.get("type") == "attachment-change" for e in own):
        return None
    attempt = attempts[0]
    if not isinstance(sent.get("captureId"), str) or sent["captureId"] != attempt.get("captureId"):
        return None
    epoch = sent.get("epoch", 0)
    if not isinstance(epoch, int) or epoch <= 0 or epoch != start.get("epoch") or epoch != end.get("epoch") or epoch != attempt.get("epoch"):
        return None
    if sent.get("target") != start.get("requested") or sent.get("target") != attempt.get("target"):
        return None
    control = sent.get("windowControl") or {}
    if not control.get("valid") or not 0 <= sent.get("controlElapsedMs", -1) <= 250:
        return None
    if not window_valid(control.get("snapshot") or {}, record["state"], record["caseId"], record["chromePid"], record["expectedHwnd"], record["probePid"]):
        return None
    warm_starts = [e for e in events if e.get("type") == "capture-operation-start" and e.get("fixturePath") == "/warmup"]
    if len(warm_starts) != 1 or sent.get("historyLost") is not False:
        return None
    warm = [e for e in events if e.get("operationId") == warm_starts[0].get("operationId")]
    warm_native = [e for e in warm if e.get("type") == "capture-native-send"]
    warm_end = [e for e in warm if e.get("type") == "capture-wrapper-answer"]
    if len(warm_native) != 1 or len(warm_end) != 1 or warm_end[0].get("invalidations") or len(warm_end[0].get("attempts") or []) != 1:
        return None
    if warm_native[0].get("target") != sent["target"] or warm_native[0].get("epoch") != epoch or warm_end[0].get("epoch") != epoch:
        return None
    if any(h.get("atMs", 0) > warm_end[0].get("atMs", -1) for h in sent.get("attachmentHistory") or []):
        return None
    return sent, end, own


def report_signature_matches(events, rows, record):
    context = capture_context(events, record)
    if context is None:
        return False
    sent, end, own = context
    if end["type"] != "capture-wrapper-error":
        return False
    error = end.get("error", "")
    attempt = end["attempts"][0]
    if ("timeout" not in error.lower() or "page.capturescreenshot" not in error.lower()
        or "25000ms" not in error or not 24000 <= attempt.get("elapsedMs", 0) <= 35000
        or attempt.get("nativePending") is not True):
        return False
    return any(e.get("type") == "dom-native-answer" and e.get("captureId") == sent["captureId"]
        and e.get("target") == sent["target"] and e.get("epoch") == sent["epoch"]
        and e.get("nativeStillPending") is True and e.get("wrapperStillPending") is True
        and (e.get("result") or {}).get("rows") == rows and (e.get("result") or {}).get("ready") == "complete"
        for e in own)


def experiment_evidence_valid(record):
    if not record.get("candidateExperiment"):
        return True
    if "diagnosticError" in record or "nativeImageError" in record:
        return False
    monitor = record.get("nativeMonitor") or {}
    if (monitor.get("calibrated") is not True or monitor.get("installed") != 4
        or monitor.get("unhooked") != [True] * 4 or monitor.get("pumpStoppedExpected") is not True
        or monitor.get("overflow") is not False or monitor.get("errors")):
        return False
    start, end = monitor.get("startTick"), monitor.get("endTick")
    if not isinstance(start, int) or not isinstance(end, int) or end < start:
        return False
    generation_start, generation_end = record.get("generationStartedTick"), record.get("generationConfirmedTick")
    if not isinstance(generation_start, int) or not isinstance(generation_end, int) or not start <= generation_start <= generation_end <= end:
        return False
    samples = [v for v in monitor.get("samples", []) if start <= v["tick"] <= end]
    original = record.get("windowBefore", {}).get("window", {})
    if len(samples) < 2 or samples[0]["tick"] - start > 100 or end - samples[-1]["tick"] > 100:
        return False
    if any(b["tick"]-a["tick"] > 250 for a,b in zip(samples, samples[1:])):
        return False
    for value in samples:
        window = value.get("window") or {}
        if (not value.get("identityValid") or not window.get("minimized") or window.get("foreground") != original.get("foreground")
            or window.get("rect") != original.get("rect") or window.get("hwnd") != record["expectedHwnd"]):
            return False
    if any(start <= e["tick"] <= end for e in monitor.get("events", [])):
        return False
    context = capture_context(record.get("trace", []), record)
    if context is None:
        return False
    sent, _, events = context
    after = [e for e in events if e.get("type") == "page-after-answer"]
    if len(after) != 1 or after[0].get("target") != sent["target"] or after[0].get("epoch") != sent["epoch"]:
        return False
    page = record.get("pageBefore")
    if not page or after[0].get("result") != page or page.get("generation") != record.get("generation"):
        return False
    candidate = record["candidate"]
    if sent.get("candidate") != candidate or sent.get("request", {}).get("fromSurface") is not (candidate != "view"):
        return False
    focus = [e for e in events if e.get("type", "").startswith("candidate-focus-")]
    if candidate == "focus":
        if [e["type"] for e in focus] != ["candidate-focus-send", "candidate-focus-answer"]:
            return False
        if any(e.get("target") != sent["target"] or e.get("epoch") != sent["epoch"] for e in focus):
            return False
    elif focus:
        return False
    answers = [e for e in events if e.get("type") == "capture-native-answer"]
    errors = [e for e in events if e.get("type") == "capture-native-error"]
    end = context[1]
    attempt = end["attempts"][0]
    if any(e.get("type") in ("native-image-error", "page-after-error") for e in events):
        return False
    if (record.get("capture", {}).get("response") or {}).get("success") is True and end["type"] != "capture-wrapper-answer":
        return False
    known_answer = end["type"] == "capture-wrapper-answer" or attempt.get("nativeOutcome") == "answer" or bool(answers)
    if known_answer:
        acks = [e for e in events if e.get("type") == "native-image-ack"]
        if (len(answers) != 1 or errors or len(acks) != 1
            or (end["type"] == "capture-wrapper-answer" and (attempt.get("nativeOutcome") != "answer" or attempt.get("nativePending") is not False))
            or answers[0].get("responseOwned") is not True
            or any(e.get("captureId") != sent["captureId"] or e.get("target") != sent["target"] or e.get("epoch") != sent["epoch"] for e in [answers[0], acks[0]])
            or "nativeImage" not in record
            or (acks[0].get("saved") or {}).get("sha256") != record["nativeImage"].get("sha256")):
            return False
        answer = answers[0]
        times = [sent.get("sentMs"), answer.get("nativeReceivedMs"), answer.get("ownershipVerifiedMs"), answer.get("atMs")]
        if not all(isinstance(t, (float, int)) and math.isfinite(t) for t in times) or times != sorted(times):
            return False
    elif attempt.get("nativeOutcome") == "error":
        if len(errors) != 1 or errors[0].get("captureId") != sent["captureId"] or errors[0].get("target") != sent["target"] or errors[0].get("epoch") != sent["epoch"]:
            return False
    elif attempt.get("nativePending") is not True or attempt.get("nativeOutcome") is not None:
        return False
    return True


def case_verdict(record):
    if not experiment_evidence_valid(record):
        return "INCONCLUSIVE"
    if record.get("setupVerified") is not True or record.get("traceVerified") is not True or "error" in record:
        return "INCONCLUSIVE"
    if any(key.endswith("CleanupError") for key in record):
        return "INCONCLUSIVE"
    if not all(window_valid(record.get(key) or {}, record["state"], record["caseId"], record["chromePid"], record["expectedHwnd"], record["probePid"])
               for key in ("windowBefore", "windowAfter")):
        return "INCONCLUSIVE"
    if capture_context(record.get("trace") or [], record) is None:
        return "INCONCLUSIVE"
    capture = record.get("capture") or {}
    image = record.get("image") or {}
    if (capture.get("exit") != 0 or capture.get("killedAtDeadline") is not False
        or (capture.get("response") or {}).get("success") is not True
        or not image.get("pixelValid") or not image.get("dimensionsValid")
        or (record.get("candidateExperiment") and (not image.get("freshnessValid")
            or ("nativeImage" in record and not record["nativeImage"].get("freshnessValid"))))):
        return "ISSUES"
    return "PASS"


def run_case(args, fixture, windows, root, output, state, kind, repeat, candidate="baseline"):
    args = argparse.Namespace(**vars(args), candidate=candidate)
    case_id = state + "-" + kind + "-" + str(repeat) + "-" + uuid.uuid4().hex[:8]
    work = root / case_id
    work.mkdir()
    artifact = output / case_id
    artifact.mkdir()
    fixture.register(case_id, artifact)
    extension_id, diff, instrumented_hash = instrument(args.extension, work / "extension", fixture.base, case_id, args.pending_dom_control, args.candidate)
    (artifact / "instrumentation.diff").write_text(diff, encoding="utf-8")
    (artifact / "instrumented-manifest.json").write_text((work / "extension/manifest.json").read_text(encoding="utf-8"), encoding="utf-8")
    with Registry(args.binary, work, extension_id), (artifact / "chrome.log").open("w", encoding="utf-8") as chrome_log:
        relay = work / "relay"
        relay.mkdir()
        env = dict(os.environ, CHROME_USE_RELAY_DIR=str(relay), AGENT_BROWSER_SOCKET_DIR=str(work / "socket"),
                   CHROME_USE_NO_UPDATE_CHECK="1", AGENT_BROWSER_AUTO_LOGIN="off")
        chrome = subprocess.Popen([str(args.chrome), "--no-first-run", "--no-default-browser-check",
            "--user-data-dir=" + str(work / "profile"), "--load-extension=" + str(work / "extension"),
            "--disable-extensions-except=" + str(work / "extension"), "--window-size=1280,800", "about:blank"],
            env=env, stdout=chrome_log, stderr=chrome_log)
        record = {"caseId": case_id, "state": state, "kind": kind, "repeat": repeat,
                  "protocol": "extension-relay-instrumented", "pendingDomControl": args.pending_dom_control,
                  "candidateExperiment": args.experiment in ("candidates", "baseline"), "candidate": candidate, "instrumentedBackgroundSha256": instrumented_hash,
                  "instrumentedExtensionSha256": tree_digest(work / "extension"),
                  "instrumentationDiff": str(artifact / "instrumentation.diff"), "setupVerified": False,
                  "stage": "relay-start", "traceVerified": False, "chromePid": chrome.pid, "probePid": os.getpid()}
        monitor = None
        close_cover = None
        cover_hwnd = None
        common = []
        try:
            files = wait_for(lambda: list(relay.glob("relay-cdp-url-*")), lambda x: len(x) == 1, 30)
            browser = files[0].name.removeprefix("relay-cdp-url-")
            common = [str(args.binary), "--session", case_id, "--browser", browser, "--window", "dedicated", "--json"]
            record["stage"] = "warmup"
            invoke = lambda name, *words: execute(common + list(words), env, artifact / name)
            checked(invoke("warm-open", "open", fixture.base + "/warmup?" + case_id))
            warm_hwnd = wait_for(lambda: windows.find(chrome.pid, "CaptureProbe " + case_id), lambda x: x is not None, 5)
            record["warmWindow"] = windows.state(warm_hwnd)
            if record["warmWindow"]["minimized"]:
                raise RuntimeError("fresh warmup window was minimized; refusing a restore/focus intervention")
            fixture.set_control(case_id, lambda: {"valid": True, "snapshot": {"window": windows.state(warm_hwnd)}})
            checked(invoke("warm-capture", "screenshot", str(artifact / "warm.png"), "--full-res"))
            from PIL import Image
            with Image.open(artifact / "warm.png") as warm:
                warm_pixel = warm.convert("RGB").getpixel((20, 20))
            if any(abs(a-b) > 12 for a,b in zip(warm_pixel, EXPECTED["viewport"])):
                raise RuntimeError("warmup screenshot did not contain the fixture's green sentinel")
            record["stage"] = "fixture"
            checked(invoke("report-open", "open", fixture.base + "/report?" + case_id))
            checked(invoke("viewport", "set", "viewport", "1280", "720", str(args.dpr)))
            geometry = checked(invoke("geometry", "eval", "({title:document.title,width:innerWidth,height:innerHeight,dpr:devicePixelRatio,scrollHeight:document.documentElement.scrollHeight,scrollWidth:document.documentElement.scrollWidth,rows:document.querySelectorAll('#rows > .row').length,ready:document.readyState,visibility:document.visibilityState,target:(()=>{const r=document.querySelector('#target').getBoundingClientRect();return {width:r.width,height:r.height,top:r.top}})()})"))["result"]
            if geometry["rows"] != args.rows or geometry["ready"] != "complete":
                raise RuntimeError("long fixture did not finish rendering")
            record["stage"] = "window-state"
            hwnd = wait_for(lambda: windows.find(chrome.pid, "CaptureProbe " + case_id), lambda x: x is not None, 5)
            record["expectedHwnd"] = int(hwnd)
            if args.experiment in ("candidates", "baseline"):
                from native_monitor import NativeWindowMonitor
                monitor = NativeWindowMonitor(windows, int(hwnd), chrome.pid, case_id)
                monitor.start()
            if state == "normal":
                placement = windows.restore(hwnd)
            elif state == "covered":
                close_cover, cover_hwnd = windows.cover(hwnd, "Fixture cover " + case_id)
                placement = windows.state(hwnd)
            else:
                placement = windows.minimize(hwnd)
            if monitor:
                wait_for(monitor.calibrated, bool, 3)
                settled = []
                def stable_setup(window):
                    valid = window_valid({"window":window}, "minimized", case_id, chrome.pid, int(hwnd), os.getpid())
                    value = {k:window[k] for k in ("hwnd","pid","title","minimized","rect","foreground")}
                    if not valid or (settled and settled[-1] != value):
                        settled.clear()
                    if valid:
                        settled.append(value)
                    return len(settled) >= 3
                record["setupSettledWindow"] = wait_for(lambda: windows.state(hwnd), stable_setup, 3)
                monitor.begin_measurement()
                record["generationStartedTick"] = windows.k.GetTickCount64()
                generation = secrets.token_hex(4)
                if generation == "00000000":
                    generation = "00000001"
                record["generation"] = generation
                changed = checked(invoke("generation", "eval", "fixtureAdvance(" + json.dumps(generation) + ")"))["result"]
                if changed != generation:
                    raise RuntimeError("post-minimize fixture generation update was not confirmed")
                record["generationConfirmedTick"] = windows.k.GetTickCount64()
                record["pageBefore"] = checked(invoke("page-before", "eval", "fixtureSnapshot()"))["result"]
                if record["pageBefore"]["generation"] != generation:
                    raise RuntimeError("post-minimize fixture snapshot did not contain the new generation")
            before = invoke("dom-before", "eval", "({rows:document.querySelectorAll('#rows > .row').length,ready:document.readyState,visibility:document.visibilityState})")
            dom = checked(before)["result"]
            if dom["rows"] != args.rows or dom["ready"] != "complete":
                raise RuntimeError("DOM control did not answer before capture")
            def snapshot():
                return {"window": windows.state(hwnd), "cover": windows.state(cover_hwnd) if cover_hwnd else None}
            def control():
                current = snapshot()
                return {"valid": window_valid(current, state, case_id, chrome.pid, int(hwnd), os.getpid()), "snapshot": current}
            fixture.set_control(case_id, control)
            placement = snapshot()
            if not window_valid(placement, state, case_id, chrome.pid, int(hwnd), os.getpid()):
                raise RuntimeError("window state changed before capture")
            record.update(setupVerified=True, geometry=geometry, windowBefore=placement, domBefore=before, stage="capture")
            image = artifact / "capture.png"
            command = ["screenshot", str(image), "--full-res"]
            if kind == "full":
                command.append("--full")
            elif kind == "selector":
                command.extend(["--selector", "#target"])
            record["systemBeforeCapture"] = system_status()
            record["captureAttempted"] = True
            capture = invoke("capture", *command)
            record["systemAfterCapture"] = system_status()
            record["cpuBusyPercentDuringCapture"] = cpu_busy(record["systemBeforeCapture"], record["systemAfterCapture"])
            record["capture"] = capture
            record["windowAfter"] = snapshot()
            if monitor:
                record["domAfter"] = {"source": "same-attachment worker page-after trace; no CLI health recovery"}
            elif capture["exit"] == 0 and (capture.get("response") or {}).get("success"):
                record["domAfter"] = invoke("dom-after", "eval", "({rows:document.querySelectorAll('#rows > .row').length,ready:document.readyState,visibility:document.visibilityState})")
            else:
                record["domAfter"] = {"skipped": "avoid resetting an attachment while the timed-out native capture may still be pending"}
        except Exception as exc:
            record["error"] = str(exc)
        finally:
            if record.get("captureAttempted"):
                try:
                    wait_for(lambda: fixture.events(case_id), capture_trace_complete, 5)
                    record["traceVerified"] = True
                except Exception as exc:
                    record["traceError"] = str(exc)
            if monitor:
                try:
                    def diagnostics_complete(events):
                        # Trace completion is separate from attachment validity:
                        # a recorded recovery is evidence, not a reason to discard it.
                        starts = report_operations(events)
                        if len(starts) != 1:
                            return False
                        own = [e for e in events if e.get("operationId") == starts[0].get("operationId")]
                        ends = [e for e in own if e.get("type") in ("capture-wrapper-answer", "capture-wrapper-error")]
                        if len(ends) != 1 or len(ends[0].get("attempts", [])) != 1:
                            return False
                        end = ends[0]
                        page_done = any(e.get("type") in ("page-after-answer", "page-after-error") for e in own)
                        attempt = end["attempts"][0]
                        native_answer = any(e.get("type") == "capture-native-answer" for e in own)
                        known_answer = end["type"] == "capture-wrapper-answer" or attempt.get("nativeOutcome") == "answer" or native_answer
                        upload_done = any(e.get("type") in ("native-image-ack", "native-image-error") for e in own)
                        native_error = any(e.get("type") == "capture-native-error" for e in own)
                        if known_answer:
                            return page_done and native_answer and upload_done
                        if attempt.get("nativeOutcome") == "error":
                            return page_done and native_error
                        return page_done and attempt.get("nativePending") is True and attempt.get("nativeOutcome") is None
                    wait_for(lambda: fixture.events(case_id), diagnostics_complete, 7)
                except Exception as exc:
                    record["diagnosticError"] = str(exc)
                try:
                    record["nativeMonitor"] = monitor.finish_measurement()
                except Exception as exc:
                    record["monitorCleanupError"] = str(exc)
            if close_cover:
                try:
                    close_cover()
                except Exception as exc:
                    record["coverCleanupError"] = str(exc)
            with fixture.lock:
                fixture.traces.append({"caseId": case_id, "type": "host-cleanup-start",
                                       "receivedMonotonic": time.perf_counter()})
            # Session teardown is explicit cleanup, not another capture/fallback.
            # Keep the relay alive until that closure has answered.
            if common:
                try:
                    record["sessionCleanup"] = execute(common + ["close"], env, artifact / "close", timeout=20)
                    result = record["sessionCleanup"]
                    if result["exit"] != 0 or not (result["response"] or {}).get("success"):
                        record["sessionCleanupError"] = "CLI session closure was not confirmed"
                except Exception as exc:
                    record["sessionCleanupError"] = str(exc)
            try:
                if chrome.poll() is None:
                    subprocess.run(["taskkill", "/PID", str(chrome.pid), "/T", "/F"], capture_output=True, timeout=20)
                chrome.wait(timeout=20)
            except Exception as exc:
                record["browserCleanupError"] = str(exc)
            events = fixture.events(case_id)
            record["trace"] = events
            record["matchesReportSignature"] = False
            (artifact / "result.json").write_text(json.dumps(record, indent=2), encoding="utf-8")
    if record.get("captureAttempted") and (artifact / "capture.png").exists():
        try:
            context = capture_context(record["trace"], record)
            if context is None:
                raise RuntimeError("image cannot be verified without one stable native capture context")
            record["image"] = inspect_image(artifact / "capture.png", kind, geometry, context[0].get("request") or {}, record.get("generation"))
        except Exception as exc:
            record["error"] = type(exc).__name__ + ": " + str(exc)
    if record.get("candidateExperiment") and (artifact / "native.png").exists():
        try:
            from PIL import Image
            with Image.open(artifact / "native.png") as raw:
                raw_kind = "selector" if abs(raw.height - geometry["target"]["height"] * geometry["dpr"]) <= 1 else "full" if raw.height > geometry["height"] * geometry["dpr"] else "viewport"
            record["nativeImage"] = inspect_image(artifact / "native.png", raw_kind, geometry,
                (capture_context(record["trace"], record) or ({},))[0].get("request") or {}, record.get("generation"))
        except Exception as exc:
            record["nativeImageError"] = str(exc)
    record["verdict"] = case_verdict(record)
    record["matchesReportSignature"] = record["verdict"] == "ISSUES" and report_signature_matches(record["trace"], args.rows, record)
    (artifact / "result.json").write_text(json.dumps(record, indent=2), encoding="utf-8")
    return record


class ProbeSetupFailure(Exception):
    pass


def file_version(path):
    command = str(Path(os.environ["SystemRoot"]) / "System32/WindowsPowerShell/v1.0/powershell.exe")
    env = dict(os.environ, PROBE_VERSION_EXE=str(path))
    return subprocess.check_output([command, "-NoProfile", "-Command",
        "[Diagnostics.FileVersionInfo]::GetVersionInfo($env:PROBE_VERSION_EXE).ProductVersion"],
        env=env, text=True).strip()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--experiment", choices=("original", "candidates", "baseline"), default="original")
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--chrome", type=Path, required=True)
    parser.add_argument("--extension", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--samples", type=int, default=5, choices=range(1, 6))
    parser.add_argument("--rows", type=int, default=1000, choices=range(100, 3001))
    parser.add_argument("--dpr", type=int, default=1, choices=(1, 2))
    parser.add_argument("--pending-dom-control", action=argparse.BooleanOptionalAction, default=True)
    args = parser.parse_args()
    if sys.platform != "win32" or os.environ.get("GITHUB_ACTIONS") != "true":
        parser.error("this probe only runs in an ephemeral Windows GitHub Actions job")
    if (args.rows * 32 + 288) * 1280 * args.dpr ** 2 > 200_000_000:
        parser.error("rows and DPR exceed the controlled fixture's 200MP raster bound")
    args.binary, args.chrome, args.extension = args.binary.resolve(), args.chrome.resolve(), args.extension.resolve()
    if not args.binary.is_file() or not args.chrome.is_file():
        parser.error("explicit built CLI and CfT executable paths must exist")
    args.output.mkdir(parents=True, exist_ok=False)
    from PIL import __version__ as pillow_version
    metadata = {"gitSha": os.environ.get("GITHUB_SHA"), "runId": os.environ.get("GITHUB_RUN_ID"),
        "os": platform.platform(), "processors": os.cpu_count(), "python": platform.python_version(),
        "pillow": pillow_version, "systemBefore": system_status(), "binary": str(args.binary), "binarySha256": digest(args.binary),
        "chrome": str(args.chrome), "chromeSha256": digest(args.chrome), "imageVersion": os.environ.get("ImageVersion"),
        "cliVersion": subprocess.check_output([str(args.binary), "--version"], text=True).strip(),
        "chromeFileVersion": file_version(args.chrome),
        "sourceBackgroundSha256": digest(args.extension / "background.js"), "rows": args.rows,
        "samples": args.samples, "dpr": args.dpr, "experiment": args.experiment, "instrumented": True, "pendingDomControl": args.pending_dom_control,
        "scope": "Windows CI desktop + fresh source relay; no existing user session or other extensions"}
    (args.output / "metadata.json").write_text(json.dumps(metadata, indent=2), encoding="utf-8")
    root = Path(tempfile.mkdtemp(prefix="cu-capture-"))
    fixture = Fixture(args.rows)
    records = []
    setup_error = None
    try:
        windows = Windows()
        metadata["physicalDpiContextSet"] = windows.dpi_context_set
        (args.output / "metadata.json").write_text(json.dumps(metadata, indent=2), encoding="utf-8")
        deadline = time.monotonic() + 1800
        for repeat in range(args.samples):
            candidates = ["baseline", "view", "focus"] if args.experiment == "candidates" else ["baseline"]
            candidates = candidates[repeat % len(candidates):] + candidates[:repeat % len(candidates)]
            states = ["minimized"] if args.experiment in ("candidates", "baseline") else ["normal", "covered", "minimized"]
            for candidate in candidates:
              for state in states:
                for kind in (("viewport",) if args.experiment == "baseline" else ("viewport", "full", "selector")):
                    if time.monotonic() >= deadline:
                        raise ProbeSetupFailure("matrix stopped at its 30-minute experiment budget")
                    record = run_case(args, fixture, windows, root, args.output, state, kind, repeat, candidate)
                    records.append(record)
                    (args.output / "progress.json").write_text(json.dumps({"expectedCases": args.samples * (1 if args.experiment == "baseline" else 9), "records": records}, indent=2), encoding="utf-8")
                    print(json.dumps({"case": record["caseId"], "setup": record["setupVerified"],
                                      "signature": record["matchesReportSignature"], "verdict": record["verdict"], "error": record.get("error")}), flush=True)
                    if record.get("verdict") == "INCONCLUSIVE":
                        raise ProbeSetupFailure("matrix stopped after unverified setup or capture dispatch")
                    if any(key.endswith("CleanupError") for key in record):
                        raise ProbeSetupFailure("matrix stopped after unconfirmed cleanup")
    except ProbeSetupFailure as exc:
        setup_error = str(exc)
    except Exception as exc:
        setup_error = type(exc).__name__ + ": " + str(exc)
    finally:
        fixture.close()
    invalid = [r for r in records if r.get("verdict") == "INCONCLUSIVE"]
    reproduced = [r for r in records if r["matchesReportSignature"]]
    failed = [r for r in records if r.get("verdict") == "ISSUES"]
    incomplete = len(records) != args.samples * (1 if args.experiment == "baseline" else 9)
    verdict = "ISSUES" if failed else "INCONCLUSIVE" if invalid or incomplete or setup_error else "PASS"
    cleanup_failed = any(any(key.endswith("CleanupError") for key in r) for r in records)
    if cleanup_failed:
        verdict = "INCONCLUSIVE"
    summary = {"verdict": verdict, "expectedCases": args.samples * (1 if args.experiment == "baseline" else 9), "cases": len(records), "inconclusiveCases": len(invalid),
        "captureFailures": len(failed), "matchingReportSignature": len(reproduced), "records": records,
        "setupError": setup_error, "matrixComplete": not invalid and not incomplete,
        "interpretation": "PASS means this matrix did not reproduce the report; it does not prove customer Windows acceptance."}
    (args.output / "summary.json").write_text(json.dumps(summary, indent=2), encoding="utf-8")
    print(json.dumps({k: v for k, v in summary.items() if k != "records"}), flush=True)
    return 2 if invalid or cleanup_failed or setup_error or incomplete else 1 if failed else 0


if __name__ == "__main__":
    raise SystemExit(main())

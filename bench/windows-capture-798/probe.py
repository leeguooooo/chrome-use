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
import os
from pathlib import Path
import platform
import shutil
import subprocess
import sys
import tempfile
import threading
import time
import uuid

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


def instrument(extension, target, base_url, case_id):
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
    observer = r'''
// CI fixture diagnostic only. This file is a generated, unshipped copy.
async function observeFixtureCapture(tabId, params, operation) {
  const started = performance.now();
  const captureId = crypto.randomUUID();
  const initial = await chrome.tabs.get(tabId).catch(() => null);
  if (!ownedTabs.has(tabId) || !String(initial?.url || '').startsWith(__BASE__ + '/')) return;
  let pending = true;
  const emit = (event) => void fetch(__SINK__, {
    method: 'POST', headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ caseId: __CASE__, captureId, tabId, atMs: performance.now(), ...event }),
  }).catch(() => {});
  emit({ type: 'capture-start', startedMs: started, fixturePath: new URL(initial.url).pathname,
    request: { format: params?.format, clip: params?.clip,
      captureBeyondViewport: params?.captureBeyondViewport, fromSurface: params?.fromSurface } });
  const timer = setTimeout(async () => {
    if (!pending) return;
    const tab = await chrome.tabs.get(tabId).catch(() => null);
    if (!pending || !ownedTabs.has(tabId) || !String(tab?.url || '').startsWith(__BASE__ + '/')) return;
    const sentMs = performance.now();
    emit({ type: 'dom-sent', sentMs, captureStillPending: pending });
    let timeout;
    try {
      const result = await Promise.race([
        chrome.debugger.sendCommand({ tabId }, 'Runtime.evaluate', {
          expression: "({ready:document.readyState,rows:document.querySelectorAll('#rows > .row').length,visibility:document.visibilityState})",
          returnByValue: true,
        }),
        new Promise((_, reject) => { timeout = setTimeout(() => reject(new Error('fixture DOM probe exceeded 3000ms')), 3000); }),
      ]);
      emit({ type: 'dom-answer', sentMs, captureStillPending: pending, result: result?.result?.value });
    } catch (error) {
      emit({ type: 'dom-error', sentMs, captureStillPending: pending, error: String(error?.message || error) });
    } finally { clearTimeout(timeout); }
  }, Math.max(0, 2000 - (performance.now() - started)));
  operation.then(result => {
    pending = false; clearTimeout(timer);
    emit({ type: 'capture-answer', elapsedMs: performance.now() - started,
      encodedBytes: typeof result?.data === 'string' ? result.data.length : null });
  }, error => {
    pending = false; clearTimeout(timer);
    emit({ type: 'capture-error', elapsedMs: performance.now() - started, error: String(error?.message || error) });
  });
}
'''.replace("__SINK__", json.dumps(base_url + "/trace")).replace("__BASE__", json.dumps(base_url)).replace("__CASE__", json.dumps(case_id))
    replacement = """  const operation = observeDebuggerCommand(
    method,
    sendTabCommand(tabId, method, params, childSessionId, tabCommandDependencies()),
  );
  if (method === 'Page.captureScreenshot' && !childSessionId) {
    void observeFixtureCapture(tabId, params, operation).catch(() => {});
  }
  return await operation;"""
    generated = original.replace(START_ANCHOR, observer + "\n" + START_ANCHOR).replace(RETURN_ANCHOR, replacement)
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
        owner = self

        class Handler(http.server.BaseHTTPRequestHandler):
            def log_message(self, *_):
                pass

            def do_OPTIONS(self):
                self.send_response(204)
                self.send_header("Access-Control-Allow-Origin", "*")
                self.send_header("Access-Control-Allow-Headers", "Content-Type")
                self.end_headers()

            def do_POST(self):
                if self.path != "/trace":
                    self.send_error(404)
                    return
                size = int(self.headers.get("Content-Length", "0"))
                if not 0 < size < 65536:
                    self.send_error(413)
                    return
                event = json.loads(self.rfile.read(size))
                event["receivedMonotonic"] = time.perf_counter()
                with owner.lock:
                    owner.traces.append(event)
                self.send_response(204)
                self.send_header("Access-Control-Allow-Origin", "*")
                self.end_headers()

            def do_GET(self):
                query = self.path.partition("?")[2]
                count = 1 if self.path.startswith("/warmup") else owner.rows
                body = ("<!doctype html><meta charset=utf-8><title>CaptureProbe " + query + "</title>"
                        "<style>html,body{margin:0}#top{height:160px;background:#12ab34}"
                        ".row{height:32px;box-sizing:border-box;border-bottom:1px solid #ccc;background:#3478c1}"
                        "#target{height:128px;background:#c84d19}</style>"
                        "<div id=top></div><main id=rows>" +
                        "".join("<div class=row>Fixture row " + str(i) + "</div>" for i in range(count)) +
                        "</main><div id=target></div>")
                self.send_response(200)
                self.send_header("Content-Type", "text/html; charset=utf-8")
                self.send_header("Cache-Control", "no-store")
                self.end_headers()
                self.wfile.write(body.encode())

        self.server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.base = "http://127.0.0.1:" + str(self.server.server_port)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()

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
        return {"hwnd": int(hwnd), "minimized": bool(self.u.IsIconic(hwnd)),
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
        return close


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


def inspect_image(path, kind, geometry):
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
            points.append(("long-report", 20, 408, (52, 120, 193)))
        elif kind == "full":
            points.extend([("middle", 20, 160 + geometry["rows"] // 2 * 32 + 24, (52, 120, 193)),
                           ("bottom", 20, geometry["scrollHeight"] - 64, (200, 77, 25))])
        samples = []
        for label, x, y, expected in points:
            pixel = rgb.getpixel((min(rgb.width - 1, int(x * dpr)), min(rgb.height - 1, int(y * dpr))))
            samples.append({"label": label, "pixel": list(pixel), "expected": list(expected),
                            "valid": all(abs(a-b) <= 12 for a,b in zip(pixel, expected))})
        valid = all(p["valid"] for p in samples)
        css_width = geometry["target"]["width"] if kind == "selector" else geometry["width"]
        css_height = geometry["target"]["height"] if kind == "selector" else geometry["scrollHeight"] if kind == "full" else geometry["height"]
        dimensions = abs(rgb.width - css_width * dpr) <= 4 and abs(rgb.height - css_height * dpr) <= 4
        return {"bytes": path.stat().st_size, "sha256": digest(path), "width": rgb.width, "height": rgb.height,
                "pixels": samples, "pixelValid": valid, "dimensionsValid": dimensions}


def run_case(args, fixture, windows, root, output, state, kind, repeat):
    case_id = state + "-" + kind + "-" + str(repeat) + "-" + uuid.uuid4().hex[:8]
    work = root / case_id
    work.mkdir()
    artifact = output / case_id
    artifact.mkdir()
    extension_id, diff, instrumented_hash = instrument(args.extension, work / "extension", fixture.base, case_id)
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
                  "protocol": "extension-relay-instrumented", "instrumentedBackgroundSha256": instrumented_hash,
                  "instrumentedExtensionSha256": tree_digest(work / "extension"),
                  "instrumentationDiff": str(artifact / "instrumentation.diff"), "setupVerified": False,
                  "stage": "relay-start"}
        close_cover = None
        common = []
        try:
            files = wait_for(lambda: list(relay.glob("relay-cdp-url-*")), lambda x: len(x) == 1, 30)
            browser = files[0].name.removeprefix("relay-cdp-url-")
            common = [str(args.binary), "--session", case_id, "--browser", browser, "--window", "dedicated", "--json"]
            record["stage"] = "warmup"
            invoke = lambda name, *words: execute(common + list(words), env, artifact / name)
            checked(invoke("warm-open", "open", fixture.base + "/warmup?" + case_id))
            warm_hwnd = wait_for(lambda: windows.find(chrome.pid, "CaptureProbe " + case_id), lambda x: x is not None, 5)
            record["warmWindow"] = windows.restore(warm_hwnd)
            checked(invoke("warm-capture", "screenshot", str(artifact / "warm.png"), "--full-res"))
            from PIL import Image
            with Image.open(artifact / "warm.png") as warm:
                warm_pixel = warm.convert("RGB").getpixel((20, 20))
            if any(abs(a-b) > 12 for a,b in zip(warm_pixel, EXPECTED["viewport"])):
                raise RuntimeError("warmup screenshot did not contain the fixture's green sentinel")
            record["stage"] = "fixture"
            checked(invoke("report-open", "open", fixture.base + "/report?" + case_id))
            checked(invoke("viewport", "set", "viewport", "1280", "720", str(args.dpr)))
            geometry = checked(invoke("geometry", "eval", "({title:document.title,width:innerWidth,height:innerHeight,dpr:devicePixelRatio,scrollHeight:document.documentElement.scrollHeight,rows:document.querySelectorAll('#rows > .row').length,ready:document.readyState,visibility:document.visibilityState,target:(()=>{const r=document.querySelector('#target').getBoundingClientRect();return {width:r.width,height:r.height,top:r.top}})()})"))["result"]
            if geometry["rows"] != args.rows or geometry["ready"] != "complete":
                raise RuntimeError("long fixture did not finish rendering")
            record["stage"] = "window-state"
            hwnd = wait_for(lambda: windows.find(chrome.pid, "CaptureProbe " + case_id), lambda x: x is not None, 5)
            if state == "normal":
                placement = windows.restore(hwnd)
            elif state == "covered":
                close_cover = windows.cover(hwnd, "Fixture cover " + case_id)
                placement = windows.state(hwnd)
                record["coverWindow"] = windows.state(placement["centerWindow"])
            else:
                placement = windows.minimize(hwnd)
            before = invoke("dom-before", "eval", "({rows:document.querySelectorAll('#rows > .row').length,ready:document.readyState,visibility:document.visibilityState})")
            dom = checked(before)["result"]
            if dom["rows"] != args.rows or dom["ready"] != "complete":
                raise RuntimeError("DOM control did not answer before capture")
            record.update(setupVerified=True, geometry=geometry, windowBefore=placement, domBefore=before, stage="capture")
            image = artifact / "capture.png"
            command = ["screenshot", str(image), "--full-res"]
            if kind == "full":
                command.append("--full")
            elif kind == "selector":
                command.extend(["--selector", "#target"])
            record["systemBeforeCapture"] = system_status()
            capture = invoke("capture", *command)
            record["systemAfterCapture"] = system_status()
            record["cpuBusyPercentDuringCapture"] = cpu_busy(record["systemBeforeCapture"], record["systemAfterCapture"])
            record["capture"] = capture
            record["windowAfter"] = windows.state(hwnd)
            if image.exists():
                record["image"] = inspect_image(image, kind, geometry)
            record["domAfter"] = invoke("dom-after", "eval", "({rows:document.querySelectorAll('#rows > .row').length,ready:document.readyState,visibility:document.visibilityState})")
        except Exception as exc:
            record["error"] = str(exc)
        finally:
            if close_cover:
                try:
                    close_cover()
                except Exception as exc:
                    record["coverCleanupError"] = str(exc)
            # Each case gets a fresh browser. A timed-out capture is never overlapped
            # with another capture or a fallback in the same profile.
            try:
                if chrome.poll() is None:
                    subprocess.run(["taskkill", "/PID", str(chrome.pid), "/T", "/F"], capture_output=True, timeout=20)
                chrome.wait(timeout=20)
            except Exception as exc:
                record["browserCleanupError"] = str(exc)
            if common:
                try:
                    record["sessionCleanup"] = execute(common + ["close"], env, artifact / "close", timeout=20)
                    result = record["sessionCleanup"]
                    if result["exit"] != 0 or not (result["response"] or {}).get("success"):
                        record["sessionCleanupError"] = "CLI session closure was not confirmed"
                except Exception as exc:
                    record["sessionCleanupError"] = str(exc)
            events = fixture.events(case_id)
            record["trace"] = events
            pending_dom = {e.get("captureId") for e in events if e.get("type") == "dom-answer" and e.get("captureStillPending") and (e.get("result") or {}).get("rows") == args.rows}
            capture_errors = {e.get("captureId") for e in events if e.get("type") == "capture-error" and "timeout" in e.get("error", "").lower()}
            record["matchesReportSignature"] = bool(record["setupVerified"] and pending_dom.intersection(capture_errors))
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
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--chrome", type=Path, required=True)
    parser.add_argument("--extension", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--samples", type=int, default=5, choices=range(1, 6))
    parser.add_argument("--rows", type=int, default=1000, choices=range(100, 3001))
    parser.add_argument("--dpr", type=int, default=1, choices=(1, 2))
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
        "samples": args.samples, "dpr": args.dpr, "instrumented": True,
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
            # Rotate state order so a later case is not always minimized.
            states = ["normal", "covered", "minimized"]
            states = states[repeat % 3:] + states[:repeat % 3]
            for state in states:
                for kind in ("viewport", "full", "selector"):
                    if time.monotonic() >= deadline:
                        raise ProbeSetupFailure("matrix stopped at its 30-minute experiment budget")
                    record = run_case(args, fixture, windows, root, args.output, state, kind, repeat)
                    records.append(record)
                    (args.output / "progress.json").write_text(json.dumps({"expectedCases": args.samples * 9, "records": records}, indent=2), encoding="utf-8")
                    print(json.dumps({"case": record["caseId"], "setup": record["setupVerified"],
                                      "signature": record["matchesReportSignature"], "error": record.get("error")}), flush=True)
                    if not record["setupVerified"]:
                        raise ProbeSetupFailure("matrix stopped after an unverified setup")
                    if any(key.endswith("CleanupError") for key in record):
                        raise ProbeSetupFailure("matrix stopped after unconfirmed cleanup")
    except ProbeSetupFailure as exc:
        setup_error = str(exc)
    except Exception as exc:
        setup_error = type(exc).__name__ + ": " + str(exc)
    finally:
        fixture.close()
    invalid = [r for r in records if not r["setupVerified"]]
    reproduced = [r for r in records if r["matchesReportSignature"]]
    failed = [r for r in records if r["setupVerified"] and (not (r.get("capture", {}).get("response") or {}).get("success") or not r.get("image", {}).get("pixelValid") or not r.get("image", {}).get("dimensionsValid"))]
    incomplete = len(records) != args.samples * 9
    verdict = "ISSUES" if failed else "INCONCLUSIVE" if invalid or incomplete or setup_error else "PASS"
    cleanup_failed = any(any(key.endswith("CleanupError") for key in r) for r in records)
    if cleanup_failed:
        verdict = "INCONCLUSIVE"
    summary = {"verdict": verdict, "expectedCases": args.samples * 9, "cases": len(records), "setupFailures": len(invalid),
        "captureFailures": len(failed), "matchingReportSignature": len(reproduced), "records": records,
        "setupError": setup_error, "matrixComplete": not invalid and not incomplete,
        "interpretation": "PASS means this matrix did not reproduce the report; it does not prove customer Windows acceptance."}
    (args.output / "summary.json").write_text(json.dumps(summary, indent=2), encoding="utf-8")
    print(json.dumps({k: v for k, v in summary.items() if k != "records"}), flush=True)
    return 2 if invalid or cleanup_failed or setup_error or incomplete else 1 if failed else 0


if __name__ == "__main__":
    raise SystemExit(main())

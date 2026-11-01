#!/usr/bin/env python3
"""CI-only Bitwarden UI diagnostic. Synthetic metadata is not a real vault."""

import argparse
import base64
import ctypes
from ctypes import wintypes
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
import urllib.request
import urllib.parse
import uuid

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "windows-capture-798"))
from probe import Registry, Windows, checked, digest, execute, wait_for
from owned_processes import (
    OwnedProcessHandles,
    cleanup_acknowledged,
    descendants,
    processes,
)


def read_debugging_port(path, errors):
    """Observe a completed port record without restarting Chrome on a file race."""
    try:
        lines = path.read_text().splitlines()
        port = int(lines[0]) if lines else None
        return port if port is not None and 0 < port <= 65535 else None
    except (OSError, ValueError) as error:
        if len(errors) < 16:
            errors.append(type(error).__name__ + ": " + str(error))
        return None


class WorkerRequestError(RuntimeError):
    pass


def worker(endpoint, expression, operation="query"):
    try:
        result = subprocess.run(
            ["node", str(Path(__file__).with_name("worker-eval.mjs"))],
            input=json.dumps({"endpoint": endpoint, "expression": expression}),
            capture_output=True,
            text=True,
            encoding="utf-8",
            timeout=12,
        )
    except subprocess.TimeoutExpired as error:
        raise WorkerRequestError(
            operation + ": worker controller exceeded its outer deadline"
        ) from error
    if result.returncode:
        raise WorkerRequestError(
            operation
            + ": "
            + (result.stderr.strip() or "controller exited without a diagnostic")[:900]
        )
    return json.loads(result.stdout)


def worker_candidate(target, entrypoints):
    if target.get("type") != "service_worker":
        return False
    try:
        url = urllib.parse.urlsplit(target.get("url", ""))
    except ValueError:
        return False
    return url.scheme == "chrome-extension" and url.path in entrypoints


def discover_workers(port, errors, ignored, entrypoints):
    try:
        with urllib.request.urlopen(
            f"http://127.0.0.1:{port}/json/list", timeout=3
        ) as response:
            targets = json.load(response)
    except (OSError, ValueError) as error:
        errors.append({"operation": "list-workers", "error": str(error)[:300]})
        return {}
    if not isinstance(targets, list):
        errors.append(
            {"operation": "list-workers", "error": "worker list is not an array"}
        )
        return {}
    result = {}
    ambiguous = set()
    for target in targets:
        if not worker_candidate(target, entrypoints):
            if (
                target.get("type") == "service_worker"
                and target.get("url") not in ignored
            ):
                ignored.append(target.get("url"))
            continue
        endpoint = target.get("webSocketDebuggerUrl")
        if not endpoint:
            errors.append(
                {"operation": "manifest", "error": "worker endpoint not published yet"}
            )
            continue
        try:
            identity = worker(
                endpoint,
                "({id:chrome.runtime.id,version:chrome.runtime.getManifest().version,name:chrome.runtime.getManifest().name})",
                "manifest",
            )
        except WorkerRequestError as error:
            # Discovery is read-only. Re-list the same Chrome process until
            # its startup worker realms are ready; never restart the browser.
            errors.append(
                {"operation": "manifest", "target": target["url"], "error": str(error)}
            )
            continue
        if not isinstance(identity, dict):
            errors.append(
                {"operation": "manifest", "error": "worker identity is not an object"}
            )
            continue
        if identity.get("id") != urllib.parse.urlsplit(target["url"]).hostname:
            errors.append(
                {
                    "operation": "manifest",
                    "error": "worker identity does not match its target origin",
                }
            )
            continue
        name = (
            "bitwarden"
            if identity.get("version") == "2026.10.1"
            else "chromeUse"
            if identity.get("name") == "chrome-use"
            else None
        )
        if name:
            if name in result:
                ambiguous.add(name)
                errors.append(
                    {
                        "operation": "manifest",
                        "error": "multiple live " + name + " workers",
                    }
                )
            result[name] = (endpoint, identity)
    return {name: value for name, value in result.items() if name not in ambiguous}


def fixture_storage_status(endpoint):
    expression = """(async()=>{
      const user='78000000-0000-4000-8000-000000000780';
      const keys=['global_account_accounts','global_account_activeAccountId',
        'global_autofillSettingsLocal_inlineMenuVisibility','user_'+user+'_token_accessToken'];
      const raw=await chrome.storage.local.get(keys);
      const read=key=>raw[key]?.__json__===true?JSON.parse(raw[key].value):raw[key];
      const accounts=read(keys[0])||{};
      return {accountCount:Object.keys(accounts).length,fixtureAccountPresent:!!accounts[user],
        activeFixture:read(keys[1])===user,inlineMenuVisibility:read(keys[2])??null,
        fixtureTokenPresent:!!read(keys[3])};
    })()"""
    return worker(endpoint, expression)


def official_runtime_status(endpoint):
    expression = """(async()=>{
      const main=self.bitwardenMain;
      if(!main?.authService||!main?.accountService)return {servicesReady:false};
      const first=observable=>new Promise(resolve=>{
        let subscription;
        const timer=setTimeout(()=>{subscription?.unsubscribe();resolve({timedOut:true});},1500);
        subscription=observable.subscribe({next:value=>{clearTimeout(timer);resolve({value});queueMicrotask(()=>subscription?.unsubscribe());},
          error:error=>{clearTimeout(timer);resolve({error:String(error?.message||error).slice(0,200)});}});
      });
      const [active,auth]=await Promise.all([first(main.accountService.activeAccount$),first(main.authService.activeAccountStatus$)]);
      const monitoring=main.autofillLifecycleService?.monitoringState?.value;
      return {servicesReady:true,activeFixture:active.value?.id==='78000000-0000-4000-8000-000000000780',
        activeTimedOut:active.timedOut===true,authStatus:auth.value??null,authError:auth.error??null,authTimedOut:auth.timedOut===true,
        monitoringFrames:monitoring instanceof Map?monitoring.size:null,
        activeMonitoringFrames:monitoring instanceof Map?[...monitoring.values()].filter(Boolean).length:null};
    })()"""
    return worker(endpoint, expression, "runtime-status")


def official_content_status(endpoint, tab_id):
    # Read only Bitwarden's own isolated realm in our disposable web fixture.
    # No main-world property, DOM node, listener, or event is added.
    expression = """(async()=>{return await chrome.scripting.executeScript({
      target:{tabId:__TAB__,frameIds:[0]},world:'ISOLATED',func:()=>{
        const init=globalThis.bitwardenAutofillInit;
        const overlay=init?.autofillOverlayContentService;
        const ui=init?.autofillInlineMenuContentService;
        const summarize=element=>{if(!element?.getBoundingClientRect)return null;const r=element.getBoundingClientRect();
          return {connected:element.isConnected,width:r.width,height:r.height,display:getComputedStyle(element).display};};
        const email=document.getElementById('email');
        return {initialized:!!init,emailCached:overlay?.formFieldElements?.has(email)??false,
          cachedFields:overlay?.formFieldElements?[...overlay.formFieldElements.keys()].map(el=>({id:el.id,type:el.type})):[],
          emailRect:summarize(email),button:summarize(ui?.buttonElement),list:summarize(ui?.listElement),
          recentField:overlay?.mostRecentlyFocusedField?.id??null,
          initKeys:init?Object.keys(init):[],
          overlayKeys:init?.autofillOverlayContentService?Object.keys(init.autofillOverlayContentService):[],
          overlayMonitoring:init?.autofillOverlayContentService?.isMonitoring??null,
          visibility:document.visibilityState,focused:document.hasFocus(),
          activeElement:document.activeElement?.id||null,ready:document.readyState};
      }});})()""".replace("__TAB__", str(tab_id))
    return worker(endpoint, expression, "content-status")


def seed_locked_fixture(endpoint):
    if fixture_storage_status(endpoint)["accountCount"] != 0:
        raise RuntimeError(
            "fixture profile already has account metadata; refusing to overwrite it"
        )
    user = "78000000-0000-4000-8000-000000000780"
    # A local UI fixture token, never accepted by any server. DNS for external
    # services is disabled for this entire disposable Chrome process.
    encode = lambda value: (
        base64.urlsafe_b64encode(json.dumps(value).encode()).decode().rstrip("=")
    )
    token = (
        encode({"alg": "none", "typ": "JWT"})
        + "."
        + encode({"sub": user, "email": "fixture@example.invalid", "exp": 4102444800})
        + ".fixture-not-a-signature"
    )
    seed = {
        "global_account_accounts": {
            user: {
                "email": "fixture@example.invalid",
                "emailVerified": True,
                "name": "Disposable fixture",
            }
        },
        "global_account_activeAccountId": user,
        "global_autofillSettingsLocal_inlineMenuVisibility": 2,
        f"user_{user}_token_accessToken": token,
    }
    # The official storage adapter intentionally ignores multi-key change
    # notifications. Match its per-key serialized writes so derived account
    # observables adopt the fixture state, then verify adoption separately.
    expression = (
        "(async()=>{for(const [key,value] of Object.entries("
        + json.dumps(seed)
        + ")){await chrome.storage.local.set({[key]:{__json__:true,value:JSON.stringify(value)}});}return {syntheticAccount:true,version:chrome.runtime.getManifest().version};})()"
    )
    return worker(endpoint, expression, "seed-fixture")


class FormFixture:
    def __init__(self, case):
        self.events = []
        self.lock = threading.Lock()
        owner = self
        html = """<!doctype html><title>BitwardenFixture CASE</title>
        <form id="fixture"><label>Email<input id="email" name="email" type="email" autocomplete="username"></label>
        <label>Password<input id="password" name="password" type="password" autocomplete="current-password"></label>
        <button id="continue" type="button">Continue locally</button>
        <button type="submit">Do not submit</button></form>
        <script>
        const recordedEvents=[];let continuations=0;
        document.getElementById('continue').addEventListener('click',()=>continuations++);
        window.fixtureState=()=>({caseId:'CASE',events:recordedEvents.slice(),continuations});
        for(const type of ['focus','blur','keydown','keyup','click','visibilitychange','submit'])
          document.addEventListener(type,event=>{
            if(type==='submit')event.preventDefault();
            const observed={type,trusted:event.isTrusted,key:event.key||null,
              code:event.code||null,visibility:document.visibilityState,atMs:performance.now()};
            recordedEvents.push(observed);
            fetch('/event',{method:'POST',body:JSON.stringify(observed)});
          },true);
        </script>""".replace("CASE", case).encode()

        class Handler(http.server.BaseHTTPRequestHandler):
            def log_message(self, *_):
                pass

            def do_GET(self):
                self.send_response(200)
                self.send_header("Content-Type", "text/html")
                self.end_headers()
                self.wfile.write(html)

            def do_POST(self):
                size = int(self.headers.get("Content-Length", "0"))
                if self.path != "/event" or not 0 < size < 4096:
                    self.send_error(400)
                    return
                event = json.loads(self.rfile.read(size))
                with owner.lock:
                    owner.events.append(event)
                self.send_response(204)
                self.end_headers()

        self.server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.url = f"http://127.0.0.1:{self.server.server_port}/login?{case}"
        threading.Thread(target=self.server.serve_forever, daemon=True).start()

    def close(self):
        self.server.shutdown()
        self.server.server_close()


def menu_frames(endpoint, fixture_url, bitwarden_id):
    expression = """(async()=>{const matches=(await chrome.tabs.query({})).filter(t=>t.url===URL);
    if(matches.length!==1)throw new Error('fixture tab is not unique');
    const tab=matches[0],frames=await chrome.webNavigation.getAllFrames({tabId:tab.id});
    const owner=chrome.runtime.id===BW;
    const paths=['/overlay/menu-button.html','/overlay/menu-list.html','/overlay/menu.html'];
    const expected=owner?paths.map(path=>chrome.runtime.getURL(path.slice(1))):[];
    const contexts=owner?await chrome.runtime.getContexts({tabIds:[tab.id]}):[];
    const sameResource=(raw,allowed)=>{try{const u=new globalThis.URL(raw),e=new globalThis.URL(allowed);
      return u.protocol===e.protocol&&u.host===e.host&&u.pathname===e.pathname;}catch{return false;}};
    const ownedContext=context=>{try{const u=new globalThis.URL(context.documentUrl);
      return context.tabId===tab.id&&u.protocol==='chrome-extension:'&&paths.includes(u.pathname);}catch{return false;}};
    const found=frames.filter(frame=>expected.some(url=>sameResource(frame.url,url)))
      .map(frame=>({frameId:frame.frameId,url:frame.url,proof:'owner-runtime-url'}));
    for(const context of contexts.filter(ownedContext))if(!found.some(frame=>frame.frameId===context.frameId&&frame.url===context.documentUrl))
      found.push({frameId:context.frameId,url:context.documentUrl,documentId:context.documentId,proof:'owner-runtime-context'});
    return {observerExtensionId:chrome.runtime.id,tabId:tab.id,windowId:tab.windowId,active:tab.active,
      expectedMenuUrls:expected,observedFrames:frames.map(frame=>({frameId:frame.frameId,url:frame.url})),
      ownerContexts:contexts.map(context=>({contextType:context.contextType,frameId:context.frameId,tabId:context.tabId,documentUrl:context.documentUrl})),
      bitwardenFrames:found};})()"""
    return worker(
        endpoint,
        expression.replace("===URL", "===" + json.dumps(fixture_url)).replace(
            "===BW", "===" + json.dumps(bitwarden_id)
        ),
        "menu-contexts",
    )


def native_focus(windows, hwnd, pid, title):
    before = windows.state(hwnd)
    if before["pid"] != pid or title not in before["title"] or before["minimized"]:
        raise RuntimeError("native input target not verified")
    user32 = ctypes.WinDLL("user32", use_last_error=True)
    user32.SetForegroundWindow.argtypes = [wintypes.HWND]
    user32.SetForegroundWindow.restype = wintypes.BOOL
    user32.SetForegroundWindow(hwnd)
    focused = wait_for(
        lambda: windows.state(hwnd),
        lambda w: w["foreground"] == hwnd and w["centerOwned"] is True,
        2,
    )
    if focused["visible"] is not True:
        raise RuntimeError("foreground fixture window is not visible")
    return before, focused


def restore_fixture_cover(windows, hwnd, pid, case, cover_hwnd, before_focus):
    """Restore only this controller's cover, provided nobody moved focus away."""
    page = windows.state(hwnd)
    cover = windows.state(cover_hwnd)
    if (
        before_focus["foreground"] != cover_hwnd
        or page["foreground"] != hwnd
        or page["pid"] != pid
        or page["title"] != before_focus["title"]
        or not page["title"].startswith("BitwardenFixture " + case)
        or cover["pid"] != os.getpid()
        or cover["title"] != "Bitwarden fixture cover " + case
        or cover["minimized"]
        or not cover["visible"]
    ):
        return {
            "attempted": False,
            "restored": False,
            "reason": "foreground-or-identity-changed",
        }
    # One attempt, followed by observation. Do not retry an uncertain change or
    # move focus again if another window took the foreground in the meantime.
    admitted = bool(windows.u.SetForegroundWindow(cover_hwnd))
    observed = wait_for(
        lambda: windows.state(cover_hwnd),
        lambda state: state["foreground"] == cover_hwnd and state["centerOwned"],
        2,
    )
    if observed["pid"] != os.getpid() or observed["title"] != cover["title"]:
        raise RuntimeError("restored cover identity changed")
    return {
        "attempted": True,
        "admitted": admitted,
        "restored": True,
        "window": observed,
    }


def native_menu_image(windows, hwnd, pid, case, path):
    from PIL import ImageGrab

    observed = windows.state(hwnd)
    if (
        observed["pid"] != pid
        or case not in observed["title"]
        or observed["foreground"] != hwnd
        or observed["centerOwned"] is not True
    ):
        raise RuntimeError("native image target is not the foreground CI fixture")
    rectangle = observed["rect"]
    image = ImageGrab.grab(bbox=tuple(rectangle), all_screens=True)
    expected = (rectangle[2] - rectangle[0], rectangle[3] - rectangle[1])
    if image.size != expected:
        raise RuntimeError("native image crop dimensions changed")
    image.save(path)
    return {
        "sha256": digest(path),
        "width": image.width,
        "height": image.height,
        "window": observed,
    }


def inspect_native_fixture(hwnd, pid, case, fixture_url):
    production = (
        Path(__file__).resolve().parents[2]
        / "cli/src/native/focus_recovery/windows-inspect.ps1"
    )
    script_path = (
        production
        if production.is_file()
        else Path(__file__).with_name("native-ui-inspect.ps1")
    )
    script = script_path.read_text()
    encoded = base64.b64encode(script.encode("utf-16-le")).decode("ascii")
    result = subprocess.run(
        ["powershell.exe", "-NoProfile", "-NonInteractive", "-EncodedCommand", encoded],
        input=json.dumps(
            {
                "hwnd": hwnd,
                "pid": pid,
                "title": "BitwardenFixture " + case,
                "url": fixture_url,
            }
        ),
        capture_output=True,
        text=True,
        encoding="utf-8",
        timeout=15,
        creationflags=0x08000000,
    )
    if result.returncode:
        raise RuntimeError(
            "native UI inspection failed: " + result.stderr.strip()[:600]
        )
    observed = json.loads(result.stdout)
    observed["reader"] = "production" if production.is_file() else "ci-prototype"
    observed["readerSha256"] = digest(script_path)
    return observed


def send_escape(hwnd):
    extra = ctypes.c_size_t

    class Mouse(ctypes.Structure):
        _fields_ = [
            ("dx", wintypes.LONG),
            ("dy", wintypes.LONG),
            ("mouseData", wintypes.DWORD),
            ("flags", wintypes.DWORD),
            ("time", wintypes.DWORD),
            ("extra", extra),
        ]

    class Key(ctypes.Structure):
        _fields_ = [
            ("vk", wintypes.WORD),
            ("scan", wintypes.WORD),
            ("flags", wintypes.DWORD),
            ("time", wintypes.DWORD),
            ("extra", extra),
        ]

    class Hardware(ctypes.Structure):
        _fields_ = [
            ("message", wintypes.DWORD),
            ("low", wintypes.WORD),
            ("high", wintypes.WORD),
        ]

    class Payload(ctypes.Union):
        _fields_ = [("mouse", Mouse), ("key", Key), ("hardware", Hardware)]

    class Input(ctypes.Structure):
        _fields_ = [("kind", wintypes.DWORD), ("payload", Payload)]

    assert ctypes.sizeof(Input) == 40, "x64 Windows INPUT ABI"
    user32 = ctypes.WinDLL("user32", use_last_error=True)
    user32.GetForegroundWindow.restype = wintypes.HWND
    if user32.GetForegroundWindow() != hwnd:
        raise RuntimeError("foreground changed; no input sent")
    user32.GetAsyncKeyState.argtypes = [ctypes.c_int]
    user32.GetAsyncKeyState.restype = wintypes.SHORT
    if any(user32.GetAsyncKeyState(key) & 0x8000 for key in [16, 17, 18, 91, 92]):
        raise RuntimeError("modifier key is held; no native Escape sent")
    user32.SendInput.argtypes = [wintypes.UINT, ctypes.POINTER(Input), ctypes.c_int]
    user32.SendInput.restype = wintypes.UINT
    inputs = (Input * 2)(
        Input(1, Payload(key=Key(27, 1, 0, 0, 0))),
        Input(1, Payload(key=Key(27, 1, 2, 0, 0))),
    )
    count = user32.SendInput(2, inputs, ctypes.sizeof(Input))
    if count != 2:
        raise RuntimeError(
            f"unknown native input outcome: {count}/2 events admitted; never replay"
        )
    return {"admitted": count, "replayed": False}


def terminate_owned(chrome):
    owned = descendants(processes(), chrome.pid)
    handles = OwnedProcessHandles(owned)
    try:
        killed = subprocess.run(
            ["taskkill", "/PID", str(chrome.pid), "/T", "/F"],
            capture_output=True,
            text=True,
            timeout=20,
        )
        parent = chrome.wait(timeout=20)
        exits = handles.observe()
    finally:
        handles.close()
    after = processes()
    post = descendants(after, chrome.pid, owned)
    result = {
        "before": owned,
        "taskkill": {
            "exit": killed.returncode,
            "stdout": killed.stdout,
            "stderr": killed.stderr,
        },
        "parentExit": parent,
        "ownedHandleExits": exits,
        "postTree": post,
    }
    result["verified"] = cleanup_acknowledged(
        result["taskkill"], owned, exits, parent, post
    )
    return result


def protected_access_refusal(result):
    response = result.get("response") or {}
    return (
        result.get("exit") == 1
        and result.get("killedAtDeadline") is False
        and response.get("success") is False
        and response.get("code") == "debugger_access_denied"
    )


def native_escape_delivered(events):
    keys = [event for event in events if event.get("type") in ["keydown", "keyup"]]
    return [event.get("type") for event in keys] == ["keydown", "keyup"] and all(
        event.get("key") == "Escape"
        and event.get("code") == "Escape"
        and event.get("trusted") is True
        for event in keys
    )


def session_close_confirmed(result):
    response = result.get("response") or {}
    data = response.get("data") or {}
    return (
        result.get("exit") == 0
        and result.get("killedAtDeadline") is False
        and response.get("success") is True
        and data.get("closed") is True
        and data.get("verifiedAbsent") is True
    )


def window_is_topmost(windows, hwnd):
    getter = windows.u.GetWindowLongPtrW
    getter.argtypes = [wintypes.HWND, ctypes.c_int]
    getter.restype = ctypes.c_ssize_t
    ctypes.set_last_error(0)
    style = getter(hwnd, -20)
    if style == 0 and ctypes.get_last_error():
        raise ctypes.WinError(ctypes.get_last_error())
    return bool(style & 0x00000008)


def close_fixture_session(record, common, environment, output):
    if common:
        try:
            record["sessionCleanup"] = execute(
                common + ["close"], environment, output / "close", timeout=20
            )
            record["sessionCleanupConfirmed"] = session_close_confirmed(
                record["sessionCleanup"]
            )
            if not record["sessionCleanupConfirmed"]:
                record["sessionCleanupError"] = "session closure was not confirmed"
        except Exception as error:
            record["sessionCleanupConfirmed"] = False
            record["sessionCleanupError"] = str(error)[:300]
    else:
        record["sessionCleanupStatus"] = "not-started"
        record["sessionCleanupConfirmed"] = True
    if not record["sessionCleanupConfirmed"]:
        record["verdict"] = "INCONCLUSIVE"


def registry_restored(registry):
    api = registry.winreg
    for path, existed, original in registry.saved:
        try:
            with api.OpenKey(api.HKEY_CURRENT_USER, path) as key:
                try:
                    current = api.QueryValueEx(key, "")
                except FileNotFoundError:
                    current = None
        except FileNotFoundError:
            if existed:
                return False
            continue
        if not existed or current != original:
            return False
    return True


def prepare_process_observer(record):
    history = []
    record["processObserverPreflight"] = history
    for attempt in range(3):
        try:
            snapshot = processes()
            if not any(item.get("ProcessId") == os.getpid() for item in snapshot):
                raise ValueError("process snapshot does not contain this controller")
            descendants(snapshot, os.getpid())
        except (
            subprocess.TimeoutExpired,
            subprocess.CalledProcessError,
            OSError,
            ValueError,
            KeyError,
        ) as error:
            history.append(
                {
                    "ready": False,
                    "error": type(error).__name__ + ": " + str(error)[:400],
                }
            )
            if attempt < 2:
                time.sleep(0.25)
            continue
        history.append({"ready": True, "observedProcesses": len(snapshot)})
        return
    raise RuntimeError(
        "process identity observer is not ready; no fixture browser was started"
    )


def foreground_blocked_without_input(result):
    response = result.get("response") or {}
    native = response.get("nativeFocus") or {}
    return (
        result.get("exit") == 1
        and result.get("killedAtDeadline") is False
        and response.get("success") is False
        and str(response.get("error", "")).startswith(
            "Foreground did not become the attested Chrome within 500ms; no Escape was sent"
        )
        and "input" in native
        and native["input"] is None
        and native.get("originalOperationReplayed") is False
    )


def foreground_permission_control(windows, hwnd, chrome_pid, cover_hwnd, client=None, env=None, artifact=None):
    """CI control: delegate rights from our foreground cover to our Chrome only."""
    cover = windows.state(cover_hwnd)
    target = windows.state(hwnd)
    if (
        chrome_pid <= 0
        or chrome_pid >= 0xFFFFFFFF
        or cover["pid"] != os.getpid()
        or cover["foreground"] != cover_hwnd
        or target["foreground"] != cover_hwnd
        or target["pid"] != chrome_pid
        or cover_hwnd == hwnd
    ):
        raise RuntimeError("foreground grant requires our unchanged cover and Chrome")
    child = None
    if client is None:
        delegate = windows.u.AllowSetForegroundWindow
        delegate.argtypes = [wintypes.DWORD]
        delegate.restype = wintypes.BOOL
        ctypes.set_last_error(0)
        admitted = bool(delegate(chrome_pid))
        error = 0 if admitted else ctypes.get_last_error()
    else:
        child = execute([str(client), "--ci-allow-foreground-delegation", str(chrome_pid)], env, artifact, timeout=10)
        data = (child.get("response") or {}).get("data") or {}
        if (
            child.get("killedAtDeadline") is not False
            or child.get("exit") not in (0, 1)
            or data.get("chromePid") != chrome_pid
            or not isinstance(data.get("callerPid"), int)
            or data["callerPid"] in (0, os.getpid())
            or data.get("foregroundBefore") != cover_hwnd
            or data.get("foregroundPidBefore") != os.getpid()
            or data.get("nativeInputAttempted") is not False
            or data.get("windowActivationAttempted") is not False
            or not isinstance(data.get("admitted"), bool)
            or child.get("exit") != (0 if data.get("admitted") is True else 1)
            or (child.get("response") or {}).get("success") is not data.get("admitted")
            or data.get("foregroundAfter") != cover_hwnd
            or data.get("foregroundPidAfter") != os.getpid()
            or not isinstance(data.get("errorCode"), int)
        ):
            raise RuntimeError("foreground child control outcome is unconfirmed; no operation was repeated")
        admitted = data["admitted"]
        error = data.get("errorCode")
    after = windows.state(cover_hwnd)
    return {
        "controllerPid": os.getpid(),
        "chromePid": chrome_pid,
        "admitted": admitted,
        "errorCode": error,
        "foregroundUnchanged": after["pid"] == os.getpid()
        and after["foreground"] == cover_hwnd,
        "unassistedProductionAcceptance": False,
        "childControl": child,
    }


def main():
    if platform.system() != "Windows" or os.environ.get("GITHUB_ACTIONS") != "true":
        raise SystemExit("Only a disposable Windows GitHub runner is allowed")
    parser = argparse.ArgumentParser()
    for name in ["binary", "chrome", "extension", "bitwarden", "output"]:
        parser.add_argument("--" + name, type=Path, required=True)
    parser.add_argument(
        "--setup-mode",
        choices=["default", "foreground", "background"],
        default="default",
    )
    parser.add_argument("--inspect-native-ui", action="store_true")
    parser.add_argument("--runtime-focus-recovery", action="store_true")
    parser.add_argument("--grant-chrome-foreground", action="store_true")
    parser.add_argument("--foreground-grant-client", type=Path)
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=True)
    case = "bw780-" + uuid.uuid4().hex[:10]
    fixture = None
    record = {
        "caseId": case,
        "verdict": "INCONCLUSIVE",
        "scope": "Windows CI + official Bitwarden 2026.10.1 and synthetic account metadata; auth/UI state requires observation, no real account or vault",
        "probeGitSha": os.environ.get("GITHUB_SHA"),
        "binaryBuildGitSha": os.environ.get(
            "PROBE_BINARY_BUILD_SHA", os.environ.get("GITHUB_SHA")
        ),
        "binarySource": os.environ.get("PROBE_BINARY_SOURCE", "built"),
        "realVaultTested": False,
        "productionRecoveryImplemented": False,
        "setupMode": args.setup_mode,
        "runtimeFocusRecoveryRequested": args.runtime_focus_recovery,
        "foregroundPermissionControlRequested": args.grant_chrome_foreground,
        "foregroundGrantCaller": "child" if args.foreground_grant_client else "controller",
        "browserStarted": False,
        "registryRestored": False,
        "sessionCleanupConfirmed": False,
    }
    try:
        record["stage"] = "input-identities"
        record.update(
            binarySha256=digest(args.binary),
            chromeSha256=digest(args.chrome),
            bitwardenBackgroundSha256=digest(args.bitwarden / "background.js"),
            sourceExtensionBackgroundSha256=digest(args.extension / "background.js"),
        )
        record["stage"] = "process-observer-preflight"
        prepare_process_observer(record)
        record["stage"] = "fixture-prerequisites"
        fixture = FormFixture(case)
        windows = Windows()
        with tempfile.TemporaryDirectory(prefix="chrome780-") as tmp:
            root = Path(tmp)
            extension = root / "chrome-use"
            shutil.copytree(args.extension, extension)
            manifest_path = extension / "manifest.json"
            manifest = json.loads(manifest_path.read_text())
            # Equivalent to granting only our disposable fixture site access.
            manifest.setdefault("host_permissions", []).append("http://127.0.0.1/*")
            manifest_path.write_text(json.dumps(manifest, indent=2))
            record["fixtureManifestHostPermission"] = "http://127.0.0.1/*"
            record["sourceExtensionVersion"] = manifest["version"]
            key = hashlib.sha256(base64.b64decode(manifest["key"])).hexdigest()[:32]
            extension_id = "".join(chr(ord("a") + int(c, 16)) for c in key)
            profile = root / "profile"
            relay = root / "relay"
            relay.mkdir()
            env = dict(
                os.environ,
                CHROME_USE_RELAY_DIR=str(relay),
                AGENT_BROWSER_SOCKET_DIR=str(root / "socket"),
                CHROME_USE_NO_UPDATE_CHECK="1",
                AGENT_BROWSER_AUTO_LOGIN="off",
            )
            registry = Registry(args.binary, root, extension_id)
            with registry, (args.output / "chrome.log").open("w") as log:
                chrome = subprocess.Popen(
                    [
                        str(args.chrome),
                        "--no-first-run",
                        "--no-default-browser-check",
                        "--user-data-dir=" + str(profile),
                        "--remote-debugging-port=0",
                        "--load-extension="
                        + str(extension)
                        + ","
                        + str(args.bitwarden),
                        "--disable-extensions-except="
                        + str(extension)
                        + ","
                        + str(args.bitwarden),
                        "--disable-background-networking",
                        "--host-resolver-rules=MAP * ~NOTFOUND, EXCLUDE localhost, EXCLUDE 127.0.0.1",
                        "--window-size=1280,800",
                        "about:blank",
                    ],
                    env=env,
                    stdout=log,
                    stderr=log,
                )
                record["chromePid"] = chrome.pid
                record["browserStarted"] = True
                common = None
                close_cover = None
                try:
                    record["debuggingPortReadErrors"] = []
                    port = wait_for(
                        lambda: read_debugging_port(
                            profile / "DevToolsActivePort",
                            record["debuggingPortReadErrors"],
                        ),
                        lambda value: value is not None,
                        15,
                    )
                    record["stage"] = "worker-discovery"
                    record["workerDiscoveryErrors"] = []
                    record["ignoredWorkerTargets"] = []
                    bw_manifest = json.loads(
                        (args.bitwarden / "manifest.json").read_text()
                    )
                    entrypoints = {
                        "/" + m["background"]["service_worker"].lstrip("/")
                        for m in [manifest, bw_manifest]
                    }
                    record["declaredWorkerEntrypoints"] = sorted(entrypoints)
                    controls = wait_for(
                        lambda: discover_workers(
                            port,
                            record["workerDiscoveryErrors"],
                            record["ignoredWorkerTargets"],
                            entrypoints,
                        ),
                        lambda x: set(x) == {"chromeUse", "bitwarden"},
                        30,
                    )
                    bw_endpoint, bw_identity = controls["bitwarden"]
                    our_endpoint, our_identity = controls["chromeUse"]
                    record["extensions"] = {
                        "bitwarden": bw_identity,
                        "chromeUse": our_identity,
                    }
                    record["stage"] = "seed-fixture"
                    record["seed"] = seed_locked_fixture(bw_endpoint)
                    record["storageAfterSeed"] = fixture_storage_status(bw_endpoint)
                    record["runtimeAfterSeed"] = wait_for(
                        lambda: official_runtime_status(bw_endpoint),
                        lambda state: (
                            state.get("activeFixture") is True
                            and state.get("authStatus") == 1
                        ),
                        5,
                    )
                    record["stealth"] = worker(
                        our_endpoint,
                        '(async()=>{await chrome.storage.local.set({ab_stealth:true});return (await chrome.storage.local.get("ab_stealth")).ab_stealth;})()',
                    )
                    files = wait_for(
                        lambda: list(relay.glob("relay-cdp-url-*")),
                        lambda f: len(f) == 1,
                        30,
                    )
                    browser = files[0].name.removeprefix("relay-cdp-url-")
                    common = [
                        str(args.binary),
                        "--session",
                        case,
                        "--browser",
                        browser,
                        "--window",
                        "dedicated",
                        "--json",
                    ]
                    invoke = lambda name, *words: execute(
                        common + list(words), env, args.output / name, timeout=40
                    )
                    record["stage"] = "open-fixture"
                    record["open"] = invoke("open", "open", fixture.url)
                    opened = checked(record["open"])
                    if opened.get("url") != fixture.url:
                        raise RuntimeError(
                            "fixture URL not reached: " + str(opened.get("url"))
                        )
                    hwnd = wait_for(
                        lambda: windows.find(chrome.pid, "BitwardenFixture " + case),
                        lambda x: x is not None,
                        5,
                    )
                    record["stage"] = "window-setup"
                    record["windowBefore"] = windows.state(hwnd)
                    if args.setup_mode == "foreground":
                        before, after = native_focus(
                            windows, hwnd, chrome.pid, "BitwardenFixture " + case
                        )
                        record["setupNativeFocus"] = {"before": before, "after": after}
                    elif args.setup_mode == "background":
                        close_cover, cover_hwnd = windows.cover(
                            hwnd, "Bitwarden fixture cover " + case, topmost=False
                        )
                        record["setupCover"] = {
                            "window": windows.state(cover_hwnd),
                            "coveredChrome": windows.state(hwnd),
                            "topmost": window_is_topmost(windows, cover_hwnd),
                        }
                        if record["setupCover"]["topmost"]:
                            raise RuntimeError("fixture cover is unexpectedly topmost")
                    record["stage"] = "field-readiness"
                    record["storageBeforeFocus"] = fixture_storage_status(bw_endpoint)
                    record["runtimeBeforeFocus"] = official_runtime_status(bw_endpoint)
                    fixture_tab = menu_frames(
                        bw_endpoint, fixture.url, bw_identity["id"]
                    )
                    record["contentBeforeFocus"] = official_content_status(
                        bw_endpoint, fixture_tab["tabId"]
                    )
                    record["contentReady"] = wait_for(
                        lambda: official_content_status(
                            bw_endpoint, fixture_tab["tabId"]
                        ),
                        lambda items: any(
                            item.get("result", {}).get("initialized") is True
                            and item.get("result", {}).get("overlayMonitoring") is True
                            and item.get("result", {}).get("emailCached") is True
                            for item in items
                        ),
                        8,
                    )
                    if args.runtime_focus_recovery:
                        record["counterBeforeRuntime"] = invoke(
                            "counter-before-runtime", "eval", "window.fixtureState()"
                        )
                        initial_state = (
                            checked(record["counterBeforeRuntime"]).get("result") or {}
                        )
                        if (
                            initial_state.get("caseId") != case
                            or initial_state.get("continuations") != 0
                        ):
                            raise RuntimeError(
                                "runtime fixture did not start with a verified zero counter"
                            )
                    # One focus action only. A failure may mean focus took effect:
                    # never replay it in search of a successful acknowledgement.
                    record["stage"] = "field-focus"
                    record["focus"] = invoke("focus", "click", "#email")
                    record["contentAfterFocus"] = official_content_status(
                        bw_endpoint, fixture_tab["tabId"]
                    )
                    record["relayObservedMenuAfterFocus"] = menu_frames(
                        our_endpoint, fixture.url, bw_identity["id"]
                    )
                    record["storageAfterFocus"] = fixture_storage_status(bw_endpoint)
                    record["stage"] = "menu-open"
                    record["initialMenu"] = wait_for(
                        lambda: menu_frames(
                            bw_endpoint, fixture.url, bw_identity["id"]
                        ),
                        lambda x: len(x["bitwardenFrames"]) > 0,
                        8,
                    )
                    record["stage"] = "blocked-read"
                    record["accessBefore"] = invoke(
                        "access-before", "eval", "document.title"
                    )
                    if not protected_access_refusal(record["accessBefore"]):
                        raise RuntimeError(
                            "protected-access failure was not reproduced"
                        )
                    menu = menu_frames(bw_endpoint, fixture.url, bw_identity["id"])
                    record["menuAfterExistingGuard"] = menu
                    if args.runtime_focus_recovery:
                        if not menu["bitwardenFrames"]:
                            raise RuntimeError(
                                "runtime recovery requires the actual blocked menu to remain present"
                            )
                        record["stage"] = "runtime-focus-recovery"
                        record["windowBeforeRuntime"] = windows.state(hwnd)
                        record["runtimeRecovery"] = invoke(
                            "runtime-recovery",
                            "--allow-focus-recovery",
                            "click",
                            "#continue",
                        )
                        if args.grant_chrome_foreground:
                            record["unassistedRuntimeRecovery"] = record["runtimeRecovery"]
                            record["windowAfterFirstRuntime"] = windows.state(hwnd)
                            record["coverAfterFirstRuntime"] = windows.state(cover_hwnd)
                            if foreground_blocked_without_input(record["runtimeRecovery"]):
                                if (
                                    args.setup_mode != "background"
                                    or any(e.get("type") in ("keydown", "keyup") for e in fixture.events)
                                ):
                                    raise RuntimeError("foreground control requires a verified no-input foreground refusal")
                                record["foregroundPermissionControl"] = foreground_permission_control(
                                    windows, hwnd, chrome.pid, cover_hwnd,
                                    args.foreground_grant_client, env, args.output / "foreground-client"
                                )
                                control = record["foregroundPermissionControl"]
                                if not control["admitted"] or not control["foregroundUnchanged"]:
                                    raise RuntimeError("foreground permission delegation was not verified")
                                record["scope"] += "; controller-granted foreground rights, NOT unassisted production recovery acceptance"
                                record["runtimeRecovery"] = invoke(
                                    "runtime-recovery-granted", "--allow-focus-recovery", "click", "#continue"
                                )
                            else:
                                record["foregroundPermissionControlSkipped"] = "The first operation was not a verified no-input foreground refusal; it was not repeated"
                            record["windowAfterRuntime"] = windows.state(hwnd)
                            record["coverAfterRuntime"] = windows.state(cover_hwnd)
                            if record["runtimeRecovery"].get("exit") == 0:
                                record["runtimeImmediateRead"] = invoke(
                                    "runtime-immediate-read", "eval", "({title:document.title,fixture:window.fixtureState()})"
                                )
                        checked(record["runtimeRecovery"])
                        receipt = (
                            record["runtimeRecovery"]["response"].get("nativeFocus")
                            or {}
                        )
                        native_input = receipt.get("input") or {}
                        native_restore = (receipt.get("restore") or {}).get(
                            "restore"
                        ) or {}
                        binding = native_input.get("beforeKeyDown") or {}
                        if (
                            native_input.get("keyDownAdmitted") != 1
                            or native_input.get("keyUpAdmitted") != 1
                            or native_input.get("delivery") != "unverified"
                            or native_input.get("replayed") is not False
                            or binding.get("hwnd") != hwnd
                            or binding.get("pid") != chrome.pid
                            or receipt.get("originalOperationReplayed") is not False
                            or native_restore.get("restored") is not True
                        ):
                            raise RuntimeError(
                                "runtime native admission/binding/restore receipt was not verified"
                            )
                        record["runtimeNativeReceipt"] = receipt
                        record["nativeEscape"] = {
                            "source": "runtime-native-host",
                            "admitted": 2,
                            "replayed": False,
                            "nativeInput": native_input,
                        }
                        record["mechanism"] = "native-input-attempted"
                        record["menuAfterNative"] = wait_for(
                            lambda: menu_frames(
                                bw_endpoint, fixture.url, bw_identity["id"]
                            ),
                            lambda item: not item["bitwardenFrames"],
                            4,
                        )
                        if args.setup_mode == "background":
                            restored_window = windows.state(cover_hwnd)
                            if (
                                restored_window["foreground"] != cover_hwnd
                                or not restored_window["centerOwned"]
                            ):
                                raise RuntimeError(
                                    "runtime did not return the original CI cover to the foreground"
                                )
                            record["nativeFocusRestore"] = {
                                **native_restore,
                                "source": "runtime-native-host",
                                "window": restored_window,
                            }
                    elif not menu["bitwardenFrames"]:
                        record["mechanism"] = "menu-absent-after-cli-read"
                    else:
                        if menu["active"] is not True:
                            raise RuntimeError(
                                "native candidate requires the bound fixture tab already active"
                            )
                        record["stage"] = "native-focus"
                        before, focused = native_focus(
                            windows, hwnd, chrome.pid, "BitwardenFixture " + case
                        )
                        record["nativeFocus"] = {"before": before, "after": focused}
                        record["menuAfterNativeFocus"] = menu_frames(
                            bw_endpoint, fixture.url, bw_identity["id"]
                        )
                        if record["menuAfterNativeFocus"]["bitwardenFrames"]:
                            record["stage"] = "native-escape"
                            record["nativeMenuImage"] = native_menu_image(
                                windows,
                                hwnd,
                                chrome.pid,
                                case,
                                args.output / "native-menu.png",
                            )
                            if args.inspect_native_ui:
                                record["nativeUiInspection"] = inspect_native_fixture(
                                    hwnd, chrome.pid, case, fixture.url
                                )
                                record["menuAfterNativeInspection"] = menu_frames(
                                    bw_endpoint, fixture.url, bw_identity["id"]
                                )
                                if not record["menuAfterNativeInspection"][
                                    "bitwardenFrames"
                                ]:
                                    raise RuntimeError(
                                        "native inspection changed the menu before Escape; observation is not an uninstrumented recovery"
                                    )
                            record["nativeEscape"] = send_escape(hwnd)
                            record["mechanism"] = "native-input-attempted"
                        else:
                            record["mechanism"] = "menu-absent-after-native-focus"
                        record["menuAfterNative"] = wait_for(
                            lambda: menu_frames(
                                bw_endpoint, fixture.url, bw_identity["id"]
                            ),
                            lambda x: not x["bitwardenFrames"],
                            4,
                        )
                    record["accessAfter"] = invoke(
                        "access-after",
                        "eval",
                        "({title:document.title,fixture:window.fixtureState()})",
                    )
                    after_data = checked(record["accessAfter"])
                    if ((after_data.get("result") or {}).get("fixture") or {}).get(
                        "continuations"
                    ) != (1 if args.runtime_focus_recovery else 0):
                        raise RuntimeError(
                            "continuation count differs from the authorized runtime action"
                        )
                    record["stage"] = "continuation"
                    record["continuation"] = (
                        record["runtimeRecovery"]
                        if args.runtime_focus_recovery
                        else invoke("continuation", "click", "#continue")
                    )
                    checked(record["continuation"])
                    record["continuationRead"] = invoke(
                        "continuation-read",
                        "eval",
                        "({title:document.title,fixture:window.fixtureState()})",
                    )
                    after_data = checked(record["continuationRead"])
                    observed = after_data.get("result") or {}
                    if (
                        observed.get("title") != "BitwardenFixture " + case
                        or (observed.get("fixture") or {}).get("caseId") != case
                    ):
                        raise RuntimeError(
                            "recovery read answered from a different page"
                        )
                    if observed["fixture"].get("continuations") != 1:
                        raise RuntimeError(
                            "continuation was not performed exactly once"
                        )
                    if (
                        args.setup_mode == "background"
                        and record.get("nativeFocus")
                        and not args.runtime_focus_recovery
                    ):
                        record["stage"] = "restore-native-focus"
                        record["nativeFocusRestore"] = restore_fixture_cover(
                            windows,
                            hwnd,
                            chrome.pid,
                            case,
                            cover_hwnd,
                            record["nativeFocus"]["before"],
                        )
                        if not record["nativeFocusRestore"]["restored"]:
                            raise RuntimeError(
                                "native focus restore could not be verified"
                            )
                    if record.get("nativeFocusRestore"):
                        record["postRestoreRead"] = invoke(
                            "post-restore-read",
                            "eval",
                            "({title:document.title,fixture:window.fixtureState()})",
                        )
                        restored_data = (
                            checked(record["postRestoreRead"]).get("result") or {}
                        )
                        if (
                            restored_data.get("title") != "BitwardenFixture " + case
                            or (restored_data.get("fixture") or {}).get("caseId")
                            != case
                            or (restored_data.get("fixture") or {}).get("continuations")
                            != 1
                        ):
                            raise RuntimeError(
                                "restored foreground changed the verified page result"
                            )
                        observed = restored_data
                    record["finalMenu"] = menu_frames(
                        bw_endpoint, fixture.url, bw_identity["id"]
                    )
                    with fixture.lock:
                        record["receivedFixtureEvents"] = list(fixture.events)
                    record["fixtureEvents"] = observed["fixture"].get("events")
                    if not isinstance(record["fixtureEvents"], list):
                        raise RuntimeError("fixture history missing")
                    if record["finalMenu"]["bitwardenFrames"]:
                        raise RuntimeError("menu reopened after recovery read")
                    if any(
                        e["type"] == "submit"
                        or (
                            e["type"] == "visibilitychange" and e["trusted"] is not True
                        )
                        for e in record["fixtureEvents"]
                    ):
                        raise RuntimeError(
                            "unexpected submission or synthetic visibility event"
                        )
                    if record.get("nativeEscape"):
                        observed = native_escape_delivered(record["fixtureEvents"])
                        record["nativeEscape"]["deliveryObserved"] = observed
                        record["mechanism"] = (
                            "menu-absent-after-observed-native-escape"
                            if observed
                            else "menu-absent-after-native-input-attempt"
                        )
                        if not observed:
                            raise RuntimeError(
                                "menu closure observed but native Escape delivery is unattributed"
                            )
                    record["stage"] = "verified"
                    record["verdict"] = "PASS"
                except Exception as exc:
                    record["error"] = type(exc).__name__ + ": " + str(exc)[:1200]
                finally:
                    # Retain the independent receiver's observations even when
                    # the renderer/readback failed. Missing fetches are not
                    # evidence that input was absent.
                    with fixture.lock:
                        record["receivedFixtureEvents"] = list(fixture.events)
                    if close_cover:
                        try:
                            close_cover()
                            record["coverClosed"] = True
                        except Exception as exc:
                            record["coverCleanupError"] = str(exc)[:300]
                            record["verdict"] = "INCONCLUSIVE"
                    close_fixture_session(record, common, env, args.output)
                    try:
                        if chrome.poll() is not None:
                            raise RuntimeError(
                                "owned root exited before cleanup identity observation"
                            )
                        record["browserCleanup"] = terminate_owned(chrome)
                        if record["browserCleanup"]["verified"] is not True:
                            record["verdict"] = "INCONCLUSIVE"
                    except Exception as exc:
                        record["cleanupError"] = str(exc)[:300]
                        record["verdict"] = "INCONCLUSIVE"
                        if chrome.poll() is None:
                            chrome.kill()
                            chrome.wait(timeout=10)
            record["registryRestored"] = registry_restored(registry)
            if record["registryRestored"] is not True:
                raise RuntimeError("native-host registry restoration was not verified")
    except Exception as exc:
        record["error"] = type(exc).__name__ + ": " + str(exc)[:1200]
        record["verdict"] = "INCONCLUSIVE"
    finally:
        if fixture is not None:
            try:
                fixture.close()
            except Exception as exc:
                record["fixtureCleanupError"] = str(exc)[:300]
                record["verdict"] = "INCONCLUSIVE"
        (args.output / "summary.json").write_text(
            json.dumps(record, indent=2), encoding="utf-8"
        )
    print(
        json.dumps(
            {
                k: record.get(k)
                for k in ["verdict", "mechanism", "error", "cleanupError"]
            }
        )
    )
    return 0 if record["verdict"] == "PASS" else 2


if __name__ == "__main__":
    sys.exit(main())

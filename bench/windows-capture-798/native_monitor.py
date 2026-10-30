"""Read-only, exact-window WinEvent and state evidence for a private CI case."""
import ctypes
from ctypes import wintypes
import threading
import time

EVENTS = (0x0003, 0x0016, 0x0017, 0x800B)


class NativeWindowMonitor:
    def __init__(self, windows, hwnd, pid, case_id):
        self.windows, self.hwnd, self.pid, self.case_id = windows, hwnd, pid, case_id
        self.lock = threading.Lock()
        self.ready, self.stopped = threading.Event(), threading.Event()
        self.events, self.samples, self.errors, self.unhooked = [], [], [], []
        self.hooks = []
        self.start_tick = self.end_tick = None
        self.thread_id = None
        self.quit_requested = False
        self.pump_stopped_expected = False
        self.overflow = False
        self.callback_type = ctypes.WINFUNCTYPE(None, wintypes.HANDLE, wintypes.DWORD, wintypes.HWND,
            wintypes.LONG, wintypes.LONG, wintypes.DWORD, wintypes.DWORD)
        u, k = windows.u, windows.k
        k.GetCurrentThreadId.restype = wintypes.DWORD
        k.GetTickCount64.restype = ctypes.c_ulonglong
        u.SetWinEventHook.argtypes = [wintypes.DWORD, wintypes.DWORD, wintypes.HMODULE, self.callback_type,
                                     wintypes.DWORD, wintypes.DWORD, wintypes.DWORD]
        u.SetWinEventHook.restype = wintypes.HANDLE
        u.UnhookWinEvent.argtypes, u.UnhookWinEvent.restype = [wintypes.HANDLE], wintypes.BOOL
        u.PeekMessageW.argtypes = [ctypes.POINTER(wintypes.MSG), wintypes.HWND, wintypes.UINT, wintypes.UINT, wintypes.UINT]
        u.PeekMessageW.restype = wintypes.BOOL
        self.thread = threading.Thread(target=self._messages, daemon=True)
        self.poll = threading.Thread(target=self._poll, daemon=True)

    def _messages(self):
        u, k = self.windows.u, self.windows.k
        self.thread_id = k.GetCurrentThreadId()
        message = wintypes.MSG()
        u.PeekMessageW(ctypes.byref(message), None, 0, 0, 0)
        def callback(hook, event, hwnd, object_id, child_id, event_thread, event_tick):
            try:
                if hwnd != self.hwnd or event not in EVENTS or object_id != 0 or child_id != 0:
                    return
                owner = wintypes.DWORD()
                u.GetWindowThreadProcessId(hwnd, ctypes.byref(owner))
                if owner.value != self.pid:
                    return
                now = k.GetTickCount64()
                tick = now - ((now - event_tick) & 0xffffffff)
                with self.lock:
                    if len(self.events) >= 20000:
                        self.overflow = True
                        return
                    self.events.append({"event": event, "hwnd": int(hwnd), "pid": owner.value,
                        "tick": tick, "receivedTick": now, "monotonic": time.perf_counter()})
            except Exception as exc:
                with self.lock:
                    self.errors.append("callback: " + type(exc).__name__ + ": " + str(exc))
        self.callback = self.callback_type(callback)
        try:
            for event in EVENTS:
                hook = u.SetWinEventHook(event, event, None, self.callback, self.pid, 0, 0)
                if not hook:
                    raise ctypes.WinError(ctypes.get_last_error())
                self.hooks.append(hook)
            self.ready.set()
            while True:
                result = u.GetMessageW(ctypes.byref(message), None, 0, 0)
                if result == -1:
                    raise RuntimeError("GetMessageW returned -1")
                if result == 0:
                    self.pump_stopped_expected = self.quit_requested
                    if not self.quit_requested:
                        raise RuntimeError("message pump quit before requested shutdown")
                    break
                u.TranslateMessage(ctypes.byref(message))
                u.DispatchMessageW(ctypes.byref(message))
        except Exception as exc:
            with self.lock:
                self.errors.append(type(exc).__name__ + ": " + str(exc))
            self.ready.set()
        finally:
            for hook in self.hooks:
                self.unhooked.append(bool(u.UnhookWinEvent(hook)))

    def _poll(self):
        while not self.stopped.wait(0.02):
            try:
                window = self.windows.state(self.hwnd)
                identity = window["hwnd"] == self.hwnd and window["pid"] == self.pid and "CaptureProbe " + self.case_id in window["title"]
                value = {"tick": self.windows.k.GetTickCount64(), "monotonic": time.perf_counter(),
                         "window": window if identity else None, "identityValid": identity}
                with self.lock:
                    self.samples.append(value)
                    if len(self.samples) > 20000:
                        self.errors.append("state trajectory exceeded its bound")
                        return
            except Exception as exc:
                with self.lock:
                    self.errors.append(type(exc).__name__ + ": " + str(exc))
                return

    def start(self):
        self.thread.start()
        if not self.ready.wait(5) or len(self.hooks) != len(EVENTS) or self.errors:
            raise RuntimeError("native event hooks were not confirmed")
        self.poll.start()

    def calibrated(self):
        with self.lock:
            return any(event["event"] == 0x0016 for event in self.events)

    def begin_measurement(self):
        if not self.calibrated():
            raise RuntimeError("owned minimize event did not calibrate the native hooks")
        self.start_tick = self.windows.k.GetTickCount64()

    def finish_measurement(self):
        self.end_tick = self.windows.k.GetTickCount64()
        # A read-only drain interval permits queued out-of-context notifications
        # to arrive. Event timestamps, not receipt phase, define the interval.
        self.stopped.wait(0.1)
        self.stopped.set()
        if self.poll.is_alive():
            self.poll.join(5)
        if self.thread_id is not None:
            self.quit_requested = True
            if not self.windows.u.PostThreadMessageW(self.thread_id, 0x0012, 0, 0):
                self.errors.append("PostThreadMessageW shutdown was not acknowledged")
        self.thread.join(5)
        if self.thread.is_alive() or self.poll.is_alive():
            self.errors.append("native monitor did not stop")
        with self.lock:
            return {"calibrated": self.calibrated_unlocked(), "installed": len(self.hooks),
                "unhooked": list(self.unhooked), "startTick": self.start_tick, "endTick": self.end_tick,
                "events": list(self.events), "samples": list(self.samples), "errors": list(self.errors),
                "pumpStoppedExpected": self.pump_stopped_expected, "overflow": self.overflow,
                "drainMs": 100, "samplingIntervalMs": 20,
                "scope": "SetWinEventHook uses nonzero Chrome PID and exact HWND callback filter; 20ms trajectory reads only that window plus foreground handle ID"}

    def calibrated_unlocked(self):
        return any(event["event"] == 0x0016 for event in self.events)

"""Read-only identity/exit proof for CI-owned Windows processes."""
import ctypes
from ctypes import wintypes
import re
import time


def creation_millis(process):
    match = re.fullmatch(r"/Date\((-?\d+)(?:[+-]\d{4})?\)/", str(process.get("CreationDate")))
    return int(match[1]) if match else None


class OwnedProcessHandles:
    def __init__(self, snapshot):
        self.kernel = ctypes.WinDLL("kernel32", use_last_error=True)
        self.kernel.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
        self.kernel.OpenProcess.restype = wintypes.HANDLE
        self.kernel.GetProcessTimes.argtypes = [wintypes.HANDLE] + [ctypes.POINTER(wintypes.FILETIME)] * 4
        self.kernel.GetProcessTimes.restype = wintypes.BOOL
        self.kernel.WaitForSingleObject.argtypes = [wintypes.HANDLE, wintypes.DWORD]
        self.kernel.WaitForSingleObject.restype = wintypes.DWORD
        self.kernel.CloseHandle.argtypes = [wintypes.HANDLE]
        self.kernel.CloseHandle.restype = wintypes.BOOL
        self.rows = []
        self.handles = []
        for process in snapshot:
            row = {"pid": process["ProcessId"], "parentPid": process["ParentProcessId"],
                "name": process["Name"], "bindingConfirmed": False, "signaled": False}
            self.rows.append(row)
            handle = self.kernel.OpenProcess(0x00100000 | 0x1000, False, row["pid"])
            if not handle:
                row["bindError"] = ctypes.get_last_error()
                self.handles.append(None)
                continue
            self.handles.append(handle)
            try:
                created, _, _, _ = self.times(handle)
                expected = creation_millis(process)
                actual = created // 10000 - 11644473600000
                row.update(expectedCreatedMs=expected, actualCreatedMs=actual,
                    bindingConfirmed=expected is not None and actual == expected)
            except Exception as error:
                row["bindError"] = str(error)

    def times(self, handle):
        values = [wintypes.FILETIME() for _ in range(4)]
        if not self.kernel.GetProcessTimes(handle, *(ctypes.byref(v) for v in values)):
            raise ctypes.WinError(ctypes.get_last_error())
        return tuple((v.dwHighDateTime << 32) | v.dwLowDateTime for v in values)

    def observe(self):
        deadline = time.monotonic() + 2
        for row, handle in zip(self.rows, self.handles):
            if not handle or not row["bindingConfirmed"]:
                continue
            wait = self.kernel.WaitForSingleObject(handle, max(0, int((deadline - time.monotonic()) * 1000)))
            row["waitCode"] = wait
            if wait == 0:
                try:
                    _, exited, _, _ = self.times(handle)
                    row.update(exitFileTime=exited, signaled=exited > 0)
                except Exception as error:
                    row["observeError"] = str(error)
            elif wait != 258:
                row["observeError"] = ctypes.get_last_error()
        return self.rows

    def close(self):
        for row, handle in zip(self.rows, self.handles):
            if handle:
                row["handleClosed"] = bool(self.kernel.CloseHandle(handle))
        self.handles = []


def cleanup_acknowledged(taskkill, owned, handles, parent_exit, post_tree):
    """Accept only OS acknowledgment or known, retained targets proven exited."""
    if type(parent_exit) is not int or post_tree != [] or not owned or len(handles) != len(owned):
        return False
    expected = {p["ProcessId"]: p for p in owned}
    if len(expected) != len(owned) or {h.get("pid") for h in handles} != set(expected):
        return False
    if any(h.get("bindingConfirmed") is not True or h.get("signaled") is not True
           or h.get("handleClosed") is not True or h.get("parentPid") != expected[h["pid"]]["ParentProcessId"]
           or type(h.get("expectedCreatedMs")) is not int
           or h["expectedCreatedMs"] != creation_millis(expected[h["pid"]])
           or h.get("actualCreatedMs") != h["expectedCreatedMs"]
           or type(h.get("waitCode")) is not int or h["waitCode"] != 0
           or type(h.get("exitFileTime")) is not int or h["exitFileTime"] <= 0
           or h.get("observeError") is not None for h in handles):
        return False
    code, stderr = taskkill.get("exit"), taskkill.get("stderr")
    if type(code) is not int or not isinstance(stderr, str):
        return False
    if code == 0:
        return not stderr.strip()
    if code != 255:
        return False
    pattern = re.compile(r"ERROR: The process with PID (\d+) \(child process of PID (\d+)\) could not be terminated\.\r?\nReason: There is no running instance of the task\.\s*")
    errors = pattern.findall(stderr)
    return bool(errors) and not pattern.sub("", stderr).strip() and all(
        int(pid) in expected and expected[int(pid)]["ParentProcessId"] == int(parent) for pid, parent in errors)

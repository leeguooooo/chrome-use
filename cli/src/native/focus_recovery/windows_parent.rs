//! Bind native UI authority to the Chrome process that launched this host.
//! A caller-supplied pid, window title, or extension-origin argv is insufficient.

use std::mem::size_of;
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcessId, GetExitCodeProcess, OpenProcess, QueryFullProcessImageNameW,
    PROCESS_QUERY_LIMITED_INFORMATION,
};

struct Handle(HANDLE);
impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: constructed only from a successful handle-returning API;
        // ownership is unique and this is its only close.
        unsafe {
            CloseHandle(self.0);
        }
    }
}

/// A held process handle prevents a reused pid from becoming native authority.
/// It also lets every later native action reject a Chrome process that exited.
pub(super) struct ChromeParent {
    process: Handle,
    pid: u32,
}

fn parent_pid() -> Result<u32, &'static str> {
    // SAFETY: snapshot acquisition has no pointer parameters.
    let raw = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if raw == INVALID_HANDLE_VALUE || raw == 0 {
        return Err("could not inspect native host ancestry");
    }
    let snapshot = Handle(raw);
    // SAFETY: PROCESSENTRY32W is a C record containing integers and an inline
    // UTF-16 array. Zero initialization is valid; dwSize is required by Win32.
    let mut entry: PROCESSENTRY32W = unsafe { std::mem::zeroed() };
    entry.dwSize = size_of::<PROCESSENTRY32W>() as u32;
    // SAFETY: both handles and the sized, writable record remain live.
    let mut available = unsafe { Process32FirstW(snapshot.0, &mut entry) } != 0;
    // SAFETY: GetCurrentProcessId takes no arguments.
    let current = unsafe { GetCurrentProcessId() };
    while available {
        if entry.th32ProcessID == current {
            return (entry.th32ParentProcessID != 0)
                .then_some(entry.th32ParentProcessID)
                .ok_or("native host has no identifiable Chrome parent");
        }
        // SAFETY: same live snapshot and writable record as above.
        available = unsafe { Process32NextW(snapshot.0, &mut entry) } != 0;
    }
    Err("native host process was absent from its ancestry snapshot")
}

impl ChromeParent {
    /// Called only in the Chrome native-messaging host. Standalone CLI and test
    /// wrappers do not gain desktop authority by providing a pid or origin.
    pub(super) fn attest() -> Result<Self, &'static str> {
        let pid = parent_pid()?;
        // SAFETY: requests read-only information about the observed OS parent.
        let raw = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if raw == 0 {
            return Err("could not bind the native host parent");
        }
        let process = Handle(raw);
        let mut name = vec![0u16; 32768];
        let mut length = name.len() as u32;
        // SAFETY: the buffer is writable for length UTF-16 units, and the held
        // process handle has PROCESS_QUERY_LIMITED_INFORMATION access.
        if unsafe { QueryFullProcessImageNameW(process.0, 0, name.as_mut_ptr(), &mut length) } == 0
        {
            return Err("could not identify the native host parent executable");
        }
        let path = String::from_utf16(&name[..length as usize])
            .map_err(|_| "invalid native host parent executable path")?;
        let file = std::path::Path::new(&path)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        if !matches!(
            file.as_str(),
            "chrome.exe"
                | "chromium.exe"
                | "msedge.exe"
                | "brave.exe"
                | "vivaldi.exe"
                | "opera.exe"
                | "helium.exe"
        ) {
            return Err("native focus recovery requires a Chrome-launched native host");
        }
        let parent = Self { process, pid };
        parent.require_alive()?;
        Ok(parent)
    }

    pub(super) fn require_alive(&self) -> Result<u32, &'static str> {
        let mut exit = 0;
        // SAFETY: this process handle stays owned until self is dropped; exit
        // points to a writable DWORD. STILL_ACTIVE is the Win32 value 259.
        if unsafe { GetExitCodeProcess(self.process.0, &mut exit) } == 0 || exit != 259 {
            return Err("the attested Chrome parent exited or cannot be verified");
        }
        Ok(self.pid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standalone_test_process_cannot_claim_chrome_parent_authority() {
        // Native integration fixtures launch from the test runner, not Chrome.
        assert!(ChromeParent::attest().is_err());
    }
}

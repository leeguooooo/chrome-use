//! Bind native UI authority to the Chrome process that launched this host.
//! A caller-supplied pid, window title, or extension-origin argv is insufficient.

use std::mem::size_of;
use windows_sys::Win32::Foundation::{
    CloseHandle, FILETIME, HANDLE, INVALID_HANDLE_VALUE, WAIT_TIMEOUT,
};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, GetCurrentProcessId, GetProcessTimes, OpenProcess,
    QueryFullProcessImageNameW, WaitForSingleObject, PROCESS_QUERY_LIMITED_INFORMATION,
    PROCESS_SYNCHRONIZE,
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
    _launcher: Option<Handle>,
}

fn parent_pid(child_pid: u32) -> Result<u32, &'static str> {
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
    while available {
        if entry.th32ProcessID == child_pid {
            return (entry.th32ParentProcessID != 0)
                .then_some(entry.th32ParentProcessID)
                .ok_or("native host has no identifiable Chrome parent");
        }
        // SAFETY: same live snapshot and writable record as above.
        available = unsafe { Process32NextW(snapshot.0, &mut entry) } != 0;
    }
    Err("native host process was absent from its ancestry snapshot")
}

fn created(handle: HANDLE) -> Result<u64, &'static str> {
    let zero = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let (mut creation, mut exit, mut kernel, mut user) = (zero, zero, zero, zero);
    // SAFETY: all four FILETIME output records are distinct, writable and live.
    if unsafe { GetProcessTimes(handle, &mut creation, &mut exit, &mut kernel, &mut user) } == 0 {
        return Err("could not verify native host ancestry creation times");
    }
    Ok(((creation.dwHighDateTime as u64) << 32) | creation.dwLowDateTime as u64)
}

fn held_process(pid: u32, child_created: u64) -> Result<(Handle, u64, String), &'static str> {
    // SAFETY: requests only read/synchronization access to an observed parent.
    let raw = unsafe {
        OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
            0,
            pid,
        )
    };
    if raw == 0 {
        return Err("could not bind the native host parent");
    }
    let process = Handle(raw);
    let birth = created(process.0)?;
    if birth > child_created {
        return Err("the native host parent pid was reused");
    }
    let mut name = vec![0u16; 32768];
    let mut length = name.len() as u32;
    // SAFETY: the owned process handle has query rights and the buffer is live.
    if unsafe { QueryFullProcessImageNameW(process.0, 0, name.as_mut_ptr(), &mut length) } == 0 {
        return Err("could not identify the native host parent executable");
    }
    let path = String::from_utf16(&name[..length as usize])
        .map_err(|_| "invalid parent executable path")?;
    Ok((process, birth, path))
}

fn executable_name(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
}

fn system_command_intermediary(path: &str) -> bool {
    let mut directory = vec![0u16; 32768];
    // SAFETY: the UTF-16 buffer is writable for the supplied length.
    let length = unsafe {
        windows_sys::Win32::System::SystemInformation::GetWindowsDirectoryW(
            directory.as_mut_ptr(),
            directory.len() as u32,
        )
    } as usize;
    if length == 0 || length >= directory.len() {
        return false;
    }
    let directory = std::path::PathBuf::from(String::from_utf16_lossy(&directory[..length]));
    ["System32", "SysWOW64"].iter().any(|folder| {
        directory
            .join(folder)
            .join("cmd.exe")
            .to_string_lossy()
            .eq_ignore_ascii_case(path)
    })
}

impl ChromeParent {
    #[cfg(all(target_arch = "x86_64", feature = "windows-build-identity-prototype"))]
    pub(super) fn from_verified_tool(
        host: &crate::windows_build_identity::MatchingLoadedGeneration,
    ) -> Result<ChromeFromVerifiedTool<'_>, &'static str> {
        let (pid, birth) = host
            .kernel_identity_after_revalidation()
            .map_err(|_| "the matched tool peer exited or changed")?;
        let bound = ChromeFromVerifiedTool {
            parent: Self::from_host_identity(pid, birth)?,
            host,
        };
        bound.require_alive()?;
        Ok(bound)
    }

    /// Attest either Chrome's direct launch or its documented Windows system
    /// cmd.exe intermediary. Do not search arbitrary shell/agent ancestry.
    pub(super) fn attest() -> Result<Self, &'static str> {
        // SAFETY: these process identity APIs take no pointer arguments.
        let host_pid = unsafe { GetCurrentProcessId() };
        let host_created = created(unsafe { GetCurrentProcess() })?;
        Self::from_host_identity(host_pid, host_created)
    }

    fn from_host_identity(host_pid: u32, host_created: u64) -> Result<Self, &'static str> {
        let mut pid = parent_pid(host_pid)?;
        let (mut process, birth, mut path) = held_process(pid, host_created)?;
        let mut launcher = None;
        if executable_name(&path) == "cmd.exe" {
            if !system_command_intermediary(&path) {
                return Err(
                    "native host command intermediary is not the Windows system executable",
                );
            }
            let chrome_pid = parent_pid(pid)?;
            let (chrome, _, chrome_path) = held_process(chrome_pid, birth)?;
            launcher = Some(process);
            process = chrome;
            path = chrome_path;
            pid = chrome_pid;
        }
        if !matches!(
            executable_name(&path).as_str(),
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
        let parent = Self {
            process,
            pid,
            _launcher: launcher,
        };
        parent.require_alive()?;
        Ok(parent)
    }

    pub(super) fn require_alive(&self) -> Result<u32, &'static str> {
        // SAFETY: the held handle has PROCESS_SYNCHRONIZE access. A zero-time
        // wait observes liveness without waiting or interpreting an exit code.
        if unsafe { WaitForSingleObject(self.process.0, 0) } != WAIT_TIMEOUT {
            return Err("the attested Chrome parent exited or cannot be verified");
        }
        Ok(self.pid)
    }
}

/// Keeps the matched native tool proof borrowed throughout the grant decision;
/// observing only its parent PID must never outlive that source verification.
#[cfg(all(target_arch = "x86_64", feature = "windows-build-identity-prototype"))]
pub(super) struct ChromeFromVerifiedTool<'a> {
    parent: ChromeParent,
    host: &'a crate::windows_build_identity::MatchingLoadedGeneration,
}
#[cfg(all(target_arch = "x86_64", feature = "windows-build-identity-prototype"))]
impl ChromeFromVerifiedTool<'_> {
    pub(super) fn require_alive(&self) -> Result<u32, &'static str> {
        self.host
            .revalidate()
            .map_err(|_| "the matched native tool peer changed")?;
        crate::windows_build_identity::require_current_principal(self.parent.process.0)
            .map_err(|_| "the Chrome parent does not match the caller user, logon and session")?;
        self.parent.require_alive()
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

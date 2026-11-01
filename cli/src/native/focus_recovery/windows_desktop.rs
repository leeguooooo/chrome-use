//! A native host's single Escape attempt and conservative foreground restore.
//! Chrome tab ownership is checked by the extension; this layer additionally
//! binds the real foreground window and native documents to the host's parent.

use super::windows_parent::ChromeParent;
use serde_json::{json, Value};
use std::{mem::size_of, sync::Arc, time::Instant};
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, WAIT_TIMEOUT};
use windows_sys::Win32::System::Threading::{
    OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, GetLastInputInfo, MapVirtualKeyW, SendInput, INPUT, INPUT_0, INPUT_KEYBOARD,
    KEYBDINPUT, KEYEVENTF_KEYUP, LASTINPUTINFO, MAPVK_VK_TO_VSC, VK_ESCAPE,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetClassNameW, GetForegroundWindow, GetWindowTextW, GetWindowThreadProcessId, IsIconic,
    IsWindow, IsWindowVisible, SetForegroundWindow,
};

#[derive(Clone, PartialEq, Eq)]
struct Window {
    hwnd: isize,
    pid: u32,
    title: Vec<u16>,
    class: Vec<u16>,
}

impl Window {
    fn read(hwnd: isize) -> Result<Self, &'static str> {
        // SAFETY: these read-only Win32 calls accept arbitrary HWND values and
        // report invalid/closed windows without dereferencing caller memory.
        if hwnd == 0 || unsafe { IsWindow(hwnd) } == 0 {
            return Err("native window is unavailable");
        }
        let mut pid = 0;
        // SAFETY: pid is a live writable DWORD.
        if unsafe { GetWindowThreadProcessId(hwnd, &mut pid) } == 0 || pid == 0 {
            return Err("native window owner is unavailable");
        }
        let mut title = vec![0u16; 4097];
        let mut class = vec![0u16; 256];
        // SAFETY: both UTF-16 buffers are writable for the supplied lengths.
        let title_len = unsafe { GetWindowTextW(hwnd, title.as_mut_ptr(), title.len() as i32) };
        let class_len = unsafe { GetClassNameW(hwnd, class.as_mut_ptr(), class.len() as i32) };
        if title_len >= 4096 || class_len <= 0 || class_len >= 255 {
            return Err("native window metadata is incomplete");
        }
        title.truncate(title_len as usize);
        class.truncate(class_len as usize);
        Ok(Self {
            hwnd,
            pid,
            title,
            class,
        })
    }
    fn foreground() -> Result<Self, &'static str> {
        // SAFETY: GetForegroundWindow has no arguments or side effects.
        Self::read(unsafe { GetForegroundWindow() })
    }
    fn unchanged_foreground(&self) -> bool {
        Self::foreground().is_ok_and(|current| current == *self)
    }
}

fn input_stamp() -> Result<u32, &'static str> {
    let mut info = LASTINPUTINFO {
        cbSize: size_of::<LASTINPUTINFO>() as u32,
        dwTime: 0,
    };
    // SAFETY: info is a live, correctly sized writable record.
    if unsafe { GetLastInputInfo(&mut info) } == 0 {
        return Err("native input state unavailable");
    }
    Ok(info.dwTime)
}

fn keys_or_buttons_held() -> bool {
    // Observe the high bit only: toggle state is irrelevant. Do not interrupt
    // typing, composition, a shortcut, or a mouse drag already in progress.
    (1..=254).any(|key| {
        // SAFETY: GetAsyncKeyState reads a virtual-key state.
        (unsafe { GetAsyncKeyState(key) }) < 0
    })
}

#[derive(Debug, PartialEq, Eq)]
enum RestoreObservation {
    Original,
    Pending,
    Changed,
}

/// None means an observed null foreground during activation, not a failed read.
fn restore_observation(
    current: Option<&Window>,
    original: &Window,
    target: &Window,
    unchanged: bool,
) -> RestoreObservation {
    if !unchanged {
        return RestoreObservation::Changed;
    }
    match current {
        Some(window) if window == original => RestoreObservation::Original,
        None => RestoreObservation::Pending,
        Some(window)
            if window.hwnd == target.hwnd
                && window.pid == target.pid
                && window.class == target.class =>
        {
            RestoreObservation::Pending
        }
        Some(_) => RestoreObservation::Changed,
    }
}

struct OriginalWindow {
    window: Window,
    process: HANDLE,
}
impl Drop for OriginalWindow {
    fn drop(&mut self) {
        // SAFETY: this uniquely owned process handle was opened successfully.
        unsafe {
            CloseHandle(self.process);
        }
    }
}
impl OriginalWindow {
    fn capture() -> Result<Self, &'static str> {
        let window = Window::foreground()?;
        // SAFETY: requests only synchronization access to the observed owner.
        let process = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, window.pid) };
        if process == 0 {
            return Err("original foreground owner could not be held");
        }
        let original = Self { window, process };
        if !original.still_same() {
            return Err("original foreground identity changed");
        }
        Ok(original)
    }
    fn still_same(&self) -> bool {
        // SAFETY: process remains owned until Drop; this is a zero-time query.
        (unsafe { WaitForSingleObject(self.process, 0) }) == WAIT_TIMEOUT
            && Window::read(self.window.hwnd).is_ok_and(|window| window == self.window)
    }
}

pub(super) struct DesktopLease {
    chrome: Arc<ChromeParent>,
    fence: Arc<super::authority::DispatchFence>,
    original: OriginalWindow,
    target: Option<Window>,
    inspection_target: Option<Window>,
    input_after: Option<u32>,
    input_before: u32,
    inspection_started: bool,
    armed_until: Option<Instant>,
    attempted: bool,
}

impl DesktopLease {
    /// Capture before the extension focuses its exact owned Chrome window.
    pub(super) fn prepare(
        fence: Arc<super::authority::DispatchFence>,
    ) -> Result<Self, &'static str> {
        let chrome = Arc::new(ChromeParent::attest()?);
        if keys_or_buttons_held() {
            return Err("User input is held; native recovery declined before focus");
        }
        let input_before = input_stamp()?;
        let original = OriginalWindow::capture()?;
        if input_stamp()? != input_before {
            return Err("User input changed before native preparation completed");
        }
        Ok(Self {
            chrome,
            fence,
            original,
            target: None,
            inspection_target: None,
            input_after: None,
            input_before,
            inspection_started: false,
            armed_until: None,
            attempted: false,
        })
    }

    pub(super) fn inspection_started(&self) -> bool {
        self.inspection_started
    }

    /// Chrome's focus request may return before the native foreground changes.
    /// Observe that one request; never activate again or follow another window.
    async fn focused_chrome(&self, pid: u32) -> Result<Window, String> {
        let deadline = Instant::now() + std::time::Duration::from_millis(500);
        loop {
            if !self.fence.available() {
                return Err("native focus authority was revoked before inspection".into());
            }
            self.chrome.require_alive()?;
            if input_stamp()? != self.input_before || keys_or_buttons_held() {
                return Err(
                    "User input changed while focus was settling; no Escape was sent".into(),
                );
            }
            // SAFETY: a read-only foreground query. A null result is possible
            // during an activation transition and conveys no window authority.
            let hwnd = unsafe { GetForegroundWindow() };
            if hwnd != 0 {
                let window = Window::read(hwnd)?;
                if window.pid == pid {
                    return Ok(window);
                }
                if window != self.original.window || !self.original.still_same() {
                    return Err("Foreground changed to another window; no Escape was sent".into());
                }
            }
            if Instant::now() >= deadline {
                return Err("Foreground did not become the attested Chrome within 500ms; no Escape was sent".into());
            }
            let pause = crate::native::humanize::key_dwell(
                crate::native::humanize::HumanizeLevel::Fast,
                crate::native::humanize::next_seed(),
            );
            tokio::time::sleep(pause.min(deadline.saturating_duration_since(Instant::now()))).await;
        }
    }

    /// The permit is consumed even when inspection/input acknowledgement fails.
    /// No original browser operation is dispatched or repeated here.
    pub(super) async fn inspect(
        &mut self,
        page_url: &str,
        deadline: Instant,
    ) -> Result<Value, String> {
        if !self.fence.available() {
            return Err("native inspection authority was revoked or expired".into());
        }
        if self.inspection_started {
            return Err("native inspection was already attempted".into());
        }
        self.inspection_started = true;
        let pid = self.chrome.require_alive()?;
        let target = self.focused_chrome(pid).await?;
        if target.pid != pid
            || !String::from_utf16_lossy(&target.class).starts_with("Chrome_WidgetWin_")
            // SAFETY: target is an observed HWND; these are read-only queries.
            || unsafe { IsWindowVisible(target.hwnd) } == 0
            || unsafe { IsIconic(target.hwnd) } != 0
            || keys_or_buttons_held()
        {
            return Err("Attested Chrome foreground is not a visible, normal Chrome window or input is held; no Escape was sent".into());
        }
        let stamp = input_stamp()?;
        if stamp != self.input_before {
            return Err("User input changed after preparation; no Escape was sent".into());
        }
        let _proof = super::windows::inspect(target.hwnd as u64, pid, page_url).await?;
        let settle = crate::native::humanize::settle_before_press(
            crate::native::humanize::active_level(),
            crate::native::humanize::next_seed(),
        );
        tokio::time::sleep(settle).await;
        self.chrome.require_alive()?;
        if !target.unchanged_foreground() || input_stamp()? != stamp || keys_or_buttons_held() {
            return Err("native target or user input changed before Escape; no key sent".into());
        }
        // Retain a successful read-only binding for explicitly authorized
        // rollback even when cancellation won before a key could be armed.
        self.inspection_target = Some(target.clone());
        self.input_after = Some(stamp);
        if !self.fence.available() || Instant::now() >= deadline {
            return Err("native authority was revoked or expired; no key sent".into());
        }
        let until = deadline.min(Instant::now() + std::time::Duration::from_secs(2));
        if !self.fence.renew(until) {
            return Err("native authority was revoked during inspection".into());
        }
        self.armed_until = Some(until);
        Ok(json!({"verified":true}))
    }

    /// Called only after the extension rechecks the exact active tab following
    /// the asynchronous native inspection. Consume the arm before any input.
    pub(super) async fn escape(&mut self, lease_deadline: Instant) -> Result<Value, String> {
        if self.attempted {
            return Err("native Escape was already attempted".into());
        }
        self.attempted = true;
        let deadline = self
            .armed_until
            .take()
            .filter(|deadline| Instant::now() < *deadline)
            .ok_or("native inspection arm expired; no key was sent")?
            .min(lease_deadline);
        let target = self
            .inspection_target
            .clone()
            .ok_or("native target was not inspected")?;
        let stamp = self
            .input_after
            .ok_or("native input state was not inspected")?;
        let pid = self.chrome.require_alive()?;
        // Keep key release in one blocking task: cancelling an async request
        // during the humanized dwell must not leave Escape logically held.
        let chrome = self.chrome.clone();
        let fence = self.fence.clone();
        let native_hwnd = target.hwnd as u64;
        let admitted_target = target.clone();
        let outcome = tokio::task::spawn_blocking(move || {
            chrome.require_alive()?;
            if Instant::now() >= deadline
                || !target.unchanged_foreground()
                || input_stamp()? != stamp
                || keys_or_buttons_held()
            {
                return Err("native target changed before key admission");
            }
            // Bitwarden checks KeyboardEvent.code, not only key. Preserve the
            // hardware scan code as well as the logical VK_ESCAPE value.
            // SAFETY: this maps one fixed virtual key and performs no input.
            let scan = unsafe { MapVirtualKeyW(VK_ESCAPE as u32, MAPVK_VK_TO_VSC) };
            if scan == 0 || scan > u16::MAX as u32 {
                return Err("native Escape scan code is unavailable");
            }
            let _dispatch = fence.begin()?;
            let input = |flags| INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 {
                    ki: KEYBDINPUT {
                        wVk: VK_ESCAPE,
                        wScan: scan as u16,
                        dwFlags: flags,
                        time: 0,
                        dwExtraInfo: 0,
                    },
                },
            };
            // SAFETY: each INPUT uses the SDK ABI and remains live for the call.
            let down = unsafe { SendInput(1, &input(0), size_of::<INPUT>() as i32) };
            if down == 0 {
                return Ok((0, 0, input_stamp()?));
            }
            std::thread::sleep(crate::native::humanize::key_dwell(
                crate::native::humanize::active_level(),
                crate::native::humanize::next_seed(),
            ));
            // Release exactly once even if foreground changed during the dwell.
            // It releases our admitted key; it never sends a second key-down.
            let up = unsafe { SendInput(1, &input(KEYEVENTF_KEYUP), size_of::<INPUT>() as i32) };
            Ok((down, up, input_stamp()?))
        })
        .await
        .map_err(|_| "native key outcome is unconfirmed; it was not repeated")??;
        self.target = Some(admitted_target);
        self.input_after = Some(outcome.2);
        Ok(
            json!({"keyDownAdmitted":outcome.0, "keyUpAdmitted":outcome.1,
            "delivery":"unverified", "replayed":false,
            "beforeKeyDown":{"hwnd":native_hwnd,"pid":pid}}),
        )
    }

    /// Restore only while the exact original window and our foreground binding
    /// survive, and no new OS input occurred. Never chase a user's new focus.
    pub(super) fn finish(&mut self, allow_inspected: bool) -> Value {
        let target = self.target.take().or_else(|| {
            if allow_inspected {
                self.inspection_target.take()
            } else {
                None
            }
        });
        let Some(target) = target else {
            if self.original.still_same() && self.original.window.unchanged_foreground() {
                return json!({"attempted":false,"restored":true,"reason":"original-foreground-unchanged"});
            }
            return json!({"attempted":false,"restored":false,"reason":"no-verified-native-target"});
        };
        let same_target = Window::foreground().is_ok_and(|window| {
            window.hwnd == target.hwnd && window.pid == target.pid && window.class == target.class
        });
        let chrome_alive = self.chrome.require_alive().is_ok();
        if same_target && chrome_alive && self.original.window.hwnd == target.hwnd {
            return json!({"attempted":false,"restored":true,"reason":"already-original"});
        }
        if !self.original.still_same()
            || !same_target
            || !chrome_alive
            || input_stamp().ok() != self.input_after
        {
            return json!({"attempted":false,"restored":false,"reason":"foreground-or-input-changed"});
        }
        // SAFETY: both window identities and held process handles were checked.
        let admitted = unsafe { SetForegroundWindow(self.original.window.hwnd) } != 0;
        let deadline = Instant::now() + std::time::Duration::from_millis(250);
        let mut restored = false;
        loop {
            // Observation only: a delayed foreground transition is not a
            // reason to activate again or to chase a user's intervening input.
            let unchanged = input_stamp().ok() == self.input_after && self.original.still_same();
            // SAFETY: read-only query. A null result grants no window authority.
            let hwnd = unsafe { GetForegroundWindow() };
            let current = if hwnd == 0 {
                None
            } else {
                match Window::read(hwnd) {
                    Ok(window) => Some(window),
                    Err(_) => break,
                }
            };
            match restore_observation(current.as_ref(), &self.original.window, &target, unchanged) {
                RestoreObservation::Original => {
                    restored = true;
                    break;
                }
                RestoreObservation::Changed => break,
                RestoreObservation::Pending => {}
            }
            if Instant::now() >= deadline {
                break;
            }
            let pause = crate::native::humanize::key_dwell(
                crate::native::humanize::HumanizeLevel::Fast,
                crate::native::humanize::next_seed(),
            );
            std::thread::sleep(pause.min(deadline.saturating_duration_since(Instant::now())));
        }
        json!({"attempted":true,"admitted":admitted,"restored":restored})
    }
}

impl DesktopLease {
    pub(super) fn abandon_restore(&mut self) -> Value {
        self.target = None;
        self.inspection_target = None;
        json!({"attempted":false,"restored":false,"reason":"extension-target-binding-changed"})
    }
}

impl Drop for DesktopLease {
    fn drop(&mut self) {
        // Host disconnect/cancellation must not silently keep a verified window
        // in front. The same no-interference checks apply to this fallback.
        let _ = self.finish(false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(hwnd: isize, pid: u32) -> Window {
        Window {
            hwnd,
            pid,
            title: vec![1],
            class: vec![2],
        }
    }

    #[test]
    fn restore_accepts_the_original_when_it_arrives_between_observations() {
        let original = window(1, 10);
        let target = window(2, 20);
        assert_eq!(
            restore_observation(Some(&target), &original, &target, true),
            RestoreObservation::Pending
        );
        assert_eq!(
            restore_observation(None, &original, &target, true),
            RestoreObservation::Pending
        );
        assert_eq!(
            restore_observation(Some(&original), &original, &target, true),
            RestoreObservation::Original
        );
    }

    #[test]
    fn restore_stops_on_changed_input_or_a_different_window() {
        let original = window(1, 10);
        let target = window(2, 20);
        let third_window = window(3, 20);
        let different_process = window(2, 30);
        assert_eq!(
            restore_observation(Some(&third_window), &original, &target, true),
            RestoreObservation::Changed
        );
        assert_eq!(
            restore_observation(Some(&different_process), &original, &target, true),
            RestoreObservation::Changed
        );
        assert_eq!(
            restore_observation(Some(&original), &original, &target, false),
            RestoreObservation::Changed
        );
        assert_eq!(
            restore_observation(None, &original, &target, false),
            RestoreObservation::Changed
        );
    }

    #[test]
    fn a_page_title_change_is_not_a_new_native_target() {
        let original = window(1, 10);
        let target = window(2, 20);
        let mut updated = target.clone();
        updated.title = vec![3];
        assert_eq!(
            restore_observation(Some(&updated), &original, &target, true),
            RestoreObservation::Pending
        );
        updated.class = vec![4];
        assert_eq!(
            restore_observation(Some(&updated), &original, &target, true),
            RestoreObservation::Changed
        );
    }
}

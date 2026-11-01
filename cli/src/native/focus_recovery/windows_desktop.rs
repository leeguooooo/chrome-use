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
    GetAsyncKeyState, GetLastInputInfo, SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT,
    KEYEVENTF_KEYUP, LASTINPUTINFO, VK_ESCAPE,
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
    // Mouse buttons, modifiers, Windows keys and Escape. Do not interfere with
    // a drag, shortcut or an Escape key the user already holds.
    [
        1,
        2,
        4,
        5,
        6,
        0x10,
        0x11,
        0x12,
        0x5b,
        0x5c,
        VK_ESCAPE as i32,
    ]
    .into_iter()
    .any(|key| {
        // SAFETY: GetAsyncKeyState reads a virtual-key state.
        (unsafe { GetAsyncKeyState(key) }) < 0
    })
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
    original: OriginalWindow,
    target: Option<Window>,
    input_after: Option<u32>,
    attempted: bool,
}

impl DesktopLease {
    /// Capture before the extension focuses its exact owned Chrome window.
    pub(super) fn prepare() -> Result<Self, &'static str> {
        Ok(Self {
            chrome: Arc::new(ChromeParent::attest()?),
            original: OriginalWindow::capture()?,
            target: None,
            input_after: None,
            attempted: false,
        })
    }

    /// The permit is consumed even when inspection/input acknowledgement fails.
    /// No original browser operation is dispatched or repeated here.
    pub(super) async fn escape(
        &mut self,
        page_url: &str,
        deadline: Instant,
    ) -> Result<Value, String> {
        if self.attempted {
            return Err("native Escape was already attempted".into());
        }
        self.attempted = true;
        let pid = self.chrome.require_alive()?;
        let target = Window::foreground()?;
        if target.pid != pid
            || !String::from_utf16_lossy(&target.class).starts_with("Chrome_WidgetWin_")
            // SAFETY: target is an observed HWND; these are read-only queries.
            || unsafe { IsWindowVisible(target.hwnd) } == 0
            || unsafe { IsIconic(target.hwnd) } != 0
            || keys_or_buttons_held()
        {
            return Err("foreground is not the attested Chrome window or input is held".into());
        }
        let stamp = input_stamp()?;
        let _proof = super::windows::inspect(
            target.hwnd as u64,
            pid,
            &String::from_utf16_lossy(&target.title),
            page_url,
        )
        .await?;
        let settle = crate::native::humanize::settle_before_press(
            crate::native::humanize::active_level(),
            crate::native::humanize::next_seed(),
        );
        tokio::time::sleep(settle).await;
        self.chrome.require_alive()?;
        if Instant::now() >= deadline
            || !target.unchanged_foreground()
            || input_stamp()? != stamp
            || keys_or_buttons_held()
        {
            return Err("native target or user input changed before Escape; no key sent".into());
        }
        self.target = Some(target.clone());
        self.input_after = Some(stamp);
        // Keep key release in one blocking task: cancelling an async request
        // during the humanized dwell must not leave Escape logically held.
        let chrome = self.chrome.clone();
        let outcome = tokio::task::spawn_blocking(move || {
            chrome.require_alive()?;
            if Instant::now() >= deadline
                || !target.unchanged_foreground()
                || input_stamp()? != stamp
                || keys_or_buttons_held()
            {
                return Err("native target changed before key admission");
            }
            let input = |flags| INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 {
                    ki: KEYBDINPUT {
                        wVk: VK_ESCAPE,
                        wScan: 0,
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
        self.input_after = Some(outcome.2);
        Ok(
            json!({"keyDownAdmitted":outcome.0, "keyUpAdmitted":outcome.1,
            "delivery":"unverified", "replayed":false}),
        )
    }

    /// Restore only while the exact original window and our foreground binding
    /// survive, and no new OS input occurred. Never chase a user's new focus.
    pub(super) fn finish(&mut self) -> Value {
        let Some(target) = self.target.take() else {
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
        let restored = self.original.window.unchanged_foreground();
        json!({"attempted":true,"admitted":admitted,"restored":restored})
    }
}

impl Drop for DesktopLease {
    fn drop(&mut self) {
        // Host disconnect/cancellation must not silently keep a verified window
        // in front. The same no-interference checks apply to this fallback.
        let _ = self.finish();
    }
}

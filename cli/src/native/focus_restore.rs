//! Put the user's app back in front after a page-opened tab raised Chrome
//! (#468). On by default on macOS; [`RESTORE_FOCUS_ENV`]`=0` turns it off.
//!
//! Chrome itself activates its window, and with it the app, when a page opens
//! a tab or pop-up (see the note at the top of `popup_guard.rs`), before any
//! extension or CDP event about the tab. Nothing chrome-use can do before the
//! click prevents it without changing the click, so it undoes it instead:
//!
//! 1. Right before an agent click, read the frontmost app.
//! 2. While the click's tab check runs (at most [`WINDOW`] after the click),
//!    poll the frontmost app every [`POLL`]. When it becomes the browser and
//!    the browser has a tab that did not exist before the click (the
//!    `chrome.tabs` baseline over the relay, the target list for a browser
//!    the daemon launched), re-activate the app that was in front before.
//!
//! It never acts when:
//! - the browser was already in front before the click (the user is in
//!   Chrome: switching apps would take them out of it);
//! - the frontmost app became anything other than the browser (the user moved
//!   somewhere else; that is theirs);
//! - the browser came to the front without a new tab (the user clicked into
//!   it), or more than [`WINDOW`] after the click.
//!
//! Mechanism (measured on macOS 27, Chrome for Testing 155): the frontmost
//! app is read with `GetFrontProcess` (Process Manager, deprecated but
//! exported). `NSWorkspace.frontmostApplication` is NOT used: in a process
//! without a running main run loop (the daemon) it is a cached value that
//! never changes. The app is brought back with `SetFrontProcessWithOptions`
//! (front window only), which put Finder back 6–9 ms after the call from a
//! background thread of a run-loop-less process, with no prompt.
//! `-[NSRunningApplication activateWithOptions:]` works from the main thread
//! but off it (where the daemon's tokio workers run) the call blocked
//! ~1010 ms before Finder came back. Neither launches anything, opens windows
//! or sends a reopen event (`open -b` would); AppleScript `activate` was not
//! used because sending Apple Events to another app can raise an Automation
//! permission prompt. The frameworks are loaded with `dlopen` the first time
//! they are needed, so other commands do not pay for them.
//!
//! Windows and Linux: no equivalent is attempted. Windows' foreground lock
//! normally keeps a background process from taking the foreground, and on
//! Linux it depends on the window manager; the click says so.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::cdp::client::CdpClient;

/// `0`/`off`/`false`/`no` turns focus restore off. Read by the daemon from
/// its own environment.
pub const RESTORE_FOCUS_ENV: &str = "AGENT_BROWSER_RESTORE_FOCUS";

/// How long after the click Chrome coming to the front is still undone.
pub const WINDOW: Duration = Duration::from_millis(1500);

/// How often the frontmost app is read while watching.
pub const POLL: Duration = Duration::from_millis(10);

/// How long the click's tab check waits for the watch to decide, once it
/// knows the click opened a tab.
pub const REPORT_WAIT: Duration = Duration::from_millis(400);

/// Whether focus restore is on (default on).
pub fn enabled(value: Option<&str>) -> bool {
    !matches!(
        value.map(|v| v.trim().to_ascii_lowercase()).as_deref(),
        Some("0" | "off" | "false" | "no")
    )
}

/// An application, as the window server sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct App {
    pub pid: i32,
    pub bundle_id: Option<String>,
}

/// Chromium-based browsers the extension (and so the relay) can run in.
const BROWSER_BUNDLE_PREFIXES: &[&str] = &[
    "com.google.chrome",
    "org.chromium.",
    "com.microsoft.edgemac",
    "com.brave.browser",
    "com.vivaldi.vivaldi",
    "com.operasoftware.opera",
    "company.thebrowser.",
    "net.imput.helium",
];

/// Whether `app` is the browser chrome-use drives: the browser the daemon
/// launched (`browser_pid`), or over the relay any Chromium-based browser.
pub fn is_browser(app: &App, browser_pid: Option<i32>) -> bool {
    if browser_pid == Some(app.pid) {
        return true;
    }
    let Some(id) = app.bundle_id.as_deref() else {
        return false;
    };
    let id = id.to_ascii_lowercase();
    BROWSER_BUNDLE_PREFIXES
        .iter()
        .any(|p| id == p.trim_end_matches('.') || id.starts_with(p))
}

/// What the watch found. Reported on the click as `focusRestored` and,
/// unless restored, `focusNote`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    /// Still watching: Chrome has not come to the front (yet).
    Watching,
    /// Chrome came to the front and the previous app is in front again.
    Restored {
        app: String,
        /// How long Chrome was in front as far as the watch saw (from the
        /// first sample showing it to the sample showing the app again).
        chrome_front_ms: u64,
    },
    /// Nothing was done, and why.
    Skipped(Reason),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reason {
    Off,
    UnsupportedPlatform,
    FrontUnknown,
    BrowserWasFront,
    UserMoved,
    NoNewTab,
    NotRaised,
    ActivateFailed(String),
}

impl Reason {
    pub fn note(&self) -> String {
        match self {
            Reason::Off => format!("focus restore is off ({RESTORE_FOCUS_ENV}=0)"),
            Reason::UnsupportedPlatform => {
                "focus restore is only done on macOS; Chrome's window may be in front".into()
            }
            Reason::FrontUnknown => "chrome-use could not read which app was in front".into(),
            Reason::BrowserWasFront => "the browser was already the frontmost app before the \
                 click, so chrome-use did not switch apps; the agent's window may now be the \
                 browser's focused window"
                .into(),
            Reason::UserMoved => "another app came to the front during the click; chrome-use \
                 left it there"
                .into(),
            Reason::NoNewTab => "the browser came to the front without a new tab, so it was \
                 not the click's doing; chrome-use left it there"
                .into(),
            Reason::NotRaised => "the browser did not come to the front".into(),
            Reason::ActivateFailed(why) => {
                format!("chrome-use could not put the previous app back in front ({why})")
            }
        }
    }
}

impl Status {
    /// The click's `focusRestored` / `focusNote` fields.
    pub fn fields(&self) -> Value {
        match self {
            Status::Restored {
                app,
                chrome_front_ms,
            } => json!({
                "focusRestored": true,
                "focusRestoredApp": app,
                "focusRestoredAfterMs": chrome_front_ms,
            }),
            Status::Skipped(r) => json!({ "focusRestored": false, "focusNote": r.note() }),
            Status::Watching => {
                json!({ "focusRestored": false, "focusNote": Reason::NotRaised.note() })
            }
        }
    }
}

/// The click's #468 note (`popup_guard::chrome_raised_note`), completed
/// with what the focus watch did.
pub fn raised_note(note: &str, status: Option<&Status>) -> String {
    match status {
        Some(Status::Restored {
            app,
            chrome_front_ms,
        }) => {
            let restored = format!(
                "; it did, and chrome-use put the app the user was in ({app}) back in front \
                 {chrome_front_ms} ms later ({RESTORE_FOCUS_ENV}=0 turns this off)"
            );
            // Right after the sentence about Chrome coming to the front, before
            // any hint about AGENT_BROWSER_BACKGROUND_LINKS.
            match note.find("(#468)") {
                Some(i) => {
                    let at = i + "(#468)".len();
                    format!("{}{restored}{}", &note[..at], &note[at..])
                }
                None => format!("{note}{restored}"),
            }
        }
        _ => note.to_string(),
    }
}

/// How the watch tells that the browser has a tab the click opened.
pub enum NewTabProbe {
    /// Over the relay: Chrome tab ids (`chrome.tabs.query`) before the click.
    RelayTabs(HashSet<i64>),
    /// A browser the daemon launched: page target ids before the click.
    Targets(HashSet<String>),
}

impl NewTabProbe {
    async fn new_tab_exists(&self, client: &CdpClient) -> bool {
        let fut = async {
            match self {
                NewTabProbe::RelayTabs(before) => {
                    let v = client
                        .send_command(
                            "ABExt.call",
                            Some(json!({ "namespace": "tabs", "method": "query", "args": [{}] })),
                            None,
                        )
                        .await
                        .ok()?;
                    let ids = relay_tab_ids(&v);
                    Some(ids.iter().any(|id| !before.contains(id)))
                }
                NewTabProbe::Targets(before) => {
                    let v = client
                        .send_command("Target.getTargets", None, None)
                        .await
                        .ok()?;
                    Some(new_page_target(&v, before))
                }
            }
        };
        tokio::time::timeout(Duration::from_millis(500), fut)
            .await
            .ok()
            .flatten()
            .unwrap_or(false)
    }
}

/// Tab ids in an `ABExt.call` `tabs.query` reply.
fn relay_tab_ids(reply: &Value) -> Vec<i64> {
    reply
        .get("result")
        .and_then(Value::as_array)
        .map(|tabs| {
            tabs.iter()
                .filter_map(|t| t.get("id").and_then(Value::as_i64))
                .collect()
        })
        .unwrap_or_default()
}

/// Whether a `Target.getTargets` reply has a page target not in `before`.
fn new_page_target(reply: &Value, before: &HashSet<String>) -> bool {
    reply
        .get("targetInfos")
        .and_then(Value::as_array)
        .is_some_and(|ts| {
            ts.iter().any(|t| {
                t.get("type").and_then(Value::as_str) == Some("page")
                    && t.get("targetId")
                        .and_then(Value::as_str)
                        .is_some_and(|id| !before.contains(id))
            })
        })
}

/// A running watch for one click. Dropping it stops the watch.
pub struct FocusWatch {
    status: Arc<Mutex<Status>>,
    stop: Arc<std::sync::atomic::AtomicBool>,
    /// With [`TRACE_ENV`]`=1`: what the watch saw, in ms since it was armed.
    trace: Option<Arc<Mutex<Vec<String>>>>,
}

/// `1` adds `focusTrace` (the watch's timeline) to the click, for measuring.
pub const TRACE_ENV: &str = "AGENT_BROWSER_FOCUS_TRACE";

impl Drop for FocusWatch {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

impl FocusWatch {
    fn done(status: Status) -> Self {
        Self {
            status: Arc::new(Mutex::new(status)),
            stop: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            trace: None,
        }
    }

    /// The watch's timeline, when [`TRACE_ENV`] is set.
    pub fn trace(&self) -> Option<Value> {
        let t = self.trace.as_ref()?.lock().ok()?;
        Some(json!(*t))
    }

    pub fn status(&self) -> Status {
        self.status
            .lock()
            .map(|s| s.clone())
            .unwrap_or(Status::Watching)
    }

    /// The status once it has settled, waiting at most `wait` for a watch
    /// that has not seen the browser come to the front yet.
    pub async fn settled(&self, wait: Duration) -> Status {
        let deadline = Instant::now() + wait;
        loop {
            let s = self.status();
            if s != Status::Watching || Instant::now() >= deadline {
                return s;
            }
            tokio::time::sleep(POLL).await;
        }
    }

    /// Start watching, right before an agent click. `browser_pid` is the pid
    /// of a browser the daemon launched (`None` over the relay).
    pub fn arm(client: Arc<CdpClient>, probe: NewTabProbe, browser_pid: Option<i32>) -> Self {
        if !enabled(std::env::var(RESTORE_FOCUS_ENV).ok().as_deref()) {
            return Self::done(Status::Skipped(Reason::Off));
        }
        if !platform::SUPPORTED {
            return Self::done(Status::Skipped(Reason::UnsupportedPlatform));
        }
        let Some(before) = platform::frontmost() else {
            return Self::done(Status::Skipped(Reason::FrontUnknown));
        };
        if is_browser(&before, browser_pid) {
            return Self::done(Status::Skipped(Reason::BrowserWasFront));
        }
        let trace = (std::env::var(TRACE_ENV).ok().as_deref() == Some("1"))
            .then(|| Arc::new(Mutex::new(Vec::new())));
        let watch = Self {
            status: Arc::new(Mutex::new(Status::Watching)),
            stop: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            trace: trace.clone(),
        };
        let (status, stop) = (watch.status.clone(), watch.stop.clone());
        let armed_at = Instant::now();
        let note = move |what: String| {
            if let Some(t) = trace.as_ref().and_then(|t| t.lock().ok()).as_mut() {
                t.push(format!("{} {what}", armed_at.elapsed().as_millis()));
            }
        };
        note(format!(
            "armed; front pid {} {}",
            before.pid,
            before.bundle_id.as_deref().unwrap_or("?")
        ));
        tokio::spawn(async move {
            let set = |s: Status| {
                if let Ok(mut g) = status.lock() {
                    *g = s;
                }
            };
            let mut seen_at: Option<Instant> = None;
            let mut confirmed = false;
            let mut restored = false;
            loop {
                if armed_at.elapsed() > WINDOW || stop.load(std::sync::atomic::Ordering::Relaxed) {
                    note("watch ended".into());
                    return;
                }
                let Some(front) = platform::frontmost() else {
                    tokio::time::sleep(POLL).await;
                    continue;
                };
                if front.pid == before.pid {
                    tokio::time::sleep(POLL).await;
                    continue;
                }
                if !is_browser(&front, browser_pid) {
                    note(format!(
                        "front is pid {} {}: not the browser",
                        front.pid,
                        front.bundle_id.as_deref().unwrap_or("?")
                    ));
                    // The user went to another app: theirs. A restore already
                    // done stays reported.
                    if !restored {
                        set(Status::Skipped(Reason::UserMoved));
                    }
                    return;
                }
                let seen = *seen_at.get_or_insert_with(|| {
                    note(format!(
                        "browser in front: pid {} {}",
                        front.pid,
                        front.bundle_id.as_deref().unwrap_or("?")
                    ));
                    Instant::now()
                });
                if !confirmed {
                    confirmed = probe.new_tab_exists(&client).await;
                    note(format!("new tab: {confirmed}"));
                    if !confirmed {
                        set(Status::Skipped(Reason::NoNewTab));
                        return;
                    }
                }
                // Still the browser, still inside the window: put the app
                // the user was in back in front.
                if armed_at.elapsed() > WINDOW
                    || platform::frontmost().map(|f| f.pid) != Some(front.pid)
                {
                    continue;
                }
                match platform::activate(before.pid).await {
                    Ok(()) => {
                        note("previous app in front again".into());
                        restored = true;
                        seen_at = None;
                        set(Status::Restored {
                            app: before
                                .bundle_id
                                .clone()
                                .unwrap_or_else(|| format!("pid {}", before.pid)),
                            chrome_front_ms: seen.elapsed().as_millis() as u64,
                        });
                        // Keep watching to the end of the window: if Chrome
                        // raises itself again (a second window), undo that
                        // too; any other app ends the watch.
                    }
                    Err(e) => {
                        note(format!("activation failed: {e}"));
                        if !restored {
                            set(Status::Skipped(Reason::ActivateFailed(e)));
                        }
                        return;
                    }
                }
            }
        });
        watch
    }
}

#[cfg(target_os = "macos")]
mod platform {
    //! The Process Manager (`GetFrontProcess`, `SetFrontProcessWithOptions`,
    //! `ProcessInformationCopyDictionary`) and CoreFoundation, loaded with
    //! `dlopen` so other commands do not load them. All of it works from any
    //! thread without a run loop. AppKit is not used: `-[NSRunningApplication
    //! activateWithOptions:]` called off the main thread (the daemon's tokio
    //! workers) blocked ~1010 ms before the activation took effect, while
    //! `SetFrontProcessWithOptions` took 6–9 ms.
    use super::App;
    use std::ffi::{c_char, c_void, CStr};
    use std::sync::OnceLock;
    use std::time::{Duration, Instant};

    pub const SUPPORTED: bool = true;

    #[repr(C)]
    struct Psn {
        high: u32,
        low: u32,
    }

    /// `kSetFrontProcessFrontWindowOnly`: bring the app's front window
    /// forward, as `activateWithOptions:` without `AllWindows` does.
    const FRONT_WINDOW_ONLY: u32 = 1;
    /// `kProcessDictionaryIncludeAllInformationMask`.
    const ALL_INFORMATION: u32 = 0xFFFF_FFFF;
    /// `kCFStringEncodingUTF8`.
    const UTF8: u32 = 0x0800_0100;

    type CfRef = *const c_void;

    struct Api {
        get_front_process: unsafe extern "C" fn(*mut Psn) -> i32,
        get_process_pid: unsafe extern "C" fn(*const Psn, *mut i32) -> i32,
        get_process_for_pid: unsafe extern "C" fn(i32, *mut Psn) -> i32,
        set_front_process_with_options: unsafe extern "C" fn(*const Psn, u32) -> i32,
        process_information_copy_dictionary: unsafe extern "C" fn(*const Psn, u32) -> CfRef,
        cf_dictionary_get_value: unsafe extern "C" fn(CfRef, CfRef) -> CfRef,
        cf_string_create_with_c_string: unsafe extern "C" fn(CfRef, *const c_char, u32) -> CfRef,
        cf_string_get_c_string: unsafe extern "C" fn(CfRef, *mut c_char, isize, u32) -> u8,
        cf_get_type_id: unsafe extern "C" fn(CfRef) -> usize,
        cf_string_get_type_id: unsafe extern "C" fn() -> usize,
        cf_release: unsafe extern "C" fn(CfRef),
    }

    fn api() -> Option<&'static Api> {
        static API: OnceLock<Option<Api>> = OnceLock::new();
        API.get_or_init(|| unsafe { load() }).as_ref()
    }

    unsafe fn load() -> Option<Api> {
        for lib in [
            c"/System/Library/Frameworks/CoreFoundation.framework/CoreFoundation",
            c"/System/Library/Frameworks/ApplicationServices.framework/ApplicationServices",
        ] {
            if libc::dlopen(lib.as_ptr(), libc::RTLD_LAZY | libc::RTLD_GLOBAL).is_null() {
                return None;
            }
        }
        let sym = |name: &CStr| {
            let p = libc::dlsym(libc::RTLD_DEFAULT, name.as_ptr());
            (!p.is_null()).then_some(p)
        };
        macro_rules! f {
            ($name:literal) => {
                std::mem::transmute::<*mut c_void, _>(sym($name)?)
            };
        }
        Some(Api {
            get_front_process: f!(c"GetFrontProcess"),
            get_process_pid: f!(c"GetProcessPID"),
            get_process_for_pid: f!(c"GetProcessForPID"),
            set_front_process_with_options: f!(c"SetFrontProcessWithOptions"),
            process_information_copy_dictionary: f!(c"ProcessInformationCopyDictionary"),
            cf_dictionary_get_value: f!(c"CFDictionaryGetValue"),
            cf_string_create_with_c_string: f!(c"CFStringCreateWithCString"),
            cf_string_get_c_string: f!(c"CFStringGetCString"),
            cf_get_type_id: f!(c"CFGetTypeID"),
            cf_string_get_type_id: f!(c"CFStringGetTypeID"),
            cf_release: f!(c"CFRelease"),
        })
    }

    fn front(a: &Api) -> Option<(Psn, i32)> {
        let mut psn = Psn { high: 0, low: 0 };
        let mut pid = -1;
        unsafe {
            if (a.get_front_process)(&mut psn) != 0 || (a.get_process_pid)(&psn, &mut pid) != 0 {
                return None;
            }
        }
        (pid > 0).then_some((psn, pid))
    }

    fn bundle_id(a: &Api, psn: &Psn) -> Option<String> {
        unsafe {
            let dict = (a.process_information_copy_dictionary)(psn, ALL_INFORMATION);
            if dict.is_null() {
                return None;
            }
            let key = (a.cf_string_create_with_c_string)(
                std::ptr::null(),
                c"CFBundleIdentifier".as_ptr(),
                UTF8,
            );
            let mut out = None;
            if !key.is_null() {
                let v = (a.cf_dictionary_get_value)(dict, key);
                if !v.is_null() && (a.cf_get_type_id)(v) == (a.cf_string_get_type_id)() {
                    let mut buf = [0 as c_char; 512];
                    if (a.cf_string_get_c_string)(v, buf.as_mut_ptr(), buf.len() as isize, UTF8)
                        != 0
                    {
                        out = Some(CStr::from_ptr(buf.as_ptr()).to_string_lossy().into_owned());
                    }
                }
                (a.cf_release)(key);
            }
            (a.cf_release)(dict);
            out
        }
    }

    /// The frontmost application, read from the window server each time.
    pub fn frontmost() -> Option<App> {
        let a = api()?;
        let (psn, pid) = front(a)?;
        Some(App {
            pid,
            bundle_id: bundle_id(a, &psn),
        })
    }

    /// Bring `pid` to the front and wait (briefly) until it is the frontmost
    /// app. Never launches anything or opens a window.
    pub async fn activate(pid: i32) -> Result<(), String> {
        let a = api().ok_or("the Process Manager is not available")?;
        let mut psn = Psn { high: 0, low: 0 };
        let err = unsafe {
            if (a.get_process_for_pid)(pid, &mut psn) != 0 {
                return Err("the app is no longer running".into());
            }
            (a.set_front_process_with_options)(&psn, FRONT_WINDOW_ONLY)
        };
        if err != 0 {
            return Err(format!(
                "macOS refused to bring it to the front (error {err})"
            ));
        }
        let deadline = Instant::now() + Duration::from_millis(300);
        while Instant::now() < deadline {
            if front(a).map(|(_, p)| p) == Some(pid) {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        Err("it did not come to the front within 300 ms".into())
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    use super::App;

    pub const SUPPORTED: bool = false;

    pub fn frontmost() -> Option<App> {
        None
    }

    pub async fn activate(_pid: i32) -> Result<(), String> {
        Err("unsupported platform".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn on_by_default_and_off_only_when_said() {
        assert!(enabled(None));
        assert!(enabled(Some("")));
        assert!(enabled(Some("1")));
        assert!(enabled(Some("on")));
        for off in ["0", "off", " OFF ", "false", "no"] {
            assert!(!enabled(Some(off)), "{off}");
        }
    }

    #[test]
    fn the_browser_is_the_launched_pid_or_a_chromium_bundle() {
        let app = |pid, id: Option<&str>| App {
            pid,
            bundle_id: id.map(str::to_string),
        };
        assert!(is_browser(&app(5, Some("com.google.Chrome")), None));
        assert!(is_browser(&app(5, Some("com.google.Chrome.canary")), None));
        assert!(is_browser(
            &app(5, Some("com.google.chrome.for.testing")),
            None
        ));
        assert!(is_browser(&app(5, Some("org.chromium.Chromium")), None));
        assert!(is_browser(&app(5, Some("com.microsoft.edgemac.Dev")), None));
        assert!(is_browser(&app(5, Some("com.brave.Browser")), None));
        assert!(is_browser(
            &app(5, Some("company.thebrowser.Browser")),
            None
        ));
        assert!(!is_browser(&app(5, Some("com.apple.finder")), None));
        assert!(!is_browser(&app(5, Some("com.apple.Safari")), None));
        assert!(!is_browser(&app(5, None), None));
        // A launched browser is matched by pid, whatever its bundle.
        assert!(is_browser(&app(7, None), Some(7)));
        assert!(!is_browser(&app(8, None), Some(7)));
    }

    #[test]
    fn a_new_tab_is_one_not_in_the_baseline() {
        let before: HashSet<String> = ["A".to_string()].into_iter().collect();
        let only_old = json!({ "targetInfos": [{ "type": "page", "targetId": "A" }] });
        let new_worker = json!({ "targetInfos": [
            { "type": "page", "targetId": "A" }, { "type": "service_worker", "targetId": "W" }] });
        let new_page = json!({ "targetInfos": [
            { "type": "page", "targetId": "A" }, { "type": "page", "targetId": "B" }] });
        assert!(!new_page_target(&only_old, &before));
        assert!(!new_page_target(&new_worker, &before));
        assert!(new_page_target(&new_page, &before));
        assert!(!new_page_target(&json!({}), &before));
        assert_eq!(
            relay_tab_ids(&json!({ "result": [{ "id": 3 }, { "id": 9 }, {}] })),
            vec![3, 9]
        );
        assert!(relay_tab_ids(&json!({ "result": null })).is_empty());
    }

    #[test]
    fn the_click_reports_what_happened() {
        let restored = Status::Restored {
            app: "com.apple.finder".into(),
            chrome_front_ms: 12,
        }
        .fields();
        assert_eq!(restored["focusRestored"], true);
        assert_eq!(restored["focusRestoredApp"], "com.apple.finder");
        assert!(restored.get("focusNote").is_none());
        let user = Status::Skipped(Reason::BrowserWasFront).fields();
        assert_eq!(user["focusRestored"], false);
        assert!(user["focusNote"]
            .as_str()
            .unwrap()
            .contains("did not switch apps"));
        let off = Status::Skipped(Reason::Off).fields();
        assert!(off["focusNote"]
            .as_str()
            .unwrap()
            .contains(RESTORE_FOCUS_ENV));
    }

    #[test]
    fn the_468_note_says_the_app_was_put_back() {
        let note = "the page opened this tab itself, and Chrome brings its window to the \
                    front, so it may have come over the app the user is in (#468). HINT";
        let restored = Status::Restored {
            app: "com.apple.finder".into(),
            chrome_front_ms: 40,
        };
        let out = raised_note(note, Some(&restored));
        assert!(out.contains("(#468); it did, and chrome-use put the app the user was in (com.apple.finder) back in front 40 ms later"));
        assert!(out.ends_with(". HINT"));
        assert_eq!(raised_note(note, None), note);
        assert_eq!(
            raised_note(note, Some(&Status::Skipped(Reason::BrowserWasFront))),
            note
        );
    }

    #[test]
    fn a_settled_watch_does_not_wait() {
        let w = FocusWatch::done(Status::Skipped(Reason::Off));
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        let start = Instant::now();
        let s = rt.block_on(w.settled(Duration::from_secs(5)));
        assert_eq!(s, Status::Skipped(Reason::Off));
        assert!(start.elapsed() < Duration::from_secs(1));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_frontmost_app_can_be_read_or_is_unknown() {
        // Headless CI has no window server session; either way no panic.
        if let Some(app) = platform::frontmost() {
            assert!(app.pid > 0);
        }
    }
}

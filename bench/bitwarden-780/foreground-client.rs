//! CI-only foreground delegation probe. No window activation or input APIs.

#[cfg(windows)]
fn main() {
    #[link(name = "user32")]
    unsafe extern "system" {
        fn AllowSetForegroundWindow(pid: u32) -> i32;
        fn GetForegroundWindow() -> isize;
        fn GetWindowThreadProcessId(hwnd: isize, pid: *mut u32) -> u32;
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetLastError() -> u32;
    }
    let args: Vec<String> = std::env::args().collect();
    let pid = args.get(2).and_then(|value| value.parse::<u32>().ok());
    if std::env::var("GITHUB_ACTIONS").as_deref() != Ok("true")
        || args.len() != 3
        || args[1] != "--ci-allow-foreground-delegation"
        || !pid.is_some_and(|value| value > 0 && value < u32::MAX)
    {
        eprintln!("Only an explicitly authorized CI fixture Chrome PID is accepted");
        std::process::exit(2);
    }
    let pid = pid.unwrap();
    // SAFETY: read-only query and a live writable DWORD. No input is posted.
    let before = unsafe { GetForegroundWindow() };
    let mut before_pid = 0;
    unsafe { GetWindowThreadProcessId(before, &mut before_pid) };
    // SAFETY: one validated process ID, never ASFW_ANY. The OS decides whether
    // this fresh child has permission to delegate; no policy is bypassed.
    let admitted = unsafe { AllowSetForegroundWindow(pid) } != 0;
    let error = if admitted {
        0
    } else {
        unsafe { GetLastError() }
    };
    let after = unsafe { GetForegroundWindow() };
    let mut after_pid = 0;
    unsafe { GetWindowThreadProcessId(after, &mut after_pid) };
    println!(
        "{{\"success\":{admitted},\"data\":{{\"callerPid\":{},\"chromePid\":{pid},\"admitted\":{admitted},\"errorCode\":{error},\"foregroundBefore\":{before},\"foregroundPidBefore\":{before_pid},\"foregroundAfter\":{after},\"foregroundPidAfter\":{after_pid},\"nativeInputAttempted\":false,\"windowActivationAttempted\":false}}}}",
        std::process::id()
    );
    if !admitted {
        std::process::exit(1);
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!("Only the Windows CI fixture is supported");
    std::process::exit(2);
}

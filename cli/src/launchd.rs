//! One-shot launchd jobs: start a process of ours outside the caller's
//! process tree.
//!
//! Under Chrome every descendant of the native-messaging host inherits
//! Chrome's process quarantine, and every file it writes is quarantined. The
//! panel service and a background upgrade started from the host therefore run
//! as a launchd job in the user's GUI domain instead of as a child.

#![cfg(target_os = "macos")]

use std::ffi::OsStr;
use std::path::Path;
use std::time::Duration;

/// `launchctl` with `args`, quietly; whether it succeeded.
pub fn launchctl(args: &[&OsStr]) -> bool {
    std::process::Command::new("/bin/launchctl")
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// The pid of a launchd job's running process, if it has one.
pub fn job_pid(target: &str) -> Option<u32> {
    let out = std::process::Command::new("/bin/launchctl")
        .args(["print", target])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .find_map(|line| line.trim().strip_prefix("pid = ")?.trim().parse().ok())
}

/// What launchd reports about a job (state, pid, runs, last exit), for an
/// error that has to say whether the job ever ran. `not loaded` only when
/// launchd says it has no such job; a query that fails or takes longer than
/// two seconds is an error.
pub fn job_summary(target: &str) -> Result<String, String> {
    use std::io::Read;
    let mut child = std::process::Command::new("/bin/launchctl")
        .args(["print", target])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|error| format!("launchctl print did not start ({error})"))?;
    // The report includes the job's environment, which the plist copies in
    // full, so it can outgrow the pipe buffer: drain it while the child runs.
    let stdout = child.stdout.take();
    let reader = std::thread::spawn(move || {
        let mut text = String::new();
        if let Some(mut stdout) = stdout {
            let _ = stdout.read_to_string(&mut text);
        }
        text
    });
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    let status = loop {
        let failure = match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
                continue;
            }
            Ok(None) => "launchctl print timed out".to_string(),
            Err(error) => format!("launchctl print could not be awaited ({error})"),
        };
        let _ = child.kill();
        let _ = child.wait();
        let _ = reader.join();
        return Err(failure);
    };
    let text = reader.join().unwrap_or_default();
    match status.code() {
        Some(0) => Ok(summarize_print(&text)),
        // EX_NOTFOUND-style 113: "Could not find service ... in domain".
        Some(113) => Ok("not loaded".into()),
        _ => Err(format!("launchctl print failed ({status})")),
    }
}

/// The first `state`, `pid`, `runs` and last-exit lines of `launchctl print`.
/// Nested sections repeat some keys; the job's own come first.
fn summarize_print(text: &str) -> String {
    const KEYS: [&str; 6] = [
        "state = ",
        "pid = ",
        "runs = ",
        "last exit code = ",
        "last exit reason = ",
        "last terminating signal = ",
    ];
    let mut found: Vec<Option<&str>> = vec![None; KEYS.len()];
    for line in text.lines().map(str::trim) {
        if let Some(i) = KEYS.iter().position(|key| line.starts_with(key)) {
            found[i].get_or_insert(line);
        }
    }
    let fields: Vec<&str> = found.into_iter().flatten().collect();
    if fields.is_empty() {
        "no state reported".into()
    } else {
        fields.join(", ")
    }
}

/// `gui/<uid>`, the domain of the logged-in user's jobs.
pub fn gui_domain() -> String {
    format!("gui/{}", unsafe { libc::getuid() })
}

/// Load the job defined at `plist` into `domain`, replacing a job of the same
/// `target` left loaded by an earlier start. The file is removed afterwards:
/// it carries the caller's environment, so it lives only until launchd has
/// read it.
pub fn bootstrap(domain: &str, target: &str, plist: &Path) -> bool {
    // A quarantined writer quarantines the plist too, and launchd refuses to
    // load a quarantined job definition.
    {
        use std::os::unix::ffi::OsStrExt;
        if let Ok(file) = std::ffi::CString::new(plist.as_os_str().as_bytes()) {
            unsafe { libc::removexattr(file.as_ptr(), c"com.apple.quarantine".as_ptr(), 0) };
        }
    }
    let _ = launchctl(&["bootout".as_ref(), target.as_ref()]);
    // bootout returns before launchd forgets the label; retry briefly.
    let mut loaded = false;
    for _ in 0..20 {
        if launchctl(&["bootstrap".as_ref(), domain.as_ref(), plist.as_os_str()]) {
            loaded = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = std::fs::remove_file(plist);
    loaded
}

/// A job that runs `executable arguments` once at load, with this process's
/// environment and working directory, and is not restarted.
pub fn plist(
    label: &str,
    executable: &Path,
    arguments: &[std::ffi::OsString],
) -> Result<String, String> {
    fn text(value: &OsStr) -> Option<String> {
        let value = value.to_str()?;
        if value
            .chars()
            .any(|c| c.is_control() && !matches!(c, '\t' | '\n' | '\r'))
        {
            return None;
        }
        Some(
            value
                .replace('&', "&amp;")
                .replace('<', "&lt;")
                .replace('>', "&gt;"),
        )
    }
    let mut program = String::new();
    for argument in
        std::iter::once(executable.as_os_str()).chain(arguments.iter().map(|a| a.as_os_str()))
    {
        let argument = text(argument).ok_or("launchd job arguments are not plist text")?;
        program.push_str(&format!("<string>{argument}</string>"));
    }
    let mut environment = String::new();
    // A job runs outside Chrome's tree, whatever the caller's marker says.
    for (key, value) in std::env::vars_os().filter(|(k, _)| k != crate::upgrade::UNDER_CHROME_ENV) {
        if let (Some(key), Some(value)) = (text(&key), text(&value)) {
            environment.push_str(&format!("<key>{key}</key><string>{value}</string>"));
        }
    }
    let directory = std::env::current_dir()
        .ok()
        .and_then(|d| text(d.as_os_str()))
        .unwrap_or_else(|| "/".into());
    Ok(format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
<key>Label</key><string>{label}</string>
<key>ProgramArguments</key><array>{program}</array>
<key>EnvironmentVariables</key><dict>{environment}</dict>
<key>WorkingDirectory</key><string>{directory}</string>
<key>RunAtLoad</key><true/>
<key>KeepAlive</key><false/>
<key>ProcessType</key><string>Interactive</string>
<key>AbandonProcessGroup</key><true/>
</dict>
</plist>
"#
    ))
}

#[cfg(test)]
mod tests {
    use super::summarize_print;

    #[test]
    fn a_job_summary_keeps_the_jobs_own_state_and_last_exit() {
        let print = "gui/501/com.chrome-use.panel-service.0123 = {\n\
            \tactive count = 0\n\
            \tstate = not running\n\
            \tpid = 4242\n\
            \tprogram = /tmp/chrome-use\n\
            \truns = 1\n\
            \tlast exit code = 78: EX_CONFIG\n\
            \tendpoints = {\n\
            \t\tstate = active\n\
            \t}\n\
            }\n";
        assert_eq!(
            summarize_print(print),
            "state = not running, pid = 4242, runs = 1, last exit code = 78: EX_CONFIG"
        );
        assert_eq!(summarize_print("nothing useful\n"), "no state reported");
    }
}

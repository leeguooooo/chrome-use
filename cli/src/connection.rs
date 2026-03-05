use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::env;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

#[cfg(unix)]
use std::os::unix::net::UnixStream;

#[derive(Serialize)]
#[allow(dead_code)]
pub struct Request {
    pub id: String,
    pub action: String,
    #[serde(flatten)]
    pub extra: Value,
}

#[derive(Deserialize, Serialize, Default)]
pub struct Response {
    pub success: bool,
    pub data: Option<Value>,
    pub error: Option<String>,
}

#[allow(dead_code)]
pub enum Connection {
    #[cfg(unix)]
    Unix(UnixStream),
    Tcp(TcpStream),
}

impl Read for Connection {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            #[cfg(unix)]
            Connection::Unix(s) => s.read(buf),
            Connection::Tcp(s) => s.read(buf),
        }
    }
}

impl Write for Connection {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            #[cfg(unix)]
            Connection::Unix(s) => s.write(buf),
            Connection::Tcp(s) => s.write(buf),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            #[cfg(unix)]
            Connection::Unix(s) => s.flush(),
            Connection::Tcp(s) => s.flush(),
        }
    }
}

impl Connection {
    pub fn set_read_timeout(&self, dur: Option<Duration>) -> std::io::Result<()> {
        match self {
            #[cfg(unix)]
            Connection::Unix(s) => s.set_read_timeout(dur),
            Connection::Tcp(s) => s.set_read_timeout(dur),
        }
    }

    pub fn set_write_timeout(&self, dur: Option<Duration>) -> std::io::Result<()> {
        match self {
            #[cfg(unix)]
            Connection::Unix(s) => s.set_write_timeout(dur),
            Connection::Tcp(s) => s.set_write_timeout(dur),
        }
    }
}

/// Get the base directory for socket/pid files.
/// Priority: AGENT_BROWSER_SOCKET_DIR > XDG_RUNTIME_DIR > ~/.agent-browser > tmpdir
pub fn get_socket_dir() -> PathBuf {
    // 1. Explicit override (ignore empty string)
    if let Ok(dir) = env::var("AGENT_BROWSER_SOCKET_DIR") {
        if !dir.is_empty() {
            return PathBuf::from(dir);
        }
    }

    // 2. XDG_RUNTIME_DIR (Linux standard, ignore empty string)
    if let Ok(runtime_dir) = env::var("XDG_RUNTIME_DIR") {
        if !runtime_dir.is_empty() {
            return PathBuf::from(runtime_dir).join("agent-browser");
        }
    }

    // 3. Home directory fallback (like Docker Desktop's ~/.docker/run/)
    if let Some(home) = dirs::home_dir() {
        return home.join(".agent-browser");
    }

    // 4. Last resort: temp dir
    env::temp_dir().join("agent-browser")
}

#[cfg(unix)]
fn get_socket_path(session: &str) -> PathBuf {
    get_socket_dir().join(format!("{}.sock", session))
}

fn get_pid_path(session: &str) -> PathBuf {
    get_socket_dir().join(format!("{}.pid", session))
}

fn get_stream_path(session: &str) -> PathBuf {
    get_socket_dir().join(format!("{}.stream", session))
}

fn get_meta_path(session: &str) -> PathBuf {
    get_socket_dir().join(format!("{}.meta.json", session))
}

fn remove_session_artifacts(session: &str) {
    let _ = fs::remove_file(get_pid_path(session));
    let _ = fs::remove_file(get_stream_path(session));
    let _ = fs::remove_file(get_meta_path(session));

    #[cfg(unix)]
    {
        let _ = fs::remove_file(get_socket_path(session));
    }

    #[cfg(windows)]
    {
        let _ = fs::remove_file(get_port_path(session));
    }
}

/// Clean up stale socket and PID files for a session
fn cleanup_stale_files(session: &str) {
    // Never delete files for a live daemon. A missing PID file can happen in
    // race scenarios, but the socket is authoritative for liveness.
    if daemon_ready(session) {
        return;
    }

    remove_session_artifacts(session);
}

fn should_reap_for_default(session: &str) -> bool {
    session != "default"
}

#[cfg(unix)]
fn process_exists(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    let rc = unsafe { libc::kill(pid as i32, 0) };
    if rc == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
}

#[cfg(windows)]
fn process_exists(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    let output = Command::new("tasklist")
        .args(["/FI", &format!("PID eq {}", pid)])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output();
    match output {
        Ok(out) => {
            let text = String::from_utf8_lossy(&out.stdout);
            text.contains(&format!(" {}", pid))
        }
        Err(_) => false,
    }
}

#[cfg(unix)]
fn terminate_pid(pid: u32) {
    if !process_exists(pid) {
        return;
    }

    let _ = unsafe { libc::kill(pid as i32, libc::SIGTERM) };
    for _ in 0..20 {
        if !process_exists(pid) {
            return;
        }
        thread::sleep(Duration::from_millis(50));
    }

    let _ = unsafe { libc::kill(pid as i32, libc::SIGKILL) };
    for _ in 0..10 {
        if !process_exists(pid) {
            return;
        }
        thread::sleep(Duration::from_millis(20));
    }
}

#[cfg(windows)]
fn terminate_pid(pid: u32) {
    if !process_exists(pid) {
        return;
    }

    let _ = Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

fn terminate_session_daemon(session: &str) {
    let pid_path = get_pid_path(session);
    if let Ok(pid_str) = fs::read_to_string(&pid_path) {
        if let Ok(pid) = pid_str.trim().parse::<u32>() {
            terminate_pid(pid);
        }
    }
}

fn reap_sessions_for_default_start() {
    let socket_dir = get_socket_dir();
    let entries = match fs::read_dir(&socket_dir) {
        Ok(v) => v,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.ends_with(".pid") {
            continue;
        }

        let session = name.trim_end_matches(".pid");
        if session.is_empty() || !should_reap_for_default(session) {
            continue;
        }

        terminate_session_daemon(session);
        remove_session_artifacts(session);
    }
}

fn validate_default_daemon_identity(expected_daemon_path: &Path) -> bool {
    let meta_path = get_meta_path("default");
    let meta_raw = match fs::read_to_string(&meta_path) {
        Ok(v) => v,
        Err(_) => return false,
    };

    let meta: Value = match serde_json::from_str(&meta_raw) {
        Ok(v) => v,
        Err(_) => return false,
    };

    let cli_version = meta
        .get("cliVersion")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let daemon_path = meta
        .get("daemonPath")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    if cli_version != env!("CARGO_PKG_VERSION") || daemon_path.is_empty() {
        return false;
    }

    let expected = expected_daemon_path
        .canonicalize()
        .unwrap_or_else(|_| expected_daemon_path.to_path_buf());
    let observed = PathBuf::from(daemon_path);
    let observed = observed.canonicalize().unwrap_or(observed);
    observed == expected
}

fn resolve_daemon_path() -> Result<PathBuf, String> {
    let exe_path = env::current_exe().map_err(|e| e.to_string())?;
    // Canonicalize to resolve symlinks (e.g., npm global bin symlink -> actual binary)
    let exe_path = exe_path.canonicalize().unwrap_or(exe_path);
    let exe_dir = exe_path.parent().unwrap();

    let mut daemon_paths = vec![
        exe_dir.join("daemon.js"),
        exe_dir.join("../dist/daemon.js"),
        PathBuf::from("dist/daemon.js"),
    ];

    if let Ok(home) = env::var("AGENT_BROWSER_HOME") {
        let home_path = PathBuf::from(&home);
        daemon_paths.insert(0, home_path.join("dist/daemon.js"));
        daemon_paths.insert(1, home_path.join("daemon.js"));
    }

    let daemon_path = daemon_paths
        .into_iter()
        .find(|p| p.exists())
        .ok_or("Daemon not found. Set AGENT_BROWSER_HOME environment variable or run from project directory.")?;
    Ok(daemon_path.canonicalize().unwrap_or(daemon_path))
}

pub fn list_live_sessions() -> Vec<String> {
    let socket_dir = get_socket_dir();
    let mut sessions = Vec::new();

    if let Ok(entries) = fs::read_dir(&socket_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.ends_with(".pid") {
                continue;
            }
            let session_name = name.trim_end_matches(".pid");
            if session_name.is_empty() {
                continue;
            }

            if daemon_ready(session_name) {
                sessions.push(session_name.to_string());
            } else {
                cleanup_stale_files(session_name);
            }
        }
    }

    sessions.sort();
    sessions
}

#[cfg(windows)]
fn get_port_path(session: &str) -> PathBuf {
    get_socket_dir().join(format!("{}.port", session))
}

#[cfg(windows)]
fn get_port_for_session(session: &str) -> u16 {
    let mut hash: i32 = 0;
    for c in session.chars() {
        hash = ((hash << 5).wrapping_sub(hash)).wrapping_add(c as i32);
    }
    // Correct logic: first take absolute modulo, then cast to u16
    // Using unsigned_abs() to safely handle i32::MIN
    49152 + ((hash.unsigned_abs() as u32 % 16383) as u16)
}

fn daemon_ready(session: &str) -> bool {
    #[cfg(unix)]
    {
        let socket_path = get_socket_path(session);
        UnixStream::connect(&socket_path).is_ok()
    }
    #[cfg(windows)]
    {
        let port = get_port_for_session(session);
        TcpStream::connect_timeout(
            &format!("127.0.0.1:{}", port).parse().unwrap(),
            Duration::from_millis(50),
        )
        .is_ok()
    }
}

/// Result of ensure_daemon indicating whether a new daemon was started
pub struct DaemonResult {
    /// True if we connected to an existing daemon, false if we started a new one
    pub already_running: bool,
}

#[allow(clippy::too_many_arguments)]
pub fn ensure_daemon(
    session: &str,
    headed: bool,
    // Keep daemon resident and disable idle auto-shutdown.
    resident: bool,
    executable_path: Option<&str>,
    extensions: &[String],
    args: Option<&str>,
    user_agent: Option<&str>,
    proxy: Option<&str>,
    proxy_bypass: Option<&str>,
    ignore_https_errors: bool,
    allow_file_access: bool,
    state: Option<&str>,
    provider: Option<&str>,
    device: Option<&str>,
    session_name: Option<&str>,
    debug: bool,
    download_path: Option<&str>,
    tab_group: Option<&str>,
    tab_group_plugin_id: Option<&str>,
) -> Result<DaemonResult, String> {
    let daemon_path = resolve_daemon_path()?;

    // Project policy: the default runtime channel is a singleton control plane.
    // Before touching it, reap all non-default channels to avoid stale daemon reuse.
    if session == "default" {
        reap_sessions_for_default_start();
    }

    // Socket readiness is the source of truth for a usable daemon.
    // PID files can be missing/stale under concurrent start/stop races.
    if daemon_ready(session) {
        let mut should_reuse = true;
        if session == "default" {
            should_reuse = validate_default_daemon_identity(&daemon_path);
        }

        if should_reuse {
            // Double-check it's actually responsive by waiting and checking again
            // This handles the race condition where daemon is shutting down
            // (daemon has a 100ms shutdown delay, so we wait longer)
            thread::sleep(Duration::from_millis(150));
            if daemon_ready(session) {
                return Ok(DaemonResult {
                    already_running: true,
                });
            }
        } else {
            terminate_session_daemon(session);
            remove_session_artifacts(session);
        }
    }

    // Clean up any stale socket/pid files before starting fresh
    cleanup_stale_files(session);

    // Ensure socket directory exists
    let socket_dir = get_socket_dir();
    if !socket_dir.exists() {
        fs::create_dir_all(&socket_dir)
            .map_err(|e| format!("Failed to create socket directory: {}", e))?;
    }

    // Pre-flight check: Validate socket path length (Unix limit is 104 bytes including null terminator)
    #[cfg(unix)]
    {
        let socket_path = get_socket_path(session);
        let path_len = socket_path.as_os_str().len();
        if path_len > 103 {
            return Err(format!(
                "Session name '{}' is too long. Socket path would be {} bytes (max 103).\n\
                 Use a shorter session name or set AGENT_BROWSER_SOCKET_DIR to a shorter path.",
                session, path_len
            ));
        }
    }

    // Pre-flight check: Verify socket directory is writable
    {
        let test_file = socket_dir.join(".write_test");
        match fs::write(&test_file, b"") {
            Ok(_) => {
                let _ = fs::remove_file(&test_file);
            }
            Err(e) => {
                return Err(format!(
                    "Socket directory '{}' is not writable: {}",
                    socket_dir.display(),
                    e
                ));
            }
        }
    }

    // Keep handle to detect early daemon exit and surface startup errors.
    #[allow(unused_assignments)]
    let mut daemon_child: Option<std::process::Child> = None;

    // Spawn daemon as a fully detached background process
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;

        let mut cmd = Command::new("node");
        cmd.arg(&daemon_path)
            .arg(if resident {
                "--resident"
            } else {
                "--idle-auto-shutdown"
            })
            .env("AGENT_BROWSER_DAEMON", "1")
            .env("AGENT_BROWSER_SESSION", session)
            .env("AGENT_BROWSER_CLI_VERSION", env!("CARGO_PKG_VERSION"));

        if headed {
            cmd.env("AGENT_BROWSER_HEADED", "1");
        }

        if let Some(path) = executable_path {
            cmd.env("AGENT_BROWSER_EXECUTABLE_PATH", path);
        }

        if !extensions.is_empty() {
            cmd.env("AGENT_BROWSER_EXTENSIONS", extensions.join(","));
        }

        if let Some(a) = args {
            cmd.env("AGENT_BROWSER_ARGS", a);
        }

        if let Some(ua) = user_agent {
            cmd.env("AGENT_BROWSER_USER_AGENT", ua);
        }

        if let Some(p) = proxy {
            cmd.env("AGENT_BROWSER_PROXY", p);
        }

        if let Some(pb) = proxy_bypass {
            cmd.env("AGENT_BROWSER_PROXY_BYPASS", pb);
        }

        if ignore_https_errors {
            cmd.env("AGENT_BROWSER_IGNORE_HTTPS_ERRORS", "1");
        }

        if allow_file_access {
            cmd.env("AGENT_BROWSER_ALLOW_FILE_ACCESS", "1");
        }

        if let Some(st) = state {
            cmd.env("AGENT_BROWSER_STATE", st);
        }

        if let Some(p) = provider {
            cmd.env("AGENT_BROWSER_PROVIDER", p);
        }

        if let Some(d) = device {
            cmd.env("AGENT_BROWSER_IOS_DEVICE", d);
        }

        if let Some(sn) = session_name {
            cmd.env("AGENT_BROWSER_SESSION_NAME", sn);
        }

        cmd.env("AGENT_BROWSER_STEALTH", "1");
        if debug {
            cmd.env("AGENT_BROWSER_DEBUG", "1");
        }
        if let Some(dp) = download_path {
            cmd.env("AGENT_BROWSER_DOWNLOAD_PATH", dp);
        }
        if let Some(tg) = tab_group {
            cmd.env("AGENT_BROWSER_TAB_GROUP", tg);
        }
        if let Some(plugin_id) = tab_group_plugin_id {
            cmd.env("AGENT_BROWSER_TAB_GROUP_PLUGIN_ID", plugin_id);
        }

        // Create new process group and session to fully detach
        unsafe {
            cmd.pre_exec(|| {
                // Create new session (detach from terminal)
                libc::setsid();
                Ok(())
            });
        }

        daemon_child = Some(
            cmd.stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()
                .map_err(|e| format!("Failed to start daemon: {}", e))?,
        );
    }

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;

        // On Windows, call node directly. Command::new handles PATH resolution (node.exe or node.cmd)
        // and automatically quotes arguments containing spaces.
        let mut cmd = Command::new("node");
        cmd.arg(&daemon_path)
            .arg(if resident {
                "--resident"
            } else {
                "--idle-auto-shutdown"
            })
            .env("AGENT_BROWSER_DAEMON", "1")
            .env("AGENT_BROWSER_SESSION", session)
            .env("AGENT_BROWSER_CLI_VERSION", env!("CARGO_PKG_VERSION"));

        if headed {
            cmd.env("AGENT_BROWSER_HEADED", "1");
        }

        if let Some(path) = executable_path {
            cmd.env("AGENT_BROWSER_EXECUTABLE_PATH", path);
        }

        if !extensions.is_empty() {
            cmd.env("AGENT_BROWSER_EXTENSIONS", extensions.join(","));
        }

        if let Some(a) = args {
            cmd.env("AGENT_BROWSER_ARGS", a);
        }

        if let Some(ua) = user_agent {
            cmd.env("AGENT_BROWSER_USER_AGENT", ua);
        }

        if let Some(p) = proxy {
            cmd.env("AGENT_BROWSER_PROXY", p);
        }

        if let Some(pb) = proxy_bypass {
            cmd.env("AGENT_BROWSER_PROXY_BYPASS", pb);
        }

        if ignore_https_errors {
            cmd.env("AGENT_BROWSER_IGNORE_HTTPS_ERRORS", "1");
        }

        if allow_file_access {
            cmd.env("AGENT_BROWSER_ALLOW_FILE_ACCESS", "1");
        }

        if let Some(st) = state {
            cmd.env("AGENT_BROWSER_STATE", st);
        }

        if let Some(p) = provider {
            cmd.env("AGENT_BROWSER_PROVIDER", p);
        }

        if let Some(d) = device {
            cmd.env("AGENT_BROWSER_IOS_DEVICE", d);
        }

        if let Some(sn) = session_name {
            cmd.env("AGENT_BROWSER_SESSION_NAME", sn);
        }

        cmd.env("AGENT_BROWSER_STEALTH", "1");
        if debug {
            cmd.env("AGENT_BROWSER_DEBUG", "1");
        }
        if let Some(dp) = download_path {
            cmd.env("AGENT_BROWSER_DOWNLOAD_PATH", dp);
        }
        if let Some(tg) = tab_group {
            cmd.env("AGENT_BROWSER_TAB_GROUP", tg);
        }
        if let Some(plugin_id) = tab_group_plugin_id {
            cmd.env("AGENT_BROWSER_TAB_GROUP_PLUGIN_ID", plugin_id);
        }

        // CREATE_NEW_PROCESS_GROUP | DETACHED_PROCESS
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x00000200;
        const DETACHED_PROCESS: u32 = 0x00000008;

        daemon_child = Some(
            cmd.creation_flags(CREATE_NEW_PROCESS_GROUP | DETACHED_PROCESS)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()
                .map_err(|e| format!("Failed to start daemon: {}", e))?,
        );
    }

    for _ in 0..50 {
        if daemon_ready(session) {
            return Ok(DaemonResult {
                already_running: false,
            });
        }

        // Surface daemon startup stderr instead of returning an opaque timeout.
        if let Some(ref mut child) = daemon_child {
            if let Ok(Some(_)) = child.try_wait() {
                let mut stderr_output = String::new();
                if let Some(mut stderr) = child.stderr.take() {
                    let _ = stderr.read_to_string(&mut stderr_output);
                }
                let stderr_trimmed = stderr_output.trim();
                if !stderr_trimmed.is_empty() {
                    return Err(format!("Daemon failed to start: {}", stderr_trimmed));
                }
                return Err("Daemon failed to start: process exited during startup".to_string());
            }
        }

        thread::sleep(Duration::from_millis(100));
    }

    Err(format!(
        "Daemon failed to start (socket: {})",
        get_socket_dir().join(format!("{}.sock", session)).display()
    ))
}

fn connect(session: &str) -> Result<Connection, String> {
    #[cfg(unix)]
    {
        let socket_path = get_socket_path(session);
        UnixStream::connect(&socket_path)
            .map(Connection::Unix)
            .map_err(|e| format!("Failed to connect: {}", e))
    }
    #[cfg(windows)]
    {
        let port = get_port_for_session(session);
        TcpStream::connect(format!("127.0.0.1:{}", port))
            .map(Connection::Tcp)
            .map_err(|e| format!("Failed to connect: {}", e))
    }
}

pub fn send_command(cmd: Value, session: &str) -> Result<Response, String> {
    // Retry logic for transient errors (EAGAIN/EWOULDBLOCK/connection issues)
    const MAX_RETRIES: u32 = 5;
    const RETRY_DELAY_MS: u64 = 200;

    let mut last_error = String::new();

    for attempt in 0..MAX_RETRIES {
        if attempt > 0 {
            thread::sleep(Duration::from_millis(RETRY_DELAY_MS * (attempt as u64)));
        }

        match send_command_once(&cmd, session) {
            Ok(response) => return Ok(response),
            Err(e) => {
                if is_transient_error(&e) {
                    last_error = e;
                    continue;
                }
                // Non-transient error, fail immediately
                return Err(e);
            }
        }
    }

    Err(format!(
        "{} (after {} retries - daemon may be busy or unresponsive)",
        last_error, MAX_RETRIES
    ))
}

/// Check if an error is transient and worth retrying.
/// Transient errors include:
/// - EAGAIN/EWOULDBLOCK (os error 35 on macOS, 11 on Linux)
/// - EOF errors (daemon closed connection before responding)
/// - Connection reset/broken pipe (daemon crashed or restarting)
/// - Connection refused/socket not found (daemon still starting)
fn is_transient_error(error: &str) -> bool {
    error.contains("os error 35") // EAGAIN on macOS
        || error.contains("os error 11") // EAGAIN on Linux
        || error.contains("WouldBlock")
        || error.contains("Resource temporarily unavailable")
        || error.contains("EOF")
        || error.contains("line 1 column 0") // Empty JSON response
        || error.contains("Connection reset")
        || error.contains("Broken pipe")
        || error.contains("os error 54") // Connection reset by peer (macOS)
        || error.contains("os error 104") // Connection reset by peer (Linux)
        || error.contains("os error 2") // No such file or directory (socket gone)
        || error.contains("os error 61") // Connection refused (macOS)
        || error.contains("os error 111") // Connection refused (Linux)
}

fn send_command_once(cmd: &Value, session: &str) -> Result<Response, String> {
    let mut stream = connect(session)?;

    stream.set_read_timeout(Some(Duration::from_secs(30))).ok();
    stream.set_write_timeout(Some(Duration::from_secs(5))).ok();

    let mut json_str = serde_json::to_string(cmd).map_err(|e| e.to_string())?;
    json_str.push('\n');

    stream
        .write_all(json_str.as_bytes())
        .map_err(|e| format!("Failed to send: {}", e))?;

    let mut reader = BufReader::new(stream);
    let mut response_line = String::new();
    reader
        .read_line(&mut response_line)
        .map_err(|e| format!("Failed to read: {}", e))?;

    serde_json::from_str(&response_line).map_err(|e| format!("Invalid response: {}", e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::EnvGuard;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn test_temp_dir(prefix: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        env::temp_dir().join(format!("{}-{}-{}", prefix, std::process::id(), nonce))
    }

    #[test]
    fn test_get_socket_dir_explicit_override() {
        let _guard = EnvGuard::new(&["AGENT_BROWSER_SOCKET_DIR", "XDG_RUNTIME_DIR"]);

        _guard.set("AGENT_BROWSER_SOCKET_DIR", "/custom/socket/path");
        _guard.remove("XDG_RUNTIME_DIR");

        assert_eq!(get_socket_dir(), PathBuf::from("/custom/socket/path"));
    }

    #[test]
    fn test_get_socket_dir_ignores_empty_socket_dir() {
        let _guard = EnvGuard::new(&["AGENT_BROWSER_SOCKET_DIR", "XDG_RUNTIME_DIR"]);

        _guard.set("AGENT_BROWSER_SOCKET_DIR", "");
        _guard.remove("XDG_RUNTIME_DIR");

        assert!(get_socket_dir()
            .to_string_lossy()
            .ends_with(".agent-browser"));
    }

    #[test]
    fn test_get_socket_dir_xdg_runtime() {
        let _guard = EnvGuard::new(&["AGENT_BROWSER_SOCKET_DIR", "XDG_RUNTIME_DIR"]);

        _guard.remove("AGENT_BROWSER_SOCKET_DIR");
        _guard.set("XDG_RUNTIME_DIR", "/run/user/1000");

        assert_eq!(
            get_socket_dir(),
            PathBuf::from("/run/user/1000/agent-browser")
        );
    }

    #[test]
    fn test_get_socket_dir_ignores_empty_xdg_runtime() {
        let _guard = EnvGuard::new(&["AGENT_BROWSER_SOCKET_DIR", "XDG_RUNTIME_DIR"]);

        _guard.set("AGENT_BROWSER_SOCKET_DIR", "");
        _guard.set("XDG_RUNTIME_DIR", "");

        assert!(get_socket_dir()
            .to_string_lossy()
            .ends_with(".agent-browser"));
    }

    #[test]
    fn test_get_socket_dir_home_fallback() {
        let _guard = EnvGuard::new(&["AGENT_BROWSER_SOCKET_DIR", "XDG_RUNTIME_DIR"]);

        _guard.remove("AGENT_BROWSER_SOCKET_DIR");
        _guard.remove("XDG_RUNTIME_DIR");

        let result = get_socket_dir();
        assert!(result.to_string_lossy().ends_with(".agent-browser"));
        assert!(
            result.to_string_lossy().contains("home") || result.to_string_lossy().contains("Users")
        );
    }

    #[test]
    fn test_should_reap_for_default_policy() {
        assert!(!should_reap_for_default("default"));
        assert!(should_reap_for_default("parallel-worker-a"));
        assert!(should_reap_for_default("legacy-session"));
    }

    #[test]
    fn test_reap_sessions_for_default_start_removes_non_default_artifacts() {
        let _guard = EnvGuard::new(&["AGENT_BROWSER_SOCKET_DIR"]);
        let dir = test_temp_dir("agent-browser-reap");
        fs::create_dir_all(&dir).unwrap();
        _guard.set("AGENT_BROWSER_SOCKET_DIR", dir.to_string_lossy().as_ref());

        fs::write(dir.join("default.pid"), "999999").unwrap();
        fs::write(dir.join("parallel-a.pid"), "999999").unwrap();
        fs::write(dir.join("parallel-a.meta.json"), "{}").unwrap();
        fs::write(dir.join("legacy-x.pid"), "not-a-pid").unwrap();
        fs::write(dir.join("legacy-x.meta.json"), "{}").unwrap();
        #[cfg(unix)]
        {
            fs::write(dir.join("parallel-a.sock"), "").unwrap();
            fs::write(dir.join("legacy-x.sock"), "").unwrap();
        }
        #[cfg(windows)]
        {
            fs::write(dir.join("parallel-a.port"), "").unwrap();
            fs::write(dir.join("legacy-x.port"), "").unwrap();
        }

        reap_sessions_for_default_start();

        assert!(dir.join("default.pid").exists());
        assert!(!dir.join("parallel-a.pid").exists());
        assert!(!dir.join("parallel-a.meta.json").exists());
        assert!(!dir.join("legacy-x.pid").exists());
        assert!(!dir.join("legacy-x.meta.json").exists());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_remove_session_artifacts_cleans_all_known_files() {
        let _guard = EnvGuard::new(&["AGENT_BROWSER_SOCKET_DIR"]);
        let dir = test_temp_dir("agent-browser-clean-artifacts");
        fs::create_dir_all(&dir).unwrap();
        _guard.set("AGENT_BROWSER_SOCKET_DIR", dir.to_string_lossy().as_ref());

        let session = "legacy-x";
        fs::write(dir.join(format!("{}.pid", session)), "999999").unwrap();
        fs::write(dir.join(format!("{}.stream", session)), "35555").unwrap();
        fs::write(dir.join(format!("{}.meta.json", session)), "{}").unwrap();
        #[cfg(unix)]
        fs::write(dir.join(format!("{}.sock", session)), "").unwrap();
        #[cfg(windows)]
        fs::write(dir.join(format!("{}.port", session)), "45555").unwrap();

        remove_session_artifacts(session);

        assert!(!dir.join(format!("{}.pid", session)).exists());
        assert!(!dir.join(format!("{}.stream", session)).exists());
        assert!(!dir.join(format!("{}.meta.json", session)).exists());
        #[cfg(unix)]
        assert!(!dir.join(format!("{}.sock", session)).exists());
        #[cfg(windows)]
        assert!(!dir.join(format!("{}.port", session)).exists());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_validate_default_daemon_identity_match() {
        let _guard = EnvGuard::new(&["AGENT_BROWSER_SOCKET_DIR"]);
        let dir = test_temp_dir("agent-browser-meta-ok");
        fs::create_dir_all(&dir).unwrap();
        _guard.set("AGENT_BROWSER_SOCKET_DIR", dir.to_string_lossy().as_ref());

        let daemon_path = dir.join("daemon.js");
        fs::write(&daemon_path, "// test").unwrap();
        let canonical = daemon_path.canonicalize().unwrap();
        let meta = serde_json::json!({
            "cliVersion": env!("CARGO_PKG_VERSION"),
            "daemonPath": canonical.to_string_lossy(),
        });
        fs::write(dir.join("default.meta.json"), meta.to_string()).unwrap();

        assert!(validate_default_daemon_identity(&daemon_path));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_validate_default_daemon_identity_version_mismatch() {
        let _guard = EnvGuard::new(&["AGENT_BROWSER_SOCKET_DIR"]);
        let dir = test_temp_dir("agent-browser-meta-bad");
        fs::create_dir_all(&dir).unwrap();
        _guard.set("AGENT_BROWSER_SOCKET_DIR", dir.to_string_lossy().as_ref());

        let daemon_path = dir.join("daemon.js");
        fs::write(&daemon_path, "// test").unwrap();
        let canonical = daemon_path.canonicalize().unwrap();
        let meta = serde_json::json!({
            "cliVersion": "0.0.0-fork.0",
            "daemonPath": canonical.to_string_lossy(),
        });
        fs::write(dir.join("default.meta.json"), meta.to_string()).unwrap();

        assert!(!validate_default_daemon_identity(&daemon_path));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_validate_default_daemon_identity_path_mismatch() {
        let _guard = EnvGuard::new(&["AGENT_BROWSER_SOCKET_DIR"]);
        let dir = test_temp_dir("agent-browser-meta-path-mismatch");
        fs::create_dir_all(&dir).unwrap();
        _guard.set("AGENT_BROWSER_SOCKET_DIR", dir.to_string_lossy().as_ref());

        let daemon_path = dir.join("daemon.js");
        let other_path = dir.join("daemon-other.js");
        fs::write(&daemon_path, "// test").unwrap();
        fs::write(&other_path, "// other").unwrap();
        let other_canonical = other_path.canonicalize().unwrap();
        let meta = serde_json::json!({
            "cliVersion": env!("CARGO_PKG_VERSION"),
            "daemonPath": other_canonical.to_string_lossy(),
        });
        fs::write(dir.join("default.meta.json"), meta.to_string()).unwrap();

        assert!(!validate_default_daemon_identity(&daemon_path));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_validate_default_daemon_identity_missing_meta() {
        let _guard = EnvGuard::new(&["AGENT_BROWSER_SOCKET_DIR"]);
        let dir = test_temp_dir("agent-browser-meta-missing");
        fs::create_dir_all(&dir).unwrap();
        _guard.set("AGENT_BROWSER_SOCKET_DIR", dir.to_string_lossy().as_ref());

        let daemon_path = dir.join("daemon.js");
        fs::write(&daemon_path, "// test").unwrap();

        assert!(!validate_default_daemon_identity(&daemon_path));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_validate_default_daemon_identity_bad_json() {
        let _guard = EnvGuard::new(&["AGENT_BROWSER_SOCKET_DIR"]);
        let dir = test_temp_dir("agent-browser-meta-bad-json");
        fs::create_dir_all(&dir).unwrap();
        _guard.set("AGENT_BROWSER_SOCKET_DIR", dir.to_string_lossy().as_ref());

        let daemon_path = dir.join("daemon.js");
        fs::write(&daemon_path, "// test").unwrap();
        fs::write(dir.join("default.meta.json"), "{invalid-json").unwrap();

        assert!(!validate_default_daemon_identity(&daemon_path));
        let _ = fs::remove_dir_all(&dir);
    }

    // === Transient Error Detection Tests ===

    #[test]
    fn test_is_transient_error_eagain_macos() {
        assert!(is_transient_error(
            "Failed to read: Resource temporarily unavailable (os error 35)"
        ));
    }

    #[test]
    fn test_is_transient_error_eagain_linux() {
        assert!(is_transient_error(
            "Failed to read: Resource temporarily unavailable (os error 11)"
        ));
    }

    #[test]
    fn test_is_transient_error_would_block() {
        assert!(is_transient_error("operation WouldBlock"));
    }

    #[test]
    fn test_is_transient_error_resource_unavailable() {
        assert!(is_transient_error("Resource temporarily unavailable"));
    }

    #[test]
    fn test_is_transient_error_eof() {
        assert!(is_transient_error(
            "Invalid response: EOF while parsing a value at line 1 column 0"
        ));
    }

    #[test]
    fn test_is_transient_error_empty_json() {
        assert!(is_transient_error(
            "Invalid response: expected value at line 1 column 0"
        ));
    }

    #[test]
    fn test_is_transient_error_connection_reset() {
        assert!(is_transient_error("Connection reset by peer"));
    }

    #[test]
    fn test_is_transient_error_broken_pipe() {
        assert!(is_transient_error("Broken pipe"));
    }

    #[test]
    fn test_is_transient_error_connection_reset_macos() {
        assert!(is_transient_error(
            "Failed to send: Connection reset by peer (os error 54)"
        ));
    }

    #[test]
    fn test_is_transient_error_connection_reset_linux() {
        assert!(is_transient_error(
            "Failed to send: Connection reset by peer (os error 104)"
        ));
    }

    #[test]
    fn test_is_transient_error_socket_not_found() {
        assert!(is_transient_error(
            "Failed to connect: No such file or directory (os error 2)"
        ));
    }

    #[test]
    fn test_is_transient_error_connection_refused_macos() {
        assert!(is_transient_error(
            "Failed to connect: Connection refused (os error 61)"
        ));
    }

    #[test]
    fn test_is_transient_error_connection_refused_linux() {
        assert!(is_transient_error(
            "Failed to connect: Connection refused (os error 111)"
        ));
    }

    #[test]
    fn test_is_transient_error_non_transient() {
        // These should NOT be considered transient
        assert!(!is_transient_error("Unknown command: foo"));
        assert!(!is_transient_error("Invalid JSON syntax"));
        assert!(!is_transient_error("Permission denied"));
        assert!(!is_transient_error("Daemon not found"));
    }
}

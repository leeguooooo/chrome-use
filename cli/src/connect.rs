//! `agent-browser connect` — zero-confirmation control of the user's real,
//! logged-in Chrome via the `ab-connect` MV3 extension over Chrome **native
//! messaging** (no localhost port, no token; Chrome authenticates the extension
//! to this host by id).
//!
//! Two pieces live here:
//! - `run_connect` — `--install` writes the native-messaging host manifest (and
//!   a tiny launcher) so Chrome will spawn us; with no flag it reports status.
//! - `run_nm_host` — the hidden `__nm-host` mode Chrome launches: it speaks the
//!   native-messaging stdio framing (4-byte little-endian length + JSON).
//!
//! This step wires the transport end-to-end (Chrome ⇄ host). Bridging the host
//! to the daemon's relay + CdpClient is layered on next.

use std::io::{Read, Write};
use std::path::PathBuf;

/// Native-messaging host name; must match `HOST_NAME` in the extension and the
/// manifest filename.
pub const HOST_NAME: &str = "com.agent_browser.connect";

/// Stable id of the `ab-connect` extension, pinned by the `key` in its
/// manifest.json. Chrome only lets that extension talk to this host.
pub const EXTENSION_ID: &str = "bdoiejojpjogcjojeladhioioijhgade";

/// `agent-browser extension <install|uninstall|status>` (local; no daemon).
/// `args` is the cleaned argv including the leading "extension".
pub fn run_connect(args: &[String], json: bool) {
    let install = args.iter().any(|a| a == "--install" || a == "install");
    let uninstall = args.iter().any(|a| a == "--uninstall" || a == "uninstall");

    if uninstall {
        let removed = remove_host_manifests();
        report(json, true, &format!("removed {removed} native-host manifest(s)"));
        return;
    }
    if install {
        match install_native_host() {
            Ok(paths) => {
                if json {
                    println!(
                        "{}",
                        serde_json::to_string(&serde_json::json!({
                            "success": true,
                            "data": { "installed": paths, "extensionId": EXTENSION_ID }
                        }))
                        .unwrap_or_default()
                    );
                } else {
                    println!("✓ native-messaging host installed:");
                    for p in &paths {
                        println!("  {p}");
                    }
                    println!(
                        "\nNext: load the ab-connect extension in Chrome (chrome://extensions →\n\
                         Developer mode → Load unpacked → extensions/ab-connect), then this host\n\
                         is reachable with no token and no per-use confirmation."
                    );
                }
            }
            Err(e) => report(json, false, &format!("install failed: {e}")),
        }
        return;
    }

    // Status.
    let manifest = host_manifest_path_for_chrome();
    let installed = manifest.as_ref().map(|p| p.exists()).unwrap_or(false);
    if json {
        println!(
            "{}",
            serde_json::to_string(&serde_json::json!({
                "success": true,
                "data": {
                    "installed": installed,
                    "manifest": manifest.as_ref().map(|p| p.display().to_string()),
                    "extensionId": EXTENSION_ID,
                }
            }))
            .unwrap_or_default()
        );
    } else if installed {
        println!("✓ native-messaging host installed ({HOST_NAME}).");
        println!("  Load the ab-connect extension and it connects automatically.");
    } else {
        println!("✗ not installed. Run: agent-browser connect --install");
    }
}

/// Write the launcher script + native-messaging host manifest(s).
fn install_native_host() -> Result<Vec<String>, String> {
    let home = dirs::home_dir().ok_or("no home dir")?;
    let ab_dir = home.join(".agent-browser");
    std::fs::create_dir_all(&ab_dir).map_err(|e| e.to_string())?;

    // Chrome execs the manifest `path` directly with the calling extension's
    // origin as argv[1]; a launcher lets us run the binary in __nm-host mode
    // regardless of how/where agent-browser is installed.
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let launcher = ab_dir.join("nm-host.sh");
    let script = format!(
        "#!/bin/sh\n# agent-browser native-messaging host launcher (auto-generated)\nexec \"{}\" __nm-host \"$@\"\n",
        exe.display()
    );
    std::fs::write(&launcher, script).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o755));
    }

    let manifest = serde_json::json!({
        "name": HOST_NAME,
        "description": "agent-browser connect — native messaging host",
        "path": launcher.display().to_string(),
        "type": "stdio",
        "allowed_origins": [format!("chrome-extension://{EXTENSION_ID}/")],
    });
    let body = serde_json::to_string_pretty(&manifest).map_err(|e| e.to_string())?;

    let mut written = Vec::new();
    for dir in native_messaging_dirs() {
        if let Some(parent) = dir.parent() {
            if !parent.exists() {
                continue; // that browser isn't installed
            }
        }
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let path = dir.join(format!("{HOST_NAME}.json"));
        std::fs::write(&path, &body).map_err(|e| e.to_string())?;
        written.push(path.display().to_string());
    }
    if written.is_empty() {
        return Err("no Chrome/Chromium NativeMessagingHosts directory found".into());
    }
    Ok(written)
}

fn remove_host_manifests() -> usize {
    let mut n = 0;
    for dir in native_messaging_dirs() {
        let path = dir.join(format!("{HOST_NAME}.json"));
        if path.exists() && std::fs::remove_file(&path).is_ok() {
            n += 1;
        }
    }
    n
}

/// Per-OS NativeMessagingHosts directories for Chrome + Chromium-family browsers.
fn native_messaging_dirs() -> Vec<PathBuf> {
    let mut dirs_out = Vec::new();
    #[cfg(target_os = "macos")]
    {
        if let Some(app_support) = dirs::config_dir() {
            for sub in [
                "Google/Chrome",
                "Google/Chrome Beta",
                "Google/Chrome Canary",
                "Chromium",
                "Microsoft Edge",
                "BraveSoftware/Brave-Browser",
            ] {
                dirs_out.push(app_support.join(sub).join("NativeMessagingHosts"));
            }
        }
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        if let Some(config) = dirs::config_dir() {
            for sub in ["google-chrome", "chromium", "microsoft-edge", "BraveSoftware/Brave-Browser"] {
                dirs_out.push(config.join(sub).join("NativeMessagingHosts"));
            }
        }
    }
    dirs_out
}

fn host_manifest_path_for_chrome() -> Option<PathBuf> {
    native_messaging_dirs()
        .into_iter()
        .map(|d| d.join(format!("{HOST_NAME}.json")))
        .find(|p| p.exists())
        .or_else(|| {
            native_messaging_dirs()
                .into_iter()
                .next()
                .map(|d| d.join(format!("{HOST_NAME}.json")))
        })
}

fn report(json: bool, ok: bool, msg: &str) {
    if json {
        println!(
            "{}",
            serde_json::to_string(&serde_json::json!({ "success": ok, "error": if ok { serde_json::Value::Null } else { serde_json::json!(msg) }, "message": msg }))
                .unwrap_or_default()
        );
    } else if ok {
        println!("✓ {msg}");
    } else {
        eprintln!("✗ {msg}");
    }
    if !ok {
        std::process::exit(1);
    }
}

// ---- native messaging host (`__nm-host`) ----------------------------------

/// Read one native-messaging frame from stdin: 4-byte native-endian length,
/// then that many bytes of UTF-8 JSON. Returns `None` on clean EOF.
fn read_frame(stdin: &mut impl Read) -> std::io::Result<Option<Vec<u8>>> {
    let mut len_buf = [0u8; 4];
    match stdin.read_exact(&mut len_buf) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let len = u32::from_ne_bytes(len_buf) as usize;
    let mut buf = vec![0u8; len];
    stdin.read_exact(&mut buf)?;
    Ok(Some(buf))
}

/// Write one native-messaging frame to stdout.
fn write_frame(stdout: &mut impl Write, payload: &[u8]) -> std::io::Result<()> {
    let len = payload.len() as u32;
    stdout.write_all(&len.to_ne_bytes())?;
    stdout.write_all(payload)?;
    stdout.flush()
}

/// Hidden `__nm-host` mode: launched by Chrome for the ab-connect extension.
///
/// Step 1 (this commit): speak the framing correctly and log what the extension
/// sends to `~/.agent-browser/nm-host.log`, replying `pong` to `ping`. The next
/// step bridges these frames to the daemon's relay + CdpClient.
pub fn run_nm_host() {
    let log_path = dirs::home_dir()
        .map(|h| h.join(".agent-browser").join("nm-host.log"))
        .unwrap_or_else(|| PathBuf::from("/tmp/ab-nm-host.log"));
    if let Some(p) = log_path.parent() {
        let _ = std::fs::create_dir_all(p);
    }
    let mut log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .ok();
    let mut logln = |s: &str| {
        if let Some(f) = log.as_mut() {
            let _ = writeln!(f, "{s}");
            let _ = f.flush();
        }
    };
    logln(&format!(
        "[nm-host] started argv={:?}",
        std::env::args().skip(1).collect::<Vec<_>>()
    ));

    let mut stdin = std::io::stdin().lock();
    let mut stdout = std::io::stdout().lock();
    let mut count = 0usize;
    loop {
        match read_frame(&mut stdin) {
            Ok(Some(bytes)) => {
                count += 1;
                let text = String::from_utf8_lossy(&bytes);
                // Log a compact summary (method + sizes) without flooding.
                let summary: String = text.chars().take(300).collect();
                logln(&format!("[nm-host] recv #{count} ({} bytes): {summary}", bytes.len()));
                if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                    if v.get("method").and_then(|m| m.as_str()) == Some("ping") {
                        let _ = write_frame(&mut stdout, br#"{"method":"pong"}"#);
                    }
                }
            }
            Ok(None) => {
                logln("[nm-host] stdin EOF — Chrome closed the port");
                break;
            }
            Err(e) => {
                logln(&format!("[nm-host] read error: {e}"));
                break;
            }
        }
    }
}

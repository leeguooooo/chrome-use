//! `chrome-use connect` — zero-confirmation control of the user's real,
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

use std::io::Write;
use std::path::{Path, PathBuf};

/// Native-messaging host name; must match `HOST_NAME` in the extension and the
/// manifest filename. `com.agent_browser.connect` is the original name, used by
/// every shipped extension up to ab-connect 0.4.2.
pub const HOST_NAME: &str = "com.agent_browser.connect";

/// Alternate host name for the chrome-use rebrand era (ab-connect 0.5.0+). We
/// install AND recognize both names so the relay works regardless of which
/// extension version a user has — old (0.4.2) or new — with no forced
/// re-install. See [`install_native_host`] / [`host_installed`].
pub const HOST_NAME_ALT: &str = "com.leeguoo.chrome_use";

/// Every native-messaging host name this CLI installs and accepts.
pub const HOST_NAMES: &[&str] = &[HOST_NAME, HOST_NAME_ALT];

/// Stable id of the `ab-connect` extension, pinned by the `key` in its
/// manifest.json (and the signing key of the published `.crx`). Chrome only lets
/// that extension talk to this host, and the force-install policy references it.
pub const EXTENSION_ID: &str = "ciiljdlhdpfckdcfkphgmfalanpdejep";

/// The Chrome Web Store assigns its own id (the manifest "key" is stripped from
/// store uploads), so the published build has a different origin than the local
/// Load-unpacked one. Allow both to talk to the native-messaging host.
pub const STORE_EXTENSION_ID: &str = "knfcmbamhjmaonkfnjhldjedeobeafmk";

/// Update URL the force-install policy points at. MUST be the Chrome Web Store
/// endpoint: Chrome 149 tags any **off-Web-Store** force-installed extension
/// `[BLOCKED]` on an unmanaged browser (verified on macOS — chrome://policy shows
/// `[BLOCKED]…` / "Error, Warning"). Self-hosting a `.crx` therefore does NOT
/// work on consumer Chrome; the extension must be published to the Web Store, and
/// then this policy force-installs it silently (Web Store extensions are allowed).
pub const UPDATE_URL: &str = "https://clients2.google.com/service/update2/crx";

/// Public Web Store listing — the guaranteed one-click "Add to Chrome" path,
/// and the fallback when the force-install profile can't be approved headlessly.
pub const STORE_URL: &str =
    "https://chromewebstore.google.com/detail/ciiljdlhdpfckdcfkphgmfalanpdejep";

/// The published Store listing for the **Store build** (`STORE_EXTENSION_ID`) —
/// the page we auto-open when the relay can't be reached and the extension isn't
/// set up yet, so the user just clicks "Add to Chrome".
pub const STORE_INSTALL_URL: &str =
    "https://chromewebstore.google.com/detail/chrome-use/knfcmbamhjmaonkfnjhldjedeobeafmk?utm_source=cli-autoheal";

/// Stable identifiers for the generated Chrome configuration profile, so a
/// re-install replaces (rather than duplicates) it in System Settings.
const PROFILE_ID: &str = "work.pwtk.chrome-use.ab-connect";
const PROFILE_UUID: &str = "A1B2C3D4-AB00-4CCE-9E10-AAAABBBBCCCC";
const PROFILE_PAYLOAD_UUID: &str = "A1B2C3D4-AB01-4CCE-9E10-DDDDEEEEFFFF";

/// `chrome-use extension <install|uninstall|status>` (local; no daemon).
/// `args` is the cleaned argv including the leading "extension".
pub fn run_connect(args: &[String], json: bool) {
    let install = args.iter().any(|a| a == "--install" || a == "install");
    let uninstall = args.iter().any(|a| a == "--uninstall" || a == "uninstall");

    if uninstall {
        let removed = remove_host_manifests();
        let profile_removed = remove_force_install_profile();
        if json {
            report(
                json,
                true,
                &format!("removed {removed} native-host manifest(s)"),
            );
        } else {
            println!("✓ removed {removed} native-host manifest(s).");
            if profile_removed {
                println!("✓ removed ~/.chrome-use/ab-connect.mobileconfig");
            }
            if cfg!(target_os = "macos") {
                println!(
                    "  To fully remove the extension, delete the \"chrome-use connect\" profile\n\
                     in System Settings → Profiles (or run: profiles remove -identifier {PROFILE_ID})."
                );
            }
        }
        return;
    }
    if install {
        let no_open = args.iter().any(|a| a == "--no-open");
        match install_native_host() {
            Ok(paths) => {
                let profile = install_force_install_profile(no_open);
                if json {
                    println!(
                        "{}",
                        serde_json::to_string(&serde_json::json!({
                            "success": true,
                            "data": {
                                "installed": paths,
                                "extensionId": EXTENSION_ID,
                                "profile": profile.as_ref().ok().map(|p| p.display().to_string()),
                                "profileError": profile.as_ref().err(),
                                "updateUrl": UPDATE_URL,
                            }
                        }))
                        .unwrap_or_default()
                    );
                } else {
                    println!("✓ native-messaging host installed:");
                    for p in &paths {
                        println!("  {p}");
                    }
                    match profile {
                        Ok(path) => {
                            println!(
                                "\n✓ Chrome force-install profile written:\n  {}",
                                path.display()
                            );
                            if cfg!(target_os = "macos") {
                                println!(
                                    "\nGet the extension into Chrome (one-time). Either:\n\
                                     A) One click: open {STORE_URL}\n   and press \"Add to Chrome\".\n\
                                     B) Silent: approve the profile, then restart Chrome —\n   \
                                     System Settings → General → Device Management → double-click\n   \
                                     \"chrome-use connect\" → Install. Chrome then force-installs +\n   \
                                     auto-updates it (no token, no per-use confirmation).\n\
                                     Both need the extension published to the Web Store; until then use\n   \
                                     chrome://extensions → Developer mode → Load unpacked → extensions/ab-connect."
                                );
                            }
                        }
                        Err(e) => {
                            println!("\n! could not write the force-install profile: {e}");
                            println!(
                                "  Fallback: load extensions/ab-connect via chrome://extensions →\n\
                                 Developer mode → Load unpacked."
                            );
                        }
                    }
                }
            }
            Err(e) => report(json, false, &format!("install failed: {e}")),
        }
        return;
    }

    // Status.
    let manifests = installed_host_manifests();
    let installed = !manifests.is_empty();
    let extension_status = chrome_extension_status();
    let relay_url = relay_url();
    let live_extension_version = relay_ext_version();
    let expected_extension_version = env!("AB_CONNECT_VERSION");
    if json {
        println!(
            "{}",
            serde_json::to_string(&serde_json::json!({
                "success": true,
                "data": {
                    "installed": installed,
                    "manifests": manifests.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
                    "manifest": manifests.first().map(|p| p.display().to_string()),
                    "hostNames": HOST_NAMES,
                    "extensionIds": {
                        "webStore": STORE_EXTENSION_ID,
                        "unpacked": EXTENSION_ID,
                    },
                    "extensionId": EXTENSION_ID,
                    "expectedExtensionVersion": expected_extension_version,
                    "liveExtensionVersion": live_extension_version,
                    "relayUp": relay_url.is_some(),
                    "relayUrl": relay_url,
                    "chromeExtension": extension_status,
                }
            }))
            .unwrap_or_default()
        );
    } else if installed {
        println!("✓ native-messaging host installed ({HOST_NAME}).");
        for path in &manifests {
            println!("  {}", path.display());
        }
        println!("  Web Store extension id: {STORE_EXTENSION_ID}");
        println!("  unpacked/dev extension id: {EXTENSION_ID}");
        println!("  expected extension version: {expected_extension_version}");
        match live_extension_version {
            Some(ver) => println!("✓ live extension version: {ver}"),
            None => {
                println!("  live extension version: unknown (relay has not reported hello yet)")
            }
        }
        if relay_url.is_some() {
            println!("✓ extension relay (__nm-host): up");
        } else {
            println!("  extension relay (__nm-host): not currently connected");
        }
        if let Some(status) = extension_status {
            print_chrome_extension_status(&status, expected_extension_version);
        } else {
            println!("  Chrome profile extension status: not found in the default Chrome profile");
        }
    } else {
        println!("✗ not installed. Run: chrome-use connect --install");
    }
}

/// Write the launcher script + native-messaging host manifest(s).
fn install_native_host() -> Result<Vec<String>, String> {
    let home = dirs::home_dir().ok_or("no home dir")?;
    let ab_dir = home.join(".chrome-use");
    std::fs::create_dir_all(&ab_dir).map_err(|e| e.to_string())?;

    // Chrome execs the manifest `path` directly with the calling extension's
    // origin as argv[1]; a launcher lets us run the binary in __nm-host mode
    // regardless of how/where chrome-use is installed.
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let launcher = ab_dir.join("nm-host.sh");
    let script = format!(
        "#!/bin/sh\n# chrome-use native-messaging host launcher (auto-generated)\nexec \"{}\" __nm-host \"$@\"\n",
        exe.display()
    );
    std::fs::write(&launcher, script).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o755));
    }

    // Write a manifest under EVERY accepted host name (both point to the same
    // launcher + allowed extensions), so any extension version's
    // `connectNative(<its host name>)` finds a matching host json.
    let mut written = Vec::new();
    for dir in native_messaging_dirs() {
        if let Some(parent) = dir.parent() {
            if !parent.exists() {
                continue; // that browser isn't installed
            }
        }
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        for host in HOST_NAMES {
            let manifest = serde_json::json!({
                "name": host,
                "description": "chrome-use connect — native messaging host",
                "path": launcher.display().to_string(),
                "type": "stdio",
                "allowed_origins": [
                    format!("chrome-extension://{EXTENSION_ID}/"),
                    format!("chrome-extension://{STORE_EXTENSION_ID}/"),
                ],
            });
            let body = serde_json::to_string_pretty(&manifest).map_err(|e| e.to_string())?;
            let path = dir.join(format!("{host}.json"));
            std::fs::write(&path, &body).map_err(|e| e.to_string())?;
            written.push(path.display().to_string());
        }
    }
    if written.is_empty() {
        return Err("no Chrome/Chromium NativeMessagingHosts directory found".into());
    }
    Ok(written)
}

/// Write a Chrome configuration profile that force-installs `ab-connect` from
/// [`UPDATE_URL`], and (unless `no_open`) `open` it so the user approves it once
/// in System Settings. Returns the profile path. macOS only — elsewhere it
/// returns an error and the caller prints the manual fallback.
fn install_force_install_profile(no_open: bool) -> Result<PathBuf, String> {
    if !cfg!(target_os = "macos") {
        return Err("force-install profile is macOS-only; on Linux set Chrome's \
                    ExtensionInstallForcelist policy JSON, or Load unpacked from chrome://extensions"
            .into());
    }
    let home = dirs::home_dir().ok_or("no home dir")?;
    let ab_dir = home.join(".chrome-use");
    std::fs::create_dir_all(&ab_dir).map_err(|e| e.to_string())?;
    let path = ab_dir.join("ab-connect.mobileconfig");
    std::fs::write(&path, force_install_mobileconfig()).map_err(|e| e.to_string())?;
    if !no_open {
        // `open` queues the profile in System Settings for one-time approval.
        let _ = std::process::Command::new("open").arg(&path).status();
    }
    Ok(path)
}

/// The `.mobileconfig` payload: a user-scope Chrome policy that force-installs
/// the extension from the Chrome Web Store. User scope installs without admin —
/// just a one-time approval click. Must use the STORE id (the Web Store update
/// server serves the published extension under the id it assigned, not the local
/// Load-unpacked id).
fn force_install_mobileconfig() -> String {
    let forcelist = format!("{STORE_EXTENSION_ID};{UPDATE_URL}");
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>PayloadContent</key>
  <array>
    <dict>
      <key>PayloadType</key><string>com.google.Chrome</string>
      <key>PayloadVersion</key><integer>1</integer>
      <key>PayloadIdentifier</key><string>{PROFILE_ID}.chrome</string>
      <key>PayloadUUID</key><string>{PROFILE_PAYLOAD_UUID}</string>
      <key>PayloadEnabled</key><true/>
      <key>PayloadDisplayName</key><string>chrome-use connect (Chrome)</string>
      <key>ExtensionInstallForcelist</key>
      <array>
        <string>{forcelist}</string>
      </array>
    </dict>
  </array>
  <key>PayloadType</key><string>Configuration</string>
  <key>PayloadVersion</key><integer>1</integer>
  <key>PayloadIdentifier</key><string>{PROFILE_ID}</string>
  <key>PayloadUUID</key><string>{PROFILE_UUID}</string>
  <key>PayloadDisplayName</key><string>chrome-use connect</string>
  <key>PayloadDescription</key><string>Force-installs the chrome-use connect extension so chrome-use can drive your logged-in Chrome. No token, no per-use confirmation.</string>
  <key>PayloadOrganization</key><string>chrome-use</string>
  <key>PayloadScope</key><string>User</string>
  <key>PayloadRemovalDisallowed</key><false/>
</dict>
</plist>
"#
    )
}

/// Remove the generated `.mobileconfig` file (the profile itself is removed by
/// the user from System Settings, or via `profiles remove`).
fn remove_force_install_profile() -> bool {
    dirs::home_dir()
        .map(|h| h.join(".chrome-use").join("ab-connect.mobileconfig"))
        .filter(|p| p.exists())
        .map(|p| std::fs::remove_file(&p).is_ok())
        .unwrap_or(false)
}

fn remove_host_manifests() -> usize {
    let mut n = 0;
    for dir in native_messaging_dirs() {
        for host in HOST_NAMES {
            let path = dir.join(format!("{host}.json"));
            if path.exists() && std::fs::remove_file(&path).is_ok() {
                n += 1;
            }
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
            for sub in [
                "google-chrome",
                "chromium",
                "microsoft-edge",
                "BraveSoftware/Brave-Browser",
            ] {
                dirs_out.push(config.join(sub).join("NativeMessagingHosts"));
            }
        }
    }
    dirs_out
}

fn installed_host_manifests() -> Vec<PathBuf> {
    let mut out = Vec::new();
    for dir in native_messaging_dirs() {
        for host in HOST_NAMES {
            let path = dir.join(format!("{host}.json"));
            if path.exists() {
                out.push(path);
            }
        }
    }
    out
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ChromeExtensionStatus {
    id: String,
    name: Option<String>,
    version: Option<String>,
    path: Option<String>,
    profile_file: String,
    idle_version: Option<String>,
    idle_path: Option<String>,
    active_permissions: Vec<String>,
    disable_reasons: Vec<String>,
    active_bit: Option<bool>,
    from_webstore: Option<bool>,
}

fn chrome_profile_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(config) = dirs::config_dir() {
        #[cfg(target_os = "macos")]
        {
            for sub in [
                "Google/Chrome",
                "Google/Chrome Beta",
                "Google/Chrome Canary",
                "Chromium",
            ] {
                roots.push(config.join(sub));
            }
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            for sub in [
                "google-chrome",
                "google-chrome-beta",
                "google-chrome-unstable",
                "chromium",
            ] {
                roots.push(config.join(sub));
            }
        }
        #[cfg(target_os = "windows")]
        {
            roots.push(config.join("Google").join("Chrome").join("User Data"));
        }
    }
    roots
}

fn chrome_extension_status() -> Option<ChromeExtensionStatus> {
    for root in chrome_profile_roots() {
        for profile in ["Default", "Profile 1", "Profile 2", "Profile 3"] {
            for file in ["Secure Preferences", "Preferences"] {
                let path = root.join(profile).join(file);
                if let Some(status) = chrome_extension_status_from_file(&path) {
                    return Some(status);
                }
            }
        }
    }
    None
}

fn chrome_extension_status_from_file(path: &Path) -> Option<ChromeExtensionStatus> {
    let text = std::fs::read_to_string(path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    for id in [STORE_EXTENSION_ID, EXTENSION_ID] {
        if let Some(settings) = value
            .pointer(&format!("/extensions/settings/{id}"))
            .and_then(|v| v.as_object())
        {
            return Some(parse_chrome_extension_status(
                id,
                &serde_json::Value::Object(settings.clone()),
                path,
            ));
        }
    }
    None
}

fn parse_chrome_extension_status(
    id: &str,
    settings: &serde_json::Value,
    profile_file: &Path,
) -> ChromeExtensionStatus {
    let manifest = settings.get("manifest");
    let idle_manifest = settings.pointer("/idle_install_info/manifest");
    let active_permissions = settings
        .pointer("/active_permissions/api")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(ToString::to_string))
                .collect()
        })
        .unwrap_or_default();
    let disable_reasons = settings
        .get("disable_reasons")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| {
                    x.as_str()
                        .map(ToString::to_string)
                        .or_else(|| x.as_i64().map(|n| n.to_string()))
                })
                .collect()
        })
        .unwrap_or_default();
    ChromeExtensionStatus {
        id: id.to_string(),
        name: manifest
            .and_then(|m| m.get("name"))
            .and_then(|v| v.as_str())
            .map(ToString::to_string),
        version: manifest
            .and_then(|m| m.get("version"))
            .and_then(|v| v.as_str())
            .map(ToString::to_string),
        path: settings
            .get("path")
            .and_then(|v| v.as_str())
            .map(ToString::to_string),
        profile_file: profile_file.display().to_string(),
        idle_version: idle_manifest
            .and_then(|m| m.get("version"))
            .and_then(|v| v.as_str())
            .map(ToString::to_string),
        idle_path: settings
            .pointer("/idle_install_info/path")
            .and_then(|v| v.as_str())
            .map(ToString::to_string),
        active_permissions,
        disable_reasons,
        active_bit: settings.get("active_bit").and_then(|v| v.as_bool()),
        from_webstore: settings.get("from_webstore").and_then(|v| v.as_bool()),
    }
}

fn print_chrome_extension_status(status: &ChromeExtensionStatus, expected_version: &str) {
    let source = if status.id == STORE_EXTENSION_ID {
        "Web Store"
    } else {
        "unpacked/dev"
    };
    println!(
        "  Chrome profile extension: {} ({source}, id {})",
        status.name.as_deref().unwrap_or("unknown"),
        status.id
    );
    if let Some(ver) = &status.version {
        println!("  active manifest version: {ver}");
        if crate::upgrade::version_is_newer(expected_version, ver) {
            println!(
                "! active extension is older than bundled {expected_version}; open chrome://extensions, accept pending permissions/reload, or restart Chrome"
            );
        }
    }
    if let Some(idle) = &status.idle_version {
        if status.version.as_deref() != Some(idle.as_str()) {
            println!(
                "! Chrome has a pending extension update: active {} -> downloaded {idle}; restart Chrome or accept pending permissions",
                status.version.as_deref().unwrap_or("unknown")
            );
        }
    }
    if !status.disable_reasons.is_empty() {
        println!(
            "! extension has disable reasons: {} (open chrome://extensions/?id={} and accept permissions/enable it)",
            status.disable_reasons.join(", "),
            status.id
        );
    }
    if !status.active_permissions.is_empty() {
        println!(
            "  active permissions: {}",
            status.active_permissions.join(", ")
        );
    }
}

/// True if the ab-connect native-messaging host manifest is present — i.e. the
/// user has set up the extension path. When installed, auto-connect treats the
/// dialog-free extension relay as the *intended* transport and refuses to fall
/// back to a raw debug port (which would pop Chrome 136+'s "Allow remote
/// debugging?" consent modal). The relay-url file comes and goes with the
/// service worker; this manifest is the durable signal that the extension is
/// the chosen path.
pub fn host_installed() -> bool {
    native_messaging_dirs().into_iter().any(|d| {
        HOST_NAMES
            .iter()
            .any(|h| d.join(format!("{h}.json")).exists())
    })
}

/// Register the native-messaging host manifest if it isn't already, so the CLI
/// sets up its own half of the relay with **zero user action** (no manual
/// `chrome-use extension install`). Returns true if the host is present
/// afterwards (already was, or we just wrote it). Best-effort: a write failure
/// returns false rather than erroring.
pub fn ensure_host_installed() -> bool {
    host_installed() || install_native_host().is_ok()
}

/// Best-effort open `url` in the user's default browser (the Web Store install
/// page). No-op when `AGENT_BROWSER_NO_AUTO_OPEN` is set, so headless/CI/agent
/// contexts can suppress it. Failures are swallowed — opening a page is a
/// convenience, never load-bearing.
pub fn open_url(url: &str) {
    if std::env::var("AGENT_BROWSER_NO_AUTO_OPEN").is_ok() {
        return;
    }
    #[cfg(target_os = "macos")]
    let mut cmd = std::process::Command::new("open");
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut cmd = std::process::Command::new("xdg-open");
    #[cfg(target_os = "windows")]
    let mut cmd = {
        let mut c = std::process::Command::new("cmd");
        c.args(["/C", "start", ""]);
        c
    };
    let _ = cmd.arg(url).status();
}

/// Message for when the relay can't be reached AND the host wasn't registered —
/// i.e. the user hasn't set the extension up. By the time this shows we've
/// already registered the host and opened the Store page, so the ask is a single
/// click — never "quit and restart Chrome with a debug port".
pub fn extension_not_installed_message() -> String {
    format!(
        "chrome-use couldn't reach your Chrome — the browser extension isn't connected yet.\n\n\
         I registered the native-messaging host for you and opened the Chrome Web Store install \
         page. Click \"Add to Chrome\" there, then re-run your command:\n  {STORE_INSTALL_URL}\n\n\
         Prefer a throwaway browser instead? `chrome-use --launch --profile auto open <url>` \
         launches a separate Chrome that keeps your login."
    )
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

fn nm_log(line: &str) {
    let path = dirs::home_dir()
        .map(|h| h.join(".chrome-use").join("nm-host.log"))
        .unwrap_or_else(|| PathBuf::from("/tmp/ab-nm-host.log"));
    if let Some(p) = path.parent() {
        let _ = std::fs::create_dir_all(p);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = writeln!(f, "{line}");
    }
}

fn random_guid() -> String {
    let mut b = [0u8; 16];
    let _ = getrandom::getrandom(&mut b);
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Where the daemon/CLI reads the relay's CDP WebSocket URL (perms 600).
///
/// Cross-binary handoff: the native-messaging *host* writes it and the CLI reads
/// it, but the two may be different binaries under different brand dirs after
/// the agent-browser → chrome-use rename. Read from whichever brand dir actually
/// has the file (an old `agent-browser` host writes `~/.agent-browser`; a
/// `chrome-use` host writes `~/.chrome-use`); default to [`config_home`].
fn relay_url_path() -> PathBuf {
    if let Some(home) = dirs::home_dir() {
        for base in [".chrome-use", ".agent-browser"] {
            let p = home.join(base).join("relay-cdp-url");
            if p.exists() {
                return p;
            }
        }
        return crate::connection::config_home().join("relay-cdp-url");
    }
    PathBuf::from("/tmp/ab-relay-cdp-url")
}

/// The live relay CDP WebSocket URL, if the native-messaging host is running
/// (it writes the file on connect and removes it on exit). Used by
/// `chrome-use extension connect` to attach without the user copying a URL.
pub fn relay_url() -> Option<String> {
    let s = std::fs::read_to_string(relay_url_path()).ok()?;
    let s = s.trim().to_string();
    if s.starts_with("ws://") {
        Some(s)
    } else {
        None
    }
}

/// Append a one-line record of how a CDP connection was established, to
/// `~/.chrome-use/connect-mode.log`. This is the smoking-gun detector for the
/// "Allow remote debugging?" consent modal: that modal ONLY appears on a raw
/// remote-debugging attach / a browser we launched with a debug port — NEVER on
/// the extension relay. When the modal reappears, this log says which session
/// took which path and when, so we can tell a code regression (`raw-port` /
/// `launched` while the relay was up) from Chrome's own extension-debugger
/// consent UX. Low volume (one line per connection); best-effort, never fails a
/// connection.
pub fn log_connect_mode(ws_url: &str, launched: bool, session: &str) {
    let relay = relay_url();
    let relay_up = relay.is_some();
    let mode = if launched {
        "launched(debug-port)"
    } else if relay.as_deref() == Some(ws_url) {
        "relay"
    } else if ws_url.contains("127.0.0.1") || ws_url.contains("localhost") {
        "raw-port-attach"
    } else {
        "remote-ws"
    };
    // A raw-port attach or a self-launch while the relay was available is the
    // exact thing that pops the consent modal — flag it loudly in the line.
    let suspect = (mode == "raw-port-attach" || launched) && relay_up;
    let line = format!(
        "session={session} mode={mode} relay_up={relay_up}{} ws={ws_url}\n",
        if suspect { " CONSENT-MODAL-RISK" } else { "" }
    );
    if let Some(home) = dirs::home_dir() {
        let path = home.join(".chrome-use").join("connect-mode.log");
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            let _ = f.write_all(line.as_bytes());
        }
    }
}

/// Sidecar recording the connected extension's version, written by the host when
/// it receives the extension's `hello` (sibling of `relay-cdp-url`). Lets
/// `doctor` surface which extension build is live without a CDP round-trip.
fn relay_ext_version_path() -> PathBuf {
    relay_url_path().with_file_name("relay-ext-version")
}

/// Version of the connected `ab-connect` extension, if the host learned it from
/// the extension's `hello`. `None` when no extension has connected since the
/// host started, or the extension predates version reporting.
pub fn relay_ext_version() -> Option<String> {
    let s = std::fs::read_to_string(relay_ext_version_path())
        .ok()?
        .trim()
        .to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// Hidden `__nm-host` mode: launched by Chrome for the ab-connect extension.
///
/// Bridges the extension (native-messaging stdio, envelope protocol) to a local
/// **CDP WebSocket endpoint** that chrome-use connects to like any Chrome.
/// `relay::RelayState` translates envelope ⇄ raw CDP and emulates browser-level
/// Target discovery. The ws URL carries an unguessable guid (written to a 600
/// file) so only this user's chrome-use — not arbitrary local processes —
/// can drive the browser. No token, no user interaction.
pub fn run_nm_host() {
    let rt = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            nm_log(&format!("[nm-host] runtime build failed: {e}"));
            return;
        }
    };
    rt.block_on(nm_host_main());
}

async fn nm_host_main() {
    use crate::native::relay::{RelayOut, RelayState};
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::sync::{mpsc, Mutex};

    /// client_id -> unbounded sender feeding that client's ws writer.
    type ClientMap = Arc<Mutex<HashMap<u64, mpsc::UnboundedSender<String>>>>;

    nm_log(&format!(
        "[nm-host] start argv={:?}",
        std::env::args().skip(1).collect::<Vec<_>>()
    ));

    let listener = match tokio::net::TcpListener::bind("127.0.0.1:0").await {
        Ok(l) => l,
        Err(e) => {
            nm_log(&format!("[nm-host] bind failed: {e}"));
            return;
        }
    };
    let port = listener.local_addr().map(|a| a.port()).unwrap_or(0);
    let guid = random_guid();
    let url = format!("ws://127.0.0.1:{port}/{guid}");
    let url_path = relay_url_path();
    if let Some(p) = url_path.parent() {
        let _ = std::fs::create_dir_all(p);
    }
    if std::fs::write(&url_path, &url).is_ok() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&url_path, std::fs::Permissions::from_mode(0o600));
        }
    }
    nm_log(&format!("[nm-host] cdp endpoint {url}"));

    let state = Arc::new(Mutex::new(RelayState::new()));
    let clients: ClientMap = Arc::new(Mutex::new(HashMap::new()));
    let next_client_id = Arc::new(AtomicU64::new(1));
    let (to_ext, mut to_ext_rx) = mpsc::channel::<Vec<u8>>(4096);

    // Single writer to Chrome (extension) over stdout, native-messaging framed.
    tokio::spawn(async move {
        let mut out = tokio::io::stdout();
        while let Some(frame) = to_ext_rx.recv().await {
            let len = (frame.len() as u32).to_ne_bytes();
            if out.write_all(&len).await.is_err() || out.write_all(&frame).await.is_err() {
                break;
            }
            let _ = out.flush().await;
        }
    });

    // Accept chrome-use CDP clients on the guid-scoped ws endpoint.
    {
        let state = state.clone();
        let clients = clients.clone();
        let next_client_id = next_client_id.clone();
        let to_ext = to_ext.clone();
        let guid = guid.clone();
        tokio::spawn(async move {
            loop {
                let (stream, _) = match listener.accept().await {
                    Ok(x) => x,
                    Err(_) => break,
                };
                let st = state.clone();
                let client_id = next_client_id.fetch_add(1, Ordering::Relaxed);
                let (ctx, crx) = mpsc::unbounded_channel::<String>();
                clients.lock().await.insert(client_id, ctx);
                let tx = to_ext.clone();
                let g = guid.clone();
                let cls = clients.clone();
                tokio::spawn(async move {
                    handle_cdp_client(stream, g, st, client_id, crx, tx, cls).await;
                });
            }
        });
    }

    // Extension → host frames.
    let mut stdin = tokio::io::stdin();
    loop {
        let mut len_buf = [0u8; 4];
        if stdin.read_exact(&mut len_buf).await.is_err() {
            break;
        }
        let len = u32::from_ne_bytes(len_buf) as usize;
        let mut buf = vec![0u8; len];
        if stdin.read_exact(&mut buf).await.is_err() {
            break;
        }
        let v: serde_json::Value = match serde_json::from_slice(&buf) {
            Ok(v) => v,
            Err(_) => continue,
        };
        // Extension version handshake: record it next to the relay URL so
        // `doctor` can report which extension build is live (and whether it's
        // behind). Best-effort; the message carries no CDP payload.
        if v.get("method").and_then(|m| m.as_str()) == Some("hello") {
            if let Some(ver) = v.get("version").and_then(|x| x.as_str()) {
                let _ = std::fs::write(relay_ext_version_path(), ver);
            }
            continue;
        }
        let outs = {
            let mut s = state.lock().await;
            s.handle_ext_message(&v, "")
        };
        for o in outs {
            match o {
                RelayOut::ToClient { to, msg } => {
                    let text = msg.to_string();
                    let cls = clients.lock().await;
                    match to {
                        // Command reply → only the client that issued it.
                        Some(cid) => {
                            if let Some(tx) = cls.get(&cid) {
                                let _ = tx.send(text);
                            }
                        }
                        // CDP event → fan out to every connected client.
                        None => {
                            for tx in cls.values() {
                                let _ = tx.send(text.clone());
                            }
                        }
                    }
                }
                RelayOut::ToExt(m) => {
                    let _ = to_ext.send(m.to_string().into_bytes()).await;
                }
            }
        }
    }
    nm_log("[nm-host] stdin EOF — Chrome closed the port");
    let _ = std::fs::remove_file(relay_url_path());
    let _ = std::fs::remove_file(relay_ext_version_path());
}

#[allow(clippy::too_many_arguments)]
// The handshake-callback Result type is dictated by tokio-tungstenite's
// accept_hdr_async contract; its Err variant (an http Response) can't be shrunk.
#[allow(clippy::result_large_err)]
async fn handle_cdp_client(
    stream: tokio::net::TcpStream,
    guid: String,
    state: std::sync::Arc<tokio::sync::Mutex<crate::native::relay::RelayState>>,
    client_id: u64,
    mut from_relay: tokio::sync::mpsc::UnboundedReceiver<String>,
    to_ext: tokio::sync::mpsc::Sender<Vec<u8>>,
    clients: std::sync::Arc<
        tokio::sync::Mutex<
            std::collections::HashMap<u64, tokio::sync::mpsc::UnboundedSender<String>>,
        >,
    >,
) {
    use crate::native::relay::ClientRoute;
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;

    let want_path = format!("/{guid}");
    let cb = |req: &tokio_tungstenite::tungstenite::handshake::server::Request,
              resp: tokio_tungstenite::tungstenite::handshake::server::Response| {
        if req.uri().path() == want_path {
            Ok(resp)
        } else {
            let mut reject = tokio_tungstenite::tungstenite::handshake::server::ErrorResponse::new(
                Some("forbidden".to_string()),
            );
            *reject.status_mut() = tokio_tungstenite::tungstenite::http::StatusCode::FORBIDDEN;
            Err(reject)
        }
    };
    let ws = match tokio_tungstenite::accept_hdr_async(stream, cb).await {
        Ok(ws) => ws,
        Err(_) => return,
    };
    nm_log("[nm-host] cdp client connected");
    // Ask the extension to (re)attach + announce every tab so this client
    // discovers the user's existing tabs instead of racing an empty list.
    let _ = to_ext.send(br#"{"method":"attachAll"}"#.to_vec()).await;
    let (mut tx, mut rx) = ws.split();
    loop {
        tokio::select! {
            relayed = from_relay.recv() => match relayed {
                Some(text) => { if tx.send(Message::Text(text)).await.is_err() { break } }
                None => break,
            },
            incoming = rx.next() => match incoming {
                Some(Ok(Message::Text(text))) => {
                    let v: serde_json::Value = match serde_json::from_str(&text) {
                        Ok(v) => v,
                        Err(_) => continue,
                    };
                    let route = { state.lock().await.route_client_command(client_id, &v) };
                    match route {
                        ClientRoute::Local(reply) => {
                            if tx.send(Message::Text(reply.to_string())).await.is_err() { break }
                        }
                        ClientRoute::Forward(env) => {
                            let _ = to_ext.send(env.to_string().into_bytes()).await;
                        }
                    }
                }
                Some(Ok(Message::Close(_))) | None => break,
                _ => {}
            },
        }
    }
    // Unregister and forget this client's in-flight commands.
    clients.lock().await.remove(&client_id);
    state.lock().await.drop_client(client_id);
    nm_log("[nm-host] cdp client disconnected");
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::Path;

    #[test]
    fn store_install_url_points_at_the_store_build() {
        // Must be the published Store id, not the dev id, or "Add to Chrome" 404s.
        assert!(STORE_INSTALL_URL.contains(STORE_EXTENSION_ID));
        assert!(STORE_INSTALL_URL.starts_with("https://chromewebstore.google.com/"));
    }

    #[test]
    fn not_installed_message_guides_to_store_never_restarts_chrome() {
        let m = extension_not_installed_message();
        assert!(m.contains(STORE_INSTALL_URL));
        // The whole point of this rework: no "quit/restart Chrome with a debug port".
        assert!(!m.contains("--remote-debugging-port"));
        assert!(!m.to_lowercase().contains("quit chrome"));
    }


    #[test]
    fn parses_active_and_pending_store_extension_versions() {
        let settings = json!({
            "from_webstore": true,
            "active_bit": false,
            "path": "knfcmbamhjmaonkfnjhldjedeobeafmk/0.4.12_1",
            "manifest": { "name": "chrome-use", "version": "0.4.12" },
            "active_permissions": { "api": ["debugger", "nativeMessaging"] },
            "idle_install_info": {
                "path": "knfcmbamhjmaonkfnjhldjedeobeafmk/0.5.1_0",
                "manifest": { "name": "chrome-use", "version": "0.5.1" }
            }
        });

        let status = parse_chrome_extension_status(
            STORE_EXTENSION_ID,
            &settings,
            Path::new("/tmp/Secure Preferences"),
        );

        assert_eq!(status.id, STORE_EXTENSION_ID);
        assert_eq!(status.name.as_deref(), Some("chrome-use"));
        assert_eq!(status.version.as_deref(), Some("0.4.12"));
        assert_eq!(status.idle_version.as_deref(), Some("0.5.1"));
        assert_eq!(
            status.idle_path.as_deref(),
            Some("knfcmbamhjmaonkfnjhldjedeobeafmk/0.5.1_0")
        );
        assert_eq!(
            status.active_permissions,
            vec!["debugger", "nativeMessaging"]
        );
        assert_eq!(status.from_webstore, Some(true));
    }

    #[test]
    fn parses_numeric_disable_reasons_for_permission_prompts() {
        let settings = json!({
            "path": "knfcmbamhjmaonkfnjhldjedeobeafmk/0.5.1_0",
            "manifest": { "name": "chrome-use", "version": "0.5.1" },
            "disable_reasons": [4, "permissions_increase"]
        });

        let status = parse_chrome_extension_status(
            STORE_EXTENSION_ID,
            &settings,
            Path::new("/tmp/Secure Preferences"),
        );

        assert_eq!(status.version.as_deref(), Some("0.5.1"));
        assert_eq!(status.disable_reasons, vec!["4", "permissions_increase"]);
    }
}

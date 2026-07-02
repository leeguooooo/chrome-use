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

/// The published Chrome Web Store listing — the guaranteed one-click
/// "Add to Chrome" path. MUST use the **published Store id**
/// (`STORE_EXTENSION_ID`), not the dev/unpacked id (`EXTENSION_ID`), or the page
/// 404s — that exact mix-up shipped a wrong install link to a user. Single
/// source of truth: every user-facing "install the extension" URL uses this.
pub const STORE_URL: &str =
    "https://chromewebstore.google.com/detail/chrome-use/knfcmbamhjmaonkfnjhldjedeobeafmk";

/// Back-compat alias for callers that referenced the (now-merged) install URL.
pub const STORE_INSTALL_URL: &str = STORE_URL;

/// Stable identifiers for the generated Chrome configuration profile, so a
/// re-install replaces (rather than duplicates) it in System Settings.
/// Reverse-DNS of leeguoo.com, the product's home. Earlier releases shipped
/// pwtk-branded ids ([`OLD_PROFILE_IDS`]); the UUIDs are fresh too, so macOS
/// treats this as a new profile instead of an update to the stale one.
const PROFILE_ID: &str = "com.leeguoo.chrome-use.connect";
const PROFILE_UUID: &str = "5EE60001-AB00-4CCE-9E10-AAAABBBBCC01";
const PROFILE_PAYLOAD_UUID: &str = "5EE60001-AB01-4CCE-9E10-AAAABBBBCC02";

/// Configuration-profile identifiers shipped by earlier releases: the
/// agent-browser era and the pwtk-branded chrome-use era. The old payloads
/// force-installed the dev extension id from a self-hosted update URL, which
/// Chrome 149+ blocks on unmanaged machines — so an approved copy sits there
/// silently doing nothing. Setup detects them and tells the user to remove.
const OLD_PROFILE_IDS: &[&str] = &[
    "work.pwtk.agent-browser.ab-connect",
    "work.pwtk.chrome-use.ab-connect",
];

/// `chrome-use browsers` — list the Chrome profiles whose ab-connect worker is
/// currently connected to the relay, so an agent/user can pin a session to one
/// with `--browser <id|email>` (issue #60). Local; no daemon.
pub fn run_browsers(json: bool) {
    let profiles = list_relay_profiles();
    // The generic (last-writer) default the relay binds to without `--browser`.
    let default = relay_ext_profile().map(|(id, _)| id);
    if json {
        let arr: Vec<_> = profiles
            .iter()
            .map(|(id, email, ws)| {
                serde_json::json!({
                    "id": id,
                    "email": email,
                    "wsUrl": ws,
                    "default": default.as_deref() == Some(id.as_str()),
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string(&serde_json::json!({
                "success": true,
                "data": { "browsers": arr }
            }))
            .unwrap_or_default()
        );
        return;
    }
    if profiles.is_empty() {
        println!(
            "no connected Chrome profiles.\n\
             (needs ab-connect \u{2265}0.5.3; if Chrome is running, try `chrome-use reconnect`.)"
        );
        return;
    }
    println!("Connected Chrome profiles — drive one with `--browser <id|email>`:");
    for (id, email, _) in &profiles {
        let mark = if default.as_deref() == Some(id.as_str()) {
            "  (default)"
        } else {
            ""
        };
        match email {
            Some(e) => println!("  {e}  [{id}]{mark}"),
            None => println!(
                "  {id}{mark}  (no email — grant the ext `identity` permission to show it)"
            ),
        }
    }
}

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
                let (approved, old_ids) = approved_config_profiles();
                let _ = approved;
                for id in old_ids {
                    println!("  Also remove the older one: profiles remove -identifier {id}");
                }
            }
        }
        return;
    }
    if install {
        run_install(args, json);
        return;
    }

    // Status.
    let manifests = installed_host_manifests();
    let installed = !manifests.is_empty();
    let extension_status = chrome_extension_status();
    let relay_url = relay_url();
    let live_extension_version = relay_ext_version();
    let expected_extension_version = env!("AB_CONNECT_VERSION");
    let driving_profile = relay_ext_profile();
    let profiles = chrome_profiles();
    let policy = managed_policy_state();
    let (_, old_ids) = approved_config_profiles();
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
                    "drivingProfileId": driving_profile.as_ref().map(|(id, _)| id.clone()),
                    "drivingProfileEmail": driving_profile.as_ref().and_then(|(_, e)| e.clone()),
                    "profiles": profiles,
                    "policy": {
                        "state": policy.as_str(),
                        "staleEntry": match &policy { PolicyState::Stale(e) => Some(e.clone()), _ => None },
                        "staleProfileIds": old_ids,
                        "profileId": PROFILE_ID,
                    },
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
        // Which Chrome profile is the relay bound to (issue #60). With many
        // profiles, this disambiguates a "logged out" result (wrong profile vs.
        // genuinely not logged in).
        match &driving_profile {
            Some((id, Some(email))) => println!("✓ driving Chrome profile: {email} ({id})"),
            Some((id, None)) => println!(
                "✓ driving Chrome profile id: {id} \
                 (grant the extension the optional `identity` permission to also see the email)"
            ),
            None => println!(
                "  driving Chrome profile: unknown (extension predates profile reporting, or \
                 hasn't sent hello yet)"
            ),
        }
        if let Some(status) = extension_status {
            print_chrome_extension_status(&status, expected_extension_version);
        } else {
            println!("  Chrome profile extension status: not found in any Chrome profile");
        }
        // Per-profile coverage: with many profiles (one per account), the thing
        // that bites is "the extension is only in SOME of them".
        let zh = ui_zh();
        let with_ext = profiles.iter().filter(|p| p.extension.is_some()).count();
        if zh {
            println!(
                "\n  Chrome profile（找到 {} 个，{} 个已装扩展）：",
                profiles.len(),
                with_ext
            );
        } else {
            println!(
                "\n  Chrome profiles ({} found, {} with the extension):",
                profiles.len(),
                with_ext
            );
        }
        for p in &profiles {
            match &p.extension {
                Some(ext) => println!(
                    "    ✓ {}  —  {}",
                    p.label(),
                    ext.version.as_deref().unwrap_or("?")
                ),
                None => println!(
                    "    ✗ {}  —  {}",
                    p.label(),
                    if zh { "未安装" } else { "missing" }
                ),
            }
        }
        match &policy {
            PolicyState::Active => println!(
                "{}",
                if zh {
                    "  ✓ 静默安装策略：已生效（缺失的 profile 会在 Chrome 重启时装上）"
                } else {
                    "  ✓ silent-install policy: active (missing profiles install on Chrome restart)"
                }
            ),
            PolicyState::Stale(entry) => {
                if zh {
                    println!(
                        "  ! 静默安装策略：已过期（{entry}）—— 虽已批准但被 Chrome 屏蔽。\n\
                         \x20   运行 `chrome-use extension install` 修复。"
                    );
                } else {
                    println!(
                        "  ! silent-install policy: STALE ({entry}) — approved but Chrome blocks it.\n\
                         \x20   Run `chrome-use extension install` to fix."
                    );
                }
            }
            PolicyState::Absent => {
                if with_ext < profiles.len() {
                    println!(
                        "{}",
                        if zh {
                            "  给缺失的 profile 装上扩展：chrome-use extension install"
                        } else {
                            "  Get the extension into the missing profiles: chrome-use extension install"
                        }
                    );
                }
            }
            PolicyState::Unknown => {}
        }
        if !old_ids.is_empty() {
            if zh {
                println!(
                    "  ! 检测到已批准的过期策略描述文件（{}）—— 在 系统设置 → 隐私与安全性\n\
                     \x20   → 描述文件 中删除，或 `profiles remove -identifier <id>`",
                    old_ids.join(", ")
                );
            } else {
                println!(
                    "  ! outdated policy profile(s) approved ({}) — remove in System Settings →\n\
                     \x20   Privacy & Security → Profiles, or `profiles remove -identifier <id>`",
                    old_ids.join(", ")
                );
            }
        }
    } else {
        println!("✗ not installed. Run: chrome-use connect --install");
    }
}

/// `extension install [--no-open] [--all-profiles]` — the guided setup.
///
/// Everything that CAN be automatic is: the native-messaging host (user-level,
/// shared by every profile) is written outright, and the state of every Chrome
/// profile + the install policy is detected and reported. What remains is the
/// one action Chrome's security model reserves for the human, and the guide
/// makes that a single decision:
///   A) approve the policy profile once → Chrome silently installs the
///      extension into EVERY profile, current and future (macOS), or
///   B) `--all-profiles` → the Web Store page opens inside each missing
///      profile and the user clicks "Add to Chrome" in each.
fn run_install(args: &[String], json: bool) {
    let no_open = args.iter().any(|a| a == "--no-open");
    let all_profiles = args.iter().any(|a| a == "--all-profiles");

    let host = install_native_host();
    if let Err(e) = &host {
        report(json, false, &format!("install failed: {e}"));
        return;
    }
    let host_paths = host.unwrap_or_default();

    let profiles = chrome_profiles();
    let missing: Vec<&ChromeProfileInfo> = profiles
        .iter()
        .filter(|p| p.extension.is_none())
        .collect();
    let with_ext = profiles.len() - missing.len();
    let policy = managed_policy_state();
    let (_, old_ids) = approved_config_profiles();

    // Path B: open the Store page inside each profile that lacks the extension.
    let mut opened: Vec<String> = Vec::new();
    if all_profiles {
        for (i, p) in missing.iter().enumerate() {
            if i > 0 {
                // Chrome races when many windows spawn at once; pace the opens.
                std::thread::sleep(std::time::Duration::from_millis(900));
            }
            if open_store_in_profile(p) {
                opened.push(p.dir.clone());
            }
        }
    }

    // Path A: write + open the policy profile, unless it's already doing its
    // job or the user explicitly chose the per-profile route.
    let policy_needed = policy != PolicyState::Active && !all_profiles;
    let mobileconfig = if policy_needed {
        Some(install_force_install_profile(no_open))
    } else {
        None
    };

    if json {
        println!(
            "{}",
            serde_json::to_string(&serde_json::json!({
                "success": true,
                "data": {
                    "installed": host_paths,
                    "extensionId": STORE_EXTENSION_ID,
                    "updateUrl": UPDATE_URL,
                    "policy": {
                        "state": policy.as_str(),
                        "staleEntry": match &policy { PolicyState::Stale(e) => Some(e.clone()), _ => None },
                        "staleProfileIds": old_ids,
                        "profileId": PROFILE_ID,
                    },
                    "profile": mobileconfig.as_ref().and_then(|r| r.as_ref().ok().map(|p| p.display().to_string())),
                    "profileError": mobileconfig.as_ref().and_then(|r| r.as_ref().err().cloned()),
                    "profiles": profiles,
                    "openedStorePageIn": opened,
                }
            }))
            .unwrap_or_default()
        );
        return;
    }

    // -- The human-facing guide (localized to the user's locale). ------------
    let zh = ui_zh();
    if zh {
        println!("chrome-use 安装向导 — 连接你的 Chrome\n");
        println!("[1/3] native messaging host（所有 profile 共享）");
        println!("  ✓ 已自动装好（{} 个 manifest）", host_paths.len());
        println!(
            "\n[2/3] 你的 Chrome profile（找到 {} 个，{} 个已装扩展）",
            profiles.len(),
            with_ext
        );
    } else {
        println!("chrome-use setup — connect your real Chrome\n");
        println!("[1/3] native-messaging host (shared by every profile)");
        println!("  ✓ installed ({} manifest(s))", host_paths.len());
        println!(
            "\n[2/3] your Chrome profiles ({} found, {} with the extension)",
            profiles.len(),
            with_ext
        );
    }
    for p in &profiles {
        match &p.extension {
            Some(ext) => println!(
                "  ✓ {}  —  {}{}",
                p.label(),
                if zh { "扩展 " } else { "extension " },
                ext.version.as_deref().unwrap_or("?")
            ),
            None => println!(
                "  ✗ {}  —  {}",
                p.label(),
                if zh { "未安装" } else { "missing" }
            ),
        }
    }
    if profiles.is_empty() {
        println!(
            "  {}",
            if zh {
                "（没有找到 Chrome profile —— Chrome 装了吗？）"
            } else {
                "(no Chrome profiles found — is Chrome installed?)"
            }
        );
    }

    println!(
        "\n[3/3] {}",
        if zh {
            "把扩展装进缺失的 profile"
        } else {
            "get the extension into the missing profiles"
        }
    );
    if !old_ids.is_empty() {
        if zh {
            println!(
                "  ! 检测到旧版本留下的过期策略描述文件（已批准但被 Chrome 屏蔽，\n\
                 \x20   装不上任何东西）。请先删除：\n\
                 \x20     系统设置 → 隐私与安全性 → 描述文件 → \"chrome-use connect\" → −"
            );
        } else {
            println!(
                "  ! an OUTDATED policy profile from an earlier release is approved — Chrome\n\
                 \x20   blocks its install source, so it silently does nothing. Remove it first:\n\
                 \x20     System Settings → Privacy & Security → Profiles → \"chrome-use connect\" → −"
            );
        }
        for id in &old_ids {
            println!("     (or: profiles remove -identifier {id})");
        }
    }
    if all_profiles {
        if zh {
            println!(
                "  → 已在 {} 个 profile 里打开商店安装页：{}\n\
                 \x20   在每个窗口点一次「加入 Chrome」即可（Cmd+` 在窗口间切换）。",
                opened.len(),
                opened.join(", ")
            );
            if opened.len() < missing.len() {
                println!(
                    "  ! 有 {} 个 profile 没能自动打开 —— 请自行在其中访问\n\
                     \x20   {STORE_URL}",
                    missing.len() - opened.len()
                );
            }
        } else {
            println!(
                "  → opened the Web Store install page in {} profile(s): {}\n\
                 \x20   Press \"Add to Chrome\" in each window (Cmd+` cycles through them).",
                opened.len(),
                opened.join(", ")
            );
            if opened.len() < missing.len() {
                println!(
                    "  ! {} profile(s) could not be opened automatically — visit\n\
                     \x20   {STORE_URL} in them yourself.",
                    missing.len() - opened.len()
                );
            }
        }
    } else if policy == PolicyState::Active {
        if zh {
            println!(
                "  ✓ 静默安装策略已生效。重启一次 Chrome，缺失的 profile 会全部自动装上。"
            );
        } else {
            println!(
                "  ✓ the silent-install policy is active. Restart Chrome once and every\n\
                 \x20   missing profile installs the extension automatically."
            );
        }
    } else {
        match mobileconfig {
            Some(Ok(path)) => {
                if zh {
                    println!("  二选一：");
                    println!(
                        "  A) 静默、一次覆盖全部 profile{}：批准我们刚{}的策略描述文件\n\
                         \x20    （{}）\n\
                         \x20      系统设置 → 隐私与安全性 → 描述文件\n\
                         \x20      → 双击 \"chrome-use connect\"（leeguoo.com）→ 安装…\n\
                         \x20    然后重启 Chrome：现有和将来新建的每个 profile 都会自动装上\n\
                         \x20    并保持更新，之后再也不用点任何东西。",
                        if cfg!(target_os = "macos") { "" } else { "（仅 macOS）" },
                        if no_open { "写好" } else { "打开" },
                        path.display()
                    );
                    println!(
                        "  B) 每个 profile 点一下：chrome-use extension install --all-profiles\n\
                         \x20    会在每个缺失的 profile 里打开商店安装页 —— 逐个点「加入 Chrome」。"
                    );
                } else {
                    println!("  Pick ONE:");
                    println!(
                        "  A) Silent, all profiles at once{}: approve the policy we just {}\n\
                         \x20    ({})\n\
                         \x20      System Settings → Privacy & Security → Profiles\n\
                         \x20      → \"chrome-use connect\" (leeguoo.com) → double-click → Install…\n\
                         \x20    Then restart Chrome: every profile — current and future — gets the\n\
                         \x20    extension installed and auto-updated. Nothing else to click, ever.",
                        if cfg!(target_os = "macos") { "" } else { " (macOS)" },
                        if no_open { "wrote" } else { "opened" },
                        path.display()
                    );
                    println!(
                        "  B) One click per profile: chrome-use extension install --all-profiles\n\
                         \x20    opens the Web Store page inside each missing profile — press\n\
                         \x20    \"Add to Chrome\" in each."
                    );
                }
                if cfg!(target_os = "macos") && !no_open && interactive_tty() {
                    guide_through_approval(zh);
                }
            }
            Some(Err(e)) => {
                if zh {
                    println!("  ! 策略描述文件写入失败：{e}");
                    println!(
                        "  退路：chrome-use extension install --all-profiles（每个 profile\n\
                         \x20 点一下），或自行在各 profile 里打开 {STORE_URL}"
                    );
                } else {
                    println!("  ! could not write the policy profile: {e}");
                    println!(
                        "  Fallback: chrome-use extension install --all-profiles (one click per\n\
                         \x20 profile), or open {STORE_URL} in each profile yourself."
                    );
                }
            }
            None => {}
        }
    }

    if zh {
        println!(
            "\n验证：chrome-use browsers\n\
             （profile 开着窗口时才会出现在列表里 —— 扩展只在运行中的 profile 里活动）"
        );
    } else {
        println!(
            "\nverify:  chrome-use browsers\n\
             (a profile appears there once one of its windows is open — the extension\n\
             \x20only runs in profiles that are running)"
        );
    }
}

/// stdin+stderr are TTYs → a wait/confirm flow is safe (same rule as silence).
fn interactive_tty() -> bool {
    use std::io::IsTerminal;
    std::io::stdin().is_terminal() && std::io::stderr().is_terminal()
}

/// Chinese UI for the human-facing setup/guide text? Honors CHROME_USE_LANG
/// (`zh`/`en` override), then the POSIX locale chain. JSON output stays
/// English — it's for agents, and keys/values are contract, not prose.
fn ui_zh() -> bool {
    if let Ok(explicit) = std::env::var("CHROME_USE_LANG") {
        if !explicit.is_empty() {
            return explicit.to_lowercase().starts_with("zh");
        }
    }
    ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .filter_map(|v| std::env::var(v).ok())
        .find(|v| !v.is_empty())
        .is_some_and(|v| v.to_lowercase().starts_with("zh"))
}

/// The closed loop around the ONE manual step — this is where users get lost:
/// `open <file>.mobileconfig` only QUEUES the profile. No window appears,
/// System Settings does not open itself, and the queued item silently EXPIRES
/// after ~8 minutes. Left alone, most people never find it. So: land the user
/// on the exact Settings pane, wait right here for the approval to appear,
/// restart Chrome for them (session preserved) so the policy applies NOW
/// instead of "someday", and show the extension reaching their profiles until
/// coverage is done. TTY-only; JSON/agent/`--no-open` runs skip it.
fn guide_through_approval(zh: bool) {
    use std::io::Write as _;
    use std::time::{Duration, Instant};

    // Deep-link straight to the pane the queued profile is sitting in.
    open_url("x-apple.systempreferences:com.apple.preferences.configurationprofiles");

    if zh {
        eprintln!(
            "\n  系统设置已打开并停在「描述文件」面板。在「已下载」下面\n\
             \x20 双击 \"chrome-use connect\" → 安装…（macOS 会要求输入 Mac 密码 ——\n\
             \x20 这是 Apple 的批准步骤，我们看不到你的密码）。\n\
             \x20 我在这里等你。（Ctrl-C 可以稍后再装；上面的 B 方案也随时可用。）"
        );
        eprint!("  等待批准 ");
    } else {
        eprintln!(
            "\n  System Settings is open on the Profiles pane. Under \"Downloaded\",\n\
             \x20 double-click \"chrome-use connect\" → Install… (macOS asks for your Mac\n\
             \x20 password — that's Apple's approval step, not something we see).\n\
             \x20 I'll wait here. (Ctrl-C to finish later; option B above also works.)"
        );
        eprint!("  waiting for the approval ");
    }
    let deadline = Instant::now() + Duration::from_secs(300);
    let mut approved = false;
    while Instant::now() < deadline {
        if approved_config_profiles().0 {
            approved = true;
            break;
        }
        eprint!(".");
        let _ = std::io::stderr().flush();
        std::thread::sleep(Duration::from_secs(2));
    }
    if !approved {
        if zh {
            eprintln!(
                "\n  ! 5 分钟内没有等到批准 —— 待安装的描述文件可能已过期（macOS 约 8\n\
                 \x20   分钟后丢弃）。重新运行 `chrome-use extension install` 再排一次即可。"
            );
        } else {
            eprintln!(
                "\n  ! not approved within 5 minutes — the queued profile may have expired\n\
                 \x20   (macOS drops it after ~8). Re-run `chrome-use extension install` to\n\
                 \x20   queue it again."
            );
        }
        return;
    }
    eprintln!("{}", if zh { " ✓ 已批准" } else { " ✓ approved" });

    // The approval lands in managed preferences asynchronously; confirm Chrome
    // will actually see it before promising anything.
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline && managed_policy_state() != PolicyState::Active {
        std::thread::sleep(Duration::from_secs(1));
    }
    if managed_policy_state() != PolicyState::Active {
        if zh {
            eprintln!(
                "  ! 批准成功，但策略还没同步到 Chrome 的受管配置 —— 等一两分钟后\n\
                 \x20   自行重启 Chrome 即可。"
            );
        } else {
            eprintln!(
                "  ! approved, but the policy hasn't reached Chrome's managed preferences\n\
                 \x20   yet — give it a minute, then restart Chrome yourself."
            );
        }
        return;
    }
    eprintln!(
        "{}",
        if zh {
            "  ✓ 策略已生效 —— Chrome 会把扩展装进每一个 profile"
        } else {
            "  ✓ policy active — Chrome now installs the extension into every profile"
        }
    );

    // Apply it NOW instead of leaving a "restart Chrome later" chore.
    eprint!(
        "{}",
        if zh {
            "  现在重启 Chrome 让它立即生效（标签页会自动恢复）？[Y/n] "
        } else {
            "  Restart Chrome to apply it (your tabs are restored automatically)? [Y/n] "
        }
    );
    let _ = std::io::stderr().flush();
    let mut input = String::new();
    let restart = std::io::stdin().read_line(&mut input).is_ok() && {
        let a = input.trim().to_lowercase();
        a.is_empty() || a == "y" || a == "yes"
    };
    if !restart {
        eprintln!(
            "{}",
            if zh {
                "  好的 —— 下次重启 Chrome 时扩展会自动装上。"
            } else {
                "  OK — the extension installs on Chrome's next restart."
            }
        );
        return;
    }
    match crate::silence::restart_chrome_preserving_session() {
        Ok(true) => {}
        Ok(false) => {
            eprintln!(
                "{}",
                if zh {
                    "  Chrome 没在运行 —— 每个 profile 会在打开时自动装上扩展。"
                } else {
                    "  Chrome isn't running — each profile picks the extension up when opened."
                }
            );
            return;
        }
        Err(e) => {
            eprintln!("  ! {e}");
            return;
        }
    }

    // Chrome applies the forcelist per profile as each profile session starts,
    // so restored profiles fill in over the next seconds — show it happening.
    eprint!(
        "{}",
        if zh {
            "  Chrome 已重启；正在看着扩展铺到各个 profile "
        } else {
            "  Chrome restarted; watching the extension reach your profiles "
        }
    );
    let deadline = Instant::now() + Duration::from_secs(60);
    let (mut done, mut total) = (0usize, 0usize);
    while Instant::now() < deadline {
        let profiles = chrome_profiles();
        total = profiles.len();
        done = profiles.iter().filter(|p| p.extension.is_some()).count();
        if total > 0 && done == total {
            break;
        }
        eprint!(".");
        let _ = std::io::stderr().flush();
        std::thread::sleep(Duration::from_secs(3));
    }
    eprintln!();
    if total > 0 && done == total {
        if zh {
            eprintln!("  ✓ 全部 {total} 个 profile 都装上扩展了。安装完成。");
        } else {
            eprintln!("  ✓ all {total} profiles have the extension. Setup complete.");
        }
    } else if zh {
        eprintln!(
            "  ✓ 目前 {done}/{total} 个 profile 已装上。其余的会在你下次打开它们的\n\
             \x20   瞬间自动装上（Chrome 按 profile 加载时应用策略）—— 之后再也不用\n\
             \x20   点任何东西。"
        );
    } else {
        eprintln!(
            "  ✓ {done}/{total} profiles have it so far. The rest install the moment you\n\
             \x20   next open them (Chrome applies the policy per profile as it loads) —\n\
             \x20   nothing more to click, ever."
        );
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
  <key>PayloadDescription</key><string>Installs the chrome-use connect extension into every Chrome profile (current and future) so chrome-use can drive your logged-in Chrome. No token, no per-use confirmation.</string>
  <key>PayloadOrganization</key><string>leeguoo.com</string>
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

/// One real Chrome profile, from `Local State`'s `profile.info_cache`, with the
/// per-profile extension install status. This is what "did the extension reach
/// every profile?" is answered from — a person routinely has 10+ profiles
/// (one per account), and only profiles WITH the extension can be driven.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ChromeProfileInfo {
    /// Browser data root, e.g. `…/Google/Chrome`.
    root: String,
    /// Profile directory name: `Default`, `Profile 2`, …
    dir: String,
    /// Human name shown in Chrome's profile switcher.
    name: Option<String>,
    /// Signed-in Google account, when the profile has one.
    email: Option<String>,
    extension: Option<ChromeExtensionStatus>,
}

impl ChromeProfileInfo {
    /// `Default` first, then `Profile N` numerically, then anything else.
    fn sort_key(&self) -> (u8, u64, String) {
        if self.dir == "Default" {
            (0, 0, String::new())
        } else if let Some(n) = self
            .dir
            .strip_prefix("Profile ")
            .and_then(|s| s.parse().ok())
        {
            (1, n, String::new())
        } else {
            (2, 0, self.dir.clone())
        }
    }

    fn label(&self) -> String {
        let mut parts = vec![self.dir.clone()];
        if let Some(n) = &self.name {
            parts.push(n.clone());
        }
        if let Some(e) = &self.email {
            parts.push(e.clone());
        }
        parts.join("  ")
    }
}

/// Every profile of every installed Chrome-family browser, enumerated from
/// `Local State` (the authoritative registry — profile dirs are NOT guessable:
/// deleting Profile 3 leaves a numbering gap, and this user-visible bug shipped
/// once as a hardcoded `Profile 1..3` scan that missed Profile 4-14).
fn chrome_profiles() -> Vec<ChromeProfileInfo> {
    let mut out = Vec::new();
    for root in chrome_profile_roots() {
        let Ok(text) = std::fs::read_to_string(root.join("Local State")) else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        let Some(cache) = value
            .pointer("/profile/info_cache")
            .and_then(|v| v.as_object())
        else {
            continue;
        };
        for (dir, info) in cache {
            out.push(ChromeProfileInfo {
                root: root.display().to_string(),
                dir: dir.clone(),
                name: info
                    .get("name")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.trim().is_empty())
                    .map(ToString::to_string),
                email: info
                    .get("user_name")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.trim().is_empty())
                    .map(ToString::to_string),
                extension: profile_extension_status(&root, dir),
            });
        }
    }
    out.sort_by_key(|p| p.sort_key());
    out
}

fn profile_extension_status(root: &Path, dir: &str) -> Option<ChromeExtensionStatus> {
    for file in ["Secure Preferences", "Preferences"] {
        if let Some(status) = chrome_extension_status_from_file(&root.join(dir).join(file)) {
            return Some(status);
        }
    }
    None
}

fn chrome_extension_status() -> Option<ChromeExtensionStatus> {
    chrome_profiles().into_iter().find_map(|p| p.extension)
}

/// State of the ExtensionInstallForcelist policy Chrome actually sees (merged
/// managed preferences), which decides whether the silent all-profiles install
/// path is live.
#[derive(Debug, Clone, PartialEq)]
enum PolicyState {
    /// Forcelist carries the Web Store id + Web Store update URL — Chrome
    /// installs the extension into every profile on its own.
    Active,
    /// A forcelist entry for OUR extension exists but is wrong (old dev id, or
    /// a self-hosted update URL Chrome 149+ blocks). Approved yet inert — the
    /// worst state, because it LOOKS done. Carries the offending entry.
    Stale(String),
    /// No forcelist entry for our extension.
    Absent,
    /// Not macOS, or managed preferences unreadable.
    Unknown,
}

impl PolicyState {
    fn as_str(&self) -> &'static str {
        match self {
            PolicyState::Active => "active",
            PolicyState::Stale(_) => "stale",
            PolicyState::Absent => "absent",
            PolicyState::Unknown => "unknown",
        }
    }
}

/// Read the forcelist Chrome sees from macOS managed preferences (user scope,
/// then device scope). `defaults read` fails when the key is absent, which maps
/// to `Absent`; corporate forcelists for OTHER extensions are ignored.
fn managed_policy_state() -> PolicyState {
    if !cfg!(target_os = "macos") {
        return PolicyState::Unknown;
    }
    let expected = format!("{STORE_EXTENSION_ID};{UPDATE_URL}");
    let user = std::env::var("USER").unwrap_or_default();
    let domains = [
        format!("/Library/Managed Preferences/{user}/com.google.Chrome"),
        "/Library/Managed Preferences/com.google.Chrome".to_string(),
    ];
    let mut stale: Option<String> = None;
    for domain in domains {
        let Ok(out) = std::process::Command::new("defaults")
            .args(["read", &domain, "ExtensionInstallForcelist"])
            .output()
        else {
            continue;
        };
        if !out.status.success() {
            continue;
        }
        let text = String::from_utf8_lossy(&out.stdout);
        if text.contains(&expected) {
            return PolicyState::Active;
        }
        for line in text.lines() {
            let entry = line.trim().trim_matches(|c| c == '"' || c == ',' || c == ';');
            let is_ours = entry.contains(EXTENSION_ID)
                || entry.contains(STORE_EXTENSION_ID)
                || entry.contains("agent-browser");
            if is_ours && entry.contains(';') {
                stale = Some(entry.trim_matches('"').to_string());
            }
        }
    }
    match stale {
        Some(entry) => PolicyState::Stale(entry),
        None => PolicyState::Absent,
    }
}

/// Which of our configuration profiles the user has APPROVED in System
/// Settings (macOS `profiles list`): (current id approved, old ids present).
fn approved_config_profiles() -> (bool, Vec<&'static str>) {
    if !cfg!(target_os = "macos") {
        return (false, Vec::new());
    }
    let Ok(out) = std::process::Command::new("profiles").arg("list").output() else {
        return (false, Vec::new());
    };
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let approved = text.contains(PROFILE_ID);
    let old = OLD_PROFILE_IDS
        .iter()
        .copied()
        .filter(|id| text.contains(id))
        .collect();
    (approved, old)
}

/// Open the Web Store install page inside one specific Chrome profile, so the
/// user's only remaining action is "Add to Chrome" in that window. (The Store
/// button itself cannot be automated: Chrome detaches debuggers on
/// chromewebstore pages, and off-store force-install is blocked — this
/// one-click-per-profile flow is the floor the browser's security model allows.)
fn open_store_in_profile(profile: &ChromeProfileInfo) -> bool {
    let root = Path::new(&profile.root);
    let profile_arg = format!("--profile-directory={}", profile.dir);
    #[cfg(target_os = "macos")]
    {
        let app = match root.file_name().and_then(|s| s.to_str()) {
            Some("Chrome") => "Google Chrome",
            Some("Chrome Beta") => "Google Chrome Beta",
            Some("Chrome Canary") => "Google Chrome Canary",
            Some("Chromium") => "Chromium",
            _ => return false,
        };
        std::process::Command::new("open")
            .args(["-na", app, "--args", &profile_arg, STORE_URL])
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let bin = match root.file_name().and_then(|s| s.to_str()) {
            Some("google-chrome") => "google-chrome",
            Some("google-chrome-beta") => "google-chrome-beta",
            Some("google-chrome-unstable") => "google-chrome-unstable",
            Some("chromium") => "chromium",
            _ => return false,
        };
        std::process::Command::new(bin)
            .args([&profile_arg, STORE_URL])
            .spawn()
            .is_ok()
    }
    #[cfg(target_os = "windows")]
    {
        let _ = (root, profile_arg);
        false
    }
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
         Use several Chrome profiles? `chrome-use extension install` sets it up for ALL of \
         them at once (one policy approval instead of a click per profile).\n\n\
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

/// Sidecar recording WHICH Chrome profile the relay is bound to, written by the
/// host from the extension's `hello` (issue #60). With many profiles, "logged
/// out" on a site is otherwise indistinguishable from "wrong profile entirely" —
/// this lets `doctor`/`extension status` name the driving profile. Sibling of
/// `relay-cdp-url`.
fn relay_ext_profile_path() -> PathBuf {
    relay_url_path().with_file_name("relay-ext-profile")
}

/// The Chrome profile the relay is currently driving, as `(id, email)` learned
/// from the extension's `hello`. `id` is a stable per-profile UUID (always
/// present on a new-enough extension); `email` is the signed-in account, only
/// present when the profile granted the optional `identity` permission. `None`
/// when no profile-aware extension has connected yet.
pub fn relay_ext_profile() -> Option<(String, Option<String>)> {
    let s = std::fs::read_to_string(relay_ext_profile_path()).ok()?;
    parse_ext_profile(&s)
}

/// Parse the `relay-ext-profile` sidecar JSON into `(id, email)`. Pure, so the
/// shape can be unit-tested without touching the filesystem. Returns `None` when
/// the id is absent/blank; an empty/missing email becomes `None`.
fn parse_ext_profile(s: &str) -> Option<(String, Option<String>)> {
    let v: serde_json::Value = serde_json::from_str(s.trim()).ok()?;
    let id = v.get("id").and_then(|x| x.as_str())?.trim().to_string();
    if id.is_empty() {
        return None;
    }
    let email = v
        .get("email")
        .and_then(|x| x.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    Some((id, email))
}

// --- Per-profile relay endpoints (issue #60 `--browser` selection) -----------
//
// With several Chrome profiles, each profile's extension worker spawns its OWN
// native-messaging host, and they all clobber the single `relay-cdp-url` (last
// writer wins) — which is why the bound profile is non-deterministic. To let a
// session *pick* a profile deterministically, each host ALSO writes a stable
// per-profile sidecar keyed by the `hello`'s profileId:
//   relay-cdp-url-<id>      — that host's ws URL (unique per host/profile)
//   relay-ext-profile-<id>  — {id,email}
// removed by that host on disconnect. `--browser <id|email-substr>` resolves to
// `relay-cdp-url-<id>`; the generic file stays the (last-writer) default.

/// Keep a profileId safe as a filename suffix. profileIds are UUIDs, but be
/// defensive against anything the extension might report.
fn sanitize_profile_id(id: &str) -> String {
    id.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn relay_url_path_for(id: &str) -> PathBuf {
    relay_url_path().with_file_name(format!("relay-cdp-url-{}", sanitize_profile_id(id)))
}

fn relay_ext_profile_path_for(id: &str) -> PathBuf {
    relay_url_path().with_file_name(format!("relay-ext-profile-{}", sanitize_profile_id(id)))
}

/// Every profile whose extension worker is currently connected: `(id, email,
/// ws_url)`. Reads the per-profile sidecars next to `relay-cdp-url`. Powers
/// `chrome-use browsers` and `--browser` resolution.
pub fn list_relay_profiles() -> Vec<(String, Option<String>, String)> {
    let Some(dir) = relay_url_path().parent().map(|p| p.to_path_buf()) else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Some(suffix) = name.strip_prefix("relay-ext-profile-") else {
            continue;
        };
        // Resolve via the recorded id (not the filename suffix) so email rides along.
        let Some((id, email)) = std::fs::read_to_string(entry.path())
            .ok()
            .and_then(|s| parse_ext_profile(&s))
        else {
            continue;
        };
        // Pair with its live ws URL; skip if the endpoint file is gone (host died).
        let Some(ws) = std::fs::read_to_string(relay_url_path_for(suffix))
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| s.starts_with("ws://"))
        else {
            continue;
        };
        out.push((id, email, ws));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// Resolve a `--browser` selector to a profile's relay ws URL. Matches a
/// profileId (exact or prefix) or an email substring (case-insensitive).
/// Returns `Err` with the available list when nothing/ambiguous matches.
pub fn relay_url_for_browser(selector: &str) -> Result<String, String> {
    match_browser(&list_relay_profiles(), selector)
}

/// Pure selector→ws-url resolution (separated from the filesystem read so it can
/// be unit-tested). Matches a profileId (exact, then prefix) or an email
/// substring (case-insensitive). Exact id wins over prefix/email so a full id is
/// never ambiguous; otherwise multiple matches are an explicit error.
fn match_browser(
    profiles: &[(String, Option<String>, String)],
    selector: &str,
) -> Result<String, String> {
    let sel = selector.trim();
    let sel_lc = sel.to_lowercase();
    if let Some((_, _, ws)) = profiles.iter().find(|(id, _, _)| id == sel) {
        return Ok(ws.clone());
    }
    let matches: Vec<&(String, Option<String>, String)> = profiles
        .iter()
        .filter(|(id, email, _)| {
            id.starts_with(sel)
                || email
                    .as_deref()
                    .is_some_and(|e| e.to_lowercase().contains(&sel_lc))
        })
        .collect();
    match matches.as_slice() {
        [one] => Ok(one.2.clone()),
        [] => Err(format!(
            "--browser: no connected Chrome profile matches '{sel}'.{}",
            render_browser_list(profiles)
        )),
        many => Err(format!(
            "--browser: '{sel}' is ambiguous ({} profiles match) — use a longer id/email.{}",
            many.len(),
            render_browser_list(profiles)
        )),
    }
}

/// Human-readable list of connected profiles for error messages and `browsers`.
fn render_browser_list(profiles: &[(String, Option<String>, String)]) -> String {
    if profiles.is_empty() {
        return " (no profile-aware extension is connected — needs ab-connect ≥0.5.3)".to_string();
    }
    let mut s = String::from("\nConnected Chrome profiles:");
    for (id, email, _) in profiles {
        match email {
            Some(e) => s.push_str(&format!("\n  {e}  ({id})")),
            None => s.push_str(&format!("\n  {id}")),
        }
    }
    s
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
    // The profileId this host got from `hello` — so we can clean up our
    // per-profile sidecars on disconnect (issue #60).
    let mut bound_profile_id: Option<String> = None;
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
            // Record which profile is driving (issue #60), if the extension
            // reported one. Stored as JSON so `email` can be added when present.
            if let Some(id) = v.get("profileId").and_then(|x| x.as_str()) {
                let rec = serde_json::json!({
                    "id": id,
                    "email": v.get("profileEmail").and_then(|x| x.as_str()),
                });
                let rec = rec.to_string();
                let _ = std::fs::write(relay_ext_profile_path(), &rec);
                // Stable per-profile endpoint so `--browser <id>` can pin to THIS
                // profile regardless of who last clobbered the generic file.
                let _ = std::fs::write(relay_url_path_for(id), &url);
                let _ = std::fs::write(relay_ext_profile_path_for(id), &rec);
                bound_profile_id = Some(id.to_string());
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
    let _ = std::fs::remove_file(relay_ext_profile_path());
    // Drop this profile's per-profile sidecars so `browsers` doesn't list a dead
    // endpoint (issue #60).
    if let Some(id) = &bound_profile_id {
        let _ = std::fs::remove_file(relay_url_path_for(id));
        let _ = std::fs::remove_file(relay_ext_profile_path_for(id));
    }
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
    fn match_browser_resolves_by_id_email_prefix_and_errors() {
        let p = |id: &str, email: Option<&str>, ws: &str| {
            (id.to_string(), email.map(|e| e.to_string()), ws.to_string())
        };
        let profiles = vec![
            p("uuid-aaa", Some("me@example.com"), "ws://127.0.0.1:1/a"),
            p("uuid-bbb", None, "ws://127.0.0.1:2/b"),
            p("ccc-work", Some("work@corp.com"), "ws://127.0.0.1:3/c"),
        ];
        // exact id
        assert_eq!(
            match_browser(&profiles, "uuid-bbb").unwrap(),
            "ws://127.0.0.1:2/b"
        );
        // unique prefix
        assert_eq!(
            match_browser(&profiles, "ccc").unwrap(),
            "ws://127.0.0.1:3/c"
        );
        // email substring (case-insensitive)
        assert_eq!(
            match_browser(&profiles, "WORK@corp").unwrap(),
            "ws://127.0.0.1:3/c"
        );
        // exact id wins even though "uuid-aaa" is also a prefix of itself
        assert_eq!(
            match_browser(&profiles, "uuid-aaa").unwrap(),
            "ws://127.0.0.1:1/a"
        );
        // ambiguous prefix
        assert!(match_browser(&profiles, "uuid-").is_err());
        // no match
        let e = match_browser(&profiles, "nope").unwrap_err();
        assert!(e.contains("no connected Chrome profile matches 'nope'"));
        // empty set
        assert!(match_browser(&[], "anything")
            .unwrap_err()
            .contains("no connected"));
    }

    #[test]
    fn sanitize_profile_id_keeps_safe_chars() {
        assert_eq!(
            sanitize_profile_id("cedc799c-9af4-4718"),
            "cedc799c-9af4-4718"
        );
        assert_eq!(sanitize_profile_id("a/b c..d"), "a_b_c..d");
        assert_eq!(sanitize_profile_id("../etc/passwd"), ".._etc_passwd");
    }

    #[test]
    fn parse_ext_profile_reads_id_and_optional_email() {
        // id + email present
        assert_eq!(
            parse_ext_profile(r#"{"id":"uuid-1","email":"me@example.com"}"#),
            Some(("uuid-1".to_string(), Some("me@example.com".to_string())))
        );
        // id only (no identity permission) → email None
        assert_eq!(
            parse_ext_profile(r#"{"id":"uuid-2","email":null}"#),
            Some(("uuid-2".to_string(), None))
        );
        assert_eq!(
            parse_ext_profile(r#"{"id":"uuid-3"}"#),
            Some(("uuid-3".to_string(), None))
        );
        // blank email is treated as absent
        assert_eq!(
            parse_ext_profile(r#"{"id":"uuid-4","email":"  "}"#),
            Some(("uuid-4".to_string(), None))
        );
        // missing/blank id → None; malformed → None
        assert_eq!(parse_ext_profile(r#"{"email":"x@y.z"}"#), None);
        assert_eq!(parse_ext_profile(r#"{"id":"   "}"#), None);
        assert_eq!(parse_ext_profile("not json"), None);
    }

    #[test]
    fn profile_id_is_leeguoo_branded_and_distinct_from_the_stale_ones() {
        // The product is leeguoo.com's; the pwtk-era ids live ONLY in
        // OLD_PROFILE_IDS so cleanup keeps recognizing them.
        assert!(PROFILE_ID.starts_with("com.leeguoo."));
        assert!(!PROFILE_ID.contains("pwtk"));
        for old in OLD_PROFILE_IDS {
            assert_ne!(PROFILE_ID, *old);
            // substring collisions would corrupt `profiles list` detection
            assert!(!old.contains(PROFILE_ID) && !PROFILE_ID.contains(old));
        }
        // Fresh UUIDs: reusing the pwtk-era ones made macOS treat the fixed
        // payload as the already-approved (broken) one.
        assert!(!force_install_mobileconfig().contains("A1B2C3D4"));
    }

    #[test]
    fn mobileconfig_forcelist_uses_the_store_id_and_store_update_url() {
        let cfg = force_install_mobileconfig();
        // Chrome 149 blocks off-store force-installs on unmanaged machines; the
        // ONLY working silent path is Web Store id + Web Store update URL. The
        // stale shipped profile broke exactly this way (dev id + self-hosted
        // updates.xml).
        assert!(cfg.contains(&format!("{STORE_EXTENSION_ID};{UPDATE_URL}")));
        assert!(!cfg.contains(EXTENSION_ID), "dev id must not be force-installed");
        assert!(!cfg.contains("updates.xml"), "no self-hosted update feed");
        assert!(cfg.contains(PROFILE_ID));
        assert!(cfg.contains("leeguoo.com"));
    }

    #[test]
    fn store_url_points_at_the_published_store_build_not_the_dev_id() {
        // Must be the published Store id, not the dev/unpacked id, or
        // "Add to Chrome" 404s — that exact mix-up shipped a wrong link to a user.
        assert!(STORE_URL.contains(STORE_EXTENSION_ID));
        assert!(
            !STORE_URL.contains(EXTENSION_ID),
            "STORE_URL must not use the dev id"
        );
        assert!(STORE_URL.starts_with("https://chromewebstore.google.com/"));
        // The install-URL alias stays in sync.
        assert_eq!(STORE_INSTALL_URL, STORE_URL);
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

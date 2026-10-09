mod account;
mod autologin;
mod bwu_login;
mod chat;
mod choosebrowser;
mod color;
mod commands;
mod connect;
mod connection;
mod cookie_export;
mod doctor;
mod error_envelope;
mod findurl;
mod flags;
mod friction;
mod install;
mod jev;
mod mcp;
mod native;
mod opencli;
mod output;
mod ownership;
mod profiles;
mod read;
mod report;
mod session_title;
mod silence;
mod site;
mod skills;
mod test_runner;
#[cfg(test)]
mod test_utils;
mod upgrade;
mod upgrade_handoff;
mod validation;

use serde_json::{json, Value};
use std::env;
use std::fs;
use std::process::exit;

#[cfg(windows)]
use windows_sys::Win32::Foundation::CloseHandle;
#[cfg(windows)]
use windows_sys::Win32::System::Threading::OpenProcess;

use commands::{gen_id, parse_command, ParseError};
use connection::{
    cleanup_stale_files, get_socket_dir, is_pid_alive, restart_all_daemons, send_command,
    walk_daemons, DaemonOptions,
};
use flags::{clean_args, parse_flags, Flags};
use install::run_install;
use output::{
    print_command_help, print_help, print_response_with_opts, print_version, OutputOptions,
};
use upgrade::run_upgrade;

fn serialize_json_value(value: &serde_json::Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| {
        r#"{"success":false,"error":"Failed to serialize JSON response"}"#.to_string()
    })
}

fn print_json_value(value: serde_json::Value) {
    println!("{}", serialize_json_value(&value));
}

fn print_json_error(message: impl AsRef<str>) {
    print_json_value(error_envelope::error_value(message.as_ref()));
}

/// Print a command error (JSON or text, per `--json`) and exit 1.
fn fail_command(flags: &flags::Flags, msg: &str) -> ! {
    if flags.json {
        print_json_error(msg);
    } else {
        eprintln!("{} {msg}", color::error_indicator());
    }
    exit(1);
}

/// A pinned session whose profile's endpoint can't be resolved (#472).
fn pinned_profile_unresolvable(session: &str, id: &str, e: &impl std::fmt::Display) -> String {
    format!(
        "Session '{session}' is bound to Chrome profile {id}, but that profile's relay endpoint can't be determined: {e}. Not connecting to any other profile. Check `chrome-use browsers`, then retry, or start a new session with --session <name>."
    )
}

/// Relay recovery for a session pinned to a relay profile (#472).
///
/// Only that profile is waited for. Its sidecars being ambiguous (duplicate,
/// corrupt, unreadable) is refused at once rather than treated as "down", and
/// nothing is killed but this session's own stale daemon: no `pkill` of
/// native hosts (other profiles' hosts are healthy and in use) and no Chrome
/// launch (which profile directory a relay id lives in is not known here).
/// The pin survives the daemon being stopped, so if the profile does not come
/// back the next command is refused the same way instead of re-choosing.
fn recover_pinned_relay(flags: &mut Flags, id: &str) {
    use connect::ProfileEndpointError as E;
    let session = flags.session.clone();
    let probe = || connection::probe_daemon_healthy(&session, std::time::Duration::from_secs(3));
    match connect::relay_endpoint_for_profile(id) {
        Ok(_) => return,
        // A daemon still holding a live connection keeps it; otherwise refuse.
        Err(E::Ambiguous(e)) => {
            if probe() {
                return;
            }
            fail_command(flags, &pinned_profile_unresolvable(&session, id, &e));
        }
        Err(E::NotConnected(_)) => {}
    }
    // Without automatic recovery the daemon waits for the profile itself and
    // fails closed; a healthy daemon is left alone.
    if !flags.auto_connect
        || flags.force_launch
        || std::env::var("AGENT_BROWSER_NO_AUTO_RECONNECT").is_ok()
        || !connect::host_installed()
        || probe()
    {
        return;
    }
    let _relay_recovery_lock = match connection::lock_relay_recovery() {
        Ok(lock) => lock,
        Err(e) => fail_command(flags, &e),
    };
    match connect::relay_endpoint_for_profile(id) {
        Ok(_) => return,
        Err(E::Ambiguous(e)) => fail_command(flags, &pinned_profile_unresolvable(&session, id, &e)),
        Err(E::NotConnected(_)) => {}
    }
    // Runtime files only: the pin is never deleted or rewritten here.
    if let Err(e) = connection::stop_daemon_for_recovery(&session) {
        fail_command(flags, &e);
    }
    eprint!(
        "{} Chrome relay of profile {id} dropped — waiting for it to reconnect…",
        color::success_indicator()
    );
    let _ = std::io::Write::flush(&mut std::io::stderr());
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(45);
    loop {
        match connect::relay_endpoint_for_profile(id) {
            Ok(ws) => {
                eprintln!();
                flags.cdp = Some(ws);
                flags.auto_connect = false;
                return;
            }
            Err(E::NotConnected(_)) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(300));
            }
            Err(e) => {
                eprintln!();
                fail_command(flags, &pinned_profile_unresolvable(&session, id, &e));
            }
        }
    }
}

fn print_json_error_with_type(message: impl AsRef<str>, error_type: &str) {
    print_json_value(json!({
        "success": false,
        "error": message.as_ref(),
        "type": error_type,
        "code": error_type,
        "retryable": false,
    }));
}

fn should_send_hide_scrollbars_launch_option(
    cli_hide_scrollbars: bool,
    hide_scrollbars: bool,
) -> bool {
    cli_hide_scrollbars || !hide_scrollbars
}

fn apply_hide_scrollbars_launch_option(
    launch_cmd: &mut serde_json::Value,
    cli_hide_scrollbars: bool,
    hide_scrollbars: bool,
) {
    if should_send_hide_scrollbars_launch_option(cli_hide_scrollbars, hide_scrollbars) {
        launch_cmd["hideScrollbars"] = json!(hide_scrollbars);
    }
}

struct ParsedProxy {
    server: String,
    username: Option<String>,
    password: Option<String>,
}

fn parse_proxy(proxy_str: &str) -> ParsedProxy {
    let Some(protocol_end) = proxy_str.find("://") else {
        return ParsedProxy {
            server: proxy_str.to_string(),
            username: None,
            password: None,
        };
    };
    let protocol = &proxy_str[..protocol_end + 3];
    let rest = &proxy_str[protocol_end + 3..];

    let Some(at_pos) = rest.rfind('@') else {
        return ParsedProxy {
            server: proxy_str.to_string(),
            username: None,
            password: None,
        };
    };

    let creds = &rest[..at_pos];
    let server_part = &rest[at_pos + 1..];
    let server = format!("{}{}", protocol, server_part);

    let (username, password) = match creds.find(':') {
        Some(colon_pos) => {
            let u = &creds[..colon_pos];
            let p = &creds[colon_pos + 1..];
            (
                if u.is_empty() {
                    None
                } else {
                    Some(u.to_string())
                },
                if p.is_empty() {
                    None
                } else {
                    Some(p.to_string())
                },
            )
        }
        None => (
            if creds.is_empty() {
                None
            } else {
                Some(creds.to_string())
            },
            None,
        ),
    };

    ParsedProxy {
        server,
        username,
        password,
    }
}

#[derive(Debug, PartialEq, Eq)]
enum SessionCommandRoute {
    Lifecycle,
    Ownership,
}

/// Keep daemon lifecycle commands on the lifecycle path while ownership
/// commands remain CLI-local. This classifier prevents a broad `session`
/// intercept from making later subcommands unreachable.
fn session_command_route(sub: Option<&str>) -> SessionCommandRoute {
    match sub {
        Some("stop") | Some("prune") => SessionCommandRoute::Lifecycle,
        _ => SessionCommandRoute::Ownership,
    }
}

/// `session name [title]` — set or show this session's tab-group label.
///
/// The label is written to a sidecar so a tab opened after a daemon recycle
/// still carries it. Tabs that are *already* open keep the old label until the
/// extension renames the group, so the daemon is asked to do that; when it
/// cannot (no daemon yet, or an older extension), the reply says which tabs the
/// new name applies to rather than implying it applied to all of them.
/// Resolve a profile named in `~/.chrome-use/config.json` (`profiles`) to its
/// relay endpoint, or stop with the reason — the user wrote that rule, so a
/// silent fallback to another profile would be the wrong account (#437).
fn configured_profile_ws(selector: &str, why: &str) -> String {
    match profiles::resolve_connected(selector) {
        Ok(row) => row.ws.unwrap_or_default(),
        Err(msg) => {
            eprintln!(
                "{} {msg} (chosen by {why} in the chrome-use config)",
                color::error_indicator()
            );
            exit(1);
        }
    }
}

/// Verbs whose parsed command can carry a url to navigate to. Only these are
/// parsed early: other parsers may read stdin (`fill --stdin`, `eval
/// --stdin`), which must happen exactly once, later.
const NAVIGATING_VERBS: &[&str] = &["open", "goto", "navigate", "tab", "tabs", "a11y"];

/// The url a parsed command navigates to — the same JSON the daemon receives,
/// so the ChooseBrowser guard checks exactly the url that is sent.
fn navigation_url(parsed: &serde_json::Value) -> Option<String> {
    match parsed.get("action")?.as_str()? {
        "navigate" | "tab_new" | "a11y" => parsed.get("url")?.as_str().map(str::to_string),
        _ => None,
    }
}

/// Every url this invocation will navigate to, in order: the command's own,
/// then each `batch` step's. Read off the formal parse (no second url
/// heuristic) and normalised the way the daemon guard normalises it.
///
/// Only the commands that *navigate* carry one. A `snapshot` or a `click` acts
/// on whatever the session already has open, so consulting a routing rule there
/// would answer a question nobody asked.
fn navigation_urls(
    clean: &[String],
    flags: &Flags,
    batch_steps: Option<&[Vec<String>]>,
) -> Vec<String> {
    let navigates = |argv: &[String]| {
        argv.first()
            .is_some_and(|v| NAVIGATING_VERBS.contains(&v.as_str()))
    };
    let mut out = Vec::new();
    if navigates(clean) {
        if let Some(u) = commands::parse_command(clean, flags)
            .ok()
            .as_ref()
            .and_then(navigation_url)
        {
            out.push(u);
        }
    }
    for step in batch_steps.unwrap_or(&[]) {
        if navigates(step) {
            if let Some(u) = commands::parse_batch_step(step, flags)
                .ok()
                .as_ref()
                .and_then(navigation_url)
            {
                out.push(u);
            }
        }
    }
    out.into_iter()
        .filter_map(|u| profiles::guard_url(&u))
        .collect()
}

/// Turn a `--remember` invocation into the request to hand ChooseBrowser, or
/// into the reason it cannot be one.
///
/// Every check lives here, before the navigation runs, because ChooseBrowser
/// drops a malformed request **without showing a dialog** — so a mistake caught
/// later would look exactly like the user declining to save. The one thing this
/// feature must never do is stay quiet about not working.
///
/// Returns the `choosebrowser://` url plus the host it is about, for the line
/// the user sees.
///
/// `on_macos` is a parameter rather than a `cfg!` so the platform refusal is
/// testable — and so the other checks stay reachable on a Linux CI runner,
/// which a `cfg!` made them not: every test hit the platform branch instead of
/// what it meant to exercise.
fn remember_request(
    argv: &[String],
    target_url: Option<&str>,
    browser_selector: Option<&str>,
    no_choosebrowser: bool,
    profile_email: Option<&str>,
    local_state: Option<&str>,
    on_macos: bool,
) -> Result<(String, String), String> {
    if !on_macos {
        return Err(
            "--remember needs ChooseBrowser, which is macOS-only. Nothing was recorded.".into(),
        );
    }
    if no_choosebrowser {
        return Err(
            "--remember and --no-choosebrowser ask for opposite things — one writes a \
             ChooseBrowser rule, the other ignores them. Drop whichever you did not mean."
                .into(),
        );
    }
    // Deliberately explicit-only. Remembering the profile *we* guessed would
    // turn one inference into a permanent rule the user never stated.
    let Some(selector) = browser_selector else {
        return Err(
            "--remember records which Chrome profile a site belongs to, so it needs \
             you to name one: add --browser <id|email>. Run `chrome-use browsers` for the list."
                .into(),
        );
    };
    let verb = argv.first().map(String::as_str).unwrap_or("");
    let Some(url) = target_url else {
        return Err(format!(
            "--remember applies to a command that opens a url — `open`, `goto`, `navigate` or `tab new <url>`. \
             `{verb}` acts on whatever the session already has open, so there is no site to \
             write a rule for."
        ));
    };
    let host = url::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
        .ok_or_else(|| format!("--remember: could not read a hostname out of '{url}'."))?;

    // The relay knows a profile by uuid; ChooseBrowser writes a gaia id. Email
    // is the only field both sides carry, so a profile that never granted the
    // extension's `identity` permission cannot be named in a rule at all.
    let Some(email) = profile_email.map(str::trim).filter(|e| !e.is_empty()) else {
        return Err(format!(
            "--remember: --browser '{selector}' matched a connected profile with no signed-in \
             account, and a rule has to point at an account. Grant the ab-connect extension \
             the identity permission in that profile, or add the rule in ChooseBrowser directly."
        ));
    };
    let Some(local_state) = local_state else {
        return Err(
            "--remember: could not read Chrome's profile registry (Local State), which \
             is where the portable profile key comes from."
                .into(),
        );
    };
    let Some(key) = choosebrowser::portable_key_for_email(local_state, email) else {
        return Err(format!(
            "--remember: '{email}' is not in Chrome's profile registry on this machine, so \
             there is no stable key to write into a rule. Add the rule in ChooseBrowser directly."
        ));
    };
    // Domain-only on purpose: a rule scoped to the exact path this command
    // happened to open would stop applying on the next page of the same site.
    let Some(request) = choosebrowser::remember_url(&host, None, &key) else {
        return Err(format!(
            "--remember: '{host}' is not a plain hostname, and ChooseBrowser drops a request \
             it cannot parse without telling anyone. Add the rule in ChooseBrowser directly."
        ));
    };
    Ok((request, host))
}

/// Hand the request to ChooseBrowser and say what was — and was not — done.
///
/// `open` exiting 0 means the url reached a handler, nothing more. There is no
/// success callback by design, so the wording stops at "asked": the user reads
/// the outcome off ChooseBrowser's own dialog.
fn send_remember_request(request: &str, host: &str, profile: &str) {
    let launched = std::process::Command::new("/usr/bin/open")
        .arg(request)
        .status();
    match launched {
        Ok(st) if st.success() => eprintln!(
            "{} asked ChooseBrowser to route {} to {} from now on — confirm in its dialog. \
             Nothing is saved unless you do.",
            color::dim("·"),
            host,
            profile,
        ),
        // A non-zero exit means no application claimed `choosebrowser://`.
        // That is the common case today, not an edge case: the ChooseBrowser
        // builds in the store register only http/https, and the scheme ships in
        // a later version. So "is it installed?" would be the wrong question
        // for most people who see this — they have it, just not that version.
        Ok(_) | Err(_) => eprintln!(
            "{} --remember: nothing on this Mac handles choosebrowser:// urls, so no rule was \
             proposed for {host}. ChooseBrowser is either not installed or older than {} — \
             update it, or add the rule in ChooseBrowser directly.",
            color::warning_indicator(),
            choosebrowser::MIN_APP_VERSION_FOR_RULE_REQUESTS,
        ),
    }
}

fn run_session_name(session: &str, json_mode: bool, zh: bool) {
    let requested: Vec<String> = std::env::args()
        .skip_while(|a| a != "name")
        .skip(1)
        .collect();
    // `--clear` drops the label so the group falls back to the session id.
    // Without it a name could be set but never taken back, which matters when
    // a long-lived session moves on to unrelated work and the old label would
    // otherwise keep describing the wrong task.
    let clearing = requested.iter().any(|a| a == "--clear");
    let requested = requested
        .iter()
        .filter(|a| !a.starts_with("--"))
        .cloned()
        .collect::<Vec<_>>()
        .join(" ");

    if clearing {
        let previous = session_title::display_name(session);
        session_title::clear_title(session);
        let restored = session.to_string();
        let renamed = connection::rename_session_group(session, &previous, &restored);
        if json_mode {
            println!(
                "{}",
                serde_json::json!({
                    "ok": true,
                    "session": session,
                    "name": serde_json::Value::Null,
                    "renamedExistingGroup": renamed,
                })
            );
        } else if zh {
            println!("✓ 会话 '{session}' 的标签组名已清除，恢复显示会话 id");
        } else {
            println!("✓ tab group for session '{session}' shows the session id again");
        }
        return;
    }

    if requested.trim().is_empty() {
        let current = session_title::title_of(session);
        if json_mode {
            println!(
                "{}",
                serde_json::json!({
                    "session": session,
                    "name": current,
                    "label": session_title::display_name(session),
                })
            );
        } else {
            match current {
                Some(t) => println!("{t}"),
                None if zh => println!(
                    "（未命名，标签组显示会话 id：{session}）\n  设置：chrome-use session name \"🔎 任务名\""
                ),
                None => println!(
                    "(unnamed — the tab group shows the session id: {session})\n  set one with: chrome-use session name \"🔎 task name\""
                ),
            }
        }
        return;
    }

    let previous = session_title::display_name(session);
    let title = match session_title::set_title(session, &requested) {
        Ok(t) => t,
        Err(e) => {
            if json_mode {
                print_json_error(e);
            } else {
                eprintln!("{}", color::red(&e));
            }
            exit(1);
        }
    };

    // Best effort: an existing group only changes label if a daemon is up and
    // the live extension knows the command. Anything else leaves already-open
    // tabs under the old label, which the caller is told rather than left to
    // discover.
    let renamed = connection::rename_session_group(session, &previous, &title);

    if json_mode {
        println!(
            "{}",
            serde_json::json!({
                "ok": true,
                "session": session,
                "name": title,
                "renamedExistingGroup": renamed,
            })
        );
        return;
    }
    if zh {
        println!("✓ 会话 '{session}' 的标签组名为 {title}");
        if !renamed {
            println!(
                "  {}",
                color::dim("已经打开的标签仍在原来的组里；新开的标签会用这个名字。")
            );
        }
    } else {
        println!("✓ tab group for session '{session}' is now {title}");
        if !renamed {
            println!(
                "  {}",
                color::dim("already-open tabs stay in the old group; new tabs use this name.")
            );
        }
    }
}

/// `session <handoff|resume|status|list>` — ownership + human handoff (#89,
/// ported from ego-lite). CLI-local: only touches the `.owner` sidecar.
fn run_session_ownership(sub: Option<&str>, session: &str, json_mode: bool) {
    use ownership::{hand_off, owner_of, resume, session_flag_suffix, Owner};
    let zh = connect::ui_zh();
    match sub {
        // `session name [title]` — label this session's tab group with
        // something the human can read in their own browser, instead of the
        // routing id. With no argument it reports the current label.
        Some("name") => run_session_name(session, json_mode, zh),
        Some("handoff") => {
            if let Err(e) = hand_off(session) {
                if json_mode {
                    print_json_error(format!("handoff failed: {e}"));
                } else {
                    eprintln!("{}", color::red(&format!("handoff failed: {e}")));
                }
                exit(1);
            }
            if json_mode {
                println!(
                    "{}",
                    serde_json::json!({"ok": true, "session": session, "owner": "user"})
                );
            } else if zh {
                println!("✓ 会话 '{session}' 已交接给用户——你现在可以登录 / 过验证码 / 手动操作。");
                println!(
                    "  期间 agent 不会驱动这个会话。完事后跑：chrome-use session resume{}",
                    session_flag_suffix(session)
                );
            } else {
                println!("✓ Session '{session}' handed off to you — log in / solve the captcha / do the manual step.");
                println!(
                    "  The agent won't drive this session meanwhile. When done: chrome-use session resume{}",
                    session_flag_suffix(session)
                );
            }
        }
        Some("resume") | Some("takeover") => {
            if let Err(e) = resume(session) {
                if json_mode {
                    print_json_error(format!("resume failed: {e}"));
                } else {
                    eprintln!("{}", color::red(&format!("resume failed: {e}")));
                }
                exit(1);
            }
            if json_mode {
                println!(
                    "{}",
                    serde_json::json!({"ok": true, "session": session, "owner": "agent"})
                );
            } else if zh {
                println!("✓ 会话 '{session}' 已由 agent 接管，可以继续自动化了。");
            } else {
                println!("✓ Session '{session}' resumed by the agent — automation can continue.");
            }
        }
        Some("list") => {
            let inv = walk_daemons();
            if json_mode {
                let rows: Vec<_> = inv
                    .sessions
                    .iter()
                    .map(|s| {
                        serde_json::json!({
                            "name": s.name,
                            "pid": s.pid,
                            "owner": owner_of(&s.name).as_str(),
                        })
                    })
                    .collect();
                println!("{}", serde_json::json!({"ok": true, "sessions": rows}));
            } else if inv.sessions.is_empty() {
                println!(
                    "{}",
                    if zh {
                        "没有活动会话。"
                    } else {
                        "No active sessions."
                    }
                );
            } else {
                for s in &inv.sessions {
                    let owner = owner_of(&s.name);
                    let tag = match owner {
                        Owner::User => {
                            if zh {
                                "已交接给用户"
                            } else {
                                "handed off to user"
                            }
                        }
                        Owner::Agent => "agent",
                    };
                    println!("- {}  (pid {})  —  {}", s.name, s.pid, tag);
                }
            }
        }
        None | Some("status") => {
            let owner = owner_of(session);
            if json_mode {
                println!(
                    "{}",
                    serde_json::json!({"ok": true, "session": session, "owner": owner.as_str()})
                );
            } else {
                match owner {
                    Owner::User => {
                        if zh {
                            println!("会话 '{session}'：已交接给用户（agent 暂不驱动）。");
                            println!(
                                "  取回控制：chrome-use session resume{}",
                                session_flag_suffix(session)
                            );
                        } else {
                            println!("Session '{session}': handed off to the user (agent paused).");
                            println!(
                                "  Take it back: chrome-use session resume{}",
                                session_flag_suffix(session)
                            );
                        }
                    }
                    Owner::Agent => {
                        if zh {
                            println!("会话 '{session}'：由 agent 控制。");
                        } else {
                            println!("Session '{session}': owned by the agent.");
                        }
                    }
                }
            }
        }
        Some(other) => {
            let msg = format!(
                "unknown `session` subcommand '{other}'. Use: handoff | resume | status | list | stop | prune"
            );
            if json_mode {
                print_json_error(&msg);
            } else {
                eprintln!("{}", color::red(&msg));
            }
            exit(1);
        }
    }
}

fn run_profiles(json_mode: bool) {
    use crate::native::cdp::chrome::{find_chrome_user_data_dir, list_chrome_profiles};

    let user_data_dir = match find_chrome_user_data_dir() {
        Some(dir) => dir,
        None => {
            if json_mode {
                print_json_error("No Chrome user data directory found");
            } else {
                eprintln!("{}", color::red("No Chrome user data directory found"));
            }
            exit(1);
        }
    };

    let profiles = list_chrome_profiles(&user_data_dir);
    if profiles.is_empty() {
        if json_mode {
            print_json_value(json!({
                "success": true,
                "data": []
            }));
        } else {
            println!("No Chrome profiles found");
        }
        return;
    }

    if json_mode {
        let items: Vec<serde_json::Value> = profiles
            .iter()
            .map(|p| {
                json!({
                    "directory": p.directory,
                    "name": p.name
                })
            })
            .collect();
        print_json_value(json!({
            "success": true,
            "data": items
        }));
    } else {
        println!(
            "{} ({}):\n",
            color::bold("Chrome profiles"),
            user_data_dir.display()
        );
        for p in &profiles {
            println!(
                "  {}  {}",
                color::bold(&p.directory),
                color::dim(&format!("({})", p.name))
            );
        }
    }
}

fn run_cookies_export(args: &[String], flags: &Flags) {
    // Source profile comes from `--from <profile>`, falling back to the global
    // `--profile` (which the flag parser has already moved into flags.profile).
    let from = args
        .iter()
        .position(|a| a == "--from")
        .and_then(|i| args.get(i + 1))
        .map(|s| s.as_str())
        .or(flags.profile.as_deref());
    let profile = match from {
        Some(p) => p,
        None => {
            let msg = "cookies export needs a source profile: cookies export --from <profile> [--domain <d>]";
            if flags.json {
                print_json_error(msg);
            } else {
                eprintln!("{} {}", color::error_indicator(), msg);
            }
            exit(1);
        }
    };
    let domain = args
        .iter()
        .position(|a| a == "--domain")
        .and_then(|i| args.get(i + 1))
        .map(|s| s.as_str());

    match cookie_export::export_cookies(profile, domain) {
        Ok(cookies) => {
            if flags.json {
                print_json_value(json!({ "success": true, "data": cookies }));
            } else {
                // A JSON array ready for `cookies set --curl <file>`.
                println!(
                    "{}",
                    serde_json::to_string(&cookies).unwrap_or_else(|_| "[]".to_string())
                );
                eprintln!(
                    "{}",
                    color::dim(&format!(
                        "{} cookies exported from \"{}\"",
                        cookies.len(),
                        profile
                    ))
                );
            }
        }
        Err(e) => {
            if flags.json {
                print_json_error(&e);
            } else {
                eprintln!("{} {}", color::error_indicator(), e);
            }
            exit(1);
        }
    }
}

/// What to say when the daemon is stopped but its tabs could not be closed.
///
/// The old text asked the user to "reconnect this session to its original
/// browser with the original connection options" — but the daemon has just
/// been killed, and the usual reason the tabs are unreachable is that the
/// original browser endpoint no longer exists (a relay restart after an
/// upgrade, a Chrome restart). Advice that cannot be followed reads as a dead
/// end (#256). Say what is true instead: stopped; some tabs remain; here is
/// how to either pick them back up or let go of the record.
fn session_stop_incomplete_message(session: &str, error: &str, tabs: usize) -> String {
    let tab_word = if tabs == 1 { "tab" } else { "tabs" };
    format!(
        "stopped session daemon {session}, but {tabs} {tab_word} it created could not be closed: {error}. \
         This usually means the browser endpoint changed since those tabs were opened (an upgrade or a \
         Chrome restart), not that anything is wrong with them — they are still open. \
         Run `chrome-use open <url>` in this session to pick them back up, or \
         `chrome-use session stop {session} --force` to drop the record and leave them as they are."
    )
}

/// What `--force` actually did, including what it did not do.
///
/// #309 reported running this against a session name that kept refusing
/// commands, reading "dropped its record", and concluding the name should now
/// be clear — it was not. `--force` drops the CLI's record; the tabs stay open,
/// and on the extension relay a session's tabs live in a tab group named after
/// the session, so a fresh daemon under the same name meets them again. If one
/// of them is a tab whose renderer is still busy, the name keeps behaving as if
/// nothing was cleared. Saying so is the difference between a command that
/// looks broken and one whose limits are known.
fn session_stop_forced_note(session: &str, tabs: usize) -> String {
    let tab_word = if tabs == 1 { "tab" } else { "tabs" };
    format!(
        "stopped session daemon {session} and dropped its record of {tabs} {tab_word} — \
         they were not closed and stay open in the browser; close them by hand if you no longer want them. \
         Note this clears the record, not the tabs: a new daemon under the name `{session}` finds the same \
         tabs again, so if that name was refusing commands because one of them is busy, close that tab or \
         use a different --session name."
    )
}

/// Why `target`'s daemon is being stopped, for its own agent to read on its
/// next command — `None` when the caller is stopping its own session. Another
/// session's `session stop <name>` or `session prune` closes that session's
/// tabs while its agent may be mid-task; without this its next command opens
/// a blank tab and answers as if nothing happened.
fn stopped_from_outside_reason(target: &str, own: &str, command: &str) -> Option<String> {
    if target == own {
        return None;
    }
    Some(format!(
        "`chrome-use {command}` run from session `{own}` stopped it at {}",
        chrono::Local::now().format("%H:%M:%S")
    ))
}

fn run_session_lifecycle(args: &[String], session: &str, json_mode: bool) {
    let subcommand = args.get(1).map(|s| s.as_str());

    match subcommand {
        // Stop a specific session daemon (issue #48). Graceful: kill_stale_daemon
        // sends SIGTERM first, so the daemon's shutdown handler runs `close()` and
        // tidies the tabs IT created (its tab group) before exiting.
        Some("stop") => {
            let force = args.iter().skip(2).any(|a| a == "--force");
            let target = args
                .iter()
                .skip(2)
                .find(|a| !a.starts_with("--"))
                .map(|s| s.as_str())
                .unwrap_or(session);
            if !validation::is_valid_session_name(target) {
                let msg = validation::session_name_error(target);
                if json_mode {
                    print_json_error_with_type(msg, "invalid_session_name");
                } else {
                    eprintln!("{} {}", color::error_indicator(), msg);
                }
                exit(1);
            }
            let stopped = (|| -> Result<(), String> {
                let _lock = connection::lock_session_lifecycle(target)?;
                if let Some(reason) = stopped_from_outside_reason(target, session, "session stop") {
                    native::daemon::mark_session_closed(target, &reason);
                }
                connection::kill_stale_daemon(target);
                if connection::has_created_targets(target) {
                    let _ = native::browser::DAEMON_SESSION.set(target.to_string());
                    tokio::runtime::Runtime::new()
                        .map_err(|e| e.to_string())?
                        .block_on(async {
                            tokio::time::timeout(
                                std::time::Duration::from_secs(20),
                                native::browser::close_persisted_session_tabs(target),
                            )
                            .await
                            .map_err(|_| "timed out reconnecting for tab cleanup".to_string())?
                        })?;
                }
                Ok(())
            })();
            if let Err(error) = stopped {
                let tabs = connection::created_target_count(target);
                if force {
                    // The daemon is already gone; only the claim on its tabs
                    // remains, and that claim cannot be exercised against a
                    // browser we cannot reach. Forget it. Nothing is closed —
                    // deletion rights come from the record, and we are giving
                    // the record up, not using it.
                    if let Err(e) = connection::forget_created_targets(target) {
                        eprintln!(
                            "{} could not drop the ownership record: {e}",
                            color::error_indicator()
                        );
                        exit(1);
                    }
                    // A stopped session's carried tab ids must not come back.
                    if let Err(e) = connection::remove_carried_tabs(target) {
                        eprintln!("{} session stopped, but {e}", color::error_indicator());
                        exit(1);
                    }
                    let note = session_stop_forced_note(target, tabs);
                    if json_mode {
                        print_json_value(
                            json!({ "success": true, "data": { "stopped": target, "forgottenTabs": tabs } }),
                        );
                    } else {
                        println!("{} {note}", color::success_indicator());
                    }
                    return;
                }
                let message = session_stop_incomplete_message(target, &error, tabs);
                if json_mode {
                    print_json_error(&message);
                } else {
                    eprintln!("{} {}", color::error_indicator(), message);
                }
                exit(1);
            }
            // The session ended: its carried tab ids (#473) must not come back
            // on the next daemon. Not removable means not cleanly stopped.
            if let Err(e) = connection::remove_carried_tabs(target) {
                let message = format!("session stopped, but {e}");
                if json_mode {
                    print_json_error(&message);
                } else {
                    eprintln!("{} {}", color::error_indicator(), message);
                }
                exit(1);
            }
            if json_mode {
                print_json_value(json!({ "success": true, "data": { "stopped": target } }));
            } else {
                println!(
                    "{} stopped session daemon: {}",
                    color::success_indicator(),
                    target
                );
            }
        }
        // Reclaim ALL session daemons now (issue #48) — for clearing the pile of
        // idle daemons left after a round of automation/debugging without waiting
        // for the idle timeout. Each is stopped gracefully (closes its own tabs);
        // they respawn clean on next use. The `__nm-host` relay is not a tracked
        // session daemon, so the extension/live-Chrome connection survives.
        Some("prune") => {
            let sessions: Vec<String> = walk_daemons()
                .sessions
                .into_iter()
                .map(|s| s.name)
                .collect();
            for s in &sessions {
                if let Some(reason) = stopped_from_outside_reason(s, session, "session prune") {
                    native::daemon::mark_session_closed(s, &reason);
                }
                connection::kill_stale_daemon(s);
            }
            let unremoved: Vec<String> = sessions
                .iter()
                .filter_map(|s| connection::remove_carried_tabs(s).err())
                .collect();
            if !unremoved.is_empty() {
                let message = format!("sessions pruned, but {}", unremoved.join("; "));
                if json_mode {
                    print_json_error(&message);
                } else {
                    eprintln!("{} {}", color::error_indicator(), message);
                }
                exit(1);
            }
            if json_mode {
                print_json_value(json!({ "success": true, "data": { "pruned": sessions } }));
            } else if sessions.is_empty() {
                println!("No session daemons to prune");
            } else {
                println!(
                    "{} pruned {} session daemon(s): {}",
                    color::success_indicator(),
                    sessions.len(),
                    sessions.join(", ")
                );
            }
        }
        _ => unreachable!("session lifecycle route only accepts stop or prune"),
    }
}

/// `chrome-use daemon <restart|status>` — manage the per-session daemon workers
/// without resorting to `pgrep`/`kill`. `restart` clears corrupted or
/// cross-leaked daemon state (e.g. after a mid-session `chrome-use upgrade`
/// where stale tab handles bleed across sessions, issue #20) by killing every
/// session worker. The Chrome-launched `__nm-host` native-messaging bridge is
/// NOT a tracked session daemon, so the extension relay survives a restart —
/// the next command spins up a fresh, clean daemon against the same live Chrome.
fn run_daemon(args: &[String], json_mode: bool) {
    match args.get(1).map(|s| s.as_str()) {
        Some("restart") => {
            let stopped = restart_all_daemons();
            let relay_up = connect::relay_url().is_some();
            if json_mode {
                print_json_value(json!({
                    "success": true,
                    "data": { "stopped": stopped, "count": stopped.len(), "relay": relay_up },
                }));
            } else if stopped.is_empty() {
                println!("No session daemons running — nothing to restart.");
                if relay_up {
                    println!(
                        "{}",
                        color::dim("Extension relay still up; next command starts a fresh daemon.")
                    );
                }
            } else {
                for s in &stopped {
                    println!("{} Stopped daemon: {}", color::green("✓"), s);
                }
                println!(
                    "{}",
                    color::dim(if relay_up {
                        "Extension relay (__nm-host) left running; next command starts a fresh daemon."
                    } else {
                        "Next command starts a fresh daemon."
                    })
                );
            }
        }
        Some("status") | Some("list") => {
            let inventory = walk_daemons();
            let relay_up = connect::relay_url().is_some();
            if json_mode {
                let sessions: Vec<_> = inventory
                    .sessions
                    .iter()
                    .map(|s| json!({ "name": s.name, "pid": s.pid, "version": s.version }))
                    .collect();
                print_json_value(json!({
                    "success": true,
                    "data": { "sessions": sessions, "relay": relay_up },
                }));
            } else if inventory.sessions.is_empty() {
                println!("No session daemons running.");
                if relay_up {
                    println!("{}", color::dim("Extension relay (__nm-host): up"));
                }
            } else {
                println!("Session daemons:");
                for s in &inventory.sessions {
                    let ver = s
                        .version
                        .as_deref()
                        .map(|v| format!(" {}", color::dim(&format!("(v{})", v))))
                        .unwrap_or_default();
                    println!("  {} pid {}{}", s.name, s.pid, ver);
                }
                if relay_up {
                    println!("{}", color::dim("Extension relay (__nm-host): up"));
                }
            }
        }
        other => {
            eprintln!(
                "{} usage: chrome-use daemon <restart|status>",
                color::error_indicator()
            );
            if let Some(unknown) = other {
                eprintln!(
                    "{}",
                    color::dim(&format!("  unknown subcommand: {}", unknown))
                );
            }
            exit(2);
        }
    }
}

/// `chrome-use status` — one daemon-free health snapshot covering the installed
/// CLI, real-Chrome extension relay, driving profile, and current session.
/// Keeping this path local means it still answers when a session daemon is the
/// component that is stuck (issue #145).
fn run_status(session: &str, json_mode: bool) {
    let inventory = walk_daemons();
    let host_report = connect::native_host_report();
    let host_installed = !host_report.manifests.is_empty();
    let host_healthy = host_report.is_healthy();
    let relay_up = connect::relay_is_responsive();
    // Printed next to the driving profile below, so it must be that profile's
    // version, not the last `hello` writer's (#319).
    let extension_version = relay_up.then(connect::relay_ext_version_driving).flatten();
    let profile = relay_up.then(connect::driving_profile).flatten();
    let current = inventory.sessions.iter().find(|item| item.name == session);

    if json_mode {
        let sessions: Vec<_> = inventory
            .sessions
            .iter()
            .map(|item| {
                json!({
                    "name": item.name,
                    "pid": item.pid,
                    "version": item.version,
                })
            })
            .collect();
        let current_session = current
            .map(|item| {
                json!({
                    "name": item.name,
                    "running": true,
                    "pid": item.pid,
                    "version": item.version,
                })
            })
            .unwrap_or_else(|| {
                json!({
                    "name": session,
                    "running": false,
                    "pid": null,
                    "version": null,
                })
            });
        let (profile_id, profile_email) = profile
            .as_ref()
            .map(|(id, email)| (Some(id.as_str()), email.as_deref()))
            .unwrap_or((None, None));
        print_json_value(json!({
            "success": true,
            "data": {
                "cliVersion": env!("CARGO_PKG_VERSION"),
                "extension": {
                    "hostInstalled": host_installed,
                    "hostHealthy": host_healthy,
                    "relayUp": relay_up,
                    "expectedVersion": env!("AB_CONNECT_VERSION"),
                    "liveVersion": extension_version,
                    "profileId": profile_id,
                    "profileEmail": profile_email,
                },
                "currentSession": current_session,
                "sessions": sessions,
            }
        }));
        return;
    }

    println!("chrome-use {}", env!("CARGO_PKG_VERSION"));
    println!(
        "Extension relay: {}{}",
        if relay_up { "up" } else { "down" },
        if !host_installed {
            " (native host not installed)"
        } else if !host_healthy {
            " (native host launcher broken)"
        } else {
            ""
        }
    );
    println!(
        "  extension: live {}, expected {}",
        extension_version.as_deref().unwrap_or("unknown"),
        env!("AB_CONNECT_VERSION")
    );
    if let Some((id, email)) = profile {
        println!(
            "  profile: {}",
            connect::profile_label(&id, email.as_deref())
        );
    } else {
        println!("  profile: unknown");
    }

    if let Some(item) = current {
        let version = item
            .version
            .as_deref()
            .map(|value| format!(", v{value}"))
            .unwrap_or_default();
        println!(
            "Current session: {} (running, pid {}{})",
            item.name, item.pid, version
        );
    } else {
        println!("Current session: {} (not running)", session);
    }

    if inventory.sessions.is_empty() {
        println!("Session daemons: none");
    } else {
        println!("Session daemons:");
        for item in &inventory.sessions {
            let version = item
                .version
                .as_deref()
                .map(|value| format!(" (v{value})"))
                .unwrap_or_default();
            println!("  {} pid {}{}", item.name, item.pid, version);
        }
    }
    if !relay_up {
        println!(
            "{}",
            color::dim("Run `chrome-use extension status`, then `chrome-use extension connect`.")
        );
    }
}

fn get_dashboard_pid_path() -> std::path::PathBuf {
    get_socket_dir().join("dashboard.pid")
}

fn run_dashboard_start(port: u16, json_mode: bool) {
    let pid_path = get_dashboard_pid_path();

    // Check if already running
    if let Ok(pid_str) = fs::read_to_string(&pid_path) {
        if let Ok(pid) = pid_str.trim().parse::<u32>() {
            if is_pid_alive(pid) {
                if json_mode {
                    print_json_value(json!({
                        "success": true,
                        "data": { "port": port, "pid": pid, "already_running": true },
                    }));
                } else {
                    println!("Dashboard already running at http://localhost:{}", port);
                }
                return;
            }
        }
        let _ = fs::remove_file(&pid_path);
    }

    let socket_dir = get_socket_dir();
    if !socket_dir.exists() {
        let _ = fs::create_dir_all(&socket_dir);
    }

    let exe_path = match env::current_exe() {
        Ok(p) => p.canonicalize().unwrap_or(p),
        Err(e) => {
            if json_mode {
                print_json_error(format!("Failed to get executable path: {}", e));
            } else {
                eprintln!(
                    "{} Failed to get executable path: {}",
                    color::error_indicator(),
                    e
                );
            }
            exit(1);
        }
    };

    let mut cmd = std::process::Command::new(&exe_path);
    cmd.env("AGENT_BROWSER_DASHBOARD", "1")
        .env("AGENT_BROWSER_DASHBOARD_PORT", port.to_string());

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
    }

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x00000200;
        const DETACHED_PROCESS: u32 = 0x00000008;
        cmd.creation_flags(CREATE_NEW_PROCESS_GROUP | DETACHED_PROCESS);
    }

    match cmd
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(child) => {
            let pid = child.id();
            let _ = fs::write(&pid_path, pid.to_string());

            if json_mode {
                print_json_value(json!({
                    "success": true,
                    "data": { "port": port, "pid": pid },
                }));
            } else {
                println!("Dashboard started at http://localhost:{}", port);
            }
        }
        Err(e) => {
            if json_mode {
                print_json_error(format!("Failed to start dashboard: {}", e));
            } else {
                eprintln!(
                    "{} Failed to start dashboard: {}",
                    color::error_indicator(),
                    e
                );
            }
            exit(1);
        }
    }
}

fn run_dashboard_stop(json_mode: bool) {
    let pid_path = get_dashboard_pid_path();

    let pid_str = match fs::read_to_string(&pid_path) {
        Ok(s) => s,
        Err(_) => {
            if json_mode {
                print_json_value(
                    json!({ "success": true, "data": { "stopped": false, "reason": "not running" } }),
                );
            } else {
                println!("Dashboard is not running");
            }
            return;
        }
    };

    let pid: u32 = match pid_str.trim().parse() {
        Ok(p) => p,
        Err(_) => {
            let _ = fs::remove_file(&pid_path);
            if json_mode {
                print_json_value(
                    json!({ "success": true, "data": { "stopped": false, "reason": "invalid pid" } }),
                );
            } else {
                println!("Dashboard is not running");
            }
            return;
        }
    };

    #[cfg(unix)]
    {
        unsafe {
            libc::kill(pid as i32, libc::SIGTERM);
        }
    }
    #[cfg(windows)]
    {
        unsafe {
            let handle = OpenProcess(1, 0, pid); // PROCESS_TERMINATE = 1
            if handle != 0 {
                windows_sys::Win32::System::Threading::TerminateProcess(handle, 0);
                CloseHandle(handle);
            }
        }
    }

    let _ = fs::remove_file(&pid_path);

    if json_mode {
        print_json_value(json!({ "success": true, "data": { "stopped": true } }));
    } else {
        println!("{} Dashboard stopped", color::green("✓"));
    }
}

/// A live session `close --all` would close, as shown when it refuses.
#[derive(Debug, Clone, PartialEq, Eq)]
struct CloseAllTarget {
    name: String,
    pid: u32,
    /// Seconds since the daemon started (its `.pid` file was written).
    age_secs: Option<u64>,
}

/// The live sessions that are not the caller's own. `close --all` closes
/// every session in the user's Chrome — other agents' and other Claude
/// sessions' work included — so it refuses while any of these exist unless
/// `--force` is given. An empty result means `close --all` proceeds.
fn close_all_blockers(sessions: &[CloseAllTarget], own: &str, force: bool) -> Vec<CloseAllTarget> {
    if force {
        return Vec::new();
    }
    sessions.iter().filter(|s| s.name != own).cloned().collect()
}

/// `--force` (or `--yes` / `-y`, the confirmation `cookies clear --all` takes)
/// lets `close --all` close other agents' sessions too.
fn close_all_forced(args: &[String]) -> bool {
    args.iter()
        .any(|a| matches!(a.as_str(), "--force" | "--yes" | "-y"))
}

fn format_age(secs: u64) -> String {
    match secs {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m", s / 60),
        s if s < 86400 => format!("{}h{}m", s / 3600, (s % 3600) / 60),
        s => format!("{}d{}h", s / 86400, (s % 86400) / 3600),
    }
}

fn close_all_refusal_message(own: &str, others: &[CloseAllTarget]) -> String {
    let (count, verb) = if others.len() == 1 {
        ("1 other live session".to_string(), "belongs")
    } else {
        (format!("{} other live sessions", others.len()), "belong")
    };
    let mut msg = format!(
        "refusing `close --all`: {count} {verb} to other agents or other Claude sessions \
         and would be closed too:\n"
    );
    for s in others {
        let age = s
            .age_secs
            .map(|a| format!(", started {} ago", format_age(a)))
            .unwrap_or_default();
        msg.push_str(&format!("  - {} (pid {}{age})\n", s.name, s.pid));
    }
    msg.push_str(&format!(
        "To close only your own session ({own}), run `chrome-use close`.\n\
         Do not close the sessions above to get past this: they are other agents' work in \
         progress. Only the user can decide to close them all."
    ));
    msg
}

fn run_close_all(flags: &Flags, force: bool) {
    // walk_daemons auto-cleans stale .pid / .sock / .stream sidecar files and
    // separates out the standalone dashboard. We only want to send `close` to
    // real session daemons; the dashboard has its own `dashboard stop`.
    let inventory = walk_daemons();
    let socket_dir = get_socket_dir();
    let now = std::time::SystemTime::now();
    let targets: Vec<CloseAllTarget> = inventory
        .sessions
        .iter()
        .map(|s| CloseAllTarget {
            name: s.name.clone(),
            pid: s.pid,
            age_secs: fs::metadata(socket_dir.join(format!("{}.pid", s.name)))
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| now.duration_since(t).ok())
                .map(|d| d.as_secs()),
        })
        .collect();

    let blockers = close_all_blockers(&targets, &flags.session, force);
    if !blockers.is_empty() {
        let message = close_all_refusal_message(&flags.session, &blockers);
        if flags.json {
            print_json_value(json!({
                "success": false,
                "error": message,
                "type": "close_all_other_sessions",
                "code": "close_all_other_sessions",
                "retryable": false,
                "data": {
                    "ownSession": flags.session,
                    "otherSessions": blockers
                        .iter()
                        .map(|s| json!({ "name": s.name, "pid": s.pid, "ageSecs": s.age_secs }))
                        .collect::<Vec<_>>(),
                },
            }));
        } else {
            eprintln!("{} {}", color::error_indicator(), message);
        }
        exit(1);
    }

    let sessions: Vec<(String, u32)> = targets.into_iter().map(|s| (s.name, s.pid)).collect();

    if sessions.is_empty() {
        if flags.json {
            print_json_value(json!({
                "success": true,
                "data": { "closed": 0, "sessions": [] },
            }));
        } else {
            println!("No active sessions");
        }
        return;
    }

    let mut closed: Vec<String> = Vec::new();
    let mut failed: Vec<(String, String)> = Vec::new();

    for (session, pid) in &sessions {
        // `closedBy` lets every OTHER session's daemon record that its tabs
        // were closed from outside, so its own agent is told on its next
        // command instead of silently getting a fresh about:blank.
        let cmd = json!({ "id": gen_id(), "action": "close", "closedBy": flags.session });
        match send_command(cmd, session) {
            Ok(resp) if resp.success => closed.push(session.clone()),
            Ok(resp) => {
                let err = resp.error.unwrap_or_else(|| "Unknown error".to_string());
                failed.push((session.clone(), err));
            }
            Err(_) => {
                // Daemon is unreachable despite its process existing.
                // Force-kill the process and clean up stale files so future
                // sessions are not poisoned.
                #[cfg(unix)]
                unsafe {
                    libc::kill(*pid as i32, libc::SIGKILL);
                }
                #[cfg(windows)]
                unsafe {
                    let handle = OpenProcess(1, 0, *pid); // PROCESS_TERMINATE = 1
                    if handle != 0 {
                        windows_sys::Win32::System::Threading::TerminateProcess(handle, 1);
                        CloseHandle(handle);
                    }
                }
                cleanup_stale_files(session);
                closed.push(session.clone());
            }
        }
    }

    if flags.json {
        print_json_value(json!({
            "success": failed.is_empty(),
            "data": {
                "closed": closed.len(),
                "sessions": closed,
                "failed": failed.iter().map(|(s, e)| json!({"session": s, "error": e})).collect::<Vec<_>>(),
            },
        }));
    } else {
        for s in &closed {
            println!("{} Closed session: {}", color::green("✓"), s);
        }
        for (s, e) in &failed {
            eprintln!("{} Failed to close {}: {}", color::error_indicator(), s, e);
        }
        if closed.is_empty() && !failed.is_empty() {
            exit(1);
        }
    }

    if !failed.is_empty() {
        exit(1);
    }
}

fn main() {
    // Rust ignores SIGPIPE by default, causing println! to panic on broken pipes.
    // Reset to SIG_DFL so the OS terminates the process cleanly instead.
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }

    // Prevent MSYS/Git Bash path translation from mangling arguments
    #[cfg(windows)]
    {
        env::set_var("MSYS_NO_PATHCONV", "1");
        env::set_var("MSYS2_ARG_CONV_EXCL", "*");
    }

    // Native-messaging host mode: Chrome launches `chrome-use __nm-host
    // <extension-origin> [...]` for the ab-connect extension. Must run before
    // ANY stdout write — stdout is the Chrome native-messaging channel.
    // On Windows the manifest points straight at chrome-use.exe (no shell
    // launcher), so Chrome runs it with the extension origin as argv[1] and
    // no `__nm-host` marker — detect that origin too and enter the same mode.
    let nm_arg1 = env::args().nth(1);
    let native_host = nm_arg1.as_deref() == Some("__nm-host")
        || nm_arg1
            .as_deref()
            .is_some_and(|arg| arg.starts_with("chrome-extension://"));
    if let Err(error) = connect::validate_relay_configuration() {
        // Native-host stdout is a framed protocol, never ordinary JSON/text.
        if !native_host && env::args().any(|arg| arg == "--json") {
            print_json_error_with_type(&error, "invalid_configuration");
        } else {
            eprintln!("{error}");
        }
        exit(1);
    }
    if native_host {
        connect::run_nm_host();
        return;
    }

    // Hidden update-check worker, spawned detached by maybe_notify_update() to
    // refresh the cached latest version without blocking a real command.
    if env::args().nth(1).as_deref() == Some("__update-check") {
        upgrade::run_update_check();
        return;
    }

    // Hidden background auto-upgrade worker, spawned detached by
    // maybe_notify_update() when CHROME_USE_AUTO_UPGRADE=1 and a newer release
    // exists — applies the install.sh in place for the user's next run.
    if env::args().nth(1).as_deref() == Some("__auto-upgrade") {
        upgrade::run_auto_upgrade();
        return;
    }

    // Non-blocking "update available" hint (stderr only; self-skips meta
    // commands, daemon mode, CI, and the opt-out env vars).
    upgrade::maybe_notify_update();

    // Native daemon mode: when AGENT_BROWSER_DAEMON is set, run as the daemon process
    if env::var("AGENT_BROWSER_DAEMON").is_ok() {
        // Ignore SIGPIPE so the daemon isn't killed when the parent drops
        // the piped stderr handle after confirming the daemon is ready.
        #[cfg(unix)]
        unsafe {
            libc::signal(libc::SIGPIPE, libc::SIG_IGN);
        }
        let session = env::var("AGENT_BROWSER_SESSION").unwrap_or_else(|_| "default".to_string());
        // Commands run on worker threads, whose default 2 MiB stack a debug
        // build of the command dispatcher comes close to; give them a main
        // thread's worth.
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_stack_size(8 * 1024 * 1024)
            .build()
            .expect("Failed to create tokio runtime");
        rt.block_on(native::daemon::run_daemon(&session));
        return;
    }

    // Standalone dashboard server mode
    if env::var("AGENT_BROWSER_DASHBOARD").is_ok() {
        let port: u16 = env::var("AGENT_BROWSER_DASHBOARD_PORT")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(4848);
        let rt = tokio::runtime::Runtime::new().expect("Failed to create tokio runtime");
        rt.block_on(native::stream::run_dashboard_server(port));
        return;
    }

    let args: Vec<String> = env::args().skip(1).collect();
    let mut flags = parse_flags(&args);
    let mut clean = clean_args(&args);

    // Loudly warn when launching a fresh browser with no profile: it gets a
    // temporary EMPTY profile (no cookies / no login). For logged-in sites the
    // user almost always wants --profile auto (their real Chrome profile).
    // Skipped under CI (force_launch is implicit there and login isn't expected).
    // Only when this call is the one that launches: repeated on every command
    // of a running session it was the noise that taught agents `2>/dev/null`.
    if flags.force_launch
        && flags.profile.is_none()
        && env::var("CI").is_err()
        && !connection::daemon_ready(&flags.session)
    {
        eprintln!(
            "⚠ --launch opens a fresh, isolated test profile (no cookies, no login, no \
             extensions). The window is labelled `chrome-use (<session>)` in Chrome's \
             profile menu so you can tell it apart from your real browser.\n  \
             • reuse your real Chrome (cookies/login/extensions): `--profile auto` \
             (or set AGENT_BROWSER_PROFILE=auto once)\n  \
             • load an unpacked extension into the test profile: \
             `--args \"--load-extension=<dir>\"`"
        );
    }

    // `chrome-use help [command]` is what agents type for help.
    if clean.first().map(String::as_str) == Some("help") {
        clean.remove(0);
        if let Some(cmd) = clean.first() {
            if !print_command_help(cmd) {
                output::print_help_excerpt(cmd);
            }
        } else {
            print_help();
        }
        return;
    }
    let has_help = args.iter().any(|a| a == "--help" || a == "-h");
    let has_version = args.iter().any(|a| a == "--version" || a == "-V");

    if has_help {
        if let Some(cmd) = clean.first() {
            if print_command_help(cmd) {
                return;
            }
            if commands::is_known_command(cmd) {
                output::print_help_excerpt(cmd);
                return;
            }
        }
        print_help();
        return;
    }

    if has_version {
        print_version();
        return;
    }

    if clean.is_empty() {
        print_help();
        return;
    }

    // A session started with `--launch` stays one until it is closed, even when
    // its daemon (and browser) went away between commands.
    match flags.launched_session(connection::session_was_launched(&flags.session)) {
        flags::LaunchedSession::Continue => flags.continue_launched_session(),
        flags::LaunchedSession::Switch => connection::clear_session_launched(&flags.session),
        flags::LaunchedSession::Unchanged => {}
    }

    // Handle install separately
    if clean.first().map(|s| s.as_str()) == Some("install") {
        let with_deps = args.iter().any(|a| a == "--with-deps" || a == "-d");
        run_install(with_deps);
        return;
    }

    // Handle upgrade separately
    if clean.first().map(|s| s.as_str()) == Some("upgrade") {
        run_upgrade(&args);
        return;
    }

    // Handle doctor separately (doesn't need daemon; spawns its own scratch
    // session for the live launch test).
    if clean.first().map(|s| s.as_str()) == Some("doctor") {
        let opts = doctor::DoctorOptions {
            offline: args.iter().any(|a| a == "--offline"),
            quick: args.iter().any(|a| a == "--quick"),
            fix: args.iter().any(|a| a == "--fix"),
            json: flags.json,
        };
        exit(doctor::run_doctor(opts));
    }

    // Handle MCP stdio server mode. This must never share stdout with normal
    // CLI output — stdout is reserved for newline-delimited JSON-RPC messages.
    if clean.first().map(|s| s.as_str()) == Some("mcp") {
        if let Err(err) = mcp::run_mcp(&clean[1..]) {
            eprintln!("{} {}", color::error_indicator(), err);
            exit(1);
        }
        return;
    }

    // Handle dashboard subcommand
    if clean.first().map(|s| s.as_str()) == Some("dashboard") {
        match clean.get(1).map(|s| s.as_str()) {
            Some("start") | None => {
                let port = clean
                    .iter()
                    .position(|a| a == "--port")
                    .and_then(|i| clean.get(i + 1))
                    .and_then(|s| s.parse::<u16>().ok())
                    .unwrap_or(4848);
                run_dashboard_start(port, flags.json);
                return;
            }
            Some("stop") => {
                run_dashboard_stop(flags.json);
                return;
            }
            Some(unknown) => {
                eprintln!(
                    "{} Unknown dashboard subcommand: {}",
                    color::error_indicator(),
                    unknown
                );
                exit(1);
            }
        }
    }

    // `auth autologin …` (#481): the stored login-wall decisions; no daemon.
    if clean.first().map(|s| s.as_str()) == Some("auth")
        && clean.get(1).map(|s| s.as_str()) == Some("autologin")
    {
        exit(autologin::run_cli(&clean[2..], flags.json));
    }

    // Handle profiles command (doesn't need daemon)
    if clean.first().map(|s| s.as_str()) == Some("profiles") {
        run_profiles(flags.json);
        return;
    }

    // Handle `cookies export` (doesn't need daemon): decrypt an on-disk Chrome
    // profile's cookies and print them as JSON for `cookies set --curl`.
    if clean.first().map(|s| s.as_str()) == Some("cookies")
        && clean.get(1).map(|s| s.as_str()) == Some("export")
    {
        run_cookies_export(&clean, &flags);
        return;
    }

    // Handle `test <suite.yaml>`: run a browser test suite. It orchestrates by
    // re-invoking this binary per step, so it lives outside the normal dispatch.
    if clean.first().map(|s| s.as_str()) == Some("test") {
        let Some(suite) = clean.get(1) else {
            eprintln!(
                "{} usage: chrome-use test <suite.yaml> [--launch | --session <name>]",
                color::error_indicator()
            );
            exit(2);
        };
        exit(test_runner::run_test(suite, &flags));
    }

    // Handle `site`: site adapters — turn a website into a structured-data CLI by
    // running a per-command JS adapter inside your logged-in tab. `update`/`list`/
    // `info` are CLI-side (download/filesystem); `site <name>/<cmd> [args]` falls
    // through to the daemon dispatch below (navigate to the adapter's domain + eval).
    if clean.first().map(|s| s.as_str()) == Some("site") {
        // Auto-sync the adapter packs on first use and periodically (TTL, default
        // 7d) so adapters stay fresh without a manual `site update`. Skipped for an
        // explicit `update` (full sync below). Best-effort: offline → cached pack.
        // Disable with AGENT_BROWSER_SITES_NO_AUTO_UPDATE=1.
        // Pure config/subcommands (`update` does its own sync; `sources`/`add`/
        // `remove` just edit the source list) skip the implicit auto-sync.
        let sub = clean.get(1).map(|s| s.as_str());
        if !matches!(
            sub,
            Some("update") | Some("sources") | Some("add") | Some("remove")
        ) && site::needs_refresh()
        {
            let rt = tokio::runtime::Runtime::new().expect("Failed to create tokio runtime");
            match rt.block_on(site::update()) {
                Ok(n) => {
                    eprintln!(
                        "{}",
                        color::dim(&format!("site: synced {n} adapters (auto)"))
                    )
                }
                Err(e) => eprintln!(
                    "{}",
                    color::dim(&format!(
                        "site: auto-sync skipped ({e}); using cached adapters"
                    ))
                ),
            }
        }
        // A `name/cmd` we have no adapter for but OpenCLI does: run it with
        // OpenCLI's runtime over this session (opencli.rs). `site verify` too.
        {
            let verify = clean.get(1).map(|s| s.as_str()) == Some("verify");
            let at = if verify { 2 } else { 1 };
            if let Some(spec) = clean.get(at).filter(|s| opencli::handles(s)) {
                let entry = opencli::lookup(spec).unwrap_or_default();
                let write_fixture = verify && clean.iter().any(|a| a == "--write-fixture");
                let rest: Vec<String> = clean[at + 1..]
                    .iter()
                    .filter(|a| !(verify && a.as_str() == "--write-fixture"))
                    .cloned()
                    .collect();
                let mut env = opencli::run(spec, &entry, &rest, &flags.session);
                let mut ok = env.get("success").and_then(|v| v.as_bool()) == Some(true);
                let result = env.get("data").cloned().unwrap_or(Value::Null);
                if ok && verify {
                    let (vok, report) = site::verify_result(spec, &result, write_fixture);
                    if !vok {
                        ok = false;
                        let issues: Vec<String> = report
                            .get("issues")
                            .and_then(|x| x.as_array())
                            .map(|a| {
                                a.iter()
                                    .filter_map(|i| i.as_str().map(String::from))
                                    .collect()
                            })
                            .unwrap_or_default();
                        env["error"] = json!(format!("site verify {spec}: {}", issues.join("; ")));
                    }
                    env["verify"] = report;
                }
                if flags.json {
                    let mut data = json!({ "result": result, "source": opencli::SOURCE_LABEL });
                    if let Some(v) = env.get("verify") {
                        data["verify"] = v.clone();
                    }
                    println!(
                        "{}",
                        json!({ "success": ok, "data": data, "error": if ok { Value::Null } else { env.get("error").cloned().unwrap_or(Value::Null) } })
                    );
                } else if ok {
                    eprintln!("{}", color::dim(&format!("site {spec} (via OpenCLI)")));
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&result).unwrap_or_default()
                    );
                    if let Some(v) = env.get("verify") {
                        if v.get("recorded").and_then(|x| x.as_bool()) == Some(true) {
                            eprintln!(
                                "{} site verify: fixture recorded",
                                color::success_indicator()
                            );
                        } else {
                            eprintln!("{} site verify: ok", color::success_indicator());
                        }
                    }
                } else {
                    let err = env
                        .get("error")
                        .and_then(|v| v.as_str())
                        .unwrap_or("failed");
                    match env
                        .get("hint")
                        .and_then(|v| v.as_str())
                        .filter(|h| !h.is_empty())
                    {
                        Some(h) => eprintln!("{} {err} — {h}", color::error_indicator()),
                        None => eprintln!("{} {err}", color::error_indicator()),
                    }
                }
                exit(if ok { 0 } else { 1 });
            }
        }
        match clean.get(1).map(|s| s.as_str()) {
            Some("update") => {
                let rt = tokio::runtime::Runtime::new().expect("Failed to create tokio runtime");
                match rt.block_on(site::update()) {
                    Ok(n) if flags.json => {
                        println!("{}", json!({ "success": true, "adapters": n }))
                    }
                    Ok(n) => println!(
                        "{} synced {} site adapters → ~/.chrome-use/sites (run `chrome-use site list`)",
                        color::success_indicator(),
                        n
                    ),
                    Err(e) => {
                        eprintln!("{} {}", color::error_indicator(), e);
                        exit(1);
                    }
                }
                return;
            }
            Some("list") => {
                let theirs: Vec<String> = {
                    let ours = site::list_adapters().unwrap_or_default();
                    let mut v: Vec<String> = opencli::manifest()
                        .iter()
                        .filter_map(opencli::spec_of)
                        .filter(|s| !ours.contains(s))
                        .collect();
                    v.sort();
                    v.dedup();
                    v
                };
                match site::list_adapters() {
                    Ok(list) if flags.json => {
                        println!(
                            "{}",
                            json!({ "success": true, "adapters": list, "opencli": theirs })
                        )
                    }
                    Ok(list) if list.is_empty() => {
                        println!("no site adapters installed — run `chrome-use site update`")
                    }
                    Ok(list) => {
                        for a in &list {
                            println!("{a}");
                        }
                        for a in &theirs {
                            println!("{a} {}", color::dim("(opencli)"));
                        }
                        eprintln!(
                            "{}",
                            color::dim(&format!(
                                "{} adapters · run: chrome-use site <name>/<cmd> [args]",
                                list.len()
                            ))
                        );
                    }
                    Err(e) => {
                        eprintln!("{} {}", color::error_indicator(), e);
                        exit(1);
                    }
                }
                return;
            }
            Some("info") => {
                let spec = clean.get(2).cloned().unwrap_or_default();
                if opencli::handles(&spec) {
                    if let Some(entry) = opencli::lookup(&spec) {
                        println!(
                            "{}",
                            serde_json::to_string_pretty(&opencli::info(&entry))
                                .unwrap_or_default()
                        );
                        return;
                    }
                }
                match site::load_adapter(&spec) {
                    Ok(a) => println!(
                        "{}",
                        serde_json::to_string_pretty(&a.meta).unwrap_or_default()
                    ),
                    Err(e) => {
                        eprintln!("{} {}", color::error_indicator(), e);
                        exit(1);
                    }
                }
                return;
            }
            // `site sources` — list the built-in defaults and configured extras.
            Some("sources") => {
                let sources = site::read_sources();
                if flags.json {
                    println!(
                        "{}",
                        json!({
                            "success": true,
                            "defaultSources": site::default_sources(),
                            "sources": sources
                        })
                    );
                } else {
                    println!("{} (default: community)", site::COMMUNITY_SITES_SOURCE);
                    println!("{} (default: official)", site::OFFICIAL_SITES_SOURCE);
                    for source in &sources {
                        println!("{source} (extra)");
                    }
                    eprintln!(
                        "{}",
                        color::dim(&format!(
                            "2 default source(s) + {} extra source(s) · synced on `site update`",
                            sources.len()
                        ))
                    );
                }
                return;
            }
            // `site add <owner/repo|zip-url|local-dir>` — register an extra source.
            Some("add") => {
                let src = clean.get(2).cloned().unwrap_or_default();
                if src.is_empty() {
                    eprintln!(
                        "{} usage: chrome-use site add <owner/repo | https://…zip | /local/dir>",
                        color::error_indicator()
                    );
                    exit(2);
                }
                match site::add_source(&src) {
                    Ok(true) => {
                        if flags.json {
                            println!("{}", json!({ "success": true, "added": src }));
                        } else {
                            println!(
                                "{} added source `{}` — run `chrome-use site update` to sync it",
                                color::success_indicator(),
                                src
                            );
                        }
                    }
                    Ok(false) => {
                        let note = if site::is_default_source(&src) {
                            "built-in default"
                        } else {
                            "already present"
                        };
                        if flags.json {
                            println!(
                                "{}",
                                json!({ "success": true, "added": serde_json::Value::Null, "note": note })
                            );
                        } else {
                            println!("source `{src}` is already configured ({note})");
                        }
                    }
                    Err(e) => {
                        eprintln!("{} {}", color::error_indicator(), e);
                        exit(1);
                    }
                }
                return;
            }
            // `site remove <source>` — unregister an extra source (installed
            // adapter files are left in place; a future `update` won't re-fetch).
            Some("remove") => {
                let src = clean.get(2).cloned().unwrap_or_default();
                if src.is_empty() {
                    eprintln!(
                        "{} usage: chrome-use site remove <source>",
                        color::error_indicator()
                    );
                    exit(2);
                }
                match site::remove_source(&src) {
                    Ok(true) => {
                        if flags.json {
                            println!("{}", json!({ "success": true, "removed": src }));
                        } else {
                            println!("{} removed source `{}`", color::success_indicator(), src);
                        }
                    }
                    Ok(false) => {
                        if flags.json {
                            print_json_error(format!("source `{src}` not found"));
                        } else {
                            eprintln!(
                                "{} source `{}` was not in the list (run `chrome-use site sources`)",
                                color::warning_indicator(),
                                src
                            );
                        }
                        exit(1);
                    }
                    Err(e) => {
                        eprintln!("{} {}", color::error_indicator(), e);
                        exit(1);
                    }
                }
                return;
            }
            // `site analyze [url]` / `site verify <name>/<cmd> …` → daemon dispatch
            // (commands.rs builds them).
            Some("analyze") | Some("verify") => {}
            // `site <name>/<cmd> [args]` → fall through to the daemon dispatch.
            Some(spec) if spec.contains('/') => {
                // #125: an adapter arg whose name collides with a reserved global
                // flag (e.g. `--state`) is swallowed by clean_args BEFORE the
                // adapter sees it — the override silently vanishes and the adapter
                // uses its default. Warn loudly and point at the escape hatches:
                // pass it positionally, or after the `--` end-of-options marker.
                // Only warn for a collision that appears BEFORE a `--` (after `--`
                // the value is forwarded verbatim and reaches the adapter fine).
                if let Ok(adapter) = site::load_adapter(spec) {
                    let passthrough_at = args.iter().position(|a| a == "--");
                    for name in &adapter.arg_order {
                        if !flags::is_reserved_global_flag(name) {
                            continue;
                        }
                        let dashed = format!("--{name}");
                        let consumed_globally = args
                            .iter()
                            .enumerate()
                            .any(|(i, a)| a == &dashed && passthrough_at.is_none_or(|p| i < p));
                        if consumed_globally {
                            eprintln!(
                                "{} adapter arg \"{name}\" collides with the global --{name} flag, \
                                 so it was consumed as a global flag and never reached the adapter. \
                                 Pass it positionally, or after `--`: \
                                 `chrome-use site {spec} -- --{name} <value>`",
                                color::warning_indicator()
                            );
                        }
                    }
                }
            }
            _ => {
                eprintln!(
                    "{} usage: chrome-use site <name>/<cmd> [args] | site analyze [url] | site verify <name>/<cmd> [args] [--write-fixture] | site update | site list | \
                     site info <name>/<cmd> | site sources | site add|remove <source>",
                    color::error_indicator()
                );
                exit(2);
            }
        }
    }

    // Handle skills command (doesn't need daemon). `skill` is an alias.
    if matches!(
        clean.first().map(|s| s.as_str()),
        Some("skills") | Some("skill")
    ) {
        skills::run_skills(&clean, flags.json);
        return;
    }

    // Handle find-url (doesn't need daemon): search local bookmarks
    if matches!(
        clean.first().map(|s| s.as_str()),
        Some("find-url") | Some("findurl")
    ) {
        findurl::run_find_url(&clean, flags.json);
        return;
    }

    // `friction` (no daemon): aggregate the local friction log — what's been
    // painful to drive. Data for the next round of features.
    if clean.first().map(|s| s.as_str()) == Some("friction") {
        friction::run_friction(&clean[1..], flags.json);
        return;
    }

    // `report` (no daemon): draft a redacted GitHub issue from the friction
    // log; `--submit` files it only with the user's OK (report.rs).
    if clean.first().map(|s| s.as_str()) == Some("report") {
        report::run_report(&clean[1..], &args, &flags.session, flags.json);
        return;
    }

    // `browsers` (no daemon): list the connected Chrome profiles so an agent can
    // pin a session to one with `--browser <id|email>` (issue #60).
    if clean.first().map(|s| s.as_str()) == Some("browsers") {
        profiles::run_browsers(&clean[1..], &flags.session, flags.json);
        return;
    }

    // `connect --browser <selector>` (no port/url): connect one more Chrome
    // profile lazily — open the Web Store page (or just a window, if the
    // extension is already there) in that profile and wait for its relay.
    if clean.first().map(|s| s.as_str()) == Some("connect")
        && clean.get(1).is_none_or(|a| a.starts_with("--"))
    {
        if let Some(sel) = flags.browser.as_deref() {
            profiles::run_connect_profile(sel, &clean[1..], flags.json);
            return;
        }
    }

    // Session management is local and daemon-free. Route stop/prune to daemon
    // lifecycle handling; keep ownership commands on the `.owner` sidecar path.
    if clean.first().map(|s| s.as_str()) == Some("session") {
        let sub = clean.get(1).map(|s| s.as_str());
        match session_command_route(sub) {
            SessionCommandRoute::Lifecycle => {
                run_session_lifecycle(&clean, &flags.session, flags.json)
            }
            SessionCommandRoute::Ownership => {
                run_session_ownership(sub, &flags.session, flags.json)
            }
        }
        return;
    }

    // `reconnect` is a friendly alias for `extension connect` (issue #58): re-bind
    // the session to the running Chrome's relay without any reinstall. Rewrite it
    // into `extension connect …` (preserving any flags like --silent) and let the
    // block below handle it.
    if clean.first().map(|s| s.as_str()) == Some("reconnect") {
        let mut rebuilt = vec!["extension".to_string(), "connect".to_string()];
        rebuilt.extend(clean.into_iter().skip(1));
        clean = rebuilt;
    }

    // Handle extension: native-messaging host install/status, and
    // `extension connect` which attaches to the live relay (auto-discovers the
    // CDP url the host wrote) by rewriting into the normal `connect <url>` flow.
    // (`connect <port>` stays the plain CDP-attach command.)
    if clean.first().map(|s| s.as_str()) == Some("extension") {
        if clean.get(1).map(|s| s.as_str()) == Some("connect") {
            // Optionally silence Chrome's `chrome.debugger` "started debugging
            // this browser" banner by cold-relaunching the user's Chrome with
            // --silent-debugger-extension-api. Default (Auto) only restarts after
            // an interactive confirm; `--silent` forces it, `--keep-banner` skips.
            let silence_mode = if clean.iter().any(|a| a == "--keep-banner") {
                silence::SilenceMode::Off
            } else if clean.iter().any(|a| a == "--silent") {
                silence::SilenceMode::Force
            } else {
                silence::SilenceMode::Auto
            };
            if silence_mode != silence::SilenceMode::Off {
                match silence::ensure_banner_silenced(silence_mode) {
                    silence::SilenceOutcome::Restarted => {
                        // Chrome dropped the relay on quit; wait for ab-connect
                        // to respawn the native host and rewrite its CDP url.
                        eprint!(
                            "{} Chrome restarted; waiting for the extension relay to reconnect…",
                            color::success_indicator()
                        );
                        let _ = std::io::Write::flush(&mut std::io::stderr());
                        let deadline =
                            std::time::Instant::now() + std::time::Duration::from_secs(25);
                        while connect::relay_url().is_none() && std::time::Instant::now() < deadline
                        {
                            std::thread::sleep(std::time::Duration::from_millis(500));
                        }
                        eprintln!();
                    }
                    silence::SilenceOutcome::Failed(e) => {
                        eprintln!(
                            "{} could not silence the debugging banner: {e}",
                            color::warning_indicator()
                        );
                    }
                    silence::SilenceOutcome::Ambiguous(n) => {
                        eprintln!(
                            "{} {n} Chrome instances are running — not auto-restarting (quitting \
                             would close all of them). Quit the extra Chrome instances and retry, \
                             or launch Chrome with --silent-debugger-extension-api yourself.",
                            color::warning_indicator()
                        );
                    }
                    // AlreadySilent / NotRunning / Declined → proceed as before.
                    _ => {}
                }
            }
            // The relay may not be up the instant we ask: after a fresh host
            // install or an MV3 service-worker sleep, the extension reconnects on
            // its keepalive (~30s, allow up to ~45s) and only THEN writes its CDP
            // url. Failing instantly here is exactly what misled users into
            // quitting/restarting Chrome — verified locally that a running Chrome
            // picks the host back up on its own, no restart needed. So if the host
            // is registered, register-to-be-safe and poll for the relay to come up.
            let target_browser = flags.browser.as_deref().or(flags.profile.as_deref());
            let current_relay = match connect::relay_url_for_selector_or_default(target_browser) {
                Ok(url) => url,
                Err(e) => {
                    eprintln!("{} {e}", color::error_indicator());
                    exit(1);
                }
            };
            if current_relay.is_none() && crate::connect::host_installed() {
                crate::connect::ensure_host_installed();
                eprint!(
                    "{} extension relay reconnecting (the worker wakes ~every 30s)…",
                    color::success_indicator()
                );
                let _ = std::io::Write::flush(&mut std::io::stderr());
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(45);
                while connect::relay_url_for_selector_or_default(target_browser)
                    .ok()
                    .flatten()
                    .is_none()
                    && std::time::Instant::now() < deadline
                {
                    std::thread::sleep(std::time::Duration::from_millis(750));
                }
                eprintln!();
            }
            match connect::relay_url_for_selector_or_default(target_browser) {
                Ok(Some(url)) => {
                    // The connect path reads `flags.cdp` (parsed from the original
                    // argv, which was `extension connect` → None), NOT `clean`.
                    // Without this the relay URL is dropped and we fall through to
                    // auto-connect, grabbing some other Chrome (stale :9222) or
                    // popping the remote-debug prompt. Point the daemon at the
                    // relay explicitly.
                    flags.cdp = Some(url.clone());
                    flags.auto_connect = false;
                    clean = vec!["connect".to_string(), url];
                }
                Err(e) => {
                    eprintln!("{} {e}", color::error_indicator());
                    exit(1);
                }
                Ok(None) if !crate::connect::host_installed() => {
                    // Host not set up → register it + open the Store page (one
                    // click). Never the dev-mode "Load unpacked" lecture.
                    crate::connect::ensure_host_installed();
                    crate::connect::open_url(crate::connect::STORE_URL);
                    eprintln!("{}", crate::connect::extension_not_installed_message());
                    exit(1);
                }
                Ok(None) => {
                    // Extension IS set up — the worker just hasn't reconnected yet.
                    // Accurate guidance: retry, or reload ONLY the extension. NEVER
                    // "restart Chrome" (a running Chrome picks the host up on its
                    // own — verified) and never dev-mode "Load unpacked".
                    eprintln!(
                        "{} The chrome-use extension is installed, but its background worker \
                         hasn't reconnected to the native host yet (MV3 workers sleep and wake \
                         on a ~30s timer). This usually clears on its own within ~30–60s — just \
                         re-run this command. To force it immediately, reload ONLY the chrome-use \
                         extension at chrome://extensions (the ↻ reload icon). A full Chrome \
                         restart is NOT required.",
                        color::error_indicator()
                    );
                    exit(1);
                }
            }
        } else if matches!(
            clean.get(1).map(|s| s.as_str()),
            Some("call") | Some("state")
        ) {
            // `extension call <ns.method> [json-args]` / `extension state` need a
            // live relay session, so they run as ordinary daemon commands.
            // Rewritten here (the `extension` word is otherwise local-only) and
            // allowed to fall through to the normal command path below.
            let sub = clean[1].clone();
            let mut rebuilt = vec![format!("extension_{sub}")];
            rebuilt.extend(clean.into_iter().skip(2));
            clean = rebuilt;
        } else {
            connect::run_connect(&clean, flags.json);
            return;
        }
    }

    // `adopt <url|targetId>`: read a PRE-EXISTING tab (the user's own, or another
    // session's) WITHOUT opening a new one. Forces a fresh daemon and points it at
    // the relay (like `extension connect`); the AGENT_BROWSER_ADOPT env makes the
    // daemon's first connect ADOPT the matching tab instead of creating an
    // about:blank. Rewrites into `connect <relay-url>` BEFORE parse_command so the
    // daemon attaches to the user's real Chrome. Must run before parse_command.
    if clean.first().map(|s| s.as_str()) == Some("adopt") {
        match clean.get(1) {
            Some(spec) if !spec.trim().is_empty() => {
                std::env::set_var("AGENT_BROWSER_ADOPT", spec.trim());
                connection::kill_stale_daemon(&flags.session);
                // Same as `extension connect`: the worker may be mid-reconnect, so
                // wait for the relay to come up instead of failing instantly (which
                // misled users into restarting Chrome).
                let target_browser = flags.browser.as_deref().or(flags.profile.as_deref());
                let current_relay = match connect::relay_url_for_selector_or_default(target_browser)
                {
                    Ok(url) => url,
                    Err(e) => {
                        eprintln!("{} {e}", color::error_indicator());
                        exit(1);
                    }
                };
                if current_relay.is_none() && crate::connect::host_installed() {
                    crate::connect::ensure_host_installed();
                    eprint!(
                        "{} extension relay reconnecting (the worker wakes ~every 30s)…",
                        color::success_indicator()
                    );
                    let _ = std::io::Write::flush(&mut std::io::stderr());
                    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(45);
                    while connect::relay_url_for_selector_or_default(target_browser)
                        .ok()
                        .flatten()
                        .is_none()
                        && std::time::Instant::now() < deadline
                    {
                        std::thread::sleep(std::time::Duration::from_millis(750));
                    }
                    eprintln!();
                }
                match connect::relay_url_for_selector_or_default(target_browser) {
                    Ok(Some(url)) => {
                        flags.cdp = Some(url.clone());
                        flags.auto_connect = false;
                        clean = vec!["connect".to_string(), url];
                    }
                    Err(e) => {
                        eprintln!("{} {e}", color::error_indicator());
                        exit(1);
                    }
                    Ok(None) if !crate::connect::host_installed() => {
                        crate::connect::ensure_host_installed();
                        crate::connect::open_url(crate::connect::STORE_URL);
                        eprintln!("{}", crate::connect::extension_not_installed_message());
                        exit(1);
                    }
                    Ok(None) => {
                        eprintln!(
                            "{} The chrome-use extension is installed but its background worker \
                             hasn't reconnected to the native host yet (MV3 workers sleep, ~30s \
                             wake timer). Re-run this in a moment, or reload ONLY the chrome-use \
                             extension at chrome://extensions (↻). A Chrome restart is NOT needed.",
                            color::error_indicator()
                        );
                        exit(1);
                    }
                }
            }
            _ => {
                eprintln!(
                    "{} usage: chrome-use adopt <url-substring|targetId>  (reads an existing tab, no new tab)",
                    color::error_indicator()
                );
                exit(2);
            }
        }
    }

    // `--browser <id|email-substr>` (issue #60): pin this session to a specific
    // connected Chrome profile by resolving to that profile's stable relay
    // endpoint and connecting the daemon to it. Session-sticky: the per-session
    // daemon binds to this endpoint on its first connect and keeps it for life
    // (a different session can pick a different profile — no global state, so
    // concurrent agents don't fight). To switch a *running* session's profile,
    // start a fresh `--session` (or close it first).
    let mut browser_email: Option<String> = None;
    // Which relay endpoint this invocation picked for the session, and why —
    // for the one-line "profile: …" note (#437).
    let mut profile_choice: Option<(String, String)> = None;
    // Did the user name the browser endpoint themselves? Captured before the
    // profile choice below fills `flags.cdp` in on their behalf.
    let user_chose_cdp = flags.cdp.is_some();
    // `batch` steps navigate as much as `open` does. Their stdin is read here,
    // once, so the rule check sees every step before anything runs.
    let batch_steps: Option<Vec<Vec<String>>> =
        if clean.first().map(String::as_str) == Some("batch") {
            Some(load_batch_steps(&clean, &flags))
        } else {
            None
        };
    let nav_urls = navigation_urls(&clean, &flags, batch_steps.as_deref());
    let first_attach = !connection::daemon_ready(&flags.session);
    // `--profile` / AGENT_BROWSER_PROFILE doubles as a profile selector when it
    // names a Chrome profile (display name, directory, email, id). A path or
    // an unknown name keeps its launch-mode meaning.
    let profile_as_selector = flags.browser.is_none()
        && flags.cdp.is_none()
        && !flags.force_launch
        && flags
            .profile
            .as_deref()
            .is_some_and(profiles::names_a_profile);
    let browser_selector = if flags.browser.is_some() {
        flags.browser.clone()
    } else if profile_as_selector {
        flags.profile.clone()
    } else {
        None
    };
    // ChooseBrowser rules bind only when nothing explicit chose the browser.
    // The daemon re-checks every navigation it is sent (batch steps, MCP
    // calls, scripts), so it is told the same thing.
    let rules_skipped = flags.no_choosebrowser
        || browser_selector.is_some()
        || user_chose_cdp
        || flags.provider.is_some()
        || flags.force_launch;
    connection::set_choosebrowser_skip(rules_skipped);
    let configured = profiles::load_profiles_config();
    let rule_hits: Vec<profiles::RuleHit> = if rules_skipped {
        Vec::new()
    } else {
        nav_urls
            .iter()
            // A config route for the url wins over a ChooseBrowser rule.
            .filter(|u| {
                configured
                    .as_ref()
                    .and_then(|cfg| profiles::choose_route(cfg, u))
                    .is_none()
            })
            .filter_map(|u| match profiles::rule_outcome_for_url(u) {
                profiles::RuleOutcome::Hit(h) => Some(h),
                // Stale rules only warn; the daemon attaches that warning to
                // the navigation it belongs to.
                profiles::RuleOutcome::Stale(_) | profiles::RuleOutcome::None => None,
            })
            .collect()
    };
    if let Some(msg) = profiles::conflicting_rules(&rule_hits) {
        eprintln!("{} {msg}", color::error_indicator());
        exit(1);
    }

    // #472: a session pinned to a relay profile stays on it, also across a
    // daemon that died or was stopped by a relay recovery. With nothing
    // explicit on this command, the pin is the choice: no config route,
    // ChooseBrowser rule, default or focus guess may pick another profile.
    // (An explicit --browser / --cdp is checked against the pin when the
    // daemon is ensured.) An unreadable pin is an unknown binding: refuse.
    let pinned_profile: Option<String> =
        if browser_selector.is_none() && !user_chose_cdp && !flags.force_launch {
            match connection::session_relay_profile(&flags.session) {
                Ok(pin) => pin,
                Err(e) => fail_command(&flags, &e),
            }
        } else {
            None
        };
    if let Some(id) = pinned_profile.as_deref() {
        if first_attach && flags.auto_connect && flags.cdp.is_none() {
            match connect::relay_endpoint_for_profile(id) {
                Ok(ws) => {
                    profile_choice = Some((ws.clone(), "this session is bound to it".to_string()));
                    flags.cdp = Some(ws);
                    flags.auto_connect = false;
                }
                // Not connected right now: the relay recovery below waits for
                // it (or the daemon does), never for anything else.
                Err(connect::ProfileEndpointError::NotConnected(_)) => {}
                Err(e) => {
                    fail_command(&flags, &pinned_profile_unresolvable(&flags.session, id, &e))
                }
            }
        }
    }

    if let Some(sel) = browser_selector.as_ref() {
        match connect::relay_profile_for_browser(sel) {
            Ok((_, email, url)) => {
                browser_email = email;
                let why = if flags.browser.is_some() {
                    "--browser"
                } else {
                    "--profile / AGENT_BROWSER_PROFILE"
                };
                profile_choice = Some((url.clone(), why.to_string()));
                flags.cdp = Some(url);
                flags.auto_connect = false;
            }
            Err(msg) => {
                eprintln!("{} {msg}", color::error_indicator());
                exit(1);
            }
        }
    } else if flags.auto_connect
        && flags.cdp.is_none()
        && !flags.force_launch
        // Only choose a profile when establishing this session's daemon for the
        // first time. Once it's running it stays bound to its profile — otherwise
        // re-resolving on every invocation would silently hop the session to a
        // different profile as the user changes window focus mid-task.
        && first_attach
        // A pinned session never re-chooses, even when its daemon is gone.
        && pinned_profile.is_none()
    {
        // No explicit `--browser`. With several Chrome profiles each running the
        // extension, the relay's generic endpoint is whichever host connected
        // last — often NOT the profile the user is logged into for the task, so
        // the agent lands on a logged-out profile. In order:
        //   1. a route in ~/.chrome-use/config.json `profiles.routes` (#437),
        //   2. a ChooseBrowser rule for the site (#244),
        //   3. `profiles.default` from the same config (#437),
        //   4. the profile the user is actively using (most recently focused),
        //   5. the legacy last-connected default, with a warning.
        let target_url = nav_urls.first().cloned();
        if let Some((sel, why)) = configured
            .as_ref()
            .zip(target_url.as_deref())
            .and_then(|(cfg, url)| profiles::choose_route(cfg, url))
        {
            let ws = configured_profile_ws(&sel, &why);
            profile_choice = Some((ws.clone(), why));
            flags.cdp = Some(ws);
            flags.auto_connect = false;
        }

        // Before guessing from focus, though: the user may already have written
        // down which account this site belongs to. ChooseBrowser stores exactly
        // that mapping, and a rule they authored beats any inference we make
        // from which window they happen to be looking at (issue #244).
        //
        // The rule is binding, not advice: when the profile it names has no
        // relay endpoint, stop with the fix instead of falling through to the
        // default or the focused profile — that fall-through opened claude.ai
        // in the wrong account with no message. A config route chosen above
        // (flags.cdp set) and --no-choosebrowser still win.
        let rule_hit = if flags.cdp.is_some() {
            None
        } else {
            rule_hits.first().cloned()
        };
        if rule_hit.is_some() {
            let rows = profiles::load_rows();
            match profiles::decide_rule(
                &rows,
                rule_hit.as_ref(),
                flags.no_choosebrowser,
                flags.cdp.is_some(),
                profiles::SessionBinding::New,
                &flags.session,
            ) {
                profiles::RuleDecision::Use(i) => {
                    let url = rows[i].ws.clone().unwrap_or_default();
                    // Say where the choice came from (in the profile line).
                    // Without this the user sees a different account open than
                    // the window they were looking at, with nothing to explain it.
                    profile_choice = Some((
                        url.clone(),
                        format!(
                            "a ChooseBrowser rule routes this site there{} (skip with --no-choosebrowser)",
                            rule_hit
                                .as_ref()
                                .and_then(|h| h.rule_id.as_deref())
                                .map(|r| format!(" ({r})"))
                                .unwrap_or_default(),
                        ),
                    ));
                    flags.cdp = Some(url);
                    flags.auto_connect = false;
                }
                profiles::RuleDecision::Refuse(msg) => {
                    eprintln!("{} {msg}", color::error_indicator());
                    exit(1);
                }
                profiles::RuleDecision::NotApplicable | profiles::RuleDecision::AlreadyThere => {}
            }
        }

        if flags.cdp.is_none() {
            if let Some((sel, why)) = configured.as_ref().and_then(profiles::configured_default) {
                let ws = configured_profile_ws(&sel, &why);
                profile_choice = Some((ws.clone(), why));
                flags.cdp = Some(ws);
                flags.auto_connect = false;
            }
        }

        let relay_profiles = connect::list_relay_profiles();
        if flags.cdp.is_none() && relay_profiles.len() >= 2 {
            match connect::most_recently_focused_profile() {
                Some((_, _, ws)) => {
                    profile_choice = Some((
                        ws.clone(),
                        format!(
                            "most recently used of {} connected profiles; pick with --browser",
                            relay_profiles.len()
                        ),
                    ));
                    flags.cdp = Some(ws);
                    flags.auto_connect = false;
                }
                None => {
                    eprintln!(
                        "{} {} Chrome profiles connected and none is clearly in focus — driving the last-connected one, which may be logged out. Pick one with --browser <name|email> (see `chrome-use browsers`):",
                        color::warning_indicator(),
                        relay_profiles.len(),
                    );
                    for (id, email, _) in &relay_profiles {
                        eprintln!("    {}", connect::profile_label(id, email.as_deref()));
                    }
                }
            }
        }
    }

    // A running session keeps its profile, so a rule naming another profile
    // is enforced by the daemon right before it sends the navigation — the one
    // point every client goes through (direct commands, batch steps, MCP tool
    // calls, scripts). See `profiles::guard_navigation`.

    // #437: which profile does this session use? Said once — on the session's
    // first attach, when `--browser` re-points it, and on `open`/`goto` — not
    // on every command. Written after the daemon is up (its startup clears
    // per-session sidecars).
    let mut profile_record: Option<(profiles::ProfileRow, String)> = None;
    let mut profile_note: Option<serde_json::Value> = None;
    let is_navigation = matches!(
        clean.first().map(|s| s.as_str()),
        Some("open") | Some("goto") | Some("navigate")
    );
    if flags.provider.is_none() && !flags.force_launch {
        let previous = profiles::session_profile(&flags.session);
        if let Some((ws, why)) = &profile_choice {
            if let Some(row) = profiles::row_for_ws(ws) {
                let changed = previous
                    .as_ref()
                    .and_then(|v| v.get("id").and_then(|x| x.as_str()).map(String::from))
                    != row.relay_id;
                if first_attach || changed || is_navigation {
                    profile_record = Some((row, why.clone()));
                }
            }
        } else if first_attach
            && flags.auto_connect
            && flags.cdp.is_none()
            && pinned_profile.is_none()
        {
            // The generic endpoint: whichever profile's host connected last.
            if let Some((id, _)) = connect::relay_ext_profile() {
                if let Some(row) = profiles::row_for_relay_id(&id) {
                    let why = if connect::list_relay_profiles().len() <= 1 {
                        "the only connected profile"
                    } else {
                        "the last-connected profile"
                    };
                    profile_record = Some((row, why.to_string()));
                }
            }
        } else if is_navigation && !first_attach {
            profile_note = previous;
        }
    }

    // `--remember` (issue #244, write-back): resolve the whole request NOW, while
    // nothing has happened yet. Everything it needs is already known here, and a
    // failure discovered after the page has opened would be a confusing half-done
    // command. Validated but not sent — it is sent only if the navigation works.
    let remember_request = if flags.remember {
        match remember_request(
            &clean,
            nav_urls.first().map(String::as_str),
            browser_selector.as_deref(),
            flags.no_choosebrowser,
            browser_email.as_deref(),
            choosebrowser::read_local_state().as_deref(),
            cfg!(target_os = "macos"),
        ) {
            Ok(req) => Some(req),
            Err(msg) => {
                eprintln!("{} {msg}", color::error_indicator());
                exit(1);
            }
        }
    } else {
        None
    };

    // Handle daemon management (doesn't talk to a daemon — it manages them).
    if clean.first().map(|s| s.as_str()) == Some("daemon") {
        run_daemon(&clean, flags.json);
        return;
    }

    // `sessions` is a natural top-level guess for "list my sessions" (the skill
    // advertises sessions as a feature) — route it to the daemon inventory the
    // same way `daemon status` does (issue #29).
    if clean.first().map(|s| s.as_str()) == Some("sessions") {
        run_daemon(&["sessions".to_string(), "status".to_string()], flags.json);
        return;
    }

    // One stable health command for the recovery guidance used by the bundled
    // skills. It intentionally runs before daemon bootstrap so a wedged daemon
    // cannot make the health check hang (issue #145).
    if clean.first().map(|s| s.as_str()) == Some("status") {
        run_status(&flags.session, flags.json);
        return;
    }

    // Handle close --all: close all active sessions
    if matches!(
        clean.first().map(|s| s.as_str()),
        Some("close") | Some("quit") | Some("exit")
    ) && clean.iter().any(|a| a == "--all")
    {
        run_close_all(&flags, close_all_forced(&clean));
        return;
    }

    // Handle chat command
    if clean.first().map(|s| s.as_str()) == Some("chat") {
        let message = if clean.len() > 1 {
            Some(clean[1..].join(" "))
        } else {
            None
        };
        chat::run_chat(&flags, message);
        return;
    }

    // `whoami [filter]` — which vault account each site's live session belongs
    // to (cookie-use fingerprint match). It needs the daemon + connection like
    // any action, so it parses as a harmless `cookies get` to ride the normal
    // bootstrap below, and takes over right before dispatch.
    let whoami_filter: Option<Option<String>> =
        if clean.first().map(|s| s.as_str()) == Some("whoami") {
            let filter = clean.get(1).filter(|s| !s.starts_with("--")).cloned();
            clean = vec!["cookies".to_string(), "get".to_string()];
            Some(filter)
        } else {
            None
        };

    let mut cmd = match parse_command(&clean, &flags) {
        Ok(c) => c,
        Err(e) => {
            if flags.json {
                let error_type = match &e {
                    ParseError::UnknownCommand { .. } => "unknown_command",
                    ParseError::UnknownSubcommand { .. } => "unknown_subcommand",
                    ParseError::MissingArguments { .. } => "missing_arguments",
                    ParseError::InvalidValue { .. } => "invalid_value",
                    ParseError::InvalidSessionName { .. } => "invalid_session_name",
                };
                print_json_error_with_type(e.format(), error_type);
            } else {
                output::print_error_line(&color::red(&e.format()));
            }
            exit(1);
        }
    };

    // Handle --password-stdin for auth save
    if cmd.get("action").and_then(|v| v.as_str()) == Some("auth_save") {
        if cmd.get("password").is_some() {
            eprintln!(
                "{} Passwords on the command line may be visible in process listings and shell history. Use --password-stdin instead.",
                color::warning_indicator()
            );
        }
        if cmd
            .get("passwordStdin")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            let mut pass = String::new();
            if std::io::stdin().read_line(&mut pass).is_err() || pass.is_empty() {
                eprintln!(
                    "{} Failed to read password from stdin",
                    color::error_indicator()
                );
                exit(1);
            }
            let pass = pass.trim_end_matches('\n').trim_end_matches('\r');
            if pass.is_empty() {
                eprintln!("{} Password from stdin is empty", color::error_indicator());
                exit(1);
            }
            cmd["password"] = json!(pass);
            cmd.as_object_mut().unwrap().remove("passwordStdin");
        }
    }

    // Validate session name before starting daemon
    if let Some(ref name) = flags.session_name {
        if !validation::is_valid_session_name(name) {
            let msg = validation::session_name_error(name);
            if flags.json {
                print_json_error_with_type(msg, "invalid_session_name");
            } else {
                eprintln!("{} {}", color::error_indicator(), msg);
            }
            exit(1);
        }
    }

    // Handle state management commands locally — these are pure file operations
    // that don't need a daemon, avoiding an unnecessary daemon startup that
    // would lack runtime config like session_name.
    if let Some(result) = native::state::dispatch_state_command(&cmd) {
        let action = cmd.get("action").and_then(|v| v.as_str());
        let resp = match result {
            Ok(data) => connection::Response {
                success: true,
                data: Some(data),
                error: None,
                code: None,
                retryable: None,
                warning: None,
                timing: None,
            },
            Err(e) => {
                let metadata = error_envelope::classify_error(&e);
                connection::Response {
                    code: Some(metadata.code.to_string()),
                    retryable: Some(metadata.retryable),
                    success: false,
                    data: None,
                    error: Some(e),
                    warning: None,
                    timing: None,
                }
            }
        };
        let output_opts = OutputOptions::from_flags(&flags);
        output::print_response_with_opts(&resp, action, &output_opts);
        if !resp.success {
            exit(1);
        }
        return;
    }

    // Serialize this session's relay teardown and daemon replacement across CLI
    // processes. The lock stays held through ensure_daemon below, preventing a
    // concurrent command from entering the missing-socket window (issue #152).
    let _session_lifecycle_lock = match connection::lock_session_lifecycle(&flags.session) {
        Ok(lock) => lock,
        Err(e) => {
            if flags.json {
                print_json_error(e);
            } else {
                output::print_error_line(&format!("{} {}", color::error_indicator(), e));
            }
            exit(1);
        }
    };

    // Relay self-heal (the "用不了" fix). On the extension-relay path, a dropped
    // relay used to mean either a 2-minute hang (a stale daemon still bound to the
    // dead relay ws keeps sending into the void) or a hard error that forced the
    // user to run `chrome-use reconnect` by hand. Instead, when we're about to
    // drive the user's real Chrome and the relay is down (host installed but
    // `relay-cdp-url` gone), recover automatically: drop the stale daemon so it
    // can't reuse the dead binding, then wait (bounded, with progress) for the MV3
    // worker to republish the relay — the fresh daemon then connects clean. Opt
    // out with AGENT_BROWSER_NO_AUTO_RECONNECT. Skipped for --launch/--cdp.
    //
    // A session pinned to a relay profile (#472) is healed toward THAT profile
    // only (`recover_pinned_relay`): the generic endpoint belongs to whichever
    // host wrote it last, and recovering through it could hop the session to
    // another profile. That path never kills a native host either: every
    // other profile's host is healthy and in use.
    let bound_relay_profile = if flags.cdp.is_none() {
        pinned_profile.clone()
    } else {
        None
    };
    if let Some(id) = bound_relay_profile.as_deref() {
        recover_pinned_relay(&mut flags, id);
    }
    let target_browser = flags.browser.as_deref().or(flags.profile.as_deref());
    let relay_target_up = connect::relay_url_for_selector_or_default(target_browser)
        .ok()
        .flatten()
        .is_some();
    if bound_relay_profile.is_none()
        && flags.auto_connect
        && flags.cdp.is_none()
        && !flags.force_launch
        && std::env::var("AGENT_BROWSER_NO_AUTO_RECONNECT").is_err()
        && connect::host_installed()
        && !relay_target_up
        // Don't disturb a session that already has a healthy daemon — e.g. one
        // driving a `--launch`ed browser (its follow-up commands omit --launch and
        // would otherwise trip this relay-down branch and get the daemon killed). A
        // daemon stuck on a dead relay fails this probe (hangs → times out) and is
        // healed; a live launched browser answers fast and is left alone.
        && !connection::probe_daemon_healthy(&flags.session, std::time::Duration::from_secs(3))
    {
        // The native host is global, not per-session. Serialize its restart too,
        // then recheck because another session may have restored the relay while
        // this process waited for the lock.
        let _relay_recovery_lock = match connection::lock_relay_recovery() {
            Ok(lock) => lock,
            Err(e) => {
                if flags.json {
                    print_json_error(e);
                } else {
                    eprintln!("{} {}", color::error_indicator(), e);
                }
                exit(1);
            }
        };
        if connect::relay_url_for_selector_or_default(target_browser)
            .ok()
            .flatten()
            .is_none()
            && !connection::probe_daemon_healthy(&flags.session, std::time::Duration::from_secs(3))
        {
            connection::kill_stale_daemon(&flags.session);
            connect::ensure_host_installed();
            // Reap any zombie native-messaging host (it already lost its Chrome port —
            // relay-cdp-url is gone — but the process can linger). Killing it makes the
            // extension worker's port disconnect fire immediately, so it reconnects and
            // republishes the relay in ~2s instead of waiting ~30s for the keepalive
            // alarm. Best-effort, unix-only; safe here because the relay is already down.
            #[cfg(unix)]
            {
                let _ = std::process::Command::new("pkill")
                    .args(["-f", "__nm-host"])
                    .output();
            }
            // With no browser running there is no relay to wait for: the old
            // path sat here for the full 45s deadline and then failed with "no
            // connected Chrome profiles" without ever starting Chrome (#213).
            // Start it into a specific profile (respecting --browser), which
            // also sidesteps the profile picker on multi-profile installs.
            if !connect::chrome_running() {
                match connect::launch_chrome_for_relay(flags.browser.as_deref()) {
                    Some(what) => eprintln!(
                        "{} Chrome is not running — starting {what} so the extension relay can come up…",
                        color::success_indicator()
                    ),
                    None => eprintln!(
                        "{} Chrome is not running and no profile could be started — open Chrome, then rerun",
                        color::error_indicator()
                    ),
                }
            }
            eprint!(
                "{} Chrome relay dropped — reconnecting…",
                color::success_indicator()
            );
            let _ = std::io::Write::flush(&mut std::io::stderr());
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(45);
            while connect::relay_url_for_selector_or_default(target_browser)
                .ok()
                .flatten()
                .is_none()
                && std::time::Instant::now() < deadline
            {
                std::thread::sleep(std::time::Duration::from_millis(300));
            }
            eprintln!();
        }
    }

    // Parse proxy URL to separate server from credentials for the daemon.
    let (proxy_server, proxy_username, proxy_password) = if let Some(ref proxy_str) = flags.proxy {
        let parsed = parse_proxy(proxy_str);
        (Some(parsed.server), parsed.username, parsed.password)
    } else {
        (None, None, None)
    };
    let daemon_opts = DaemonOptions {
        headed: flags.headed,
        debug: flags.debug,
        executable_path: flags.executable_path.as_deref(),
        extensions: &flags.extensions,
        init_scripts: &flags.init_scripts,
        enable: &flags.enable,
        args: flags.args.as_deref(),
        user_agent: flags.user_agent.as_deref(),
        proxy: proxy_server.as_deref(),
        proxy_bypass: flags.proxy_bypass.as_deref(),
        proxy_username: proxy_username.as_deref(),
        proxy_password: proxy_password.as_deref(),
        ignore_https_errors: flags.ignore_https_errors,
        allow_file_access: flags.allow_file_access,
        hide_scrollbars: flags.hide_scrollbars,
        profile: flags.profile.as_deref(),
        state: flags.state.as_deref(),
        provider: flags.provider.as_deref(),
        device: flags.device.as_deref(),
        session_name: flags.session_name.as_deref(),
        download_path: flags.download_path.as_deref(),
        allowed_domains: flags.allowed_domains.as_deref(),
        action_policy: flags.action_policy.as_deref(),
        confirm_actions: flags.confirm_actions.as_deref(),
        engine: flags.engine.as_deref(),
        auto_connect: flags.auto_connect,
        force_launch: flags.force_launch,
        idle_timeout: flags.idle_timeout.as_deref(),
        default_timeout: flags.default_timeout,
        cdp: flags.cdp.as_deref(),
        no_auto_dialog: flags.no_auto_dialog,
    };

    let daemon_result =
        match connection::ensure_daemon_with_lifecycle_lock(&flags.session, &daemon_opts) {
            Ok(result) => result,
            Err(e) => {
                if flags.json {
                    print_json_error(e);
                } else {
                    eprintln!("{} {}", color::error_indicator(), e);
                }
                exit(1);
            }
        };
    drop(_session_lifecycle_lock);
    if let Some((row, why)) = profile_record.take() {
        profiles::record_session_profile(&flags.session, &row, &why);
        profile_note = profiles::session_profile(&flags.session);
    }
    if let Some(note) = &profile_note {
        if !flags.json {
            eprintln!("{}", color::dim(&profiles::profile_line(note)));
        }
    }
    if flags.force_launch && flags.cdp.is_none() && flags.provider.is_none() {
        connection::mark_session_launched(&flags.session);
    }

    // Warn if launch-time options were explicitly passed via CLI but daemon was already running
    // Only warn about flags that were passed on the command line, not those set via environment
    // variables (since the daemon already uses the env vars when it starts).
    if daemon_result.already_running {
        let ignored_flags: Vec<&str> = [
            if flags.cli_executable_path {
                Some("--executable-path")
            } else {
                None
            },
            if flags.cli_extensions {
                Some("--extension")
            } else {
                None
            },
            if flags.cli_profile {
                Some("--profile")
            } else {
                None
            },
            if flags.cli_state {
                Some("--state")
            } else {
                None
            },
            if flags.cli_args { Some("--args") } else { None },
            if flags.cli_user_agent {
                Some("--user-agent")
            } else {
                None
            },
            if flags.cli_proxy {
                Some("--proxy")
            } else {
                None
            },
            if flags.cli_proxy_bypass {
                Some("--proxy-bypass")
            } else {
                None
            },
            flags.ignore_https_errors.then_some("--ignore-https-errors"),
            flags.cli_allow_file_access.then_some("--allow-file-access"),
            flags.cli_hide_scrollbars.then_some("--hide-scrollbars"),
            flags.cli_download_path.then_some("--download-path"),
            flags.cli_headed.then_some("--headed"),
        ]
        .into_iter()
        .flatten()
        .collect();

        if !ignored_flags.is_empty() && !flags.json {
            // Special case: --headed is irrelevant in CDP-attach mode
            // (your existing Chrome is always already visible). The
            // "chrome-use close + reopen" advice doesn't help because
            // the new daemon will attach right back to the same Chrome.
            // Don't suggest a useless workaround.
            if ignored_flags == ["--headed"] {
                eprintln!(
                    "{} --headed has no effect when attached to your running Chrome (it's already visible). \
                     Pass --launch to spawn a separate browser if you need to control headedness.",
                    color::warning_indicator(),
                );
            } else {
                eprintln!(
                    "{} {} ignored: daemon already running. Use 'chrome-use close' first to restart with new options.",
                    color::warning_indicator(),
                    ignored_flags.join(", ")
                );
            }
        }
    }

    // Validate mutually exclusive options
    if flags.cdp.is_some() && flags.provider.is_some() {
        let msg = "Cannot use --cdp and -p/--provider together";
        if flags.json {
            print_json_error(msg);
        } else {
            eprintln!("{} {}", color::error_indicator(), msg);
        }
        exit(1);
    }

    // Explicit --cdp or --provider disables auto-connect (they specify the connection)
    if flags.cdp.is_some() || flags.provider.is_some() {
        flags.auto_connect = false;
    }

    if flags.provider.is_some() && !flags.extensions.is_empty() {
        let msg = "Cannot use --extension with -p/--provider (extensions require local browser)";
        if flags.json {
            print_json_error(msg);
        } else {
            eprintln!("{} {}", color::error_indicator(), msg);
        }
        exit(1);
    }

    if flags.cdp.is_some() && !flags.extensions.is_empty() {
        let msg = "Cannot use --extension with --cdp (extensions require local browser)";
        if flags.json {
            print_json_error(msg);
        } else {
            eprintln!("{} {}", color::error_indicator(), msg);
        }
        exit(1);
    }

    // Auto-connect to existing browser.
    // Skip when the daemon was already running — it already holds the connection
    // from a previous auto-connect launch, so re-sending the launch command would
    // redundantly probe Chrome and may trigger repeated permission prompts (#962).
    // Also skip when the command itself is `connect`: it establishes its own
    // connection, and auto-connecting first (typically to the extension relay)
    // made the fresh daemon hold a different endpoint the instant before the
    // explicit one was requested.
    let command_is_connect = clean.first().map(|s| s.as_str()) == Some("connect");
    if flags.auto_connect && !daemon_result.already_running && !command_is_connect {
        let mut launch_cmd = json!({
            "id": gen_id(),
            "action": "launch",
            "autoConnect": true
        });

        if flags.ignore_https_errors {
            launch_cmd["ignoreHTTPSErrors"] = json!(true);
        }

        if let Some(ref cs) = flags.color_scheme {
            launch_cmd["colorScheme"] = json!(cs);
        }

        if let Some(ref dp) = flags.download_path {
            launch_cmd["downloadPath"] = json!(dp);
        }

        let err = match send_command(launch_cmd, &flags.session) {
            Ok(resp) if resp.success => None,
            Ok(resp) => Some(
                resp.error
                    .unwrap_or_else(|| "Auto-connect failed".to_string()),
            ),
            Err(e) => Some(e.to_string()),
        };

        if let Some(msg) = err {
            if flags.json {
                print_json_error(msg);
            } else {
                eprintln!("{} {}", color::error_indicator(), msg);
            }
            exit(1);
        }
    }

    // Connect via CDP if --cdp flag is set
    // Accepts either a port number (e.g., "9222") or a full URL (e.g., "ws://..." or "wss://...")
    if let Some(ref cdp_value) = flags.cdp {
        // Validate CDP value eagerly (even when daemon is already running) so
        // the user gets an immediate error for bad input instead of a silent no-op.
        let launch_cmd = if cdp_value.starts_with("ws://")
            || cdp_value.starts_with("wss://")
            || cdp_value.starts_with("http://")
            || cdp_value.starts_with("https://")
        {
            // It's a URL - use cdpUrl field
            json!({
                "id": gen_id(),
                "action": "launch",
                "cdpUrl": cdp_value
            })
        } else {
            // It's a port number - validate and use cdpPort field
            let cdp_port: u16 = match cdp_value.parse::<u32>() {
                Ok(0) => {
                    let msg = "Invalid CDP port: port must be greater than 0".to_string();
                    if flags.json {
                        print_json_error(&msg);
                    } else {
                        eprintln!("{} {}", color::error_indicator(), msg);
                    }
                    exit(1);
                }
                Ok(p) if p > 65535 => {
                    let msg = format!(
                        "Invalid CDP port: {} is out of range (valid range: 1-65535)",
                        p
                    );
                    if flags.json {
                        print_json_error(&msg);
                    } else {
                        eprintln!("{} {}", color::error_indicator(), msg);
                    }
                    exit(1);
                }
                Ok(p) => p as u16,
                Err(_) => {
                    let msg = format!(
                        "Invalid CDP value: '{}' is not a valid port number or URL",
                        cdp_value
                    );
                    if flags.json {
                        print_json_error(&msg);
                    } else {
                        eprintln!("{} {}", color::error_indicator(), msg);
                    }
                    exit(1);
                }
            };
            json!({
                "id": gen_id(),
                "action": "launch",
                "cdpPort": cdp_port
            })
        };

        // Send even when the daemon is already running: it may hold a DIFFERENT
        // connection (e.g. the auto-connect relay), and the daemon-side reuse
        // check compares endpoints — same endpoint is a cheap reuse, a different
        // one rebinds. Skipping here made `--cdp <port>` a silent no-op.
        {
            let mut launch_cmd = launch_cmd;

            if flags.ignore_https_errors {
                launch_cmd["ignoreHTTPSErrors"] = json!(true);
            }

            if let Some(ref cs) = flags.color_scheme {
                launch_cmd["colorScheme"] = json!(cs);
            }

            if let Some(ref dp) = flags.download_path {
                launch_cmd["downloadPath"] = json!(dp);
            }

            let err = match send_command(launch_cmd, &flags.session) {
                Ok(resp) if resp.success => None,
                Ok(resp) => Some(
                    resp.error
                        .unwrap_or_else(|| "CDP connection failed".to_string()),
                ),
                Err(e) => Some(e.to_string()),
            };

            if let Some(msg) = err {
                if flags.json {
                    print_json_error(msg);
                } else {
                    eprintln!("{} {}", color::error_indicator(), msg);
                }
                exit(1);
            }
        }
    }

    // Launch with cloud provider if -p flag is set
    // Skip when daemon already running — it already holds the provider connection.
    if let Some(ref provider) = flags.provider {
        if !daemon_result.already_running {
            let mut launch_cmd = json!({
                "id": gen_id(),
                "action": "launch",
                "provider": provider
            });

            if let Some(ref cs) = flags.color_scheme {
                launch_cmd["colorScheme"] = json!(cs);
            }

            let err = match send_command(launch_cmd, &flags.session) {
                Ok(resp) if resp.success => None,
                Ok(resp) => Some(
                    resp.error
                        .unwrap_or_else(|| "Provider connection failed".to_string()),
                ),
                Err(e) => Some(e.to_string()),
            };

            if let Some(msg) = err {
                if flags.json {
                    print_json_error(msg);
                } else {
                    eprintln!("{} {}", color::error_indicator(), msg);
                }
                exit(1);
            }
        }
    }

    // Launch headed browser or configure browser options (without CDP or provider)
    if (flags.headed
        || flags.cli_headed  // User explicitly set --headed (even if false)
        || flags.executable_path.is_some()
        || flags.profile.is_some()
        || flags.state.is_some()
        || flags.proxy.is_some()
        || flags.args.is_some()
        || flags.user_agent.is_some()
        || flags.allow_file_access
        || should_send_hide_scrollbars_launch_option(
            flags.cli_hide_scrollbars,
            flags.hide_scrollbars,
        )
        || flags.color_scheme.is_some()
        || flags.download_path.is_some()
        || flags.engine.is_some()
        || !flags.extensions.is_empty())
        && flags.cdp.is_none()
        && flags.provider.is_none()
        && (flags.force_launch || !flags.auto_connect)
    {
        // Launching a debug-port Chrome pops Chrome's "Allow remote debugging?"
        // consent modal (Chrome 136+). When the ab-connect relay is already up,
        // this is almost always unintended — the relay drives the user's real
        // Chrome with NO modal. Warn so the modal is self-explained and the
        // caller (often a stray --launch / --no-auto-connect) is fixable (#32).
        if !flags.json && connect::relay_url().is_some() {
            eprintln!(
                "{} launching a new Chrome with a debug port — this pops Chrome's \
                 \"Allow remote debugging?\" modal.\n  The ab-connect relay is up; \
                 drop --launch/--new (and don't pass --no-auto-connect) to drive your \
                 real Chrome with no modal.",
                color::warning_indicator()
            );
        }
        let mut launch_cmd = json!({
            "id": gen_id(),
            "action": "launch",
            "headless": !flags.headed
        });

        let cmd_obj = launch_cmd
            .as_object_mut()
            .expect("json! macro guarantees object type");

        // Add executable path if specified
        if let Some(ref exec_path) = flags.executable_path {
            cmd_obj.insert("executablePath".to_string(), json!(exec_path));
        }

        // Add profile path if specified
        if let Some(ref profile_path) = flags.profile {
            cmd_obj.insert("profile".to_string(), json!(profile_path));
        }

        // Add state path if specified
        if let Some(ref state_path) = flags.state {
            cmd_obj.insert("storageState".to_string(), json!(state_path));
        }

        if let Some(ref proxy_str) = flags.proxy {
            let parsed = parse_proxy(proxy_str);
            let mut proxy_obj = json!({ "server": parsed.server });
            if let Some(ref username) = parsed.username {
                proxy_obj["username"] = json!(username);
            }
            if let Some(ref password) = parsed.password {
                proxy_obj["password"] = json!(password);
            }
            if let Some(ref bypass) = flags.proxy_bypass {
                proxy_obj["bypass"] = json!(bypass);
            }
            cmd_obj.insert("proxy".to_string(), proxy_obj);
        }

        if let Some(ref ua) = flags.user_agent {
            cmd_obj.insert("userAgent".to_string(), json!(ua));
        }

        if let Some(ref a) = flags.args {
            // Parse args (comma or newline separated)
            let args_vec: Vec<String> = a
                .split(&[',', '\n'][..])
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            cmd_obj.insert("args".to_string(), json!(args_vec));
        }

        if !flags.extensions.is_empty() {
            cmd_obj.insert("extensions".to_string(), json!(&flags.extensions));
        }

        if flags.ignore_https_errors {
            launch_cmd["ignoreHTTPSErrors"] = json!(true);
        }

        if flags.allow_file_access {
            launch_cmd["allowFileAccess"] = json!(true);
        }

        apply_hide_scrollbars_launch_option(
            &mut launch_cmd,
            flags.cli_hide_scrollbars,
            flags.hide_scrollbars,
        );

        if let Some(ref cs) = flags.color_scheme {
            launch_cmd["colorScheme"] = json!(cs);
        }

        if let Some(ref dp) = flags.download_path {
            launch_cmd["downloadPath"] = json!(dp);
        }

        if let Some(ref domains) = flags.allowed_domains {
            launch_cmd["allowedDomains"] = json!(domains);
        }

        if let Some(ref engine) = flags.engine {
            launch_cmd["engine"] = json!(engine);
        }

        match send_command(launch_cmd, &flags.session) {
            Ok(resp) if !resp.success => {
                // Launch command failed (e.g., invalid state file, profile error)
                let error_msg = resp
                    .error
                    .unwrap_or_else(|| "Browser launch failed".to_string());
                if flags.json {
                    print_json_error(error_msg);
                } else {
                    eprintln!("{} {}", color::error_indicator(), error_msg);
                }
                exit(1);
            }
            Err(e) => {
                if flags.json {
                    print_json_error(e);
                } else {
                    eprintln!(
                        "{} Could not configure browser: {}",
                        color::error_indicator(),
                        e
                    );
                }
                exit(1);
            }
            Ok(_) => {
                // Launch succeeded
            }
        }
    }

    // `whoami` takes over here — daemon + connection are up, exactly like any
    // other action; it does its own cookies_get round-trip(s) and renders.
    if let Some(filter) = whoami_filter {
        account::run_whoami(filter.as_deref(), &flags);
        return;
    }

    // `--as <account>` guard (the wrong-account protection): verify the live
    // session IS that cookie-use account before the command executes; on
    // mismatch auto-apply its session (or fail under --as-strict). Runs on
    // every guarded invocation — sessions drift (logouts, other agents,
    // account choosers), so yesterday's verification proves nothing.
    if let Some(acct) = flags.as_account.clone() {
        if let Err(e) = account::enforce_as(&acct, flags.as_strict, &flags) {
            if flags.json {
                print_json_error(e);
            } else {
                eprintln!("{} {}", color::error_indicator(), e);
            }
            exit(1);
        }
    }

    // `jev run --goal <text> [--url <url>]`: Jev-driven agent loop, in-process.
    if cmd.get("action").and_then(|v| v.as_str()) == Some("jev") {
        let opts = jev::Options {
            goal: cmd["goal"].as_str().unwrap_or("").to_string(),
            url: cmd["url"].as_str().map(str::to_string),
            terminal_shadow: cmd["terminalShadow"].as_bool().unwrap_or(false),
        };
        match jev::run(&flags, opts) {
            Ok(result) => {
                if flags.json {
                    let done = result["status"] == "done";
                    let error = (!done).then(|| format!("jev run ended {}", result["status"]));
                    println!(
                        "{}",
                        json!({"success": done, "data": result, "error": error})
                    );
                } else {
                    let indicator = if result["status"] == "done" {
                        color::success_indicator()
                    } else {
                        color::error_indicator()
                    };
                    println!(
                        "{} {} in {:.1}s ({} decisions, {} actions) - {}",
                        indicator,
                        result["status"].as_str().unwrap_or(""),
                        result["elapsed_ms"].as_f64().unwrap_or(0.0) / 1000.0,
                        result["decisions"],
                        result["actions"],
                        result["url"].as_str().unwrap_or("")
                    );
                }
                if result["status"] != "done" {
                    exit(1);
                }
            }
            Err(e) => {
                if flags.json {
                    print_json_error(e);
                } else {
                    eprintln!("{} {}", color::error_indicator(), e);
                }
                exit(1);
            }
        }
        return;
    }

    // Handle batch command: from args or stdin
    if cmd.get("action").and_then(|v| v.as_str()) == Some("batch") {
        let bail = cmd.get("bail").and_then(|v| v.as_bool()).unwrap_or(false);
        // Steps were read (args or stdin) before the profile choice, so the
        // ChooseBrowser check saw all of them.
        run_batch(&flags, bail, batch_steps.clone().unwrap_or_default());
        return;
    }

    // Handle single-pass `script`: load the JSON op-list (file arg or stdin), send
    // it as ONE daemon round-trip, and map the 3-way exit code (0 ok / 1 runtime
    // failure or assert / 2 invalid program).
    if cmd.get("action").and_then(|v| v.as_str()) == Some("script") {
        run_script(&flags, cmd.clone());
        return;
    }

    // `auth login --bwu` (outside `bwu run`): pick the vault account for the
    // current page, then run this command again under `bwu run`.
    if cmd.get("action").and_then(|v| v.as_str()) == Some("auth_login_bwu_probe") {
        exit(bwu_login::run(&flags, &cmd));
    }

    let output_opts = OutputOptions::from_flags(&flags);

    match send_command(cmd.clone(), &flags.session) {
        Ok(mut resp) => {
            // #122: promote an adapter's application-level error into the
            // envelope (see `site::promote_adapter_error`).
            let raw_eval = cmd.get("rawEval").and_then(|v| v.as_bool()) == Some(true);
            let is_site = cmd.get("action").and_then(|v| v.as_str()) == Some("site") && !raw_eval;
            if is_site {
                site::promote_adapter_error(&mut resp);
            }
            // #479: an adapter that failed because the site is not signed in
            // is a login wall: say so, and with auto-login on, sign in and
            // run it again once.
            let site_wall = is_site && {
                let session = flags.session.clone();
                let mut navigate = |u: &str| -> Result<(), String> {
                    let r = connection::send_command(
                        json!({ "id": commands::gen_id(), "action": "navigate", "url": u }),
                        &session,
                    )?;
                    if r.success {
                        Ok(())
                    } else {
                        Err(r.error.unwrap_or_else(|| "navigation failed".into()))
                    }
                };
                let mut sign_in = |w: &Value| bwu_login::auto_login(&flags, w);
                let mut rerun = || {
                    let mut again = cmd.clone();
                    again["id"] = json!(commands::gen_id());
                    connection::send_command(again, &session)
                };
                let json_mode = flags.json;
                let ask_session = flags.session.clone();
                let mut decide = |w: &Value| {
                    autologin::decide(
                        w["host"].as_str().unwrap_or(""),
                        &ask_session,
                        w["loginUrl"].as_str(),
                        true,
                        json_mode,
                    )
                };
                site::apply_site_login_wall(
                    &cmd,
                    &mut resp,
                    site::SiteLoginIo {
                        decide: &mut decide,
                        json: flags.json,
                        navigate: &mut navigate,
                        sign_in: &mut sign_in,
                        rerun: &mut rerun,
                    },
                )
            };
            // A failed adapter whose name OpenCLI also has, as a read: run that
            // instead of failing. Precedence picks ours first, which must not
            // hide a working command behind a broken one (e.g. a page CSP that
            // blocks the adapter's API). Writes never retry — they may have
            // half-run.
            // Not behind a login wall: OpenCLI would run as the same signed-out
            // user.
            if is_site && !site_wall && !resp.success {
                let spec = cmd.get("spec").and_then(|v| v.as_str()).unwrap_or("");
                let entry = opencli::lookup(spec)
                    .filter(|e| e.get("access").and_then(|v| v.as_str()) == Some("read"));
                if let Some(entry) = entry {
                    let args =
                        opencli::fallback_args(&entry, cmd.get("siteArgs").unwrap_or(&Value::Null));
                    let env = opencli::run(spec, &entry, &args, &flags.session);
                    if env.get("success").and_then(|v| v.as_bool()) == Some(true) {
                        let first = resp
                            .error
                            .take()
                            .unwrap_or_default()
                            .lines()
                            .next()
                            .unwrap_or("")
                            .to_string();
                        if !flags.json {
                            eprintln!(
                                "{}",
                                color::dim(&format!(
                                    "site {spec} failed ({first}); used OpenCLI's {spec} instead"
                                ))
                            );
                        }
                        resp.success = true;
                        resp.data = Some(json!({
                            "result": env.get("data").cloned().unwrap_or(Value::Null),
                            "source": opencli::SOURCE_LABEL,
                            "fallbackFrom": first,
                        }));
                    }
                }
            }
            // `site verify`: compare the result's shape with the stored fixture
            // (or record it). A mismatch fails the command like an adapter error.
            if let Some(v) = cmd.get("verify").filter(|v| !v.is_null()) {
                if resp.success {
                    let spec = v.get("spec").and_then(|x| x.as_str()).unwrap_or("");
                    let write = v.get("writeFixture").and_then(|x| x.as_bool()) == Some(true);
                    let result = resp
                        .data
                        .as_ref()
                        .and_then(|d| d.get("result"))
                        .cloned()
                        .unwrap_or(Value::Null);
                    let (ok, report) = site::verify_result(spec, &result, write);
                    if !ok {
                        resp.success = false;
                        let issues: Vec<String> = report
                            .get("issues")
                            .and_then(|x| x.as_array())
                            .map(|a| {
                                a.iter()
                                    .filter_map(|i| i.as_str().map(String::from))
                                    .collect()
                            })
                            .unwrap_or_default();
                        resp.error = Some(format!(
                            "site verify {spec}: {}",
                            if issues.is_empty() {
                                "could not record the fixture".to_string()
                            } else {
                                issues.join("; ")
                            }
                        ));
                    }
                    if let Some(d) = resp.data.as_mut().and_then(|d| d.as_object_mut()) {
                        d.insert("verify".into(), report);
                    }
                }
            }
            // #434: the tab landed on a sign-in page. Say so on stderr (once
            // per host per session; the daemon decides), and with auto-login
            // on, sign in from the vault and go back.
            let wall = resp
                .data
                .as_ref()
                .and_then(|d| d.get("loginWall"))
                .filter(|w| w.get("source").and_then(|v| v.as_str()) != Some("site"))
                .cloned();
            if let Some(wall) = wall.filter(|_| !site_wall) {
                let host = wall["host"].as_str().unwrap_or("").to_string();
                // #481: sign in, skip, or ask, from the user's decision for
                // this host. The tab is on the sign-in page already.
                let outcome = autologin::decide(&host, &flags.session, None, false, flags.json);
                let set = |resp: &mut connection::Response, key: &str, v: Value| {
                    if let Some(w) = resp
                        .data
                        .as_mut()
                        .and_then(|d| d.get_mut("loginWall"))
                        .and_then(|w| w.as_object_mut())
                    {
                        w.insert(key.into(), v);
                    }
                };
                match outcome {
                    autologin::Outcome::Ask(ask) => {
                        eprintln!(
                            "{} {}",
                            color::warning_indicator(),
                            autologin::ask_text(&host, &ask)
                        );
                        set(&mut resp, "ask", ask);
                    }
                    autologin::Outcome::Skip { source } => {
                        if let Some(h) = wall.get("hint").and_then(|v| v.as_str()) {
                            eprintln!("{} {}", color::warning_indicator(), h);
                        }
                        set(&mut resp, "autoLoginDecision", json!(source));
                    }
                    autologin::Outcome::SignIn { .. } => {
                        if let Some(h) = wall.get("hint").and_then(|v| v.as_str()) {
                            eprintln!("{} {}", color::warning_indicator(), h);
                        }
                        let auto = bwu_login::auto_login(&flags, &wall);
                        match auto.get("error").and_then(|v| v.as_str()) {
                            Some(e) => eprintln!(
                                "{} login wall: auto-login failed: {e}",
                                color::warning_indicator()
                            ),
                            None => eprintln!(
                                "login wall: signed in{}",
                                auto.get("returnedTo")
                                    .and_then(|v| v.as_str())
                                    .map(|u| format!("; back on {u}"))
                                    .unwrap_or_default()
                            ),
                        }
                        set(&mut resp, "autoLogin", auto);
                    }
                }
            }
            if let Some(err) = resp.error.as_mut() {
                if err.contains("has NO snapshot refs") {
                    // Keep what to do last: agents read errors through `tail -1`.
                    let hint = flags::no_refs_session_hint(&flags);
                    match err.find(" Otherwise run") {
                        Some(i) => err.insert_str(i, &hint.replacen('\n', " ", 1)),
                        None => err.push_str(&hint),
                    }
                }
            }
            let success = resp.success;
            // A gated action reports `success: true` while it is still only
            // *pending* — the page has not been opened. `--remember` must not
            // treat that as a navigation that happened, and must not be dropped
            // when the user does approve it, so both paths are handled below
            // rather than left to the generic tail.
            let awaiting_confirmation = resp
                .data
                .as_ref()
                .and_then(|d| d.get("confirmation_required"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            // Handle interactive confirmation
            if flags.confirm_interactive {
                if let Some(data) = &resp.data {
                    if data
                        .get("confirmation_required")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false)
                    {
                        let desc = data
                            .get("description")
                            .and_then(|v| v.as_str())
                            .unwrap_or("unknown action");
                        let category = data.get("category").and_then(|v| v.as_str()).unwrap_or("");
                        let cid = data
                            .get("confirmation_id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("");

                        eprintln!("[chrome-use] Action requires confirmation:");
                        eprintln!("  {}: {}", category, desc);
                        eprint!("  Allow? [y/N]: ");

                        let mut input = String::new();
                        let approved = if std::io::IsTerminal::is_terminal(&std::io::stdin()) {
                            std::io::stdin().read_line(&mut input).is_ok()
                                && matches!(input.trim().to_lowercase().as_str(), "y" | "yes")
                        } else {
                            false
                        };

                        let confirm_cmd = if approved {
                            json!({ "id": gen_id(), "action": "confirm", "confirmationId": cid })
                        } else {
                            json!({ "id": gen_id(), "action": "deny", "confirmationId": cid })
                        };

                        match send_command(confirm_cmd, &flags.session) {
                            Ok(r) => {
                                if !approved {
                                    eprintln!("{} Action denied", color::error_indicator());
                                    exit(1);
                                }
                                print_response_with_opts(&r, None, &output_opts);
                                // The navigation only happened here, after the
                                // approval — so this is where the rule offer
                                // belongs. Reaching the generic tail instead
                                // would have dropped it without a word.
                                //
                                // Still gated on the confirmed command actually
                                // working: approving a navigation that then
                                // fails must not produce a rule for a site that
                                // never opened, which is the same mistake in a
                                // later place.
                                if let (true, Some((request, host))) =
                                    (r.success, &remember_request)
                                {
                                    send_remember_request(
                                        request,
                                        host,
                                        browser_email.as_deref().unwrap_or("that profile"),
                                    );
                                }
                            }
                            Err(e) => {
                                eprintln!("{} {}", color::error_indicator(), e);
                                exit(1);
                            }
                        }
                        return;
                    }
                }
            }
            // Extract action for context-specific output handling
            let action = cmd.get("action").and_then(|v| v.as_str());
            // #437: `--json` carries the session's profile as a field on the
            // same occasions text mode prints the "profile:" line.
            if let (true, Some(note)) = (flags.json, &profile_note) {
                match resp.data.as_mut() {
                    Some(serde_json::Value::Object(map)) => {
                        map.insert("profile".to_string(), profiles::profile_json(note));
                    }
                    None if resp.success => {
                        resp.data = Some(json!({ "profile": profiles::profile_json(note) }));
                    }
                    _ => {}
                }
            }
            print_response_with_opts(&resp, action, &output_opts);
            // `expect` is an assertion: map to a 3-way exit code so it composes in
            // shells/CI — 0 pass, 1 condition false, 2 un-evaluable (transport
            // error: no browser / bad grammar). Must run before the generic
            // `!success → exit(1)` below.
            if action == Some("expect") {
                if !success {
                    exit(2);
                }
                let pass = resp
                    .data
                    .as_ref()
                    .and_then(|d| d.get("pass"))
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                exit(if pass { 0 } else { 1 });
            }
            if !success {
                exit(1);
            }
            // Only now, and only on a navigation that actually ran: a rule
            // saying "this site belongs to that profile" is worth nothing if the
            // site would not open there, and an action still waiting for
            // confirmation has not opened anything yet.
            if let Some((request, host)) = &remember_request {
                if awaiting_confirmation {
                    eprintln!(
                        "{} --remember: this navigation is waiting for confirmation, so no rule \
                         was proposed. Run `chrome-use confirm <id>` and repeat the command with \
                         --remember once it goes through.",
                        color::warning_indicator(),
                    );
                } else {
                    send_remember_request(
                        request,
                        host,
                        browser_email.as_deref().unwrap_or("that profile"),
                    );
                }
            }
        }
        Err(e) => {
            if flags.json {
                print_json_error(e);
            } else {
                output::print_error_line(&format!("{} {}", color::error_indicator(), e));
            }
            exit(1);
        }
    }
}

fn run_script(flags: &Flags, mut cmd: serde_json::Value) {
    use std::io::Read as _;

    // Context management (#289) carries no program: `--drop <name>` and
    // `--contexts` are answered by the daemon on their own. Reading stdin for
    // them would just hang on a terminal, which is what a first try did.
    if cmd.get("dropContext").is_some() || cmd.get("listContexts").is_some() {
        dispatch_script(flags, cmd);
        return;
    }

    // Load the program: from a file arg, or stdin (`-` / no path).
    let src = match cmd.get("file").and_then(|v| v.as_str()) {
        Some(f) if f != "-" => {
            std::fs::read_to_string(f).map_err(|e| format!("cannot read script file {}: {}", f, e))
        }
        _ => {
            let mut s = String::new();
            std::io::stdin()
                .read_to_string(&mut s)
                .map(|_| s)
                .map_err(|e| format!("failed to read script from stdin: {}", e))
        }
    };
    let src = match src {
        Ok(s) => s,
        Err(e) => {
            if flags.json {
                print_json_error(e);
            } else {
                eprintln!("{} {}", color::error_indicator(), e);
            }
            exit(2);
        }
    };
    // A JSON array is the op-list (Phase 1); anything else is a JS program run
    // through the `cu.*` helpers (Phase 2). This lets `chrome-use script` accept
    // both a `prog.json` recipe and a `<<'JS' … JS` heredoc.
    let as_program = match serde_json::from_str::<serde_json::Value>(&src) {
        Ok(v @ serde_json::Value::Array(_)) => Some(v),
        _ => None,
    };

    if let Some(obj) = cmd.as_object_mut() {
        match as_program {
            Some(program) => {
                obj.insert("program".to_string(), program);
            }
            None => {
                obj.insert("source".to_string(), serde_json::Value::String(src));
            }
        }
        if let Some(a) = obj.remove("scriptArgs") {
            obj.insert("args".to_string(), a);
        }
        obj.remove("file");
    }

    dispatch_script(flags, cmd);
}

/// Send a prepared `script` command and map its 3-way exit code.
/// Split out of `run_script` so context management (#289), which has no program
/// to load, can reach it without going through the stdin read.
fn dispatch_script(flags: &Flags, cmd: serde_json::Value) {
    match send_command(cmd, &flags.session) {
        Ok(resp) => {
            let data = resp.data.clone().unwrap_or(serde_json::Value::Null);
            if flags.json {
                println!("{}", serde_json::to_string(&data).unwrap_or_default());
            } else if let Some(e) = &resp.error {
                eprintln!("{} {}", color::error_indicator(), e);
            } else if let Some(contexts) = data.get("contexts").and_then(|v| v.as_array()) {
                // `--drop` / `--contexts`: there is no program result to print,
                // so report the surviving contexts instead of nothing at all.
                if let Some(dropped) = data.get("dropped").and_then(|v| v.as_bool()) {
                    let name = data.get("context").and_then(|v| v.as_str()).unwrap_or("");
                    println!(
                        "{}",
                        if dropped {
                            format!("dropped script context `{name}`")
                        } else {
                            format!("no script context named `{name}`")
                        }
                    );
                }
                if contexts.is_empty() {
                    println!("no script contexts in this session");
                } else {
                    let names: Vec<&str> = contexts.iter().filter_map(|v| v.as_str()).collect();
                    println!("script contexts: {}", names.join(", "));
                }
            } else {
                let ret = data
                    .get("return")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);
                match ret {
                    serde_json::Value::Null => {}
                    serde_json::Value::String(s) => println!("{}", s),
                    other => println!(
                        "{}",
                        serde_json::to_string_pretty(&other).unwrap_or_default()
                    ),
                }
                for advisory in data
                    .get("advisories")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    if let Some(hint) = advisory.get("hint").and_then(Value::as_str) {
                        eprintln!("{} {}", color::warning_indicator(), hint);
                    }
                }
                if let Some(err) = data.get("error").and_then(|v| v.as_str()) {
                    eprintln!("{} {}", color::error_indicator(), err);
                }
            }
            // Exit code: 2 = invalid program (transport-level failure); 1 = runtime
            // failure / assert (ok:false); 0 = ok.
            if !resp.success {
                exit(2);
            }
            let ok = data.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
            if !ok {
                exit(1);
            }
        }
        Err(e) => {
            if flags.json {
                print_json_error(e);
            } else {
                output::print_error_line(&format!("{} {}", color::error_indicator(), e));
            }
            exit(1);
        }
    }
}

/// A `batch`'s steps: the quoted strings on the command line, or a JSON array
/// of string arrays on stdin. Read once, before anything runs, so every step
/// can be checked first.
fn load_batch_steps(clean: &[String], flags: &Flags) -> Vec<Vec<String>> {
    let arg_commands = commands::parse_command(clean, flags).ok().and_then(|cmd| {
        cmd.get("commands").and_then(|v| v.as_array()).map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .map(commands::shell_words_split)
                .collect::<Vec<Vec<String>>>()
        })
    });
    if let Some(cmds) = arg_commands {
        cmds
    } else {
        use std::io::Read as _;

        let mut input = String::new();
        if let Err(e) = std::io::stdin().read_to_string(&mut input) {
            if flags.json {
                print_json_error(format!("Failed to read stdin: {}", e));
            } else {
                eprintln!("{} Failed to read stdin: {}", color::error_indicator(), e);
            }
            exit(1);
        }

        match serde_json::from_str(&input) {
            Ok(c) => c,
            Err(e) => {
                if flags.json {
                    print_json_error(format!(
                        "Invalid JSON input: {}. Expected an array of string arrays, e.g. [[\"open\", \"https://example.com\"], [\"snapshot\"]]",
                        e
                    ));
                } else {
                    eprintln!(
                        "{} Invalid JSON input: {}. Expected an array of string arrays.",
                        color::error_indicator(),
                        e
                    );
                }
                exit(1);
            }
        }
    }
}

fn run_batch(flags: &Flags, bail: bool, commands: Vec<Vec<String>>) {
    // Check every navigating step against the session's profile before any
    // step runs. Refusing only the offending step would let the steps after
    // it (a click, a fill) act on whatever page the session already had, in
    // the account the rule said not to use.
    for step in &commands {
        if !step
            .first()
            .is_some_and(|v| NAVIGATING_VERBS.contains(&v.as_str()))
        {
            continue;
        }
        let Ok(parsed) = commands::parse_batch_step(step, flags) else {
            continue;
        };
        if let Err(e) =
            profiles::guard_outgoing(&parsed, &flags.session, connection::choosebrowser_skip())
        {
            let msg = format!("batch not run: {e}");
            if flags.json {
                print_json_error(msg);
            } else {
                eprintln!("{} {msg}", color::error_indicator());
            }
            exit(1);
        }
    }

    if commands.is_empty() {
        if flags.json {
            println!("[]");
        }
        return;
    }

    let output_opts = OutputOptions::from_flags(flags);

    let mut results: Vec<serde_json::Value> = Vec::new();
    let mut had_error = false;

    for (i, cmd_args) in commands.iter().enumerate() {
        if cmd_args.is_empty() {
            continue;
        }

        let parsed = match commands::parse_batch_step(cmd_args, flags) {
            Ok(c) => c,
            Err(e) => {
                had_error = true;
                if flags.json {
                    results.push(json!({
                        "command": cmd_args,
                        "success": false,
                        "error": e.format(),
                    }));
                    if bail {
                        break;
                    }
                } else {
                    eprintln!(
                        "{} Command {}: {}",
                        color::error_indicator(),
                        i + 1,
                        e.format()
                    );
                    if bail {
                        exit(1);
                    }
                }
                continue;
            }
        };

        let action = parsed
            .get("action")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        match send_command(parsed, &flags.session) {
            Ok(resp) => {
                if flags.json {
                    results.push(json!({
                        "command": cmd_args,
                        "success": resp.success,
                        "result": resp.data,
                        "error": resp.error,
                    }));
                } else {
                    if i > 0 {
                        println!();
                    }
                    print_response_with_opts(&resp, action.as_deref(), &output_opts);
                }
                if !resp.success {
                    had_error = true;
                    if bail {
                        if !flags.json {
                            exit(1);
                        }
                        break;
                    }
                }
            }
            Err(e) => {
                had_error = true;
                if flags.json {
                    results.push(json!({
                        "command": cmd_args,
                        "success": false,
                        "error": e.to_string(),
                    }));
                    if bail {
                        break;
                    }
                } else {
                    eprintln!("{} Command {}: {}", color::error_indicator(), i + 1, e);
                    if bail {
                        exit(1);
                    }
                }
            }
        }
    }

    if flags.json {
        println!(
            "{}",
            serde_json::to_string(&results).unwrap_or_else(|_| "[]".to_string())
        );
    }

    if had_error {
        exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Stopping another session leaves it a reason naming who stopped it;
    /// stopping your own does not.
    #[test]
    fn stopping_another_session_is_explained_to_it() {
        let reason = stopped_from_outside_reason("ab-so2", "ab-hn1", "session prune")
            .expect("another session is told");
        assert!(reason.contains("`chrome-use session prune`"), "{reason}");
        assert!(reason.contains("`ab-hn1`"), "{reason}");
        assert!(stopped_from_outside_reason("ab-hn1", "ab-hn1", "session stop").is_none());
    }

    fn target(name: &str) -> CloseAllTarget {
        CloseAllTarget {
            name: name.to_string(),
            pid: 100,
            age_secs: Some(90),
        }
    }

    #[test]
    fn close_all_with_only_own_session_proceeds() {
        let sessions = vec![target("mine")];
        assert!(close_all_blockers(&sessions, "mine", false).is_empty());
        assert!(close_all_blockers(&[], "mine", false).is_empty());
    }

    #[test]
    fn close_all_refuses_when_other_sessions_are_live() {
        let sessions = vec![target("mine"), target("other-a"), target("other-b")];
        let blockers = close_all_blockers(&sessions, "mine", false);
        let names: Vec<_> = blockers.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["other-a", "other-b"]);
        // The caller's own session need not be running for others to block.
        assert_eq!(close_all_blockers(&sessions[1..], "mine", false).len(), 2);
    }

    #[test]
    fn close_all_force_overrides_the_guard() {
        let sessions = vec![target("mine"), target("other")];
        assert!(close_all_blockers(&sessions, "mine", true).is_empty());
    }

    #[test]
    fn close_all_force_flag_spellings() {
        let args = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(!close_all_forced(&args(&["close", "--all"])));
        assert!(close_all_forced(&args(&["close", "--all", "--force"])));
        assert!(close_all_forced(&args(&["close", "--yes", "--all"])));
        assert!(close_all_forced(&args(&["quit", "--all", "-y"])));
    }

    #[test]
    fn close_all_refusal_lists_sessions_and_both_ways_out() {
        let msg = close_all_refusal_message("mine", &[target("other")]);
        assert!(msg.contains("1 other live session belongs"), "{msg}");
        assert!(msg.contains("other (pid 100, started 1m ago)"), "{msg}");
        assert!(msg.contains("`chrome-use close`"), "{msg}");
        // Agents ran the override as soon as the message named it.
        assert!(!msg.contains("--force"), "{msg}");
        assert!(msg.contains("(mine)"), "{msg}");
    }

    #[test]
    fn format_age_units() {
        assert_eq!(format_age(5), "5s");
        assert_eq!(format_age(125), "2m");
        assert_eq!(format_age(3_900), "1h5m");
        assert_eq!(format_age(90_000), "1d1h");
    }

    #[test]
    fn test_parse_proxy_simple() {
        let result = parse_proxy("http://proxy.com:8080");
        assert_eq!(result.server, "http://proxy.com:8080");
        assert!(result.username.is_none());
        assert!(result.password.is_none());
    }

    #[test]
    fn test_parse_proxy_with_auth() {
        let result = parse_proxy("http://user:pass@proxy.com:8080");
        assert_eq!(result.server, "http://proxy.com:8080");
        assert_eq!(result.username.as_deref(), Some("user"));
        assert_eq!(result.password.as_deref(), Some("pass"));
    }

    #[test]
    fn test_parse_proxy_username_only() {
        let result = parse_proxy("http://user@proxy.com:8080");
        assert_eq!(result.server, "http://proxy.com:8080");
        assert_eq!(result.username.as_deref(), Some("user"));
        assert!(result.password.is_none());
    }

    #[test]
    fn test_parse_proxy_no_protocol() {
        let result = parse_proxy("proxy.com:8080");
        assert_eq!(result.server, "proxy.com:8080");
        assert!(result.username.is_none());
    }

    #[test]
    fn test_parse_proxy_socks5() {
        let result = parse_proxy("socks5://proxy.com:1080");
        assert_eq!(result.server, "socks5://proxy.com:1080");
        assert!(result.username.is_none());
    }

    #[test]
    fn test_parse_proxy_socks5_with_auth() {
        let result = parse_proxy("socks5://admin:secret@proxy.com:1080");
        assert_eq!(result.server, "socks5://proxy.com:1080");
        assert_eq!(result.username.as_deref(), Some("admin"));
        assert_eq!(result.password.as_deref(), Some("secret"));
    }

    #[test]
    fn test_parse_proxy_complex_password() {
        let result = parse_proxy("http://user:p@ss:w0rd@proxy.com:8080");
        assert_eq!(result.server, "http://proxy.com:8080");
        assert_eq!(result.username.as_deref(), Some("user"));
        assert_eq!(result.password.as_deref(), Some("p@ss:w0rd"));
    }

    #[test]
    fn test_session_lifecycle_commands_are_not_intercepted_by_ownership() {
        assert_eq!(
            session_command_route(Some("stop")),
            SessionCommandRoute::Lifecycle
        );
        assert_eq!(
            session_command_route(Some("prune")),
            SessionCommandRoute::Lifecycle
        );
    }

    #[test]
    fn test_session_ownership_commands_remain_cli_local() {
        for sub in [
            None,
            Some("handoff"),
            Some("resume"),
            Some("status"),
            Some("list"),
        ] {
            assert_eq!(
                session_command_route(sub),
                SessionCommandRoute::Ownership,
                "unexpected route for {sub:?}"
            );
        }
    }

    #[test]
    fn test_serialize_json_value_escapes_control_characters() {
        let payload = serialize_json_value(&json!({
            "success": false,
            "error": "Daemon process exited during startup:\nline \"quoted\"\u{001b}[2mansi\u{001b}[22m",
        }));

        let parsed: serde_json::Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(parsed["success"], false);
        assert_eq!(
            parsed["error"],
            "Daemon process exited during startup:\nline \"quoted\"\u{001b}[2mansi\u{001b}[22m"
        );
    }

    #[test]
    fn test_hide_scrollbars_launch_option_serialization() {
        assert!(!should_send_hide_scrollbars_launch_option(false, true));
        assert!(should_send_hide_scrollbars_launch_option(false, false));
        assert!(should_send_hide_scrollbars_launch_option(true, true));

        let mut default_cmd = json!({ "action": "launch" });
        apply_hide_scrollbars_launch_option(&mut default_cmd, false, true);
        assert!(default_cmd.get("hideScrollbars").is_none());

        let mut config_false_cmd = json!({ "action": "launch" });
        apply_hide_scrollbars_launch_option(&mut config_false_cmd, false, false);
        assert_eq!(config_false_cmd["hideScrollbars"], false);

        let mut cli_true_cmd = json!({ "action": "launch" });
        apply_hide_scrollbars_launch_option(&mut cli_true_cmd, true, true);
        assert_eq!(cli_true_cmd["hideScrollbars"], true);
    }
    // --- `--remember` refusals ------------------------------------------------
    //
    // Each of these is a way the request would have been dropped by
    // ChooseBrowser without a dialog. Showing no dialog is also what declining
    // looks like, so every one of them has to be caught here and named.

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    const STATE: &str = r#"{"profile":{"info_cache":{
        "Default": {"gaia_id":"103695396640962395023","user_name":"leo@gmail.com"}
    }}}"#;

    #[test]
    fn remember_produces_a_request_for_the_named_profile() {
        let (url, host) = remember_request(
            &argv(&["open", "https://github.com/leeguooooo/chrome-use"]),
            nav(&argv(&["open", "https://github.com/leeguooooo/chrome-use"])).as_deref(),
            Some("leo@gmail.com"),
            false,
            Some("leo@gmail.com"),
            Some(STATE),
            true,
        )
        .expect("a valid request");
        assert_eq!(host, "github.com");
        assert_eq!(
            url,
            "choosebrowser://remember?domain=github.com\
             &target=com.google.Chrome::profile::103695396640962395023\
             &source=chrome-use"
                .replace(' ', "")
        );
    }

    /// Domain-only by design: scoping the rule to the path this one command
    /// happened to open would stop it applying on the site's next page.
    #[test]
    fn remember_writes_a_domain_rule_not_a_path_one() {
        let (url, _) = remember_request(
            &argv(&[
                "open",
                "https://github.com/leeguooooo/chrome-use/issues/244",
            ]),
            nav(&argv(&[
                "open",
                "https://github.com/leeguooooo/chrome-use/issues/244",
            ]))
            .as_deref(),
            Some("leo@gmail.com"),
            false,
            Some("leo@gmail.com"),
            Some(STATE),
            true,
        )
        .unwrap();
        assert!(!url.contains("path="), "{url}");
    }

    /// Remembering a profile the user never named would turn one of our own
    /// guesses into a permanent rule.
    #[test]
    fn remember_needs_an_explicit_browser() {
        let err = remember_request(
            &argv(&["open", "https://github.com/"]),
            nav(&argv(&["open", "https://github.com/"])).as_deref(),
            None,
            false,
            Some("leo@gmail.com"),
            Some(STATE),
            true,
        )
        .unwrap_err();
        assert!(err.contains("--browser"), "{err}");
    }

    #[test]
    fn remember_rejects_a_command_that_opens_nothing() {
        let err = remember_request(
            &argv(&["snapshot"]),
            nav(&argv(&["snapshot"])).as_deref(),
            Some("leo@gmail.com"),
            false,
            Some("leo@gmail.com"),
            Some(STATE),
            true,
        )
        .unwrap_err();
        assert!(err.contains("snapshot"), "{err}");
    }

    #[test]
    fn remember_and_no_choosebrowser_cannot_both_be_meant() {
        let err = remember_request(
            &argv(&["open", "https://github.com/"]),
            nav(&argv(&["open", "https://github.com/"])).as_deref(),
            Some("leo@gmail.com"),
            true,
            Some("leo@gmail.com"),
            Some(STATE),
            true,
        )
        .unwrap_err();
        assert!(err.contains("--no-choosebrowser"), "{err}");
    }

    /// A profile that never granted the extension's identity permission has no
    /// email, and email is the only field the relay and Chrome's registry share
    /// — so there is no way to name it in a rule.
    #[test]
    fn remember_refuses_a_profile_with_no_account() {
        let err = remember_request(
            &argv(&["open", "https://github.com/"]),
            nav(&argv(&["open", "https://github.com/"])).as_deref(),
            Some("27ade1bc"),
            false,
            None,
            Some(STATE),
            true,
        )
        .unwrap_err();
        assert!(err.contains("identity"), "{err}");

        let err = remember_request(
            &argv(&["open", "https://github.com/"]),
            nav(&argv(&["open", "https://github.com/"])).as_deref(),
            Some("someone@else.test"),
            false,
            Some("someone@else.test"),
            Some(STATE),
            true,
        )
        .unwrap_err();
        assert!(err.contains("profile registry"), "{err}");
    }
    /// ChooseBrowser is macOS-only, and `/usr/bin/open` is not a url opener
    /// anywhere else, so the flag has to refuse rather than quietly do nothing.
    #[test]
    fn remember_refuses_off_macos() {
        let err = remember_request(
            &argv(&["open", "https://github.com/"]),
            nav(&argv(&["open", "https://github.com/"])).as_deref(),
            Some("leo@gmail.com"),
            false,
            Some("leo@gmail.com"),
            Some(STATE),
            false,
        )
        .unwrap_err();
        assert!(err.contains("macOS-only"), "{err}");
    }
    /// The url the rule check sees for a command line, through the same parse
    /// that builds the command sent to the daemon.
    fn nav(raw: &[String]) -> Option<String> {
        let flags = crate::flags::parse_flags(raw);
        let clean = crate::flags::clean_args(raw);
        navigation_urls(&clean, &flags, None).into_iter().next()
    }

    /// The parsed command's url, as sent on the wire.
    fn wire(raw: &[String]) -> Option<String> {
        let flags = crate::flags::parse_flags(raw);
        let clean = crate::flags::clean_args(raw);
        commands::parse_command(&clean, &flags)
            .ok()
            .as_ref()
            .and_then(navigation_url)
    }

    /// `--browser`'s value looks like a url (no leading dash, contains a
    /// dot); the formal parse never mistakes it for the site.
    #[test]
    fn a_browser_selector_is_never_mistaken_for_the_target_url() {
        let raw = argv(&[
            "open",
            "--browser",
            "leo@gmail.com",
            "https://github.com/leeguooooo",
        ]);
        assert_eq!(nav(&raw).as_deref(), Some("https://github.com/leeguooooo"));
    }

    /// The guard checks the url that is sent, not a second guess at it: flag
    /// values that look like hosts (`--label other.example`) are skipped by
    /// the real parser, and hosts without a dot (localhost, IPv6) still count.
    #[test]
    fn the_guarded_url_is_the_url_sent_on_the_wire() {
        let cases: &[(&[&str], &str)] = &[
            (
                &["open", "--label", "other.example", "https://rule.example/x"],
                "https://rule.example/x",
            ),
            (
                &["open", "https://rule.example/x", "--label", "other.example"],
                "https://rule.example/x",
            ),
            (
                &["goto", "localhost:8765/cb/page.html"],
                "https://localhost:8765/cb/page.html",
            ),
            (
                &["open", "http://localhost:8765/"],
                "http://localhost:8765/",
            ),
            (&["navigate", "http://[::1]:8765/a"], "http://[::1]:8765/a"),
            (&["open", "claude.ai"], "https://claude.ai/"),
            (&["tab", "new", "claude.ai"], "https://claude.ai/"),
            (
                &["tab", "new", "--label", "x.y", "https://claude.ai/new"],
                "https://claude.ai/new",
            ),
            (
                &["tab", "new", "https://claude.ai/", "--label", "a"],
                "https://claude.ai/",
            ),
        ];
        for (parts, want) in cases {
            let raw = argv(parts);
            let sent = wire(&raw).unwrap_or_else(|| panic!("no wire url for {parts:?}"));
            assert_eq!(
                crate::profiles::guard_url(&sent).as_deref(),
                Some(*want),
                "wire url {sent:?} for {parts:?}"
            );
            assert_eq!(nav(&raw).as_deref(), Some(*want), "{parts:?}");
        }
        for parts in [
            &["tab", "new"][..],
            &["tab", "list"],
            &["click", "a.b"],
            &["snapshot"],
            &["open", "about:blank"],
        ] {
            assert_eq!(nav(&argv(parts)), None, "{parts:?}");
        }
    }

    /// Every navigating step of a batch is checked, through the step parser
    /// the batch itself uses.
    #[test]
    fn batch_steps_are_navigations_for_the_rule_check() {
        let raw = argv(&["batch"]);
        let flags = crate::flags::parse_flags(&raw);
        let steps = vec![
            argv(&["open", "https://a.example/"]),
            argv(&["snapshot"]),
            argv(&["tab", "new", "b.example"]),
            argv(&["click", "c.example"]),
        ];
        assert_eq!(
            navigation_urls(&raw, &flags, Some(&steps)),
            vec![
                "https://a.example/".to_string(),
                "https://b.example/".to_string()
            ]
        );
    }

    // --- session stop wording (#256) ------------------------------------------

    /// The advice must be followable after the daemon is already gone.
    #[test]
    fn stop_incomplete_message_does_not_ask_for_a_browser_that_no_longer_exists() {
        let m = session_stop_incomplete_message("cu-x", "the connected browser does not match", 3);
        assert!(m.starts_with("stopped session daemon cu-x"), "{m}");
        assert!(m.contains("3 tabs"), "{m}");
        assert!(m.contains("still open"), "{m}");
        assert!(m.contains("--force"), "{m}");
        assert!(
            !m.contains("Reconnect this session to its original browser"),
            "{m}"
        );
    }

    #[test]
    fn forced_stop_says_nothing_was_closed() {
        let m = session_stop_forced_note("cu-x", 1);
        assert!(m.contains("1 tab —"), "{m}");
        assert!(m.contains("not closed"), "{m}");
    }

    /// #309: "dropped its record" read as "the name is clear now", and it is
    /// not. The note must say the record is not the tabs, and give the move
    /// that works when the name keeps refusing.
    #[test]
    fn forced_stop_does_not_claim_the_name_is_clear() {
        let m = session_stop_forced_note("cu-x", 2);
        assert!(m.contains("the record, not the tabs"), "{m}");
        assert!(m.contains("--session"), "{m}");
    }
}

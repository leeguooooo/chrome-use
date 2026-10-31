//! Native services for the extension's agent panel.
//!
//! Discovery is intentionally independent of browser/daemon preparation. It
//! inspects installation evidence only: no agent is launched, no credential
//! file is read, and no debugger is attached. Presence is not runtime health.

use serde::Serialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

mod acp;
mod approval;
pub(crate) mod artifacts;
mod attachments;
mod backend;
mod config_edit;
mod config_transaction;
mod configuration;
mod conversation;
mod evidence;
pub(crate) mod evidence_proto;
mod heic;
mod injection_guard;
mod inspect;
mod inspection;
mod operator;
mod options;
mod progress;
mod project_hosts;
mod projects;
mod recovery;
mod redact;
mod repos;
mod schedules;
mod service;
mod sessions;
mod skill_library;
#[cfg(windows)]
mod windows;

/// Panel-only MCP tools receive their caller from the host-generated server
/// environment. Tool arguments cannot select another profile or conversation.
pub(crate) fn panel_mcp_context() -> Option<(String, String)> {
    let profile = std::env::var("CHROME_USE_PANEL_PROFILE").ok()?;
    let caller = std::env::var("CHROME_USE_PANEL_CONVERSATION").ok()?;
    if profile.is_empty()
        || profile.len() > 80
        || !profile
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        || uuid::Uuid::parse_str(&caller).is_err()
    {
        return None;
    }
    Some((profile, caller))
}
pub(crate) fn call_panel_mcp(tool: &str, arguments: &Value) -> Result<Value, String> {
    let (profile, caller) = panel_mcp_context()
        .ok_or("This tool is available only in a bound agent-panel conversation")?;
    let mut params = arguments
        .as_object()
        .cloned()
        .ok_or("Panel tool arguments must be an object")?;
    let action = params
        .remove("action")
        .and_then(|v| v.as_str().map(str::to_owned))
        .ok_or("Panel tool action is required")?;
    if params.contains_key("callerConversation") {
        return Err("A panel tool cannot override its caller".into());
    }
    params.insert("callerConversation".into(), caller.into());
    let operation = match (tool, action.as_str()) {
        ("chrome_use_schedules", "list") => "agentScheduleList",
        ("chrome_use_schedules", "save") => "agentScheduleSave",
        ("chrome_use_schedules", "delete") => "agentScheduleDelete",
        ("chrome_use_schedules", "run") => "agentScheduleRun",
        ("chrome_use_skills", "list") => "agentSkillList",
        ("chrome_use_skills", "read") => "agentSkillRead",
        ("chrome_use_skills", "save") => "agentSkillSave",
        _ => return Err("Unsupported panel tool action".into()),
    };
    service::call(operation, &Value::Object(params), Some(&profile))
}

pub fn run_service(args: &[String]) -> Result<(), String> {
    service::run(args)
}

/// Reach the running panel service of `profile` without starting one: the
/// mobile gateway's background polling must never launch a service.
pub(crate) fn call_running_service(
    operation: &str,
    params: &Value,
    profile: &str,
    budget: std::time::Duration,
) -> Result<Value, String> {
    service::call_existing(operation, params, profile, budget)
}

/// Panel profiles that have a service directory, for choosing one from the CLI.
pub(crate) fn panel_profiles() -> Result<Vec<String>, String> {
    let root = service::data_root(&DiscoveryContext::current()?)?;
    let mut profiles: Vec<String> = match fs::read_dir(root.join("profiles")) {
        Ok(entries) => entries
            .flatten()
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
            .filter_map(|e| e.file_name().into_string().ok())
            .collect(),
        Err(_) => Vec::new(),
    };
    profiles.sort();
    Ok(profiles)
}

/// Private storage shared with other local stores: directories 0700 (a
/// user-only ACL on Windows) and files written atomically at 0600.
pub(crate) mod private_fs {
    pub(crate) fn directory(path: &std::path::Path) -> Result<(), String> {
        super::config_transaction::private_tree(path)
    }
    pub(crate) fn write(path: &std::path::Path, bytes: &[u8]) -> Result<(), String> {
        super::config_transaction::atomic_write(path, bytes, None)
    }
    pub(crate) fn open(path: &std::path::Path, create: bool) -> Result<std::fs::File, String> {
        super::config_transaction::private_open(path, create)
    }
}

/// Image attachment limits a message is held to: (count, bytes per image).
pub(crate) const ATTACHMENT_LIMITS: (usize, usize) =
    (attachments::MAX_IMAGES, attachments::MAX_BYTES);

/// Stop every running panel service and its conversations (`upgrade --force`).
pub(crate) fn force_stop_services() -> Result<Vec<String>, String> {
    service::force_stop_all()
}

#[derive(Clone, Copy)]
struct AgentSpec {
    id: &'static str,
    name: &'static str,
    binary: &'static str,
    directory: &'static str,
    root_variable: Option<&'static str>,
    configs: &'static [&'static str],
}

// Files are installation evidence, not credentials. Adapter capabilities are
// negotiated separately; finding an executable never promises chat support.
const AGENTS: &[AgentSpec] = &[
    AgentSpec {
        id: "codex",
        name: "Codex",
        binary: "codex",
        directory: ".codex",
        root_variable: Some("CODEX_HOME"),
        configs: &["config.toml"],
    },
    AgentSpec {
        id: "claude",
        name: "Claude Code",
        binary: "claude",
        directory: ".claude",
        root_variable: Some("CLAUDE_CONFIG_DIR"),
        configs: &["settings.json"],
    },
    AgentSpec {
        id: "gemini",
        name: "Gemini CLI",
        binary: "gemini",
        directory: ".gemini",
        root_variable: None,
        configs: &["settings.json"],
    },
    AgentSpec {
        id: "opencode",
        name: "OpenCode",
        binary: "opencode",
        directory: ".config/opencode",
        root_variable: Some("OPENCODE_CONFIG_DIR"),
        configs: &["opencode.json", "opencode.jsonc"],
    },
    AgentSpec {
        id: "pi",
        name: "Pi",
        binary: "pi",
        directory: ".pi/agent",
        root_variable: Some("PI_CODING_AGENT_DIR"),
        configs: &["settings.json"],
    },
    AgentSpec {
        id: "qoder",
        name: "Qoder CLI",
        binary: "qodercli",
        directory: ".qoder",
        root_variable: Some("QODER_CONFIG_DIR"),
        configs: &["settings.json"],
    },
    AgentSpec {
        id: "qoder-cn",
        name: "Qoder CN",
        binary: "qoderclicn",
        directory: ".qoder-cn",
        root_variable: Some("QODERCN_CONFIG_DIR"),
        configs: &["settings.json"],
    },
    AgentSpec {
        id: "grok",
        name: "Grok Build",
        binary: "grok",
        directory: ".grok",
        root_variable: Some("GROK_HOME"),
        configs: &["config.toml"],
    },
    AgentSpec {
        id: "kimi",
        name: "Kimi Code",
        binary: "kimi",
        directory: ".kimi-code",
        root_variable: Some("KIMI_CODE_HOME"),
        configs: &["config.toml"],
    },
    AgentSpec {
        id: "qwen",
        name: "Qwen Code",
        binary: "qwen",
        directory: ".qwen",
        root_variable: Some("QWEN_HOME"),
        configs: &["settings.json"],
    },
    AgentSpec {
        id: "goose",
        name: "Goose",
        binary: "goose",
        directory: ".config/goose",
        root_variable: None,
        configs: &["config.yaml"],
    },
    AgentSpec {
        id: "hermes",
        name: "Hermes Agent",
        binary: "hermes",
        directory: ".hermes",
        root_variable: Some("HERMES_HOME"),
        configs: &["config.yaml"],
    },
    AgentSpec {
        id: "droid",
        name: "Droid",
        binary: "droid",
        directory: ".factory",
        root_variable: Some("FACTORY_HOME_OVERRIDE"),
        configs: &["settings.json"],
    },
    AgentSpec {
        id: "cline",
        name: "Cline",
        binary: "cline",
        directory: ".cline",
        root_variable: Some("CLINE_DIR"),
        configs: &["data/settings/providers.json"],
    },
];

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AgentInstallation {
    id: &'static str,
    name: &'static str,
    executable: Option<PathBuf>,
    config_directory: PathBuf,
    config_files: Vec<PathBuf>,
    directory_evidence: bool,
    // Deliberately not bool: discovery cannot establish authentication or
    // successful inference, even when an auth.json happens to be present.
    authentication: &'static str,
    runtime: &'static str,
    warnings: Vec<String>,
}

/// An agent CLI's own history files for one session, found by exact session id
/// in the file name under that agent's home (`CODEX_HOME` / `CLAUDE_CONFIG_DIR`
/// honored). Matching the id rather than a working directory keeps every other
/// session out of reach. Symlinks are reported as links, never followed.
pub(super) fn cli_session_files(agent_id: &str, session_id: &str) -> Result<Vec<PathBuf>, String> {
    if !["codex", "claude"].contains(&agent_id) {
        return Ok(vec![]);
    }
    uuid::Uuid::parse_str(session_id).map_err(|_| "The agent session id is not a UUID")?;
    let spec = AGENTS
        .iter()
        .copied()
        .find(|s| s.id == agent_id && ["codex", "claude"].contains(&s.id))
        .ok_or("This agent has no known session history layout")?;
    let root = DiscoveryContext::current()?.root(spec);
    let mut found = Vec::new();
    fn walk(directory: &Path, depth: u8, suffix: &str, found: &mut Vec<PathBuf>) {
        let Ok(entries) = fs::read_dir(directory) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(meta) = fs::symlink_metadata(&path) else {
                continue;
            };
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if meta.is_dir() && depth > 0 {
                walk(&path, depth - 1, suffix, found);
            } else if !meta.is_dir() && name.starts_with("rollout-") && name.ends_with(suffix) {
                found.push(path);
            }
        }
    }
    if agent_id == "codex" {
        let suffix = format!("-{session_id}.jsonl");
        for base in ["sessions", "archived_sessions"] {
            walk(&root.join(base), 4, &suffix, &mut found);
        }
    } else if let Ok(projects) = fs::read_dir(root.join("projects")) {
        for project in projects.flatten() {
            if !fs::symlink_metadata(project.path()).is_ok_and(|m| m.is_dir()) {
                continue;
            }
            for candidate in [
                project.path().join(format!("{session_id}.jsonl")),
                project.path().join(session_id),
            ] {
                if fs::symlink_metadata(&candidate).is_ok() {
                    found.push(candidate);
                }
            }
        }
    }
    Ok(found)
}

/// Remove one path found by [`cli_session_files`]: a directory recursively, a
/// file or symlink itself.
pub(super) fn remove_session_path(path: &Path) -> std::io::Result<()> {
    if fs::symlink_metadata(path)?.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
}

/// Agents the panel can hold a conversation with.
pub(super) const CHAT_AGENTS: &[&str] = &["codex", "claude", "opencode", "pi"];

/// The program name discovery looks for, for an agent the panel can run.
pub(super) fn agent_binary(agent_id: &str) -> Result<&'static str, String> {
    AGENTS
        .iter()
        .find(|spec| spec.id == agent_id && CHAT_AGENTS.contains(&spec.id))
        .map(|spec| spec.binary)
        .ok_or_else(|| "This agent has no conversational adapter yet".into())
}

/// PATH for agent CLIs the panel starts. Chrome launches the native host with a
/// minimal PATH, so a wrapper such as a version-manager shim would resolve a
/// different, often older, program than the user's terminal does. Use the
/// directories discovery searched, in the same order.
pub(super) fn agent_search_path() -> Option<OsString> {
    let context = DiscoveryContext::current().ok()?;
    let mut seen = std::collections::HashSet::new();
    let directories: Vec<_> = context
        .directories
        .into_iter()
        .filter(|d| seen.insert(d.clone()))
        .collect();
    std::env::join_paths(directories).ok()
}

struct DiscoveryContext {
    home: PathBuf,
    cwd: PathBuf,
    directories: Vec<PathBuf>,
    environment: HashMap<String, OsString>,
    windows: bool,
}

/// Windows environment names are case-insensitive, including inherited Path.
fn environment_key(key: &str, windows: bool) -> String {
    if windows {
        key.to_ascii_uppercase()
    } else {
        key.into()
    }
}

impl DiscoveryContext {
    fn current() -> Result<Self, String> {
        let home = dirs::home_dir().ok_or("Cannot locate the user's home directory")?;
        let environment: HashMap<_, _> = std::env::vars_os()
            .filter_map(|(k, v)| {
                k.into_string()
                    .ok()
                    .map(|k| (environment_key(&k, cfg!(windows)), v))
            })
            .collect();
        let mut directories: Vec<PathBuf> = environment
            .get("PATH")
            .map(|p| {
                std::env::split_paths(p)
                    .filter(|p| p.is_absolute())
                    .collect()
            })
            .unwrap_or_default();
        // Chrome may start the native host with a minimal PATH. Resolve
        // actual binaries without sourcing shell aliases/functions or hooks.
        for suffix in [
            ".local/bin",
            ".cargo/bin",
            ".bun/bin",
            ".volta/bin",
            ".npm-global/bin",
        ] {
            directories.push(home.join(suffix));
        }
        if cfg!(windows) {
            for key in ["APPDATA", "PNPM_HOME"] {
                if let Some(path) = environment.get(key).filter(|v| !v.is_empty()) {
                    let p = PathBuf::from(path);
                    directories.push(if key == "APPDATA" { p.join("npm") } else { p });
                }
            }
        } else {
            directories.extend([
                PathBuf::from("/opt/homebrew/bin"),
                PathBuf::from("/usr/local/bin"),
            ]);
        }
        Ok(Self {
            home,
            cwd: std::env::current_dir()
                .map_err(|_| "Cannot locate the process working directory")?,
            directories,
            environment,
            windows: cfg!(windows),
        })
    }

    fn root(&self, spec: AgentSpec) -> PathBuf {
        if let Some(value) = spec.root_variable.and_then(|key| self.variable_path(key)) {
            // Factory's override replaces HOME, not the .factory directory.
            return if spec.id == "droid" {
                value.join(".factory")
            } else {
                value
            };
        }
        // The new Kimi Code has a different home from legacy kimi-cli. Once
        // migrated, the old directory no longer controls its model settings.
        if spec.id == "kimi" && !self.home.join(".kimi-code").is_dir() {
            if let Some(value) = self.variable_path("KIMI_SHARE_DIR") {
                return value;
            }
            if self.home.join(".kimi").is_dir() {
                return self.home.join(".kimi");
            }
        }
        if spec.id == "goose" && self.windows {
            if let Some(value) = self.variable_path("APPDATA") {
                return value.join("Block/goose/config");
            }
        }
        if let Some(suffix) = spec.directory.strip_prefix(".config/") {
            if let Some(value) = self.variable_path("XDG_CONFIG_HOME") {
                return value.join(suffix);
            }
        }
        self.home.join(spec.directory)
    }

    fn variable_path(&self, key: &str) -> Option<PathBuf> {
        let value = self.environment.get(key).filter(|v| !v.is_empty())?;
        let p = PathBuf::from(value);
        if p == Path::new("~") {
            return Some(self.home.clone());
        }
        if let Ok(suffix) = p.strip_prefix("~") {
            return Some(self.home.join(suffix));
        }
        Some(if p.is_absolute() { p } else { self.cwd.join(p) })
    }

    fn config_path(&self, spec: AgentSpec, directory: &Path, name: &str) -> PathBuf {
        if spec.id == "cline" {
            if let Some(data) = self.variable_path("CLINE_DATA_DIR") {
                return data.join("settings/providers.json");
            }
        }
        directory.join(name)
    }

    fn executable(&self, binary: &str) -> Option<PathBuf> {
        let names = if self.windows {
            vec![
                format!("{binary}.exe"),
                format!("{binary}.cmd"),
                format!("{binary}.bat"),
                binary.into(),
            ]
        } else {
            vec![binary.into()]
        };
        self.directories
            .iter()
            .filter(|p| p.is_absolute())
            .find_map(|dir| {
                names
                    .iter()
                    .map(|name| dir.join(name))
                    .find(|path| is_executable(path, self.windows))
            })
    }
}

fn is_executable(path: &Path, windows: bool) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        windows || metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        let _ = windows;
        true
    }
}

/// Ignore skill installers and remnants of an uninstalled agent. Only
/// directories with meaningful contents supply directory evidence. A file
/// named "logs" still counts; a logs directory doesn't.
fn directory_evidence(path: &Path) -> Result<bool, std::io::Error> {
    match fs::read_dir(path) {
        Ok(entries) => {
            for entry in entries {
                let entry = entry?;
                let name = entry.file_name().to_string_lossy().to_lowercase();
                if name == ".ds_store" {
                    continue;
                }
                if ["skills", "logs", "log", "debug", "diagnostics", "cache"]
                    .contains(&name.as_str())
                    && fs::metadata(entry.path())?.is_dir()
                {
                    continue;
                }
                return Ok(true);
            }
            Ok(false)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

fn discover_in(context: &DiscoveryContext) -> Vec<AgentInstallation> {
    AGENTS
        .iter()
        .map(|&spec| {
            let directory = context.root(spec);
            let mut warnings = Vec::new();
            let directory_evidence = match directory_evidence(&directory) {
                Ok(value) => value,
                Err(e) => {
                    warnings.push(format!(
                        "Cannot inspect configuration directory: {:?}",
                        e.kind()
                    ));
                    false
                }
            };
            let config_files = spec
                .configs
                .iter()
                .filter_map(|name| {
                    let path = context.config_path(spec, &directory, name);
                    match fs::metadata(&path) {
                        Ok(metadata) if metadata.is_file() => Some(path),
                        Ok(_) => {
                            warnings
                                .push(format!("Configuration path is not a regular file: {name}"));
                            None
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                        Err(e) => {
                            warnings.push(format!(
                                "Cannot inspect configuration file {name}: {:?}",
                                e.kind()
                            ));
                            None
                        }
                    }
                })
                .collect();
            AgentInstallation {
                id: spec.id,
                name: spec.name,
                executable: context.executable(spec.binary),
                config_directory: directory,
                config_files,
                directory_evidence,
                authentication: "unverified",
                runtime: "unverified",
                warnings,
            }
        })
        .collect()
}

/// Dispatch panel service requests without starting a browser or modifying
/// agent configuration. Unknown operations fail explicitly, including on an
/// older host that supports discovery but not a later panel service.
#[cfg(test)]
pub fn handle(operation: &str, params: &Value) -> Result<Value, String> {
    handle_for_profile(operation, params, None)
}

/// Profile identity is supplied by the native host's extension handshake,
/// never by a panel request. Public reads need no persisted profile store.
pub fn capabilities() -> Value {
    json!({"schemaVersion":1,"mode":"full","modelInspection":true,"conversationDelete":true,"fullAccess":true,"pageContext":true,"conversationOptions":true,"imageAttachments":true,"heicConvert":true,"skillLibrary":true,"schedules":true,"inspect":true,"inspectResolve":true,"inspectReplies":1,"requestRevision":1,"turnEvidence":1,"artifacts":1,"recovery":1,"recording":1,"sessionResume":1,"projectDirectory":2,"tabGrant":1,"teach":1,"chatAgents":CHAT_AGENTS})
}

/// `chrome-use inspect link|unlink|list`; returns the process exit code.
pub fn run_inspect_cli(args: &[String], json: bool) -> i32 {
    inspect::run_cli(args, json)
}

pub fn handle_for_profile(
    operation: &str,
    params: &Value,
    profile: Option<&str>,
) -> Result<Value, String> {
    match operation {
        "discover" => {
            if !params.is_object() || params.as_object().is_some_and(|p| !p.is_empty()) {
                return Err("discover expects an empty params object".into());
            }
            let context = DiscoveryContext::current()?;
            Ok(json!({ "schemaVersion": 1, "agents": discover_in(&context) }))
        }
        "imageConvert" => heic::handle(params),
        // The side panel's Record button: compiling a recording and saving
        // the reviewed script (teach.rs).
        "teachCompile" | "teachSave" => crate::teach::panel_handle(operation, params),
        "configRead" | "configPlan" | "configApply" | "configRestore" => {
            configuration::handle(operation, params, profile)
        }
        "conversationList"
        | "tabOperator"
        | "agentInspect"
        | "conversationCreate"
        | "conversationSessionList"
        | "conversationProjectList"
        | "conversationProjectChoose"
        | "conversationResume"
        | "conversationRead"
        | "conversationSend"
        | "conversationSendDirect"
        | "conversationMessageStatus"
        | "conversationQueueEdit"
        | "conversationQueueMessageRead"
        | "conversationQueueRemove"
        | "conversationStop"
        | "conversationResumeQueue"
        | "conversationRespond"
        | "conversationDelete"
        | "conversationSetAccess"
        | "conversationTabGrant"
        | "conversationSetOptions"
        | "conversationAttachmentPut"
        | "conversationAttachmentRead"
        | "conversationArtifactRead"
        | "conversationRecoveryReview"
        | "conversationRecoveryTerminate"
        | "conversationRecoveryFork"
        | "conversationSkillSelect"
        | "skillList"
        | "skillRead"
        | "skillSave"
        | "skillImport"
        | "skillSetEnabled"
        | "scheduleList"
        | "scheduleSave"
        | "scheduleDelete"
        | "scheduleRun"
        | "agentScheduleList"
        | "agentScheduleSave"
        | "agentScheduleDelete"
        | "agentScheduleRun"
        | "agentSkillList"
        | "agentSkillRead"
        | "agentSkillSave"
        | "serviceHealth"
        | "serviceStop" => service::call(operation, params, profile),
        operation if inspect::OPERATIONS.contains(&operation) => {
            // On macOS this process runs under Chrome and carries its file
            // quarantine. Inspect & Ask runs `ocs`, a Bun-built program whose
            // extracted `.bun-*.dylib` would inherit it and be stopped by
            // Gatekeeper ("Apple cannot verify .bun-501-….dylib"). The panel
            // service runs outside Chrome's process tree (launchd), so the
            // work goes there. The acceptance hook's fake ocs runs here.
            if inspect::runs_in_service() {
                service::call(operation, params, profile)
            } else {
                inspect::handle(operation, params, profile)
            }
        }
        _ => Err(format!("Unsupported agent panel operation: {operation}")),
    }
}

#[cfg(test)]
mod tests {

    #[cfg(not(windows))]
    #[test]
    fn agent_path_includes_user_and_homebrew_directories_without_duplicates() {
        let path = agent_search_path().expect("agent PATH");
        let entries: Vec<_> = std::env::split_paths(&path).collect();
        let home = dirs::home_dir().unwrap();
        assert!(entries.contains(&home.join(".local/bin")));
        assert!(entries.contains(&PathBuf::from("/opt/homebrew/bin")));
        let unique: std::collections::HashSet<_> = entries.iter().collect();
        assert_eq!(unique.len(), entries.len());
    }
    use super::*;
    use tempfile::TempDir;
    #[test]
    fn all_platforms_advertise_the_complete_panel_contract() {
        let c = capabilities();
        assert_eq!(c["mode"], "full");
        for feature in [
            "modelInspection",
            "conversationDelete",
            "fullAccess",
            "pageContext",
            "conversationOptions",
            "imageAttachments",
            "heicConvert",
            "skillLibrary",
            "schedules",
            "inspect",
            "inspectResolve",
        ] {
            assert_eq!(c[feature], true, "{feature} unavailable");
        }
        assert_eq!(c["inspectReplies"], 1);
        assert_eq!(c["chatAgents"], json!(CHAT_AGENTS));
        assert_eq!(c["sessionResume"], 1);
        assert_eq!(c["projectDirectory"], 2);
        assert_eq!(c["tabGrant"], 1);
    }

    #[test]
    fn inherited_windows_environment_keys_match_cli_overrides() {
        assert_eq!(environment_key("Path", true), "PATH");
        assert_eq!(environment_key("AppData", true), "APPDATA");
        assert_eq!(environment_key("Codex_Home", true), "CODEX_HOME");
        assert_eq!(environment_key("Path", false), "Path");
    }

    fn context(home: &TempDir) -> DiscoveryContext {
        DiscoveryContext {
            home: home.path().into(),
            cwd: home.path().join("working"),
            directories: vec![home.path().join("bin")],
            environment: HashMap::new(),
            windows: false,
        }
    }

    fn write(path: impl AsRef<Path>, body: &str) {
        let path = path.as_ref();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }

    #[test]
    fn skill_installer_and_uninstall_leftovers_do_not_prove_installation() {
        let home = TempDir::new().unwrap();
        for name in ["skills", "logs", "cache", "diagnostics", "debug"] {
            fs::create_dir_all(home.path().join(".codex").join(name)).unwrap();
        }
        let found = discover_in(&context(&home));
        let codex = &found[0];
        assert!(!codex.directory_evidence);
        assert!(codex.executable.is_none());
        assert!(codex.config_files.is_empty());
    }

    #[test]
    fn credential_contents_are_never_read_or_returned() {
        let home = TempDir::new().unwrap();
        write(
            home.path().join(".codex/auth.json"),
            "synthetic-secret-never-return",
        );
        write(
            home.path().join(".claude/settings.json"),
            "synthetic-config-secret",
        );
        let result = serde_json::to_string(&discover_in(&context(&home))).unwrap();
        assert!(!result.contains("synthetic-"));
        let found = discover_in(&context(&home));
        assert!(found[0].directory_evidence);
        assert_eq!(found[0].authentication, "unverified");
        assert_eq!(found[1].runtime, "unverified");
        assert_eq!(found[1].config_files.len(), 1);
    }

    #[test]
    fn configured_root_and_xdg_override_default_paths() {
        let home = TempDir::new().unwrap();
        let mut c = context(&home);
        c.environment.insert(
            "CODEX_HOME".into(),
            home.path().join("custom").into_os_string(),
        );
        c.environment.insert(
            "XDG_CONFIG_HOME".into(),
            home.path().join("xdg").into_os_string(),
        );
        write(
            home.path().join("custom/config.toml"),
            "model = 'synthetic'",
        );
        write(home.path().join("xdg/opencode/opencode.jsonc"), "{}");
        let found = discover_in(&c);
        assert_eq!(
            found[0].config_files,
            vec![home.path().join("custom/config.toml")]
        );
        assert_eq!(
            found[3].config_files,
            vec![home.path().join("xdg/opencode/opencode.jsonc")]
        );
    }

    #[test]
    fn per_agent_roots_follow_backend_semantics_instead_of_one_generic_override() {
        let home = TempDir::new().unwrap();
        let mut c = context(&home);
        c.environment.insert(
            "FACTORY_HOME_OVERRIDE".into(),
            home.path().join("factory-home").into_os_string(),
        );
        c.environment.insert(
            "CLINE_DATA_DIR".into(),
            home.path().join("cline-data").into_os_string(),
        );
        c.environment.insert(
            "QWEN_HOME".into(),
            home.path().join("qwen-home").into_os_string(),
        );
        write(
            home.path().join("factory-home/.factory/settings.json"),
            "{}",
        );
        write(home.path().join("cline-data/settings/providers.json"), "{}");
        write(home.path().join("qwen-home/settings.json"), "{}");
        let found = discover_in(&c);
        assert_eq!(
            found[12].config_files,
            vec![home.path().join("factory-home/.factory/settings.json")]
        );
        assert_eq!(
            found[13].config_files,
            vec![home.path().join("cline-data/settings/providers.json")]
        );
        assert_eq!(
            found[9].config_files,
            vec![home.path().join("qwen-home/settings.json")]
        );
    }

    #[test]
    fn new_kimi_directory_wins_over_legacy_configuration() {
        let home = TempDir::new().unwrap();
        let c = context(&home);
        write(home.path().join(".kimi/config.toml"), "legacy");
        assert_eq!(
            discover_in(&c)[8].config_files,
            vec![home.path().join(".kimi/config.toml")]
        );
        write(home.path().join(".kimi-code/config.toml"), "new");
        assert_eq!(
            discover_in(&c)[8].config_files,
            vec![home.path().join(".kimi-code/config.toml")]
        );
    }

    #[test]
    fn relative_override_and_tilde_are_resolved_without_a_shell() {
        let home = TempDir::new().unwrap();
        let mut c = context(&home);
        c.environment
            .insert("OPENCODE_CONFIG_DIR".into(), "relative-config".into());
        c.environment
            .insert("PI_CODING_AGENT_DIR".into(), "~/pi-config".into());
        assert_eq!(c.root(AGENTS[3]), c.cwd.join("relative-config"));
        assert_eq!(c.root(AGENTS[4]), c.home.join("pi-config"));
    }

    #[test]
    fn windows_goose_configuration_uses_its_appdata_directory() {
        let home = TempDir::new().unwrap();
        let mut c = context(&home);
        c.windows = true;
        c.environment.insert(
            "APPDATA".into(),
            home.path().join("roaming").into_os_string(),
        );
        write(
            home.path().join("roaming/Block/goose/config/config.yaml"),
            "{}",
        );
        assert_eq!(
            discover_in(&c)[10].config_files,
            vec![home.path().join("roaming/Block/goose/config/config.yaml")]
        );
    }

    #[test]
    fn empty_root_override_does_not_mean_current_directory() {
        let home = TempDir::new().unwrap();
        let mut c = context(&home);
        c.environment.insert("CODEX_HOME".into(), OsString::new());
        assert_eq!(c.root(AGENTS[0]), home.path().join(".codex"));
    }

    #[test]
    fn relative_path_and_non_executable_file_are_not_programs() {
        let home = TempDir::new().unwrap();
        let mut c = context(&home);
        c.directories.insert(0, PathBuf::from("."));
        write(home.path().join("bin/codex"), "synthetic");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(
                home.path().join("bin/codex"),
                fs::Permissions::from_mode(0o600),
            )
            .unwrap();
            assert!(c.executable("codex").is_none());
            fs::set_permissions(
                home.path().join("bin/codex"),
                fs::Permissions::from_mode(0o700),
            )
            .unwrap();
        }
        assert_eq!(c.executable("codex"), Some(home.path().join("bin/codex")));
    }

    #[test]
    fn windows_shims_can_be_discovered_without_executing_them() {
        let home = TempDir::new().unwrap();
        let mut c = context(&home);
        c.windows = true;
        write(
            home.path().join("bin/claude.cmd"),
            "synthetic fixture, never executed",
        );
        assert_eq!(
            c.executable("claude"),
            Some(home.path().join("bin/claude.cmd"))
        );
    }

    #[test]
    fn qoder_china_uses_its_own_binary_name() {
        let home = TempDir::new().unwrap();
        let mut c = context(&home);
        c.windows = true;
        write(home.path().join("bin/qoderclicn.exe"), "synthetic");
        let found = discover_in(&c);
        assert!(found[5].executable.is_none());
        assert_eq!(
            found[6].executable,
            Some(home.path().join("bin/qoderclicn.exe"))
        );
    }

    #[cfg(unix)]
    #[test]
    fn discovery_does_not_open_credential_or_configuration_fifos() {
        use std::ffi::CString;
        use std::sync::mpsc;
        use std::time::Duration;
        let home = TempDir::new().unwrap();
        fs::create_dir_all(home.path().join(".codex")).unwrap();
        for name in ["auth.json", "config.toml"] {
            let path = home.path().join(".codex").join(name);
            let raw = CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
            // Opening either path for reading would wait for a writer forever.
            assert_eq!(unsafe { libc::mkfifo(raw.as_ptr(), 0o600) }, 0);
        }
        let c = context(&home);
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(discover_in(&c));
        });
        let found = rx
            .recv_timeout(Duration::from_secs(2))
            .expect("discovery opened a FIFO");
        assert!(found[0].config_files.is_empty());
        assert_eq!(found[0].authentication, "unverified");
    }

    #[test]
    fn bad_configuration_path_is_visible_not_silently_empty() {
        let home = TempDir::new().unwrap();
        fs::create_dir_all(home.path().join(".claude/settings.json")).unwrap();
        let found = discover_in(&context(&home));
        assert!(found[1].config_files.is_empty());
        assert_eq!(found[1].warnings.len(), 1);
    }

    #[test]
    fn unknown_and_malformed_requests_are_refused() {
        assert!(handle("run", &json!({}))
            .unwrap_err()
            .contains("Unsupported"));
        assert!(handle("discover", &json!({"program": "arbitrary"})).is_err());
        assert!(handle("discover", &Value::Null).is_err());
    }
}

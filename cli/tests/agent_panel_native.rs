//! Real framed native-host acceptance, without Chrome or an agent process.
//! Run through scripts/remote-cargo.py on the configured build host. The relay
//! registry is temporary; discovery reads installation metadata only.

use serde_json::{json, Value};
use std::io::{Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

/// Use the installed interpreter through a CRLF batch shim on Windows.
/// Spaces in the shim path exercise the real installed-CLI launch path.
#[cfg(windows)]
fn protect_fixture_endpoint(path: &std::path::Path) {
    let script = r#"$ErrorActionPreference='Stop'; $acl=New-Object System.Security.AccessControl.FileSecurity; $acl.SetAccessRuleProtection($true,$false); $user=[System.Security.Principal.WindowsIdentity]::GetCurrent().User; foreach($sid in @($user,[System.Security.Principal.SecurityIdentifier]::new('S-1-5-18'))) { $rule=[System.Security.AccessControl.FileSystemAccessRule]::new($sid,'FullControl','Allow'); $acl.AddAccessRule($rule) }; [System.IO.File]::SetAccessControl($env:PANEL_FIXTURE_FILE,$acl)"#;
    let output = Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .env("PANEL_FIXTURE_FILE", path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

// Independent cases each launch several services. Serialize case setup so a
// cold hosted runner can honor the startup deadline; concurrency inside each
// case (sibling chats, profiles, disconnects and upgrades) remains exercised.
static NATIVE_CASES: std::sync::Mutex<()> = std::sync::Mutex::new(());
fn native_case() -> std::sync::MutexGuard<'static, ()> {
    NATIVE_CASES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn test_binary() -> std::path::PathBuf {
    std::env::var_os("CHROME_USE_TEST_BIN")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| env!("CARGO_BIN_EXE_chrome-use").into())
}

fn fixture_program(root: &std::path::Path) -> std::path::PathBuf {
    let script =
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/panel_agent.py");
    #[cfg(unix)]
    {
        let _ = root;
        script
    }
    #[cfg(windows)]
    {
        let output = Command::new("python")
            .args(["-c", "import sys; print(sys.executable)"])
            .output()
            .unwrap();
        assert!(output.status.success());
        let python = String::from_utf8(output.stdout).unwrap();
        let path = root.join("fixture runner.cmd");
        std::fs::write(
            &path,
            format!(
                "@echo off\r\n@set \"PYTHONUTF8=1\"\r\n@\"{}\" \"{}\" %*\r\n",
                python.trim(),
                script.display()
            ),
        )
        .unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert!(bytes.windows(2).any(|b| b == b"\r\n"));
        path
    }
}

struct Host(Child);

impl Drop for Host {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn send(host: &mut Host, value: Value) {
    let data = serde_json::to_vec(&value).unwrap();
    let input = host.0.stdin.as_mut().unwrap();
    input.write_all(&(data.len() as u32).to_ne_bytes()).unwrap();
    input.write_all(&data).unwrap();
    input.flush().unwrap();
}

fn frames(host: &mut Host) -> mpsc::Receiver<Result<Value, String>> {
    let mut stdout = host.0.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || loop {
        let mut length = [0u8; 4];
        if stdout.read_exact(&mut length).is_err() {
            break;
        }
        let length = u32::from_ne_bytes(length) as usize;
        if length > 1024 * 1024 {
            let _ = tx.send(Err("Native stdout was not a bounded frame".into()));
            break;
        }
        let mut raw = vec![0u8; length];
        if stdout.read_exact(&mut raw).is_err() {
            break;
        }
        if tx
            .send(serde_json::from_slice(&raw).map_err(|_| "Invalid native JSON frame".into()))
            .is_err()
        {
            break;
        }
    });
    rx
}

fn panel_reply(
    host: &mut Host,
    rx: &mpsc::Receiver<Result<Value, String>>,
    request_id: &str,
    operation: &str,
    params: Value,
) -> Value {
    send(
        host,
        json!({"method":"agentPanel","requestId":request_id,"operation":operation,"params":params}),
    );
    // Generous: a slow CLI model check legitimately takes over 10 s on CI.
    let deadline = std::time::Instant::now() + Duration::from_secs(40);
    loop {
        let reply = rx
            .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
            .expect("Native panel service did not answer")
            .unwrap();
        if reply["method"] == "ping" {
            send(host, json!({"method":"pong"}));
            continue;
        }
        if reply["method"] == "pong" {
            assert_eq!(reply["params"]["agentPanel"]["schemaVersion"], 1);
            continue;
        }
        assert_eq!(
            reply["method"], "agentPanelReply",
            "Panel request entered browser command dispatch"
        );
        assert_eq!(reply["requestId"], request_id);
        return reply;
    }
}

#[cfg(any(unix, windows))]
#[test]
fn configuration_preview_apply_and_restore_use_only_isolated_fixture_settings() {
    let _case = native_case();
    let registry = tempfile::tempdir().unwrap();
    let agent_dir = registry.path().join("synthetic-agent");
    std::fs::create_dir(&agent_dir).unwrap();
    let settings = agent_dir.join("settings.json");
    let original = br#"{"model":"old","env":{"ANTHROPIC_MODEL":"pinned","ANTHROPIC_AUTH_TOKEN":"synthetic-token"},"theme":"light"}"#;
    std::fs::write(&settings, original).unwrap();
    let mut host = Host(
        Command::new(test_binary())
            .args(["__nm-host", "chrome-extension://agent-panel-fixture/"])
            .env("CHROME_USE_RELAY_DIR", registry.path())
            .env("CLAUDE_CONFIG_DIR", &agent_dir)
            .env("CHROME_USE_NO_UPDATE_CHECK", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let rx = frames(&mut host);
    let unbound = panel_reply(
        &mut host,
        &rx,
        "unbound",
        "configPlan",
        json!({"agentId":"claude","changes":{"model":"new"}}),
    );
    assert!(unbound["error"]
        .as_str()
        .unwrap()
        .contains("bound Chrome profile"));
    assert_eq!(std::fs::read(&settings).unwrap(), original);
    send(
        &mut host,
        json!({"method":"hello","version":"fixture","profileId":"native-config-fixture"}),
    );
    let read = panel_reply(
        &mut host,
        &rx,
        "read",
        "configRead",
        json!({"agentId":"claude"}),
    );
    assert!(read.get("error").is_none());
    assert_eq!(read["result"]["configuration"]["agentId"], "claude");
    assert!(!read.to_string().contains("synthetic-token"));
    let planned = panel_reply(
        &mut host,
        &rx,
        "preview",
        "configPlan",
        json!({"agentId":"claude","changes":{"model":"new","effort":"high"}}),
    );
    assert!(planned.get("error").is_none());
    assert_eq!(planned["result"]["configuration"]["phase"], "planned");
    assert!(!planned.to_string().contains("synthetic-token"));
    assert_eq!(std::fs::read(&settings).unwrap(), original);
    let plan = planned["result"]["configuration"]["planId"]
        .as_str()
        .unwrap();
    assert!(registry
        .path()
        .join(format!(
            "agent-panel/profiles/native-config-fixture/configuration/{plan}.json"
        ))
        .is_file());
    let applied = panel_reply(
        &mut host,
        &rx,
        "apply",
        "configApply",
        json!({"agentId":"claude","planId":plan}),
    );
    assert_eq!(applied["result"]["configuration"]["phase"], "applied");
    assert!(!applied.to_string().contains("synthetic-token"));
    let current: Value = serde_json::from_slice(&std::fs::read(&settings).unwrap()).unwrap();
    assert_eq!(current["model"], "new");
    assert_eq!(current["effortLevel"], "high");
    assert_eq!(current["env"]["ANTHROPIC_AUTH_TOKEN"], "synthetic-token");
    assert!(current["env"].get("ANTHROPIC_MODEL").is_none());
    let restored = panel_reply(
        &mut host,
        &rx,
        "restore",
        "configRestore",
        json!({"agentId":"claude","planId":plan}),
    );
    assert_eq!(restored["result"]["configuration"]["phase"], "restored");
    assert_eq!(std::fs::read(&settings).unwrap(), original);
    let invalid = panel_reply(
        &mut host,
        &rx,
        "unknown-field",
        "configPlan",
        json!({"agentId":"claude","changes":{"apiKey":"synthetic-new-secret"}}),
    );
    assert!(invalid.get("error").is_some());
    assert_eq!(std::fs::read(&settings).unwrap(), original);
}

#[cfg(any(unix, windows))]
struct ServiceFixture {
    root: std::path::PathBuf,
    profile: String,
    chats: Vec<String>,
}

#[cfg(any(unix, windows))]
impl ServiceFixture {
    fn request(&self, operation: &str, params: Value) -> Option<Value> {
        use std::net::TcpStream;
        let endpoint: Value = serde_json::from_slice(
            &std::fs::read(self.root.join(format!(
                "agent-panel/profiles/{}/service.json",
                self.profile
            )))
            .ok()?,
        )
        .ok()?;
        if endpoint["profile"] != self.profile {
            return None;
        }
        let mut stream = TcpStream::connect_timeout(
            &endpoint["address"].as_str()?.parse().ok()?,
            Duration::from_millis(300),
        )
        .ok()?;
        stream.set_read_timeout(Some(Duration::from_secs(3))).ok()?;
        let id = uuid::Uuid::new_v4().to_string();
        let raw=json!({"capability":endpoint["capability"],"requestId":id,"operation":operation,"params":params}).to_string().into_bytes();
        stream.write_all(&(raw.len() as u32).to_le_bytes()).ok()?;
        stream.write_all(&raw).ok()?;
        let mut len = [0u8; 4];
        stream.read_exact(&mut len).ok()?;
        let len = u32::from_le_bytes(len) as usize;
        if len > 768 * 1024 {
            return None;
        }
        let mut raw = vec![0u8; len];
        stream.read_exact(&mut raw).ok()?;
        let reply: Value = serde_json::from_slice(&raw).ok()?;
        if reply["requestId"] != id {
            return None;
        }
        Some(reply)
    }
}

#[cfg(any(unix, windows))]
impl Drop for ServiceFixture {
    fn drop(&mut self) {
        for id in &self.chats {
            let _ = self.request("conversationStop", json!({"conversationId":id}));
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(8);
        while std::time::Instant::now() < deadline {
            let Some(health) = self.request("health", json!({})) else {
                break;
            };
            if health["result"]["active"] == 0 {
                let _ = self.request("serviceStop", json!({}));
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

/// Loaded `com.chrome-use.panel-service.*` launchd jobs whose arguments name
/// `root`. Starting the service must not leave one per data directory.
#[cfg(target_os = "macos")]
fn loaded_service_jobs(root: &std::path::Path) -> Vec<String> {
    let name = root.file_name().unwrap().to_string_lossy().into_owned();
    let uid = unsafe { libc::getuid() };
    let list = Command::new("/bin/launchctl").arg("list").output().unwrap();
    String::from_utf8_lossy(&list.stdout)
        .lines()
        .filter_map(|line| line.split_whitespace().nth(2))
        .filter(|label| label.starts_with("com.chrome-use.panel-service."))
        .filter(|label| {
            Command::new("/bin/launchctl")
                .args(["print", &format!("gui/{uid}/{label}")])
                .output()
                .is_ok_and(|out| String::from_utf8_lossy(&out.stdout).contains(&name))
        })
        .map(String::from)
        .collect()
}

#[cfg(any(unix, windows))]
fn conversation_poll(
    host: &mut Host,
    rx: &mpsc::Receiver<Result<Value, String>>,
    id: &str,
    phase: &str,
) -> Value {
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    loop {
        let reply = panel_reply(
            host,
            rx,
            "poll-conversation",
            "conversationRead",
            json!({"conversationId":id,"after":0}),
        );
        assert!(
            reply.get("error").is_none(),
            "Conversation read failed: {}",
            reply["error"]
        );
        if reply["result"]["conversation"]["phase"] == phase {
            return reply["result"].clone();
        }
        assert!(
            std::time::Instant::now() < deadline,
            "Conversation did not reach {phase}; observed {}",
            reply["result"]["conversation"]["phase"]
        );
        std::thread::sleep(Duration::from_millis(40));
    }
}

#[test]
#[cfg(any(unix, windows))]
fn both_cli_protocols_keep_conversations_alive_across_native_host_disconnects() {
    let _case = native_case();
    let registry = tempfile::tempdir().unwrap();
    let profile = "native-conversation-fixture";
    let mut cleanup = ServiceFixture {
        root: registry.path().into(),
        profile: profile.into(),
        chats: vec![],
    };
    let spawn = || {
        Host(
            Command::new(test_binary())
                .args(["__nm-host", "chrome-extension://agent-panel-fixture/"])
                .env("CHROME_USE_RELAY_DIR", registry.path())
                .env("CHROME_USE_NO_UPDATE_CHECK", "1")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        )
    };
    let mut host = spawn();
    let mut rx = frames(&mut host);
    send(
        &mut host,
        json!({"method":"hello","version":"fixture","profileId":profile}),
    );
    let health = panel_reply(&mut host, &rx, "health", "serviceHealth", json!({}));
    assert!(health.get("error").is_none());
    let service_pid = health["result"]["pid"].clone();
    let fixture = fixture_program(registry.path());
    let relative = panel_reply(
        &mut host,
        &rx,
        "inspect-relative",
        "agentInspect",
        json!({"agentId":"codex","executable":"relative-program"}),
    );
    assert!(relative["error"]
        .as_str()
        .unwrap()
        .contains("absolute path"));
    for agent in ["codex", "claude"] {
        let inspected = panel_reply(
            &mut host,
            &rx,
            "inspect-cli",
            "agentInspect",
            json!({"agentId":agent,"executable":fixture}),
        );
        assert!(
            inspected.get("error").is_none(),
            "Metadata inspection failed: {}",
            inspected["error"]
        );
        let metadata = &inspected["result"]["inspection"];
        assert_eq!(metadata["source"], "cliProtocol");
        assert_eq!(metadata["modelAccessVerified"], false);
        assert_eq!(metadata["processExitConfirmed"], true);
        assert_eq!(metadata["complete"], true);
        assert_eq!(
            metadata["models"].as_array().unwrap().len(),
            if agent == "codex" { 2 } else { 1 }
        );
        let serialized = metadata.to_string();
        assert!(!serialized.contains("private@synthetic.invalid"));
        assert!(!serialized.contains("synthetic-never-transport"));
    }
    let empty_after_inspection = panel_reply(
        &mut host,
        &rx,
        "inspect-no-conversations",
        "conversationList",
        json!({}),
    );
    assert!(empty_after_inspection["result"]["conversations"]
        .as_array()
        .unwrap()
        .is_empty());
    for agent in ["codex", "claude"] {
        let created = panel_reply(
            &mut host,
            &rx,
            "create",
            "conversationCreate",
            json!({"agentId":agent,"executable":fixture}),
        );
        assert!(created.get("error").is_none());
        let id = created["result"]["conversation"]["id"]
            .as_str()
            .unwrap()
            .to_string();
        cleanup.chats.push(id.clone());
        let message = uuid::Uuid::new_v4().to_string();
        let sent = panel_reply(
            &mut host,
            &rx,
            "send-first",
            "conversationSend",
            json!({"conversationId":id,"messageId":message,"text":"first"}),
        );
        assert!(sent.get("error").is_none());
        let done = conversation_poll(&mut host, &rx, &id, "idle");
        assert_eq!(
            done["conversation"]["model"],
            format!("synthetic-{agent}-model")
        );
        let repeated = panel_reply(
            &mut host,
            &rx,
            "send-duplicate",
            "conversationSend",
            json!({"conversationId":id,"messageId":message,"text":"first"}),
        );
        assert_eq!(repeated["result"]["message"]["accepted"], false);
        let status = panel_reply(
            &mut host,
            &rx,
            "message-status",
            "conversationMessageStatus",
            json!({"conversationId":id,"messageId":message}),
        );
        assert_eq!(status["result"]["message"]["id"], message);
        assert_eq!(status["result"]["message"]["known"], true);
        assert_eq!(status["result"]["message"]["status"], "previouslyAccepted");
        let unknown = panel_reply(
            &mut host,
            &rx,
            "unknown-message-status",
            "conversationMessageStatus",
            json!({"conversationId":id,"messageId":uuid::Uuid::new_v4().to_string()}),
        );
        assert_eq!(unknown["result"]["message"]["known"], false);
        assert_eq!(unknown["result"]["message"]["status"], "unconfirmed");
        let history = conversation_poll(&mut host, &rx, &id, "idle");
        assert_eq!(
            history["journal"]["events"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|e| e["kind"] == "user")
                .count(),
            1
        );
        let waiting = uuid::Uuid::new_v4().to_string();
        panel_reply(
            &mut host,
            &rx,
            "send-wait",
            "conversationSend",
            json!({"conversationId":id,"messageId":waiting,"text":"wait"}),
        );
        conversation_poll(&mut host, &rx, &id, "running");
        drop(host);
        host = spawn();
        rx = frames(&mut host);
        send(
            &mut host,
            json!({"method":"hello","version":"fixture","profileId":profile}),
        );
        let reconnected = panel_reply(
            &mut host,
            &rx,
            "health-reconnect",
            "serviceHealth",
            json!({}),
        );
        assert_eq!(reconnected["result"]["pid"], service_pid);
        let observed = conversation_poll(&mut host, &rx, &id, "running");
        assert_eq!(observed["conversation"]["activeMessageId"], waiting);
        panel_reply(
            &mut host,
            &rx,
            "stop",
            "conversationStop",
            json!({"conversationId":id}),
        );
        conversation_poll(&mut host, &rx, &id, "interrupted");
        panel_reply(
            &mut host,
            &rx,
            "resume",
            "conversationResumeQueue",
            json!({"conversationId":id}),
        );
        panel_reply(
            &mut host,
            &rx,
            "send-approval",
            "conversationSend",
            json!({"conversationId":id,"messageId":uuid::Uuid::new_v4().to_string(),"text":"approval"}),
        );
        let blocked = conversation_poll(&mut host, &rx, &id, "waitingInput");
        let shown = blocked["journal"]["events"]
            .as_array()
            .unwrap()
            .iter()
            .rev()
            .find(|e| e["kind"] == "request")
            .unwrap()["data"]
            .clone();
        let request = shown["requestId"].clone();
        let answer = if agent == "codex" {
            json!({"decision":"accept"})
        } else {
            // An older panel echoes the request it was shown plus its answers;
            // anything else is refused (plan 7.3).
            let mut input = shown["params"]["input"].clone();
            input["answers"] = json!({"Synthetic question":"One"});
            json!({"behavior":"allow","updatedInput":input})
        };
        let reply = panel_reply(
            &mut host,
            &rx,
            "answer",
            "conversationRespond",
            json!({"conversationId":id,"requestId":request,"result":answer}),
        );
        assert_eq!(reply["result"]["response"]["confirmed"], false);
        let completed = conversation_poll(&mut host, &rx, &id, "idle");
        assert!(completed["journal"]["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(
                |event| event["kind"] == "requestResolved" && event["data"]["requestId"] == request
            ));
        panel_reply(
            &mut host,
            &rx,
            "send-fail",
            "conversationSend",
            json!({"conversationId":id,"messageId":uuid::Uuid::new_v4().to_string(),"text":"fail"}),
        );
        let queued_id = uuid::Uuid::new_v4().to_string();
        panel_reply(
            &mut host,
            &rx,
            "queue-next",
            "conversationSend",
            json!({"conversationId":id,"messageId":queued_id,"text":"queued-after-failure"}),
        );
        let failed = conversation_poll(&mut host, &rx, &id, "failed");
        assert_eq!(failed["conversation"]["queuePaused"], true);
        assert_eq!(failed["conversation"]["queued"], 1);
        assert_eq!(failed["queuedMessages"][0]["id"], queued_id);
        let full = panel_reply(
            &mut host,
            &rx,
            "queue-full",
            "conversationQueueMessageRead",
            json!({"conversationId":id,"messageId":queued_id}),
        );
        assert_eq!(
            full["result"]["queuedMessage"]["text"],
            "queued-after-failure"
        );
        let replacement = uuid::Uuid::new_v4().to_string();
        let edited = panel_reply(
            &mut host,
            &rx,
            "queue-edit",
            "conversationQueueEdit",
            json!({"conversationId":id,"messageId":queued_id,"expectedText":"queued-after-failure","replacementId":replacement,"text":"edited-after-failure"}),
        );
        assert_eq!(
            edited["result"]["queueChange"]["replacementId"],
            replacement
        );
        let stale = panel_reply(
            &mut host,
            &rx,
            "queue-stale",
            "conversationQueueRemove",
            json!({"conversationId":id,"messageId":queued_id,"expectedText":"queued-after-failure"}),
        );
        assert!(stale["error"].is_string());
        let fresh = uuid::Uuid::new_v4().to_string();
        panel_reply(
            &mut host,
            &rx,
            "send-after-failure",
            "conversationSend",
            json!({"conversationId":id,"messageId":fresh,"text":"continue-after-failure"}),
        );
        let before_resume = conversation_poll(&mut host, &rx, &id, "idle");
        assert_eq!(before_resume["conversation"]["queuePaused"], true);
        assert!(before_resume["journal"]["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|event| event["kind"] == "textDelta" && event["data"]["messageId"] == fresh));
        assert_eq!(before_resume["queuedMessages"].as_array().unwrap().len(), 1);
        assert_eq!(
            before_resume["queuedMessages"][0]["text"],
            "edited-after-failure"
        );
        panel_reply(
            &mut host,
            &rx,
            "resume-after-fail",
            "conversationResumeQueue",
            json!({"conversationId":id}),
        );
        let resumed = conversation_poll(&mut host, &rx, &id, "idle");
        assert_eq!(resumed["conversation"]["queued"], 0);
        let output: String = resumed["journal"]["events"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|event| event["kind"] == "textDelta")
            .filter_map(|event| event["data"]["text"].as_str())
            .collect();
        assert!(output.contains("synthetic reply: edited-after-failure"));
        assert!(output.contains("synthetic reply: continue-after-failure"));
        let busy_id = uuid::Uuid::new_v4().to_string();
        panel_reply(
            &mut host,
            &rx,
            "direct-wait",
            "conversationSend",
            json!({"conversationId":id,"messageId":busy_id,"text":"wait"}),
        );
        conversation_poll(&mut host, &rx, &id, "running");
        let ordinary = uuid::Uuid::new_v4().to_string();
        panel_reply(
            &mut host,
            &rx,
            "ordinary-before-direct",
            "conversationSend",
            json!({"conversationId":id,"messageId":ordinary,"text":"ordinary-after-direct"}),
        );
        let direct = uuid::Uuid::new_v4().to_string();
        let ack = panel_reply(
            &mut host,
            &rx,
            "direct-send",
            "conversationSendDirect",
            json!({"conversationId":id,"messageId":direct,"text":"direct-after-stop"}),
        );
        assert_eq!(ack["result"]["message"]["accepted"], true);
        let after_direct = conversation_poll(&mut host, &rx, &id, "idle");
        assert_eq!(after_direct["conversation"]["queuePaused"], true);
        assert_eq!(after_direct["queuedMessages"][0]["id"], ordinary);
        let events = after_direct["journal"]["events"].as_array().unwrap();
        let stopped_at = events
            .iter()
            .position(|event| event["kind"] == "terminal" && event["data"]["messageId"] == busy_id)
            .unwrap();
        let direct_output = events
            .iter()
            .position(|event| event["kind"] == "textDelta" && event["data"]["messageId"] == direct)
            .unwrap();
        assert!(
            stopped_at < direct_output,
            "Direct text executed before confirmed interruption"
        );
        let repeat = panel_reply(
            &mut host,
            &rx,
            "direct-repeat",
            "conversationSendDirect",
            json!({"conversationId":id,"messageId":direct,"text":"direct-after-stop"}),
        );
        assert_eq!(repeat["result"]["message"]["accepted"], false);
        panel_reply(
            &mut host,
            &rx,
            "direct-resume-ordinary",
            "conversationResumeQueue",
            json!({"conversationId":id}),
        );
        let resumed = conversation_poll(&mut host, &rx, &id, "idle");
        // The fixture's version is not on the generation-reuse allowlist, so
        // every turn runs on a new process (plan D1a); each one resumes the
        // same CLI session instead of starting a history-less one.
        let ready: Vec<&Value> = resumed["journal"]["events"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| e["kind"] == "ready")
            .collect();
        let sessions: std::collections::HashSet<_> = ready
            .iter()
            .map(|e| e["data"]["sessionId"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(sessions.len(), 1, "a turn started a new CLI session");
        let retired = resumed["journal"]["events"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| e["kind"] == "generationRetired")
            .count();
        assert!(
            retired >= ready.len() - 1,
            "a process was replaced without retiring"
        );
    }
    for agent in ["codex", "claude"] {
        let created = panel_reply(
            &mut host,
            &rx,
            "exit-direct-create",
            "conversationCreate",
            json!({"agentId":agent,"executable":fixture}),
        );
        let id = created["result"]["conversation"]["id"]
            .as_str()
            .unwrap()
            .to_string();
        cleanup.chats.push(id.clone());
        panel_reply(
            &mut host,
            &rx,
            "exit-direct-wait",
            "conversationSend",
            json!({"conversationId":id,"messageId":uuid::Uuid::new_v4().to_string(),"text":"wait-exit"}),
        );
        conversation_poll(&mut host, &rx, &id, "running");
        let direct = uuid::Uuid::new_v4().to_string();
        panel_reply(
            &mut host,
            &rx,
            "exit-direct-send",
            "conversationSendDirect",
            json!({"conversationId":id,"messageId":direct,"text":"direct-after-process-exit"}),
        );
        let done = conversation_poll(&mut host, &rx, &id, "idle");
        let events = done["journal"]["events"].as_array().unwrap();
        let exited = events
            .iter()
            .position(|event| {
                event["kind"] == "exited" && event["data"]["processExitConfirmed"] == true
            })
            .unwrap();
        let output = events
            .iter()
            .position(|event| event["kind"] == "textDelta" && event["data"]["messageId"] == direct)
            .unwrap();
        assert!(
            exited < output,
            "New process sent direct text before confirmed old-process exit"
        );
        let pids: std::collections::HashSet<_> = events
            .iter()
            .filter(|event| event["kind"] == "ready")
            .map(|event| event["data"]["pid"].as_u64().unwrap())
            .collect();
        assert_eq!(
            pids.len(),
            2,
            "Confirmed exit did not start a fresh CLI process"
        );
    }
    let invalid = panel_reply(
        &mut host,
        &rx,
        "direct-invalid-create",
        "conversationCreate",
        json!({"agentId":"codex","executable":registry.path().join("missing-synthetic-cli")}),
    );
    let invalid_id = invalid["result"]["conversation"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    cleanup.chats.push(invalid_id.clone());
    let invalid_message = uuid::Uuid::new_v4().to_string();
    let not_started = panel_reply(
        &mut host,
        &rx,
        "direct-invalid-send",
        "conversationSendDirect",
        json!({"conversationId":invalid_id,"messageId":invalid_message,"text":"never reaches a process"}),
    );
    // No process ever existed, so nothing can have run: the start simply
    // failed and the message is not kept as an unconfirmed active turn.
    let conversation = &not_started["result"]["conversation"];
    assert_eq!(conversation["phase"], "failed");
    assert!(conversation["activeMessageId"].is_null());
    let refused = conversation_poll(&mut host, &rx, &invalid_id, "failed");
    let start_failed = refused["journal"]["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|event| event["kind"] == "turnStartFailed")
        .expect("turnStartFailed event");
    assert_eq!(start_failed["data"]["messageId"], invalid_message);
    assert_eq!(start_failed["data"]["delivered"], false);
    assert!(start_failed["data"]["retiredGeneration"].is_null());
    let other_profile = "native-other-profile-fixture";
    let _other_cleanup = ServiceFixture {
        root: registry.path().into(),
        profile: other_profile.into(),
        chats: vec![],
    };
    let mut other_host = spawn();
    let other_rx = frames(&mut other_host);
    send(
        &mut other_host,
        json!({"method":"hello","version":"fixture","profileId":other_profile}),
    );
    let forbidden = panel_reply(
        &mut other_host,
        &other_rx,
        "cross-profile",
        "conversationRead",
        json!({"conversationId":cleanup.chats[0],"after":0}),
    );
    assert!(forbidden["error"]
        .as_str()
        .unwrap()
        .contains("this profile"));
    let other_health = panel_reply(
        &mut other_host,
        &other_rx,
        "other-health",
        "serviceHealth",
        json!({}),
    );
    assert_ne!(other_health["result"]["pid"], service_pid);
    let endpoint_path = registry
        .path()
        .join(format!("agent-panel/profiles/{profile}/service.json"));
    let endpoint_data: Value =
        serde_json::from_slice(&std::fs::read(&endpoint_path).unwrap()).unwrap();
    let mut unauthenticated =
        std::net::TcpStream::connect(endpoint_data["address"].as_str().unwrap()).unwrap();
    unauthenticated
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let raw=json!({"capability":"synthetic-wrong-capability","requestId":"denied","operation":"health","params":{}}).to_string().into_bytes();
    unauthenticated
        .write_all(&(raw.len() as u32).to_le_bytes())
        .unwrap();
    unauthenticated.write_all(&raw).unwrap();
    let mut byte = [0u8; 1];
    assert_eq!(unauthenticated.read(&mut byte).unwrap(), 0);
    let history_before_restart = conversation_poll(&mut host, &rx, &cleanup.chats[0], "idle");
    let users_before_restart: Vec<Value> = history_before_restart["journal"]["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|event| event["kind"] == "user")
        .map(|event| event["data"].clone())
        .collect();
    let stopped = panel_reply(&mut host, &rx, "service-stop", "serviceStop", json!({}));
    assert_eq!(stopped["result"]["service"]["stopRequested"], true);
    let endpoint = registry
        .path()
        .join(format!("agent-panel/profiles/{profile}/service.json"));
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while endpoint.exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "Service shutdown was not confirmed"
        );
        std::thread::sleep(Duration::from_millis(40));
    }
    // Restart the independent service without resubmitting any prior intent.
    let restarted = panel_reply(
        &mut host,
        &rx,
        "service-restart",
        "serviceHealth",
        json!({}),
    );
    assert!(restarted.get("error").is_none());
    assert_ne!(restarted["result"]["pid"], service_pid);
    let id = &cleanup.chats[0];
    let before = conversation_poll(&mut host, &rx, id, "idle");
    let count = before["journal"]["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"] == "user")
        .count();
    let users_after_restart: Vec<Value> = before["journal"]["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|event| event["kind"] == "user")
        .map(|event| event["data"].clone())
        .collect();
    assert_eq!(
        users_after_restart, users_before_restart,
        "Service restart changed prior message identity or text"
    );
    panel_reply(
        &mut host,
        &rx,
        "after-restart",
        "conversationSend",
        json!({"conversationId":id,"messageId":uuid::Uuid::new_v4().to_string(),"text":"new-after-restart"}),
    );
    let after = conversation_poll(&mut host, &rx, id, "idle");
    assert_eq!(
        after["journal"]["events"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| e["kind"] == "user")
            .count(),
        count + 1
    );
}

#[test]
fn discovery_and_errors_are_correlated_without_entering_browser_dispatch() {
    let _case = native_case();
    let registry = tempfile::tempdir().unwrap();
    let mut host = Host(
        Command::new(test_binary())
            .args(["__nm-host", "chrome-extension://agent-panel-fixture/"])
            .env("CHROME_USE_RELAY_DIR", registry.path())
            .env("CHROME_USE_NO_UPDATE_CHECK", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let mut stdout = host.0.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel::<Result<Value, String>>();
    std::thread::spawn(move || loop {
        let mut length = [0u8; 4];
        if stdout.read_exact(&mut length).is_err() {
            break;
        }
        let length = u32::from_ne_bytes(length) as usize;
        if length > 1024 * 1024 {
            let _ = tx.send(Err("Native stdout was not a bounded framed message".into()));
            break;
        }
        let mut data = vec![0u8; length];
        if stdout.read_exact(&mut data).is_err() {
            break;
        }
        let value = serde_json::from_slice(&data).map_err(|_| "Invalid native JSON frame".into());
        if tx.send(value).is_err() {
            break;
        }
    });

    send(
        &mut host,
        json!({"method": "agentPanel", "requestId": "discovery", "operation": "discover", "params": {}}),
    );
    send(&mut host, json!({"method": "ping"}));
    send(
        &mut host,
        json!({"method": "agentPanel", "requestId": "unknown", "operation": "run-arbitrary", "params": {}}),
    );
    send(
        &mut host,
        json!({"method": "agentPanel", "requestId": "malformed", "operation": "discover", "params": {"arbitrary": "value"}}),
    );

    let mut discovery = false;
    let mut pong = false;
    let mut unknown = false;
    let mut malformed = false;
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    while !(discovery && pong && unknown && malformed) {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        let frame = rx
            .recv_timeout(remaining)
            .expect("Native host did not answer before the deadline")
            .unwrap();
        match frame["method"].as_str() {
            Some("pong") => pong = true,
            Some("agentPanelReply") => match frame["requestId"].as_str() {
                Some("discovery") => {
                    assert!(frame.get("error").is_none());
                    assert_eq!(frame["result"]["schemaVersion"], 1);
                    let agents = frame["result"]["agents"].as_array().unwrap();
                    assert!(agents.iter().any(|a| a["id"] == "codex"));
                    assert!(agents.iter().any(|a| a["id"] == "claude"));
                    for agent in agents {
                        assert_eq!(agent["authentication"], "unverified");
                        assert_eq!(agent["runtime"], "unverified");
                        assert!(agent.get("credentials").is_none());
                        assert!(agent.get("token").is_none());
                    }
                    discovery = true;
                }
                Some("unknown") => {
                    assert!(frame["error"].as_str().unwrap().contains("Unsupported"));
                    assert!(frame.get("result").is_none());
                    unknown = true;
                }
                Some("malformed") => {
                    assert!(frame["error"].as_str().unwrap().contains("empty params"));
                    assert!(frame.get("result").is_none());
                    malformed = true;
                }
                _ => panic!("Reply has an unknown request identity"),
            },
            Some("ping") => send(&mut host, json!({"method": "pong"})),
            _ => panic!("Panel work entered the browser dispatcher or polluted protocol stdout"),
        }
    }
    drop(host.0.stdin.take());
    let exit_deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = host.0.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        assert!(
            std::time::Instant::now() < exit_deadline,
            "Native host did not exit after EOF"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[cfg(any(unix, windows))]
#[test]
fn upgrade_refuses_busy_old_service_and_replaces_it_only_after_owned_idle_shutdown() {
    let _case = native_case();
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use std::sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    };
    let registry = tempfile::tempdir().unwrap();
    let profile = "upgrade-fixture";
    let directory = registry.path().join("agent-panel/profiles").join(profile);
    std::fs::create_dir_all(&directory).unwrap();
    #[cfg(unix)]
    std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let ownership = std::fs::File::create(directory.join("service.lock")).unwrap();
    ownership.lock().unwrap();
    let endpoint_path = directory.join("service.json");
    std::fs::write(&endpoint_path, json!({"schemaVersion":1,"profile":profile,"address":listener.local_addr().unwrap(),"capability":"f".repeat(64),"binaryHash":"synthetic-old-build","pid":std::process::id()}).to_string()).unwrap();
    #[cfg(unix)]
    std::fs::set_permissions(&endpoint_path, std::fs::Permissions::from_mode(0o600)).unwrap();
    #[cfg(windows)]
    protect_fixture_endpoint(&endpoint_path);
    let active = Arc::new(AtomicU64::new(1));
    let active_for_server = active.clone();
    let server = std::thread::spawn(move || {
        for incoming in listener.incoming() {
            let mut stream = incoming.unwrap();
            let mut length = [0u8; 4];
            stream.read_exact(&mut length).unwrap();
            let mut bytes = vec![0; u32::from_le_bytes(length) as usize];
            stream.read_exact(&mut bytes).unwrap();
            let request: Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(request["capability"], "f".repeat(64));
            let stop = request["operation"] == "serviceStop";
            let result = if stop {
                assert_eq!(active_for_server.load(Ordering::SeqCst), 0);
                json!({"schemaVersion":1,"service":{"stopRequested":true}})
            } else if request["operation"] == "conversationList" {
                json!({"schemaVersion":1,"conversations":[{"id":"12b6a0cd-eca8-4462-a809-16db0688704c","title":"Waiting for approval","phase":"waitingInput"}]})
            } else {
                assert_eq!(request["operation"], "health");
                json!({"schemaVersion":1,"binaryHash":"synthetic-old-build","active":active_for_server.load(Ordering::SeqCst)})
            };
            let reply = json!({"requestId":request["requestId"],"result":result})
                .to_string()
                .into_bytes();
            stream
                .write_all(&(reply.len() as u32).to_le_bytes())
                .unwrap();
            stream.write_all(&reply).unwrap();
            if stop {
                std::fs::remove_file(endpoint_path).unwrap();
                drop(ownership);
                break;
            }
        }
    });
    let mut host = Host(
        Command::new(test_binary())
            .args(["__nm-host", "chrome-extension://agent-panel-fixture/"])
            .env("CHROME_USE_RELAY_DIR", registry.path())
            .env("CHROME_USE_NO_UPDATE_CHECK", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let rx = frames(&mut host);
    send(
        &mut host,
        json!({"method":"hello","version":"fixture","profileId":profile}),
    );
    let refused = panel_reply(&mut host, &rx, "busy-upgrade", "serviceHealth", json!({}));
    assert!(refused["error"]
        .as_str()
        .unwrap()
        .contains("stop its conversations"));
    assert!(refused["error"]
        .as_str()
        .unwrap()
        .ends_with("[blocking:12b6a0cd-eca8-4462-a809-16db0688704c]"));
    active.store(0, Ordering::SeqCst);
    let upgraded = panel_reply(&mut host, &rx, "idle-upgrade", "serviceHealth", json!({}));
    assert!(upgraded.get("error").is_none(), "{}", upgraded["error"]);
    assert_ne!(upgraded["result"]["binaryHash"], "synthetic-old-build");
    server.join().unwrap();
    let stop = panel_reply(&mut host, &rx, "stop-new-service", "serviceStop", json!({}));
    assert!(stop.get("error").is_none());
    for _ in 0..100 {
        if !directory.join("service.json").exists() {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("Owned upgraded service exit was not confirmed");
}

#[test]
#[cfg(any(unix, windows))]
fn deleting_a_conversation_stops_its_agent_and_leaves_siblings_intact() {
    let _case = native_case();
    let registry = tempfile::tempdir().unwrap();
    let profile = "native-delete-fixture";
    let codex_home = tempfile::tempdir().unwrap();
    let mut cleanup = ServiceFixture {
        root: registry.path().into(),
        profile: profile.into(),
        chats: vec![],
    };
    let mut host = Host(
        Command::new(test_binary())
            .args(["__nm-host", "chrome-extension://agent-panel-fixture/"])
            .env("CHROME_USE_RELAY_DIR", registry.path())
            .env("CHROME_USE_NO_UPDATE_CHECK", "1")
            .env("CODEX_HOME", codex_home.path())
            .env("PANEL_FIXTURE_UUID_SESSION", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let rx = frames(&mut host);
    send(
        &mut host,
        json!({"method":"hello","version":"fixture","profileId":profile}),
    );
    let fixture = fixture_program(registry.path());
    let mut ids = vec![];
    for _ in 0..2 {
        let created = panel_reply(
            &mut host,
            &rx,
            "create",
            "conversationCreate",
            json!({"agentId":"codex","executable":fixture}),
        );
        let id = created["result"]["conversation"]["id"]
            .as_str()
            .unwrap()
            .to_string();
        cleanup.chats.push(id.clone());
        let sent = panel_reply(
            &mut host,
            &rx,
            "send",
            "conversationSend",
            json!({"conversationId":id,"messageId":uuid::Uuid::new_v4().to_string(),"text":"first"}),
        );
        assert!(sent.get("error").is_none());
        conversation_poll(&mut host, &rx, &id, "idle");
        ids.push(id);
    }
    // The service the host started left no launchd job loaded behind it.
    #[cfg(target_os = "macos")]
    assert_eq!(
        loaded_service_jobs(registry.path()),
        Vec::<String>::new(),
        "a launchd job for this data directory stayed loaded"
    );
    let folder = registry
        .path()
        .join(format!("agent-panel/profiles/{profile}/conversations"));
    assert!(folder.join(&ids[0]).is_dir());
    let session = |id: &str| -> String {
        let state: Value =
            serde_json::from_slice(&std::fs::read(folder.join(id).join("state.json")).unwrap())
                .unwrap();
        state["sessionId"].as_str().unwrap().to_string()
    };
    let (own, sibling) = (session(&ids[0]), session(&ids[1]));
    assert_ne!(own, sibling);
    let day = codex_home.path().join("sessions/2026/10/13");
    let archived = codex_home.path().join("archived_sessions");
    std::fs::create_dir_all(&day).unwrap();
    std::fs::create_dir_all(&archived).unwrap();
    let decoy = uuid::Uuid::new_v4();
    let own_files = [
        day.join(format!("rollout-a-{own}.jsonl")),
        archived.join(format!("rollout-b-{own}.jsonl")),
    ];
    let kept_files = [
        day.join(format!("rollout-c-{sibling}.jsonl")),
        day.join(format!("rollout-d-{decoy}.jsonl")),
        day.join(format!("notes-{own}.jsonl")),
    ];
    for path in own_files.iter().chain(kept_files.iter()) {
        std::fs::write(path, "{}").unwrap();
    }
    let deleted = panel_reply(
        &mut host,
        &rx,
        "delete",
        "conversationDelete",
        json!({"conversationId":ids[0]}),
    );
    assert!(
        deleted.get("error").is_none(),
        "Delete failed: {}",
        deleted["error"]
    );
    assert_eq!(deleted["result"]["deleted"]["id"], ids[0]);
    let history = &deleted["result"]["deleted"]["cliHistory"];
    assert_eq!(history["sessionId"], own);
    assert_eq!(history["removed"].as_array().unwrap().len(), 2);
    assert!(history["failed"].as_array().unwrap().is_empty());
    assert!(own_files.iter().all(|p| !p.exists()));
    assert!(kept_files.iter().all(|p| p.exists()));
    assert_eq!(
        deleted["result"]["deleted"]["browserSession"]["session"],
        format!("panel-{}", ids[0])
    );
    assert!(!folder.join(&ids[0]).exists());
    let listed = panel_reply(&mut host, &rx, "list", "conversationList", json!({}));
    let listed: Vec<_> = listed["result"]["conversations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(listed, vec![ids[1].clone()]);
    let gone = panel_reply(
        &mut host,
        &rx,
        "read-deleted",
        "conversationRead",
        json!({"conversationId":ids[0],"after":0}),
    );
    assert!(gone["error"].as_str().unwrap().contains("not found"));
    let again = panel_reply(
        &mut host,
        &rx,
        "delete-again",
        "conversationDelete",
        json!({"conversationId":ids[0]}),
    );
    assert!(again.get("error").is_some());
    // The sibling keeps its history and its live agent.
    let sent = panel_reply(
        &mut host,
        &rx,
        "send-sibling",
        "conversationSend",
        json!({"conversationId":ids[1],"messageId":uuid::Uuid::new_v4().to_string(),"text":"second"}),
    );
    assert!(sent.get("error").is_none());
    let sibling = conversation_poll(&mut host, &rx, &ids[1], "idle");
    assert!(sibling["journal"]["events"].as_array().unwrap().len() > 2);
}

#[test]
#[cfg(any(unix, windows))]
fn full_access_conversations_run_without_prompts_and_can_be_switched_mid_request() {
    let _case = native_case();
    let registry = tempfile::tempdir().unwrap();
    let profile = "native-full-access-fixture";
    let mut cleanup = ServiceFixture {
        root: registry.path().into(),
        profile: profile.into(),
        chats: vec![],
    };
    let mut host = Host(
        Command::new(test_binary())
            .args(["__nm-host", "chrome-extension://agent-panel-fixture/"])
            .env("CHROME_USE_RELAY_DIR", registry.path())
            .env("CHROME_USE_NO_UPDATE_CHECK", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let rx = frames(&mut host);
    send(
        &mut host,
        json!({"method":"hello","version":"fixture","profileId":profile}),
    );
    let fixture = fixture_program(registry.path());
    let turn = |host: &mut Host, id: &str, text: &str, phase: &str| -> Value {
        let sent = panel_reply(
            host,
            &rx,
            "send",
            "conversationSend",
            json!({"conversationId":id,"messageId":uuid::Uuid::new_v4().to_string(),"text":text}),
        );
        assert!(
            sent.get("error").is_none(),
            "send failed: {}",
            sent["error"]
        );
        conversation_poll(host, &rx, id, phase)
    };
    let replies = |result: &Value| -> String {
        result["journal"]["events"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| e["kind"] == "textDelta" || e["kind"] == "textFinal")
            .map(|e| e["data"]["text"].as_str().unwrap_or(""))
            .collect()
    };
    let created = panel_reply(
        &mut host,
        &rx,
        "create-full",
        "conversationCreate",
        json!({"agentId":"codex","executable":fixture,"fullAccess":true}),
    );
    assert_eq!(created["result"]["conversation"]["fullAccess"], true);
    let full = created["result"]["conversation"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    cleanup.chats.push(full.clone());
    let done = turn(&mut host, &full, "approval", "idle");
    assert!(replies(&done).contains("synthetic approval handled"));
    let sent = panel_reply(
        &mut host,
        &rx,
        "send-page",
        "conversationSend",
        json!({"conversationId":full,"messageId":uuid::Uuid::new_v4().to_string(),"text":"echo-input","page":{"tabId":42,"url":"https://example.test/post","title":"A post"}}),
    );
    assert!(
        sent.get("error").is_none(),
        "send failed: {}",
        sent["error"]
    );
    let done = conversation_poll(&mut host, &rx, &full, "idle");
    let echoed = replies(&done);
    assert!(echoed.contains("tabId \"chrome-tab:42\""), "{echoed}");
    assert!(echoed.contains("https://example.test/post"));
    let user = done["journal"]["events"]
        .as_array()
        .unwrap()
        .iter()
        .rev()
        .find(|e| e["kind"] == "user")
        .unwrap()
        .clone();
    assert_eq!(user["data"]["text"], "echo-input");
    assert_eq!(user["data"]["page"]["tabId"], 42);
    assert_eq!(done["conversation"]["site"], "example.test");
    let bad = panel_reply(
        &mut host,
        &rx,
        "send-bad-page",
        "conversationSend",
        json!({"conversationId":full,"messageId":uuid::Uuid::new_v4().to_string(),"text":"x","page":{"tabId":-1,"url":"u","title":"t"}}),
    );
    assert!(bad.get("error").is_some());
    let done = turn(&mut host, &full, "policy", "idle");
    assert!(replies(&done).contains("policy: never/danger-full-access"));

    let created = panel_reply(
        &mut host,
        &rx,
        "create-ask",
        "conversationCreate",
        json!({"agentId":"codex","executable":fixture}),
    );
    assert_eq!(created["result"]["conversation"]["fullAccess"], false);
    let ask = created["result"]["conversation"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    cleanup.chats.push(ask.clone());
    let done = turn(&mut host, &ask, "policy", "idle");
    assert!(replies(&done).contains("policy: on-request/read-only"));
    turn(&mut host, &ask, "approval", "waitingInput");
    let switched = panel_reply(
        &mut host,
        &rx,
        "set-access",
        "conversationSetAccess",
        json!({"conversationId":ask,"fullAccess":true}),
    );
    assert_eq!(switched["result"]["conversation"]["fullAccess"], true);
    let done = conversation_poll(&mut host, &rx, &ask, "idle");
    assert!(replies(&done).contains("synthetic approval handled"));
    // The next turn restarts the idle agent with full-access flags.
    let done = turn(&mut host, &ask, "policy", "idle");
    assert!(replies(&done).ends_with("policy: never/danger-full-access"));
}

#[cfg(any(unix, windows))]
#[test]
fn opencode_acp_streams_tools_approvals_stop_resume_queue_and_delete() {
    let _case = native_case();
    let registry = tempfile::tempdir().unwrap();
    let profile = "native-opencode-fixture";
    let mut cleanup = ServiceFixture {
        root: registry.path().into(),
        profile: profile.into(),
        chats: vec![],
    };
    let mut host = Host(
        Command::new(test_binary())
            .args(["__nm-host", "chrome-extension://agent-panel-fixture/"])
            .env("CHROME_USE_RELAY_DIR", registry.path())
            .env("CHROME_USE_NO_UPDATE_CHECK", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let rx = frames(&mut host);
    send(
        &mut host,
        json!({"method":"hello","version":"fixture","profileId":profile}),
    );
    let fixture = fixture_program(registry.path());
    let inspection = panel_reply(
        &mut host,
        &rx,
        "inspect",
        "agentInspect",
        json!({"agentId":"opencode","executable":fixture}),
    );
    assert!(inspection.get("error").is_none(), "{inspection}");
    assert_eq!(
        inspection["result"]["inspection"]["models"][0]["id"],
        "synthetic/opencode-model"
    );
    assert_eq!(
        inspection["result"]["inspection"]["processExitConfirmed"],
        true
    );
    let created = panel_reply(
        &mut host,
        &rx,
        "create",
        "conversationCreate",
        json!({"agentId":"opencode","executable":fixture,"model":"synthetic/opencode-model"}),
    );
    assert!(created.get("error").is_none(), "{created}");
    let id = created["result"]["conversation"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    cleanup.chats.push(id.clone());
    let send_message = |host: &mut Host, text: &str| {
        let mut params =
            json!({"conversationId":id,"messageId":uuid::Uuid::new_v4().to_string(),"text":text});
        if text == "hello" {
            params["page"] = json!({"tabId":74,"title":"Synthetic ACP page","url":"https://fixture.invalid/page"});
        }
        let reply = panel_reply(host, &rx, "send", "conversationSend", params);
        assert!(reply.get("error").is_none(), "{reply}");
    };
    send_message(&mut host, "hello");
    let completed = conversation_poll(&mut host, &rx, &id, "idle");
    let events = completed["journal"]["events"].as_array().unwrap();
    assert!(events
        .iter()
        .any(|event| event["kind"] == "textDelta"
            && event["data"]["text"] == "synthetic reply: hello"));
    assert!(events
        .iter()
        .any(|event| event["kind"] == "tool" && event["data"]["completed"] == true));
    let tool = &events
        .iter()
        .rev()
        .find(|event| event["kind"] == "tool")
        .unwrap()["data"]["item"];
    assert_eq!(tool["name"], "Synthetic tool");
    assert_eq!(tool["input"]["command"], "synthetic");
    assert_eq!(
        completed["conversation"]["model"],
        "synthetic/opencode-model"
    );
    send_message(&mut host, "approval");
    let waiting = conversation_poll(&mut host, &rx, &id, "waitingInput");
    let request = &waiting["journal"]["events"]
        .as_array()
        .unwrap()
        .iter()
        .rev()
        .find(|event| event["kind"] == "request")
        .unwrap()["data"];
    assert_eq!(request["method"], "session/request_permission");
    let bad = panel_reply(
        &mut host,
        &rx,
        "bad-option",
        "conversationRespond",
        json!({"conversationId":id,"requestId":request["requestId"],"result":{"outcome":{"outcome":"selected","optionId":"unknown"}}}),
    );
    assert!(bad.get("error").is_some());
    let approved = panel_reply(
        &mut host,
        &rx,
        "approve",
        "conversationRespond",
        json!({"conversationId":id,"requestId":request["requestId"],"result":{"outcome":{"outcome":"selected","optionId":"once"}}}),
    );
    assert!(approved.get("error").is_none(), "{approved}");
    conversation_poll(&mut host, &rx, &id, "idle");
    // Switching access restarts an idle process. The restored ACP session must
    // discard pre-response replay and retain only the panel's durable history.
    let access = panel_reply(
        &mut host,
        &rx,
        "access",
        "conversationSetAccess",
        json!({"conversationId":id,"fullAccess":true}),
    );
    assert!(access.get("error").is_none());
    send_message(&mut host, "approval");
    let automatic = conversation_poll(&mut host, &rx, &id, "idle");
    assert!(automatic["journal"]["events"]
        .as_array()
        .unwrap()
        .iter()
        .any(|event| event["kind"] == "replySent"));
    assert!(!automatic.to_string().contains("REPLAY MUST NOT APPEAR"));
    assert!(automatic["conversation"]["generation"].as_u64().unwrap() >= 2);
    send_message(&mut host, "wait");
    conversation_poll(&mut host, &rx, &id, "running");
    send_message(&mut host, "queued");
    let stop = panel_reply(
        &mut host,
        &rx,
        "stop",
        "conversationStop",
        json!({"conversationId":id}),
    );
    assert!(stop.get("error").is_none());
    let stopped = conversation_poll(&mut host, &rx, &id, "interrupted");
    assert_eq!(stopped["conversation"]["queued"], 1);
    let resume = panel_reply(
        &mut host,
        &rx,
        "queue",
        "conversationResumeQueue",
        json!({"conversationId":id}),
    );
    assert!(resume.get("error").is_none());
    let done = conversation_poll(&mut host, &rx, &id, "idle");
    assert!(done.to_string().contains("synthetic reply: queued"));
    assert_eq!(done["conversation"]["queued"], 0);
    let access = panel_reply(
        &mut host,
        &rx,
        "ask",
        "conversationSetAccess",
        json!({"conversationId":id,"fullAccess":false}),
    );
    assert!(access.get("error").is_none());
    send_message(&mut host, "approval");
    conversation_poll(&mut host, &rx, &id, "waitingInput");
    let stop = panel_reply(
        &mut host,
        &rx,
        "cancel-permission",
        "conversationStop",
        json!({"conversationId":id}),
    );
    assert!(stop.get("error").is_none());
    conversation_poll(&mut host, &rx, &id, "interrupted");
    let resume = panel_reply(
        &mut host,
        &rx,
        "unpause",
        "conversationResumeQueue",
        json!({"conversationId":id}),
    );
    assert!(resume.get("error").is_none());
    send_message(&mut host, "fail");
    conversation_poll(&mut host, &rx, &id, "failed");
    let resume = panel_reply(
        &mut host,
        &rx,
        "unpause-after-fail",
        "conversationResumeQueue",
        json!({"conversationId":id}),
    );
    assert!(resume.get("error").is_none(), "{resume}");
    send_message(&mut host, "refuse");
    let refused = conversation_poll(&mut host, &rx, &id, "failed");
    assert!(
        refused["conversation"]["lastError"]
            .as_str()
            .unwrap_or("")
            .contains("Model opencode/exo-free is no longer available"),
        "{refused}"
    );
    let deleted = panel_reply(
        &mut host,
        &rx,
        "delete",
        "conversationDelete",
        json!({"conversationId":id}),
    );
    assert!(deleted.get("error").is_none(), "{deleted}");
    let history = &deleted["result"]["deleted"]["cliHistory"];
    assert!(history["error"].is_null(), "{deleted}");
    assert_eq!(history["removed"].as_array().unwrap().len(), 1);
    assert!(history["removed"][0]
        .as_str()
        .unwrap()
        .starts_with("opencode session "));
    let proof = std::fs::read_to_string(registry.path().join("acp-deletions.log")).unwrap();
    assert!(
        proof.lines().count() >= 2,
        "Metadata and conversation sessions must both be deleted"
    );
    assert!(!registry
        .path()
        .join(format!("agent-panel/profiles/{profile}/conversations/{id}"))
        .exists());
}

#[test]
#[cfg(any(unix, windows))]
fn pi_conversations_use_rpc_mode_browser_only_tools_and_their_own_session_id() {
    let _case = native_case();
    let registry = tempfile::tempdir().unwrap();
    let profile = "native-pi-fixture";
    let mut cleanup = ServiceFixture {
        root: registry.path().into(),
        profile: profile.into(),
        chats: vec![],
    };
    let mut host = Host(
        Command::new(test_binary())
            .args(["__nm-host", "chrome-extension://agent-panel-fixture/"])
            .env("CHROME_USE_RELAY_DIR", registry.path())
            .env("CHROME_USE_NO_UPDATE_CHECK", "1")
            .env("PI_CODING_AGENT_DIR", registry.path().join("pi-home"))
            .env("PANEL_FIXTURE_PI_EXTRA_MODELS", "300")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let rx = frames(&mut host);
    send(
        &mut host,
        json!({"method":"hello","version":"fixture","profileId":profile}),
    );
    let fixture = fixture_program(registry.path());
    let inspected = panel_reply(
        &mut host,
        &rx,
        "inspect-pi",
        "agentInspect",
        json!({"agentId":"pi","executable":fixture}),
    );
    assert!(inspected.get("error").is_none(), "{}", inspected["error"]);
    assert_eq!(
        inspected["result"]["inspection"]["models"][0]["id"],
        "synthetic/pi-model"
    );
    // Pi lists every configured model; the menu keeps 256 and says so.
    assert_eq!(
        inspected["result"]["inspection"]["models"]
            .as_array()
            .unwrap()
            .len(),
        256
    );
    assert_eq!(inspected["result"]["inspection"]["complete"], false);
    assert_eq!(
        inspected["result"]["inspection"]["models"][1]["id"],
        "openrouter/~anthropic/claude-opus-latest"
    );
    let replies = |result: &Value| -> String {
        result["journal"]["events"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| e["kind"] == "textFinal")
            .map(|e| e["data"]["text"].as_str().unwrap_or(""))
            .collect()
    };
    let turn = |host: &mut Host, id: &str, text: &str| {
        let sent = panel_reply(
            host,
            &rx,
            "send",
            "conversationSend",
            json!({"conversationId":id,"messageId":uuid::Uuid::new_v4().to_string(),"text":text}),
        );
        assert!(
            sent.get("error").is_none(),
            "send failed: {}",
            sent["error"]
        );
    };
    let mut create = |host: &mut Host, full: bool| -> String {
        let created = panel_reply(
            host,
            &rx,
            "create-pi",
            "conversationCreate",
            json!({"agentId":"pi","executable":fixture,"fullAccess":full}),
        );
        assert!(created.get("error").is_none(), "{}", created["error"]);
        let id = created["result"]["conversation"]["id"]
            .as_str()
            .unwrap()
            .to_string();
        cleanup.chats.push(id.clone());
        id
    };
    let ask = create(&mut host, false);
    turn(&mut host, &ask, "policy");
    let done = conversation_poll(&mut host, &rx, &ask, "idle");
    let text = replies(&done);
    assert!(text.contains("tools: mcp__chrome_use__*"), "{text}");
    assert!(text.contains(&format!("session: {ask}")), "{text}");
    assert!(done["journal"]["events"]
        .as_array()
        .unwrap()
        .iter()
        .any(|e| e["kind"] == "tool" && e["data"]["completed"] == true));
    turn(&mut host, &ask, "fail");
    let failed = conversation_poll(&mut host, &rx, &ask, "failed");
    assert!(failed["conversation"]["lastError"]
        .as_str()
        .unwrap_or("")
        .contains("synthetic pi failure"));
    let full = create(&mut host, true);
    turn(&mut host, &full, "policy");
    let done = conversation_poll(&mut host, &rx, &full, "idle");
    assert!(replies(&done).contains("tools: all"));
    turn(&mut host, &full, "wait");
    conversation_poll(&mut host, &rx, &full, "running");
    let stopped = panel_reply(
        &mut host,
        &rx,
        "stop",
        "conversationStop",
        json!({"conversationId":full}),
    );
    assert!(stopped.get("error").is_none(), "{}", stopped["error"]);
    conversation_poll(&mut host, &rx, &full, "interrupted");
    let deleted = panel_reply(
        &mut host,
        &rx,
        "delete-pi",
        "conversationDelete",
        json!({"conversationId":full}),
    );
    assert!(deleted.get("error").is_none(), "{}", deleted["error"]);
    assert!(deleted["result"]["deleted"]["cliHistory"]["error"].is_null());
}

#[test]
#[cfg(any(unix, windows))]
fn a_slow_cli_model_check_is_answered_instead_of_dropping_the_connection() {
    let _case = native_case();
    let registry = tempfile::tempdir().unwrap();
    let profile = "native-slow-inspection-fixture";
    let _cleanup = ServiceFixture {
        root: registry.path().into(),
        profile: profile.into(),
        chats: vec![],
    };
    let mut host = Host(
        Command::new(test_binary())
            .args(["__nm-host", "chrome-extension://agent-panel-fixture/"])
            .env("CHROME_USE_RELAY_DIR", registry.path())
            .env("CHROME_USE_NO_UPDATE_CHECK", "1")
            .env("PANEL_FIXTURE_SLOW_ACP", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let rx = frames(&mut host);
    send(
        &mut host,
        json!({"method":"hello","version":"fixture","profileId":profile}),
    );
    let fixture = fixture_program(registry.path());
    let inspected = panel_reply(
        &mut host,
        &rx,
        "inspect-slow",
        "agentInspect",
        json!({"agentId":"opencode","executable":fixture}),
    );
    assert!(
        inspected.get("error").is_none(),
        "slow inspection failed: {}",
        inspected["error"]
    );
    assert!(!inspected["result"]["inspection"]["models"]
        .as_array()
        .unwrap()
        .is_empty());
}

#[test]
#[cfg(any(unix, windows))]
fn panel_rich_messages_options_and_schedules_reach_the_real_native_service() {
    let _case = native_case();
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    #[cfg(unix)]
    use std::os::unix::fs::symlink;
    let registry = tempfile::tempdir().unwrap();
    let profile = "native-rich-panel-fixture";
    let mut cleanup = ServiceFixture {
        root: registry.path().into(),
        profile: profile.into(),
        chats: vec![],
    };
    let home = registry.path().join("home");
    let bin = home.join(".local/bin");
    std::fs::create_dir_all(&bin).unwrap();
    let fixture = fixture_program(registry.path());
    #[cfg(unix)]
    symlink(&fixture, bin.join("codex")).unwrap();
    #[cfg(windows)]
    std::fs::copy(&fixture, bin.join("codex.cmd")).unwrap();
    let proof = registry.path().join("protocol.jsonl");
    let spawn = || {
        Host(
            Command::new(test_binary())
                .args(["__nm-host", "chrome-extension://agent-panel-fixture/"])
                .env("CHROME_USE_RELAY_DIR", registry.path())
                .env("CHROME_USE_NO_UPDATE_CHECK", "1")
                .env("HOME", &home)
                .env("USERPROFILE", &home)
                .env("CODEX_HOME", home.join(".codex"))
                .env("CLAUDE_CONFIG_DIR", home.join(".claude"))
                .env(
                    "PATH",
                    std::env::join_paths(std::iter::once(bin.clone()).chain(
                        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
                    ))
                    .unwrap(),
                )
                .env("PANEL_FIXTURE_CAPTURE", &proof)
                .env("PANEL_FIXTURE_IMAGES", "1")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        )
    };
    let mut host = spawn();
    let rx = frames(&mut host);
    send(
        &mut host,
        json!({"method":"hello","version":"fixture","profileId":profile}),
    );
    let call = |host: &mut Host, operation: &str, params: Value| -> Value {
        let reply = panel_reply(host, &rx, operation, operation, params);
        assert!(
            reply.get("error").is_none(),
            "{operation}: {}",
            reply["error"]
        );
        reply["result"].clone()
    };
    let text="---\nname: synthetic-skill\ndescription: Isolated test skill\n---\nUse the browser tools to read the fixture. Do not run commands.";
    let catalog = call(
        &mut host,
        "skillSave",
        json!({"agentId":"codex","name":"synthetic-skill","content":text}),
    );
    let skill_id = catalog["skills"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == "synthetic-skill")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let mut png = std::io::Cursor::new(Vec::new());
    image::DynamicImage::new_rgb8(2, 2)
        .write_to(&mut png, image::ImageFormat::Png)
        .unwrap();
    for agent in ["codex", "claude", "opencode", "pi"] {
        let created = call(
            &mut host,
            "conversationCreate",
            json!({"agentId":agent,"executable":fixture,"fullAccess":true,"options":{"effort":if ["codex","claude"].contains(&agent){Some("high")}else{None},"toolAccess":if agent=="opencode"{"runCommands"}else{"writeFiles"}}}),
        );
        let id = created["conversation"]["id"].as_str().unwrap().to_string();
        cleanup.chats.push(id.clone());
        let snapshot = call(
            &mut host,
            "conversationSkillSelect",
            json!({"conversationId":id,"skillId":skill_id}),
        );
        let attachment = call(
            &mut host,
            "conversationAttachmentPut",
            json!({"conversationId":id,"data":STANDARD.encode(png.get_ref())}),
        )["attachment"]
            .clone();
        let read = call(
            &mut host,
            "conversationAttachmentRead",
            json!({"conversationId":id,"attachment":attachment}),
        );
        assert_eq!(read["data"], STANDARD.encode(png.get_ref()));
        let message_id = uuid::Uuid::new_v4().to_string();
        let message = json!({"conversationId":id,"messageId":message_id,"text":"rich-input","skillId":snapshot["skill"]["id"],"attachments":[attachment]});
        call(&mut host, "conversationSend", message.clone());
        conversation_poll(&mut host, &rx, &id, "idle");
        // Changing the library cannot change an already selected skill snapshot.
        call(
            &mut host,
            "skillSave",
            json!({"agentId":agent,"name":"synthetic-skill","content":text.replace("read the fixture","read a different fixture")}),
        );
        assert_eq!(
            call(&mut host, "conversationSend", message.clone())["message"]["accepted"],
            false
        );
        let mut forged = message;
        forged["attachments"] = json!([]);
        assert!(panel_reply(
            &mut host,
            &rx,
            "changed-rich-input",
            "conversationSend",
            forged
        )
        .get("error")
        .is_some());
        call(
            &mut host,
            "conversationSetOptions",
            json!({"conversationId":id,"options":{"effort":if ["codex","claude"].contains(&agent){Some("low")}else{None},"toolAccess":if agent=="opencode"{"runCommands"}else{"readFiles"}}}),
        );
        call(
            &mut host,
            "conversationSend",
            json!({"conversationId":id,"messageId":uuid::Uuid::new_v4().to_string(),"text":"policy"}),
        );
        conversation_poll(&mut host, &rx, &id, "idle");
        if agent == "codex" {
            call(
                &mut host,
                "conversationSetOptions",
                json!({"conversationId":id,"options":{"toolAccess":"readFiles"}}),
            );
            call(
                &mut host,
                "conversationSend",
                json!({"conversationId":id,"messageId":uuid::Uuid::new_v4().to_string(),"text":"default-options"}),
            );
            conversation_poll(&mut host, &rx, &id, "idle");
        }
    }
    let records: Vec<Value> = std::fs::read_to_string(&proof)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let starts: Vec<_> = records
        .iter()
        .filter(|r| {
            ["thread/start", "thread/resume"]
                .contains(&r["message"]["method"].as_str().unwrap_or(""))
        })
        .collect();
    assert_eq!(starts.len(), 3);
    assert_ne!(starts[0]["pid"], starts[1]["pid"]);
    assert_eq!(
        starts[0]["message"]["params"]["config"]["model_reasoning_effort"],
        "high"
    );
    assert_eq!(starts[0]["message"]["params"]["sandbox"], "workspace-write");
    assert_eq!(
        starts[1]["message"]["params"]["config"]["model_reasoning_effort"],
        "low"
    );
    assert_eq!(starts[1]["message"]["params"]["sandbox"], "read-only");
    assert_eq!(
        starts[2]["message"]["params"]["config"]["model_reasoning_effort"],
        "medium"
    );
    let codex_rich = records
        .iter()
        .find(|r| {
            r["message"]["method"] == "turn/start"
                && r["message"]["params"]["input"]
                    .as_array()
                    .is_some_and(|a| a.len() == 2)
        })
        .unwrap();
    assert_eq!(
        codex_rich["message"]["params"]["input"][1]["type"],
        "localImage"
    );
    assert!(codex_rich["message"]["params"]["input"][0]["text"]
        .as_str()
        .unwrap()
        .contains("explicitly selected the skill synthetic-skill"));
    let claude_rich = records
        .iter()
        .find(|r| r["message"]["type"] == "user" && r["message"]["message"]["content"].is_array())
        .unwrap();
    assert_eq!(
        claude_rich["message"]["message"]["content"][1]["source"]["media_type"],
        "image/png"
    );
    assert_eq!(
        claude_rich["message"]["message"]["content"][1]["source"]["data"],
        STANDARD.encode(png.get_ref())
    );
    assert!(records.iter().any(|r| r["argv"]
        .as_array()
        .unwrap()
        .windows(2)
        .any(|args| args == [json!("--effort"), json!("low")])));
    let pi_rich = records
        .iter()
        .find(|r| {
            r["message"]["type"] == "prompt"
                && r["message"]["images"]
                    .as_array()
                    .is_some_and(|a| a.len() == 1)
        })
        .unwrap();
    assert_eq!(pi_rich["message"]["images"][0]["mimeType"], "image/png");
    let acp_rich = records
        .iter()
        .find(|r| {
            r["message"]["method"] == "session/prompt"
                && r["message"]["params"]["prompt"]
                    .as_array()
                    .is_some_and(|a| a.len() == 2)
        })
        .unwrap();
    assert_eq!(acp_rich["message"]["params"]["prompt"][1]["type"], "image");
    assert_eq!(
        acp_rich["message"]["params"]["prompt"][1]["mimeType"],
        "image/png"
    );
    let saved = call(
        &mut host,
        "scheduleSave",
        json!({"name":"Synthetic schedule","prompt":"scheduled fixture","agentId":"codex","rule":{"kind":"interval","minutes":10},"enabled":false,"fullAccess":false,"options":{"toolAccess":"readFiles"}}),
    );
    let schedule_id = saved["schedule"]["id"].as_str().unwrap().to_string();
    let next_run = saved["schedule"]["nextRun"].clone();
    let run = call(&mut host, "scheduleRun", json!({"id":schedule_id}));
    assert_eq!(run["run"]["completed"], false);
    let run_id = run["run"]["conversationId"].as_str().unwrap().to_string();
    cleanup.chats.push(run_id.clone());
    let done = conversation_poll(&mut host, &rx, &run_id, "idle");
    assert_eq!(done["conversation"]["scheduleId"], schedule_id);
    for action in [
        "agentScheduleSave",
        "agentScheduleRun",
        "agentScheduleDelete",
    ] {
        let reply = panel_reply(
            &mut host,
            &rx,
            "recursive",
            action,
            json!({"callerConversation":run_id,"id":schedule_id}),
        );
        assert!(reply["error"]
            .as_str()
            .unwrap()
            .contains("Scheduled conversations"));
    }
    let mut result = Value::Null;
    for _ in 0..80 {
        result = call(&mut host, "scheduleList", json!({}));
        if result["schedules"][0]["lastResult"] == "completed" {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(result["schedules"][0]["lastResult"], "completed");
    assert_eq!(result["schedules"][0]["nextRun"], next_run);
    call(&mut host, "scheduleDelete", json!({"id":schedule_id}));
}

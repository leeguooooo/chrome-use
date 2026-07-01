//! `chrome-use test <suite.yaml>` — a tiny, re-runnable browser test runner.
//!
//! Turns repetitive browser checks into unit-test-style suites for the frontend.
//! A suite is a YAML file of cases; each case is a list of `steps` (which reuse
//! chrome-use's own commands) followed by `assert`s (which compile to a single
//! `eval` expression read back as a boolean). The runner drives the session by
//! re-invoking the chrome-use binary per step, so it inherits every flag /
//! launch / daemon / `@ref` semantic for free; the daemon stays up for the
//! session, so each step is just a fast socket round-trip.
//!
//! ```yaml
//! suite: chatgpt smoke
//! setup:
//!   - account: chatgpt/huayue        # cookie-use injects this login (optional)
//! cases:
//!   - name: home loads logged in
//!     steps:
//!       - open: https://chatgpt.com/
//!       - wait: { load: networkidle }
//!     assert:
//!       - url: { contains: chatgpt.com }
//!       - visible: "#prompt-textarea"
//! ```

use crate::flags::Flags;
use serde_json::Value;
use std::process::Command;
use std::time::Instant;

pub fn run_test(suite_path: &str, flags: &Flags) -> i32 {
    let text = match std::fs::read_to_string(suite_path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("{} cannot read suite '{}': {}", err(), suite_path, e);
            return 2;
        }
    };
    // YAML deserializes straight into serde_json::Value (maps→objects, etc.).
    let suite: Value = match serde_yaml::from_str(&text) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{} invalid YAML in '{}': {}", err(), suite_path, e);
            return 2;
        }
    };

    let cases = match suite.get("cases").and_then(|c| c.as_array()) {
        Some(c) if !c.is_empty() => c.clone(),
        _ => {
            eprintln!("{} suite has no `cases`", err());
            return 2;
        }
    };
    let suite_name = suite
        .get("suite")
        .and_then(|s| s.as_str())
        .unwrap_or("suite");

    let exe = match std::env::current_exe() {
        Ok(p) => p.to_string_lossy().into_owned(),
        Err(e) => {
            eprintln!("{} cannot find own binary: {}", err(), e);
            return 2;
        }
    };

    // A dedicated launched browser by default (deterministic, re-runnable). If
    // the user named a --session, target that existing one instead.
    let (session, do_launch) = if flags.session == "default" {
        ("cu-test".to_string(), true)
    } else {
        (flags.session.clone(), flags.force_launch)
    };
    let owns_session = session == "cu-test";

    let mut base: Vec<String> = vec!["--session".into(), session.clone()];
    if do_launch {
        base.push("--launch".into());
    }
    if let Some(p) = &flags.profile {
        base.push("--profile".into());
        base.push(p.clone());
    }

    let artifacts_dir = flags
        .download_path
        .clone()
        .unwrap_or_else(|| "cu-test-artifacts".to_string());

    let runner = Runner {
        exe,
        base,
        artifacts_dir,
        session: session.clone(),
    };

    // --- setup (runs once) ---
    if let Some(setup) = suite.get("setup").and_then(|s| s.as_array()) {
        for item in setup {
            if let Err(e) = runner.run_setup_item(item) {
                eprintln!("{} setup failed: {}", err(), e);
                if owns_session {
                    runner.close();
                }
                return 2;
            }
        }
    }

    // --- cases ---
    println!("suite: {}  (session {})", suite_name, session);
    let mut passed = 0usize;
    let mut failed = 0usize;
    for case in &cases {
        let name = case
            .get("name")
            .and_then(|n| n.as_str())
            .unwrap_or("(unnamed)");
        let start = Instant::now();
        let outcome = runner.run_case(case);
        let secs = start.elapsed().as_secs_f64();
        match outcome {
            Ok(()) => {
                passed += 1;
                println!("  {} {}  {:.1}s", ok(), name, secs);
            }
            Err(failure) => {
                failed += 1;
                println!("  {} {}  {:.1}s", cross(), name, secs);
                println!("      {}", failure.reason);
                if let Some(shot) = runner.capture_artifact(name) {
                    println!("      ↳ {}", shot);
                }
            }
        }
    }

    if owns_session {
        runner.close();
    }

    println!(
        "{} cases · {} passed · {} failed",
        cases.len(),
        passed,
        failed
    );
    i32::from(failed > 0)
}

struct Failure {
    reason: String,
}

struct Runner {
    exe: String,
    base: Vec<String>,
    artifacts_dir: String,
    session: String,
}

impl Runner {
    /// Run one chrome-use sub-command. Returns the `data` object on success.
    fn cli(&self, args: &[String]) -> Result<Option<Value>, String> {
        let out = Command::new(&self.exe)
            .args(&self.base)
            .args(args)
            .arg("--json")
            .output()
            .map_err(|e| format!("spawning chrome-use: {}", e))?;
        let stdout = String::from_utf8_lossy(&out.stdout);
        if let Ok(v) = serde_json::from_str::<Value>(stdout.trim()) {
            let success = v
                .get("success")
                .and_then(|b| b.as_bool())
                .unwrap_or(out.status.success());
            if !success {
                return Err(v
                    .get("error")
                    .and_then(|e| e.as_str())
                    .unwrap_or("command failed")
                    .to_string());
            }
            return Ok(v.get("data").cloned());
        }
        if out.status.success() {
            Ok(None)
        } else {
            Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
        }
    }

    fn close(&self) {
        let _ = self.cli(&["close".to_string()]);
    }

    /// `account: <id>` swaps a stored cookie-use login into this session. Used
    /// from `setup` (once, before all cases) AND from a case's `steps` (to
    /// switch accounts mid-suite — the injected login takes effect on the next
    /// `open`/navigation in that case).
    fn inject_account(&self, acct: &str) -> Result<(), String> {
        let target = format!("session:{}", self.session);
        let out = Command::new("cookie-use")
            .args(["use", acct, "--target", &target, "--no-open"])
            .output();
        match out {
            Ok(o) if o.status.success() => Ok(()),
            Ok(o) => Err(format!(
                "cookie-use use {} failed: {}",
                acct,
                String::from_utf8_lossy(&o.stderr).trim()
            )),
            Err(e) => Err(format!(
                "cookie-use not available ({}); skip `account:` or install it",
                e
            )),
        }
    }

    fn run_setup_item(&self, item: &Value) -> Result<(), String> {
        // `account: <id>` injects a stored cookie-use login into this session.
        if let Some(acct) = item.get("account").and_then(|a| a.as_str()) {
            return self.inject_account(acct);
        }
        // Otherwise it's a normal step.
        let args = step_to_args(item)?;
        self.cli(&args).map(|_| ())
    }

    fn run_case(&self, case: &Value) -> Result<(), Failure> {
        if let Some(steps) = case.get("steps").and_then(|s| s.as_array()) {
            for step in steps {
                // `account: <id>` as a step switches the logged-in account
                // mid-suite (takes effect on the next open/navigation).
                if let Some(acct) = step.get("account").and_then(|a| a.as_str()) {
                    self.inject_account(acct).map_err(|e| Failure {
                        reason: format!("account `{}` switch failed: {}", acct, e),
                    })?;
                    continue;
                }
                let args = step_to_args(step).map_err(|e| Failure {
                    reason: format!("bad step: {}", e),
                })?;
                self.cli(&args).map_err(|e| Failure {
                    reason: format!(
                        "step `{}` failed: {}",
                        args.first().cloned().unwrap_or_default(),
                        e
                    ),
                })?;
            }
        }
        if let Some(asserts) = case.get("assert").and_then(|a| a.as_array()) {
            for a in asserts {
                let (expr, describe) = assert_to_eval(a).map_err(|e| Failure {
                    reason: format!("bad assert: {}", e),
                })?;
                let data = self.cli(&["eval".to_string(), expr]).map_err(|e| Failure {
                    reason: format!("assert `{}` could not run: {}", describe, e),
                })?;
                let result = data.as_ref().and_then(|d| d.get("result"));
                if !is_truthy(result) {
                    let got = result
                        .map(value_short)
                        .unwrap_or_else(|| "undefined".into());
                    return Err(Failure {
                        reason: format!("assert {} → got {}", describe, got),
                    });
                }
            }
        }
        Ok(())
    }

    /// Best-effort screenshot of the failing state. Returns the saved path.
    fn capture_artifact(&self, case_name: &str) -> Option<String> {
        let _ = std::fs::create_dir_all(&self.artifacts_dir);
        let path = format!("{}/{}.png", self.artifacts_dir, slug(case_name));
        match self.cli(&["screenshot".to_string(), path.clone()]) {
            Ok(Some(d)) => d
                .get("path")
                .and_then(|p| p.as_str())
                .map(String::from)
                .or(Some(path)),
            Ok(None) => Some(path),
            Err(_) => None,
        }
    }
}

/// Map a YAML step (a one-key object) to chrome-use CLI args.
fn step_to_args(step: &Value) -> Result<Vec<String>, String> {
    let obj = step
        .as_object()
        .ok_or_else(|| "step must be a key: value mapping".to_string())?;
    let (key, val) = obj.iter().next().ok_or_else(|| "empty step".to_string())?;
    let s = |v: &Value| v.as_str().map(String::from);
    match key.as_str() {
        "open" | "goto" | "navigate" => {
            let url = s(val).ok_or("open: expected a URL string")?;
            Ok(vec!["open".into(), url])
        }
        "click" => Ok(vec![
            "click".into(),
            s(val).ok_or("click: expected a selector")?,
        ]),
        "press" => Ok(vec!["press".into(), s(val).ok_or("press: expected a key")?]),
        "eval" => Ok(vec![
            "eval".into(),
            s(val).ok_or("eval: expected JS string")?,
        ]),
        "fill" | "type" => {
            let sel = field(val, &["sel", "selector"]).ok_or("fill/type: need sel")?;
            let text = field(val, &["text", "value"]).ok_or("fill/type: need text")?;
            Ok(vec![key.clone(), sel, text])
        }
        "scroll" => {
            if let Some(dir) = s(val) {
                Ok(vec!["scroll".into(), dir])
            } else {
                let dir = field(val, &["dir", "direction"]).ok_or("scroll: need dir")?;
                let mut a = vec!["scroll".into(), dir];
                if let Some(px) = field(val, &["px", "pixels"]) {
                    a.push(px);
                }
                Ok(a)
            }
        }
        "wait" => {
            if let Some(n) = val.as_i64() {
                Ok(vec!["wait".into(), n.to_string()])
            } else if let Some(load) = field(val, &["load"]) {
                Ok(vec!["wait".into(), "--load".into(), load])
            } else if let Some(sel) = s(val) {
                Ok(vec!["wait".into(), sel])
            } else {
                Err("wait: expected ms, a selector, or { load: <state> }".into())
            }
        }
        other => Err(format!("unknown step `{}`", other)),
    }
}

/// Compile a YAML assert (one-key object) into (js-bool-expr, human-describe).
fn assert_to_eval(a: &Value) -> Result<(String, String), String> {
    let obj = a
        .as_object()
        .ok_or_else(|| "assert must be a key: value mapping".to_string())?;
    let (key, val) = obj
        .iter()
        .next()
        .ok_or_else(|| "empty assert".to_string())?;
    match key.as_str() {
        "url" => {
            let (op, want) = str_op(val).ok_or("url: need contains/equals/matches")?;
            Ok((
                cmp_expr("location.href", &op, &want),
                format!("url {} {:?}", op, want),
            ))
        }
        "visible" => {
            let sel = val.as_str().ok_or("visible: expected a selector")?;
            Ok((visible_expr(sel), format!("visible {:?}", sel)))
        }
        "hidden" => {
            let sel = val.as_str().ok_or("hidden: expected a selector")?;
            Ok((
                format!("!({})", visible_expr(sel)),
                format!("hidden {:?}", sel),
            ))
        }
        "text" => {
            let sel = field(val, &["sel", "selector"]).ok_or("text: need sel")?;
            let (op, want) = str_op(val).ok_or("text: need contains/equals/matches")?;
            let base = format!(
                "((document.querySelector({})||{{}}).textContent||\"\")",
                js(&sel)
            );
            Ok((
                cmp_expr(&base, &op, &want),
                format!("text {:?} {} {:?}", sel, op, want),
            ))
        }
        "count" => {
            let sel = field(val, &["sel", "selector"]).ok_or("count: need sel")?;
            let n = val
                .get("eq")
                .or_else(|| val.get("equals"))
                .and_then(|v| v.as_i64())
                .ok_or("count: need eq: <n>")?;
            Ok((
                format!("document.querySelectorAll({}).length==={}", js(&sel), n),
                format!("count {:?} == {}", sel, n),
            ))
        }
        "eval" => {
            let expr = val.as_str().ok_or("eval: expected JS string")?;
            Ok((format!("!!({})", expr), format!("eval {:?}", expr)))
        }
        other => Err(format!("unknown assert `{}`", other)),
    }
}

fn visible_expr(sel: &str) -> String {
    format!(
        "(function(){{var e=document.querySelector({});return !!(e&&(e.offsetWidth||e.offsetHeight||e.getClientRects().length));}})()",
        js(sel)
    )
}

/// Extract (op, want) from `{contains|equals|matches: <str>}`.
fn str_op(val: &Value) -> Option<(String, String)> {
    for op in ["contains", "equals", "matches"] {
        if let Some(s) = val.get(op).and_then(|v| v.as_str()) {
            return Some((op.to_string(), s.to_string()));
        }
    }
    None
}

fn cmp_expr(base: &str, op: &str, want: &str) -> String {
    match op {
        "equals" => format!("({})==={}", base, js(want)),
        "matches" => format!("new RegExp({}).test({})", js(want), base),
        _ => format!("({}).includes({})", base, js(want)), // contains
    }
}

/// First present field among `keys`, as a string.
fn field(val: &Value, keys: &[&str]) -> Option<String> {
    for k in keys {
        if let Some(v) = val.get(*k) {
            return match v {
                Value::String(s) => Some(s.clone()),
                Value::Number(n) => Some(n.to_string()),
                Value::Bool(b) => Some(b.to_string()),
                _ => None,
            };
        }
    }
    None
}

/// JSON-encode a string so it embeds safely as a JS literal.
fn js(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into())
}

fn is_truthy(v: Option<&Value>) -> bool {
    match v {
        Some(Value::Bool(b)) => *b,
        Some(Value::Null) | None => false,
        Some(Value::Number(n)) => n.as_f64().map(|f| f != 0.0).unwrap_or(false),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}

fn value_short(v: &Value) -> String {
    let s = v.to_string();
    if s.len() > 60 {
        format!("{}…", &s[..60])
    } else {
        s
    }
}

fn slug(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect();
    s.trim_matches('-').to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn step_mapping() {
        assert_eq!(
            step_to_args(&json!({"open": "https://x.com"})).unwrap(),
            vec!["open", "https://x.com"]
        );
        assert_eq!(
            step_to_args(&json!({"fill": {"sel": "#a", "text": "hi"}})).unwrap(),
            vec!["fill", "#a", "hi"]
        );
        assert_eq!(
            step_to_args(&json!({"wait": {"load": "networkidle"}})).unwrap(),
            vec!["wait", "--load", "networkidle"]
        );
        assert_eq!(
            step_to_args(&json!({"wait": 500})).unwrap(),
            vec!["wait", "500"]
        );
        assert!(step_to_args(&json!({"bogus": 1})).is_err());
    }

    #[test]
    fn assert_compilation() {
        let (e, _) = assert_to_eval(&json!({"url": {"contains": "x.com"}})).unwrap();
        assert!(e.contains("location.href") && e.contains(".includes("));
        let (e, _) = assert_to_eval(&json!({"count": {"sel": ".a", "eq": 3}})).unwrap();
        assert!(e.contains("querySelectorAll") && e.ends_with("===3"));
        let (e, _) = assert_to_eval(&json!({"hidden": "#x"})).unwrap();
        assert!(e.starts_with("!("));
        let (e, _) = assert_to_eval(&json!({"eval": "window.ok"})).unwrap();
        assert_eq!(e, "!!(window.ok)");
        assert!(assert_to_eval(&json!({"bogus": 1})).is_err());
    }

    #[test]
    fn truthiness() {
        assert!(is_truthy(Some(&json!(true))));
        assert!(!is_truthy(Some(&json!(false))));
        assert!(!is_truthy(None));
        assert!(!is_truthy(Some(&json!(""))));
        assert!(is_truthy(Some(&json!("x"))));
        assert!(!is_truthy(Some(&json!(0))));
    }

    #[test]
    fn js_escaping() {
        // Selectors with quotes must embed safely.
        assert_eq!(js(r#"a"b"#), r#""a\"b""#);
    }
}

fn ok() -> &'static str {
    "\x1b[32m✓\x1b[0m"
}
fn cross() -> &'static str {
    "\x1b[31m✗\x1b[0m"
}
fn err() -> &'static str {
    "\x1b[31merror:\x1b[0m"
}

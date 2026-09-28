//! `upgrade --check` / `--json` and the daily notice, against the real binary.
//! No network: the release API is a `file://` fixture, HOME and the cache are
//! temp dirs, and PATH is empty so no `claude` or `git` can be run.
#![cfg(unix)]
use std::fs;
use std::process::{Command, Output};
use tempfile::TempDir;

const BIN: &str = env!("CARGO_BIN_EXE_chrome-use");
const CURRENT: &str = env!("CARGO_PKG_VERSION");

fn release_fixture(tmp: &TempDir, tag: &str) -> String {
    let path = tmp.path().join("release.json");
    fs::write(
        &path,
        format!(r#"{{"tag_name":"{tag}","prerelease":false,"draft":false}}"#),
    )
    .unwrap();
    format!("file://{}", path.display())
}

fn command(tmp: &TempDir, args: &[&str]) -> Command {
    let mut cmd = Command::new(BIN);
    cmd.args(args)
        .env_clear()
        .env("HOME", tmp.path())
        .env("USERPROFILE", tmp.path())
        .env("XDG_CACHE_HOME", tmp.path().join("cache"))
        .env("XDG_CONFIG_HOME", tmp.path().join("config"))
        .env("PATH", "")
        .env("NO_COLOR", "1");
    cmd
}

fn run_with_curl(tmp: &TempDir, args: &[&str], api: &str) -> Output {
    // curl is the one external program the check needs; expose only it.
    let bin = tmp.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    let curl = ["/usr/bin/curl", "/bin/curl"]
        .into_iter()
        .find(|p| std::path::Path::new(p).exists())
        .expect("curl");
    let _ = std::os::unix::fs::symlink(curl, bin.join("curl"));
    command(tmp, args)
        .env("PATH", &bin)
        .env("CHROME_USE_RELEASE_API_URL", api)
        .output()
        .unwrap()
}

fn cache_file(tmp: &TempDir) -> std::path::PathBuf {
    tmp.path().join("cache/chrome-use/update-check.json")
}

fn seed_cache(tmp: &TempDir, latest: &str) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    fs::create_dir_all(cache_file(tmp).parent().unwrap()).unwrap();
    fs::write(
        cache_file(tmp),
        format!(r#"{{"checked_at":{now},"latest":"{latest}"}}"#),
    )
    .unwrap();
}

#[test]
fn json_reports_an_available_update_and_the_installed_skills() {
    let tmp = tempfile::tempdir().unwrap();
    let skill = tmp.path().join(".agents/skills/chrome-use");
    fs::create_dir_all(&skill).unwrap();
    fs::write(skill.join("SKILL.md"), "placeholder").unwrap();
    fs::create_dir_all(tmp.path().join(".claude/plugins")).unwrap();
    fs::write(
        tmp.path().join(".claude/plugins/installed_plugins.json"),
        r#"{"version":2,"plugins":{"chrome-use@leeguooooo-plugins":[{"installPath":"/placeholder"}]}}"#,
    )
    .unwrap();

    let api = release_fixture(&tmp, "v999.0.0");
    let out = run_with_curl(&tmp, &["upgrade", "--json"], &api);
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["name"], "chrome-use");
    assert_eq!(v["current"], CURRENT);
    assert_eq!(v["latest"], "999.0.0");
    assert_eq!(v["update_available"], true);
    let skills = v["skills"].as_array().unwrap();
    let channels: Vec<&str> = skills
        .iter()
        .map(|s| s["channel"].as_str().unwrap())
        .collect();
    assert!(channels.contains(&"claude-plugin"), "{v}");
    assert!(channels.contains(&"installer"), "{v}");
    for s in skills {
        assert!(s["path"].is_string() && s["update"].is_string(), "{s}");
    }
    // --json changed nothing but the check cache.
    assert_eq!(fs::read(skill.join("SKILL.md")).unwrap(), b"placeholder");
    let cache: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(cache_file(&tmp)).unwrap()).unwrap();
    assert_eq!(cache["latest"], "999.0.0");
}

#[test]
fn check_says_up_to_date_and_exits_zero() {
    let tmp = tempfile::tempdir().unwrap();
    let api = release_fixture(&tmp, &format!("v{CURRENT}"));
    let out = run_with_curl(&tmp, &["upgrade", "--check"], &api);
    assert_eq!(out.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        stdout.lines().next().unwrap(),
        format!("chrome-use {CURRENT} is up to date")
    );
}

#[test]
fn a_failed_check_exits_two() {
    let tmp = tempfile::tempdir().unwrap();
    let missing = format!("file://{}", tmp.path().join("missing.json").display());
    let out = run_with_curl(&tmp, &["upgrade", "--check"], &missing);
    assert_eq!(out.status.code(), Some(2));
    let out = run_with_curl(&tmp, &["upgrade", "--json"], &missing);
    assert_eq!(out.status.code(), Some(2));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["update_available"], false);
    assert!(v["error"].is_string());
}

#[test]
fn notice_goes_to_stderr_only() {
    let tmp = tempfile::tempdir().unwrap();
    seed_cache(&tmp, "999.0.0");
    let out = command(&tmp, &["skills", "list"]).output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let expected = format!(
        "chrome-use 999.0.0 is available (you have {CURRENT}). Upgrade: chrome-use upgrade"
    );
    assert_eq!(
        stderr.lines().filter(|l| *l == expected).count(),
        1,
        "stderr: {stderr}"
    );
    assert!(!stdout.contains("is available"), "stdout: {stdout}");
}

#[test]
fn notice_respects_opt_outs_and_meta_commands() {
    let tmp = tempfile::tempdir().unwrap();
    seed_cache(&tmp, "999.0.0");
    for var in ["CI", "CHROME_USE_NO_UPDATE_CHECK", "USE_NO_UPDATE_CHECK"] {
        let out = command(&tmp, &["skills", "list"])
            .env(var, "1")
            .output()
            .unwrap();
        assert!(
            !String::from_utf8_lossy(&out.stderr).contains("is available"),
            "{var}"
        );
    }
    for args in [&["--version"][..], &["--help"], &["skills", "--help"]] {
        let out = command(&tmp, args).output().unwrap();
        assert!(
            !String::from_utf8_lossy(&out.stderr).contains("is available"),
            "{args:?}"
        );
    }
}

#[test]
fn a_fresh_cache_is_not_rechecked() {
    let tmp = tempfile::tempdir().unwrap();
    seed_cache(&tmp, "1.0.0");
    let before = fs::read_to_string(cache_file(&tmp)).unwrap();
    command(&tmp, &["skills", "list"]).output().unwrap();
    assert_eq!(fs::read_to_string(cache_file(&tmp)).unwrap(), before);
}

#[test]
fn a_stale_cache_bumps_checked_at_even_when_offline() {
    let tmp = tempfile::tempdir().unwrap();
    fs::create_dir_all(cache_file(&tmp).parent().unwrap()).unwrap();
    fs::write(cache_file(&tmp), r#"{"checked_at":1,"latest":"1.0.0"}"#).unwrap();
    // PATH is empty, so the background check cannot even start curl.
    command(&tmp, &["skills", "list"]).output().unwrap();
    let cache: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(cache_file(&tmp)).unwrap()).unwrap();
    assert!(cache["checked_at"].as_u64().unwrap() > 1);
    assert_eq!(cache["latest"], "1.0.0");
}

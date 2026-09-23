//! Exercise the actual installer CLI with no external tools on PATH.
use std::fs;
use std::process::{Command, Output};
use tempfile::TempDir;

const BIN: &str = env!("CARGO_BIN_EXE_chrome-use");
const CONTENT: &[u8] = include_bytes!("../../skills/chrome-use/SKILL.md");

fn run(tmp: &TempDir, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(tmp.path().join("project"))
        .env("HOME", tmp.path())
        .env("USERPROFILE", tmp.path())
        .env("CODEX_HOME", tmp.path().join("custom codex"))
        .env("CLAUDE_CONFIG_DIR", tmp.path().join("custom claude"))
        .env("XDG_CONFIG_HOME", tmp.path().join("config"))
        .env("PATH", "")
        .env("NO_COLOR", "1")
        .output()
        .unwrap()
}

fn fixture() -> TempDir {
    let tmp = tempfile::tempdir().unwrap();
    fs::create_dir_all(tmp.path().join("project")).unwrap();
    tmp
}

#[test]
fn global_install_is_offline_verified_and_respects_runner_homes() {
    let tmp = fixture();
    fs::create_dir_all(tmp.path().join(".pi/agent")).unwrap();
    fs::create_dir_all(tmp.path().join("config/opencode")).unwrap();
    let out = run(&tmp, &["skill", "install", "--json"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let result: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(result["success"], true);
    let paths = result["data"]["paths"].as_array().unwrap();
    assert_eq!(paths.len(), 5);
    for path in paths {
        let path = std::path::Path::new(path.as_str().unwrap());
        assert!(path.starts_with(tmp.path()), "{}", path.display());
        assert_eq!(fs::read(path).unwrap(), CONTENT);
    }
    assert!(!tmp.path().join("custom codex").exists());
    assert!(tmp
        .path()
        .join("custom claude/skills/chrome-use/SKILL.md")
        .is_file());
    assert!(!tmp.path().join(".codex").exists());
    assert!(!tmp.path().join("project/.agents").exists());
}

#[test]
fn project_install_and_refresh_do_not_touch_global_skills() {
    let tmp = fixture();
    for name in ["install", "update", "refresh"] {
        let target = tmp
            .path()
            .join("project/.agents/skills/chrome-use/SKILL.md");
        if target.exists() {
            fs::write(&target, "obsolete").unwrap();
        }
        let out = run(&tmp, &["skills", name, "--project", "--json"]);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(fs::read(target).unwrap(), CONTENT);
        assert!(!tmp.path().join(".agents").exists());
        assert!(!tmp.path().join("custom codex").exists());
    }
}

#[test]
fn partial_failure_is_nonzero_and_names_the_failed_path() {
    let tmp = fixture();
    fs::write(tmp.path().join("custom claude"), "blocked").unwrap();
    let out = run(&tmp, &["skill", "install", "--json"]);
    assert!(!out.status.success());
    let result: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(result["success"], false);
    assert_eq!(result["data"]["paths"].as_array().unwrap().len(), 2);
    assert!(result["errors"][0]
        .as_str()
        .unwrap()
        .contains("custom claude"));
    assert_eq!(
        fs::read_to_string(tmp.path().join("custom claude")).unwrap(),
        "blocked"
    );
}

#[test]
fn doctor_finds_a_skill_in_a_custom_runner_home() {
    let tmp = fixture();
    let skill = tmp.path().join("custom codex/skills/chrome-use");
    fs::create_dir_all(&skill).unwrap();
    fs::write(skill.join("SKILL.md"), CONTENT).unwrap();
    let out = run(&tmp, &["doctor", "--quick", "--offline", "--json"]);
    let result: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let checks = result["checks"].as_array().unwrap();
    let skill = checks
        .iter()
        .find(|c| c["id"] == "skill.installed")
        .unwrap();
    assert_eq!(skill["status"], "pass");
}

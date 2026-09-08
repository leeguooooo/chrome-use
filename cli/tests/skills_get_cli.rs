//! Integration tests for `chrome-use skills get`.
//!
//! These spawn the real CLI binary so the assertion is on what a caller
//! actually receives on stdout -- the only place a "several top-level JSON
//! objects" bug is visible. `AGENT_BROWSER_SKILLS_DIR` points at a throwaway
//! skill tree so the test never depends on the shipped skill-data.

use std::fs;
use std::process::Command;
use tempfile::TempDir;

const BIN: &str = env!("CARGO_BIN_EXE_chrome-use");

/// A skill with two references, and a template whose name collides with one.
fn fixture() -> TempDir {
    let tmp = TempDir::new().unwrap();
    let skill = tmp.path().join("demo");
    fs::create_dir_all(skill.join("references")).unwrap();
    fs::create_dir_all(skill.join("templates")).unwrap();
    fs::write(
        skill.join("SKILL.md"),
        "---\nname: demo\ndescription: a demo skill\n---\n\nbody\n",
    )
    .unwrap();
    fs::write(skill.join("references/alpha.md"), "# alpha reference\n").unwrap();
    fs::write(skill.join("references/beta.md"), "# beta reference\n").unwrap();
    fs::write(skill.join("templates/alpha.md"), "# alpha template\n").unwrap();
    tmp
}

/// Run `skills get` against the fixture and return stdout, stderr and the
/// exit code -- all three, because a command can print the right thing and
/// still tell its caller it failed.
fn run(tmp: &TempDir, args: &[&str]) -> (String, String, i32) {
    let out = Command::new(BIN)
        .args(["skills", "get"])
        .args(args)
        .env("AGENT_BROWSER_SKILLS_DIR", tmp.path())
        .env("NO_COLOR", "1")
        .output()
        .expect("the CLI runs");
    (
        String::from_utf8(out.stdout).expect("stdout is utf-8"),
        String::from_utf8(out.stderr).expect("stderr is utf-8"),
        out.status.code().unwrap_or(-1),
    )
}

/// Stdout only, for the cases that assert on content rather than status.
fn skills_get(tmp: &TempDir, args: &[&str]) -> String {
    run(tmp, args).0
}

/// Serving a reference is a success, and must exit like one.
///
/// The reference branch `continue`s past the target collection, so `targets`
/// ends up empty and the "No skill name provided" error fires -- after the
/// content has already been printed. Every reference get therefore ended in a
/// spurious error line and a non-zero exit, which is the whole feature's
/// documented path reporting failure on success.
#[test]
fn serving_a_reference_exits_zero_and_says_nothing_else() {
    let tmp = fixture();

    let (stdout, stderr, code) = run(&tmp, &["demo/beta"]);
    assert_eq!(code, 0, "stdout={stdout} stderr={stderr}");
    assert!(stdout.contains("beta reference"), "{stdout}");
    assert!(!stderr.contains("No skill name provided"), "{stderr}");

    let (stdout, stderr, code) = run(&tmp, &["--json", "demo/beta"]);
    assert_eq!(code, 0, "stdout={stdout} stderr={stderr}");
    assert!(!stdout.contains("No skill name provided"), "{stdout}");
}

/// Two references in one `--json` call must be ONE parseable document.
///
/// Printing per name concatenates top-level objects, which every JSON parser
/// rejects -- and `--json` exists precisely so a program can read the output.
#[test]
fn two_references_in_json_mode_are_a_single_document() {
    let tmp = fixture();
    let stdout = skills_get(&tmp, &["--json", "demo/alpha", "demo/beta"]);

    let doc: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("not one document: {e}\n{stdout}"));
    assert_eq!(doc["success"], serde_json::json!(true));

    let refs = doc["references"].as_array().expect("a references array");
    assert_eq!(refs.len(), 2, "{doc}");
    assert_eq!(
        refs[0]["reference"],
        serde_json::json!("references/alpha.md")
    );
    assert_eq!(
        refs[1]["reference"],
        serde_json::json!("references/beta.md")
    );
    assert!(refs[0]["content"]
        .as_str()
        .unwrap()
        .contains("alpha reference"));
}

/// One reference keeps the flat shape it already emitted: fixing the multi-get
/// must not break a caller that only ever asked for one.
#[test]
fn a_single_reference_keeps_its_flat_shape() {
    let tmp = fixture();
    let stdout = skills_get(&tmp, &["--json", "demo/beta"]);

    let doc: serde_json::Value = serde_json::from_str(stdout.trim()).expect("one document");
    assert_eq!(doc["success"], serde_json::json!(true));
    assert_eq!(doc["skill"], serde_json::json!("demo"));
    assert_eq!(doc["reference"], serde_json::json!("references/beta.md"));
    assert!(doc["content"].as_str().unwrap().contains("beta reference"));
    assert!(doc.get("references").is_none(), "{doc}");
}

/// An explicit directory is served as written, not traded for the other one.
#[test]
fn an_explicit_template_request_is_not_served_the_reference() {
    let tmp = fixture();

    let stdout = skills_get(&tmp, &["--json", "demo/templates/alpha"]);
    let doc: serde_json::Value = serde_json::from_str(stdout.trim()).expect("one document");
    assert_eq!(doc["reference"], serde_json::json!("templates/alpha.md"));
    assert!(doc["content"].as_str().unwrap().contains("alpha template"));

    let stdout = skills_get(&tmp, &["--json", "demo/references/alpha"]);
    let doc: serde_json::Value = serde_json::from_str(stdout.trim()).expect("one document");
    assert_eq!(doc["reference"], serde_json::json!("references/alpha.md"));
    assert!(doc["content"].as_str().unwrap().contains("alpha reference"));
}

/// Text mode separates several references instead of running them together.
///
/// Without a separator two files arrive as one run-on document, and a reader
/// cannot tell where the first ends.
#[test]
fn two_references_in_text_mode_are_separated() {
    let tmp = fixture();
    let stdout = skills_get(&tmp, &["demo/alpha", "demo/beta"]);

    assert!(stdout.contains("alpha reference"), "{stdout}");
    assert!(stdout.contains("beta reference"), "{stdout}");
    assert!(stdout.contains("---"), "{stdout}");
}

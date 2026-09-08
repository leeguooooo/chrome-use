use std::process::Command;

#[test]
fn relative_relay_directory_is_rejected_in_different_working_directories() {
    let binary = env!("CARGO_BIN_EXE_chrome-use");
    for _ in 0..2 {
        let cwd = tempfile::tempdir().unwrap();
        let output = Command::new(binary)
            .current_dir(cwd.path())
            .env("CHROME_USE_RELAY_DIR", "relative-registry")
            .env("CHROME_USE_NO_UPDATE_CHECK", "1")
            .args(["browsers", "--json"])
            .output()
            .unwrap();
        assert!(!output.status.success());
        let response: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(response["code"], "invalid_configuration");
        assert_eq!(response["retryable"], false);
        assert!(!cwd.path().join("relative-registry").exists());
    }
}

#[test]
fn native_host_configuration_errors_never_pollute_protocol_stdout() {
    let cwd = tempfile::tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_chrome-use"))
        .current_dir(cwd.path())
        .env("CHROME_USE_RELAY_DIR", "relative-registry")
        .args(["__nm-host", "chrome-extension://fixture/"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8(output.stderr)
        .unwrap()
        .contains("absolute path"));
    assert!(!cwd.path().join("relative-registry").exists());
}

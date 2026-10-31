//! Standalone Windows subprocess regression; no browser or Cargo dependencies.
#[path = "../../cli/src/native/windows_stdio.rs"]
mod windows_stdio;
use std::io::{BufRead, Write};
use std::process::{Command, Stdio};

fn main() {
    if let Some(mode) = std::env::args().nth(1) {
        let _destination = match mode.as_str() {
            "null" => Some(
                windows_stdio::redirect_stderr(
                    std::fs::OpenOptions::new().write(true).open("NUL").unwrap(),
                )
                .unwrap(),
            ),
            "log" => Some(
                windows_stdio::redirect_stderr(
                    std::fs::File::create(std::env::args().nth(2).unwrap()).unwrap(),
                )
                .unwrap(),
            ),
            _ => None,
        };
        println!("ready");
        std::io::stdout().flush().unwrap();
        let mut gate = String::new();
        std::io::stdin().read_line(&mut gate).unwrap();
        eprintln!("daemon diagnostic after readiness");
        return;
    }
    let log = std::env::temp_dir().join(format!("chrome-use-stderr-{}.log", std::process::id()));
    for mode in ["held", "dropped", "null", "log"] {
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([mode, log.to_str().unwrap()])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut line = String::new();
        std::io::BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        assert_eq!(line.trim(), "ready");
        let stderr = child.stderr.take().unwrap();
        let held = if mode == "held" {
            Some(stderr)
        } else {
            drop(stderr);
            None
        };
        child.stdin.take().unwrap().write_all(b"go\n").unwrap();
        let status = child.wait().unwrap();
        assert_eq!(
            status.success(),
            mode != "dropped",
            "unexpected {mode} status {status}"
        );
        drop(held);
        println!("{mode}: exit={status}");
    }
    assert_eq!(
        std::fs::read_to_string(&log).unwrap().trim(),
        "daemon diagnostic after readiness"
    );
    std::fs::remove_file(log).unwrap();
}

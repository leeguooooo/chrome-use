//! Prefer the caller's sqlite3. macOS can recover from a missing or
//! incompatible executable with its native tool, never from a query failure.
use std::ffi::OsStr;
use std::io;
use std::path::Path;
use std::process::{Command, Output};

pub(crate) fn output(args: &[&OsStr]) -> io::Result<Output> {
    #[cfg(target_os = "macos")]
    let fallback = Some(Path::new("/usr/bin/sqlite3"));
    #[cfg(not(target_os = "macos"))]
    let fallback = None;
    output_with(Path::new("sqlite3"), fallback, args)
}

fn unavailable_executable(error: &io::Error) -> bool {
    if error.kind() == io::ErrorKind::NotFound {
        return true;
    }
    #[cfg(unix)]
    if error.raw_os_error() == Some(libc::ENOEXEC) {
        return true;
    }
    #[cfg(target_os = "macos")]
    if error.raw_os_error() == Some(libc::EBADARCH) {
        return true;
    }
    false
}

fn output_with(primary: &Path, fallback: Option<&Path>, args: &[&OsStr]) -> io::Result<Output> {
    match Command::new(primary).args(args).output() {
        Err(first) if unavailable_executable(&first) => {
            let Some(native) = fallback else {
                return Err(first);
            };
            Command::new(native).args(args).output().map_err(|second| io::Error::new(second.kind(),
                format!("PATH sqlite3 could not start ({first}); native {} could not start ({second})", native.display())))
        }
        // Once a process starts, its status/stderr are the query result. Do
        // not hide locks, corruption or permission failures behind a retry.
        result => result,
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn script(root: &Path, name: &str, body: &str, executable: bool) -> std::path::PathBuf {
        let path = root.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(
            &path,
            std::fs::Permissions::from_mode(if executable { 0o700 } else { 0o600 }),
        )
        .unwrap();
        path
    }

    fn fallback(root: &Path) -> std::path::PathBuf {
        let marker = root
            .join("native-used")
            .to_string_lossy()
            .replace('\'', "'\\''");
        script(
            root,
            "native-sqlite3",
            &format!("printf invoked > '{marker}'\nprintf 'fixture_name\\n'"),
            true,
        )
    }

    #[test]
    fn usable_primary_is_preferred() {
        let root = tempfile::tempdir().unwrap();
        let primary = script(root.path(), "sqlite3", "printf 'preferred_name\\n'", true);
        let native = fallback(root.path());
        let out = output_with(&primary, Some(&native), &[]).unwrap();
        assert!(out.status.success());
        assert_eq!(out.stdout, b"preferred_name\n");
        assert!(!root.path().join("native-used").exists());
    }

    #[test]
    fn missing_executable_can_use_an_explicit_fallback() {
        let root = tempfile::tempdir().unwrap();
        let native = fallback(root.path());
        let out = output_with(&root.path().join("missing-sqlite3"), Some(&native), &[]).unwrap();
        assert!(out.status.success());
        assert_eq!(out.stdout, b"fixture_name\n");
        assert!(root.path().join("native-used").exists());
    }

    #[test]
    fn sql_errors_are_not_retried_with_another_tool() {
        let root = tempfile::tempdir().unwrap();
        let native = fallback(root.path());
        for message in [
            "database is locked",
            "file is not a database",
            "permission denied",
        ] {
            let primary = script(
                root.path(),
                "sqlite3",
                &format!("printf '%s\\n' '{message}' >&2\nexit 1"),
                true,
            );
            let out = output_with(&primary, Some(&native), &[]).unwrap();
            assert!(!out.status.success());
            assert_eq!(String::from_utf8(out.stderr).unwrap().trim(), message);
            assert!(!root.path().join("native-used").exists());
        }
    }

    #[test]
    fn execution_permission_denial_is_not_bypassed() {
        let root = tempfile::tempdir().unwrap();
        let primary = script(root.path(), "sqlite3", "exit 0", false);
        let native = fallback(root.path());
        let error = output_with(&primary, Some(&native), &[]).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert!(!root.path().join("native-used").exists());
        assert!(!unavailable_executable(&io::Error::from_raw_os_error(
            libc::EACCES
        )));
    }

    #[test]
    fn absent_fallback_keeps_startup_failure_explicit() {
        let root = tempfile::tempdir().unwrap();
        let error = output_with(
            &root.path().join("missing-primary"),
            Some(&root.path().join("missing-native")),
            &[],
        )
        .unwrap_err();
        let message = error.to_string();
        assert!(message.contains("PATH sqlite3 could not start"));
        assert!(message.contains("native") && message.contains("could not start"));
        assert!(output_with(&root.path().join("missing-primary"), None, &[]).is_err());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn bad_cpu_type_is_an_executable_failure() {
        assert!(unavailable_executable(&io::Error::from_raw_os_error(86)));
        assert!(unavailable_executable(&io::Error::from_raw_os_error(
            libc::ENOEXEC
        )));
        assert!(!unavailable_executable(&io::Error::from_raw_os_error(
            libc::EIO
        )));
    }
    #[cfg(target_os = "macos")]
    #[test]
    fn native_sql_query_failure_does_not_start_a_fallback() {
        let root = tempfile::tempdir().unwrap();
        let database = root.path().join("corrupt-synthetic.sqlite");
        std::fs::write(&database, b"synthetic data, not a SQLite database").unwrap();
        let native = fallback(root.path());
        let result = output_with(
            Path::new("/usr/bin/sqlite3"),
            Some(&native),
            &[
                database.as_os_str(),
                OsStr::new("SELECT name FROM cookies;"),
            ],
        )
        .unwrap();
        assert!(!result.status.success());
        assert!(!result.stderr.is_empty());
        assert!(!root.path().join("native-used").exists());
    }
}

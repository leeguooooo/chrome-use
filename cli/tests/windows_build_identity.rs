//! GUI-free, owned actual CLI daemon tests. No Chrome, foreground or input APIs.
#![cfg(all(
    windows,
    target_arch = "x86_64",
    feature = "windows-build-identity-prototype"
))]

#[path = "../src/windows_build_identity.rs"]
mod image;

use std::fs::{self, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_chrome-use");
// Cross-compiled GNU test executables contain the build host's CARGO_BIN_EXE path.
// CI can relocate that exact owned artifact, or its extracted archive copy.
fn binary() -> PathBuf {
    std::env::var_os("CHROME_USE_IDENTITY_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(BIN))
}

struct OwnedDaemon {
    child: Child,
    root: tempfile::TempDir,
}
impl OwnedDaemon {
    fn start(executable: &Path) -> Self {
        let root = tempfile::tempdir().unwrap();
        let sockets = root.path().join("sockets");
        fs::create_dir_all(&sockets).unwrap();
        let private_temp = root.path().join("temp");
        fs::create_dir_all(&private_temp).unwrap();
        let mut command = Command::new(executable);
        command.env_clear();
        // Windows DLL loading needs the real system root; explicit socket/appdata/temp paths
        // are private; HOME/USERPROFILE do not redirect Windows Known Folders. No inherited daemon/browser/CDP/relay/updater settings.
        for name in ["SystemRoot", "WINDIR", "PATH", "PATHEXT"] {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
        command
            .env("TEMP", &private_temp)
            .env("TMP", &private_temp)
            .env("HOME", root.path())
            .env("USERPROFILE", root.path())
            .env("APPDATA", root.path().join("appdata"))
            .env("LOCALAPPDATA", root.path().join("localappdata"))
            .env("AGENT_BROWSER_SOCKET_DIR", &sockets)
            .env("AGENT_BROWSER_SESSION", "identity-owned")
            .env("AGENT_BROWSER_DAEMON", "1")
            .env("AGENT_BROWSER_SITES_NO_AUTO_UPDATE", "1")
            .env("CHROME_USE_NO_UPDATE_CHECK", "1")
            .env("AGENT_BROWSER_NO_AUTO_UPDATE", "1")
            .current_dir(root.path())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let child = command.spawn().expect("spawn owned actual CLI daemon");
        let mut result = Self { child, root };
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            assert!(
                result.child.try_wait().unwrap().is_none(),
                "owned daemon exited before readiness"
            );
            if let Ok(port) = fs::read_to_string(sockets.join("identity-owned.port")) {
                if let Ok(port) = port.trim().parse::<u16>() {
                    if TcpStream::connect_timeout(
                        &format!("127.0.0.1:{port}").parse().unwrap(),
                        Duration::from_millis(100),
                    )
                    .is_ok()
                    {
                        break;
                    }
                }
            }
            assert!(Instant::now() < deadline, "owned daemon readiness deadline");
            std::thread::sleep(Duration::from_millis(20));
        }
        result
    }
    fn stop(&mut self) {
        if self.child.try_wait().unwrap().is_none() {
            // Child::kill uses the held owned process handle on Windows, not a
            // caller PID or taskkill tree. No child browser can exist in this test.
            self.child.kill().expect("terminate owned daemon");
        }
        self.child.wait().expect("reap owned daemon");
    }
}
impl Drop for OwnedDaemon {
    fn drop(&mut self) {
        self.stop();
    }
}

fn marker() -> image::Marker {
    image::compiled_marker()
}
fn copy_binary(directory: &Path, name: &str) -> PathBuf {
    let path = directory.join(name);
    fs::copy(binary(), &path).unwrap();
    path
}
fn mutate_marker(path: &Path) -> image::Marker {
    let offset = image::fixture::marker_offset(path).unwrap();
    let mut bytes = image::fixture::disk_marker(path).unwrap().fixture_bytes();
    bytes[48] ^= 0x80; // valid schema, deliberately different generation
    let mut file = OpenOptions::new().write(true).open(path).unwrap();
    file.seek(SeekFrom::Start(offset)).unwrap();
    file.write_all(&bytes).unwrap();
    file.sync_all().unwrap();
    image::Marker::parse(&bytes).unwrap()
}

#[test]
fn actual_daemon_mapped_marker_matches_and_equal_copy_is_explicit() {
    let directory = tempfile::tempdir().unwrap();
    let copied = copy_binary(directory.path(), "owned-daemon.exe");
    let mut daemon = OwnedDaemon::start(&copied);
    assert_eq!(image::fixture::disk_marker(&binary()).unwrap(), marker());
    let expected = image::fixture::expected(&binary(), marker()).unwrap();
    let proof = image::check(expected, image::fixture::own(&daemon.child).unwrap()).unwrap();
    assert_eq!(proof.comparison, image::ArtifactMatch::EqualSha256Copy);
    assert_eq!(proof.observed_marker, marker());
    proof.revalidate().unwrap();
    // The proof retains a peer-file pin that refuses ordinary mutation/replacement.
    assert!(OpenOptions::new().write(true).open(&copied).is_err());
    assert!(fs::rename(&copied, directory.path().join("blocked.exe")).is_err());
    drop(proof);
    daemon.stop();
    // After both pins and the process release, owned installer access works again.
    OpenOptions::new().write(true).open(&copied).unwrap();
}

#[test]
fn same_file_branch_and_exited_process_refusal() {
    let directory = tempfile::tempdir().unwrap();
    let copied = copy_binary(directory.path(), "owned-daemon.exe");
    let mut daemon = OwnedDaemon::start(&copied);
    let expected = image::fixture::expected(&copied, marker()).unwrap();
    let proof = image::check(expected, image::fixture::own(&daemon.child).unwrap()).unwrap();
    assert_eq!(proof.comparison, image::ArtifactMatch::SameFile);
    daemon.stop();
    assert_eq!(
        proof.revalidate().unwrap_err(),
        image::Error::ProcessChanged
    );
}

#[test]
fn wrong_marker_and_tampered_artifact_refuse_before_loaded_check() {
    let directory = tempfile::tempdir().unwrap();
    let copied = copy_binary(directory.path(), "wrong-generation.exe");
    let changed = mutate_marker(&copied);
    assert_ne!(changed, marker());
    assert!(matches!(
        image::fixture::expected(&copied, marker()),
        Err(image::Error::AnchorMarkerMismatch)
    ));
    let mut daemon = OwnedDaemon::start(&copied);
    let expected = image::fixture::expected(&binary(), marker()).unwrap();
    let before = image::fixture::read_counts();
    assert!(matches!(
        image::check(expected, image::fixture::own(&daemon.child).unwrap()),
        Err(image::Error::ArtifactMismatch)
    ));
    assert_eq!(
        image::fixture::read_counts(),
        before,
        "unknown artifact acquired no VM_READ handle and no remote metadata read"
    );
    daemon.stop();
    // A same-generation file with a different unrelated byte also fails SHA256.
    let tampered = copy_binary(directory.path(), "tampered-overlay.exe");
    OpenOptions::new()
        .append(true)
        .open(&tampered)
        .unwrap()
        .write_all(b"owned fixture overlay")
        .unwrap();
    let mut daemon = OwnedDaemon::start(&tampered);
    let expected = image::fixture::expected(&binary(), marker()).unwrap();
    let before = image::fixture::read_counts();
    assert!(matches!(
        image::check(expected, image::fixture::own(&daemon.child).unwrap()),
        Err(image::Error::ArtifactMismatch)
    ));
    assert_eq!(
        image::fixture::read_counts(),
        before,
        "unknown artifact acquired no VM_READ handle and no remote metadata read"
    );
    daemon.stop();
}

#[test]
fn old_loaded_generation_is_not_replaced_by_new_disk_marker() {
    let directory = tempfile::tempdir().unwrap();
    let path = copy_binary(directory.path(), "running.exe");
    let replacement = copy_binary(directory.path(), "replacement.exe");
    let new_marker = mutate_marker(&replacement);
    let mut daemon = OwnedDaemon::start(&path);
    let snapshot = image::fixture::snapshot_owned(&daemon.child, &path).unwrap();
    let renamed = directory.path().join("running-old.exe");
    match fs::rename(&path, &renamed) {
        Ok(()) => (),
        Err(error) => {
            // Never call an unexecuted rename/replacement case passed. Exact
            // GNU/MSVC CI must retain this observation and run a staged scenario
            // if the host's filesystem/AV refuses the live executable rename.
            panic!("INCONCLUSIVE: Windows refused owned live-image rename: {error}");
        }
    }
    fs::rename(&replacement, &path).unwrap();
    assert_eq!(image::fixture::disk_marker(&path).unwrap(), new_marker);
    assert_eq!(snapshot.observe().unwrap(), marker());
    // Depending on Windows' process-path rename reporting, production either
    // rejects disk artifact identity or sees matching B disk but old A in memory.
    let expected_b = image::fixture::expected(&path, new_marker).unwrap();
    assert!(matches!(
        image::check(expected_b, image::fixture::own(&daemon.child).unwrap()),
        Err(image::Error::ArtifactMismatch
            | image::Error::LoadedMarkerMismatch
            | image::Error::ModuleAmbiguous
            | image::Error::LoadedLayoutMismatch)
    ));
    // The actual old caller's own loaded static cannot anchor the replaced file.
    assert!(matches!(
        image::fixture::expected(&path, marker()),
        Err(image::Error::AnchorMarkerMismatch)
    ));
    drop(snapshot);
    daemon.stop();
}

#[test]
fn preexisting_writer_prevents_pin_and_failure_paths_release_handles() {
    let directory = tempfile::tempdir().unwrap();
    let path = copy_binary(directory.path(), "owned.exe");
    let writer = OpenOptions::new().write(true).open(&path).unwrap();
    assert!(matches!(
        image::fixture::expected(&path, marker()),
        Err(image::Error::Io(_))
    ));
    drop(writer);
    let pinned = image::fixture::expected(&path, marker()).unwrap();
    assert!(OpenOptions::new().write(true).open(&path).is_err());
    assert!(fs::rename(&path, directory.path().join("pin-blocked.exe")).is_err());
    drop(pinned);
    let before_handles = image::fixture::handle_count().unwrap();
    for _ in 0..32 {
        let expected = image::fixture::expected(&path, marker()).unwrap();
        drop(expected);
    }
    assert_eq!(
        image::fixture::handle_count().unwrap(),
        before_handles,
        "run this fixture suite serially"
    );
    fs::rename(&path, directory.path().join("released.exe")).unwrap();
}

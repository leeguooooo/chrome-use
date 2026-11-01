//! GUI-free socket-owner tests using owned actual CLI daemons. No browser command
//! is sent; the second socket is a metadata fixture, not a proven selected relay.
#![cfg(all(
    windows,
    target_arch = "x86_64",
    feature = "windows-tcp-owner-prototype"
))]

#[cfg(feature = "windows-build-identity-prototype")]
#[path = "../src/windows_build_identity.rs"]
mod windows_build_identity;
#[path = "../src/windows_tcp_owner.rs"]
mod windows_tcp_owner;

use std::fs;
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use windows_tcp_owner as owner;

const BIN: &str = env!("CARGO_BIN_EXE_chrome-use");
fn binary() -> PathBuf {
    std::env::var_os("CHROME_USE_IDENTITY_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(BIN))
}
struct OwnedDaemon {
    child: Child,
    _root: tempfile::TempDir,
    port: u16,
}
impl OwnedDaemon {
    fn start() -> Self {
        let root = tempfile::tempdir().unwrap();
        let sockets = root.path().join("sockets");
        let temp = root.path().join("temp");
        fs::create_dir_all(&sockets).unwrap();
        fs::create_dir_all(&temp).unwrap();
        let mut command = Command::new(binary());
        command.env_clear();
        for name in ["SystemRoot", "WINDIR", "PATH", "PATHEXT"] {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
        command
            .env("HOME", root.path())
            .env("USERPROFILE", root.path())
            .env("APPDATA", root.path().join("appdata"))
            .env("LOCALAPPDATA", root.path().join("localappdata"))
            .env("TEMP", &temp)
            .env("TMP", &temp)
            .env("AGENT_BROWSER_SOCKET_DIR", &sockets)
            .env("AGENT_BROWSER_SESSION", "tcp-owned")
            .env("AGENT_BROWSER_DAEMON", "1")
            .env("AGENT_BROWSER_SITES_NO_AUTO_UPDATE", "1")
            .env("CHROME_USE_NO_UPDATE_CHECK", "1")
            .env("CHROME_USE_AUTO_UPGRADE", "0")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .current_dir(root.path());
        let child = command.spawn().expect("owned actual CLI daemon");
        // Construct guard before readiness assertions so panic always reaps Child.
        let mut result = Self {
            child,
            _root: root,
            port: 0,
        };
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            assert!(
                result.child.try_wait().unwrap().is_none(),
                "owned daemon exited"
            );
            if let Ok(port) = fs::read_to_string(sockets.join("tcp-owned.port")) {
                if let Ok(port) = port.trim().parse::<u16>() {
                    if TcpStream::connect_timeout(
                        &format!("127.0.0.1:{port}").parse().unwrap(),
                        Duration::from_millis(100),
                    )
                    .is_ok()
                    {
                        result.port = port;
                        break;
                    }
                }
            }
            assert!(Instant::now() < deadline, "owned daemon readiness deadline");
            std::thread::sleep(Duration::from_millis(20));
        }
        result
    }
    fn connect(&self) -> TcpStream {
        TcpStream::connect(("127.0.0.1", self.port)).unwrap()
    }
    fn stop(&mut self) {
        if self.child.try_wait().unwrap().is_none() {
            self.child.kill().expect("kill held owned child");
        }
        self.child.wait().expect("reap held owned child");
    }
}
impl Drop for OwnedDaemon {
    fn drop(&mut self) {
        self.stop();
    }
}

#[test]
fn actual_root_std_and_tokio_sockets_bind_both_kernel_owners() {
    let mut daemon = OwnedDaemon::start();
    let socket = daemon.connect();
    let proof = owner::capture_std(&socket).unwrap();
    let (local, local_birth, peer, peer_birth) = proof.fixture_owners();
    assert_eq!(local, std::process::id());
    assert_eq!(peer, daemon.child.id());
    assert!(local_birth > 0 && peer_birth > 0);
    proof.revalidate_std(&socket).unwrap();
    drop(proof);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let socket = tokio::net::TcpStream::connect(("127.0.0.1", daemon.port))
            .await
            .unwrap();
        let proof = owner::capture_tokio(&socket).unwrap();
        assert_eq!(proof.fixture_owners().2, daemon.child.id());
        proof.revalidate_tokio(&socket).unwrap();
        let (read, write) = socket.into_split();
        proof.revalidate_rows().unwrap();
        drop(write);
        drop(read);
        assert!(proof.revalidate_rows().is_err());
    });
    daemon.stop();
    assert!(owner::capture_std(&socket).is_err());
}

#[test]
fn oriented_selected_hint_must_point_to_the_same_held_root_daemon() {
    let mut daemon = OwnedDaemon::start();
    let mut other_daemon = OwnedDaemon::start();
    let root_socket = daemon.connect();
    let root = owner::capture_std(&root_socket).unwrap();
    // The daemon's accepted endpoint belongs to that actual CLI process. The
    // other endpoint belongs to this owned tool test process. This exercises the
    // ownership primitive only, not selected CDP routing/native-host ancestry.
    let second_socket = daemon.connect();
    let hint = owner::SelectedConnectionHint::from_untrusted_endpoints(
        second_socket.peer_addr().unwrap(),
        second_socket.local_addr().unwrap(),
    )
    .unwrap();
    let selected = owner::selected_from_daemon_hint(&root, hint).unwrap();
    assert_eq!(selected.fixture_host().0, std::process::id());
    selected.revalidate_std(&root_socket).unwrap();
    let reversed = owner::SelectedConnectionHint::from_untrusted_endpoints(
        second_socket.local_addr().unwrap(),
        second_socket.peer_addr().unwrap(),
    )
    .unwrap();
    assert!(matches!(
        owner::selected_from_daemon_hint(&root, reversed),
        Err(owner::Error::DaemonOwnerMismatch)
    ));
    let borrowed_socket = other_daemon.connect();
    let borrowed = owner::SelectedConnectionHint::from_untrusted_endpoints(
        borrowed_socket.peer_addr().unwrap(),
        borrowed_socket.local_addr().unwrap(),
    )
    .unwrap();
    assert!(matches!(
        owner::selected_from_daemon_hint(&root, borrowed),
        Err(owner::Error::DaemonOwnerMismatch)
    ));
    let same_root = owner::SelectedConnectionHint::from_untrusted_endpoints(
        root_socket.peer_addr().unwrap(),
        root_socket.local_addr().unwrap(),
    )
    .unwrap();
    assert!(matches!(
        owner::selected_from_daemon_hint(&root, same_root),
        Err(owner::Error::SameAsRootConnection)
    ));
    second_socket.shutdown(Shutdown::Both).unwrap();
    assert!(
        selected.revalidate_std(&root_socket).is_err(),
        "closed/closing tuple cannot remain an ESTABLISHED owner proof"
    );
    assert_eq!(
        root.revalidate_std(&borrowed_socket),
        Err(owner::Error::SocketChanged)
    );
    drop(selected);
    drop(root);
    daemon.stop();
    other_daemon.stop();
}

#[test]
fn a_listener_port_alone_and_an_unconnected_hint_never_prove_owner() {
    let mut daemon = OwnedDaemon::start();
    let root_socket = daemon.connect();
    let root = owner::capture_std(&root_socket).unwrap();
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let hint = owner::SelectedConnectionHint::from_untrusted_endpoints(
        root_socket.peer_addr().unwrap(),
        listener.local_addr().unwrap(),
    )
    .unwrap();
    assert!(matches!(
        owner::selected_from_daemon_hint(&root, hint),
        Err(owner::Error::MissingDirection)
    ));
    // A nonloopback literal or IPv6 hint is refused before any table/process use.
    for (a, b) in [
        ("127.0.0.2:111", "127.0.0.1:222"),
        ("[::1]:111", "[::1]:222"),
        ("192.168.0.190:111", "127.0.0.1:222"),
    ] {
        assert!(matches!(
            owner::SelectedConnectionHint::from_untrusted_endpoints(
                a.parse().unwrap(),
                b.parse().unwrap()
            ),
            Err(owner::Error::UnsupportedEndpoint)
        ));
    }
    daemon.stop();
    assert!(root.revalidate_std(&root_socket).is_err());
}

#[cfg(feature = "windows-build-identity-prototype")]
#[test]
fn tcp_owner_can_feed_separate_expected_artifact_check_without_a_pid_argument() {
    let mut daemon = OwnedDaemon::start();
    let socket = daemon.connect();
    let root = owner::capture_std(&socket).unwrap();
    let expected = windows_build_identity::fixture::expected(
        &binary(),
        windows_build_identity::compiled_marker(),
    )
    .unwrap();
    let peer = windows_build_identity::HeldPeer::hold(root.peer_for_metadata().unwrap()).unwrap();
    let marker = windows_build_identity::check(expected, peer).unwrap();
    root.revalidate_std(&socket).unwrap();
    marker.revalidate().unwrap();
    drop(marker);
    drop(root);
    daemon.stop();
}

#[test]
fn short_lived_proofs_release_only_their_owned_process_handles() {
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetProcessHandleCount};
    fn count() -> u32 {
        let mut value = 0;
        // SAFETY: borrowed current-process pseudo-handle, writable scalar.
        assert_ne!(
            unsafe { GetProcessHandleCount(GetCurrentProcess(), &mut value) },
            0
        );
        value
    }
    let mut daemon = OwnedDaemon::start();
    let socket = daemon.connect();
    let warm = owner::capture_std(&socket).unwrap();
    drop(warm);
    let before = count();
    for _ in 0..32 {
        let root = owner::capture_std(&socket).unwrap();
        root.revalidate_std(&socket).unwrap();
        drop(root);
    }
    assert_eq!(count(), before, "run this metadata suite serially");
    daemon.stop();
}

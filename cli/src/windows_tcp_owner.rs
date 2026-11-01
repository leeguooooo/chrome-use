//! Exact local IPv4 TCP owner observation, private and read-only.
//!
//! GetExtendedTcpTable establishes observed process ownership, not daemon/native
//! host role, loaded artifact, Chrome ancestry, selected target or permission.
//! Root callers must retain this proof and separately validate their own compiled
//! artifact/generation, principal, selected connection generation and role.
//! This module never reads a process image, memory, Chrome, or caller PID input.
#![allow(dead_code)]

use std::fmt;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};

const ROW_BYTES: usize = 24;
const HEADER_BYTES: usize = 4;
const MAX_TABLE_BYTES: usize = 4 * 1024 * 1024;
const MAX_ROWS: usize = 131_072;
const ESTABLISHED: u32 = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Error {
    UnsupportedEndpoint,
    MalformedTable(&'static str),
    TableTooLarge,
    TableUnstable,
    MissingDirection,
    AmbiguousDirection,
    NotEstablished,
    InvalidOwner,
    LocalOwnerMismatch,
    DaemonOwnerMismatch,
    SameAsRootConnection,
    OwnerChanged,
    ProcessExitedOrChanged,
    SocketChanged,
    SocketQuery,
    Os { operation: &'static str, code: u32 },
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for Error {}
type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Tuple {
    local: SocketAddrV4,
    peer: SocketAddrV4,
}
impl Tuple {
    fn new(local: SocketAddr, peer: SocketAddr) -> Result<Self> {
        let (SocketAddr::V4(local), SocketAddr::V4(peer)) = (local, peer) else {
            return Err(Error::UnsupportedEndpoint);
        };
        if local.ip() != &Ipv4Addr::LOCALHOST
            || peer.ip() != &Ipv4Addr::LOCALHOST
            || local.port() == 0
            || peer.port() == 0
            || local == peer
        {
            return Err(Error::UnsupportedEndpoint);
        }
        Ok(Self { local, peer })
    }
    fn reverse(self) -> Self {
        Self {
            local: self.peer,
            peer: self.local,
        }
    }
}

/// Untrusted callback descriptor. Port/address validation confers no owner trust.
/// The producer must capture the daemon's actual selected Plain WebSocket socket
/// before split; a URL/listener/new connection is not an equivalent descriptor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct SelectedConnectionHint {
    tuple: Tuple,
}
impl SelectedConnectionHint {
    pub(super) fn from_untrusted_endpoints(daemon: SocketAddr, host: SocketAddr) -> Result<Self> {
        Ok(Self {
            tuple: Tuple::new(daemon, host)?,
        })
    }
    /// Only the private control codec should serialize these address facts.
    pub(super) fn endpoints(self) -> (SocketAddrV4, SocketAddrV4) {
        (self.tuple.local, self.tuple.peer)
    }
}

#[derive(Debug, Clone, Copy)]
struct Row {
    tuple: TupleFields,
    state: u32,
    pid: u32,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TupleFields {
    local: SocketAddrV4,
    peer: SocketAddrV4,
}
impl From<Tuple> for TupleFields {
    fn from(tuple: Tuple) -> Self {
        Self {
            local: tuple.local,
            peer: tuple.peer,
        }
    }
}
struct Snapshot {
    rows: Vec<Row>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct OwnerPair {
    local: u32,
    peer: u32,
}
fn word(bytes: &[u8], offset: usize) -> Result<u32> {
    let end = offset
        .checked_add(4)
        .ok_or(Error::MalformedTable("offset overflow"))?;
    Ok(u32::from_le_bytes(
        bytes
            .get(offset..end)
            .ok_or(Error::MalformedTable("truncated DWORD"))?
            .try_into()
            .map_err(|_| Error::MalformedTable("DWORD length"))?,
    ))
}
impl Snapshot {
    /// Windows x64 little-endian MIB_TCPTABLE_OWNER_PID: DWORD count followed by
    /// repr(C) six-DWORD rows. No cast/dereference of caller-controlled alignment.
    fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > MAX_TABLE_BYTES {
            return Err(Error::TableTooLarge);
        }
        let count = word(bytes, 0)? as usize;
        if count > MAX_ROWS {
            return Err(Error::TableTooLarge);
        }
        let end = count
            .checked_mul(ROW_BYTES)
            .and_then(|n| n.checked_add(HEADER_BYTES))
            .ok_or(Error::MalformedTable("row count overflow"))?;
        if end > bytes.len() {
            return Err(Error::MalformedTable("truncated row table"));
        }
        let mut rows = Vec::with_capacity(count);
        for n in 0..count {
            let at = HEADER_BYTES + n * ROW_BYTES;
            // IP Helper's DWORD addresses contain network-order IPv4 bytes. On
            // supported little-endian Windows, raw memory stores those octets in
            // order. Ports use only the low 16-bit DWORD half in network order;
            // unused high halves are intentionally ignored, not presumed zero.
            let local_ip =
                Ipv4Addr::new(bytes[at + 4], bytes[at + 5], bytes[at + 6], bytes[at + 7]);
            let peer_ip = Ipv4Addr::new(
                bytes[at + 12],
                bytes[at + 13],
                bytes[at + 14],
                bytes[at + 15],
            );
            let local_port = u16::from_be_bytes([bytes[at + 8], bytes[at + 9]]);
            let peer_port = u16::from_be_bytes([bytes[at + 16], bytes[at + 17]]);
            rows.push(Row {
                tuple: TupleFields {
                    local: SocketAddrV4::new(local_ip, local_port),
                    peer: SocketAddrV4::new(peer_ip, peer_port),
                },
                state: word(bytes, at)?,
                pid: word(bytes, at + 20)?,
            });
        }
        Ok(Self { rows })
    }
    fn direction(&self, tuple: Tuple) -> Result<u32> {
        let mut matches = self
            .rows
            .iter()
            .filter(|row| row.tuple == TupleFields::from(tuple));
        let row = matches.next().ok_or(Error::MissingDirection)?;
        // Ambiguity is rejected across all states; do not choose one ESTABLISHED
        // row while ignoring a second same-tuple record in another TCP state.
        if matches.next().is_some() {
            return Err(Error::AmbiguousDirection);
        }
        if row.state != ESTABLISHED {
            return Err(Error::NotEstablished);
        }
        if row.pid == 0 {
            return Err(Error::InvalidOwner);
        }
        Ok(row.pid)
    }
    fn pair(&self, tuple: Tuple) -> Result<OwnerPair> {
        Ok(OwnerPair {
            local: self.direction(tuple)?,
            peer: self.direction(tuple.reverse())?,
        })
    }
}

#[cfg(all(windows, target_arch = "x86_64"))]
mod windows {
    use super::*;
    use std::ffi::c_void;
    use std::mem::{align_of, size_of, zeroed};
    use windows_sys::Win32::Foundation::{
        CloseHandle, GetLastError, ERROR_INSUFFICIENT_BUFFER, ERROR_SUCCESS, FILETIME, HANDLE,
        WAIT_TIMEOUT,
    };
    use windows_sys::Win32::NetworkManagement::IpHelper::{
        GetExtendedTcpTable, MIB_TCPROW_OWNER_PID, MIB_TCPTABLE_OWNER_PID, MIB_TCP_STATE_ESTAB,
        TCP_TABLE_OWNER_PID_ALL,
    };
    use windows_sys::Win32::Networking::WinSock::AF_INET;
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcessId, GetProcessTimes, OpenProcess, WaitForSingleObject,
        PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
    };

    const _: () = {
        assert!(size_of::<MIB_TCPROW_OWNER_PID>() == ROW_BYTES);
        assert!(size_of::<MIB_TCPTABLE_OWNER_PID>() == HEADER_BYTES + ROW_BYTES);
        assert!(align_of::<MIB_TCPTABLE_OWNER_PID>() <= align_of::<u64>());
        assert!(MIB_TCP_STATE_ESTAB as u32 == ESTABLISHED);
    };
    fn os(operation: &'static str) -> Error {
        // SAFETY: observe thread-local error immediately after failed Win32 call.
        Error::Os {
            operation,
            code: unsafe { GetLastError() },
        }
    }
    fn query_table() -> Result<Snapshot> {
        let mut requested = 0u32;
        // SAFETY: documented size query; no data buffer, writable byte-count scalar.
        let status = unsafe {
            GetExtendedTcpTable(
                std::ptr::null_mut(),
                &mut requested,
                0,
                u32::from(AF_INET),
                TCP_TABLE_OWNER_PID_ALL,
                0,
            )
        };
        if status == ERROR_SUCCESS {
            return Err(Error::MalformedTable("size query returned no table"));
        }
        if status != ERROR_INSUFFICIENT_BUFFER {
            return Err(Error::Os {
                operation: "GetExtendedTcpTable size",
                code: status,
            });
        }
        // Churn may grow the table between size/data calls. Four bounded attempts
        // allocate initialized aligned storage, never an unbounded retry or sleep.
        for _ in 0..4 {
            if requested < HEADER_BYTES as u32 || requested as usize > MAX_TABLE_BYTES {
                return Err(Error::TableTooLarge);
            }
            let words = (requested as usize).div_ceil(size_of::<u64>());
            let capacity = words
                .checked_mul(size_of::<u64>())
                .filter(|n| *n <= MAX_TABLE_BYTES)
                .ok_or(Error::TableTooLarge)?;
            let mut storage = vec![0u64; words];
            let mut written = capacity as u32;
            // SAFETY: u64 storage is sufficiently aligned, initialized and at least
            // written bytes. The API cannot outlive this allocation.
            let status = unsafe {
                GetExtendedTcpTable(
                    storage.as_mut_ptr().cast::<c_void>(),
                    &mut written,
                    0,
                    u32::from(AF_INET),
                    TCP_TABLE_OWNER_PID_ALL,
                    0,
                )
            };
            if status == ERROR_INSUFFICIENT_BUFFER {
                if written as usize <= capacity {
                    return Err(Error::TableUnstable);
                }
                requested = written;
                continue;
            }
            if status != ERROR_SUCCESS {
                return Err(Error::Os {
                    operation: "GetExtendedTcpTable data",
                    code: status,
                });
            }
            if written as usize > capacity || written < HEADER_BYTES as u32 {
                return Err(Error::MalformedTable("API byte count"));
            }
            // SAFETY: every byte in this prefix belongs to initialized storage;
            // parser independently checks count/row spans, never casts this slice.
            let bytes = unsafe {
                std::slice::from_raw_parts(storage.as_ptr().cast::<u8>(), written as usize)
            };
            return Snapshot::parse(bytes);
        }
        Err(Error::TableUnstable)
    }
    struct HeldProcess {
        handle: HANDLE,
        pid: u32,
        birth: u64,
    }
    impl Drop for HeldProcess {
        fn drop(&mut self) {
            // SAFETY: sole owner of a successful OpenProcess handle, never borrowed.
            unsafe {
                CloseHandle(self.handle);
            }
        }
    }
    fn creation(handle: HANDLE) -> Result<u64> {
        let (mut created, mut exit, mut kernel, mut user): (
            FILETIME,
            FILETIME,
            FILETIME,
            FILETIME,
        ) = unsafe { zeroed() };
        // SAFETY: query-capable held handle and distinct writable output records.
        if unsafe { GetProcessTimes(handle, &mut created, &mut exit, &mut kernel, &mut user) } == 0
        {
            return Err(os("GetProcessTimes"));
        }
        let birth = (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime);
        if birth == 0 {
            return Err(Error::ProcessExitedOrChanged);
        }
        Ok(birth)
    }
    impl HeldProcess {
        fn from_observed_row(pid: u32) -> Result<Self> {
            if pid == 0 {
                return Err(Error::InvalidOwner);
            }
            // SAFETY: PID comes only from an exact kernel TCP row. Read-only query
            // and synchronization rights, no VM_READ, token/write/thread rights.
            let handle = unsafe {
                OpenProcess(
                    PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
                    0,
                    pid,
                )
            };
            if handle == 0 {
                return Err(os("OpenProcess TCP owner"));
            }
            // Construct owner before any fallible query so every path closes it.
            let mut process = Self {
                handle,
                pid,
                birth: 0,
            };
            process.birth = creation(handle)?;
            process.require_alive()?;
            Ok(process)
        }
        fn require_alive(&self) -> Result<()> {
            // SAFETY: held SYNCHRONIZE/query handle; zero wait never blocks.
            if unsafe { WaitForSingleObject(self.handle, 0) } != WAIT_TIMEOUT
                || creation(self.handle)? != self.birth
            {
                return Err(Error::ProcessExitedOrChanged);
            }
            Ok(())
        }
        fn require_row(&self, pid: u32) -> Result<()> {
            if pid != self.pid {
                return Err(Error::OwnerChanged);
            }
            self.require_alive()
        }
    }
    enum BorrowedSocket<'a> {
        Std(&'a std::net::TcpStream),
        Tokio(&'a tokio::net::TcpStream),
    }
    impl BorrowedSocket<'_> {
        fn endpoints(&self) -> Result<Tuple> {
            let (local, peer) = match self {
                Self::Std(socket) => (socket.local_addr(), socket.peer_addr()),
                Self::Tokio(socket) => (socket.local_addr(), socket.peer_addr()),
            };
            Tuple::new(
                local.map_err(|_| Error::SocketQuery)?,
                peer.map_err(|_| Error::SocketQuery)?,
            )
        }
    }
    /// Owns captured endpoint/process facts, not the socket. This permits moving
    /// the actual WebSocket into split halves while CdpClient owns this record.
    /// Caller must retain the actual transport and its generation/lifetime guard;
    /// table rows do not expose a stable TCP connection-incarnation identifier.
    pub(crate) struct LocalConnection {
        tuple: Tuple,
        local: HeldProcess,
        peer: HeldProcess,
    }
    fn capture(socket: BorrowedSocket<'_>) -> Result<LocalConnection> {
        let tuple = socket.endpoints()?;
        let pair = query_table()?.pair(tuple)?;
        // SAFETY: scalar process identity query, no borrowed handle ownership.
        if pair.local != unsafe { GetCurrentProcessId() } {
            return Err(Error::LocalOwnerMismatch);
        }
        let local = HeldProcess::from_observed_row(pair.local)?;
        let peer = HeldProcess::from_observed_row(pair.peer)?;
        let connection = LocalConnection { tuple, local, peer };
        if socket.endpoints()? != tuple {
            return Err(Error::SocketChanged);
        }
        connection.revalidate_rows()?; // row->open->birth->row/liveness
        if socket.endpoints()? != tuple {
            return Err(Error::SocketChanged);
        }
        Ok(connection)
    }
    pub(crate) fn capture_std(socket: &std::net::TcpStream) -> Result<LocalConnection> {
        capture(BorrowedSocket::Std(socket))
    }
    pub(crate) fn capture_tokio(socket: &tokio::net::TcpStream) -> Result<LocalConnection> {
        capture(BorrowedSocket::Tokio(socket))
    }
    impl LocalConnection {
        fn revalidate_with(&self, snapshot: &Snapshot) -> Result<()> {
            let pair = snapshot.pair(self.tuple)?;
            self.local.require_row(pair.local)?;
            self.peer.require_row(pair.peer)?;
            Ok(())
        }
        /// Refresh both kernel rows and held creation/liveness. This alone does
        /// not prove the source transport's selected generation is still active.
        pub(crate) fn revalidate_rows(&self) -> Result<()> {
            self.revalidate_with(&query_table()?)
        }
        pub(crate) fn revalidate_std(&self, socket: &std::net::TcpStream) -> Result<()> {
            self.revalidate_source(BorrowedSocket::Std(socket))
        }
        pub(crate) fn revalidate_tokio(&self, socket: &tokio::net::TcpStream) -> Result<()> {
            self.revalidate_source(BorrowedSocket::Tokio(socket))
        }
        fn revalidate_source(&self, socket: BorrowedSocket<'_>) -> Result<()> {
            if socket.endpoints()? != self.tuple {
                return Err(Error::SocketChanged);
            }
            self.revalidate_rows()?;
            if socket.endpoints()? != self.tuple {
                return Err(Error::SocketChanged);
            }
            Ok(())
        }
        /// Caller knows whether this socket is root IPC or a selected relay.
        /// This descriptor only says what socket was observed, not why it exists.
        pub(crate) fn hint(&self) -> SelectedConnectionHint {
            SelectedConnectionHint { tuple: self.tuple }
        }

        /// Bridge stays separate from matching marker/artifact verification. The
        /// recipient is derived here, never accepted from caller-supplied PID JSON.
        /// Retain this LocalConnection through image check and final revalidation.
        #[cfg(feature = "windows-build-identity-prototype")]
        pub(crate) fn peer_for_metadata(
            &self,
        ) -> Result<crate::windows_build_identity::KernelOwner> {
            self.revalidate_rows()?;
            // SAFETY: exact both-direction rows, held liveness/birth, just rechecked.
            Ok(unsafe {
                crate::windows_build_identity::KernelOwner::from_kernel_tcp_owner(
                    self.peer.pid,
                    self.peer.birth,
                )
            })
        }
        #[cfg(test)]
        pub(crate) fn fixture_owners(&self) -> (u32, u64, u32, u64) {
            (
                self.local.pid,
                self.local.birth,
                self.peer.pid,
                self.peer.birth,
            )
        }
    }

    /// The root connection pins the daemon identity; the hinted connection must
    /// have that exact daemon at its oriented local endpoint. No local URL/port
    /// inference, process-name search, listener lookup or role claim occurs.
    pub(crate) struct SelectedConnection<'root> {
        root: &'root LocalConnection,
        tuple: Tuple,
        host: HeldProcess,
    }
    pub(crate) fn selected_from_daemon_hint<'root>(
        root: &'root LocalConnection,
        hint: SelectedConnectionHint,
    ) -> Result<SelectedConnection<'root>> {
        if hint.tuple == root.tuple || hint.tuple == root.tuple.reverse() {
            return Err(Error::SameAsRootConnection);
        }
        let snapshot = query_table()?;
        root.revalidate_with(&snapshot)?;
        let pair = snapshot.pair(hint.tuple)?;
        if pair.local != root.peer.pid {
            return Err(Error::DaemonOwnerMismatch);
        }
        // Only matched native-host endpoint is opened, after the daemon endpoint
        // is tied to the retained root peer. This still is not host/ancestry proof.
        let host = HeldProcess::from_observed_row(pair.peer)?;
        let selected = SelectedConnection {
            root,
            tuple: hint.tuple,
            host,
        };
        selected.revalidate_rows()?;
        Ok(selected)
    }
    impl SelectedConnection<'_> {
        /// Kernel facts only: caller separately revalidates actual root socket
        /// and the selected CdpClient's still-active connection generation.
        pub(crate) fn revalidate_rows(&self) -> Result<()> {
            let snapshot = query_table()?;
            self.root.revalidate_with(&snapshot)?;
            let pair = snapshot.pair(self.tuple)?;
            self.root.peer.require_row(pair.local)?;
            self.host.require_row(pair.peer)?;
            Ok(())
        }
        /// Final CLI-side check combines the actual root socket endpoints, both
        /// root directions and both selected directions in one fresh snapshot.
        pub(crate) fn revalidate_std(&self, socket: &std::net::TcpStream) -> Result<()> {
            self.revalidate_source(BorrowedSocket::Std(socket))
        }
        pub(crate) fn revalidate_tokio(&self, socket: &tokio::net::TcpStream) -> Result<()> {
            self.revalidate_source(BorrowedSocket::Tokio(socket))
        }
        fn revalidate_source(&self, socket: BorrowedSocket<'_>) -> Result<()> {
            if socket.endpoints()? != self.root.tuple {
                return Err(Error::SocketChanged);
            }
            self.revalidate_rows()?;
            if socket.endpoints()? != self.root.tuple {
                return Err(Error::SocketChanged);
            }
            Ok(())
        }
        #[cfg(feature = "windows-build-identity-prototype")]
        pub(crate) fn host_for_metadata(
            &self,
        ) -> Result<crate::windows_build_identity::KernelOwner> {
            self.revalidate_rows()?;
            // SAFETY: this exact selected tuple's host is held and just revalidated.
            Ok(unsafe {
                crate::windows_build_identity::KernelOwner::from_kernel_tcp_owner(
                    self.host.pid,
                    self.host.birth,
                )
            })
        }
        #[cfg(test)]
        pub(crate) fn fixture_host(&self) -> (u32, u64) {
            (self.host.pid, self.host.birth)
        }
    }
}

#[cfg(all(windows, target_arch = "x86_64"))]
pub(super) use windows::{
    capture_std, capture_tokio, selected_from_daemon_hint, LocalConnection, SelectedConnection,
};

#[cfg(test)]
mod parser_tests {
    use super::*;
    fn tuple(a: u16, b: u16) -> Tuple {
        Tuple::new(
            SocketAddrV4::new(Ipv4Addr::LOCALHOST, a).into(),
            SocketAddrV4::new(Ipv4Addr::LOCALHOST, b).into(),
        )
        .unwrap()
    }
    fn table(rows: &[(Tuple, u32, u32)]) -> Vec<u8> {
        let mut bytes = (rows.len() as u32).to_le_bytes().to_vec();
        for (tuple, state, pid) in rows {
            bytes.extend(state.to_le_bytes());
            bytes.extend(tuple.local.ip().octets());
            bytes.extend(tuple.local.port().to_be_bytes());
            bytes.extend([0x91, 0xf7]);
            bytes.extend(tuple.peer.ip().octets());
            bytes.extend(tuple.peer.port().to_be_bytes());
            bytes.extend([0x12, 0x34]);
            bytes.extend(pid.to_le_bytes());
        }
        bytes
    }
    #[test]
    fn exact_both_direction_and_network_byte_order_with_unused_port_bits() {
        let a = tuple(80, 55331);
        let b = tuple(80, 55332);
        let bytes = table(&[
            (b, 5, 7),
            (a.reverse(), 5, 202),
            (a, 5, 101),
            (b.reverse(), 5, 303),
        ]);
        let parsed = Snapshot::parse(&bytes).unwrap();
        assert_eq!(
            parsed.pair(a).unwrap(),
            OwnerPair {
                local: 101,
                peer: 202
            }
        );
        assert_eq!(
            parsed.pair(b).unwrap(),
            OwnerPair {
                local: 7,
                peer: 303
            }
        );
        for n in 0..bytes.len() {
            assert!(Snapshot::parse(&bytes[..n]).is_err(), "truncation {n}");
        }
    }
    #[test]
    fn no_listener_reverse_missing_stale_or_ambiguous_acceptance() {
        let a = tuple(41903, 49501);
        assert_eq!(
            Snapshot::parse(&table(&[(a, 5, 12)])).unwrap().pair(a),
            Err(Error::MissingDirection)
        );
        for state in [1, 2, 3, 4, 6, 7, 8, 9, 10, 11, 12] {
            assert_eq!(
                Snapshot::parse(&table(&[(a, 5, 12), (a.reverse(), state, 13)]))
                    .unwrap()
                    .pair(a),
                Err(Error::NotEstablished)
            );
        }
        for duplicate in [5, 8] {
            assert_eq!(
                Snapshot::parse(&table(&[
                    (a, 5, 12),
                    (a, duplicate, 999),
                    (a.reverse(), 5, 13)
                ]))
                .unwrap()
                .pair(a),
                Err(Error::AmbiguousDirection)
            );
        }
        assert_eq!(
            Snapshot::parse(&table(&[(a, 5, 0), (a.reverse(), 5, 13)]))
                .unwrap()
                .pair(a),
            Err(Error::InvalidOwner)
        );
    }
    #[test]
    fn strict_endpoint_and_table_bounds() {
        for (a, b) in [
            ("127.0.0.2:1", "127.0.0.1:2"),
            ("127.0.0.1:0", "127.0.0.1:2"),
            ("[::1]:1", "[::1]:2"),
            ("192.168.0.1:1", "127.0.0.1:2"),
            ("127.0.0.1:1", "127.0.0.1:1"),
        ] {
            assert_eq!(
                Tuple::new(a.parse().unwrap(), b.parse().unwrap()),
                Err(Error::UnsupportedEndpoint)
            );
        }
        assert!(Snapshot::parse(&u32::MAX.to_le_bytes()).is_err());
        assert!(Snapshot::parse(&vec![0; MAX_TABLE_BYTES + 1]).is_err());
        assert!(Snapshot::parse(&[0; 4]).unwrap().rows.is_empty());
        for n in 0..512 {
            let bytes = vec![n as u8; n];
            let _ = Snapshot::parse(&bytes);
        }
    }
}

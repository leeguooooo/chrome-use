//! Feature-private, read-only matching-artifact and observed-loaded-marker probe.
//!
//! This is not code integrity, installation identity, Authenticode, socket-owner
//! attestation, process-role proof, or a defense against same-user compromise.
//! Production must anchor ExpectedImage to its own compiled marker, derive the
//! peer from a kernel socket owner, and independently check role/ancestry/desktop.
//! No foreground, input, arbitrary memory address, or trusted-PID RPC exists here.
#![allow(dead_code)]

use std::fmt;

pub(super) const MARKER_LEN: usize = 64;
const MAGIC: &[u8; 8] = b"CUFGPE01";
const SECTION: &[u8; 8] = b".cufg\0\0\0";
const MAX_FILE: u64 = 256 * 1024 * 1024;
const MAX_HEADERS: usize = 64 * 1024;
const MAX_IMAGE: u32 = 512 * 1024 * 1024;
const MAX_SECTIONS: usize = 96;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Error {
    Io(&'static str),
    Malformed(&'static str),
    UnsupportedArchitecture,
    ProcessChanged,
    PrincipalMismatch,
    ArtifactMismatch,
    AnchorMarkerMismatch,
    LoadedLayoutMismatch,
    LoadedMarkerMismatch,
    ModuleAmbiguous,
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for Error {}
type Result<T> = std::result::Result<T, Error>;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct Marker([u8; MARKER_LEN]);
impl fmt::Debug for Marker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Marker(")?;
        for b in self.0 {
            write!(f, "{b:02x}")?;
        }
        write!(f, ")")
    }
}
impl Marker {
    pub(super) fn parse(bytes: &[u8]) -> Result<Self> {
        let bytes: [u8; MARKER_LEN] = bytes
            .try_into()
            .map_err(|_| Error::Malformed("marker size"))?;
        if &bytes[..8] != MAGIC
            || le16(&bytes, 8)? != 1
            || le16(&bytes, 10)? != 64
            || le16(&bytes, 12)? != 0x8664
            || le16(&bytes, 14)? != 1
            || bytes[16..48].iter().all(|b| *b == 0)
            || bytes[48..].iter().all(|b| *b == 0)
        {
            return Err(Error::Malformed("marker schema or generation"));
        }
        Ok(Self(bytes))
    }
    #[cfg(test)]
    pub(super) fn fixture_bytes(self) -> [u8; MARKER_LEN] {
        self.0
    }
}

/// The live black_box pointer plus volatile reads root the initialized section
/// through thin LTO and linker GC. Final GNU and MSVC PE retention is a CI gate.
#[cfg(all(windows, feature = "windows-build-identity-prototype"))]
#[used]
#[link_section = ".cufg"]
static BUILD_MARKER: [u8; MARKER_LEN] = include!(concat!(env!("OUT_DIR"), "/cufg_marker.rs"));

#[cfg(all(windows, feature = "windows-build-identity-prototype"))]
pub(super) fn compiled_marker() -> Marker {
    let source = std::hint::black_box(std::ptr::addr_of!(BUILD_MARKER).cast::<u8>());
    let mut bytes = [0; MARKER_LEN];
    for (offset, destination) in bytes.iter_mut().enumerate() {
        // SAFETY: source points to our live fixed initialized static, offset < 64.
        *destination = unsafe { std::ptr::read_volatile(source.add(offset)) };
    }
    Marker::parse(&bytes).expect("build script emitted an invalid CUFG marker")
}

#[derive(Debug, Clone)]
struct Layout {
    headers: Vec<u8>,
    image_size: u32,
    marker_rva: u32,
    marker_offset: u64,
    marker: Marker,
}
fn le16(data: &[u8], offset: usize) -> Result<u16> {
    let end = offset
        .checked_add(2)
        .ok_or(Error::Malformed("offset overflow"))?;
    let b = data
        .get(offset..end)
        .ok_or(Error::Malformed("truncated u16"))?;
    Ok(u16::from_le_bytes([b[0], b[1]]))
}
fn le32(data: &[u8], offset: usize) -> Result<u32> {
    let end = offset
        .checked_add(4)
        .ok_or(Error::Malformed("offset overflow"))?;
    let b = data
        .get(offset..end)
        .ok_or(Error::Malformed("truncated u32"))?;
    Ok(u32::from_le_bytes(
        b.try_into().map_err(|_| Error::Malformed("u32"))?,
    ))
}
fn bounded_span(offset: u64, length: u64, limit: u64) -> Result<std::ops::Range<usize>> {
    let end = offset
        .checked_add(length)
        .filter(|end| *end <= limit)
        .ok_or(Error::Malformed("span outside bounds"))?;
    Ok(
        usize::try_from(offset).map_err(|_| Error::Malformed("offset width"))?
            ..usize::try_from(end).map_err(|_| Error::Malformed("end width"))?,
    )
}
fn overlaps(a: &(u64, u64), b: &(u64, u64)) -> bool {
    a.0 < b.1 && b.0 < a.1
}

/// GNU and MSVC fixtures both expose the actual module base in the loaded
/// PE32+ ImageBase field, replacing the artifact's preferred address. Accept
/// only that exact observed value (or the unchanged preferred address); every
/// other header byte, including all section RVAs and sizes, must still match.
fn matching_headers(expected: &[u8], observed: &[u8], actual_base: u64) -> bool {
    if expected.len() != observed.len() {
        return false;
    }
    let Ok(pe) = le32(expected, 0x3c) else {
        return false;
    };
    let Some(start) = (pe as usize).checked_add(48) else {
        return false;
    };
    let Some(end) = start.checked_add(8) else {
        return false;
    };
    let (Some(preferred), Some(loaded)) = (expected.get(start..end), observed.get(start..end))
    else {
        return false;
    };
    (loaded == preferred || loaded == actual_base.to_le_bytes())
        && expected[..start] == observed[..start]
        && expected[end..] == observed[end..]
}

/// Pure parser: no pointer dereference and no Windows calls. The complete file is
/// capped before allocation by PinnedFile; unit tests can supply arbitrary bytes.
fn parse_image(data: &[u8]) -> Result<Layout> {
    if data.len() < 64 || data.len() as u64 > MAX_FILE || &data[..2] != b"MZ" {
        return Err(Error::Malformed("DOS header/file size"));
    }
    let pe = le32(data, 0x3c)? as usize;
    if pe < 64 || pe > MAX_HEADERS - 24 {
        return Err(Error::Malformed("PE offset"));
    }
    let coff_end = pe
        .checked_add(24)
        .ok_or(Error::Malformed("COFF overflow"))?;
    if data.get(pe..pe + 4) != Some(b"PE\0\0")
        || coff_end > data.len()
        || le16(data, pe + 4)? != 0x8664
    {
        return Err(Error::Malformed("PE signature/machine"));
    }
    let count = le16(data, pe + 6)? as usize;
    let characteristics = le16(data, pe + 22)?;
    if count == 0
        || count > MAX_SECTIONS
        || le16(data, pe + 20)? != 240
        || characteristics & 0x0002 == 0
        || characteristics & 0x2000 != 0
    {
        return Err(Error::Malformed("COFF sections/optional header/EXE"));
    }
    let opt = coff_end;
    let table = opt
        .checked_add(240)
        .ok_or(Error::Malformed("optional overflow"))?;
    let table_end = table
        .checked_add(
            count
                .checked_mul(40)
                .ok_or(Error::Malformed("section count"))?,
        )
        .ok_or(Error::Malformed("table overflow"))?;
    if table_end > MAX_HEADERS || table_end > data.len() || le16(data, opt)? != 0x20b {
        return Err(Error::Malformed("optional/table bounds"));
    }
    let image_size = le32(data, opt + 56)?;
    let header_size = le32(data, opt + 60)? as usize;
    let section_alignment = le32(data, opt + 32)?;
    let file_alignment = le32(data, opt + 36)?;
    if image_size == 0
        || image_size > MAX_IMAGE
        || header_size < table_end
        || header_size > MAX_HEADERS
        || header_size > data.len()
        || !(512..=65536).contains(&file_alignment)
        || !file_alignment.is_power_of_two()
        || !section_alignment.is_power_of_two()
        || section_alignment < file_alignment
        || image_size % section_alignment != 0
        || header_size as u32 % file_alignment != 0
    {
        return Err(Error::Malformed("PE alignment/size"));
    }
    let mut raw_spans = Vec::new();
    let mut virtual_spans = Vec::new();
    let mut marker_section = None;
    for n in 0..count {
        let at = table + n * 40;
        let virtual_size = le32(data, at + 8)?;
        let rva = le32(data, at + 12)?;
        let raw_size = le32(data, at + 16)?;
        let raw = le32(data, at + 20)?;
        let flags = le32(data, at + 36)?;
        if rva % section_alignment != 0 || rva < header_size as u32 {
            return Err(Error::Malformed("section RVA alignment"));
        }
        let virtual_len = u64::from(virtual_size.max(raw_size));
        let virtual_range = bounded_span(rva.into(), virtual_len, image_size.into())?;
        if virtual_len != 0 {
            let span = (virtual_range.start as u64, virtual_range.end as u64);
            if virtual_spans.iter().any(|s| overlaps(s, &span)) {
                return Err(Error::Malformed("overlapping RVA"));
            }
            virtual_spans.push(span);
        }
        if raw_size != 0 {
            if raw < header_size as u32
                || raw % file_alignment != 0
                || raw_size % file_alignment != 0
            {
                return Err(Error::Malformed("section raw alignment"));
            }
            let range = bounded_span(raw.into(), raw_size.into(), data.len() as u64)?;
            let span = (range.start as u64, range.end as u64);
            if raw_spans.iter().any(|s| overlaps(s, &span)) {
                return Err(Error::Malformed("overlapping file sections"));
            }
            raw_spans.push(span);
        }
        if data.get(at..at + 8) == Some(SECTION) {
            // READ + INITIALIZED_DATA, never WRITE/EXECUTE/DISCARDABLE/UNINITIALIZED.
            if marker_section.is_some()
                || virtual_size != MARKER_LEN as u32
                || raw_size < MARKER_LEN as u32
                || raw_size > 65536
                || flags & 0x4000_0040 != 0x4000_0040
                || flags & 0xa200_00a0 != 0
            {
                return Err(Error::Malformed("marker section count/size/protection"));
            }
            let range = bounded_span(raw.into(), MARKER_LEN as u64, data.len() as u64)?;
            let marker = Marker::parse(&data[range.clone()])?;
            let padding_end = bounded_span(raw.into(), raw_size.into(), data.len() as u64)?.end;
            if data[range.end..padding_end].iter().any(|b| *b != 0) {
                return Err(Error::Malformed("nonzero marker section padding"));
            }
            bounded_span(rva.into(), MARKER_LEN as u64, image_size.into())?;
            marker_section = Some((rva, u64::from(raw), marker));
        }
    }
    let (marker_rva, marker_offset, marker) =
        marker_section.ok_or(Error::Malformed("missing marker"))?;
    Ok(Layout {
        headers: data[..header_size].to_vec(),
        image_size,
        marker_rva,
        marker_offset,
        marker,
    })
}

#[cfg(all(windows, target_arch = "x86_64"))]
mod windows {
    use super::*;
    use sha2::{Digest, Sha256};
    use std::ffi::{c_void, OsString};
    use std::fs::{File, OpenOptions};
    use std::io::Read;
    use std::mem::{size_of, size_of_val, zeroed};
    use std::os::windows::ffi::OsStringExt;
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use std::path::{Path, PathBuf};
    use windows_sys::Win32::Foundation::{CloseHandle, FILETIME, HANDLE, HMODULE, WAIT_TIMEOUT};
    use windows_sys::Win32::Security::{
        EqualSid, GetTokenInformation, TokenSessionId, TokenStatistics, TokenUser, TOKEN_QUERY,
        TOKEN_STATISTICS, TOKEN_USER,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        FileIdInfo, GetFileInformationByHandleEx, GetFinalPathNameByHandleW, FILE_ID_INFO,
        FILE_SHARE_READ,
    };
    use windows_sys::Win32::System::Diagnostics::Debug::ReadProcessMemory;
    use windows_sys::Win32::System::Memory::{
        VirtualQueryEx, MEMORY_BASIC_INFORMATION, MEM_COMMIT, MEM_IMAGE, PAGE_GUARD, PAGE_NOACCESS,
        PAGE_READONLY,
    };
    use windows_sys::Win32::System::ProcessStatus::{
        EnumProcessModulesEx, GetModuleFileNameExW, GetModuleInformation, LIST_MODULES_64BIT,
        MODULEINFO,
    };
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcess, GetProcessHandleCount, GetProcessTimes, IsWow64Process2, OpenProcess,
        OpenProcessToken, QueryFullProcessImageNameW, WaitForSingleObject,
        PROCESS_QUERY_INFORMATION, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
        PROCESS_VM_READ,
    };

    struct OwnedHandle(HANDLE);
    impl Drop for OwnedHandle {
        fn drop(&mut self) {
            // SAFETY: constructed only from owned, successfully opened handles.
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
    fn birth(handle: HANDLE) -> Result<u64> {
        // SAFETY: valid process handle, distinct initialized output records.
        let (mut created, mut exit, mut kernel, mut user): (
            FILETIME,
            FILETIME,
            FILETIME,
            FILETIME,
        ) = unsafe { zeroed() };
        if unsafe { GetProcessTimes(handle, &mut created, &mut exit, &mut kernel, &mut user) } == 0
        {
            return Err(Error::Io("GetProcessTimes"));
        }
        Ok((u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime))
    }
    fn alive(handle: HANDLE, expected: u64) -> Result<()> {
        // SAFETY: held process handle has SYNCHRONIZE and query rights.
        if unsafe { WaitForSingleObject(handle, 0) } != WAIT_TIMEOUT || birth(handle)? != expected {
            return Err(Error::ProcessChanged);
        }
        Ok(())
    }
    #[cfg(test)]
    std::thread_local! {
        static READ_COUNTS: std::cell::Cell<(usize, usize)> = const { std::cell::Cell::new((0, 0)) };
    }
    fn open(pid: u32, rights: u32) -> Result<OwnedHandle> {
        #[cfg(test)]
        if rights & PROCESS_VM_READ != 0 {
            READ_COUNTS.with(|count| {
                let (opens, reads) = count.get();
                count.set((opens + 1, reads));
            });
        }

        // SAFETY: PID is from a held owned child or a future sealed kernel owner.
        let handle = unsafe { OpenProcess(rights, 0, pid) };
        if handle == 0 {
            return Err(Error::Io("OpenProcess"));
        }
        Ok(OwnedHandle(handle))
    }
    fn process_path(handle: HANDLE) -> Result<PathBuf> {
        let mut buffer = vec![0u16; 32768];
        let mut length = buffer.len() as u32;
        // SAFETY: held query-capable process and writable UTF-16 buffer.
        if unsafe { QueryFullProcessImageNameW(handle, 0, buffer.as_mut_ptr(), &mut length) } == 0
            || length == 0
            || length as usize >= buffer.len()
        {
            return Err(Error::Io("QueryFullProcessImageNameW/truncation"));
        }
        Ok(PathBuf::from(OsString::from_wide(
            &buffer[..length as usize],
        )))
    }
    fn final_path(file: &File) -> Result<PathBuf> {
        let mut buffer = vec![0u16; 32768];
        // SAFETY: owned open file, writable UTF-16 buffer; VOLUME_NAME_DOS/normalized.
        let length = unsafe {
            GetFinalPathNameByHandleW(
                file.as_raw_handle() as HANDLE,
                buffer.as_mut_ptr(),
                buffer.len() as u32,
                0,
            )
        };
        if length == 0 || length as usize >= buffer.len() {
            return Err(Error::Io("GetFinalPathNameByHandleW/truncation"));
        }
        Ok(PathBuf::from(OsString::from_wide(
            &buffer[..length as usize],
        )))
    }
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) struct FileIdentity {
        pub(crate) volume: u64,
        pub(crate) id: [u8; 16],
    }
    struct PinnedFile {
        file: File,
        identity: FileIdentity,
        digest: [u8; 32],
        layout: Layout,
    }
    impl PinnedFile {
        fn open(path: &Path) -> Result<Self> {
            let mut file = OpenOptions::new()
                .read(true)
                .share_mode(FILE_SHARE_READ)
                .open(path)
                .map_err(|_| Error::Io("pin file: no write/delete sharing"))?;
            let length = file
                .metadata()
                .map_err(|_| Error::Io("file metadata"))?
                .len();
            if !(64..=MAX_FILE).contains(&length) {
                return Err(Error::Malformed("pinned file size"));
            }
            // SAFETY: info is writable and exactly FILE_ID_INFO size.
            let mut info: FILE_ID_INFO = unsafe { zeroed() };
            if unsafe {
                GetFileInformationByHandleEx(
                    file.as_raw_handle() as HANDLE,
                    FileIdInfo,
                    (&mut info as *mut FILE_ID_INFO).cast(),
                    size_of::<FILE_ID_INFO>() as u32,
                )
            } == 0
            {
                return Err(Error::Io("FileIdInfo"));
            }
            let mut data = Vec::with_capacity(length as usize);
            (&mut file)
                .take(MAX_FILE + 1)
                .read_to_end(&mut data)
                .map_err(|_| Error::Io("read pinned file"))?;
            if data.len() as u64 != length {
                return Err(Error::Malformed("pinned file length changed"));
            }
            let digest: [u8; 32] = Sha256::digest(&data).into();
            let layout = parse_image(&data)?;
            let identity = FileIdentity {
                volume: info.VolumeSerialNumber,
                id: info.FileId.Identifier,
            };
            Ok(Self {
                file,
                identity,
                digest,
                layout,
            })
        }
    }
    pub(crate) struct ExpectedImage {
        pinned: PinnedFile,
        marker: Marker,
    }
    impl ExpectedImage {
        #[cfg(feature = "windows-build-identity-prototype")]
        pub(crate) fn pin_self() -> Result<Self> {
            let path = std::env::current_exe().map_err(|_| Error::Io("current_exe"))?;
            Self::pin(&path, compiled_marker())
        }
        fn pin(path: &Path, marker: Marker) -> Result<Self> {
            let pinned = PinnedFile::open(path)?;
            if pinned.layout.marker != marker {
                return Err(Error::AnchorMarkerMismatch);
            }
            Ok(Self { pinned, marker })
        }
    }

    /// Not serializable, not constructible from a CLI flag or arbitrary PID.
    /// Future TCP attestor must revalidate its exact tuple after check() as well.
    pub(crate) struct KernelOwner {
        pid: u32,
        birth: u64,
    }
    impl KernelOwner {
        /// # Safety
        /// Caller has just kernel-attributed an exact established socket to this
        /// PID, opened/held that process and observed this creation time. This
        /// constructor is crate-internal behind the default-off feature, never
        /// command dispatch. Its unsafe caller contract is not kernel/role proof.
        pub(crate) unsafe fn from_kernel_tcp_owner(pid: u32, birth: u64) -> Self {
            Self { pid, birth }
        }
    }
    pub(crate) struct HeldPeer {
        query: OwnedHandle,
        owner: KernelOwner,
    }
    impl HeldPeer {
        pub(crate) fn hold(owner: KernelOwner) -> Result<Self> {
            let query = open(
                owner.pid,
                PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
            )?;
            alive(query.0, owner.birth)?;
            same_principal(query.0)?;
            Ok(Self { query, owner })
        }
    }

    fn token_data(token: HANDLE, class: i32) -> Result<Vec<usize>> {
        let mut required = 0u32;
        // SAFETY: first size query has no output buffer; required is writable.
        unsafe {
            GetTokenInformation(token, class, std::ptr::null_mut(), 0, &mut required);
        }
        if required == 0 || required > 16384 {
            return Err(Error::Io("token size"));
        }
        let words = (required as usize).div_ceil(size_of::<usize>());
        let mut buffer = vec![0usize; words];
        let capacity = (words * size_of::<usize>()) as u32;
        if unsafe {
            GetTokenInformation(
                token,
                class,
                buffer.as_mut_ptr().cast(),
                capacity,
                &mut required,
            )
        } == 0
            || required == 0
            || required > capacity
        {
            return Err(Error::Io("GetTokenInformation"));
        }
        Ok(buffer)
    }
    fn token(process: HANDLE) -> Result<OwnedHandle> {
        let mut result = 0;
        // SAFETY: only TOKEN_QUERY requested from a held process.
        if unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut result) } == 0 {
            return Err(Error::Io("OpenProcessToken"));
        }
        Ok(OwnedHandle(result))
    }
    fn same_principal(process: HANDLE) -> Result<()> {
        // SAFETY: GetCurrentProcess is a borrowed pseudo-handle, never wrapped/closed.
        let current = token(unsafe { GetCurrentProcess() })?;
        let peer = token(process)?;
        let our_user = token_data(current.0, TokenUser)?;
        let their_user = token_data(peer.0, TokenUser)?;
        let our_stats = token_data(current.0, TokenStatistics)?;
        let their_stats = token_data(peer.0, TokenStatistics)?;
        let our_session = token_data(current.0, TokenSessionId)?;
        let their_session = token_data(peer.0, TokenSessionId)?;
        if our_user.len() * size_of::<usize>() < size_of::<TOKEN_USER>()
            || their_user.len() * size_of::<usize>() < size_of::<TOKEN_USER>()
            || our_stats.len() * size_of::<usize>() < size_of::<TOKEN_STATISTICS>()
            || their_stats.len() * size_of::<usize>() < size_of::<TOKEN_STATISTICS>()
        {
            return Err(Error::Io("short token structure"));
        }
        // SAFETY: buffers hold API-initialized, pointer-aligned token structures;
        // embedded SID pointers remain live until their owning buffers are dropped.
        let (a, b) = unsafe {
            (
                &*our_user.as_ptr().cast::<TOKEN_USER>(),
                &*their_user.as_ptr().cast::<TOKEN_USER>(),
            )
        };
        let (sa, sb) = unsafe {
            (
                &*our_stats.as_ptr().cast::<TOKEN_STATISTICS>(),
                &*their_stats.as_ptr().cast::<TOKEN_STATISTICS>(),
            )
        };
        if unsafe { EqualSid(a.User.Sid, b.User.Sid) } == 0
            || sa.AuthenticationId.LowPart != sb.AuthenticationId.LowPart
            || sa.AuthenticationId.HighPart != sb.AuthenticationId.HighPart
            || our_session.first().map(|v| *v as u32) != their_session.first().map(|v| *v as u32)
        {
            return Err(Error::PrincipalMismatch);
        }
        Ok(())
    }
    fn require_x64(process: HANDLE) -> Result<()> {
        let (mut machine, mut native) = (0u16, 0u16);
        // SAFETY: held process, two writable machine outputs.
        if unsafe { IsWow64Process2(process, &mut machine, &mut native) } == 0
            || machine != 0
            || native != 0x8664
        {
            return Err(Error::UnsupportedArchitecture);
        }
        Ok(())
    }
    fn main_module(process: HANDLE, peer_file: &PinnedFile) -> Result<(usize, u32)> {
        const MODULE_CAP: usize = 512;
        let mut modules = [0 as HMODULE; MODULE_CAP];
        let mut needed = 0u32;
        // SAFETY: fixed writable array, byte count fits u32, borrowed HMODULE values.
        if unsafe {
            EnumProcessModulesEx(
                process,
                modules.as_mut_ptr(),
                size_of_val(&modules) as u32,
                &mut needed,
                LIST_MODULES_64BIT,
            )
        } == 0
            || needed == 0
            || needed as usize > size_of_val(&modules)
            || needed as usize % size_of::<HMODULE>() != 0
        {
            return Err(Error::ModuleAmbiguous);
        }
        let process_file = final_path(&peer_file.file)?;
        let mut found = None;
        for module in modules
            .into_iter()
            .take(needed as usize / size_of::<HMODULE>())
        {
            let mut buffer = vec![0u16; 32768];
            let n = unsafe {
                GetModuleFileNameExW(process, module, buffer.as_mut_ptr(), buffer.len() as u32)
            };
            if n == 0 || n as usize >= buffer.len() {
                return Err(Error::Io("module path/truncation"));
            }
            let path = PathBuf::from(OsString::from_wide(&buffer[..n as usize]));
            // Paths select the candidate module, not its content identity. Canonical
            // path is resolved and compared to the already pinned main-image file.
            let canonical =
                std::fs::canonicalize(path).map_err(|_| Error::Io("module canonical path"))?;
            if canonical != process_file {
                continue;
            }
            if found.is_some() {
                return Err(Error::ModuleAmbiguous);
            }
            let mut info: MODULEINFO = unsafe { zeroed() };
            if unsafe {
                GetModuleInformation(process, module, &mut info, size_of::<MODULEINFO>() as u32)
            } == 0
                || info.lpBaseOfDll.is_null()
                || info.lpBaseOfDll as usize != module as usize
                || info.SizeOfImage != peer_file.layout.image_size
            {
                return Err(Error::ModuleAmbiguous);
            }
            found = Some((info.lpBaseOfDll as usize, info.SizeOfImage));
        }
        // HMODULE entries are borrowed; do not CloseHandle/FreeLibrary them.
        found.ok_or(Error::ModuleAmbiguous)
    }
    fn bounded_remote_read(
        process: HANDLE,
        base: usize,
        image_len: u32,
        offset: u32,
        length: usize,
        marker: bool,
    ) -> Result<Vec<u8>> {
        if length == 0
            || (marker && length != MARKER_LEN)
            || (!marker && (offset != 0 || length > MAX_HEADERS))
        {
            return Err(Error::Malformed("remote metadata span"));
        }
        bounded_span(offset.into(), length as u64, image_len.into())?;
        let address = base
            .checked_add(offset as usize)
            .ok_or(Error::Malformed("remote address overflow"))?;
        let end = address
            .checked_add(length)
            .ok_or(Error::Malformed("remote end overflow"))?;
        let mut cursor = address;
        while cursor < end {
            let mut region: MEMORY_BASIC_INFORMATION = unsafe { zeroed() };
            if unsafe {
                VirtualQueryEx(
                    process,
                    cursor as *const c_void,
                    &mut region,
                    size_of::<MEMORY_BASIC_INFORMATION>(),
                )
            } != size_of::<MEMORY_BASIC_INFORMATION>()
                || region.AllocationBase as usize != base
                || region.State != MEM_COMMIT
                || region.Type != MEM_IMAGE
                || region.Protect & (PAGE_GUARD | PAGE_NOACCESS) != 0
                || (marker && region.Protect != PAGE_READONLY)
            {
                return Err(Error::Io("metadata image region protection"));
            }
            let region_end = (region.BaseAddress as usize)
                .checked_add(region.RegionSize)
                .filter(|n| *n > cursor)
                .ok_or(Error::Malformed("region bounds"))?;
            cursor = region_end.min(end);
        }
        let mut bytes = vec![0u8; length];
        let mut read = 0usize;
        // SAFETY: address is derived from bounded pinned PE layout and validated
        // main module, buffers are live. Only headers and 64 marker bytes are read.
        #[cfg(test)]
        READ_COUNTS.with(|count| {
            let (opens, reads) = count.get();
            count.set((opens, reads + 1));
        });
        if unsafe {
            ReadProcessMemory(
                process,
                address as *const c_void,
                bytes.as_mut_ptr().cast(),
                length,
                &mut read,
            )
        } == 0
            || read != length
        {
            return Err(Error::Io("ReadProcessMemory/partial"));
        }
        Ok(bytes)
    }
    fn observed_marker(process: HANDLE, peer_file: &PinnedFile) -> Result<Marker> {
        require_x64(process)?;
        let (base, length) = main_module(process, peer_file)?;
        let layout = &peer_file.layout;
        let headers = bounded_remote_read(process, base, length, 0, layout.headers.len(), false)?;
        if !matching_headers(&layout.headers, &headers, base as u64) {
            #[cfg(test)]
            header_mismatch(&layout.headers, &headers, base);
            return Err(Error::LoadedLayoutMismatch);
        }
        let bytes =
            bounded_remote_read(process, base, length, layout.marker_rva, MARKER_LEN, true)?;
        Marker::parse(&bytes)
    }
    #[cfg(test)]
    fn header_mismatch(expected: &[u8], observed: &[u8], base: usize) {
        // Only already-read public PE metadata from an owned fixture executable.
        // Keep refusal intact until the actual loader differences are explained.
        let differences: Vec<_> = expected
            .iter()
            .zip(observed)
            .enumerate()
            .filter(|(_, (a, b))| a != b)
            .map(|(offset, (a, b))| (offset, *a, *b))
            .take(32)
            .collect();
        eprintln!("owned PE header mismatch: base={base:#x}, expectedLength={}, observedLength={}, firstDifferences={differences:?}", expected.len(), observed.len());
    }
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) enum ArtifactMatch {
        SameFile,
        EqualSha256Copy,
    }
    /// Retains both file pins and both process handles through the caller's short
    /// decision; no cached success survives reconnect/process/file replacement.
    pub(crate) struct MatchingLoadedGeneration {
        peer: HeldPeer,
        read: OwnedHandle,
        peer_file: PinnedFile,
        expected: ExpectedImage,
        pub(crate) comparison: ArtifactMatch,
        pub(crate) observed_marker: Marker,
    }
    impl MatchingLoadedGeneration {
        pub(crate) fn revalidate(&self) -> Result<()> {
            alive(self.peer.query.0, self.peer.owner.birth)?;
            alive(self.read.0, self.peer.owner.birth)?;
            same_principal(self.peer.query.0)?;
            let image = PinnedFile::open(&process_path(self.peer.query.0)?)?;
            if image.identity != self.peer_file.identity || image.digest != self.peer_file.digest {
                return Err(Error::ProcessChanged);
            }
            Ok(())
        }
    }
    pub(crate) fn check(
        expected: ExpectedImage,
        peer: HeldPeer,
    ) -> Result<MatchingLoadedGeneration> {
        alive(peer.query.0, peer.owner.birth)?;
        let peer_file = PinnedFile::open(&process_path(peer.query.0)?)?;
        // No module enumeration or VM_READ handle before matching expected file.
        if peer_file.digest != expected.pinned.digest {
            return Err(Error::ArtifactMismatch);
        }
        let comparison = if peer_file.identity == expected.pinned.identity {
            ArtifactMatch::SameFile
        } else {
            ArtifactMatch::EqualSha256Copy
        };
        let read = open(
            peer.owner.pid,
            PROCESS_QUERY_INFORMATION
                | PROCESS_QUERY_LIMITED_INFORMATION
                | PROCESS_VM_READ
                | PROCESS_SYNCHRONIZE,
        )?;
        alive(read.0, peer.owner.birth)?;
        same_principal(read.0)?;
        let marker = observed_marker(read.0, &peer_file)?;
        if marker != expected.marker {
            return Err(Error::LoadedMarkerMismatch);
        }
        let proof = MatchingLoadedGeneration {
            peer,
            read,
            peer_file,
            expected,
            comparison,
            observed_marker: marker,
        };
        proof.revalidate()?;
        Ok(proof)
    }

    /// Test boundary accepts only an actually owned Child, never arbitrary PID.
    #[cfg(test)]
    pub(crate) mod fixture {
        use super::*;
        pub(crate) fn expected(path: &Path, marker: Marker) -> Result<ExpectedImage> {
            ExpectedImage::pin(path, marker)
        }
        pub(crate) fn own(child: &std::process::Child) -> Result<HeldPeer> {
            let created = birth(child.as_raw_handle() as HANDLE)?;
            HeldPeer::hold(KernelOwner {
                pid: child.id(),
                birth: created,
            })
        }
        pub(crate) fn disk_marker(path: &Path) -> Result<Marker> {
            Ok(PinnedFile::open(path)?.layout.marker)
        }
        pub(crate) fn marker_offset(path: &Path) -> Result<u64> {
            Ok(PinnedFile::open(path)?.layout.marker_offset)
        }
        /// Owns a child handle and previously validated module/layout; deliberately
        /// holds no file pin so an owned test may perform rename/replacement.
        pub(crate) struct OwnedSnapshot {
            peer: HeldPeer,
            read: OwnedHandle,
            base: usize,
            size: u32,
            layout: Layout,
        }
        impl OwnedSnapshot {
            pub(crate) fn observe(&self) -> Result<Marker> {
                alive(self.read.0, self.peer.owner.birth)?;
                let headers = bounded_remote_read(
                    self.read.0,
                    self.base,
                    self.size,
                    0,
                    self.layout.headers.len(),
                    false,
                )?;
                if !matching_headers(&self.layout.headers, &headers, self.base as u64) {
                    header_mismatch(&self.layout.headers, &headers, self.base);
                    return Err(Error::LoadedLayoutMismatch);
                }
                let bytes = bounded_remote_read(
                    self.read.0,
                    self.base,
                    self.size,
                    self.layout.marker_rva,
                    MARKER_LEN,
                    true,
                )?;
                let marker = Marker::parse(&bytes)?;
                alive(self.read.0, self.peer.owner.birth)?;
                Ok(marker)
            }
        }
        /// Test-only, fixed metadata exception. Caller supplies an owned Child and
        /// its original fixture artifact; no arbitrary address or borrowed PID.
        pub(crate) fn snapshot_owned(
            child: &std::process::Child,
            path: &Path,
        ) -> Result<OwnedSnapshot> {
            let peer = own(child)?;
            let original = PinnedFile::open(path)?;
            let current = PinnedFile::open(&process_path(peer.query.0)?)?;
            if current.digest != original.digest {
                return Err(Error::ArtifactMismatch);
            }
            let read = open(
                child.id(),
                PROCESS_QUERY_INFORMATION | PROCESS_VM_READ | PROCESS_SYNCHRONIZE,
            )?;
            alive(read.0, peer.owner.birth)?;
            require_x64(read.0)?;
            let (base, size) = main_module(read.0, &current)?;
            let snapshot = OwnedSnapshot {
                peer,
                read,
                base,
                size,
                layout: original.layout.clone(),
            };
            if snapshot.observe()? != original.layout.marker {
                return Err(Error::LoadedMarkerMismatch);
            }
            Ok(snapshot)
        }
        pub(crate) fn handle_count() -> Result<u32> {
            let mut count = 0;
            // SAFETY: borrowed current-process pseudo-handle, writable scalar.
            if unsafe { GetProcessHandleCount(GetCurrentProcess(), &mut count) } == 0 {
                return Err(Error::Io("GetProcessHandleCount"));
            }
            Ok(count)
        }
        pub(crate) fn read_counts() -> (usize, usize) {
            READ_COUNTS.with(|count| count.get())
        }
    }
}

#[cfg(all(test, windows, target_arch = "x86_64"))]
pub(super) use windows::fixture;
#[cfg(all(windows, target_arch = "x86_64"))]
pub(super) use windows::{
    check, ArtifactMatch, ExpectedImage, HeldPeer, KernelOwner, MatchingLoadedGeneration,
};

#[cfg(test)]
mod parser_tests {
    use super::*;
    fn fixture() -> Vec<u8> {
        let mut data = vec![0; 1024];
        data[..2].copy_from_slice(b"MZ");
        data[0x3c..0x40].copy_from_slice(&128u32.to_le_bytes());
        data[128..132].copy_from_slice(b"PE\0\0");
        data[132..134].copy_from_slice(&0x8664u16.to_le_bytes());
        data[134..136].copy_from_slice(&1u16.to_le_bytes());
        data[148..150].copy_from_slice(&240u16.to_le_bytes());
        data[150..152].copy_from_slice(&2u16.to_le_bytes());
        data[152..154].copy_from_slice(&0x20bu16.to_le_bytes());
        for (at, value) in [(184, 4096u32), (188, 512), (208, 8192), (212, 512)] {
            data[at..at + 4].copy_from_slice(&value.to_le_bytes());
        }
        data[392..400].copy_from_slice(SECTION);
        for (at, value) in [
            (400, 64u32),
            (404, 4096),
            (408, 512),
            (412, 512),
            (428, 0x4000_0040),
        ] {
            data[at..at + 4].copy_from_slice(&value.to_le_bytes());
        }
        let mut marker = [1u8; 64];
        marker[..8].copy_from_slice(MAGIC);
        for (at, value) in [(8, 1u16), (10, 64), (12, 0x8664), (14, 1)] {
            marker[at..at + 2].copy_from_slice(&value.to_le_bytes());
        }
        data[512..576].copy_from_slice(&marker);
        data
    }
    #[test]
    fn strict_marker_and_layout() {
        let data = fixture();
        let layout = parse_image(&data).unwrap();
        assert_eq!(
            (
                layout.marker_rva,
                layout.marker_offset,
                layout.headers.len()
            ),
            (4096, 512, 512)
        );
        for length in 0..data.len() {
            assert!(parse_image(&data[..length]).is_err(), "truncation {length}");
        }
    }
    #[test]
    fn loaded_headers_allow_only_the_exact_module_base_field_change() {
        let mut data = fixture();
        let field = 128 + 48;
        data[field..field + 8].copy_from_slice(&0x1_4000_0000u64.to_le_bytes());
        let expected = parse_image(&data).unwrap().headers;
        let base = 0x7ff7_920c_0000u64;
        assert!(matching_headers(&expected, &expected, base));
        let mut observed = expected.clone();
        observed[field..field + 8].copy_from_slice(&base.to_le_bytes());
        assert!(matching_headers(&expected, &observed, base));
        assert!(!matching_headers(&expected, &observed, base + 65536));
        observed[404] ^= 1; // a section RVA remains identity-bearing
        assert!(!matching_headers(&expected, &observed, base));
        assert!(!matching_headers(&expected, &observed[..field + 7], base));
        assert!(!matching_headers(&[], &[], base));
    }
    #[test]
    fn rejects_mutable_missing_duplicate_overflow_and_unknown_schema() {
        for (offset, bytes) in [
            (0x3c, u32::MAX.to_le_bytes().to_vec()),
            (404, u32::MAX.to_le_bytes().to_vec()),
            (412, u32::MAX.to_le_bytes().to_vec()),
            (400, 0u32.to_le_bytes().to_vec()),
            (428, 0xc000_0040u32.to_le_bytes().to_vec()),
            (428, 0x6000_0040u32.to_le_bytes().to_vec()),
            (428, 0x4200_0040u32.to_le_bytes().to_vec()),
            (520, 2u16.to_le_bytes().to_vec()),
            (522, 63u16.to_le_bytes().to_vec()),
            (392, b".wrong\0\0".to_vec()),
        ] {
            let mut data = fixture();
            data[offset..offset + bytes.len()].copy_from_slice(&bytes);
            assert!(parse_image(&data).is_err(), "offset {offset}");
        }
        let mut data = fixture();
        data[134..136].copy_from_slice(&2u16.to_le_bytes());
        let section = data[392..432].to_vec();
        data[432..472].copy_from_slice(&section);
        assert!(parse_image(&data).is_err());
    }
    #[test]
    fn arbitrary_bounded_bytes_never_panic() {
        let mut state = 0x8a20_2104u32;
        for round in 0..2000 {
            let mut data = fixture();
            for _ in 0..1 + round % 8 {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                let offset = state as usize % data.len();
                data[offset] ^= (state >> 24) as u8;
            }
            let _ = parse_image(&data);
        }
    }
}

//! Keep detached daemon diagnostics off the parent's short-lived startup pipe.
use std::fs::File;
use std::io;
use std::os::windows::io::AsRawHandle;

/// Redirect Rust stderr through the Win32 standard handle. The returned file
/// must remain alive for as long as this process may write diagnostics.
pub(crate) fn redirect_stderr(file: File) -> io::Result<File> {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn SetStdHandle(which: u32, handle: *mut core::ffi::c_void) -> i32;
    }
    // STD_ERROR_HANDLE is the unsigned representation of -12.
    if unsafe { SetStdHandle((-12_i32) as u32, file.as_raw_handle()) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(file)
}

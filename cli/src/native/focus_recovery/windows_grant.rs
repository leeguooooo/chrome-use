//! A one-use private callback authorizes only the attested Chrome parent.
//! No input/window activation, caller PID, daemon grant or permission cache.
use super::{
    control::{ClientRequest, Decision, RelayHint},
    windows_parent::ChromeParent,
};
use crate::{
    windows_build_identity::{check, ExpectedImage, HeldPeer},
    windows_tcp_owner::{capture_std, selected_from_daemon_hint, SelectedConnectionHint},
};
use std::{mem::size_of, net::TcpStream, time::Instant};
use windows_sys::Win32::{
    Foundation::{GetLastError, SetLastError},
    System::{
        StationsAndDesktops::{GetThreadDesktop, GetUserObjectInformationW, UOI_IO},
        Threading::GetCurrentThreadId,
    },
    UI::WindowsAndMessaging::{
        AllowSetForegroundWindow, GetForegroundWindow, GetWindowThreadProcessId,
    },
};

pub(crate) struct AuthorizationReply {
    pub(crate) bytes: Vec<u8>,
    pub(crate) refusal: Option<String>,
}

fn refuse(reason: impl std::fmt::Display) -> Decision {
    let reason = reason.to_string();
    // Internal error enums/static reasons only; never a page string or token.
    let reason = reason.chars().take(200).collect();
    Decision::Refused { reason }
}

fn caller_input_desktop() -> bool {
    // GetThreadDesktop is borrowed: do not CloseDesktop it.
    let desktop = unsafe { GetThreadDesktop(GetCurrentThreadId()) };
    let (mut input, mut needed) = (0i32, 0u32);
    desktop != 0
        && unsafe {
            GetUserObjectInformationW(
                desktop,
                UOI_IO,
                (&mut input as *mut i32).cast(),
                size_of::<i32>() as u32,
                &mut needed,
            )
        } != 0
        && needed == size_of::<i32>() as u32
        && input != 0
}

fn decide(stream: &TcpStream, hint: RelayHint, deadline: Instant) -> Decision {
    if Instant::now() >= deadline {
        return refuse("foreground authorization deadline expired");
    }
    let result = (|| -> Result<_, String> {
        let root = capture_std(stream).map_err(|e| format!("root socket: {e}"))?;
        let daemon = check(
            ExpectedImage::pin_self().map_err(|e| format!("caller image: {e}"))?,
            HeldPeer::hold(root.peer_for_metadata().map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?,
        )
        .map_err(|e| format!("daemon image: {e}"))?;
        let local = format!("127.0.0.1:{}", hint.local_port)
            .parse()
            .map_err(|_| "invalid daemon endpoint")?;
        let remote = format!("127.0.0.1:{}", hint.host_port)
            .parse()
            .map_err(|_| "invalid host endpoint")?;
        let selected = selected_from_daemon_hint(
            &root,
            SelectedConnectionHint::from_untrusted_endpoints(local, remote)
                .map_err(|e| e.to_string())?,
        )
        .map_err(|e| format!("selected socket: {e}"))?;
        let host = check(
            ExpectedImage::pin_self().map_err(|e| e.to_string())?,
            HeldPeer::hold(selected.host_for_metadata().map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?,
        )
        .map_err(|e| format!("native host image: {e}"))?;
        let parent = ChromeParent::from_verified_tool(&host).map_err(str::to_string)?;
        selected.revalidate_std(stream).map_err(|e| e.to_string())?;
        daemon.revalidate().map_err(|e| e.to_string())?;
        let chrome = parent.require_alive().map_err(str::to_string)?;
        if chrome == 0 || chrome == u32::MAX {
            return Err("invalid attested Chrome process; wildcard grants are forbidden".into());
        }
        if !caller_input_desktop() {
            return Err("caller desktop is not receiving user input".into());
        }
        if Instant::now() >= deadline {
            return Err("foreground authorization deadline expired".into());
        }
        let mut foreground_pid = 0;
        // Observation only: this says Chrome's process is foreground, not that
        // its particular agent window/page is the native input target.
        unsafe {
            GetWindowThreadProcessId(GetForegroundWindow(), &mut foreground_pid);
        }
        if foreground_pid == chrome {
            return Ok(Decision::AlreadyForegroundProcess);
        }
        if Instant::now() >= deadline {
            return Err("foreground authorization deadline expired".into());
        }
        // Eligibility belongs to Chrome, not this daemon/native host. This one
        // public call neither activates a window nor delivers input. It cannot
        // be revoked by dropping these proofs or finishing the native lease.
        unsafe {
            SetLastError(0);
        }
        let admitted = unsafe { AllowSetForegroundWindow(chrome) };
        Ok(if admitted != 0 {
            Decision::Admitted
        } else {
            Decision::OsDenied {
                code: unsafe { GetLastError() },
            }
        })
    })();
    result.unwrap_or_else(refuse)
}

/// Consumes the validated request, so the synchronous client cannot authorize
/// that callback twice. Metadata syscalls are not atomically cancellable; check
/// the observed deadline again before the sole OS call.
pub(crate) fn authorize(
    stream: &TcpStream,
    request: ClientRequest,
    deadline: Instant,
) -> Result<AuthorizationReply, String> {
    let decision = decide(stream, request.hint(), deadline);
    let refusal = match &decision {
        Decision::Refused { reason } => Some(reason.clone()),
        _ => None,
    };
    let bytes = request
        .reply(decision)
        .map_err(|e| format!("private focus reply: {e:?}"))?;
    Ok(AuthorizationReply { bytes, refusal })
}

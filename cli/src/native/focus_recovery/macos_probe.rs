//! No-prompt capability observation in the real Chrome-launched host.
//! This probe cannot grant a lease, focus a window or post input.

use serde_json::{json, Value};
use std::sync::OnceLock;

type AxTrusted = unsafe extern "C" fn() -> u8; // Apple's Boolean.
type EventPreflight = unsafe extern "C" fn() -> bool;

struct Functions {
    _library: usize,
    ax_trusted: Option<AxTrusted>,
    post: Option<EventPreflight>,
    listen: Option<EventPreflight>,
}

fn functions() -> Option<&'static Functions> {
    static API: OnceLock<Option<Functions>> = OnceLock::new();
    API.get_or_init(|| {
        // SAFETY: fixed system framework path and symbol names. Keep the
        // loader reference for process lifetime because cached pointers use it.
        unsafe {
            let library = libc::dlopen(
                c"/System/Library/Frameworks/ApplicationServices.framework/ApplicationServices"
                    .as_ptr(),
                libc::RTLD_LAZY | libc::RTLD_LOCAL,
            );
            if library.is_null() {
                return None;
            }
            let ax = libc::dlsym(library, c"AXIsProcessTrusted".as_ptr());
            let post = libc::dlsym(library, c"CGPreflightPostEventAccess".as_ptr());
            let listen = libc::dlsym(library, c"CGPreflightListenEventAccess".as_ptr());
            Some(Functions {
                _library: library as usize,
                ax_trusted: if ax.is_null() {
                    None
                } else {
                    Some(std::mem::transmute::<*mut libc::c_void, AxTrusted>(ax))
                },
                post: if post.is_null() {
                    None
                } else {
                    Some(std::mem::transmute::<*mut libc::c_void, EventPreflight>(
                        post,
                    ))
                },
                listen: if listen.is_null() {
                    None
                } else {
                    Some(std::mem::transmute::<*mut libc::c_void, EventPreflight>(
                        listen,
                    ))
                },
            })
        }
    })
    .as_ref()
}

pub(super) fn observe() -> Value {
    let api = functions();
    // SAFETY: present pointers have the verified public C signatures above;
    // all three queries concern this process and do not request permission.
    let ax = api
        .and_then(|api| api.ax_trusted)
        .map(|query| unsafe { query() != 0 });
    let post = api.and_then(|api| api.post).map(|query| unsafe { query() });
    let listen = api
        .and_then(|api| api.listen)
        .map(|query| unsafe { query() });
    json!({
        "schemaVersion":1, "platform":"macos", "probeOnly":true, "nativeInputEnabled":false,
        "hostPid":std::process::id(),
        // SAFETY: getppid has no arguments or side effects. This is an observed
        // parent id, not proof of browser identity or process birth continuity.
        "parentPid":unsafe { libc::getppid() },
        "architecture":std::env::consts::ARCH,
        "executable":std::env::current_exe().ok().map(|path| path.to_string_lossy().into_owned()),
        "preflight":{"accessibilityTrusted":ax,"postEventsAllowed":post,"listenEventsAllowed":listen},
        "tccResponsibility":"unobserved", "parentIdentityVerified":false
    })
}

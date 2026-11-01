//! Capture a Plain WebSocket's actual endpoints before it is split. The browser
//! owner must still bind this client as its selected connection. A URL, listener
//! port or a freshly opened substitute cannot establish these source facts.
//! Default-off TCP metadata prototype only; no foreground permission or input.
use std::sync::atomic::{AtomicU64, Ordering};

use crate::windows_tcp_owner::SelectedConnectionHint;
use tokio_tungstenite::MaybeTlsStream;

static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);

pub(super) struct FocusTransport {
    hint: SelectedConnectionHint,
    generation: u64,
}

impl FocusTransport {
    pub(super) fn capture(stream: &MaybeTlsStream<tokio::net::TcpStream>) -> Option<Self> {
        let MaybeTlsStream::Plain(socket) = stream else {
            return None;
        };
        // Address facts only. The root CLI independently validates both kernel
        // directions and held process/image identities immediately before a
        // grant. This snapshot must not be reported as owner attestation.
        let hint = SelectedConnectionHint::from_untrusted_endpoints(
            socket.local_addr().ok()?,
            socket.peer_addr().ok()?,
        )
        .ok()?;
        let generation = NEXT_GENERATION
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                current.checked_add(1)
            })
            .ok()?;
        Some(Self { hint, generation })
    }

    pub(super) fn hint(&self) -> (SelectedConnectionHint, u64) {
        (self.hint, self.generation)
    }
}

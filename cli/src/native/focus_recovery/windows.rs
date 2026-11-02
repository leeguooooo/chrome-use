//! Read-only native UI Automation on the Chrome host. The caller must bind the
//! HWND to Chrome's attested parent process before invoking this reader.

use base64::Engine;
use std::os::windows::process::CommandExt;
use std::process::Stdio;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::documents::{self, DocumentProof, Snapshot};

const SCRIPT: &str = include_str!("windows-inspect.ps1");
const MAX_OUTPUT: u64 = 256 * 1024;

/// Inspect only Document metadata. Static encoded code and a JSON stdin record
/// keep page titles and URLs out of shell syntax and process command lines.
/// The proof is an observation, never permission to send a later key.
pub(super) async fn inspect(hwnd: u64, pid: u32, page_url: &str) -> Result<DocumentProof, String> {
    if hwnd == 0 || pid == 0 || page_url.len() > 8192 {
        return Err("invalid native document binding".into());
    }
    let code: Vec<u8> = SCRIPT.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let encoded = base64::engine::general_purpose::STANDARD.encode(code);
    let mut command = tokio::process::Command::new("powershell.exe");
    command
        .args(["-NoProfile", "-NonInteractive", "-EncodedCommand", &encoded])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    command.as_std_mut().creation_flags(0x08000000); // CREATE_NO_WINDOW
    let mut child = command
        .spawn()
        .map_err(|_| "native document reader could not start")?;
    let request = serde_json::to_vec(&serde_json::json!({
        "hwnd": hwnd, "pid": pid, "url": page_url,
    }))
    .map_err(|_| "invalid native document request")?;
    let result = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        let mut input = child
            .stdin
            .take()
            .ok_or("native document stdin unavailable")?;
        input
            .write_all(&request)
            .await
            .map_err(|_| "native document request was not accepted")?;
        drop(input);
        let output = child
            .stdout
            .take()
            .ok_or("native document stdout unavailable")?;
        let mut bytes = Vec::new();
        output
            .take(MAX_OUTPUT + 1)
            .read_to_end(&mut bytes)
            .await
            .map_err(|_| "native document reply could not be read")?;
        if bytes.len() as u64 > MAX_OUTPUT {
            return Err("native document reply exceeded its bound");
        }
        let status = child
            .wait()
            .await
            .map_err(|_| "native document reader exit unknown")?;
        if !status.success() {
            return Err("native document reader failed");
        }
        let snapshot: Snapshot =
            serde_json::from_slice(&bytes).map_err(|_| "invalid native document reply")?;
        documents::verify(&snapshot, pid, hwnd, page_url)
    })
    .await;
    match result {
        Ok(result) => result.map_err(str::to_string),
        Err(_) => Err("native document inspection timed out; no input was sent".into()),
    }
}

//! Streaming a local file into a page over the extension relay (#506).
//!
//! Chrome's `chrome.debugger` refuses `DOM.setFileInputFiles` unless the
//! extension has "Allow access to file URLs", which a Web Store install does
//! not have by default. The relay then hands the page the file's bytes itself:
//! base64 in chunks, each one `Runtime.evaluate`, rebuilt as a `File` in the
//! page and assigned to the `<input type=file>`.
//!
//! Each chunk is decoded in the page as it arrives and kept as a `Blob`, and
//! every chunk's reply is a byte count. The earlier version appended every
//! chunk to one growing page-side string with `returnByValue: true`, so each
//! reply carried the whole string received so far: the bytes crossing the
//! relay grew with the square of the file size, a 45 MB file sent ~19 GB of
//! replies, one upload's replies starved every other session on the relay
//! (8 s timeouts), and past ~48 MB of file a single reply passed the 64 MiB
//! cap on a message from the extension to its host and was dropped, which
//! surfaced as "CDP command timed out after 30s: Runtime.evaluate".

use std::path::{Path, PathBuf};
use std::time::Duration;

/// Raw bytes per chunk. Its base64 (512 KiB) plus the script around it stays
/// well under the 1 MiB cap on a native-messaging message from the host to
/// the extension, which every relayed command passes through.
pub const CHUNK_BYTES: usize = 384 * 1024;

/// The slowest streaming rate an upload is allowed before it is reported as
/// stalled. Measured rates are tens of MB/s; this is a floor for a busy
/// machine, not an estimate.
pub const MIN_BYTES_PER_SEC: u64 = 512 * 1024;

/// Fixed part of an upload's streaming budget (page setup, the final `File`
/// assembly, the page's own `change` handler).
pub const STREAM_BASE_BUDGET: Duration = Duration::from_secs(60);

/// How long an upload waits for another upload to the same browser to finish.
/// Streams to one browser run one at a time (see [`UploadQueue`]).
pub const QUEUE_MAX_WAIT: Duration = Duration::from_secs(600);

/// How long streaming `total_bytes` may take before the upload is abandoned.
pub fn stream_budget(total_bytes: u64) -> Duration {
    STREAM_BASE_BUDGET + Duration::from_secs(total_bytes / MIN_BYTES_PER_SEC)
}

/// The client's overall ceiling for an `upload` command: a queue wait, then
/// the stream, plus a margin so the daemon's own error arrives first. The
/// client hears keepalives the whole time; this only bounds a daemon that
/// never answers.
pub fn client_ceiling(total_bytes: u64) -> Duration {
    QUEUE_MAX_WAIT + stream_budget(total_bytes) + Duration::from_secs(60)
}

/// Total size of the files an `upload` command names, as far as they can be
/// read from here. A missing file counts as 0: the daemon reports it.
pub fn total_size<'a>(paths: impl IntoIterator<Item = &'a str>) -> u64 {
    paths
        .into_iter()
        .filter_map(|p| std::fs::metadata(p).ok())
        .map(|m| m.len())
        .sum()
}

/// A page-side property name for one upload, so two uploads into one page
/// never share a buffer.
pub fn page_key() -> String {
    format!("__cuUpload_{}", uuid::Uuid::new_v4().simple())
}

const MISSING: &str =
    "the page lost the upload buffer (did it navigate or reload during the upload?)";

/// Script that creates the page-side buffer.
pub fn begin_script(key: &str) -> String {
    format!(
        "(() => {{ window[{k}] = {{ files: [] }}; return 0; }})()",
        k = js_str(key)
    )
}

/// Script that starts a new file in the buffer. Returns the file count.
pub fn file_script(key: &str, name: &str, mime: &str, size: u64) -> String {
    format!(
        "(() => {{ const s = window[{k}]; if (!s) throw new Error({m}); \
         s.files.push({{ name: {n}, type: {t}, size: {size}, parts: [], got: 0 }}); \
         return s.files.length; }})()",
        k = js_str(key),
        m = js_str(MISSING),
        n = js_str(name),
        t = js_str(mime),
    )
}

/// Script that decodes one base64 chunk into the current file and returns the
/// bytes that file has received so far. The reply is a number, whatever the
/// file size.
pub fn chunk_script(key: &str, b64: &str) -> String {
    // base64's alphabet (A-Z a-z 0-9 + / =) needs no escaping inside a
    // single-quoted JS string.
    debug_assert!(b64
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/' || b == b'='));
    format!(
        "(() => {{ const s = window[{k}]; if (!s) throw new Error({m}); \
         const f = s.files[s.files.length - 1]; const b = '{b64}'; let u; \
         if (typeof Uint8Array.fromBase64 === 'function') {{ u = Uint8Array.fromBase64(b); }} \
         else {{ const bin = atob(b); u = new Uint8Array(bin.length); \
         for (let i = 0; i < bin.length; i++) u[i] = bin.charCodeAt(i); }} \
         f.parts.push(new Blob([u])); f.got += u.length; return f.got; }})()",
        k = js_str(key),
        m = js_str(MISSING),
    )
}

/// Script that drops the buffer, whatever state it is in.
pub fn discard_script(key: &str) -> String {
    format!(
        "(() => {{ try {{ delete window[{k}]; }} catch (e) {{ window[{k}] = undefined; }} return 0; }})()",
        k = js_str(key)
    )
}

/// `Runtime.callFunctionOn` body run on the target element: assemble the
/// buffered files and attach them. Arguments: the buffer key, and whether a
/// non-input target may take a synthetic drop/paste.
///
/// A non-file-input target with the dropzone path off is a hard no-op
/// (`noinput:0`): an uncancelled `drop` carrying a File makes Chrome navigate
/// to open it (the about:blank side effect). With the dropzone path on, the
/// drop is wrapped in a capture-phase `preventDefault` guard so the browser's
/// default file navigation can never fire, while the page's own drop handlers
/// still run.
pub const ATTACH_FUNCTION: &str = r#"function(key, allowDropzone) {
    const s = window[key];
    try { delete window[key]; } catch (e) { window[key] = undefined; }
    if (!s) throw new Error('the page lost the upload buffer (did it navigate or reload during the upload?)');
    const dt = new DataTransfer();
    for (const f of s.files) {
        if (f.got !== f.size) throw new Error('upload of ' + f.name + ' is incomplete: ' + f.got + ' of ' + f.size + ' bytes arrived');
        const file = new File(f.parts, f.name, { type: f.type });
        if (file.size !== f.size) throw new Error('upload of ' + f.name + ' assembled to ' + file.size + ' bytes, expected ' + f.size);
        dt.items.add(file);
    }
    const el = this;
    if (el && el.tagName === 'INPUT' && el.type === 'file') {
        el.files = dt.files;
        el.dispatchEvent(new Event('input', { bubbles: true }));
        el.dispatchEvent(new Event('change', { bubbles: true }));
        return 'input:' + dt.files.length;
    }
    if (!allowDropzone) return 'noinput:0';
    const guard = e => { e.preventDefault(); };
    window.addEventListener('dragover', guard, true);
    window.addEventListener('drop', guard, true);
    try {
        try { el.dispatchEvent(new ClipboardEvent('paste', { bubbles: true, clipboardData: dt })); } catch (e) {}
        try {
            const ev = new DragEvent('drop', { bubbles: true, cancelable: true });
            Object.defineProperty(ev, 'dataTransfer', { value: dt });
            el.dispatchEvent(ev);
        } catch (e) {}
    } finally {
        window.removeEventListener('dragover', guard, true);
        window.removeEventListener('drop', guard, true);
    }
    return 'event:' + dt.files.length;
}"#;

fn js_str(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "''".to_string())
}

/// Megabytes, one decimal, for messages.
pub fn mb(bytes: u64) -> String {
    format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
}

/// One upload stream at a time per browser.
///
/// Every session on a relay shares one native-messaging pipe to the extension.
/// Several streams at once each get a fraction of it and starve every other
/// session's commands while they run; one at a time, each upload finishes at
/// full speed and other sessions' commands slip in between its chunks. The
/// lock is an OS file lock (released by the OS if the daemon dies), keyed by
/// the browser's endpoint, so it spans all session daemons.
pub struct UploadQueue {
    file: std::fs::File,
    holder_path: PathBuf,
}

/// What the lock file records about its holder, for a waiter's message.
#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq)]
pub struct QueueHolder {
    pub session: String,
    pub bytes: u64,
    pub started_ms: u64,
}

impl UploadQueue {
    /// Where the queue locks live: the session directory every daemon of this
    /// user shares. Tests keep theirs out of it.
    pub fn lock_dir() -> PathBuf {
        if cfg!(test) {
            return std::env::temp_dir().join("chrome-use-upload-locks-test");
        }
        crate::connection::get_socket_dir()
    }

    /// The lock file for the browser at `endpoint` inside `dir`. The endpoint
    /// carries the relay's secret guid, so only its hash is used.
    pub fn lock_path(dir: &Path, endpoint: &str) -> PathBuf {
        // FNV-1a: stable across builds and processes (std's hasher is not).
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for b in endpoint.as_bytes() {
            h ^= u64::from(*b);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
        dir.join(format!("upload-{h:016x}.lock"))
    }

    /// Take the lock without waiting. `Ok(None)` when another upload holds it.
    pub fn try_acquire(path: &Path, holder: &QueueHolder) -> Result<Option<Self>, String> {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .map_err(|e| format!("cannot open the upload queue lock {}: {e}", path.display()))?;
        match file.try_lock() {
            Ok(()) => {}
            Err(std::fs::TryLockError::WouldBlock) => return Ok(None),
            Err(std::fs::TryLockError::Error(e)) => {
                return Err(format!(
                    "cannot lock the upload queue {}: {e}",
                    path.display()
                ))
            }
        }
        // The holder note lives beside the lock, not in it: Windows locks are
        // mandatory, so a waiting process could not read a locked file.
        let holder_path = Self::holder_path(path);
        let _ = std::fs::write(
            &holder_path,
            serde_json::to_string(holder).unwrap_or_default(),
        );
        Ok(Some(Self { file, holder_path }))
    }

    fn holder_path(path: &Path) -> PathBuf {
        path.with_extension("holder")
    }

    /// Who holds the lock at `path`, if it says.
    pub fn holder(path: &Path) -> Option<QueueHolder> {
        serde_json::from_str(&std::fs::read_to_string(Self::holder_path(path)).ok()?).ok()
    }

    /// Wait for the lock, up to `max_wait`, polling. The daemon keeps sending
    /// keepalives to its client meanwhile.
    pub async fn acquire(
        path: &Path,
        holder: &QueueHolder,
        max_wait: Duration,
    ) -> Result<(Self, Duration), String> {
        let started = std::time::Instant::now();
        loop {
            if let Some(lock) = Self::try_acquire(path, holder)? {
                return Ok((lock, started.elapsed()));
            }
            if started.elapsed() >= max_wait {
                let who = Self::holder(path)
                    .map(|h| {
                        let age = now_ms().saturating_sub(h.started_ms) / 1000;
                        format!(
                            " (session '{}', {}, started {age}s ago)",
                            h.session,
                            mb(h.bytes)
                        )
                    })
                    .unwrap_or_default();
                return Err(format!(
                    "another upload to this browser is still running{who}; waited {}s. \
                     Uploads over the extension relay run one at a time: retry once it \
                     has finished, and do not start several large uploads at once.",
                    started.elapsed().as_secs()
                ));
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }
}

impl Drop for UploadQueue {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.holder_path);
        let _ = self.file.unlock();
    }
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_chunk_stays_under_the_native_messaging_cap_with_its_envelope() {
        use base64::Engine;
        let b64 = base64::engine::general_purpose::STANDARD.encode(vec![0xffu8; CHUNK_BYTES]);
        let script = chunk_script(&page_key(), &b64);
        // The relay wraps the command in a forwardCDPCommand envelope with a
        // session id; allow it a generous 4 KiB.
        let envelope = serde_json::json!({
            "id": i64::MAX,
            "method": "forwardCDPCommand",
            "params": {
                "method": "Runtime.evaluate",
                "sessionId": "cb-tab-2147483647",
                "params": { "expression": script, "returnByValue": true },
            },
        })
        .to_string();
        assert!(envelope.len() + 4096 < 1024 * 1024, "{}", envelope.len());
    }

    #[test]
    fn chunk_replies_are_a_byte_count_not_the_buffer() {
        let s = chunk_script("__k", "QUJD");
        assert!(s.ends_with("return f.got; })()"), "{s}");
        assert!(
            !s.contains("+= '"),
            "a chunk must not grow a page-side string: {s}"
        );
        assert!(s.contains("new Blob([u])"));
    }

    #[test]
    fn scripts_quote_names_as_js_strings() {
        let s = file_script("__k", "it's \"a\" clip.mp4", "video/mp4", 7);
        assert!(s.contains(r#"name: "it's \"a\" clip.mp4""#), "{s}");
        assert!(s.contains("size: 7"));
        assert!(begin_script("__k").contains(r#"window["__k"] = { files: [] }"#));
        assert!(discard_script("__k").contains(r#"delete window["__k"]"#));
    }

    #[test]
    fn page_keys_differ_per_upload() {
        assert_ne!(page_key(), page_key());
        assert!(page_key().starts_with("__cuUpload_"));
    }

    #[test]
    fn budgets_scale_with_size_and_the_client_outlasts_the_daemon() {
        assert_eq!(stream_budget(0), STREAM_BASE_BUDGET);
        let mb200 = 200 * 1024 * 1024;
        assert_eq!(stream_budget(mb200), Duration::from_secs(60 + 400));
        for bytes in [0, 1 << 20, 45 << 20, mb200, 2 << 30] {
            assert!(client_ceiling(bytes) > QUEUE_MAX_WAIT + stream_budget(bytes));
            // Never below the client's ordinary 180 s ceiling.
            assert!(client_ceiling(bytes) >= Duration::from_secs(180));
        }
    }

    #[test]
    fn total_size_skips_missing_files() {
        let dir = std::env::temp_dir().join(format!("cu-upload-size-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("a.bin");
        std::fs::write(&a, vec![0u8; 1234]).unwrap();
        let missing = dir.join("missing.bin");
        assert_eq!(
            total_size([a.to_str().unwrap(), missing.to_str().unwrap()]),
            1234
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn lock_path_hides_the_endpoint_and_is_stable() {
        let dir = Path::new("/tmp/x");
        let p = UploadQueue::lock_path(dir, "ws://127.0.0.1:5/secretguid");
        assert_eq!(
            p,
            UploadQueue::lock_path(dir, "ws://127.0.0.1:5/secretguid")
        );
        assert_ne!(
            p,
            UploadQueue::lock_path(dir, "ws://127.0.0.1:6/secretguid")
        );
        assert!(!p.to_string_lossy().contains("secretguid"));
    }

    #[tokio::test]
    async fn one_upload_per_browser_and_the_second_names_the_first() {
        let dir = std::env::temp_dir().join(format!("cu-upload-q-{}", uuid::Uuid::new_v4()));
        let path = UploadQueue::lock_path(&dir, "ws://127.0.0.1:1/g");
        let first = QueueHolder {
            session: "x".into(),
            bytes: 71 << 20,
            started_ms: now_ms(),
        };
        let held = UploadQueue::try_acquire(&path, &first)
            .unwrap()
            .expect("free");
        assert_eq!(UploadQueue::holder(&path), Some(first));
        let second = QueueHolder {
            session: "y".into(),
            bytes: 1,
            started_ms: now_ms(),
        };
        // A second handle (another daemon in real use) cannot take it.
        assert!(UploadQueue::try_acquire(&path, &second).unwrap().is_none());
        let err = match UploadQueue::acquire(&path, &second, Duration::from_millis(300)).await {
            Err(e) => e,
            Ok(_) => panic!("acquired a held lock"),
        };
        assert!(err.contains("session 'x', 71.0 MB"), "{err}");
        assert!(err.contains("one at a time"), "{err}");
        // Released on drop: the waiter gets it.
        let waiter = tokio::spawn({
            let path = path.clone();
            async move {
                UploadQueue::acquire(&path, &second, Duration::from_secs(10))
                    .await
                    .map(|(_, waited)| waited)
            }
        });
        tokio::time::sleep(Duration::from_millis(300)).await;
        drop(held);
        let waited = waiter.await.unwrap().expect("acquired after release");
        assert!(waited >= Duration::from_millis(250), "{waited:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

//! Native-messaging focus leases. No panel/filesystem operation can authorize
//! desktop input, and no Chrome window is focused by a prepare request.

use serde_json::Value;
use std::sync::Arc;

pub(crate) struct NativeFocusHost {
    #[cfg(windows)]
    state: tokio::sync::Mutex<State>,
    #[cfg(windows)]
    revocations: std::sync::Mutex<
        std::collections::HashMap<String, std::sync::Weak<super::authority::DispatchFence>>,
    >,
}

#[cfg(windows)]
#[derive(Default)]
struct State {
    active: Option<Lease>,
    finished: Option<(String, Value)>,
}

#[cfg(windows)]
struct Lease {
    token: String,
    page_url: String,
    expires: std::time::Instant,
    desktop: super::windows_desktop::DesktopLease,
    _desktop_lock: std::fs::File,
    fence: Arc<super::authority::DispatchFence>,
}

#[cfg(windows)]
impl State {
    fn expire(&mut self) {
        if self
            .active
            .as_ref()
            .is_some_and(|lease| std::time::Instant::now() >= lease.expires)
        {
            let mut lease = self.active.take().unwrap();
            lease.fence.revoke();
            let result = serde_json::json!({"schemaVersion":1,"expired":true,"restore":lease.desktop.finish(false)});
            self.finished = Some((lease.token.clone(), result));
        }
    }
}

impl NativeFocusHost {
    pub(crate) fn new() -> Arc<Self> {
        let host = Arc::new(Self {
            #[cfg(windows)]
            state: tokio::sync::Mutex::new(State::default()),
            #[cfg(windows)]
            revocations: std::sync::Mutex::new(std::collections::HashMap::new()),
        });
        #[cfg(windows)]
        {
            let weak = Arc::downgrade(&host);
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                    let Some(host) = weak.upgrade() else { break };
                    {
                        let mut registry = host
                            .revocations
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        registry.retain(|_, weak| {
                            if let Some(fence) = weak.upgrade() {
                                fence.expire();
                                true
                            } else {
                                false
                            }
                        });
                    }
                    let state = host.state.try_lock();
                    if let Ok(mut state) = state {
                        state.expire();
                    }
                }
            });
        }
        host
    }

    pub(crate) fn revoke(&self, token: &str) {
        #[cfg(windows)]
        if let Some(fence) = self
            .revocations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(token)
            .and_then(std::sync::Weak::upgrade)
        {
            fence.revoke();
        }
        #[cfg(not(windows))]
        let _ = token;
    }

    pub(crate) async fn request(&self, operation: &str, params: &Value) -> Result<Value, String> {
        #[cfg(target_os = "macos")]
        if operation == "probe" {
            if params != &serde_json::json!({}) {
                return Err("Native capability probe takes no parameters".into());
            }
            return Ok(super::macos_probe::observe());
        }
        #[cfg(not(windows))]
        {
            let _ = (operation, params);
            Err("Native focus recovery is unavailable on this platform; no focus or key input was attempted".into())
        }
        #[cfg(windows)]
        {
            self.windows_request(operation, params).await
        }
    }

    #[cfg(windows)]
    async fn windows_request(&self, operation: &str, params: &Value) -> Result<Value, String> {
        use serde_json::json;
        use std::{
            fs::OpenOptions,
            time::{Duration, Instant},
        };
        let mut state = self.state.lock().await;
        state.expire();
        if operation == "prepare" {
            if params["allowFocusRecovery"].as_bool() != Some(true) {
                return Err("Native focus recovery requires explicit invocation permission".into());
            }
            let page_url = params["url"]
                .as_str()
                .filter(|url| url.len() <= 8192)
                .ok_or("Native focus recovery requires a bounded page URL")?;
            let url = url::Url::parse(page_url).map_err(|_| "Invalid native focus page URL")?;
            if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
                return Err("Native focus recovery requires an ordinary web page".into());
            }
            if state.active.is_some() {
                return Err("A native focus recovery is already in progress".into());
            }
            let directory = dirs::home_dir()
                .ok_or("Native desktop lock directory unavailable")?
                .join(".chrome-use");
            std::fs::create_dir_all(&directory)
                .map_err(|_| "Native desktop lock directory unavailable")?;
            let lock = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(directory.join("native-focus.lock"))
                .map_err(|_| "Native desktop lock unavailable")?;
            lock.try_lock()
                .map_err(|_| "Another native host owns desktop recovery; no focus changed")?;
            let expires = Instant::now() + Duration::from_secs(20);
            let fence = super::authority::DispatchFence::new(expires);
            let desktop = super::windows_desktop::DesktopLease::prepare(fence.clone())?;
            let token = uuid::Uuid::new_v4().simple().to_string();
            state.active = Some(Lease {
                token: token.clone(),
                page_url: page_url.to_string(),
                expires,
                desktop,
                _desktop_lock: lock,
                fence: fence.clone(),
            });
            let mut registry = self
                .revocations
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            registry.retain(|_, weak| weak.strong_count() != 0);
            registry.insert(token.clone(), Arc::downgrade(&fence));
            return Ok(json!({"schemaVersion":1,"token":token,"leaseMs":20000}));
        }
        let token = params["token"]
            .as_str()
            .filter(|token| token.len() == 32 && token.bytes().all(|b| b.is_ascii_hexdigit()))
            .ok_or("Invalid native focus lease")?;
        if operation == "finish" {
            if let Some((finished, receipt)) = &state.finished {
                if finished == token {
                    return Ok(receipt.clone());
                }
            }
        }
        let lease = state
            .active
            .as_mut()
            .filter(|lease| lease.token == token)
            .ok_or("Native focus lease is absent or expired; no key was repeated")?;
        match operation {
            "inspect" => {
                if lease.desktop.inspection_started() {
                    return Err(
                        "native inspection was already attempted; lease was not renewed".into(),
                    );
                }
                // Preparation without activation should release desktop
                // ownership promptly; an active operation gets its own cap.
                let active_deadline = Instant::now() + Duration::from_secs(120);
                if !lease.fence.renew(active_deadline) {
                    return Err("native recovery authority was revoked before inspection".into());
                }
                lease.expires = active_deadline;
                let verified = lease
                    .desktop
                    .inspect(&lease.page_url, lease.expires)
                    .await?;
                Ok(json!({"schemaVersion":1,"token":token,"verified":verified["verified"]}))
            }
            "escape" => {
                let receipt = lease.desktop.escape(lease.expires).await?;
                Ok(json!({"schemaVersion":1,"token":token,"input":receipt}))
            }
            "finish" => {
                let mut lease = state.active.take().unwrap();
                let restore = match params["restoreAllowed"].as_bool() {
                    Some(false) => lease.desktop.abandon_restore(),
                    allowed => lease.desktop.finish(allowed == Some(true)),
                };
                let receipt = json!({"schemaVersion":1,"expired":false,"restore":restore});
                state.finished = Some((lease.token.clone(), receipt.clone()));
                Ok(receipt)
            }
            _ => Err("Unsupported native focus operation".into()),
        }
    }
}

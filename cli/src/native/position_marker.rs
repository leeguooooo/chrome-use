//! Optional action-position marker (#721, #849–#851): a small square drawn by
//! the browser itself where the agent last clicked, hovered or scrolled, so a
//! person watching the window can follow along.
//!
//! It is drawn with `Overlay.setShowHinge`, the compositor overlay Chrome uses
//! to show a foldable device's hinge. That path never calls `Overlay.enable`
//! (which would switch the renderer's input to unbuffered delivery), injects
//! nothing into the page and sends no input, so a page cannot see it; measured
//! in `docs/evidence/position-indicator-*`. The marker is off by default:
//! `AGENT_BROWSER_SHOW_POSITION=1` or `"showPosition": true` in
//! `~/.chrome-use/config.json` turns it on (the env var wins either way).
//!
//! Rules it keeps:
//!
//! - The action path never waits for it. [`Marker::note_input`] only queues
//!   the point; a worker owned by the CDP client draws it, coalescing a burst
//!   of points into the latest one. A failure is recorded, never retried, and
//!   never fails, delays or repeats the action.
//! - Coordinates: input events are CSS pixels; the hinge rectangle is in
//!   device-independent pixels, so the point is multiplied by the page zoom
//!   (`Page.getLayoutMetrics` → `cssVisualViewport.zoom`). A pinch-zoomed page,
//!   a point outside the viewport, or a session that is not a top-level page
//!   (an out-of-process iframe's own coordinates) is suppressed, never guessed.
//! - Page screenshots hide it first ([`Marker::hide_for_capture`], waited for
//!   up to [`CAPTURE_HIDE_TIMEOUT`]) and put it back afterwards if it has not
//!   expired. `record` keeps it: a recording is for a person watching.
//! - It clears itself [`EXPIRE_AFTER`] after the last point, when another tab
//!   gets a point, and the browser drops it on a navigation or detach.
//! - An acknowledgement is reported as `acknowledged`, not "shown": an ack is
//!   not proof that pixels reached the screen.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tokio::sync::{broadcast, mpsc, oneshot};

use super::cdp::types::CdpEvent;

/// The marker clears itself this long after the last point.
pub const EXPIRE_AFTER: Duration = Duration::from_millis(2000);
/// How long a screenshot waits for the marker to be hidden before capturing
/// anyway (and saying the marker may be in the image).
pub const CAPTURE_HIDE_TIMEOUT: Duration = Duration::from_millis(500);
/// Budget for each overlay or metrics command the worker sends.
const COMMAND_TIMEOUT: Duration = Duration::from_millis(1000);
/// Layout metrics (page zoom, viewport size) are reused this long per session.
const METRICS_TTL: Duration = Duration::from_millis(2000);
/// The setting is re-read from config.json at most this often.
const SETTING_TTL: Duration = Duration::from_millis(2000);
/// Side of the square, in device-independent pixels.
const SIZE_DIP: f64 = 16.0;
/// A marker that stays put is drawn again after this long (see `Drawn::sent`).
const RESEND_AFTER: Duration = Duration::from_millis(1500);

/// What the worker sends commands through: the CDP client's own socket,
/// without the client's per-command bookkeeping. A trait so tests can fake it.
pub(crate) trait Link: Send + Sync + 'static {
    fn call<'a>(
        &'a self,
        method: &'a str,
        params: Value,
        session: &'a str,
        timeout: Duration,
    ) -> Pin<Box<dyn Future<Output = Result<Value, LinkError>> + Send + 'a>>;
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum LinkError {
    Timeout,
    Closed,
    Protocol { code: Option<i64>, message: String },
}

impl LinkError {
    /// The browser (or the relay in front of it) does not offer the command.
    fn unsupported(&self) -> bool {
        match self {
            LinkError::Protocol { code, message } => {
                *code == Some(-32601) || {
                    let m = message.to_ascii_lowercase();
                    m.contains("wasn't found")
                        || m.contains("not found")
                        || m.contains("not allowed")
                        || m.contains("not supported")
                }
            }
            _ => false,
        }
    }

    fn describe(&self) -> String {
        match self {
            LinkError::Timeout => "no reply in time".into(),
            LinkError::Closed => "the connection closed".into(),
            LinkError::Protocol { message, .. } => message.clone(),
        }
    }
}

/// Where the setting came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Env,
    Config,
    Default,
}

impl Source {
    fn name(self) -> &'static str {
        match self {
            Source::Env => "AGENT_BROWSER_SHOW_POSITION",
            Source::Config => "config.json showPosition",
            Source::Default => "default",
        }
    }
}

/// `AGENT_BROWSER_SHOW_POSITION` (1/true/on or 0/false/off), else
/// `"showPosition"` in config.json, else off.
pub fn resolve_setting(env: Option<&str>, config: Option<&Value>) -> (bool, Source) {
    match env.map(|v| v.trim().to_ascii_lowercase()).as_deref() {
        Some("1" | "true" | "on" | "yes") => return (true, Source::Env),
        Some("0" | "false" | "off" | "no") => return (false, Source::Env),
        _ => {}
    }
    match config
        .and_then(|c| c.get("showPosition"))
        .and_then(Value::as_bool)
    {
        Some(on) => (on, Source::Config),
        None => (false, Source::Default),
    }
}

/// The marker's last known state, as `status` reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// Off by setting.
    Disabled,
    /// On, nothing drawn yet or the last marker cleared.
    Idle,
    /// The browser acknowledged drawing it (not proof it is on screen).
    Acknowledged,
    /// The draw got no acknowledgement in time; it may or may not be shown.
    Unconfirmed,
    /// Not drawn because the geometry was uncertain (see `reason`).
    Suppressed,
    /// The browser or relay does not offer `Overlay.setShowHinge`; no more
    /// attempts on this connection.
    Unsupported,
}

impl State {
    pub fn name(self) -> &'static str {
        match self {
            State::Disabled => "disabled",
            State::Idle => "idle",
            State::Acknowledged => "acknowledged",
            State::Unconfirmed => "unconfirmed",
            State::Suppressed => "suppressed",
            State::Unsupported => "unsupported",
        }
    }
}

/// Result of hiding the marker before a page capture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureHide {
    /// Nothing was drawn on this session.
    NotShown,
    /// Hidden, acknowledged.
    Hidden,
    /// Asked to hide, no acknowledgement in time: it may be in the image.
    Unconfirmed,
}

impl CaptureHide {
    /// The warning a screenshot carries when the marker may be in it.
    pub fn warning(self) -> Option<&'static str> {
        (self == CaptureHide::Unconfirmed).then_some(
            "the position marker could not be confirmed hidden before this screenshot; it may \
             appear in the image as a small orange square",
        )
    }
}

struct Shared {
    setting: Option<(bool, Source, Instant)>,
    state: State,
    reason: Option<String>,
    /// The session the marker was last drawn on, while it may be visible.
    visible: Option<String>,
    /// Points queued but not drawn yet, so a capture knows to wait for them.
    queued: usize,
    marks: u64,
    suppressed: u64,
    last_at: Option<Instant>,
}

enum Msg {
    Point {
        session: String,
        x: f64,
        y: f64,
    },
    Hide {
        session: String,
        reply: oneshot::Sender<CaptureHide>,
    },
    Restore {
        session: String,
    },
    Expire {
        generation: u64,
    },
    /// Turned off: clear whatever is drawn.
    ClearNow,
}

/// One per CDP client. Cheap to call when off: a lock and a cached setting.
pub struct Marker {
    tx: mpsc::UnboundedSender<Msg>,
    shared: Arc<Mutex<Shared>>,
    read_setting: fn() -> (bool, Source),
}

impl Marker {
    /// Start the worker on `link`, following `events` for navigations and
    /// detaches. `read_setting` is called at most every [`SETTING_TTL`].
    pub(crate) fn start(
        link: Arc<dyn Link>,
        events: Option<broadcast::Receiver<CdpEvent>>,
        read_setting: fn() -> (bool, Source),
    ) -> Self {
        let shared = Arc::new(Mutex::new(Shared {
            setting: None,
            state: State::Disabled,
            reason: None,
            visible: None,
            queued: 0,
            marks: 0,
            suppressed: 0,
            last_at: None,
        }));
        let (tx, rx) = mpsc::unbounded_channel();
        let marker = Marker {
            tx: tx.clone(),
            shared: shared.clone(),
            read_setting,
        };
        let worker = Worker {
            link,
            shared,
            tx,
            read_setting,
            metrics: HashMap::new(),
            page_sessions: HashMap::new(),
            drawn: None,
            generation: 0,
            hidden_for_capture: None,
        };
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(worker.run(rx, events));
        }
        Self::refresh_setting(&marker.shared, read_setting);
        marker
    }

    fn refresh_setting(shared: &Arc<Mutex<Shared>>, read: fn() -> (bool, Source)) -> bool {
        let mut s = shared.lock().unwrap();
        if let Some((on, _, at)) = s.setting {
            if at.elapsed() < SETTING_TTL {
                return on;
            }
        }
        drop(s);
        let (on, source) = read();
        s = shared.lock().unwrap();
        s.setting = Some((on, source, Instant::now()));
        if !on {
            s.state = State::Disabled;
        } else if s.state == State::Disabled {
            s.state = State::Idle;
        }
        on
    }

    fn enabled(&self) -> bool {
        Self::refresh_setting(&self.shared, self.read_setting)
    }

    /// Forget the cached setting: the daemon changed the env var for this
    /// command. Turning it off clears a drawn marker on the next point.
    pub fn reload_setting(&self) {
        self.shared.lock().unwrap().setting = None;
        if !self.enabled() {
            let _ = self.tx.send(Msg::ClearNow);
        }
    }

    /// Whether the marker is on (and not known unsupported), for callers
    /// that would spend a command to compute a point.
    pub fn is_enabled(&self) -> bool {
        self.enabled() && self.shared.lock().unwrap().state != State::Unsupported
    }

    /// Called by the CDP client for every command it sends: queues the point
    /// of a mouse press, release or wheel event. Never blocks.
    pub fn note_input(&self, method: &str, params: Option<&Value>, session: Option<&str>) {
        if method != "Input.dispatchMouseEvent" {
            return;
        }
        let Some(p) = params else { return };
        if !matches!(
            p.get("type").and_then(Value::as_str),
            Some("mousePressed" | "mouseReleased" | "mouseWheel")
        ) {
            return;
        }
        if let (Some(x), Some(y), Some(session)) = (
            p.get("x").and_then(Value::as_f64),
            p.get("y").and_then(Value::as_f64),
            session.filter(|s| !s.is_empty()),
        ) {
            self.mark(session, x, y);
        }
    }

    /// Queue a point (CSS pixels of the session's viewport) for actions that
    /// produce no press: hover, `mouse move`, a click dispatched in the DOM.
    pub fn mark(&self, session: &str, x: f64, y: f64) {
        if !self.enabled() || !x.is_finite() || !y.is_finite() {
            return;
        }
        {
            let mut s = self.shared.lock().unwrap();
            if s.state == State::Unsupported {
                return;
            }
            s.queued += 1;
        }
        if self
            .tx
            .send(Msg::Point {
                session: session.to_string(),
                x,
                y,
            })
            .is_err()
        {
            self.shared.lock().unwrap().queued -= 1;
        }
    }

    /// Hide the marker on `session` before a page capture. Returns at once
    /// when nothing is drawn or queued there.
    pub async fn hide_for_capture(&self, session: &str) -> CaptureHide {
        {
            let s = self.shared.lock().unwrap();
            if s.visible.as_deref() != Some(session) && s.queued == 0 {
                return CaptureHide::NotShown;
            }
        }
        let (reply, rx) = oneshot::channel();
        if self
            .tx
            .send(Msg::Hide {
                session: session.to_string(),
                reply,
            })
            .is_err()
        {
            return CaptureHide::NotShown;
        }
        match tokio::time::timeout(CAPTURE_HIDE_TIMEOUT, rx).await {
            Ok(Ok(outcome)) => outcome,
            _ => CaptureHide::Unconfirmed,
        }
    }

    /// After the capture: draw it again if it would still be showing.
    pub fn restore_after_capture(&self, session: &str, hidden: CaptureHide) {
        if hidden != CaptureHide::NotShown {
            let _ = self.tx.send(Msg::Restore {
                session: session.to_string(),
            });
        }
    }

    /// `status` / `doctor` view.
    pub fn status(&self) -> Value {
        let on = self.enabled();
        let s = self.shared.lock().unwrap();
        let source = s.setting.map(|(_, src, _)| src).unwrap_or(Source::Default);
        json!({
            "enabled": on,
            "setting": source.name(),
            "state": s.state.name(),
            "reason": s.reason,
            "marks": s.marks,
            "suppressed": s.suppressed,
            "lastMs": s.last_at.map(|t| t.elapsed().as_millis() as u64),
        })
    }
}

/// The daemon's reader: the env var it was started with, else config.json.
pub fn read_setting_from_environment() -> (bool, Source) {
    resolve_setting(
        std::env::var("AGENT_BROWSER_SHOW_POSITION").ok().as_deref(),
        crate::report::user_config().as_ref(),
    )
}

/// Session metrics the worker caches: page zoom, pinch scale, viewport size
/// in CSS pixels.
#[derive(Clone, Copy, Debug)]
struct Metrics {
    zoom: f64,
    scale: f64,
    width: f64,
    height: f64,
    at: Instant,
}

/// The rectangle the hinge overlay gets for a point, or why it is suppressed.
pub(crate) fn hinge_rect(x: f64, y: f64, m: (f64, f64, f64, f64)) -> Result<Value, &'static str> {
    let (zoom, scale, width, height) = m;
    if !(zoom.is_finite() && zoom > 0.0) {
        return Err("the page zoom is unknown");
    }
    if (scale - 1.0).abs() > 0.01 {
        return Err("the page is pinch-zoomed");
    }
    if x < 0.0 || y < 0.0 || (width > 0.0 && x > width) || (height > 0.0 && y > height) {
        return Err("the point is outside the viewport");
    }
    let half = SIZE_DIP / 2.0;
    Ok(json!({
        "rect": {
            "x": (x * zoom - half).round() as i64,
            "y": (y * zoom - half).round() as i64,
            "width": SIZE_DIP as i64,
            "height": SIZE_DIP as i64,
        },
        "contentColor": { "r": 247, "g": 140, "b": 0, "a": 1.0 },
        "outlineColor": { "r": 255, "g": 255, "b": 255, "a": 0.9 },
    }))
}

struct Drawn {
    session: String,
    config: Value,
    expires: Instant,
    /// When the draw was last sent, so a marker that stays put is re-sent
    /// before the extension's watchdog (4 s, `position-marker-watchdog.js`)
    /// takes it for one its daemon abandoned.
    sent: Instant,
}

struct Worker {
    link: Arc<dyn Link>,
    shared: Arc<Mutex<Shared>>,
    tx: mpsc::UnboundedSender<Msg>,
    read_setting: fn() -> (bool, Source),
    metrics: HashMap<String, Metrics>,
    /// Whether each session is a top-level page (`Target.getTargetInfo`).
    page_sessions: HashMap<String, bool>,
    drawn: Option<Drawn>,
    generation: u64,
    /// Hidden by a capture and still due back (session).
    hidden_for_capture: Option<Drawn>,
}

impl Worker {
    async fn run(
        mut self,
        mut rx: mpsc::UnboundedReceiver<Msg>,
        mut events: Option<broadcast::Receiver<CdpEvent>>,
    ) {
        let mut stash: Option<Msg> = None;
        loop {
            let msg = if let Some(m) = stash.take() {
                m
            } else {
                tokio::select! {
                    m = rx.recv() => match m { Some(m) => m, None => break },
                    e = recv_event(&mut events) => {
                        match e {
                            Some(e) => self.on_event(&e),
                            None => events = None,
                        }
                        continue;
                    }
                }
            };
            match msg {
                Msg::Point { session, x, y } => {
                    // Coalesce a burst: only the newest point matters. Stop
                    // at anything else so a capture's hide keeps its order.
                    let (mut session, mut x, mut y, mut taken) = (session, x, y, 1usize);
                    while let Ok(next) = rx.try_recv() {
                        match next {
                            Msg::Point {
                                session: s,
                                x: nx,
                                y: ny,
                            } => {
                                session = s;
                                x = nx;
                                y = ny;
                                taken += 1;
                            }
                            other => {
                                stash = Some(other);
                                break;
                            }
                        }
                    }
                    self.draw(&session, x, y).await;
                    let mut s = self.shared.lock().unwrap();
                    s.queued = s.queued.saturating_sub(taken);
                }
                Msg::Hide { session, reply } => {
                    let outcome = self.hide(&session).await;
                    let _ = reply.send(outcome);
                }
                Msg::Restore { session } => self.restore(&session).await,
                Msg::ClearNow => {
                    if let Some(d) = self.drawn.take() {
                        let _ = self.clear(&d.session).await;
                    }
                    self.hidden_for_capture = None;
                    self.set_visible(None);
                }
                Msg::Expire { generation } => {
                    if generation == self.generation {
                        if let Some(d) = self.drawn.take() {
                            let _ = self.clear(&d.session).await;
                        }
                        self.hidden_for_capture = None;
                        self.set_visible(None);
                    }
                }
            }
        }
    }

    fn on_event(&mut self, e: &CdpEvent) {
        let Some(sid) = e.session_id.as_deref() else {
            if e.method == "Target.detachedFromTarget" {
                if let Some(s) = e.params.get("sessionId").and_then(Value::as_str) {
                    self.forget(s);
                }
            }
            return;
        };
        match e.method.as_str() {
            // A new document: the browser drops the overlay itself.
            "Page.frameNavigated" if e.params["frame"].get("parentId").is_none() => {
                self.metrics.remove(sid);
                self.forget_drawn(sid);
            }
            "Target.detachedFromTarget" => {
                if let Some(s) = e.params.get("sessionId").and_then(Value::as_str) {
                    self.forget(s);
                }
            }
            _ => {}
        }
    }

    fn forget(&mut self, session: &str) {
        self.metrics.remove(session);
        self.page_sessions.remove(session);
        self.forget_drawn(session);
    }

    fn forget_drawn(&mut self, session: &str) {
        if self.drawn.as_ref().is_some_and(|d| d.session == session) {
            self.drawn = None;
            self.set_visible(None);
        }
        if self
            .hidden_for_capture
            .as_ref()
            .is_some_and(|d| d.session == session)
        {
            self.hidden_for_capture = None;
        }
    }

    fn set_state(&self, state: State, reason: Option<String>) {
        let mut s = self.shared.lock().unwrap();
        s.state = state;
        s.reason = reason;
    }

    fn set_visible(&self, session: Option<String>) {
        self.shared.lock().unwrap().visible = session;
    }

    async fn is_page(&mut self, session: &str) -> Option<bool> {
        if let Some(&p) = self.page_sessions.get(session) {
            return Some(p);
        }
        let info = self
            .link
            .call("Target.getTargetInfo", json!({}), session, COMMAND_TIMEOUT)
            .await
            .ok()?;
        let page = info["targetInfo"]["type"].as_str() == Some("page");
        self.page_sessions.insert(session.to_string(), page);
        Some(page)
    }

    async fn metrics(&mut self, session: &str) -> Result<Metrics, LinkError> {
        if let Some(m) = self.metrics.get(session) {
            if m.at.elapsed() < METRICS_TTL {
                return Ok(*m);
            }
        }
        let v = self
            .link
            .call("Page.getLayoutMetrics", json!({}), session, COMMAND_TIMEOUT)
            .await?;
        let css = &v["cssVisualViewport"];
        let m = Metrics {
            zoom: css["zoom"].as_f64().unwrap_or(1.0),
            scale: css["scale"].as_f64().unwrap_or(1.0),
            width: css["clientWidth"].as_f64().unwrap_or(0.0),
            height: css["clientHeight"].as_f64().unwrap_or(0.0),
            at: Instant::now(),
        };
        self.metrics.insert(session.to_string(), m);
        Ok(m)
    }

    fn suppress(&self, reason: &str) {
        let mut s = self.shared.lock().unwrap();
        s.state = State::Suppressed;
        s.reason = Some(reason.to_string());
        s.suppressed += 1;
    }

    async fn draw(&mut self, session: &str, x: f64, y: f64) {
        if !Marker::refresh_setting(&self.shared, self.read_setting) {
            if let Some(d) = self.drawn.take() {
                let _ = self.clear(&d.session).await;
                self.set_visible(None);
            }
            return;
        }
        if self.shared.lock().unwrap().state == State::Unsupported {
            return;
        }
        // One marker at a time: a point on another tab clears the old one.
        if let Some(d) = self.drawn.as_ref().filter(|d| d.session != session) {
            let old = d.session.clone();
            self.drawn = None;
            let _ = self.clear(&old).await;
            self.set_visible(None);
        }
        self.hidden_for_capture = None;
        match self.is_page(session).await {
            Some(true) => {}
            Some(false) => return self.suppress("the point is in an iframe's own coordinates"),
            None => return self.suppress("could not confirm the target is a page"),
        }
        let m = match self.metrics(session).await {
            Ok(m) => m,
            Err(e) if e.unsupported() => {
                return self.set_state(
                    State::Unsupported,
                    Some(format!("Page.getLayoutMetrics: {}", e.describe())),
                )
            }
            Err(e) => {
                return self.suppress(&format!(
                    "the page zoom could not be read ({})",
                    e.describe()
                ))
            }
        };
        let config = match hinge_rect(x, y, (m.zoom, m.scale, m.width, m.height)) {
            Ok(c) => c,
            Err(why) => {
                // A stale marker elsewhere would mislead more than none.
                if let Some(d) = self.drawn.take() {
                    let _ = self.clear(&d.session).await;
                    self.set_visible(None);
                }
                return self.suppress(why);
            }
        };
        self.generation += 1;
        let generation = self.generation;
        let expires = Instant::now() + EXPIRE_AFTER;
        let same = self.drawn.as_ref().is_some_and(|d| {
            d.session == session && d.config == config && d.sent.elapsed() < RESEND_AFTER
        });
        {
            let mut s = self.shared.lock().unwrap();
            s.marks += 1;
            s.last_at = Some(Instant::now());
        }
        if same {
            // Already there: only push the expiry back.
            if let Some(d) = self.drawn.as_mut() {
                d.expires = expires;
            }
        } else {
            self.set_visible(Some(session.to_string()));
            match self
                .link
                .call(
                    "Overlay.setShowHinge",
                    json!({ "hingeConfig": config }),
                    session,
                    COMMAND_TIMEOUT,
                )
                .await
            {
                Ok(_) => self.set_state(State::Acknowledged, None),
                Err(e) if e.unsupported() => {
                    self.set_visible(None);
                    return self.set_state(
                        State::Unsupported,
                        Some(format!("Overlay.setShowHinge: {}", e.describe())),
                    );
                }
                Err(e) => self.set_state(
                    State::Unconfirmed,
                    Some(format!("Overlay.setShowHinge: {}", e.describe())),
                ),
            }
            self.drawn = Some(Drawn {
                session: session.to_string(),
                config,
                expires,
                sent: Instant::now(),
            });
        }
        let tx = self.tx.clone();
        tokio::spawn(async move {
            tokio::time::sleep(EXPIRE_AFTER).await;
            let _ = tx.send(Msg::Expire { generation });
        });
    }

    async fn clear(&self, session: &str) -> Result<Value, LinkError> {
        let r = self
            .link
            .call("Overlay.setShowHinge", json!({}), session, COMMAND_TIMEOUT)
            .await;
        let mut s = self.shared.lock().unwrap();
        if s.state == State::Acknowledged || s.state == State::Unconfirmed {
            s.state = if r.is_ok() {
                State::Idle
            } else {
                State::Unconfirmed
            };
            if let Err(e) = &r {
                s.reason = Some(format!("clearing the marker: {}", e.describe()));
            }
        }
        r
    }

    async fn hide(&mut self, session: &str) -> CaptureHide {
        let Some(d) = self.drawn.take() else {
            return CaptureHide::NotShown;
        };
        if d.session != session {
            self.drawn = Some(d);
            return CaptureHide::NotShown;
        }
        let r = tokio::time::timeout(
            CAPTURE_HIDE_TIMEOUT,
            self.link.call(
                "Overlay.setShowHinge",
                json!({}),
                session,
                CAPTURE_HIDE_TIMEOUT,
            ),
        )
        .await;
        self.set_visible(None);
        self.hidden_for_capture = Some(d);
        match r {
            Ok(Ok(_)) => CaptureHide::Hidden,
            _ => CaptureHide::Unconfirmed,
        }
    }

    async fn restore(&mut self, session: &str) {
        let Some(mut d) = self.hidden_for_capture.take() else {
            return;
        };
        if d.session != session || Instant::now() >= d.expires {
            return;
        }
        if !Marker::refresh_setting(&self.shared, self.read_setting) {
            return;
        }
        self.set_visible(Some(session.to_string()));
        let r = self
            .link
            .call(
                "Overlay.setShowHinge",
                json!({ "hingeConfig": d.config }),
                session,
                COMMAND_TIMEOUT,
            )
            .await;
        if r.is_err() {
            self.set_state(
                State::Unconfirmed,
                Some("restoring the marker after a screenshot got no reply".into()),
            );
        }
        d.sent = Instant::now();
        self.drawn = Some(d);
    }
}

/// The next event, or never when there is no event stream (so the worker's
/// `select!` waits on its queue alone instead of spinning).
async fn recv_event(events: &mut Option<broadcast::Receiver<CdpEvent>>) -> Option<CdpEvent> {
    let Some(rx) = events.as_mut() else {
        return std::future::pending().await;
    };
    loop {
        match rx.recv().await {
            Ok(e) => return Some(e),
            Err(broadcast::error::RecvError::Lagged(_)) => continue,
            Err(broadcast::error::RecvError::Closed) => return None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;

    #[test]
    fn setting_env_then_config_then_off() {
        let on = json!({"showPosition": true});
        assert_eq!(resolve_setting(None, None), (false, Source::Default));
        assert_eq!(resolve_setting(None, Some(&on)), (true, Source::Config));
        assert_eq!(resolve_setting(Some("0"), Some(&on)), (false, Source::Env));
        assert_eq!(resolve_setting(Some("ON"), None), (true, Source::Env));
        // Anything else in the env falls through to the config.
        assert_eq!(
            resolve_setting(Some("maybe"), Some(&on)),
            (true, Source::Config)
        );
    }

    #[test]
    fn rect_scales_css_points_by_page_zoom() {
        let r = hinge_rect(300.0, 220.0, (1.0, 1.0, 1000.0, 600.0)).unwrap();
        assert_eq!(
            r["rect"],
            json!({"x": 292, "y": 212, "width": 16, "height": 16})
        );
        // 200% page zoom: the same CSS point is twice as far in DIP.
        let r = hinge_rect(300.0, 220.0, (2.0, 1.0, 500.0, 300.0)).unwrap();
        assert_eq!(r["rect"]["x"], 592);
        assert_eq!(r["rect"]["y"], 432);
        assert!(hinge_rect(10.0, 10.0, (1.0, 1.5, 1000.0, 600.0)).is_err());
        assert!(hinge_rect(1200.0, 10.0, (1.0, 1.0, 1000.0, 600.0)).is_err());
        assert!(hinge_rect(-1.0, 10.0, (1.0, 1.0, 1000.0, 600.0)).is_err());
    }

    #[test]
    fn unsupported_errors() {
        let p = |code, m: &str| LinkError::Protocol {
            code,
            message: m.into(),
        };
        assert!(p(Some(-32601), "'Overlay.setShowHinge' wasn't found").unsupported());
        assert!(p(None, "Method not allowed in stealth mode").unsupported());
        assert!(!p(Some(-32000), "Not attached to an active page").unsupported());
        assert!(!LinkError::Timeout.unsupported());
    }

    /// Records calls; answers from a script keyed by method.
    struct FakeLink {
        calls: StdMutex<Vec<(String, Value, String)>>,
        hinge: StdMutex<Result<Value, LinkError>>,
        target_type: &'static str,
        zoom: f64,
        delay: Duration,
    }

    impl Link for FakeLink {
        fn call<'a>(
            &'a self,
            method: &'a str,
            params: Value,
            session: &'a str,
            _timeout: Duration,
        ) -> Pin<Box<dyn Future<Output = Result<Value, LinkError>> + Send + 'a>> {
            Box::pin(async move {
                self.calls
                    .lock()
                    .unwrap()
                    .push((method.to_string(), params, session.to_string()));
                match method {
                    "Target.getTargetInfo" => Ok(json!({"targetInfo": {"type": self.target_type}})),
                    "Page.getLayoutMetrics" => Ok(json!({"cssVisualViewport": {
                        "zoom": self.zoom, "scale": 1.0, "clientWidth": 1000.0, "clientHeight": 600.0}})),
                    _ => {
                        if !self.delay.is_zero() {
                            tokio::time::sleep(self.delay).await;
                        }
                        self.hinge.lock().unwrap().clone()
                    }
                }
            })
        }
    }

    fn fake(target_type: &'static str, zoom: f64) -> Arc<FakeLink> {
        Arc::new(FakeLink {
            calls: StdMutex::new(Vec::new()),
            hinge: StdMutex::new(Ok(json!({}))),
            target_type,
            zoom,
            delay: Duration::ZERO,
        })
    }

    fn on() -> (bool, Source) {
        (true, Source::Env)
    }

    fn hinges(link: &FakeLink) -> Vec<(Value, String)> {
        link.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(m, _, _)| m == "Overlay.setShowHinge")
            .map(|(_, p, s)| (p.clone(), s.clone()))
            .collect()
    }

    async fn settle() {
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    fn press(x: f64, y: f64) -> Value {
        json!({"type": "mousePressed", "x": x, "y": y, "button": "left"})
    }

    #[tokio::test]
    async fn a_press_draws_once_and_moves_are_ignored() {
        let link = fake("page", 1.5);
        let m = Marker::start(link.clone(), None, on);
        m.note_input(
            "Input.dispatchMouseEvent",
            Some(&json!({"type": "mouseMoved", "x": 1, "y": 1})),
            Some("S"),
        );
        m.note_input(
            "Input.dispatchMouseEvent",
            Some(&press(100.0, 50.0)),
            Some("S"),
        );
        m.note_input(
            "Input.dispatchMouseEvent",
            Some(&json!({"type": "mouseReleased", "x": 100.0, "y": 50.0})),
            Some("S"),
        );
        settle().await;
        let h = hinges(&link);
        // The release at the same point only extends the expiry.
        assert_eq!(h.len(), 1, "{h:?}");
        assert_eq!(h[0].0["hingeConfig"]["rect"]["x"], 142); // 100 * 1.5 - 8
        assert_eq!(m.status()["state"], "acknowledged");
    }

    #[tokio::test]
    async fn iframe_sessions_are_suppressed() {
        let link = fake("iframe", 1.0);
        let m = Marker::start(link.clone(), None, on);
        m.mark("F", 10.0, 10.0);
        settle().await;
        assert!(hinges(&link).is_empty());
        assert_eq!(m.status()["state"], "suppressed");
    }

    #[tokio::test]
    async fn unsupported_stops_further_attempts() {
        let link = fake("page", 1.0);
        *link.hinge.lock().unwrap() = Err(LinkError::Protocol {
            code: Some(-32601),
            message: "'Overlay.setShowHinge' wasn't found".into(),
        });
        let m = Marker::start(link.clone(), None, on);
        m.mark("S", 10.0, 10.0);
        settle().await;
        m.mark("S", 20.0, 20.0);
        settle().await;
        assert_eq!(hinges(&link).len(), 1);
        assert_eq!(m.status()["state"], "unsupported");
    }

    #[tokio::test]
    async fn capture_hides_then_restores() {
        let link = fake("page", 1.0);
        let m = Marker::start(link.clone(), None, on);
        assert_eq!(m.hide_for_capture("S").await, CaptureHide::NotShown);
        m.mark("S", 10.0, 10.0);
        // Queued but not drawn yet: the capture still waits for it.
        let hidden = m.hide_for_capture("S").await;
        assert_eq!(hidden, CaptureHide::Hidden);
        m.restore_after_capture("S", hidden);
        settle().await;
        let h = hinges(&link);
        assert_eq!(h.len(), 3, "{h:?}");
        assert!(h[0].0.get("hingeConfig").is_some());
        assert_eq!(h[1].0, json!({}));
        assert!(h[2].0.get("hingeConfig").is_some());
    }

    #[tokio::test]
    async fn a_hung_hide_is_reported_unconfirmed() {
        let link = Arc::new(FakeLink {
            calls: StdMutex::new(Vec::new()),
            hinge: StdMutex::new(Ok(json!({}))),
            target_type: "page",
            zoom: 1.0,
            delay: Duration::from_millis(1500),
        });
        let m = Marker::start(link.clone(), None, on);
        m.mark("S", 10.0, 10.0);
        let started = Instant::now();
        let hidden = m.hide_for_capture("S").await;
        assert_eq!(hidden, CaptureHide::Unconfirmed);
        assert!(started.elapsed() < Duration::from_millis(900));
        assert!(hidden.warning().is_some());
    }

    #[tokio::test]
    async fn another_tab_clears_the_old_marker() {
        let link = fake("page", 1.0);
        let m = Marker::start(link.clone(), None, on);
        m.mark("A", 10.0, 10.0);
        settle().await;
        m.mark("B", 10.0, 10.0);
        settle().await;
        let h = hinges(&link);
        assert_eq!(h.len(), 3, "{h:?}");
        assert_eq!((h[1].0.clone(), h[1].1.as_str()), (json!({}), "A"));
        assert_eq!(h[2].1, "B");
    }

    #[tokio::test]
    async fn navigation_forgets_the_marker() {
        let link = fake("page", 1.0);
        let (etx, erx) = broadcast::channel(16);
        let m = Marker::start(link.clone(), Some(erx), on);
        m.mark("S", 10.0, 10.0);
        settle().await;
        etx.send(CdpEvent {
            method: "Page.frameNavigated".into(),
            params: json!({"frame": {"id": "main"}}),
            session_id: Some("S".into()),
            received_at: Instant::now(),
        })
        .unwrap();
        settle().await;
        // The browser dropped it; a capture has nothing to hide.
        assert_eq!(m.hide_for_capture("S").await, CaptureHide::NotShown);
    }

    #[tokio::test]
    async fn it_expires() {
        let link = fake("page", 1.0);
        let m = Marker::start(link.clone(), None, on);
        m.mark("S", 10.0, 10.0);
        settle().await;
        tokio::time::sleep(EXPIRE_AFTER + Duration::from_millis(50)).await;
        settle().await;
        let h = hinges(&link);
        assert_eq!(h.last().unwrap().0, json!({}), "{h:?}");
        assert_eq!(m.status()["state"], "idle");
    }

    #[tokio::test]
    async fn off_by_default_sends_nothing() {
        fn off() -> (bool, Source) {
            (false, Source::Default)
        }
        let link = fake("page", 1.0);
        let m = Marker::start(link.clone(), None, off);
        m.mark("S", 1.0, 1.0);
        m.note_input(
            "Input.dispatchMouseEvent",
            Some(&press(1.0, 1.0)),
            Some("S"),
        );
        // Even a point that reaches the worker is dropped while off.
        let _ = m.tx.send(Msg::Point {
            session: "S".into(),
            x: 1.0,
            y: 1.0,
        });
        settle().await;
        assert!(link.calls.lock().unwrap().is_empty());
        assert_eq!(m.status()["state"], "disabled");
    }
}

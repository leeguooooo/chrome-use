//! `chrome-use diag pages` (#521): one stable, read-only, versioned report of
//! the real pages in the connected Chrome profile, for tools outside
//! chrome-use (Thermo) to shell out to instead of driving the relay with raw
//! CDP.
//!
//! It runs in this CLI process on a diagnostic connection of its own: no
//! daemon is started, no session tab is opened, and the relay is told not to
//! re-attach anything (`x-chrome-use-diagnostic`). What it may touch:
//!
//! - Without `--measure`: nothing. Over the relay it reads `ABExt.state`,
//!   `chrome.tabs.query`, `chrome.windows.getAll`, the relay's own target
//!   records and `ABExt.tabPresence`; on direct CDP `Target.getTargets` and
//!   `Browser.getWindowForTarget`. No page is attached, activated or read.
//! - With `--measure` (and `--watch`, which implies it): pages this session
//!   owns, and pages chrome-use already holds attached that no other live
//!   session holds, are measured as they are (`Performance.getMetrics`).
//! - With `--measure --force`: every other page too. A tab that is not
//!   attached is attached (`ABExt.attachTabById`, never activated or
//!   focused), measured, and released at once (`ABExt.releaseTab`,
//!   ab-connect 0.5.34+). On direct CDP each measurement is a private session
//!   that is detached right after.
//!
//! The JSON shape is versioned (`schema: 1`) and written down in
//! `skill-data/core/references/diag-pages.md`; a change that breaks a reader
//! of that document needs a new schema number.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::{Duration, Instant};

use futures_util::stream::{self, StreamExt};
use serde_json::{json, Value};

use crate::color;
use crate::connect;
use crate::connection;
use crate::flags::Flags;
use crate::native::browser::{RelayHoldings, TabOwner};
use crate::native::cdp::client::CdpClient;
use crate::native::cdp::types::TargetInfo;

/// The output schema. Bumped only when a reader of schema 1 would break.
pub const SCHEMA_VERSION: u64 = 1;

/// Pages listed unless `--limit` says otherwise, and the most it may ask.
pub const DEFAULT_PAGE_LIMIT: usize = 200;
pub const MAX_PAGE_LIMIT: usize = 1000;
/// Per-field caps, in characters; past them the field is cut and the row
/// says so (`titleCut` / `urlCut`).
pub const TITLE_MAX_CHARS: usize = 300;
pub const URL_MAX_CHARS: usize = 2048;
/// Worker sites listed; the rest are counted in `workers.omittedSites`.
pub const MAX_WORKER_SITES: usize = 50;
/// The longest `--watch` interval.
pub const MAX_WATCH_SECONDS: u64 = 3600;

/// `heap_growing`: the JS heap grows at least this fast (1.5 MiB a minute).
pub const HEAP_GROWING_BYTES_PER_MIN: f64 = 1.5 * 1024.0 * 1024.0;
/// Listeners grow when they rise by at least this many, and by at least
/// [`LISTENERS_GROWTH_REL`] of the first sample.
pub const LISTENERS_GROWTH_MIN: f64 = 20.0;
pub const LISTENERS_GROWTH_REL: f64 = 0.02;
/// Nodes are flat while they move by at most this many (or
/// [`NODES_FLAT_REL`] of the first sample, whichever is larger); past that
/// upward they climb.
pub const NODES_FLAT_ABS: f64 = 100.0;
pub const NODES_FLAT_REL: f64 = 0.02;

pub const LEAK_LISTENERS_NODES_FLAT: &str = "listeners_growing_nodes_flat";
pub const LEAK_HEAP_GROWING: &str = "heap_growing";
pub const LEAK_NODES_CLIMBING: &str = "nodes_climbing";
pub const LEAK_NONE: &str = "none";

/// Exit codes.
pub const EXIT_OK: i32 = 0;
pub const EXIT_USAGE: i32 = 1;
pub const EXIT_NOT_CONNECTED: i32 = 2;
pub const EXIT_HELD_BY_OTHER_SESSION: i32 = 3;

/// How long connecting to the endpoint may take.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// How long one read or one measuring call may take.
const CALL_TIMEOUT: Duration = Duration::from_secs(4);
/// How long dedicated workers have to show up after auto-attach (CDP).
const WORKER_DISCOVERY_WINDOW: Duration = Duration::from_millis(300);
/// Pages measured at once.
const MEASURE_CONCURRENCY: usize = 4;
/// First ab-connect that reads `chrome.tabs` through `ABExt.call`.
const TABS_MIN_EXTENSION_VERSION: &str = "0.5.25";
/// Extension capability for `ABExt.releaseTab` (ab-connect 0.5.34).
const RELEASE_TAB_CAPABILITY: &str = "releaseTab";
const RELEASE_TAB_MIN_EXTENSION_VERSION: &str = "0.5.34";
/// Worker target types reported per site.
const WORKER_TYPES: &[&str] = &["worker", "shared_worker", "service_worker"];

pub const USAGE: &str =
    "chrome-use diag pages [--measure] [--watch <seconds>] [--force] [--limit <n>] [--json]";

// ---------------------------------------------------------------------------
// Options
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagOptions {
    pub measure: bool,
    pub force: bool,
    pub watch: Option<u64>,
    pub limit: usize,
}

/// Parse the words after `diag` (`pages ...`). `--watch` implies
/// `--measure`; `--force` without either is refused, since without them
/// nothing is attached and there is nothing to force.
pub fn parse_args(args: &[String]) -> Result<DiagOptions, String> {
    let mut it = args.iter().map(String::as_str);
    match it.next() {
        Some("pages") => {}
        Some(other) => {
            return Err(format!(
                "unknown diag subcommand `{other}`; the one there is: `diag pages`"
            ))
        }
        None => return Err("diag needs a subcommand: `diag pages`".to_string()),
    }
    let mut opts = DiagOptions {
        measure: false,
        force: false,
        watch: None,
        limit: DEFAULT_PAGE_LIMIT,
    };
    while let Some(arg) = it.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f, Some(v.to_string())),
            _ => (arg, None),
        };
        let mut value = |name: &str| -> Result<String, String> {
            inline
                .clone()
                .or_else(|| it.next().map(str::to_string))
                .ok_or_else(|| format!("{name} needs a value"))
        };
        match flag {
            "--measure" => opts.measure = true,
            "--force" => opts.force = true,
            "--watch" => {
                let raw = value("--watch")?;
                let secs = raw
                    .parse::<u64>()
                    .ok()
                    .filter(|n| (1..=MAX_WATCH_SECONDS).contains(n))
                    .ok_or_else(|| {
                        format!(
                            "--watch takes whole seconds from 1 to {MAX_WATCH_SECONDS}, got {raw:?}"
                        )
                    })?;
                opts.watch = Some(secs);
            }
            "--limit" => {
                let raw = value("--limit")?;
                opts.limit = raw
                    .parse::<usize>()
                    .ok()
                    .filter(|n| (1..=MAX_PAGE_LIMIT).contains(n))
                    .ok_or_else(|| {
                        format!("--limit takes a number of pages from 1 to {MAX_PAGE_LIMIT}, got {raw:?}")
                    })?;
            }
            other => return Err(format!("unknown option `{other}` for diag pages")),
        }
    }
    if opts.watch.is_some() {
        opts.measure = true;
    }
    if opts.force && !opts.measure {
        return Err(
            "--force applies only with --measure or --watch: without them diag pages attaches \
             nothing, so there is nothing to force"
                .to_string(),
        );
    }
    Ok(opts)
}

// ---------------------------------------------------------------------------
// Metrics and leak classes (pure)
// ---------------------------------------------------------------------------

/// One `Performance.getMetrics` sample of a page.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Metrics {
    pub js_heap_used: f64,
    pub js_heap_total: f64,
    pub nodes: f64,
    pub listeners: f64,
    pub documents: f64,
}

impl Metrics {
    /// From a `Performance.getMetrics` reply. `None` unless it has the three
    /// a leak class needs.
    pub fn from_cdp(reply: &Value) -> Option<Self> {
        let mut by_name: HashMap<&str, f64> = HashMap::new();
        for m in reply.get("metrics")?.as_array()? {
            if let (Some(name), Some(value)) = (
                m.get("name").and_then(Value::as_str),
                m.get("value").and_then(Value::as_f64),
            ) {
                by_name.insert(name, value);
            }
        }
        Some(Metrics {
            js_heap_used: *by_name.get("JSHeapUsedSize")?,
            js_heap_total: by_name.get("JSHeapTotalSize").copied().unwrap_or(0.0),
            nodes: *by_name.get("Nodes")?,
            listeners: *by_name.get("JSEventListeners")?,
            documents: by_name.get("Documents").copied().unwrap_or(0.0),
        })
    }

    pub fn to_json(self) -> Value {
        json!({
            "JSHeapUsedSize": int(self.js_heap_used),
            "JSHeapTotalSize": int(self.js_heap_total),
            "Nodes": int(self.nodes),
            "JSEventListeners": int(self.listeners),
            "Documents": int(self.documents),
        })
    }
}

fn int(x: f64) -> i64 {
    x.round() as i64
}

/// What two samples of one page say.
#[derive(Debug, Clone, PartialEq)]
pub struct LeakVerdict {
    /// One of the `LEAK_*` classes.
    pub class: &'static str,
    /// Every signal that fired: `listeners_growing`, `nodes_flat`,
    /// `heap_growing`, `nodes_climbing`.
    pub signals: Vec<&'static str>,
    pub heap_bytes_per_minute: f64,
}

/// Classify two samples `seconds` apart. The first class that applies:
///
/// 1. `listeners_growing_nodes_flat`: listeners rose by at least
///    max(20, 2 % of the first sample) while nodes moved by at most
///    max(100, 2 % of the first sample) — handlers added again and again to
///    the same elements.
/// 2. `heap_growing`: the JS heap grew at 1.5 MiB a minute or more.
/// 3. `nodes_climbing`: nodes rose by more than the flat tolerance.
/// 4. `none`.
pub fn classify_leak(before: &Metrics, after: &Metrics, seconds: f64) -> LeakVerdict {
    let minutes = seconds.max(0.001) / 60.0;
    let heap_rate = (after.js_heap_used - before.js_heap_used) / minutes;
    let d_nodes = after.nodes - before.nodes;
    let d_listeners = after.listeners - before.listeners;
    let listener_tolerance = LISTENERS_GROWTH_MIN.max(LISTENERS_GROWTH_REL * before.listeners);
    let node_tolerance = NODES_FLAT_ABS.max(NODES_FLAT_REL * before.nodes);
    let listeners_growing = d_listeners >= listener_tolerance;
    let nodes_flat = d_nodes.abs() <= node_tolerance;
    let nodes_climbing = d_nodes > node_tolerance;
    let heap_growing = heap_rate >= HEAP_GROWING_BYTES_PER_MIN;
    let mut signals = Vec::new();
    if listeners_growing {
        signals.push("listeners_growing");
    }
    if nodes_flat {
        signals.push("nodes_flat");
    }
    if heap_growing {
        signals.push("heap_growing");
    }
    if nodes_climbing {
        signals.push("nodes_climbing");
    }
    let class = if listeners_growing && nodes_flat {
        LEAK_LISTENERS_NODES_FLAT
    } else if heap_growing {
        LEAK_HEAP_GROWING
    } else if nodes_climbing {
        LEAK_NODES_CLIMBING
    } else {
        LEAK_NONE
    };
    LeakVerdict {
        class,
        signals,
        heap_bytes_per_minute: heap_rate,
    }
}

/// The `delta` object of a watched page.
pub fn delta_json(before: &Metrics, after: &Metrics, seconds: f64, verdict: &LeakVerdict) -> Value {
    json!({
        "seconds": (seconds * 1000.0).round() / 1000.0,
        "JSHeapUsedSize": int(after.js_heap_used - before.js_heap_used),
        "JSHeapTotalSize": int(after.js_heap_total - before.js_heap_total),
        "Nodes": int(after.nodes - before.nodes),
        "JSEventListeners": int(after.listeners - before.listeners),
        "Documents": int(after.documents - before.documents),
        "heapBytesPerMinute": int(verdict.heap_bytes_per_minute),
    })
}

/// Exit code 3: `--measure` (or `--watch`) without `--force`, nothing was
/// measured, and every page of the profile is held by another chrome-use
/// session whose daemon is running. With no pages, or any page the user's,
/// this session's or a session that is not running, the run exits 0 and
/// each skipped page says why.
pub fn exit_code_for(measure: bool, force: bool, owners: &[TabOwner], measured: usize) -> i32 {
    let all_other_live = !owners.is_empty()
        && owners.iter().all(|o| {
            matches!(
                o,
                TabOwner::Session {
                    live: Some(true),
                    ..
                }
            )
        });
    if measure && !force && measured == 0 && all_other_live {
        EXIT_HELD_BY_OTHER_SESSION
    } else {
        EXIT_OK
    }
}

// ---------------------------------------------------------------------------
// Measuring plan (pure)
// ---------------------------------------------------------------------------

/// How one page will be measured, or why not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    NotRequested,
    /// Relay: the extension holds it attached; measure on its session.
    Existing,
    /// Relay: attach it, measure, release it.
    ForcedAttach,
    /// Direct CDP: a private session, detached right after.
    CdpSession {
        forced: bool,
    },
    Skip {
        code: &'static str,
        detail: String,
    },
}

fn other_live_session(owner: &TabOwner) -> bool {
    matches!(
        owner,
        TabOwner::Session {
            live: Some(true),
            ..
        }
    )
}

fn skip(code: &'static str, detail: impl Into<String>) -> Plan {
    Plan::Skip {
        code,
        detail: detail.into(),
    }
}

fn needs_force_detail(owner: &TabOwner) -> String {
    format!(
        "not measured: it belongs to {} and is not attached; measuring would attach it and show \
         Chrome's debugging bar on it (pass --force to measure it and detach at once)",
        owner.label()
    )
}

fn held_detail(owner: &TabOwner) -> String {
    format!(
        "not measured: {} holds this tab (pass --force to measure it as it is)",
        owner.label()
    )
}

/// The plan for one relay tab.
pub fn plan_relay(
    measure: bool,
    force: bool,
    owner: &TabOwner,
    attached: bool,
    discarded: bool,
    can_release: bool,
) -> Plan {
    if !measure {
        return Plan::NotRequested;
    }
    if discarded {
        return skip(
            "discarded",
            "not measured: Chrome discarded this tab to save memory; attaching would reload it",
        );
    }
    if attached {
        if other_live_session(owner) && !force {
            return skip("held_by_other_session", held_detail(owner));
        }
        return Plan::Existing;
    }
    if !force {
        return skip("needs_force", needs_force_detail(owner));
    }
    if !can_release {
        return skip(
            "release_unsupported",
            format!(
                "not measured: releasing a tab after measuring it needs ab-connect \
                 {RELEASE_TAB_MIN_EXTENSION_VERSION}; this extension would keep it attached"
            ),
        );
    }
    Plan::ForcedAttach
}

/// The plan for one direct-CDP page. A private session is invisible on a
/// debugging port, but the same ownership rule applies.
pub fn plan_cdp(measure: bool, force: bool, owner: &TabOwner, attached: bool) -> Plan {
    if !measure {
        return Plan::NotRequested;
    }
    if *owner == TabOwner::This {
        return Plan::CdpSession { forced: false };
    }
    if other_live_session(owner) && !force {
        return skip("held_by_other_session", held_detail(owner));
    }
    if attached || force {
        return Plan::CdpSession { forced: !attached };
    }
    skip("needs_force", needs_force_detail(owner))
}

// ---------------------------------------------------------------------------
// Observations
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
enum Outcome {
    NotRequested,
    Skipped {
        code: &'static str,
        detail: String,
    },
    Measured {
        metrics: Metrics,
        via: &'static str,
        forced: bool,
        at: Instant,
    },
    Failed {
        code: &'static str,
        detail: String,
        via: &'static str,
    },
}

/// One real page of the profile.
#[derive(Debug, Clone)]
struct PageObs {
    handle: String,
    target_id: Option<String>,
    chrome_tab_id: Option<i64>,
    window_id: Option<i64>,
    index: Option<i64>,
    title: String,
    url: String,
    active: Option<bool>,
    visible: Option<bool>,
    discarded: Option<bool>,
    attached: Option<bool>,
    owner: TabOwner,
    plan: Plan,
    outcome: Outcome,
}

#[derive(Debug, Clone)]
struct WorkerObs {
    target_id: String,
    kind: String,
    url: String,
    /// Relay: the worker's session in the relay's records.
    session: Option<String>,
    heap: Option<(f64, f64)>,
}

/// One full read of the profile.
struct Sample {
    pages: Vec<PageObs>,
    total: usize,
    windows: usize,
    stale: Option<Value>,
    workers: Vec<WorkerObs>,
    workers_note: Option<String>,
    workers_measured: bool,
    state: Option<Value>,
    forced: Vec<Value>,
    notes: Vec<String>,
}

fn clip(text: &str, max: usize) -> (String, bool) {
    match text.char_indices().nth(max) {
        Some((end, _)) => (text[..end].to_string(), true),
        None => (text.to_string(), false),
    }
}

fn rank(owner: &TabOwner) -> (u8, u8, String) {
    match owner {
        TabOwner::This => (0, 0, String::new()),
        TabOwner::Session { name, .. } => (
            1,
            u8::from(name.is_none()),
            name.clone().unwrap_or_default(),
        ),
        TabOwner::User => (2, 0, String::new()),
    }
}

fn sort_pages(pages: &mut [PageObs]) {
    pages.sort_by_key(|p| {
        let (r, unnamed, name) = rank(&p.owner);
        (
            r,
            unnamed,
            name,
            p.window_id.unwrap_or(i64::MAX),
            p.index.unwrap_or(i64::MAX),
        )
    });
}

fn measure_json(p: &PageObs) -> Value {
    match &p.outcome {
        Outcome::NotRequested => json!({
            "status": "not_requested", "reason": null, "detail": null, "via": null, "forced": false,
        }),
        Outcome::Skipped { code, detail } => json!({
            "status": "skipped", "reason": code, "detail": detail, "via": null, "forced": false,
        }),
        Outcome::Measured { via, forced, .. } => json!({
            "status": "measured", "reason": null, "detail": null, "via": via, "forced": forced,
        }),
        Outcome::Failed { code, detail, via } => json!({
            "status": "failed", "reason": code, "detail": detail, "via": via,
            "forced": matches!(p.plan, Plan::ForcedAttach | Plan::CdpSession { forced: true }),
        }),
    }
}

fn metrics_of(p: &PageObs) -> Option<(Metrics, Instant)> {
    match &p.outcome {
        Outcome::Measured { metrics, at, .. } => Some((*metrics, *at)),
        _ => None,
    }
}

/// One page row of the schema. Every key is always present.
fn page_json(p: &PageObs) -> Value {
    let (title, title_cut) = clip(&p.title, TITLE_MAX_CHARS);
    let (url, url_cut) = clip(&p.url, URL_MAX_CHARS);
    json!({
        "handle": p.handle,
        "targetId": p.target_id,
        "chromeTabId": p.chrome_tab_id,
        "windowId": p.window_id,
        "index": p.index,
        "title": title,
        "titleCut": title_cut,
        "url": url,
        "urlCut": url_cut,
        "active": p.active,
        "visible": p.visible,
        "discarded": p.discarded,
        "attached": p.attached,
        "owner": p.owner.to_json(),
        "ownerLabel": p.owner.label(),
        "rendererPid": null,
        "metrics": metrics_of(p).map(|(m, _)| m.to_json()),
        "metricsBefore": null,
        "measure": measure_json(p),
        "delta": null,
        "leakClass": null,
        "leakSignals": [],
    })
}

/// The `site` a worker url belongs to: its origin (a `blob:` url's inner
/// origin), or the scheme for an opaque one.
pub fn worker_site(url: &str) -> String {
    let inner = url.strip_prefix("blob:").unwrap_or(url);
    match url::Url::parse(inner) {
        Ok(u) => {
            let origin = u.origin();
            if origin.is_tuple() {
                origin.ascii_serialization()
            } else if let Some(host) = u.host_str().filter(|h| !h.is_empty()) {
                // chrome-extension://<id> and other non-special schemes.
                format!("{}://{host}", u.scheme())
            } else {
                format!("{}:", u.scheme())
            }
        }
        Err(_) if url.is_empty() => "(no url)".to_string(),
        Err(_) => "(opaque)".to_string(),
    }
}

/// Workers grouped per site, biggest first, capped.
fn workers_json(sample: &Sample, transport: &str) -> Value {
    #[derive(Default)]
    struct Site {
        count: usize,
        types: BTreeMap<String, usize>,
        measured: usize,
        used: f64,
        total: f64,
    }
    let mut sites: BTreeMap<String, Site> = BTreeMap::new();
    let mut measured = 0usize;
    for w in &sample.workers {
        let site = sites.entry(worker_site(&w.url)).or_default();
        site.count += 1;
        *site.types.entry(w.kind.clone()).or_default() += 1;
        if let Some((used, total)) = w.heap {
            site.measured += 1;
            site.used += used;
            site.total += total;
            measured += 1;
        }
    }
    let mut rows: Vec<(String, Site)> = sites.into_iter().collect();
    rows.sort_by(|a, b| b.1.count.cmp(&a.1.count).then_with(|| a.0.cmp(&b.0)));
    let omitted = rows.len().saturating_sub(MAX_WORKER_SITES);
    let shown: Vec<Value> = rows
        .into_iter()
        .take(MAX_WORKER_SITES)
        .map(|(name, s)| {
            json!({
                "site": clip(&name, URL_MAX_CHARS).0,
                "count": s.count,
                "types": s.types,
                "measured": s.measured,
                "usedSize": (s.measured > 0).then(|| int(s.used)),
                "totalSize": (s.measured > 0).then(|| int(s.total)),
            })
        })
        .collect();
    json!({
        "source": if transport == "relay" { "relay_records" } else { "cdp_targets" },
        "total": sample.workers.len(),
        "measured": measured,
        "measuredHeap": sample.workers_measured,
        "sites": shown,
        "omittedSites": omitted,
        "note": sample.workers_note,
    })
}

// ---------------------------------------------------------------------------
// Connection
// ---------------------------------------------------------------------------

struct Endpoint {
    relay: bool,
    ws: String,
    via: &'static str,
}

enum Unresolved {
    NotConnected(String),
    Invalid(String),
}

async fn resolve_endpoint(flags: &Flags) -> Result<Endpoint, Unresolved> {
    if let Some(cdp) = flags.cdp.as_deref() {
        if connect::is_relay_url(cdp) {
            return Ok(Endpoint {
                relay: true,
                ws: cdp.to_string(),
                via: "--cdp",
            });
        }
        let ws = tokio::time::timeout(
            CONNECT_TIMEOUT,
            crate::native::browser::resolve_cdp_url(cdp),
        )
        .await
        .map_err(|_| Unresolved::NotConnected(format!("the CDP endpoint {cdp} did not answer")))?
        .map_err(|e| Unresolved::NotConnected(format!("the CDP endpoint {cdp}: {e}")))?;
        return Ok(Endpoint {
            relay: false,
            ws,
            via: "--cdp",
        });
    }
    let selector = flags.browser.clone().or_else(|| {
        flags
            .profile
            .clone()
            .filter(|p| !flags.force_launch && crate::profiles::names_a_profile(p))
    });
    if let Some(sel) = selector {
        return match connect::relay_url_for_selector_or_default(Some(&sel)) {
            Ok(Some(ws)) => Ok(Endpoint {
                relay: true,
                ws,
                via: "--browser",
            }),
            Ok(None) => Err(Unresolved::NotConnected(format!(
                "no connected Chrome profile matches `{sel}`"
            ))),
            Err(e) => Err(Unresolved::NotConnected(e)),
        };
    }
    match connection::session_relay_profile(&flags.session) {
        Ok(Some(id)) => {
            return match connect::relay_endpoint_for_profile(&id) {
                Ok(ws) => Ok(Endpoint {
                    relay: true,
                    ws,
                    via: "session profile",
                }),
                Err(connect::ProfileEndpointError::NotConnected(e)) => {
                    Err(Unresolved::NotConnected(format!(
                        "session {} is bound to Chrome profile {id}, whose extension relay is \
                         not connected: {e}",
                        flags.session
                    )))
                }
                Err(e) => Err(Unresolved::Invalid(e.to_string())),
            }
        }
        Ok(None) => {}
        Err(e) => return Err(Unresolved::Invalid(e)),
    }
    match connect::relay_url_for_selector_or_default(None) {
        Ok(Some(ws)) => Ok(Endpoint {
            relay: true,
            ws,
            via: "default",
        }),
        Ok(None) => Err(Unresolved::NotConnected(
            "the chrome-use extension relay is not running: no Chrome profile with the \
             extension is connected (open Chrome with the extension, or check `chrome-use \
             status`)"
                .to_string(),
        )),
        Err(e) => Err(Unresolved::NotConnected(e)),
    }
}

async fn call(
    client: &CdpClient,
    method: &str,
    params: Value,
    session: Option<&str>,
) -> Result<Value, String> {
    match tokio::time::timeout(
        CALL_TIMEOUT,
        client.send_command(method, Some(params), session),
    )
    .await
    {
        Ok(r) => r,
        Err(_) => Err(format!(
            "{method} did not answer within {}s",
            CALL_TIMEOUT.as_secs()
        )),
    }
}

fn method_missing(error: &str) -> bool {
    let lower = error.to_ascii_lowercase();
    lower.contains("wasn't found") || lower.contains("method not found") || lower.contains("-32601")
}

async fn perf(client: &CdpClient, session: &str) -> Result<Metrics, String> {
    call(client, "Performance.enable", json!({}), Some(session)).await?;
    let reply = call(client, "Performance.getMetrics", json!({}), Some(session)).await;
    let _ = call(client, "Performance.disable", json!({}), Some(session)).await;
    let reply = reply?;
    Metrics::from_cdp(&reply).ok_or_else(|| {
        "Performance.getMetrics returned no JSHeapUsedSize / Nodes / JSEventListeners".to_string()
    })
}

fn heap_usage(v: &Value) -> Option<(f64, f64)> {
    Some((
        v.get("usedSize")?.as_f64()?,
        v.get("totalSize").and_then(Value::as_f64).unwrap_or(0.0),
    ))
}

fn failed(error: String, via: &'static str) -> Outcome {
    let code = if error.contains("did not answer") {
        "timeout"
    } else {
        "error"
    };
    Outcome::Failed {
        code,
        detail: error,
        via,
    }
}

// ---------------------------------------------------------------------------
// Relay
// ---------------------------------------------------------------------------

struct Ctx<'a> {
    client: &'a CdpClient,
    session: String,
    opts: &'a DiagOptions,
}

enum SampleError {
    /// The endpoint or the extension did not answer: exit 2.
    NotAnswering(String),
    /// Anything else: exit 1.
    Fatal(String),
}

fn relay_owner(
    chrome_tab: i64,
    group: Option<i64>,
    holdings: &RelayHoldings,
    others: &HashMap<String, String>,
    own_targets: &HashSet<String>,
    own_group: &str,
    live: &HashMap<String, bool>,
) -> TabOwner {
    if let Some(target) = holdings.attached.get(&chrome_tab) {
        if own_targets.contains(target) {
            return TabOwner::This;
        }
    }
    if group
        .and_then(|g| holdings.groups.get(&g))
        .is_some_and(|name| name == own_group)
    {
        return TabOwner::This;
    }
    holdings.owner_of(chrome_tab, group, others, |s| {
        live.get(s).copied().unwrap_or(false)
    })
}

fn live_sessions(records: &HashMap<String, String>) -> HashMap<String, bool> {
    records
        .values()
        .collect::<HashSet<_>>()
        .into_iter()
        .map(|s| (s.clone(), crate::native::browser::session_is_live(s)))
        .collect()
}

async fn sample_relay(ctx: &Ctx<'_>) -> Result<Sample, SampleError> {
    let client = ctx.client;
    let opts = ctx.opts;
    let state = call(client, "ABExt.state", json!({}), None)
        .await
        .map_err(|e| {
            SampleError::NotAnswering(format!("the extension did not answer ABExt.state: {e}"))
        })?;
    let tabs = match call(
        client,
        "ABExt.call",
        json!({ "namespace": "tabs", "method": "query", "args": [{}] }),
        None,
    )
    .await
    {
        Ok(v) => v,
        Err(e) if method_missing(&e) => {
            return Err(SampleError::Fatal(format!(
                "diag pages over the relay needs ab-connect {TABS_MIN_EXTENSION_VERSION} or \
                 newer (it reads chrome.tabs through ABExt.call); update the extension ({e})"
            )))
        }
        Err(e) => {
            return Err(SampleError::NotAnswering(format!(
                "the extension did not answer chrome.tabs.query: {e}"
            )))
        }
    };
    let tabs = tabs
        .get("result")
        .and_then(Value::as_array)
        .cloned()
        .ok_or_else(|| SampleError::Fatal("chrome.tabs.query returned no list".to_string()))?;
    let mut notes = Vec::new();
    // Window states, for `visible` (read-only).
    let windows: HashMap<i64, String> = match call(
        client,
        "ABExt.call",
        json!({ "namespace": "windows", "method": "getAll", "args": [{}] }),
        None,
    )
    .await
    {
        Ok(v) => v
            .get("result")
            .and_then(Value::as_array)
            .map(|rows| {
                rows.iter()
                    .filter_map(|w| {
                        Some((
                            w.get("id")?.as_i64()?,
                            w.get("state")
                                .and_then(Value::as_str)
                                .unwrap_or("normal")
                                .to_string(),
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default(),
        Err(_) => {
            notes.push("chrome.windows.getAll did not answer; `visible` is null".to_string());
            HashMap::new()
        }
    };
    let holdings = RelayHoldings::from_state(Some(&state));
    // Chrome tab id -> whether the extension's debugger is on it now.
    let attached_now: HashMap<i64, bool> = state
        .get("attachedTargets")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter_map(|r| {
                    Some((
                        r.get("tabId")?.as_i64()?,
                        r.get("attached").and_then(Value::as_bool).unwrap_or(true),
                    ))
                })
                .collect()
        })
        .unwrap_or_default();
    let others = connection::created_targets_by_other_sessions(&ctx.session);
    let live = live_sessions(&others);
    let own_targets = connection::created_target_ids(&ctx.session);
    let own_group = crate::session_title::display_name(&ctx.session);

    // The relay's own records: page records not confirmed live are stale
    // (#519); worker records are the workers the extension holds.
    let records: Option<Vec<TargetInfo>> = call(client, "Target.getTargets", json!({}), None)
        .await
        .ok()
        .and_then(|v| serde_json::from_value(v.get("targetInfos")?.clone()).ok());
    let (stale, worker_records) = match records {
        Some(records) => {
            let pages: Vec<TargetInfo> = records
                .iter()
                .filter(|t| t.target_type == "page" || t.target_type == "webview")
                .cloned()
                .collect();
            let page_count = pages.len();
            let live_pages = crate::native::relay_targets::confirm_live(client, pages).await;
            let workers: Vec<TargetInfo> = records
                .into_iter()
                .filter(|t| WORKER_TYPES.contains(&t.target_type.as_str()))
                .collect();
            (
                Some(json!({
                    "records": page_count,
                    "live": live_pages.len(),
                    "stale": page_count.saturating_sub(live_pages.len()),
                })),
                workers,
            )
        }
        None => {
            notes.push(
                "the relay did not list its target records; staleRelayRecords is null".to_string(),
            );
            (None, Vec::new())
        }
    };

    let can_release = if opts.force {
        call(client, "ABRelay.getCapabilities", json!({}), None)
            .await
            .ok()
            .and_then(|v| {
                v.get("capabilities")?
                    .as_array()
                    .map(|c| c.iter().any(|x| x.as_str() == Some(RELEASE_TAB_CAPABILITY)))
            })
            .unwrap_or(false)
    } else {
        false
    };

    let window_ids: HashSet<i64> = tabs
        .iter()
        .filter_map(|t| t.get("windowId").and_then(Value::as_i64))
        .collect();
    let mut pages: Vec<PageObs> = tabs
        .iter()
        .filter_map(|tab| {
            let id = tab.get("id").and_then(Value::as_i64)?;
            let group = tab
                .get("groupId")
                .and_then(Value::as_i64)
                .filter(|g| *g >= 0);
            let owner = relay_owner(
                id,
                group,
                &holdings,
                &others,
                &own_targets,
                &own_group,
                &live,
            );
            let window_id = tab.get("windowId").and_then(Value::as_i64);
            let active = tab.get("active").and_then(Value::as_bool);
            let discarded = tab.get("discarded").and_then(Value::as_bool);
            let attached = attached_now.get(&id).copied().unwrap_or(false);
            let visible = match (active, window_id.and_then(|w| windows.get(&w))) {
                (Some(active), Some(state)) => {
                    Some(active && state != "minimized" && discarded != Some(true))
                }
                _ => None,
            };
            let url = tab
                .get("url")
                .and_then(Value::as_str)
                .filter(|u| !u.is_empty())
                .or_else(|| tab.get("pendingUrl").and_then(Value::as_str))
                .unwrap_or("")
                .to_string();
            let plan = plan_relay(
                opts.measure,
                opts.force,
                &owner,
                attached,
                discarded == Some(true),
                can_release,
            );
            Some(PageObs {
                handle: crate::native::browser::chrome_tab_handle(id),
                target_id: holdings.attached.get(&id).cloned(),
                chrome_tab_id: Some(id),
                window_id,
                index: tab.get("index").and_then(Value::as_i64),
                title: tab
                    .get("title")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                url,
                active,
                visible,
                discarded,
                attached: Some(attached),
                owner,
                plan,
                outcome: Outcome::NotRequested,
            })
        })
        .collect();
    sort_pages(&mut pages);
    let total = pages.len();
    pages.truncate(opts.limit);

    // Measure the listed pages.
    let mut forced = Vec::new();
    if opts.measure {
        let results: Vec<(usize, Outcome, Option<Value>)> =
            stream::iter(pages.iter().enumerate().map(|(i, p)| (i, p.clone())))
                .map(|(i, p)| async move {
                    let (outcome, audit) = measure_relay_page(client, &p).await;
                    (i, outcome, audit)
                })
                .buffer_unordered(MEASURE_CONCURRENCY)
                .collect()
                .await;
        for (i, outcome, audit) in results {
            pages[i].outcome = outcome;
            if let Some(a) = audit {
                forced.push(a);
            }
        }
        forced.sort_by(|a, b| a["handle"].as_str().cmp(&b["handle"].as_str()));
    }

    // Workers: listed from the relay's records; their heap only with --force
    // (a worker cannot be told apart by owner, and asking it can re-attach
    // the tab it runs in).
    let mut workers: Vec<WorkerObs> = Vec::new();
    for w in worker_records {
        workers.push(WorkerObs {
            target_id: w.target_id.clone(),
            kind: w.target_type.clone(),
            url: w.url.clone(),
            session: None,
            heap: None,
        });
    }
    let mut workers_measured = false;
    let workers_note = if workers.is_empty() {
        Some(
            "over the relay only workers of tabs a chrome-use session drives are known (the \
             extension holds them); none are held now"
                .to_string(),
        )
    } else if opts.measure && opts.force {
        for w in workers.iter_mut() {
            let session = call(
                client,
                "Target.attachToTarget",
                json!({ "targetId": w.target_id, "flatten": true }),
                None,
            )
            .await
            .ok()
            .and_then(|v| v.get("sessionId")?.as_str().map(str::to_string));
            if let Some(sid) = session {
                if let Ok(v) = call(client, "Runtime.getHeapUsage", json!({}), Some(&sid)).await {
                    w.heap = heap_usage(&v);
                }
                w.session = Some(sid);
            }
        }
        workers_measured = true;
        Some(
            "over the relay only workers of tabs a chrome-use session drives are known (the \
             extension holds them)"
                .to_string(),
        )
    } else if opts.measure {
        Some("worker heaps are measured only with --force".to_string())
    } else {
        None
    };

    Ok(Sample {
        pages,
        total,
        windows: window_ids.len(),
        stale,
        workers,
        workers_note,
        workers_measured,
        state: Some(state),
        forced,
        notes,
    })
}

/// Measure one relay page by its plan. Returns the outcome and, for a forced
/// attach, the audit row (what was attached and how it was released).
async fn measure_relay_page(client: &CdpClient, p: &PageObs) -> (Outcome, Option<Value>) {
    let Some(chrome_tab) = p.chrome_tab_id else {
        return (
            Outcome::Skipped {
                code: "error",
                detail: "no Chrome tab id".to_string(),
            },
            None,
        );
    };
    let session = format!("cb-tab-{chrome_tab}");
    match &p.plan {
        Plan::NotRequested | Plan::CdpSession { .. } => (Outcome::NotRequested, None),
        Plan::Skip { code, detail } => (
            Outcome::Skipped {
                code,
                detail: detail.clone(),
            },
            None,
        ),
        Plan::Existing => match perf(client, &session).await {
            Ok(metrics) => (
                Outcome::Measured {
                    metrics,
                    via: "existing_attachment",
                    forced: p.owner != TabOwner::This,
                    at: Instant::now(),
                },
                None,
            ),
            Err(e) => (failed(e, "existing_attachment"), None),
        },
        Plan::ForcedAttach => {
            let attach = call(
                client,
                "ABExt.attachTabById",
                json!({ "chromeTabId": chrome_tab }),
                None,
            )
            .await;
            let target = match &attach {
                Ok(v) if v.get("attached").and_then(Value::as_bool) == Some(true) => v
                    .get("targetId")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                Ok(_) => {
                    return (
                        Outcome::Skipped {
                            code: "not_attachable",
                            detail: "not measured: Chrome does not let an extension attach this \
                                     page (a chrome:// or other protected page), or it has not \
                                     loaded yet"
                                .to_string(),
                        },
                        None,
                    )
                }
                Err(_) => None,
            };
            // An attach that timed out may still land: find it and release it.
            let target = match (target, &attach) {
                (Some(t), _) => Some(t),
                (None, Err(_)) => late_attached_target(client, chrome_tab).await,
                (None, Ok(_)) => None,
            };
            let outcome = match (&attach, &target) {
                (Ok(_), Some(_)) => match perf(client, &session).await {
                    Ok(metrics) => Outcome::Measured {
                        metrics,
                        via: "temporary_attach",
                        forced: true,
                        at: Instant::now(),
                    },
                    Err(e) => failed(e, "temporary_attach"),
                },
                (Err(e), _) => failed(format!("attach failed: {e}"), "temporary_attach"),
                (Ok(_), None) => failed(
                    "the extension attached the tab but named no target".to_string(),
                    "temporary_attach",
                ),
            };
            let release = match &target {
                Some(t) => release_tab(client, t).await,
                None => "nothing attached".to_string(),
            };
            let audit = json!({
                "handle": p.handle,
                "targetId": target,
                "attached": target.is_some(),
                "release": release,
                "activated": false,
            });
            (outcome, Some(audit))
        }
    }
}

async fn late_attached_target(client: &CdpClient, chrome_tab: i64) -> Option<String> {
    let state = call(client, "ABExt.state", json!({}), None).await.ok()?;
    RelayHoldings::from_state(Some(&state))
        .attached
        .get(&chrome_tab)
        .cloned()
}

async fn release_tab(client: &CdpClient, target: &str) -> String {
    match call(
        client,
        "ABExt.releaseTab",
        json!({ "targetId": target }),
        None,
    )
    .await
    {
        Ok(v) if v.get("released").and_then(Value::as_bool) == Some(true) => "released".to_string(),
        Ok(v) => format!(
            "not released: {}",
            v.get("reason").and_then(Value::as_str).unwrap_or("unknown")
        ),
        Err(e) => format!("not released: {e}"),
    }
}

// ---------------------------------------------------------------------------
// Direct CDP
// ---------------------------------------------------------------------------

async fn sample_cdp(ctx: &Ctx<'_>) -> Result<Sample, SampleError> {
    let client = ctx.client;
    let opts = ctx.opts;
    let targets: Vec<TargetInfo> = call(client, "Target.getTargets", json!({}), None)
        .await
        .map_err(|e| SampleError::NotAnswering(format!("Target.getTargets failed: {e}")))
        .and_then(|v| {
            serde_json::from_value(v.get("targetInfos").cloned().unwrap_or(Value::Null))
                .map_err(|e| SampleError::Fatal(format!("Target.getTargets: {e}")))
        })?;
    let others = connection::created_targets_by_other_sessions(&ctx.session);
    let live = live_sessions(&others);
    let own_targets = connection::created_target_ids(&ctx.session);
    let page_targets: Vec<TargetInfo> = targets
        .iter()
        .filter(|t| t.target_type == "page")
        .take(MAX_PAGE_LIMIT)
        .cloned()
        .collect();
    let windows: Vec<Option<i64>> =
        futures_util::future::join_all(page_targets.iter().map(|t| async move {
            call(
                client,
                "Browser.getWindowForTarget",
                json!({ "targetId": t.target_id }),
                None,
            )
            .await
            .ok()
            .and_then(|v| v.get("windowId").and_then(Value::as_i64))
        }))
        .await;
    let window_ids: HashSet<i64> = windows.iter().flatten().copied().collect();
    let mut pages: Vec<PageObs> = page_targets
        .iter()
        .zip(windows)
        .enumerate()
        .map(|(order, (t, window))| {
            let owner = if own_targets.contains(&t.target_id) {
                TabOwner::This
            } else {
                crate::native::browser::foreign_owner(
                    false,
                    others.get(&t.target_id).map(String::as_str),
                    |s| live.get(s).copied().unwrap_or(false),
                )
            };
            let attached = t.attached.unwrap_or(false);
            let plan = plan_cdp(opts.measure, opts.force, &owner, attached);
            PageObs {
                handle: t.target_id.clone(),
                target_id: Some(t.target_id.clone()),
                chrome_tab_id: None,
                window_id: window,
                index: Some(order as i64),
                title: t.title.clone(),
                url: t.url.clone(),
                active: None,
                visible: None,
                discarded: None,
                attached: t.attached,
                owner,
                plan,
                outcome: Outcome::NotRequested,
            }
        })
        .collect();
    sort_pages(&mut pages);
    let total = targets.iter().filter(|t| t.target_type == "page").count();
    pages.truncate(opts.limit);

    let mut workers: Vec<WorkerObs> = targets
        .iter()
        .filter(|t| WORKER_TYPES.contains(&t.target_type.as_str()))
        .map(|t| WorkerObs {
            target_id: t.target_id.clone(),
            kind: t.target_type.clone(),
            url: t.url.clone(),
            session: None,
            heap: None,
        })
        .collect();
    let mut forced = Vec::new();
    if opts.measure {
        let results: Vec<(usize, Outcome, Vec<WorkerObs>, Option<Value>)> =
            stream::iter(pages.iter().enumerate().map(|(i, p)| (i, p.clone())))
                .map(|(i, p)| async move {
                    let (outcome, found, audit) = measure_cdp_page(client, &p).await;
                    (i, outcome, found, audit)
                })
                .buffer_unordered(MEASURE_CONCURRENCY)
                .collect()
                .await;
        for (i, outcome, found, audit) in results {
            pages[i].outcome = outcome;
            for w in found {
                match workers.iter_mut().find(|x| x.target_id == w.target_id) {
                    Some(x) => {
                        if x.heap.is_none() {
                            x.heap = w.heap;
                        }
                    }
                    None => workers.push(w),
                }
            }
            if let Some(a) = audit {
                forced.push(a);
            }
        }
        forced.sort_by(|a, b| a["handle"].as_str().cmp(&b["handle"].as_str()));
    }
    // Shared and service workers not reached through a measured page: their
    // heap only with --force (private sessions, detached right after).
    if opts.measure && opts.force {
        for w in workers.iter_mut().filter(|w| w.heap.is_none()) {
            let Ok(att) = call(
                client,
                "Target.attachToTarget",
                json!({ "targetId": w.target_id, "flatten": true }),
                None,
            )
            .await
            else {
                continue;
            };
            let Some(sid) = att.get("sessionId").and_then(Value::as_str) else {
                continue;
            };
            if let Ok(v) = call(client, "Runtime.getHeapUsage", json!({}), Some(sid)).await {
                w.heap = heap_usage(&v);
            }
            let _ = call(
                client,
                "Target.detachFromTarget",
                json!({ "sessionId": sid }),
                None,
            )
            .await;
        }
    }
    let workers_note = if opts.measure && !opts.force {
        Some(
            "dedicated workers of measured pages are measured with them; other workers only \
             with --force"
                .to_string(),
        )
    } else {
        None
    };
    Ok(Sample {
        pages,
        total,
        windows: window_ids.len(),
        stale: None,
        workers,
        workers_note,
        workers_measured: opts.measure,
        state: None,
        forced,
        notes: vec![
            "direct CDP does not say which tab is active, visible or discarded; those are null"
                .to_string(),
        ],
    })
}

/// Measure one page on a private CDP session, with its dedicated workers,
/// and detach. Never activates.
async fn measure_cdp_page(
    client: &CdpClient,
    p: &PageObs,
) -> (Outcome, Vec<WorkerObs>, Option<Value>) {
    let forced = match &p.plan {
        Plan::CdpSession { forced } => *forced,
        Plan::Skip { code, detail } => {
            return (
                Outcome::Skipped {
                    code,
                    detail: detail.clone(),
                },
                Vec::new(),
                None,
            )
        }
        _ => return (Outcome::NotRequested, Vec::new(), None),
    };
    let Some(target) = p.target_id.clone() else {
        return (Outcome::NotRequested, Vec::new(), None);
    };
    let mut events = client.subscribe();
    let sid = match call(
        client,
        "Target.attachToTarget",
        json!({ "targetId": target, "flatten": true }),
        None,
    )
    .await
    {
        Ok(v) => match v.get("sessionId").and_then(Value::as_str) {
            Some(s) => s.to_string(),
            None => {
                return (
                    failed("attach named no session".to_string(), "cdp_session"),
                    Vec::new(),
                    None,
                )
            }
        },
        Err(e) => {
            return (
                failed(format!("attach failed: {e}"), "cdp_session"),
                Vec::new(),
                None,
            )
        }
    };
    // A target paused for its debugger (Chrome 144+) runs on.
    let _ = call(
        client,
        "Runtime.runIfWaitingForDebugger",
        json!({}),
        Some(&sid),
    )
    .await;
    let outcome = match perf(client, &sid).await {
        Ok(metrics) => Outcome::Measured {
            metrics,
            via: "cdp_session",
            forced,
            at: Instant::now(),
        },
        Err(e) => failed(e, "cdp_session"),
    };
    // Dedicated workers, through auto-attach on this private session only.
    let mut found = Vec::new();
    let mut children = Vec::new();
    if call(
        client,
        "Target.setAutoAttach",
        json!({ "autoAttach": true, "waitForDebuggerOnStart": false, "flatten": true }),
        Some(&sid),
    )
    .await
    .is_ok()
    {
        let deadline = tokio::time::Instant::now() + WORKER_DISCOVERY_WINDOW;
        loop {
            match tokio::time::timeout_at(deadline, events.recv()).await {
                Ok(Ok(ev)) => {
                    if ev.method != "Target.attachedToTarget"
                        || ev.session_id.as_deref() != Some(sid.as_str())
                    {
                        continue;
                    }
                    let Some(child) = ev.params.get("sessionId").and_then(Value::as_str) else {
                        continue;
                    };
                    children.push(child.to_string());
                    let info = &ev.params["targetInfo"];
                    let kind = info["type"].as_str().unwrap_or("");
                    if WORKER_TYPES.contains(&kind) {
                        found.push(WorkerObs {
                            target_id: info["targetId"].as_str().unwrap_or("").to_string(),
                            kind: kind.to_string(),
                            url: info["url"].as_str().unwrap_or("").to_string(),
                            session: Some(child.to_string()),
                            heap: None,
                        });
                    }
                }
                Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => continue,
                Ok(Err(_)) | Err(_) => break,
            }
        }
        for w in found.iter_mut() {
            if let Some(child) = w.session.as_deref() {
                if let Ok(v) = call(client, "Runtime.getHeapUsage", json!({}), Some(child)).await {
                    w.heap = heap_usage(&v);
                }
            }
        }
    }
    for child in &children {
        let _ = call(
            client,
            "Target.detachFromTarget",
            json!({ "sessionId": child }),
            None,
        )
        .await;
    }
    let detach = call(
        client,
        "Target.detachFromTarget",
        json!({ "sessionId": sid }),
        None,
    )
    .await;
    let audit = forced.then(|| {
        json!({
            "handle": p.handle,
            "targetId": target,
            "attached": true,
            "release": match &detach {
                Ok(_) => "detached".to_string(),
                Err(e) => format!("not detached: {e}"),
            },
            "activated": false,
        })
    });
    (outcome, found, audit)
}

// ---------------------------------------------------------------------------
// The report
// ---------------------------------------------------------------------------

/// The extension section: its version, whether it is behind the published
/// one, and what to do about it (doctor's logic).
fn extension_json(relay: bool, state: Option<&Value>) -> Value {
    let bundled = env!("AB_CONNECT_VERSION");
    if !relay {
        return json!({
            "applies": false,
            "version": null,
            "bundled": bundled,
            "published": null,
            "verdict": "not_applicable",
            "behindPublished": null,
            "hint": null,
            "pendingUpdate": null,
        });
    }
    let live = state
        .and_then(|s| s.get("version"))
        .and_then(Value::as_str)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
        .or_else(connect::relay_ext_version_driving);
    let pending = state
        .and_then(connect::extension_update_from_state)
        .and_then(|u| connect::update_notice(Some(&u)))
        .map(|n| {
            json!({
                "blocked": n.blocked,
                "message": n.message,
                "fix": n.fix,
            })
        });
    let Some(live) = live else {
        return json!({
            "applies": true,
            "version": null,
            "bundled": bundled,
            "published": null,
            "verdict": "unknown",
            "behindPublished": null,
            "hint": "the extension did not report its version (it predates version reporting); \
                     update it from chrome://extensions",
            "pendingUpdate": pending,
        });
    };
    // Ask the Web Store only when the live build is behind the bundled one,
    // as doctor does; the answer is cached for twelve hours.
    let store = if crate::upgrade::version_is_newer(bundled, &live) {
        connect::cached_store_extension_version()
    } else {
        None
    };
    let verdict = connect::classify_ext_version(&live, bundled, store.as_deref());
    let (name, behind, hint) = match &verdict {
        connect::ExtVersionVerdict::Current => ("current", Some(false), None),
        connect::ExtVersionVerdict::AheadOfBundled => (
            "ahead_of_bundled",
            Some(false),
            Some(connect::newer_extension_hint(&live, bundled)),
        ),
        connect::ExtVersionVerdict::BehindStore { store } => (
            "behind_published",
            Some(true),
            Some(format!(
                "extension {live} is behind the published {store}; update ab-connect: {}",
                connect::update_instruction()
            )),
        ),
        connect::ExtVersionVerdict::NewestPublished { .. } => {
            ("newest_published", Some(false), None)
        }
        connect::ExtVersionVerdict::AheadOfStore { .. } => {
            ("ahead_of_published", Some(false), None)
        }
        connect::ExtVersionVerdict::BehindBundledStoreUnknown => (
            "behind_bundled_published_unknown",
            None,
            Some(
                "if a newer build is published: chrome://extensions \u{2192} Developer mode \
                 \u{2192} Update"
                    .to_string(),
            ),
        ),
    };
    let hint = hint.or_else(|| {
        pending
            .as_ref()
            .and_then(|p| p.get("fix").and_then(Value::as_str))
            .map(str::to_string)
    });
    json!({
        "applies": true,
        "version": live,
        "bundled": bundled,
        "published": store,
        "verdict": name,
        "behindPublished": behind,
        "hint": hint,
        "pendingUpdate": pending,
    })
}

fn profile_json(relay: bool, ws: Option<&str>) -> Value {
    if !relay {
        return json!({ "id": null, "email": null });
    }
    let by_endpoint = ws.and_then(|ws| {
        connect::list_relay_profiles()
            .into_iter()
            .find(|(_, _, u)| u == ws)
            .map(|(id, email, _)| (id, email))
    });
    match by_endpoint.or_else(connect::driving_profile) {
        Some((id, email)) => json!({ "id": id, "email": email }),
        None => json!({ "id": null, "email": null }),
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn options_json(opts: &DiagOptions) -> Value {
    json!({
        "measure": opts.measure,
        "force": opts.force,
        "watchSeconds": opts.watch,
        "limit": opts.limit,
    })
}

fn thresholds_json() -> Value {
    json!({
        "heapGrowingBytesPerMinute": int(HEAP_GROWING_BYTES_PER_MIN),
        "listenersGrowthMin": int(LISTENERS_GROWTH_MIN),
        "listenersGrowthRel": LISTENERS_GROWTH_REL,
        "nodesFlatAbs": int(NODES_FLAT_ABS),
        "nodesFlatRel": NODES_FLAT_REL,
    })
}

/// A report that could not read the browser: schema-shaped, with no pages.
pub fn not_connected_doc(
    opts: &DiagOptions,
    session: &str,
    transport: Option<&str>,
    detail: &str,
    exit_code: i32,
) -> Value {
    let relay = transport != Some("cdp");
    json!({
        "schema": SCHEMA_VERSION,
        "command": "diag pages",
        "success": false,
        "exitCode": exit_code,
        "exitReason": if exit_code == EXIT_NOT_CONNECTED { json!("not_connected") } else { json!("error") },
        "error": detail,
        "generatedAt": now_ms(),
        "session": session,
        "options": options_json(opts),
        "readOnly": true,
        "connection": {
            "transport": transport,
            "state": if exit_code == EXIT_NOT_CONNECTED { "not_connected" } else { "error" },
            "detail": detail,
            "extensionConnected": false,
        },
        "extension": extension_json(relay, None),
        "profile": profile_json(relay, None),
        "staleRelayRecords": null,
        "relayRecords": null,
        "counts": {
            "pages": 0, "shown": 0, "omitted": 0, "windows": 0,
            "self": 0, "otherSessions": 0, "user": 0,
            "measured": 0, "skipped": 0, "failed": 0,
        },
        "pages": [],
        "omitted": { "pages": 0, "titlesCut": 0, "urlsCut": 0, "workerSites": 0, "note": null },
        "workers": null,
        "forcedAttaches": [],
        "watch": null,
        "notes": [],
    })
}

/// The schema-1 report from one sample, or two for `--watch`.
fn build_doc(
    opts: &DiagOptions,
    session: &str,
    endpoint: &Endpoint,
    first: &Sample,
    second: Option<&Sample>,
    watch_started: Option<u64>,
) -> Value {
    let last = second.unwrap_or(first);
    let transport = if endpoint.relay { "relay" } else { "cdp" };
    let before: HashMap<&str, (Metrics, Instant)> = match second {
        Some(_) => first
            .pages
            .iter()
            .filter_map(|p| metrics_of(p).map(|m| (p.handle.as_str(), m)))
            .collect(),
        None => HashMap::new(),
    };
    let mut leaks: BTreeMap<&'static str, usize> = BTreeMap::new();
    let rows: Vec<Value> = last
        .pages
        .iter()
        .map(|p| {
            let mut row = page_json(p);
            if second.is_some() {
                if let (Some((m0, t0)), Some((m1, t1))) =
                    (before.get(p.handle.as_str()), metrics_of(p))
                {
                    let secs = t1.saturating_duration_since(*t0).as_secs_f64();
                    let verdict = classify_leak(m0, &m1, secs);
                    *leaks.entry(verdict.class).or_default() += 1;
                    row["metricsBefore"] = m0.to_json();
                    row["delta"] = delta_json(m0, &m1, secs, &verdict);
                    row["leakClass"] = json!(verdict.class);
                    row["leakSignals"] = json!(verdict.signals);
                }
            }
            row
        })
        .collect();
    let count = |f: &dyn Fn(&PageObs) -> bool| last.pages.iter().filter(|p| f(p)).count();
    let measured = count(&|p: &PageObs| matches!(p.outcome, Outcome::Measured { .. }));
    let skipped = count(&|p: &PageObs| matches!(p.outcome, Outcome::Skipped { .. }));
    let failed_n = count(&|p: &PageObs| matches!(p.outcome, Outcome::Failed { .. }));
    let titles_cut = last
        .pages
        .iter()
        .filter(|p| p.title.chars().nth(TITLE_MAX_CHARS).is_some())
        .count();
    let urls_cut = last
        .pages
        .iter()
        .filter(|p| p.url.chars().nth(URL_MAX_CHARS).is_some())
        .count();
    let omitted_pages = last.total.saturating_sub(last.pages.len());
    let workers = workers_json(last, transport);
    let worker_sites_omitted = workers["omittedSites"].as_u64().unwrap_or(0);
    let mut omitted_notes = Vec::new();
    if omitted_pages > 0 {
        omitted_notes.push(format!(
            "{omitted_pages} page(s) not listed or measured: over the {}-page limit (pass --limit \
             <n>, up to {MAX_PAGE_LIMIT})",
            opts.limit
        ));
    }
    if titles_cut + urls_cut > 0 {
        omitted_notes.push(format!(
            "{titles_cut} title(s) over {TITLE_MAX_CHARS} and {urls_cut} url(s) over \
             {URL_MAX_CHARS} characters were cut (titleCut / urlCut)"
        ));
    }
    if worker_sites_omitted > 0 {
        omitted_notes.push(format!(
            "{worker_sites_omitted} worker site(s) over the {MAX_WORKER_SITES}-site cap not listed"
        ));
    }
    // Exit 3 is judged on every page of the profile, listed or not; owners
    // of unlisted pages are not kept, so with pages over the limit the
    // listed ones decide (they come first: this session, then other
    // sessions).
    let owners: Vec<TabOwner> = first.pages.iter().map(|p| p.owner.clone()).collect();
    let first_measured = first
        .pages
        .iter()
        .filter(|p| matches!(p.outcome, Outcome::Measured { .. }))
        .count();
    let exit_code = exit_code_for(opts.measure, opts.force, &owners, first_measured);
    let state = last.state.as_ref();
    let mut forced: Vec<Value> = first.forced.clone();
    if let Some(s) = second {
        forced.extend(s.forced.iter().cloned());
    }
    let mut notes = last.notes.clone();
    if exit_code == EXIT_HELD_BY_OTHER_SESSION {
        notes.push(
            "every page is held by another running chrome-use session, so nothing was measured; \
             pass --force to measure them as they are"
                .to_string(),
        );
    }
    json!({
        "schema": SCHEMA_VERSION,
        "command": "diag pages",
        "success": exit_code == EXIT_OK,
        "exitCode": exit_code,
        "exitReason": if exit_code == EXIT_HELD_BY_OTHER_SESSION { json!("held_by_other_session") } else { Value::Null },
        "error": if exit_code == EXIT_HELD_BY_OTHER_SESSION {
            json!("another chrome-use session holds every page; nothing was measured (pass --force)")
        } else {
            Value::Null
        },
        "generatedAt": now_ms(),
        "session": session,
        "options": options_json(opts),
        "readOnly": forced.is_empty(),
        "connection": {
            "transport": transport,
            "state": "connected",
            "detail": format!("connected through {}", endpoint.via),
            "extensionConnected": if endpoint.relay {
                state.and_then(|s| s.get("connected")).and_then(Value::as_bool).or(Some(true))
            } else {
                None
            },
        },
        "extension": extension_json(endpoint.relay, state),
        "profile": profile_json(endpoint.relay, Some(&endpoint.ws)),
        "staleRelayRecords": first.stale.as_ref().and_then(|s| s.get("stale")).cloned(),
        "relayRecords": first.stale.clone(),
        "counts": {
            "pages": last.total,
            "shown": last.pages.len(),
            "omitted": omitted_pages,
            "windows": last.windows,
            "self": count(&|p: &PageObs| p.owner == TabOwner::This),
            "otherSessions": count(&|p: &PageObs| matches!(p.owner, TabOwner::Session { .. })),
            "user": count(&|p: &PageObs| p.owner == TabOwner::User),
            "measured": measured,
            "skipped": skipped,
            "failed": failed_n,
        },
        "pages": rows,
        "omitted": {
            "pages": omitted_pages,
            "titlesCut": titles_cut,
            "urlsCut": urls_cut,
            "workerSites": worker_sites_omitted,
            "note": if omitted_notes.is_empty() { Value::Null } else { json!(omitted_notes.join("; ")) },
        },
        "workers": workers,
        "forcedAttaches": forced,
        "watch": second.map(|_| json!({
            "intervalSeconds": opts.watch,
            "startedAt": watch_started,
            "leakClasses": leaks,
            "thresholds": thresholds_json(),
        })),
        "notes": notes,
    })
}

// ---------------------------------------------------------------------------
// Text output
// ---------------------------------------------------------------------------

fn mb(bytes: f64) -> String {
    format!("{:.1} MB", bytes / (1024.0 * 1024.0))
}

fn short(text: &str, max: usize) -> String {
    let (s, cut) = clip(text, max);
    if cut {
        format!("{s}…")
    } else {
        s
    }
}

fn print_text(doc: &Value) {
    let s = |v: &Value| v.as_str().unwrap_or("").to_string();
    let conn = &doc["connection"];
    let ext = &doc["extension"];
    let mut head = format!(
        "diag pages (schema {}) — {}",
        doc["schema"],
        conn["transport"].as_str().unwrap_or("not connected")
    );
    if let Some(v) = ext["version"].as_str() {
        head.push_str(&format!(
            ", extension {v} ({})",
            ext["verdict"].as_str().unwrap_or("?")
        ));
    }
    if let Some(p) = doc["profile"]["email"]
        .as_str()
        .or_else(|| doc["profile"]["id"].as_str())
    {
        head.push_str(&format!(", profile {p}"));
    }
    println!("{head}");
    if doc["exitCode"] == EXIT_NOT_CONNECTED {
        eprintln!("{} {}", color::error_indicator(), s(&doc["error"]));
        return;
    }
    let c = &doc["counts"];
    println!(
        "{} page(s) in {} window(s): {} this session's, {} other sessions', {} the user's; \
         stale relay records: {}",
        c["pages"],
        c["windows"],
        c["self"],
        c["otherSessions"],
        c["user"],
        match doc["staleRelayRecords"].as_u64() {
            Some(n) => n.to_string(),
            None => "n/a".to_string(),
        }
    );
    if doc["options"]["measure"] == true {
        println!(
            "measured {}, skipped {}, failed {}{}",
            c["measured"],
            c["skipped"],
            c["failed"],
            if doc["readOnly"] == true {
                " (nothing was attached)".to_string()
            } else {
                format!(
                    " ({} tab(s) attached for measuring and released; none activated)",
                    doc["forcedAttaches"].as_array().map(Vec::len).unwrap_or(0)
                )
            }
        );
    } else {
        println!(
            "{}",
            color::dim(
                "read-only listing: nothing was attached; --measure adds heap, nodes and listeners"
            )
        );
    }
    if let Some(hint) = ext["hint"].as_str() {
        println!("{} {hint}", color::warning_indicator());
    }
    for p in doc["pages"].as_array().into_iter().flatten() {
        let marks: Vec<&str> = [
            (p["active"] == true, "active"),
            (p["discarded"] == true, "discarded"),
            (p["attached"] == true, "attached"),
        ]
        .iter()
        .filter(|(on, _)| *on)
        .map(|(_, m)| *m)
        .collect();
        let marks = if marks.is_empty() {
            String::new()
        } else {
            format!(" [{}]", marks.join(", "))
        };
        println!(
            "{} {} — {}{} — {} — {}",
            if p["active"] == true {
                color::cyan("*")
            } else {
                " ".to_string()
            },
            s(&p["handle"]),
            s(&p["ownerLabel"]),
            marks,
            short(p["title"].as_str().unwrap_or(""), 80),
            short(p["url"].as_str().unwrap_or(""), 120)
        );
        let m = &p["metrics"];
        if m.is_object() {
            let f = |k: &str| m[k].as_f64().unwrap_or(0.0);
            let mut line = format!(
                "      heap {} nodes {} listeners {} documents {}",
                mb(f("JSHeapUsedSize")),
                m["Nodes"],
                m["JSEventListeners"],
                m["Documents"]
            );
            if p["delta"].is_object() {
                let d = &p["delta"];
                line.push_str(&format!(
                    "; over {}s: heap {:+.1} MB/min, nodes {:+}, listeners {:+} → {}",
                    d["seconds"],
                    d["heapBytesPerMinute"].as_f64().unwrap_or(0.0) / (1024.0 * 1024.0),
                    d["Nodes"].as_i64().unwrap_or(0),
                    d["JSEventListeners"].as_i64().unwrap_or(0),
                    s(&p["leakClass"])
                ));
            }
            println!("{line}");
        } else if let Some(detail) = p["measure"]["detail"].as_str() {
            println!("      {}", color::dim(detail));
        }
    }
    if let Some(w) = doc["workers"].as_object() {
        let sites = w
            .get("sites")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if !sites.is_empty() {
            println!("workers per site:");
            for site in sites {
                let heap = site["usedSize"]
                    .as_f64()
                    .map(|b| format!(", heap {}", mb(b)))
                    .unwrap_or_default();
                println!("  {} — {} worker(s){heap}", s(&site["site"]), site["count"]);
            }
        }
    }
    for a in doc["forcedAttaches"].as_array().into_iter().flatten() {
        println!(
            "{}",
            color::dim(&format!(
                "attached for measuring: {} — {}",
                s(&a["handle"]),
                s(&a["release"])
            ))
        );
    }
    if let Some(note) = doc["omitted"]["note"].as_str() {
        println!("{}", color::dim(&format!("Left out: {note}")));
    }
    for n in doc["notes"].as_array().into_iter().flatten() {
        println!("{}", color::dim(n.as_str().unwrap_or("")));
    }
    if doc["exitCode"] == EXIT_HELD_BY_OTHER_SESSION {
        eprintln!("{} {}", color::warning_indicator(), s(&doc["error"]));
    }
}

fn emit(doc: &Value, json_out: bool) {
    if json_out {
        println!(
            "{}",
            serde_json::to_string(doc).unwrap_or_else(|_| "{}".to_string())
        );
    } else {
        print_text(doc);
    }
}

// ---------------------------------------------------------------------------
// Entry
// ---------------------------------------------------------------------------

/// `chrome-use diag <args>`; returns the exit code.
pub fn run(args: &[String], flags: &Flags) -> i32 {
    let opts = match parse_args(args) {
        Ok(o) => o,
        Err(e) => {
            if flags.json {
                println!(
                    "{}",
                    json!({ "schema": SCHEMA_VERSION, "command": "diag pages", "success": false,
                            "exitCode": EXIT_USAGE, "exitReason": "usage", "error": e, "usage": USAGE })
                );
            } else {
                eprintln!("{} {e}\nUsage: {USAGE}", color::error_indicator());
            }
            return EXIT_USAGE;
        }
    };
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
    {
        Ok(r) => r,
        Err(e) => {
            eprintln!("{} {e}", color::error_indicator());
            return EXIT_USAGE;
        }
    };
    let doc = runtime.block_on(run_async(&opts, flags));
    emit(&doc, flags.json);
    doc["exitCode"].as_i64().unwrap_or(1) as i32
}

async fn run_async(opts: &DiagOptions, flags: &Flags) -> Value {
    let session = flags.session.clone();
    let endpoint = match resolve_endpoint(flags).await {
        Ok(e) => e,
        Err(Unresolved::NotConnected(d)) => {
            return not_connected_doc(opts, &session, None, &d, EXIT_NOT_CONNECTED)
        }
        Err(Unresolved::Invalid(d)) => {
            return not_connected_doc(opts, &session, None, &d, EXIT_USAGE)
        }
    };
    let transport = if endpoint.relay { "relay" } else { "cdp" };
    let headers = endpoint.relay.then(|| {
        // Tells the native host not to have the extension re-attach and
        // re-announce every agent tab for this connection.
        vec![("x-chrome-use-diagnostic".to_string(), "1".to_string())]
    });
    let client = match tokio::time::timeout(
        CONNECT_TIMEOUT,
        CdpClient::connect_with_headers(&endpoint.ws, headers),
    )
    .await
    {
        Ok(Ok(c)) => c,
        Ok(Err(e)) => {
            return not_connected_doc(
                opts,
                &session,
                Some(transport),
                &format!("could not connect to the {transport} endpoint: {e}"),
                EXIT_NOT_CONNECTED,
            )
        }
        Err(_) => {
            return not_connected_doc(
                opts,
                &session,
                Some(transport),
                &format!("the {transport} endpoint did not accept a connection in time"),
                EXIT_NOT_CONNECTED,
            )
        }
    };
    let ctx = Ctx {
        client: &client,
        session: session.clone(),
        opts,
    };
    let take = |r: Result<Sample, SampleError>| match r {
        Ok(s) => Ok(s),
        Err(SampleError::NotAnswering(d)) => Err(not_connected_doc(
            opts,
            &session,
            Some(transport),
            &d,
            EXIT_NOT_CONNECTED,
        )),
        Err(SampleError::Fatal(d)) => Err(not_connected_doc(
            opts,
            &session,
            Some(transport),
            &d,
            EXIT_USAGE,
        )),
    };
    let first = match take(sample_any(&ctx, endpoint.relay).await) {
        Ok(s) => s,
        Err(doc) => return doc,
    };
    let Some(secs) = opts.watch else {
        return build_doc(opts, &session, &endpoint, &first, None, None);
    };
    // Exit 3 is decided on the first sample: there is nothing to watch.
    let owners: Vec<TabOwner> = first.pages.iter().map(|p| p.owner.clone()).collect();
    let measured = first
        .pages
        .iter()
        .filter(|p| matches!(p.outcome, Outcome::Measured { .. }))
        .count();
    if exit_code_for(opts.measure, opts.force, &owners, measured) != EXIT_OK {
        return build_doc(opts, &session, &endpoint, &first, None, None);
    }
    let started = now_ms();
    tokio::time::sleep(Duration::from_secs(secs)).await;
    match take(sample_any(&ctx, endpoint.relay).await) {
        Ok(second) => build_doc(
            opts,
            &session,
            &endpoint,
            &first,
            Some(&second),
            Some(started),
        ),
        Err(doc) => doc,
    }
}

async fn sample_any(ctx: &Ctx<'_>, relay: bool) -> Result<Sample, SampleError> {
    if relay {
        sample_relay(ctx).await
    } else {
        sample_cdp(ctx).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &str) -> Vec<String> {
        s.split_whitespace().map(String::from).collect()
    }

    fn m(heap: f64, nodes: f64, listeners: f64) -> Metrics {
        Metrics {
            js_heap_used: heap,
            js_heap_total: heap * 2.0,
            nodes,
            listeners,
            documents: 1.0,
        }
    }

    const MB: f64 = 1024.0 * 1024.0;

    fn session(name: &str, live: Option<bool>) -> TabOwner {
        TabOwner::Session {
            name: Some(name.to_string()),
            live,
        }
    }

    #[test]
    fn options_parse_and_refuse_what_means_nothing() {
        let o = parse_args(&args("pages")).unwrap();
        assert_eq!(
            o,
            DiagOptions {
                measure: false,
                force: false,
                watch: None,
                limit: DEFAULT_PAGE_LIMIT
            }
        );
        let o = parse_args(&args("pages --watch 30")).unwrap();
        assert_eq!(o.watch, Some(30));
        assert!(o.measure, "--watch implies --measure");
        let o = parse_args(&args("pages --watch=5 --force --limit 10")).unwrap();
        assert_eq!((o.watch, o.force, o.limit), (Some(5), true, 10));
        assert!(parse_args(&args("pages --measure --force")).unwrap().force);
        for bad in [
            "",
            "tabs",
            "pages --force",
            "pages --watch",
            "pages --watch 0",
            "pages --watch x",
            "pages --watch 3601",
            "pages --limit 0",
            "pages --limit 1001",
            "pages --bogus",
        ] {
            assert!(parse_args(&args(bad)).is_err(), "{bad}");
        }
    }

    #[test]
    fn metrics_come_from_performance_get_metrics() {
        let reply = json!({"metrics": [
            {"name": "Timestamp", "value": 1.0},
            {"name": "Documents", "value": 3.0},
            {"name": "Nodes", "value": 1200.0},
            {"name": "JSEventListeners", "value": 45.0},
            {"name": "JSHeapUsedSize", "value": 12345678.0},
            {"name": "JSHeapTotalSize", "value": 22345678.0},
        ]});
        let got = Metrics::from_cdp(&reply).unwrap();
        assert_eq!(got.nodes, 1200.0);
        assert_eq!(got.listeners, 45.0);
        assert_eq!(
            got.to_json(),
            json!({"JSHeapUsedSize": 12345678, "JSHeapTotalSize": 22345678, "Nodes": 1200,
                   "JSEventListeners": 45, "Documents": 3})
        );
        assert!(
            Metrics::from_cdp(&json!({"metrics": [{"name": "Nodes", "value": 1.0}]})).is_none()
        );
        assert!(Metrics::from_cdp(&json!({})).is_none());
    }

    #[test]
    fn leak_classes_follow_the_agreed_rules() {
        // Listeners up, nodes flat: the classic re-added handler.
        let v = classify_leak(
            &m(10.0 * MB, 2000.0, 300.0),
            &m(10.2 * MB, 2010.0, 900.0),
            60.0,
        );
        assert_eq!(v.class, LEAK_LISTENERS_NODES_FLAT);
        assert!(v.signals.contains(&"listeners_growing"));
        assert!(v.signals.contains(&"nodes_flat"));
        // Listeners grow with heap too: listeners-with-flat-nodes still wins.
        let v = classify_leak(
            &m(10.0 * MB, 2000.0, 300.0),
            &m(30.0 * MB, 2000.0, 900.0),
            60.0,
        );
        assert_eq!(v.class, LEAK_LISTENERS_NODES_FLAT);
        assert!(v.signals.contains(&"heap_growing"));
        // Heap at exactly 1.5 MiB/min is growing; just under is not.
        let v = classify_leak(
            &m(10.0 * MB, 2000.0, 300.0),
            &m(11.5 * MB, 2000.0, 300.0),
            60.0,
        );
        assert_eq!(v.class, LEAK_HEAP_GROWING);
        assert_eq!(int(v.heap_bytes_per_minute), int(1.5 * MB));
        let v = classify_leak(
            &m(10.0 * MB, 2000.0, 300.0),
            &m(11.4 * MB, 2000.0, 300.0),
            60.0,
        );
        assert_eq!(v.class, LEAK_NONE);
        // The rate is per minute: 0.8 MiB in 30 s is 1.6 MiB/min.
        let v = classify_leak(
            &m(10.0 * MB, 2000.0, 300.0),
            &m(10.8 * MB, 2000.0, 300.0),
            30.0,
        );
        assert_eq!(v.class, LEAK_HEAP_GROWING);
        // Nodes climbing (listeners with them): not "nodes flat".
        let v = classify_leak(
            &m(10.0 * MB, 2000.0, 300.0),
            &m(10.1 * MB, 5000.0, 900.0),
            60.0,
        );
        assert_eq!(v.class, LEAK_NODES_CLIMBING);
        assert!(!v.signals.contains(&"nodes_flat"));
        // Small movements are noise.
        let v = classify_leak(
            &m(10.0 * MB, 2000.0, 300.0),
            &m(10.1 * MB, 2040.0, 310.0),
            60.0,
        );
        assert_eq!(v.class, LEAK_NONE);
        // The relative tolerances scale with big pages: 2 % of 100k nodes.
        let v = classify_leak(
            &m(10.0 * MB, 100_000.0, 10_000.0),
            &m(10.0 * MB, 101_500.0, 10_150.0),
            60.0,
        );
        assert_eq!(v.class, LEAK_NONE);
        // Shrinking heap is not a leak; a zero interval does not divide by zero.
        let v = classify_leak(
            &m(30.0 * MB, 2000.0, 300.0),
            &m(10.0 * MB, 2000.0, 300.0),
            0.0,
        );
        assert_eq!(v.class, LEAK_NONE);
    }

    #[test]
    fn delta_reports_every_metric_and_the_heap_rate() {
        let a = m(10.0 * MB, 2000.0, 300.0);
        let b = m(13.0 * MB, 2100.0, 360.0);
        let v = classify_leak(&a, &b, 120.0);
        let d = delta_json(&a, &b, 120.0, &v);
        assert_eq!(d["Nodes"], 100);
        assert_eq!(d["JSEventListeners"], 60);
        assert_eq!(d["Documents"], 0);
        assert_eq!(d["JSHeapUsedSize"], int(3.0 * MB));
        assert_eq!(d["heapBytesPerMinute"], int(1.5 * MB));
        assert_eq!(d["seconds"], 120.0);
    }

    #[test]
    fn exit_three_only_when_other_live_sessions_hold_everything() {
        let live = session("alpha", Some(true));
        let dead = session("beta", Some(false));
        let unnamed = TabOwner::Session {
            name: None,
            live: None,
        };
        assert_eq!(
            exit_code_for(true, false, &[live.clone(), live.clone()], 0),
            EXIT_HELD_BY_OTHER_SESSION
        );
        // --force, no --measure, something measured, or a page that is not
        // another live session's: 0.
        assert_eq!(exit_code_for(true, true, &[live.clone()], 0), EXIT_OK);
        assert_eq!(exit_code_for(false, false, &[live.clone()], 0), EXIT_OK);
        assert_eq!(exit_code_for(true, false, &[live.clone()], 1), EXIT_OK);
        for other in [dead, unnamed, TabOwner::User, TabOwner::This] {
            assert_eq!(
                exit_code_for(true, false, &[live.clone(), other.clone()], 0),
                EXIT_OK,
                "{other:?}"
            );
        }
        assert_eq!(exit_code_for(true, false, &[], 0), EXIT_OK);
    }

    #[test]
    fn relay_plans_never_attach_without_force() {
        let user = TabOwner::User;
        let live = session("alpha", Some(true));
        assert_eq!(
            plan_relay(false, false, &user, true, false, true),
            Plan::NotRequested
        );
        // Already attached: measured as it is, unless another live session holds it.
        assert_eq!(
            plan_relay(true, false, &TabOwner::This, true, false, false),
            Plan::Existing
        );
        assert_eq!(
            plan_relay(true, false, &user, true, false, false),
            Plan::Existing
        );
        assert!(matches!(
            plan_relay(true, false, &live, true, false, true),
            Plan::Skip {
                code: "held_by_other_session",
                ..
            }
        ));
        assert_eq!(
            plan_relay(true, true, &live, true, false, true),
            Plan::Existing
        );
        // Not attached: needs --force, and an extension that can release it.
        assert!(matches!(
            plan_relay(true, false, &user, false, false, true),
            Plan::Skip {
                code: "needs_force",
                ..
            }
        ));
        assert!(matches!(
            plan_relay(true, false, &TabOwner::This, false, false, true),
            Plan::Skip {
                code: "needs_force",
                ..
            }
        ));
        assert!(matches!(
            plan_relay(true, true, &user, false, false, false),
            Plan::Skip {
                code: "release_unsupported",
                ..
            }
        ));
        assert_eq!(
            plan_relay(true, true, &user, false, false, true),
            Plan::ForcedAttach
        );
        // A discarded tab would reload on attach: never.
        assert!(matches!(
            plan_relay(true, true, &user, false, true, true),
            Plan::Skip {
                code: "discarded",
                ..
            }
        ));
    }

    #[test]
    fn cdp_plans_follow_the_same_ownership_rule() {
        let user = TabOwner::User;
        let live = session("alpha", Some(true));
        assert_eq!(plan_cdp(false, true, &user, true), Plan::NotRequested);
        assert_eq!(
            plan_cdp(true, false, &TabOwner::This, false),
            Plan::CdpSession { forced: false }
        );
        assert_eq!(
            plan_cdp(true, false, &user, true),
            Plan::CdpSession { forced: false }
        );
        assert!(matches!(
            plan_cdp(true, false, &user, false),
            Plan::Skip {
                code: "needs_force",
                ..
            }
        ));
        assert!(matches!(
            plan_cdp(true, false, &live, true),
            Plan::Skip {
                code: "held_by_other_session",
                ..
            }
        ));
        assert_eq!(
            plan_cdp(true, true, &user, false),
            Plan::CdpSession { forced: true }
        );
        assert_eq!(
            plan_cdp(true, true, &live, true),
            Plan::CdpSession { forced: false }
        );
    }

    #[test]
    fn worker_sites_are_origins() {
        assert_eq!(
            worker_site("https://a.example:8443/w.js"),
            "https://a.example:8443"
        );
        assert_eq!(
            worker_site("blob:https://a.example/123-456"),
            "https://a.example"
        );
        assert_eq!(worker_site("data:text/javascript,1"), "data:");
        assert_eq!(
            worker_site("chrome-extension://abcdef/background.js"),
            "chrome-extension://abcdef"
        );
        assert_eq!(worker_site(""), "(no url)");
        assert_eq!(worker_site("not a url"), "(opaque)");
    }

    fn obs(handle: &str, owner: TabOwner, outcome: Outcome) -> PageObs {
        PageObs {
            handle: handle.to_string(),
            target_id: Some(format!("T-{handle}")),
            chrome_tab_id: Some(1),
            window_id: Some(1),
            index: Some(0),
            title: "t".repeat(400),
            url: "https://x.example/".to_string(),
            active: Some(true),
            visible: Some(true),
            discarded: Some(false),
            attached: Some(false),
            owner,
            plan: Plan::NotRequested,
            outcome,
        }
    }

    fn sample(pages: Vec<PageObs>) -> Sample {
        Sample {
            total: pages.len() + 2,
            pages,
            windows: 1,
            stale: Some(json!({"records": 5, "live": 3, "stale": 2})),
            workers: vec![WorkerObs {
                target_id: "W".into(),
                kind: "worker".into(),
                url: "https://x.example/w.js".into(),
                session: None,
                heap: Some((1000.0, 2000.0)),
            }],
            workers_note: None,
            workers_measured: true,
            state: Some(json!({"version": env!("AB_CONNECT_VERSION"), "connected": true})),
            forced: vec![],
            notes: vec![],
        }
    }

    /// The schema-1 keys a reader may rely on. Adding keys is fine; removing
    /// or renaming one breaks readers and needs schema 2.
    #[test]
    fn schema_one_has_every_documented_key() {
        let opts = parse_args(&args("pages --watch 1")).unwrap();
        let endpoint = Endpoint {
            relay: true,
            ws: "ws://127.0.0.1:1/x".into(),
            via: "default",
        };
        let t0 = Instant::now();
        let t1 = t0 + Duration::from_secs(60);
        let first = sample(vec![obs(
            "chrome-tab:1",
            TabOwner::User,
            Outcome::Measured {
                metrics: m(10.0 * MB, 2000.0, 300.0),
                via: "temporary_attach",
                forced: true,
                at: t0,
            },
        )]);
        let second = sample(vec![obs(
            "chrome-tab:1",
            TabOwner::User,
            Outcome::Measured {
                metrics: m(10.0 * MB, 2000.0, 1300.0),
                via: "temporary_attach",
                forced: true,
                at: t1,
            },
        )]);
        let doc = build_doc(&opts, "s1", &endpoint, &first, Some(&second), Some(1));
        for key in [
            "schema",
            "command",
            "success",
            "exitCode",
            "exitReason",
            "error",
            "generatedAt",
            "session",
            "options",
            "readOnly",
            "connection",
            "extension",
            "profile",
            "staleRelayRecords",
            "relayRecords",
            "counts",
            "pages",
            "omitted",
            "workers",
            "forcedAttaches",
            "watch",
            "notes",
        ] {
            assert!(doc.get(key).is_some(), "missing top-level {key}: {doc}");
        }
        assert_eq!(doc["schema"], 1);
        assert_eq!(doc["exitCode"], 0);
        assert_eq!(doc["staleRelayRecords"], 2);
        assert_eq!(doc["counts"]["omitted"], 2);
        assert_eq!(doc["omitted"]["titlesCut"], 1);
        assert_eq!(doc["extension"]["verdict"], "current");
        assert_eq!(doc["extension"]["behindPublished"], false);
        assert_eq!(doc["connection"]["state"], "connected");
        let page = &doc["pages"][0];
        for key in [
            "handle",
            "targetId",
            "chromeTabId",
            "windowId",
            "index",
            "title",
            "titleCut",
            "url",
            "urlCut",
            "active",
            "visible",
            "discarded",
            "attached",
            "owner",
            "ownerLabel",
            "rendererPid",
            "metrics",
            "metricsBefore",
            "measure",
            "delta",
            "leakClass",
            "leakSignals",
        ] {
            assert!(page.get(key).is_some(), "missing page {key}: {page}");
        }
        assert_eq!(
            page["title"].as_str().unwrap().chars().count(),
            TITLE_MAX_CHARS
        );
        assert_eq!(page["titleCut"], true);
        assert_eq!(page["rendererPid"], Value::Null);
        assert_eq!(page["owner"], json!({"kind": "user"}));
        assert_eq!(page["measure"]["status"], "measured");
        assert_eq!(page["measure"]["via"], "temporary_attach");
        assert_eq!(page["metrics"]["JSEventListeners"], 1300);
        assert_eq!(page["metricsBefore"]["JSEventListeners"], 300);
        assert_eq!(page["delta"]["JSEventListeners"], 1000);
        assert_eq!(page["delta"]["seconds"], 60.0);
        assert_eq!(page["leakClass"], LEAK_LISTENERS_NODES_FLAT);
        assert_eq!(doc["watch"]["intervalSeconds"], 1);
        assert_eq!(doc["watch"]["leakClasses"][LEAK_LISTENERS_NODES_FLAT], 1);
        assert_eq!(
            doc["watch"]["thresholds"]["heapGrowingBytesPerMinute"],
            int(HEAP_GROWING_BYTES_PER_MIN)
        );
        let w = &doc["workers"];
        assert_eq!(w["sites"][0]["site"], "https://x.example");
        assert_eq!(w["sites"][0]["usedSize"], 1000);

        // A run that could not connect has the same top-level keys.
        let down = not_connected_doc(&opts, "s1", None, "relay not running", EXIT_NOT_CONNECTED);
        for key in doc.as_object().unwrap().keys() {
            assert!(down.get(key).is_some(), "not-connected doc lacks {key}");
        }
        assert_eq!(down["exitCode"], 2);
        assert_eq!(down["exitReason"], "not_connected");
        assert_eq!(down["connection"]["state"], "not_connected");
        assert_eq!(down["pages"], json!([]));
    }

    #[test]
    fn a_skipped_page_says_why_and_has_no_metrics() {
        let p = obs(
            "chrome-tab:9",
            session("alpha", Some(true)),
            Outcome::Skipped {
                code: "held_by_other_session",
                detail: "held".into(),
            },
        );
        let row = page_json(&p);
        assert_eq!(row["metrics"], Value::Null);
        assert_eq!(row["measure"]["status"], "skipped");
        assert_eq!(row["measure"]["reason"], "held_by_other_session");
        assert_eq!(
            row["owner"],
            json!({"kind": "session", "session": "alpha", "live": true})
        );
    }
}

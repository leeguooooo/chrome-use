//! Only live tabs from the relay's target list (#519).
//!
//! Over the extension relay, `Target.getTargets` is the native host's record
//! of what the extension announced, not Chrome's. Extensions up to 0.5.33 can
//! leave records of closed tabs there for good: on one machine 135 of 173
//! "pages" were dead tabs, listed as `{type: page, url: "", title: "",
//! attached: true}`, and attaching to one succeeded. 0.5.34 keeps the host in
//! step, but the CLI must not trust an older one, so every target list read
//! over the relay passes through [`confirm_live`]: a page is kept only when it
//! can be confirmed live, and a phantom is never treated as a real page.
//!
//! Evidence, cheapest first:
//! 1. `ABExt.attachedTargets` (the extension's own records, target → Chrome
//!    tab id) and `ABExt.call tabs.query` (the tabs that exist). A target the
//!    extension holds in an open tab is live.
//! 2. Anything else is asked about with `ABExt.tabPresence` (0.5.33+), which
//!    reads Chrome's own target registry: `present` keeps it (a record the
//!    extension forgot while its tab lives on), anything else drops it.
//! 3. With no evidence at all (an extension too old for both), a page is kept
//!    unless it has the phantom's shape: no url and no title.
//!
//! A target confirmed dead is remembered: Chrome never reuses a target id, so
//! later reads drop it without asking again.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::time::Duration;

use futures_util::stream::{self, StreamExt};
use serde_json::{json, Value};

use super::cdp::client::CdpClient;
use super::cdp::types::TargetInfo;

/// The most one evidence call may take; an extension that does not answer in
/// time gives no evidence.
const CALL_TIMEOUT: Duration = Duration::from_secs(3);
/// `ABExt.tabPresence` reads in flight at once.
const PRESENCE_CONCURRENCY: usize = 8;
/// Dead target ids remembered; the set is cleared when it grows past this.
const DEAD_LIMIT: usize = 4096;

static DEAD: Mutex<Option<HashSet<String>>> = Mutex::new(None);

fn known_dead(target_id: &str) -> bool {
    DEAD.lock()
        .ok()
        .and_then(|d| d.as_ref().map(|d| d.contains(target_id)))
        .unwrap_or(false)
}

fn remember_dead(target_id: &str) {
    if let Ok(mut guard) = DEAD.lock() {
        let dead = guard.get_or_insert_with(HashSet::new);
        if dead.len() >= DEAD_LIMIT {
            dead.clear();
        }
        dead.insert(target_id.to_string());
    }
}

/// What the extension says about its tabs. `None` = could not be read.
#[derive(Debug, Default, Clone)]
pub(crate) struct Evidence {
    /// targetId → Chrome tab id, from `ABExt.attachedTargets`.
    pub held: Option<HashMap<String, i64>>,
    /// Chrome tab ids that exist, from `ABExt.call tabs.query`.
    pub open_tabs: Option<HashSet<i64>>,
}

/// The first-pass verdict on one page target from [`Evidence`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FirstPass {
    /// Held by the extension, in a tab that exists.
    Live,
    /// The extension does not hold it, or holds it in a tab that is gone.
    Suspect,
    /// Not enough evidence either way.
    Unknown,
}

pub(crate) fn first_pass(target_id: &str, evidence: &Evidence) -> FirstPass {
    let Some(held) = &evidence.held else {
        return FirstPass::Unknown;
    };
    match held.get(target_id) {
        None => FirstPass::Suspect,
        Some(tab) => match &evidence.open_tabs {
            Some(open) if open.contains(tab) => FirstPass::Live,
            Some(_) => FirstPass::Suspect,
            None => FirstPass::Unknown,
        },
    }
}

/// What `ABExt.tabPresence` said about one target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Presence {
    Present,
    /// Chrome's registry was read and does not list it (or its exact tab is
    /// absent): dead for good.
    Gone,
    /// An answer that confirms nothing (an error, an older extension, an
    /// unreadable registry).
    NoAnswer,
}

/// Read one `ABExt.tabPresence` reply. Only a reply with the contract version
/// that names the same target is evidence.
pub(crate) fn presence_from_reply(reply: &Result<Value, String>, target_id: &str) -> Presence {
    let Ok(v) = reply else {
        return Presence::NoAnswer;
    };
    if v.get("tabPresenceVersion").and_then(Value::as_u64) != Some(1)
        || v.get("targetId").and_then(Value::as_str) != Some(target_id)
    {
        return Presence::NoAnswer;
    }
    match v.get("presence").and_then(Value::as_str) {
        Some("present") => Presence::Present,
        Some("absent") => Presence::Gone,
        _ if v.get("listed").and_then(Value::as_bool) == Some(false) => Presence::Gone,
        _ => Presence::NoAnswer,
    }
}

/// The final call on one page target.
pub(crate) fn keep_target(
    target: &TargetInfo,
    first: FirstPass,
    presence: Option<Presence>,
) -> (bool, bool) {
    // (keep, remember as dead)
    match (first, presence) {
        (FirstPass::Live, _) | (_, Some(Presence::Present)) => (true, false),
        (_, Some(Presence::Gone)) => (false, true),
        (FirstPass::Suspect, _) => (false, false),
        (FirstPass::Unknown, _) => (!looks_like_phantom(target), false),
    }
}

/// The shape every phantom of #519 had: a page with no url and no title.
pub(crate) fn looks_like_phantom(target: &TargetInfo) -> bool {
    target.url.is_empty() && target.title.is_empty()
}

fn is_page(target: &TargetInfo) -> bool {
    target.target_type == "page" || target.target_type == "webview"
}

async fn call(client: &CdpClient, method: &str, params: Option<Value>) -> Result<Value, String> {
    match tokio::time::timeout(CALL_TIMEOUT, client.send_command(method, params, None)).await {
        Ok(result) => result,
        Err(_) => Err(format!("{method} did not answer in time")),
    }
}

async fn read_evidence(client: &CdpClient) -> Evidence {
    let (held, open) = tokio::join!(
        call(client, "ABExt.attachedTargets", None),
        call(
            client,
            "ABExt.call",
            Some(json!({ "namespace": "tabs", "method": "query", "args": [{}] })),
        ),
    );
    let held = held.ok().and_then(|v| {
        let rows = v.get("targets")?.as_array()?;
        let mut map = HashMap::new();
        for row in rows {
            let target = row.get("targetId")?.as_str()?;
            let tab = row.get("tabId")?.as_i64()?;
            map.insert(target.to_string(), tab);
        }
        Some(map)
    });
    let open_tabs = open.ok().and_then(|v| {
        let rows = v.get("result")?.as_array()?;
        rows.iter()
            .map(|t| t.get("id").and_then(Value::as_i64))
            .collect::<Option<HashSet<i64>>>()
    });
    Evidence { held, open_tabs }
}

/// The page targets of `targets` that can be confirmed live, with every
/// non-page target passed through unchanged. Call only over the relay.
pub(crate) async fn confirm_live(client: &CdpClient, targets: Vec<TargetInfo>) -> Vec<TargetInfo> {
    let targets: Vec<TargetInfo> = targets
        .into_iter()
        .filter(|t| !(is_page(t) && known_dead(&t.target_id)))
        .collect();
    if !targets.iter().any(is_page) {
        return targets;
    }
    let evidence = read_evidence(client).await;
    let firsts: Vec<FirstPass> = targets
        .iter()
        .map(|t| first_pass(&t.target_id, &evidence))
        .collect();
    // Ask Chrome's registry only about pages the first pass did not confirm.
    let to_ask: Vec<(String, Option<i64>)> = targets
        .iter()
        .zip(&firsts)
        .filter(|(t, first)| is_page(t) && **first != FirstPass::Live)
        .map(|(t, _)| {
            let tab = evidence
                .held
                .as_ref()
                .and_then(|h| h.get(&t.target_id).copied());
            (t.target_id.clone(), tab)
        })
        .collect();
    let answers: HashMap<String, Presence> = stream::iter(to_ask)
        .map(|(target_id, tab)| async move {
            let reply = call(
                client,
                "ABExt.tabPresence",
                Some(json!({ "targetId": target_id, "tabId": tab })),
            )
            .await;
            let presence = presence_from_reply(&reply, &target_id);
            (target_id, presence)
        })
        .buffer_unordered(PRESENCE_CONCURRENCY)
        .collect()
        .await;
    targets
        .into_iter()
        .zip(firsts)
        .filter(|(t, first)| {
            if !is_page(t) {
                return true;
            }
            let (keep, dead) = keep_target(t, *first, answers.get(&t.target_id).copied());
            if dead {
                remember_dead(&t.target_id);
            }
            keep
        })
        .map(|(t, _)| t)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::SinkExt;
    use std::sync::Arc;
    use tokio_tungstenite::tungstenite::Message;

    fn page(id: &str, url: &str, title: &str) -> TargetInfo {
        TargetInfo {
            target_id: id.to_string(),
            target_type: "page".to_string(),
            title: title.to_string(),
            url: url.to_string(),
            attached: Some(true),
            browser_context_id: None,
        }
    }

    #[test]
    fn first_pass_needs_the_extensions_record_and_an_open_tab() {
        let ev = Evidence {
            held: Some(HashMap::from([("A".into(), 1), ("B".into(), 2)])),
            open_tabs: Some(HashSet::from([1])),
        };
        assert_eq!(first_pass("A", &ev), FirstPass::Live);
        assert_eq!(first_pass("B", &ev), FirstPass::Suspect, "its tab is gone");
        assert_eq!(first_pass("C", &ev), FirstPass::Suspect, "not held");
        let no_tabs = Evidence {
            open_tabs: None,
            ..ev.clone()
        };
        assert_eq!(first_pass("A", &no_tabs), FirstPass::Unknown);
        assert_eq!(first_pass("C", &no_tabs), FirstPass::Suspect);
        assert_eq!(first_pass("A", &Evidence::default()), FirstPass::Unknown);
    }

    #[test]
    fn tab_presence_counts_only_a_versioned_answer_for_the_same_target() {
        let ok = |v: Value| Ok::<Value, String>(v);
        assert_eq!(
            presence_from_reply(
                &ok(json!({"tabPresenceVersion": 1, "targetId": "T", "presence": "present"})),
                "T"
            ),
            Presence::Present
        );
        assert_eq!(
            presence_from_reply(
                &ok(
                    json!({"tabPresenceVersion": 1, "targetId": "T", "presence": "absent", "tabId": 4})
                ),
                "T"
            ),
            Presence::Gone
        );
        assert_eq!(
            presence_from_reply(
                &ok(
                    json!({"tabPresenceVersion": 1, "targetId": "T", "presence": "unknown", "listed": false})
                ),
                "T"
            ),
            Presence::Gone,
            "0.5.34 says the registry does not list it"
        );
        assert_eq!(
            presence_from_reply(
                &ok(json!({"tabPresenceVersion": 1, "targetId": "T", "presence": "unknown"})),
                "T"
            ),
            Presence::NoAnswer
        );
        assert_eq!(
            presence_from_reply(
                &ok(json!({"tabPresenceVersion": 1, "targetId": "U", "presence": "present"})),
                "T"
            ),
            Presence::NoAnswer
        );
        assert_eq!(
            presence_from_reply(&ok(json!({"targetId": "T", "presence": "present"})), "T"),
            Presence::NoAnswer
        );
        assert_eq!(
            presence_from_reply(&Err("no attached tab for targetId T".into()), "T"),
            Presence::NoAnswer
        );
    }

    #[test]
    fn a_phantom_is_never_kept_on_weak_evidence() {
        let phantom = page("P", "", "");
        let real = page("R", "https://x/", "X");
        // Suspect: dropped unless Chrome's registry confirms it.
        assert_eq!(
            keep_target(&phantom, FirstPass::Suspect, Some(Presence::NoAnswer)),
            (false, false)
        );
        assert_eq!(keep_target(&real, FirstPass::Suspect, None), (false, false));
        assert_eq!(
            keep_target(&real, FirstPass::Suspect, Some(Presence::Present)),
            (true, false)
        );
        assert_eq!(
            keep_target(&real, FirstPass::Suspect, Some(Presence::Gone)),
            (false, true)
        );
        // No evidence: only the phantom shape is dropped.
        assert_eq!(
            keep_target(&phantom, FirstPass::Unknown, Some(Presence::NoAnswer)),
            (false, false)
        );
        assert_eq!(
            keep_target(&real, FirstPass::Unknown, Some(Presence::NoAnswer)),
            (true, false)
        );
        assert_eq!(keep_target(&phantom, FirstPass::Live, None), (true, false));
    }

    /// A fake relay: `getTargets` is what the host recorded (with phantoms),
    /// and the extension side answers like ab-connect `version`.
    #[derive(Clone, Copy, PartialEq)]
    enum Ext {
        /// 0.5.34: attachedTargets, tabs.query, tabPresence with `listed`.
        V534,
        /// 0.5.33: same, tabPresence without `listed`.
        V533,
        /// Older than attachedTargets and tabPresence: errors for both.
        Old,
    }

    struct Fake {
        ext: Ext,
        /// (targetId, Chrome tab id) the extension holds.
        held: Vec<(String, i64)>,
        /// Open Chrome tabs → the target in each (Chrome's registry).
        open: Vec<(i64, String)>,
        presence_calls: Vec<String>,
    }

    async fn serve(fake: Arc<std::sync::Mutex<Fake>>) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://127.0.0.1:{}", listener.local_addr().unwrap().port());
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let fake = fake.clone();
                tokio::spawn(async move {
                    let Ok(mut ws) = tokio_tungstenite::accept_async(stream).await else {
                        return;
                    };
                    while let Some(Ok(Message::Text(text))) = ws.next().await {
                        let req: Value = serde_json::from_str(&text).unwrap();
                        let reply = answer(&fake, &req);
                        let out = match reply {
                            Ok(result) => json!({"id": req["id"], "result": result}),
                            Err(e) => {
                                json!({"id": req["id"], "error": {"code": -32000, "message": e}})
                            }
                        };
                        if ws
                            .send(Message::Text(out.to_string().into()))
                            .await
                            .is_err()
                        {
                            return;
                        }
                    }
                });
            }
        });
        url
    }

    fn answer(fake: &Arc<std::sync::Mutex<Fake>>, req: &Value) -> Result<Value, String> {
        let mut f = fake.lock().unwrap();
        let params = &req["params"];
        match (req["method"].as_str().unwrap_or(""), f.ext) {
            ("ABExt.attachedTargets", Ext::Old) | ("ABExt.tabPresence", Ext::Old) => Err(format!(
                "no attached tab for targetId {}",
                params["targetId"]
            )),
            ("ABExt.attachedTargets", _) => Ok(json!({"targets": f.held.iter().map(|(t, tab)|
                json!({"targetId": t, "tabId": tab, "attached": true})).collect::<Vec<_>>()})),
            ("ABExt.call", Ext::Old) => Err("unknown method".into()),
            ("ABExt.call", _) => Ok(json!({"result": f.open.iter().map(|(tab, _)|
                json!({"id": tab})).collect::<Vec<_>>()})),
            ("ABExt.tabPresence", ext) => {
                let target = params["targetId"].as_str().unwrap_or("").to_string();
                f.presence_calls.push(target.clone());
                let listed = f.open.iter().find(|(_, t)| *t == target);
                Ok(match listed {
                    Some((tab, _)) => json!({"tabPresenceVersion": 1, "targetId": target,
                        "tabId": tab, "presence": "present"}),
                    None if ext == Ext::V534 => json!({"tabPresenceVersion": 1, "targetId": target,
                        "tabId": null, "presence": "unknown", "listed": false}),
                    None => json!({"tabPresenceVersion": 1, "targetId": target,
                        "tabId": null, "presence": "unknown"}),
                })
            }
            (m, _) => Err(format!("unexpected {m}")),
        }
    }

    /// What the host recorded: two live tabs, a record the extension forgot
    /// while its tab lives on, a held record of a closed tab, and the
    /// url-less, title-less phantoms of #519 — plus a worker.
    // Each test prefixes its target ids (`p`): the dead-target memory is
    // process-wide and tests run in parallel.
    fn recorded(p: &str) -> Vec<TargetInfo> {
        let mut out = vec![
            page(&format!("{p}LIVE1"), "https://a/", "A"),
            page(&format!("{p}LIVE2"), "https://b/", "B"),
            page(&format!("{p}FORGOTTEN"), "https://c/", "C"),
            page(&format!("{p}HELD-DEAD"), "https://d/", "D"),
        ];
        for i in 0..5 {
            out.push(page(&format!("{p}PHANTOM{i}"), "", ""));
        }
        out.push(TargetInfo {
            target_type: "worker".into(),
            ..page(&format!("{p}W"), "https://a/w.js", "")
        });
        out
    }

    fn ids(targets: &[TargetInfo], p: &str) -> Vec<String> {
        let mut ids: Vec<String> = targets
            .iter()
            .map(|t| t.target_id.trim_start_matches(p).to_string())
            .collect();
        ids.sort_unstable();
        ids
    }

    fn fake(ext: Ext, p: &str) -> Arc<std::sync::Mutex<Fake>> {
        Arc::new(std::sync::Mutex::new(Fake {
            ext,
            held: vec![
                (format!("{p}LIVE1"), 1),
                (format!("{p}LIVE2"), 2),
                (format!("{p}HELD-DEAD"), 9),
            ],
            open: vec![
                (1, format!("{p}LIVE1")),
                (2, format!("{p}LIVE2")),
                (3, format!("{p}FORGOTTEN")),
            ],
            presence_calls: vec![],
        }))
    }

    #[tokio::test]
    async fn phantoms_from_the_relay_are_dropped_with_a_current_extension() {
        let p = "a-";
        let f = fake(Ext::V534, p);
        let client = CdpClient::connect(&serve(f.clone()).await).await.unwrap();
        let live = confirm_live(&client, recorded(p)).await;
        assert_eq!(ids(&live, p), vec!["FORGOTTEN", "LIVE1", "LIVE2", "W"]);
        // Live held tabs were confirmed without a registry read.
        let asked: HashSet<String> = f.lock().unwrap().presence_calls.drain(..).collect();
        assert!(
            !asked.contains("a-LIVE1") && !asked.contains("a-LIVE2"),
            "{asked:?}"
        );
        assert!(
            asked.contains("a-PHANTOM0") && asked.contains("a-HELD-DEAD"),
            "{asked:?}"
        );
        // Confirmed dead ids are not asked about again.
        let again = confirm_live(&client, recorded(p)).await;
        assert_eq!(ids(&again, p), vec!["FORGOTTEN", "LIVE1", "LIVE2", "W"]);
        let asked: Vec<String> = f.lock().unwrap().presence_calls.drain(..).collect();
        assert_eq!(
            asked,
            vec!["a-FORGOTTEN".to_string()],
            "only the still-unheld live tab"
        );
    }

    #[tokio::test]
    async fn phantoms_are_dropped_with_ab_connect_0_5_33() {
        let p = "b-";
        let f = fake(Ext::V533, p);
        let client = CdpClient::connect(&serve(f.clone()).await).await.unwrap();
        let live = confirm_live(&client, recorded(p)).await;
        assert_eq!(ids(&live, p), vec!["FORGOTTEN", "LIVE1", "LIVE2", "W"]);
    }

    #[tokio::test]
    async fn an_extension_without_evidence_keeps_real_pages_and_drops_the_phantom_shape() {
        let p = "c-";
        let f = fake(Ext::Old, p);
        let client = CdpClient::connect(&serve(f.clone()).await).await.unwrap();
        let live = confirm_live(&client, recorded(p)).await;
        assert_eq!(
            ids(&live, p),
            vec!["FORGOTTEN", "HELD-DEAD", "LIVE1", "LIVE2", "W"],
            "nothing to confirm with: only the url-less, title-less pages go"
        );
    }

    #[tokio::test]
    async fn an_empty_or_page_free_list_asks_nothing() {
        let p = "d-";
        let f = fake(Ext::V534, p);
        let client = CdpClient::connect(&serve(f.clone()).await).await.unwrap();
        assert!(confirm_live(&client, vec![]).await.is_empty());
        let worker = TargetInfo {
            target_type: "service_worker".into(),
            ..page("d-SW", "https://a/sw.js", "")
        };
        assert_eq!(
            ids(&confirm_live(&client, vec![worker]).await, p),
            vec!["SW"]
        );
        assert!(f.lock().unwrap().presence_calls.is_empty());
    }
}

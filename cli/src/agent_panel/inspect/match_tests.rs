//! Matching, scopes, claims and the session-side CLI, end to end against a
//! fake `System` and real temporary directories.
use super::testkit::*;
use super::*;

const TEST: &str = "https://test-admin.x.com";

/// Create a directory and return its canonical path, as the host reports
/// paths (on Windows: backslashes and a verbatim prefix).
fn mkdir(p: &Path) -> PathBuf {
    fs::create_dir_all(p).unwrap();
    fs::canonicalize(p).unwrap()
}

fn file(p: &Path, body: &str) {
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, body).unwrap();
}

fn base(w: &World) -> PathBuf {
    w.project.parent().unwrap().to_path_buf()
}

fn resolve(w: &World, sys: &Fake, origin: &str, page: &str, extra: Value) -> Value {
    let mut p = json!({"origin": origin, "pagePath": page});
    for (k, v) in extra.as_object().unwrap() {
        p[k] = v.clone();
    }
    call(w, sys, "inspectResolve", p).unwrap()
}

/// Bind exactly what a card pair showed.
fn bind_pair(w: &World, sys: &Fake, pair: &Value) -> Result<Value, String> {
    call(
        w,
        sys,
        "inspectBind",
        json!({
            "origin": pair["origin"], "pathScope": pair["pathScope"], "root": pair["root"],
            "sourceRoots": pair["sourceRoots"], "sessionId": pair["sessionId"],
            "fingerprint": pair["fingerprint"],
        }),
    )
}

fn cli(w: &World, sys: &Fake, me: &str, args: &[&str]) -> Result<cli::Done, String> {
    sys.me.replace(Some(me.into()));
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    Inspect {
        root: w.data.clone(),
        sys,
        profile: None,
    }
    .cli(&args)
}

fn deliver(
    w: &World,
    sys: &Fake,
    origin: &str,
    url: &str,
    page: Option<&str>,
    extra: Value,
) -> Value {
    let mut p = json!({
        "origin": origin, "kind": "edit", "instruction": "fix", "pageUrl": url, "env": "test",
        "annotations": [{"label": "x", "selector": "x", "markdown": "x"}],
        "deliveryId": uuid::Uuid::new_v4().to_string(),
    });
    if let Some(page) = page {
        p["pagePath"] = page.into();
        // What the panel would quote: the tab target it holds, else the
        // effective binding it just resolved.
        p["expectedTarget"] = match extra.get("target") {
            Some(t) => t["fingerprint"].clone(),
            None => {
                let r = resolve(w, sys, origin, page, json!({}));
                match r["binding"]["fingerprint"].as_str() {
                    Some(f) => f.into(),
                    None => "0".repeat(32).into(),
                }
            }
        };
    }
    for (k, v) in extra.as_object().unwrap() {
        p[k] = v.clone();
    }
    call(w, sys, "inspectDeliver", p).unwrap()
}

fn store_file(w: &World) -> Value {
    serde_json::from_slice(&fs::read(w.data.join("bindings.json")).unwrap()).unwrap()
}

fn evidence(pair: &Value) -> Vec<String> {
    pair["evidence"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e.as_str().unwrap().to_string())
        .collect()
}

/// A monorepo with an admin app, a shop app and a shared package.
struct Mono {
    top: PathBuf,
    admin: PathBuf,
    ui: PathBuf,
}

fn mono(w: &World, name: &str) -> Mono {
    let top = mkdir(&base(w).join(name));
    let admin = mkdir(&top.join("apps/admin/src"));
    file(&admin.join("x.tsx"), "x");
    file(&top.join("apps/shop/src/y.tsx"), "y");
    file(&top.join("shared/z.tsx"), "z");
    let ui = mkdir(&top.join("packages/ui"));
    file(&ui.join("Button.tsx"), "b");
    Mono {
        admin: admin.parent().unwrap().to_path_buf(),
        top,
        ui,
    }
}

const CONFIG: &str = "apps/admin/.chrome-use/inspect.json";

fn config(sys_configs: &mut Vec<(PathBuf, Vec<String>)>, top: &Path, rel: &str, body: &str) {
    file(&top.join(rel), body);
    match sys_configs.iter_mut().find(|(t, _)| t == top) {
        Some((_, files)) => files.push(rel.into()),
        None => sys_configs.push((top.to_path_buf(), vec![rel.into()])),
    }
}

#[test]
fn a_dev_server_in_a_monorepo_app_preselects_that_app_and_binds_in_one_click() {
    let w = world();
    let m = mono(&w, "repo");
    let sys = Fake {
        installed: true,
        sessions: vec![session("claude-aaaaaaaa", &m.top)],
        port: Some(m.admin.clone()),
        tops: vec![m.top.clone()],
        ..Fake::default()
    };
    let hints = json!({"sourcePaths": ["apps/admin/src/x.tsx", "/ci/ws/apps/admin/src/x.tsx", "apps/shop/src/y.tsx"]});
    let r = resolve(&w, &sys, ORIGIN, "/", hints.clone());
    assert_eq!(r["state"], "unbound");
    assert_eq!(r["schemaVersion"], 1);
    let pre = &r["card"]["preselected"];
    assert_eq!(pre["root"], json!(m.admin));
    assert_eq!(pre["sessionId"], "claude-aaaaaaaa");
    assert_eq!(pre["pathScope"], "/");
    assert!(evidence(pre).contains(&"dev-server-port".into()));
    assert_eq!(pre["matchedPaths"], 2);
    // The worktree root is a separate, weaker candidate.
    let pairs = r["card"]["pairs"].as_array().unwrap();
    assert!(pairs.iter().any(|p| p["root"] == json!(m.top)));
    assert_eq!(r["pendingClaims"], json!([]));

    let bound = bind_pair(&w, &sys, pre).unwrap();
    assert_eq!(bound["binding"]["source"], "confirmed");
    assert_eq!(bound["binding"]["evidence"]["kind"], "port");
    assert_eq!(bound["binding"]["evidence"]["cwd"], json!(m.admin));
    assert_eq!(bound["binding"]["fingerprint"], pre["fingerprint"]);
    let again = resolve(&w, &sys, ORIGIN, "/settings", json!({}));
    assert_eq!(again["state"], "bound");
    // A bound scope still offers its candidates so the target can be
    // changed (or held for one tab), but preselects none of them.
    assert!(again["card"].get("preselected").is_none());
    let offered = again["card"]["pairs"].as_array().unwrap();
    assert_eq!(offered.len(), pairs.len());
    let same = offered
        .iter()
        .find(|p| p["root"] == json!(m.admin))
        .unwrap();
    assert_eq!(same["fingerprint"], pre["fingerprint"]);
    assert!(offered.iter().any(|p| p["root"] == json!(m.top)));
    assert_eq!(again["sessions"][0]["id"], "claude-aaaaaaaa");
    assert_eq!(again["binding"]["session"]["id"], "claude-aaaaaaaa");

    // Source paths resolve only inside the bound app: repo-relative with the
    // app's prefix stripped, absolute by suffix; another app's file and an
    // escaping path stay unverified although both exist in the worktree.
    let marks = json!({"annotations": [
        {"label": "a", "sourcePath": "apps/admin/src/x.tsx", "selector": "a", "markdown": "a"},
        {"label": "b", "sourcePath": "/ci/ws/apps/admin/src/x.tsx", "selector": "b", "markdown": "b"},
        {"label": "c", "sourcePath": "apps/shop/src/y.tsx", "selector": "c", "markdown": "c"},
        {"label": "d", "sourcePath": "../../shared/z.tsx", "selector": "d", "markdown": "d"}
    ]});
    let sent = deliver(
        &w,
        &sys,
        ORIGIN,
        "http://localhost:5173/x",
        Some("/x"),
        marks,
    );
    assert_eq!(sent["state"], "sent");
    let message = sys.sent.borrow()[0].1.clone();
    assert!(message.contains("1. a — src/x.tsx (verified)"), "{message}");
    assert!(
        message.contains("2. b — src/x.tsx (matched by path suffix"),
        "{message}"
    );
    assert!(
        message.contains("3. c — apps/shop/src/y.tsx (unverified)"),
        "{message}"
    );
    assert!(
        message.contains("4. d — ../../shared/z.tsx (unverified)"),
        "{message}"
    );

    // The dev server restarted from another worktree on the same port.
    let other = mono(&w, "other");
    let moved = Fake {
        installed: true,
        sessions: vec![session("claude-aaaaaaaa", &m.top)],
        port: Some(other.admin.clone()),
        tops: vec![m.top.clone(), other.top.clone()],
        ..Fake::default()
    };
    let r = resolve(&w, &moved, ORIGIN, "/", json!({}));
    assert_eq!(
        (r["state"].as_str(), r["reason"].as_str()),
        (Some("needsConfirm"), Some("portMoved"))
    );
    assert_eq!(r["binding"]["root"], json!(m.admin));
    let refused = deliver(
        &w,
        &moved,
        ORIGIN,
        "http://localhost:5173/",
        Some("/"),
        json!({}),
    );
    assert_eq!(refused["state"], "targetChanged");
    assert!(moved.sent.borrow().is_empty());
    // Restarted from the worktree root (containing the app): still holds.
    let containing = Fake {
        installed: true,
        sessions: vec![session("claude-aaaaaaaa", &m.top)],
        port: Some(m.top.clone()),
        tops: vec![m.top.clone()],
        ..Fake::default()
    };
    assert_eq!(
        resolve(&w, &containing, ORIGIN, "/", json!({}))["state"],
        "bound"
    );
}

#[test]
fn a_test_domain_config_preselects_and_exact_origins_outrank_wildcards() {
    let w = world();
    let m = mono(&w, "repo");
    let wild = mono(&w, "wild");
    let mut configs = Vec::new();
    config(
        &mut configs,
        &m.top,
        CONFIG,
        r#"{"version":1,"sourceRoots":["../../packages/ui"],"sites":[{"origins":["https://test-admin.x.com"]}]}"#,
    );
    config(
        &mut configs,
        &wild.top,
        CONFIG,
        r#"{"version":1,"sites":[{"origins":["https://*.x.com"]}]}"#,
    );
    let sys = Fake {
        installed: true,
        sessions: vec![
            session("claude-aaaaaaaa", &m.admin),
            session("claude-bbbbbbbb", &wild.admin),
        ],
        tops: vec![m.top.clone(), wild.top.clone()],
        configs,
        ..Fake::default()
    };
    let r = resolve(&w, &sys, TEST, "/orders", json!({}));
    let pairs = r["card"]["pairs"].as_array().unwrap();
    assert_eq!(pairs[0]["root"], json!(m.admin));
    assert_eq!(evidence(&pairs[0])[0], "config");
    assert_eq!(pairs[0]["sourceRoots"], json!([m.admin, m.ui]));
    assert_eq!(pairs[1]["root"], json!(wild.admin));
    assert_eq!(evidence(&pairs[1])[0], "config-wildcard");
    let pre = &r["card"]["preselected"];
    assert_eq!(pre["root"], json!(m.admin));
    let bound = bind_pair(&w, &sys, pre).unwrap();
    assert_eq!(bound["binding"]["sourceRoots"], json!([m.admin, m.ui]));
    assert_eq!(bound["binding"]["evidence"]["kind"], "config");
    assert_eq!(resolve(&w, &sys, TEST, "/", json!({}))["state"], "bound");
    // A shared-package path resolves in its confirmed source root.
    let sent = deliver(
        &w,
        &sys,
        TEST,
        "https://test-admin.x.com/",
        Some("/"),
        json!({"annotations": [
        {"label": "btn", "sourcePath": "packages/ui/Button.tsx", "selector": "b", "markdown": "b"}]}),
    );
    assert_eq!(sent["state"], "sent");
    let expected = format!(
        "btn — {} (verified)",
        m.ui.join("Button.tsx")
            .display()
            .to_string()
            .replace('\\', "/")
    );
    assert!(sys.sent.borrow()[0].1.contains(&expected));

    // The wildcard covers one label only, and a binding keeps the exact
    // origin, so it never extends to other subdomains.
    let other = resolve(&w, &sys, "https://shop.x.com", "/", json!({}));
    let pre = &other["card"]["preselected"];
    assert_eq!(pre["root"], json!(wild.admin));
    assert_eq!(evidence(pre)[0], "config-wildcard");
    let b = bind_pair(&w, &sys, pre).unwrap();
    assert_eq!(b["binding"]["origin"], "https://shop.x.com");
    assert_eq!(b["binding"]["evidence"]["wildcard"], true);
    assert_eq!(
        resolve(&w, &sys, "https://cart.x.com", "/", json!({}))["state"],
        "unbound"
    );
    let deep = resolve(&w, &sys, "https://a.b.x.com", "/", json!({}));
    assert!(deep["card"]["pairs"]
        .as_array()
        .unwrap()
        .iter()
        .all(|p| !evidence(p).iter().any(|e| e.starts_with("config"))));
}

#[test]
fn path_scoped_apps_on_one_origin_resolve_to_their_own_roots() {
    let w = world();
    let m = mono(&w, "repo");
    let shop = mkdir(&m.top.join("apps/shop"));
    let mut configs = Vec::new();
    config(
        &mut configs,
        &m.top,
        CONFIG,
        r#"{"version":1,"sites":[{"origins":["https://test-admin.x.com"],"pathScope":"/admin"}]}"#,
    );
    config(
        &mut configs,
        &m.top,
        "apps/shop/.chrome-use/inspect.json",
        r#"{"version":1,"sites":[{"origins":["https://test-admin.x.com"],"pathScope":"/shop"}]}"#,
    );
    let sys = Fake {
        installed: true,
        sessions: vec![
            session("claude-aaaaaaaa", &m.admin),
            session("claude-bbbbbbbb", &shop),
        ],
        tops: vec![m.top.clone()],
        configs,
        ..Fake::default()
    };
    let admin = resolve(&w, &sys, TEST, "/admin/orders", json!({}));
    let pre = &admin["card"]["preselected"];
    assert_eq!(
        (pre["root"].clone(), pre["pathScope"].clone()),
        (json!(m.admin), json!("/admin"))
    );
    bind_pair(&w, &sys, pre).unwrap();
    let shop_r = resolve(&w, &sys, TEST, "/shop", json!({}));
    let pre = &shop_r["card"]["preselected"];
    assert_eq!(
        (pre["root"].clone(), pre["pathScope"].clone()),
        (json!(shop), json!("/shop"))
    );
    bind_pair(&w, &sys, pre).unwrap();
    assert_eq!(
        resolve(&w, &sys, TEST, "/admin/x", json!({}))["binding"]["root"],
        json!(m.admin)
    );
    assert_eq!(
        resolve(&w, &sys, TEST, "/shop/cart", json!({}))["binding"]["root"],
        json!(shop)
    );
    let neither = resolve(&w, &sys, TEST, "/administrator", json!({}));
    assert_eq!(neither["state"], "unbound");
    assert!(neither["card"].get("preselected").is_none());
    // Delivery follows the page's own scope.
    deliver(
        &w,
        &sys,
        TEST,
        "https://test-admin.x.com/shop/cart",
        Some("/shop/cart"),
        json!({}),
    );
    deliver(
        &w,
        &sys,
        TEST,
        "https://test-admin.x.com/admin",
        Some("/admin"),
        json!({}),
    );
    let ids: Vec<String> = sys.sent.borrow().iter().map(|(id, _)| id.clone()).collect();
    assert_eq!(ids, ["claude-bbbbbbbb", "claude-aaaaaaaa"]);
    assert_eq!(
        deliver(
            &w,
            &sys,
            TEST,
            "https://test-admin.x.com/administrator",
            Some("/administrator"),
            json!({})
        )["state"],
        "targetChanged"
    );
    // pagePath must be the page URL's own path.
    let mismatch = call(
        &w,
        &sys,
        "inspectDeliver",
        json!({"origin": TEST, "kind": "edit", "pageUrl": "https://test-admin.x.com/shop", "pagePath": "/admin", "env": "test",
        "annotations": [{"label": "x", "selector": "x", "markdown": "x"}], "deliveryId": uuid::Uuid::new_v4().to_string()}),
    );
    assert!(mismatch.unwrap_err().contains("pagePath"));
}

#[test]
fn equal_matches_in_two_worktrees_or_two_sessions_are_never_preselected() {
    let w = world();
    let main = mono(&w, "repo");
    // A second worktree of the same repository, nested inside the first.
    let feat = mono(&w, "repo/.worktrees/feat");
    let mut configs = Vec::new();
    let body = r#"{"version":1,"sites":[{"origins":["https://test-admin.x.com"]}]}"#;
    config(&mut configs, &main.top, CONFIG, body);
    config(&mut configs, &feat.top, CONFIG, body);
    let sys = Fake {
        installed: true,
        sessions: vec![
            session("claude-aaaaaaaa", &main.top),
            session("claude-bbbbbbbb", &feat.top),
        ],
        tops: vec![main.top.clone(), feat.top.clone()],
        configs,
        ..Fake::default()
    };
    let r = resolve(&w, &sys, TEST, "/", json!({}));
    assert!(r["card"].get("preselected").is_none(), "{r}");
    let pairs = r["card"]["pairs"].as_array().unwrap();
    let main_pair = pairs
        .iter()
        .find(|p| p["root"] == json!(main.admin))
        .unwrap();
    let feat_pair = pairs
        .iter()
        .find(|p| p["root"] == json!(feat.admin))
        .unwrap();
    // The main session contains the feat worktree on disk, yet is never
    // related to it: different worktrees are different projects.
    assert_eq!(main_pair["sessionId"], "claude-aaaaaaaa");
    assert_eq!(feat_pair["sessionId"], "claude-bbbbbbbb");
    assert_eq!(
        pairs
            .iter()
            .filter(|p| p["root"] == json!(feat.admin))
            .count(),
        1
    );
    let mut wrong = feat_pair.clone();
    wrong["sessionId"] = "claude-aaaaaaaa".into();
    assert!(bind_pair(&w, &sys, &wrong)
        .unwrap_err()
        .contains("worktree"));

    // Two sessions in one worktree: listed, not chosen.
    let mut configs = Vec::new();
    config(&mut configs, &main.top, CONFIG, body);
    let two = Fake {
        installed: true,
        sessions: vec![
            session("claude-aaaaaaaa", &main.top),
            session("claude-cccccccc", &main.admin),
        ],
        tops: vec![main.top.clone()],
        configs,
        ..Fake::default()
    };
    let r = resolve(&w, &two, TEST, "/", json!({}));
    assert!(r["card"].get("preselected").is_none());
    let admin_pairs: Vec<&Value> = r["card"]["pairs"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|p| p["root"] == json!(main.admin))
        .collect();
    assert_eq!(admin_pairs.len(), 2);
    // The choice is remembered.
    bind_pair(&w, &two, admin_pairs[1]).unwrap();
    assert_eq!(resolve(&w, &two, TEST, "/", json!({}))["state"], "bound");
}

#[test]
fn revision_relation_ranks_the_worktree_holding_the_deployed_commit() {
    let w = world();
    let a = mono(&w, "a");
    let b = mono(&w, "b");
    let sys = Fake {
        installed: true,
        sessions: vec![
            session("claude-aaaaaaaa", &a.top),
            session("claude-bbbbbbbb", &b.top),
        ],
        tops: vec![a.top.clone(), b.top.clone()],
        relations: vec![
            (a.top.clone(), Relation::Behind(2)),
            (b.top.clone(), Relation::Ahead(3)),
        ],
        ..Fake::default()
    };
    let r = resolve(
        &w,
        &sys,
        TEST,
        "/",
        json!({"revision": "abcdef1", "sourcePaths": ["/ci/ws/apps/admin/src/x.tsx"]}),
    );
    let pairs = r["card"]["pairs"].as_array().unwrap();
    assert_eq!(pairs[0]["root"], json!(b.top));
    assert_eq!(
        pairs[0]["revision"],
        json!({"relation": "ahead", "ahead": 3, "behind": 0})
    );
    assert_eq!(pairs[1]["revision"]["relation"], "behind");
    assert_eq!(pairs[0]["matchedPaths"], 1);
    assert_eq!(pairs[1]["matchedPaths"], 1);
    // Page data never preselects without config or port evidence.
    assert!(r["card"].get("preselected").is_none());
    // A revision that is not a hex commit is ignored, never passed on.
    let r = resolve(&w, &sys, TEST, "/", json!({"revision": "--all"}));
    assert!(r["card"]["pairs"][0].get("revision").is_none());
}

#[test]
fn stale_bindings_need_confirmation_with_the_reason() {
    let w = world();
    let m = mono(&w, "repo");
    let sys = Fake {
        installed: true,
        sessions: vec![session("claude-aaaaaaaa", &m.top)],
        tops: vec![m.top.clone()],
        ..Fake::default()
    };
    let r = resolve(&w, &sys, TEST, "/", json!({}));
    let pair = r["card"]["pairs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["root"] == json!(m.top))
        .unwrap()
        .clone();
    bind_pair(&w, &sys, &pair).unwrap();
    assert_eq!(resolve(&w, &sys, TEST, "/", json!({}))["state"], "bound");
    let restarted = Fake {
        installed: true,
        sessions: vec![session("claude-bbbbbbbb", &m.top)],
        tops: vec![m.top.clone()],
        ..Fake::default()
    };
    let r = resolve(&w, &restarted, TEST, "/", json!({}));
    assert_eq!(r["reason"], "sessionEnded");
    assert_eq!(r["binding"]["session"], Value::Null);
    assert_eq!(r["card"]["pairs"][0]["sessionId"], "claude-bbbbbbbb");
    let moved = Fake {
        installed: true,
        sessions: vec![session("claude-aaaaaaaa", &m.admin)],
        tops: vec![m.top.clone()],
        ..Fake::default()
    };
    assert_eq!(
        resolve(&w, &moved, TEST, "/", json!({}))["reason"],
        "sessionMoved"
    );
    assert_eq!(
        deliver(
            &w,
            &moved,
            TEST,
            "https://test-admin.x.com/",
            Some("/"),
            json!({})
        )["state"],
        "sessionChanged"
    );
    fs::rename(&m.top, base(&w).join("renamed")).unwrap();
    assert_eq!(
        resolve(&w, &sys, TEST, "/", json!({}))["reason"],
        "rootMissing"
    );
}

#[test]
fn bind_rebuilds_the_pair_and_refuses_changed_fingerprints_or_wider_source_roots() {
    let w = world();
    let m = mono(&w, "repo");
    let mut configs = Vec::new();
    config(
        &mut configs,
        &m.top,
        CONFIG,
        r#"{"version":1,"sites":[{"origins":["https://test-admin.x.com"]}]}"#,
    );
    let sys = Fake {
        installed: true,
        sessions: vec![session("claude-aaaaaaaa", &m.admin)],
        tops: vec![m.top.clone()],
        configs,
        ..Fake::default()
    };
    let r = resolve(&w, &sys, TEST, "/", json!({}));
    let pre = r["card"]["preselected"].clone();
    let mut forged = pre.clone();
    forged["sourceRoots"] = json!([m.admin, m.ui]);
    assert!(bind_pair(&w, &sys, &forged)
        .unwrap_err()
        .contains("source roots"));
    let mut scoped = pre.clone();
    scoped["pathScope"] = "/admin".into();
    assert!(bind_pair(&w, &sys, &scoped)
        .unwrap_err()
        .contains("changed"));
    let mut wrong = pre.clone();
    wrong["fingerprint"] = "0".repeat(32).into();
    assert!(bind_pair(&w, &sys, &wrong).unwrap_err().contains("changed"));
    let moved = Fake {
        installed: true,
        sessions: vec![session("claude-aaaaaaaa", &m.top)],
        tops: vec![m.top.clone()],
        ..Fake::default()
    };
    assert!(bind_pair(&w, &moved, &pre).unwrap_err().contains("changed"));
    assert!(
        call(&w, &sys, "inspectBindings", json!({})).unwrap()["bindings"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(call(
        &w,
        &sys,
        "inspectBind",
        json!({"origin": TEST, "root": m.admin, "fingerprint": pre["fingerprint"]})
    )
    .is_err());
    assert!(bind_pair(&w, &sys, &pre).is_ok());
}

#[test]
fn claims_from_inspect_link_never_shadow_or_bypass_a_binding() {
    let w = world();
    let a = mono(&w, "a");
    let b = mono(&w, "b");
    let c = mono(&w, "c");
    let sys = Fake {
        installed: true,
        sessions: vec![
            session("claude-aaaaaaaa", &a.top),
            session("claude-bbbbbbbb", &b.top),
            session("claude-cccccccc", &c.top),
        ],
        tops: vec![a.top.clone(), b.top.clone(), c.top.clone()],
        ..Fake::default()
    };
    // Unbound scope: zero clicks.
    let linked = cli(
        &w,
        &sys,
        "claude-aaaaaaaa",
        &["link", "https://t.x.com/anything"],
    )
    .unwrap();
    assert_eq!(linked.code, 0);
    assert_eq!(linked.data["binding"]["source"], "linked");
    assert_eq!(linked.data["binding"]["pathScope"], "/");
    let a_fp = linked.data["binding"]["fingerprint"]
        .as_str()
        .unwrap()
        .to_string();
    // Re-linking the same target is a refresh, not a claim.
    assert_eq!(
        cli(&w, &sys, "claude-aaaaaaaa", &["link", "https://t.x.com/"])
            .unwrap()
            .code,
        0
    );

    // `/` belongs to A; B linking `/admin` is a claim, and `/admin` pages
    // still go to A until it is accepted.
    let claim = cli(
        &w,
        &sys,
        "claude-bbbbbbbb",
        &["link", "https://t.x.com/admin/x", "--path-scope", "/admin/"],
    )
    .unwrap();
    assert_eq!(claim.code, 2);
    assert_eq!(claim.data["state"], "pending");
    assert!(claim
        .text
        .contains("Nothing changes until the user accepts"));
    let r = resolve(&w, &sys, "https://t.x.com", "/admin/x", json!({}));
    assert_eq!(r["state"], "bound");
    assert_eq!(r["binding"]["sessionId"], "claude-aaaaaaaa");
    let claims = r["pendingClaims"].as_array().unwrap();
    assert_eq!(claims.len(), 1);
    assert_eq!(claims[0]["pathScope"], "/admin");
    assert_eq!(claims[0]["expectedAffects"], json!([a_fp]));
    assert_eq!(claims[0]["affects"][0]["sessionId"], "claude-aaaaaaaa");
    // A page the claim does not cover does not show it.
    assert!(
        resolve(&w, &sys, "https://t.x.com", "/shop", json!({}))["pendingClaims"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    deliver(
        &w,
        &sys,
        "https://t.x.com",
        "https://t.x.com/admin/x",
        Some("/admin/x"),
        json!({}),
    );
    assert_eq!(sys.sent.borrow()[0].0, "claude-aaaaaaaa");

    // A newer claim on the same scope supersedes the older one.
    let old_id = claim.data["claim"]["claimId"].as_str().unwrap().to_string();
    let newer = cli(
        &w,
        &sys,
        "claude-cccccccc",
        &["link", "https://t.x.com/admin", "--path-scope", "/admin"],
    )
    .unwrap();
    assert_eq!(newer.code, 2);
    let stale = call(
        &w,
        &sys,
        "inspectClaimDecide",
        json!({"claimId": old_id, "expectedAffects": [a_fp], "accept": true}),
    );
    assert!(stale.unwrap_err().contains("no longer pending"));
    let new_id = newer.data["claim"]["claimId"].as_str().unwrap().to_string();
    // Accept from the current card: `/admin` moves to C, `/` stays with A.
    let decided = call(
        &w,
        &sys,
        "inspectClaimDecide",
        json!({"claimId": new_id, "expectedAffects": [a_fp], "accept": true}),
    )
    .unwrap();
    assert_eq!(decided["decided"], "accepted");
    assert_eq!(decided["binding"]["source"], "linked");
    assert_eq!(
        resolve(&w, &sys, "https://t.x.com", "/admin/x", json!({}))["binding"]["sessionId"],
        "claude-cccccccc"
    );
    assert_eq!(
        resolve(&w, &sys, "https://t.x.com", "/", json!({}))["binding"]["sessionId"],
        "claude-aaaaaaaa"
    );
    assert!(call(
        &w,
        &sys,
        "inspectClaimDecide",
        json!({"claimId": new_id, "expectedAffects": [a_fp], "accept": true})
    )
    .is_err());

    // `/admin` belongs to A; B linking `/` is a claim that lists `/admin`;
    // accepting moves `/` only.
    let o = "https://u.x.com";
    cli(
        &w,
        &sys,
        "claude-aaaaaaaa",
        &["link", "https://u.x.com/admin", "--path-scope", "/admin"],
    )
    .unwrap();
    let wide = cli(&w, &sys, "claude-bbbbbbbb", &["link", "https://u.x.com/"]).unwrap();
    assert_eq!(wide.code, 2);
    assert!(wide.text.contains("/admin"));
    assert!(wide.text.contains("moves / only"));
    let card = resolve(&w, &sys, o, "/", json!({}));
    assert_eq!(card["state"], "unbound");
    let shown = card["pendingClaims"][0].clone();
    assert_eq!(shown["affects"][0]["pathScope"], "/admin");
    let expected = shown["expectedAffects"].clone();
    // A newly added overlapping binding makes that card stale.
    cli(
        &w,
        &sys,
        "claude-aaaaaaaa",
        &["link", "https://u.x.com/shop", "--path-scope", "/shop"],
    )
    .unwrap();
    let stale = call(
        &w,
        &sys,
        "inspectClaimDecide",
        json!({"claimId": shown["claimId"], "expectedAffects": expected, "accept": true}),
    );
    assert!(stale.unwrap_err().contains("changed since the card"));
    let fresh = resolve(&w, &sys, o, "/", json!({}))["pendingClaims"][0].clone();
    assert_eq!(fresh["expectedAffects"].as_array().unwrap().len(), 2);
    call(&w, &sys, "inspectClaimDecide", json!({"claimId": fresh["claimId"], "expectedAffects": fresh["expectedAffects"], "accept": true})).unwrap();
    assert_eq!(
        resolve(&w, &sys, o, "/", json!({}))["binding"]["sessionId"],
        "claude-bbbbbbbb"
    );
    assert_eq!(
        resolve(&w, &sys, o, "/admin", json!({}))["binding"]["sessionId"],
        "claude-aaaaaaaa"
    );
    assert_eq!(
        resolve(&w, &sys, o, "/shop/x", json!({}))["binding"]["sessionId"],
        "claude-aaaaaaaa"
    );

    // Two claims on different scopes coexist and are decided independently.
    let v = "https://v.x.com";
    cli(&w, &sys, "claude-aaaaaaaa", &["link", "https://v.x.com/"]).unwrap();
    let b_claim = cli(
        &w,
        &sys,
        "claude-bbbbbbbb",
        &["link", "https://v.x.com/admin", "--path-scope", "/admin"],
    )
    .unwrap();
    let c_claim = cli(
        &w,
        &sys,
        "claude-cccccccc",
        &["link", "https://v.x.com/shop", "--path-scope", "/shop"],
    )
    .unwrap();
    assert_eq!((b_claim.code, c_claim.code), (2, 2));
    let all = call(&w, &sys, "inspectBindings", json!({})).unwrap();
    assert_eq!(
        all["pendingClaims"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|c| c["origin"] == v)
            .count(),
        2
    );
    let b_view = resolve(&w, &sys, v, "/admin", json!({}))["pendingClaims"][0].clone();
    assert_eq!(b_view["sessionId"], "claude-bbbbbbbb");
    let rejected = call(&w, &sys, "inspectClaimDecide", json!({"claimId": b_view["claimId"], "expectedAffects": b_view["expectedAffects"], "accept": false})).unwrap();
    assert_eq!(rejected["decided"], "rejected");
    assert!(rejected.get("binding").is_none());
    let c_view = resolve(&w, &sys, v, "/shop", json!({}))["pendingClaims"][0].clone();
    assert_eq!(c_view["sessionId"], "claude-cccccccc");
    call(&w, &sys, "inspectClaimDecide", json!({"claimId": c_view["claimId"], "expectedAffects": c_view["expectedAffects"], "accept": true})).unwrap();
    assert_eq!(
        resolve(&w, &sys, v, "/admin", json!({}))["binding"]["sessionId"],
        "claude-aaaaaaaa"
    );
    assert_eq!(
        resolve(&w, &sys, v, "/shop", json!({}))["binding"]["sessionId"],
        "claude-cccccccc"
    );

    // A claimant that ended cannot be accepted.
    cli(&w, &sys, "claude-bbbbbbbb", &["link", "https://v.x.com/"]).unwrap();
    let gone = Fake {
        installed: true,
        sessions: vec![session("claude-aaaaaaaa", &a.top)],
        tops: vec![a.top.clone(), b.top.clone()],
        ..Fake::default()
    };
    let pending = resolve(&w, &gone, v, "/", json!({}))["pendingClaims"][0].clone();
    let refused = call(
        &w,
        &gone,
        "inspectClaimDecide",
        json!({"claimId": pending["claimId"], "expectedAffects": pending["expectedAffects"], "accept": true}),
    );
    assert!(refused.unwrap_err().contains("ended"));
}

#[test]
fn unlink_only_touches_the_calling_sessions_own_scope_and_claims() {
    let w = world();
    let a = mono(&w, "a");
    let b = mono(&w, "b");
    let sys = Fake {
        installed: true,
        sessions: vec![
            session("claude-aaaaaaaa", &a.top),
            session("claude-bbbbbbbb", &b.top),
        ],
        tops: vec![a.top.clone(), b.top.clone()],
        ..Fake::default()
    };
    cli(&w, &sys, "claude-aaaaaaaa", &["link", "https://t.x.com/"]).unwrap();
    let refused = cli(&w, &sys, "claude-bbbbbbbb", &["unlink", "https://t.x.com/"]);
    assert!(refused.err().unwrap().contains("another session"));
    assert_eq!(
        resolve(&w, &sys, "https://t.x.com", "/", json!({}))["binding"]["sessionId"],
        "claude-aaaaaaaa"
    );
    // B's own pending claim is withdrawn; A's binding stays.
    assert_eq!(
        cli(&w, &sys, "claude-bbbbbbbb", &["link", "https://t.x.com/"])
            .unwrap()
            .code,
        2
    );
    let withdrawn = cli(&w, &sys, "claude-bbbbbbbb", &["unlink", "https://t.x.com/"]).unwrap();
    assert_eq!(withdrawn.data["withdrawnClaims"], 1);
    assert_eq!(withdrawn.data["unlinked"], false);
    let store = store_file(&w);
    assert!(store["pendingClaims"].as_array().unwrap().is_empty());
    assert_eq!(store["bindings"].as_array().unwrap().len(), 1);
    // The longest scope for the URL is the one removed.
    cli(
        &w,
        &sys,
        "claude-aaaaaaaa",
        &["link", "https://t.x.com/admin", "--path-scope", "/admin"],
    )
    .unwrap();
    let done = cli(
        &w,
        &sys,
        "claude-aaaaaaaa",
        &["unlink", "https://t.x.com/admin/x"],
    )
    .unwrap();
    assert_eq!(done.data["pathScope"], "/admin");
    assert_eq!(
        resolve(&w, &sys, "https://t.x.com", "/admin", json!({}))["binding"]["pathScope"],
        "/"
    );
    assert!(cli(
        &w,
        &sys,
        "claude-aaaaaaaa",
        &["unlink", "https://none.x.com/"]
    )
    .is_err());
    // The options page acts for the user and may remove any scope.
    let removed = call(
        &w,
        &sys,
        "inspectUnbind",
        json!({"origin": "https://t.x.com", "pathScope": "/"}),
    )
    .unwrap();
    assert_eq!(removed["existed"], true);
}

#[test]
fn link_identity_root_and_config_choice_are_checked() {
    let w = world();
    let m = mono(&w, "repo");
    let shop = mkdir(&m.top.join("apps/shop"));
    let mut configs = Vec::new();
    let body = r#"{"version":1,"sites":[{"origins":["https://test-admin.x.com"]}]}"#;
    config(&mut configs, &m.top, CONFIG, body);
    config(
        &mut configs,
        &m.top,
        "apps/shop/.chrome-use/inspect.json",
        body,
    );
    config(
        &mut configs,
        &m.top,
        "apps/bad/.chrome-use/inspect.json",
        r#"{"version":3}"#,
    );
    let sys = Fake {
        installed: true,
        sessions: vec![session("claude-aaaaaaaa", &m.top)],
        tops: vec![m.top.clone()],
        configs,
        ..Fake::default()
    };
    let several = cli(
        &w,
        &sys,
        "claude-aaaaaaaa",
        &["link", "https://test-admin.x.com/"],
    )
    .err()
    .unwrap();
    assert!(several.contains("--root"), "{several}");
    assert!(several.contains(&m.admin.display().to_string()));
    assert!(several.contains(&shop.display().to_string()));
    let root = shop.display().to_string();
    let linked = cli(
        &w,
        &sys,
        "claude-aaaaaaaa",
        &["link", "https://test-admin.x.com/", "--root", &root],
    )
    .unwrap();
    assert_eq!(linked.data["binding"]["root"], json!(shop));
    assert_eq!(linked.data["binding"]["evidence"]["kind"], "config");
    assert!(linked
        .warnings
        .iter()
        .any(|w| w.replace('\\', "/").contains("apps/bad") && w.contains("unsupported version")));
    // A root in another worktree, an unknown session, or no whoami: refused.
    let other = mono(&w, "other");
    let other_root = other.admin.display().to_string();
    assert!(cli(
        &w,
        &sys,
        "claude-aaaaaaaa",
        &["link", "https://x.test/", "--root", &other_root]
    )
    .err()
    .unwrap()
    .contains("worktree"));
    assert!(
        cli(&w, &sys, "claude-99999999", &["link", "https://x.test/"])
            .err()
            .unwrap()
            .contains("roster")
    );
    sys.me.replace(None);
    assert!(Inspect {
        root: w.data.clone(),
        sys: &sys,
        profile: None,
    }
    .cli(&["link".into(), "https://x.test/".into()])
    .is_err());
    assert!(cli(
        &w,
        &sys,
        "claude-aaaaaaaa",
        &["link", "https://x.test/a", "--path-scope", "/b"]
    )
    .err()
    .unwrap()
    .contains("does not cover"));
    assert!(cli(&w, &sys, "claude-aaaaaaaa", &["link", "ftp://x.test/"]).is_err());
    assert!(cli(
        &w,
        &sys,
        "claude-aaaaaaaa",
        &["link", "https://x.test/", "--force"]
    )
    .is_err());
    let list = cli(&w, &sys, "claude-aaaaaaaa", &["list"]).unwrap();
    assert!(list.text.contains("(this session)"));
    assert!(list.text.contains("[linked]"));
}

#[test]
fn resolve_reports_config_errors_and_skipped_probes_instead_of_guessing() {
    let w = world();
    let m = mono(&w, "repo");
    let mut configs = Vec::new();
    config(
        &mut configs,
        &m.top,
        CONFIG,
        r#"{"version":1,"sites":[{"origins":["https://test-admin.x.com"]}],"root":"/etc"}"#,
    );
    let sys = Fake {
        installed: true,
        sessions: vec![session("claude-aaaaaaaa", &m.admin)],
        tops: vec![m.top.clone()],
        configs: configs.clone(),
        ..Fake::default()
    };
    let r = resolve(&w, &sys, TEST, "/", json!({}));
    assert!(r["configErrors"][0]["reason"]
        .as_str()
        .unwrap()
        .contains("unknown field"));
    assert!(r["configErrors"][0]["path"]
        .as_str()
        .unwrap()
        .contains("inspect.json"));
    assert!(r.get("skipped").is_none());

    // Three git probes time out (3 s + 3 s + the 2 s left); everything after
    // them is skipped and reported, including the config listing.
    let slow: Vec<PathBuf> = (0..4)
        .map(|i| mkdir(&base(&w).join(format!("slow{i}"))))
        .collect();
    let mut sessions = vec![session("claude-aaaaaaaa", &m.admin)];
    sessions.extend(
        slow.iter()
            .enumerate()
            .map(|(i, d)| session(&format!("claude-0000000{i}"), d)),
    );
    let sys = Fake {
        installed: true,
        sessions,
        tops: vec![m.top.clone()],
        configs,
        slow: slow.clone(),
        ..Fake::default()
    };
    let r = resolve(&w, &sys, TEST, "/", json!({}));
    let skipped: Vec<&str> = r["skipped"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s.as_str().unwrap())
        .collect();
    assert!(
        skipped.iter().any(|s| s.contains("timed out")),
        "{skipped:?}"
    );
    assert!(
        skipped.iter().any(|s| s.contains("budget spent")),
        "{skipped:?}"
    );
    assert!(
        skipped.iter().any(|s| s.starts_with("project configs")),
        "{skipped:?}"
    );
    // Nothing is related or preselected by guesswork.
    assert!(r["card"].get("preselected").is_none());
    let probes = sys
        .calls
        .borrow()
        .iter()
        .filter(|c| c.starts_with("toplevel"))
        .count();
    assert_eq!(probes, 4, "{:?}", sys.calls.borrow());
    assert!(!sys.calls.borrow().iter().any(|c| c.starts_with("ls-files")));
    for pair in r["card"]["pairs"].as_array().unwrap() {
        assert!(
            pair["sessionId"].is_null() || pair["root"] != json!(m.top),
            "{pair}"
        );
    }
}

#[test]
fn legacy_callers_get_upgrade_required_once_an_origin_is_scoped() {
    let w = world();
    let sys = Fake {
        installed: true,
        sessions: vec![session("claude-11111111", &w.project)],
        ..Fake::default()
    };
    // Extension 0.5.56: origin-level bind, status and deliver work as before.
    call(
        &w,
        &sys,
        "inspectBind",
        json!({"origin": ORIGIN, "root": w.project, "sessionId": "claude-11111111"}),
    )
    .unwrap();
    let status = call(&w, &sys, "inspectStatus", json!({"origin": ORIGIN})).unwrap();
    assert_eq!(status["binding"]["pathScope"], "/");
    assert!(status.get("upgradeRequired").is_none());
    assert_eq!(
        deliver(&w, &sys, ORIGIN, "http://localhost:5173/a", None, json!({}))["state"],
        "sent"
    );
    // A path-scoped binding appears: the old extension is told to upgrade
    // and nothing is sent.
    sys.me.replace(Some("claude-11111111".into()));
    let linked = Inspect {
        root: w.data.clone(),
        sys: &sys,
        profile: None,
    }
    .cli(&[
        "link".into(),
        "http://localhost:5173/admin".into(),
        "--path-scope".into(),
        "/admin".into(),
    ])
    .unwrap();
    assert_eq!(linked.code, 0);
    let status = call(&w, &sys, "inspectStatus", json!({"origin": ORIGIN})).unwrap();
    assert_eq!(status["upgradeRequired"], true);
    assert!(status.get("binding").is_none());
    assert_eq!(
        deliver(&w, &sys, ORIGIN, "http://localhost:5173/a", None, json!({}))["state"],
        "upgradeRequired"
    );
    assert_eq!(sys.sent.borrow().len(), 1);
    assert!(call(
        &w,
        &sys,
        "inspectBind",
        json!({"origin": ORIGIN, "root": w.project})
    )
    .unwrap_err()
    .contains("upgradeRequired"));
    // New callers with pagePath are unaffected.
    assert_eq!(
        deliver(
            &w,
            &sys,
            ORIGIN,
            "http://localhost:5173/admin/x",
            Some("/admin/x"),
            json!({})
        )["state"],
        "sent"
    );
    // A pending claim on the origin also stops legacy callers.
    let w2 = world();
    let other = mkdir(&base(&w2).join("other"));
    let two = Fake {
        installed: true,
        sessions: vec![
            session("claude-11111111", &w2.project),
            session("claude-22222222", &other),
        ],
        ..Fake::default()
    };
    call(
        &w2,
        &two,
        "inspectBind",
        json!({"origin": ORIGIN, "root": w2.project, "sessionId": "claude-11111111"}),
    )
    .unwrap();
    assert_eq!(
        cli(
            &w2,
            &two,
            "claude-22222222",
            &["link", "http://localhost:5173/"]
        )
        .unwrap()
        .code,
        2
    );
    assert_eq!(
        call(&w2, &two, "inspectStatus", json!({"origin": ORIGIN})).unwrap()["upgradeRequired"],
        true
    );
    assert_eq!(
        deliver(&w2, &two, ORIGIN, "http://localhost:5173/", None, json!({}))["state"],
        "upgradeRequired"
    );
    assert!(two.sent.borrow().is_empty());
}

#[test]
fn version_1_bindings_migrate_unchanged_in_effect() {
    let w = world();
    let sys = Fake {
        installed: true,
        sessions: vec![session("claude-11111111", &w.project)],
        ..Fake::default()
    };
    fs::create_dir_all(&w.data).unwrap();
    let v1 = json!({ORIGIN: {"origin": ORIGIN, "root": w.project, "sessionId": "claude-11111111",
        "sessionCwd": w.project, "sessionLabel": "web", "consents": {"plugin": true, "testEnv": false}, "updatedAt": 5}});
    fs::write(w.data.join("bindings.json"), v1.to_string()).unwrap();
    let list = call(&w, &sys, "inspectBindings", json!({})).unwrap();
    let b = &list["bindings"][0];
    assert_eq!(b["pathScope"], "/");
    assert_eq!(b["source"], "confirmed");
    assert_eq!(b["sourceRoots"], json!([w.project]));
    assert_eq!(b["consents"]["plugin"], true);
    assert_eq!(list["pendingClaims"], json!([]));
    // Reading never rewrites the file; the first change writes version 2.
    assert!(store_file(&w).get("version").is_none());
    assert_eq!(
        resolve(&w, &sys, ORIGIN, "/any/page", json!({}))["state"],
        "bound"
    );
    assert_eq!(
        deliver(
            &w,
            &sys,
            ORIGIN,
            "http://localhost:5173/",
            Some("/"),
            json!({})
        )["state"],
        "sent"
    );
    call(
        &w,
        &sys,
        "inspectConsent",
        json!({"origin": ORIGIN, "kind": "testEnv", "granted": true}),
    )
    .unwrap();
    let written = store_file(&w);
    assert_eq!(written["version"], 2);
    assert_eq!(
        written["bindings"][0]["consents"],
        json!({"plugin": true, "testEnv": true})
    );
    // Two scopes on one origin round-trip.
    sys.me.replace(Some("claude-11111111".into()));
    Inspect {
        root: w.data.clone(),
        sys: &sys,
        profile: None,
    }
    .cli(&[
        "link".into(),
        "http://localhost:5173/admin".into(),
        "--path-scope".into(),
        "/admin".into(),
    ])
    .unwrap();
    let list = call(&w, &sys, "inspectBindings", json!({})).unwrap();
    let mut scopes: Vec<String> = list["bindings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["pathScope"].as_str().unwrap().into())
        .collect();
    scopes.sort();
    assert_eq!(scopes, ["/", "/admin"]);
    // A store from a newer chrome-use is left alone.
    fs::write(
        w.data.join("bindings.json"),
        r#"{"version":3,"bindings":[]}"#,
    )
    .unwrap();
    assert!(call(&w, &sys, "inspectBindings", json!({}))
        .unwrap_err()
        .contains("newer"));
}

#[test]
fn tab_targets_are_revalidated_and_never_widened() {
    let w = world();
    let m = mono(&w, "repo");
    let b = mono(&w, "b");
    let mut configs = Vec::new();
    config(
        &mut configs,
        &m.top,
        CONFIG,
        r#"{"version":1,"sourceRoots":["../../packages/ui"],"sites":[{"origins":["https://test-admin.x.com"],"pathScope":"/admin"}]}"#,
    );
    let sys = Fake {
        installed: true,
        sessions: vec![
            session("claude-aaaaaaaa", &m.admin),
            session("claude-bbbbbbbb", &b.top),
        ],
        tops: vec![m.top.clone(), b.top.clone()],
        configs,
        ..Fake::default()
    };
    // The scope default is B; this tab holds A from its card.
    sys.me.replace(Some("claude-bbbbbbbb".into()));
    Inspect {
        root: w.data.clone(),
        sys: &sys,
        profile: None,
    }
    .cli(&["link".into(), "https://test-admin.x.com/".into()])
    .unwrap();
    let r = resolve(&w, &sys, TEST, "/admin/x", json!({}));
    assert_eq!(r["binding"]["sessionId"], "claude-bbbbbbbb");
    let r = resolve(&w, &sys, "https://test-admin.x.com", "/admin/x", json!({}));
    assert_eq!(r["state"], "bound");
    // Build the A pair the way an unbound card would have shown it.
    let target = |roots: Value, fp: &str| json!({"target": {"root": m.admin, "sourceRoots": roots, "sessionId": "claude-aaaaaaaa", "sessionCwd": m.admin, "fingerprint": fp}});
    let good = scope::fingerprint(
        TEST,
        "/admin",
        &Target {
            root: m.admin.clone(),
            source_roots: vec![m.admin.clone(), m.ui.clone()],
            session_id: Some("claude-aaaaaaaa".into()),
            session_cwd: Some(m.admin.clone()),
        },
    );
    let url = "https://test-admin.x.com/admin/x";
    let sent = deliver(
        &w,
        &sys,
        TEST,
        url,
        Some("/admin/x"),
        target(json!([m.admin, m.ui]), &good),
    );
    assert_eq!(sent["state"], "sent");
    assert_eq!(sys.sent.borrow()[0].0, "claude-aaaaaaaa");
    // Another tab (no target) still goes to the default.
    deliver(&w, &sys, TEST, url, Some("/admin/x"), json!({}));
    assert_eq!(sys.sent.borrow()[1].0, "claude-bbbbbbbb");
    // A wrong fingerprint, wider source roots or a page outside the card's
    // scope are refused unsent.
    assert_eq!(
        deliver(
            &w,
            &sys,
            TEST,
            url,
            Some("/admin/x"),
            target(json!([m.admin, m.ui]), &"0".repeat(32))
        )["state"],
        "targetChanged"
    );
    let wider = json!([m.admin, m.ui, m.top]);
    assert_eq!(
        deliver(&w, &sys, TEST, url, Some("/admin/x"), target(wider, &good))["state"],
        "targetChanged"
    );
    assert_eq!(
        deliver(
            &w,
            &sys,
            TEST,
            "https://test-admin.x.com/shop",
            Some("/shop"),
            target(json!([m.admin, m.ui]), &good)
        )["state"],
        "targetChanged"
    );
    let ended = Fake {
        installed: true,
        sessions: vec![session("claude-bbbbbbbb", &b.top)],
        tops: vec![m.top.clone(), b.top.clone()],
        ..Fake::default()
    };
    assert_eq!(
        deliver(
            &w,
            &ended,
            TEST,
            url,
            Some("/admin/x"),
            target(json!([m.admin, m.ui]), &good)
        )["state"],
        "targetChanged"
    );
    assert_eq!(sys.sent.borrow().len(), 2);
    // Setup requests never go to a tab target, and a target needs pagePath.
    let mut setup = json!({"origin": TEST, "kind": "setupPlugin", "pageUrl": url, "pagePath": "/admin/x", "env": "test", "deliveryId": uuid::Uuid::new_v4().to_string()});
    setup["target"] = target(json!([m.admin]), &good)["target"].clone();
    assert!(call(&w, &sys, "inspectDeliver", setup).is_err());
    let mut bare = json!({"origin": TEST, "kind": "edit", "pageUrl": url, "env": "test",
        "annotations": [{"label": "x", "selector": "x", "markdown": "x"}], "deliveryId": uuid::Uuid::new_v4().to_string()});
    bare["target"] = target(json!([m.admin]), &good)["target"].clone();
    assert!(call(&w, &sys, "inspectDeliver", bare)
        .unwrap_err()
        .contains("pagePath"));
}

#[test]
fn capabilities_advertise_resolve_and_the_new_operations_dispatch() {
    assert_eq!(super::super::capabilities()["inspectResolve"], true);
    for op in ["inspectResolve", "inspectClaimDecide"] {
        assert!(OPERATIONS.contains(&op));
    }
    let w = world();
    let sys = Fake::default();
    assert!(call(
        &w,
        &sys,
        "inspectResolve",
        json!({"origin": ORIGIN, "pagePath": "/", "extra": 1})
    )
    .is_err());
    assert!(call(
        &w,
        &sys,
        "inspectResolve",
        json!({"origin": ORIGIN, "pagePath": "/a?b"})
    )
    .is_err());
    assert!(call(
        &w,
        &sys,
        "inspectResolve",
        json!({"origin": ORIGIN, "pagePath": "/", "port": 9999})
    )
    .is_err());
    let r = call(
        &w,
        &sys,
        "inspectResolve",
        json!({"origin": ORIGIN, "pagePath": "/", "port": 5173}),
    )
    .unwrap();
    assert_eq!(r["state"], "unbound");
    assert_eq!(r["ocs"]["installed"], false);
    assert!(call(
        &w,
        &sys,
        "inspectClaimDecide",
        json!({"claimId": "x", "expectedAffects": [], "accept": true})
    )
    .is_err());
}

#[test]
fn delivery_goes_only_to_the_target_the_panel_showed() {
    let w = world();
    let a = mono(&w, "a");
    let b = mono(&w, "b");
    let c = mono(&w, "c");
    let sys = Fake {
        installed: true,
        sessions: vec![
            session("claude-aaaaaaaa", &a.top),
            session("claude-bbbbbbbb", &b.top),
            session("claude-cccccccc", &c.top),
        ],
        tops: vec![a.top.clone(), b.top.clone(), c.top.clone()],
        ..Fake::default()
    };
    let o = "https://t.x.com";
    let url = "https://t.x.com/admin/x";
    let send = |expected: &str, extra: Value| {
        let mut e = json!({"expectedTarget": expected});
        for (k, v) in extra.as_object().unwrap() {
            e[k] = v.clone();
        }
        deliver(&w, &sys, o, url, Some("/admin/x"), e)
    };
    let changed = json!({"state": "targetChanged", "reason": "bindingChanged", "schemaVersion": 1});
    cli(&w, &sys, "claude-aaaaaaaa", &["link", "https://t.x.com/"]).unwrap();
    let shown = resolve(&w, &sys, o, "/admin/x/", json!({}));
    assert_eq!(shown["pagePath"], "/admin/x");
    let shown_a = shown["binding"]["fingerprint"]
        .as_str()
        .unwrap()
        .to_string();

    // The binding is replaced by an accepted claim after the panel resolved.
    cli(&w, &sys, "claude-bbbbbbbb", &["link", "https://t.x.com/"]).unwrap();
    let claim = resolve(&w, &sys, o, "/", json!({}))["pendingClaims"][0].clone();
    call(&w, &sys, "inspectClaimDecide", json!({"claimId": claim["claimId"], "expectedAffects": claim["expectedAffects"], "accept": true})).unwrap();
    assert_eq!(send(&shown_a, json!({})), changed);
    // Setup requests are held to the same check, before consent.
    let mut setup = json!({"origin": o, "kind": "setupPlugin", "pageUrl": url, "pagePath": "/admin/x",
        "env": "test", "expectedTarget": shown_a, "deliveryId": uuid::Uuid::new_v4().to_string()});
    assert_eq!(
        call(&w, &sys, "inspectDeliver", setup.clone()).unwrap(),
        changed
    );
    let shown_b = resolve(&w, &sys, o, "/admin/x", json!({}))["binding"]["fingerprint"]
        .as_str()
        .unwrap()
        .to_string();
    setup["expectedTarget"] = shown_b.clone().into();
    assert!(call(&w, &sys, "inspectDeliver", setup)
        .unwrap_err()
        .contains("consented"));

    // Unlinked and relinked by another session.
    cli(&w, &sys, "claude-bbbbbbbb", &["unlink", "https://t.x.com/"]).unwrap();
    cli(&w, &sys, "claude-cccccccc", &["link", "https://t.x.com/"]).unwrap();
    assert_eq!(send(&shown_b, json!({})), changed);

    // A more specific scope added for the same session still changes what
    // this page resolves to.
    let shown_c = resolve(&w, &sys, o, "/admin/x", json!({}))["binding"]["fingerprint"]
        .as_str()
        .unwrap()
        .to_string();
    cli(
        &w,
        &sys,
        "claude-cccccccc",
        &["link", url, "--path-scope", "/admin"],
    )
    .unwrap();
    assert_eq!(send(&shown_c, json!({})), changed);
    assert!(sys.sent.borrow().is_empty());
    let current = resolve(&w, &sys, o, "/admin/x", json!({}))["binding"]["fingerprint"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(send(&current, json!({}))["state"], "sent");
    assert_eq!(sys.sent.borrow()[0].0, "claude-cccccccc");

    // A tab target is checked against its own fingerprint: a changed default
    // does not affect it, and quoting the default for it is refused.
    let tab = scope::fingerprint(
        o,
        "/",
        &Target {
            root: a.top.clone(),
            source_roots: vec![a.top.clone()],
            session_id: Some("claude-aaaaaaaa".into()),
            session_cwd: Some(a.top.clone()),
        },
    );
    let target = json!({"target": {"root": a.top, "sourceRoots": [a.top], "sessionId": "claude-aaaaaaaa", "sessionCwd": a.top, "fingerprint": tab}});
    cli(&w, &sys, "claude-cccccccc", &["unlink", url]).unwrap();
    assert_eq!(send(&tab, target.clone())["state"], "sent");
    assert_eq!(sys.sent.borrow()[1].0, "claude-aaaaaaaa");
    assert_eq!(send(&current, target), changed);
    assert_eq!(sys.sent.borrow().len(), 2);

    // pagePath without expectedTarget is refused.
    let bare = json!({"origin": o, "kind": "edit", "pageUrl": url, "pagePath": "/admin/x", "env": "test",
        "annotations": [{"label": "x", "selector": "x", "markdown": "x"}], "deliveryId": uuid::Uuid::new_v4().to_string()});
    assert!(call(&w, &sys, "inspectDeliver", bare)
        .unwrap_err()
        .contains("expectedTarget"));
}

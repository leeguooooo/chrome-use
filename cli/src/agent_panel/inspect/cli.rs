//! `chrome-use inspect link|unlink|list`: the session side of Inspect & Ask.
//!
//! A session hands a site to itself with `inspect link <url>`. The link is
//! written directly only when no overlapping scope of that origin goes to
//! another target; otherwise it becomes a pending claim the user accepts or
//! keeps in the browser. Identity comes from `ocs whoami` joined with the
//! `ocs who` roster, never from arguments.
use super::{
    canonical_dir, config, origin, resolve, scope, Binding, BindingSource, Claim, Evidence, Facts,
    Host, Inspect, Store, Target,
};
use crate::color;
use resolve::Live;
use serde_json::{json, Value};
use std::path::PathBuf;

const USAGE: &str = "usage: chrome-use inspect link <url> [--root <dir>] [--path-scope </seg>] | \
                     unlink <url> [--path-scope </seg>] | list";

/// Exit codes: 0 done, 2 pending claim written (confirm in the browser),
/// 1 refused.
pub(in crate::agent_panel) fn run(args: &[String], json_mode: bool) -> i32 {
    let outcome = match dirs::home_dir() {
        Some(home) => Inspect {
            root: home.join(".chrome-use").join("inspect"),
            sys: &Host,
            profile: None,
        }
        .cli(args),
        None => Err("Cannot locate the user's home directory".into()),
    };
    match outcome {
        Ok(done) => {
            for warning in &done.warnings {
                eprintln!("{} {warning}", color::warning_indicator());
            }
            if json_mode {
                let mut data = done.data;
                if !done.warnings.is_empty() {
                    data["warnings"] = done.warnings.clone().into();
                }
                println!("{}", json!({ "success": true, "data": data }));
            } else {
                println!("{}", done.text);
            }
            done.code
        }
        Err(error) => {
            if json_mode {
                println!(
                    "{}",
                    json!({ "success": false, "data": null, "error": error })
                );
            } else {
                eprintln!("{} {error}", color::error_indicator());
            }
            1
        }
    }
}

pub(super) struct Done {
    pub code: i32,
    pub text: String,
    pub data: Value,
    pub warnings: Vec<String>,
}

#[derive(Default)]
struct Args {
    url: Option<String>,
    root: Option<String>,
    path_scope: Option<String>,
}

fn parse_args(rest: &[String], allow_root: bool, needs_url: bool) -> Result<Args, String> {
    let mut args = Args::default();
    let mut i = 0;
    while i < rest.len() {
        let arg = rest[i].as_str();
        let value = || {
            rest.get(i + 1)
                .filter(|v| !v.starts_with("--"))
                .cloned()
                .ok_or_else(|| format!("{arg} needs a value; {USAGE}"))
        };
        match arg {
            "--root" if allow_root => {
                args.root = Some(value()?);
                i += 2;
            }
            "--path-scope" if needs_url => {
                args.path_scope = Some(value()?);
                i += 2;
            }
            flag if flag.starts_with("--") => {
                return Err(format!("Unknown option {flag}; {USAGE}"))
            }
            positional if needs_url && args.url.is_none() => {
                args.url = Some(positional.to_string());
                i += 1;
            }
            extra => return Err(format!("Unexpected argument {extra:?}; {USAGE}")),
        }
    }
    if needs_url && args.url.is_none() {
        return Err(USAGE.into());
    }
    Ok(args)
}

/// The URL's origin and normalized path. Query and fragment are ignored.
fn page_of(raw: &str) -> Result<(String, String), String> {
    let url = url::Url::parse(raw).map_err(|_| format!("Invalid URL {raw:?}"))?;
    if !["http", "https"].contains(&url.scheme()) {
        return Err("The URL must be http(s)".into());
    }
    let origin = origin(&url.origin().ascii_serialization())?;
    Ok((origin, scope::page_path(url.path())?))
}

/// `--path-scope` as typed: a trailing slash is dropped.
fn scope_flag(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim_end_matches('/');
    scope::path_scope(if trimmed.is_empty() { "/" } else { trimmed })
}

fn describe(
    origin: &str,
    path_scope: &str,
    label: Option<&str>,
    id: Option<&str>,
    root: &std::path::Path,
) -> String {
    let who = label.or(id).unwrap_or("(no session)");
    format!("{origin}{path_scope} → {who} in {}", root.display())
}

impl Inspect<'_> {
    pub(super) fn cli(&self, args: &[String]) -> Result<Done, String> {
        let sub = args.first().map(String::as_str).unwrap_or_default();
        let rest = args.get(1..).unwrap_or_default();
        match sub {
            "link" => self.link(parse_args(rest, true, true)?),
            "unlink" => self.unlink(parse_args(rest, false, true)?),
            "list" => {
                parse_args(rest, false, false)?;
                self.list()
            }
            _ => Err(USAGE.into()),
        }
    }

    /// The calling session: `ocs whoami` names it, the roster gives its cwd.
    fn me(&self) -> Result<Live, String> {
        let ocs = self
            .sys
            .ocs()
            .ok_or("ocs is not installed; inspect link needs it to know which session is asking")?;
        let id = self.sys.whoami(&ocs.path)?;
        let roster = self.sys.roster(&ocs.path)?;
        let session = roster.into_iter().find(|s| s.id == id).ok_or_else(|| {
            format!("This session ({id}) is not in the ocs roster with an absolute working directory; nothing was changed")
        })?;
        let cwd = canonical_dir(&session.cwd)
            .ok_or("This session's working directory no longer exists; nothing was changed")?;
        Ok(Live { session, cwd })
    }

    fn link(&self, args: Args) -> Result<Done, String> {
        let (origin, page) = page_of(args.url.as_deref().unwrap_or_default())?;
        let flag_scope = args.path_scope.as_deref().map(scope_flag).transpose()?;
        let me = self.me()?;
        let facts = Facts::new(self.sys);
        let mut warnings = Vec::new();
        let discovered = match facts.worktree(&me.cwd) {
            super::facts::Worktree::Git(top) => resolve::discover(&facts, &[top]),
            _ => resolve::discover(&facts, &[]),
        };
        for e in &discovered.errors {
            warnings.push(format!(
                "Ignored project config {}: {}",
                e["path"].as_str().unwrap_or_default(),
                e["reason"].as_str().unwrap_or_default()
            ));
        }
        let matching: Vec<(&config::Config, config::SiteMatch)> = discovered
            .configs
            .iter()
            .filter_map(|c| c.site_for(&origin, &page).map(|m| (c, m)))
            .collect();
        let root = match &args.root {
            Some(raw) => {
                // Relative to where the command runs, like any path argument.
                let path = std::path::Path::new(raw);
                let full = if path.is_absolute() {
                    path.to_path_buf()
                } else {
                    std::env::current_dir()
                        .map_err(|_| "Cannot read the current directory")?
                        .join(path)
                };
                resolve::absolute_dir(&full.to_string_lossy(), "--root")?
            }
            None => {
                let mut roots: Vec<&PathBuf> = matching.iter().map(|(c, _)| &c.app_root).collect();
                roots.sort();
                roots.dedup();
                match roots.as_slice() {
                    [] => me.cwd.clone(),
                    [one] => (*one).clone(),
                    many => {
                        let list: Vec<String> =
                            many.iter().map(|r| format!("  {}", r.display())).collect();
                        return Err(format!(
                            "Several project configs in this worktree match {origin}{page}; choose one with --root:\n{}",
                            list.join("\n")
                        ));
                    }
                }
            }
        };
        let chosen = matching
            .iter()
            .filter(|(c, _)| c.app_root == root)
            .max_by_key(|(_, m)| (m.path_scope.len(), !m.wildcard));
        let path_scope = match (&flag_scope, chosen) {
            (Some(s), _) => s.clone(),
            (None, Some((_, m))) => m.path_scope.clone(),
            (None, None) => "/".into(),
        };
        if !scope::applies(&path_scope, &page) {
            return Err(format!(
                "--path-scope {path_scope} does not cover the URL path {page}"
            ));
        }
        if !facts.related(&me.cwd, &root) {
            return Err(format!(
                "This session works in {}, which is not in the same worktree as {} with one containing the other; nothing was changed",
                me.cwd.display(),
                root.display()
            ));
        }
        let source_roots =
            chosen.map_or_else(|| vec![root.clone()], |(c, _)| c.source_roots.clone());
        let mut evidence = chosen.map(|(c, m)| Evidence::Config {
            path: c.path.clone(),
            wildcard: m.wildcard,
        });
        if let Some(port) = resolve::loopback_port(&origin) {
            if let Some(owner) = facts.port_owner(port) {
                if facts.related(&owner, &root) {
                    evidence = Some(Evidence::Port { port, cwd: owner });
                }
            }
        }
        for skipped in facts.skipped() {
            warnings.push(format!("Not checked: {skipped}"));
        }
        let target = Target {
            root: root.clone(),
            source_roots: source_roots.clone(),
            session_id: Some(me.session.id.clone()),
            session_cwd: Some(me.cwd.clone()),
        };
        let label = me.session.name.clone();
        let now = super::super::conversation::now();
        enum Linked {
            Direct(Binding),
            Pending(Claim, Vec<Binding>),
        }
        let linked = self.locked(|| {
            let mut store: Store = self.load()?;
            let conflicts: Vec<Binding> = store
                .conflicting(&origin, &path_scope, &target)
                .into_iter()
                .cloned()
                .collect();
            let result = if conflicts.is_empty() {
                // Nothing else to take over: this session's own claim on the
                // scope is settled by the link itself.
                store.pending_claims.retain(|c| {
                    !(c.origin == origin
                        && c.path_scope == path_scope
                        && c.session_id == me.session.id)
                });
                Linked::Direct(store.put(Binding {
                    origin: origin.clone(),
                    path_scope: path_scope.clone(),
                    root: root.clone(),
                    source_roots: source_roots.clone(),
                    session_id: Some(me.session.id.clone()),
                    session_cwd: Some(me.cwd.clone()),
                    session_label: label.clone(),
                    source: BindingSource::Linked,
                    evidence: evidence.clone(),
                    consents: Default::default(),
                    updated_at: now,
                }))
            } else {
                let mut affects: Vec<String> = conflicts.iter().map(Binding::fingerprint).collect();
                affects.sort();
                let claim = Claim {
                    claim_id: super::new_claim_id()?,
                    origin: origin.clone(),
                    path_scope: path_scope.clone(),
                    root: root.clone(),
                    source_roots: source_roots.clone(),
                    session_id: me.session.id.clone(),
                    session_cwd: me.cwd.clone(),
                    session_label: label.clone(),
                    evidence: evidence.clone(),
                    affects,
                    requested_at: now,
                };
                // A newer claim on the same scope supersedes the older one.
                store
                    .pending_claims
                    .retain(|c| !(c.origin == origin && c.path_scope == path_scope));
                store.pending_claims.push(claim.clone());
                Linked::Pending(claim, conflicts)
            };
            self.persist(&store)?;
            Ok(result)
        })?;
        Ok(match linked {
            Linked::Direct(binding) => Done {
                code: 0,
                text: format!(
                    "{} Linked {}",
                    color::success_indicator(),
                    describe(
                        &origin,
                        &path_scope,
                        label.as_deref(),
                        Some(&me.session.id),
                        &root
                    )
                ),
                data: json!({"state": "linked", "binding": binding.view()}),
                warnings,
            },
            Linked::Pending(claim, conflicts) => {
                let mut text = format!(
                    "{} Pending: {}{} is handed to another target. Nothing changes until the user accepts this in the browser (Inspect & Ask frame on that page).\nIt would take over:",
                    color::warning_indicator(),
                    origin,
                    path_scope
                );
                for b in &conflicts {
                    text.push_str(&format!(
                        "\n  {}",
                        describe(
                            &b.origin,
                            &b.path_scope,
                            b.session_label.as_deref(),
                            b.session_id.as_deref(),
                            &b.root
                        )
                    ));
                }
                if conflicts.iter().any(|b| b.path_scope != path_scope) {
                    text.push_str(&format!(
                        "\nAccepting moves {path_scope} only; the other scopes above stay as they are unless the user replaces them too."
                    ));
                }
                Done {
                    code: 2,
                    text,
                    data: json!({
                        "state": "pending",
                        "claim": serde_json::to_value(&claim).unwrap_or_default(),
                        "affects": conflicts.iter().map(Binding::view).collect::<Vec<_>>(),
                    }),
                    warnings,
                }
            }
        })
    }

    fn unlink(&self, args: Args) -> Result<Done, String> {
        let (origin, page) = page_of(args.url.as_deref().unwrap_or_default())?;
        let flag_scope = args.path_scope.as_deref().map(scope_flag).transpose()?;
        let me = self.me()?;
        let id = me.session.id.clone();
        let (path_scope, removed, withdrawn) = self.locked(|| {
            let mut store = self.load()?;
            let path_scope = match &flag_scope {
                Some(s) => s.clone(),
                None => store
                    .bindings
                    .iter()
                    .filter(|b| b.origin == origin)
                    .map(|b| &b.path_scope)
                    .chain(
                        store
                            .pending_claims
                            .iter()
                            .filter(|c| c.origin == origin && c.session_id == id)
                            .map(|c| &c.path_scope),
                    )
                    .filter(|s| scope::applies(s, &page))
                    .max_by_key(|s| s.len())
                    .cloned()
                    .ok_or_else(|| format!("Nothing is linked for {origin}{page}"))?,
            };
            let bound = store.binding(&origin, &path_scope).cloned();
            let mine = bound
                .as_ref()
                .is_some_and(|b| b.session_id.as_deref() == Some(id.as_str()));
            let before = store.pending_claims.len();
            store.pending_claims.retain(|c| {
                !(c.origin == origin && c.path_scope == path_scope && c.session_id == id)
            });
            let withdrawn = before - store.pending_claims.len();
            if mine {
                store
                    .bindings
                    .retain(|b| !(b.origin == origin && b.path_scope == path_scope));
            } else if withdrawn == 0 {
                return Err(match &bound {
                    Some(b) => format!(
                        "{origin}{path_scope} is bound to another session ({}); only that session or the extension options page can remove it",
                        b.session_label.as_deref().or(b.session_id.as_deref()).unwrap_or("none")
                    ),
                    None => format!("Nothing is linked for {origin}{path_scope}"),
                });
            }
            self.persist(&store)?;
            Ok((path_scope, mine, withdrawn))
        })?;
        let what = format!("{origin}{path_scope}");
        let text = match (removed, withdrawn > 0) {
            (true, false) => format!("Unlinked {what}"),
            (true, true) => {
                format!("Unlinked {what} and withdrew this session's pending claim on it")
            }
            _ => format!(
                "Withdrew this session's pending claim on {what}; its current binding is unchanged"
            ),
        };
        Ok(Done {
            code: 0,
            text: format!("{} {text}", color::success_indicator()),
            data: json!({
                "origin": origin,
                "pathScope": path_scope,
                "unlinked": removed,
                "withdrawnClaims": withdrawn,
            }),
            warnings: Vec::new(),
        })
    }

    fn list(&self) -> Result<Done, String> {
        let store = self.load()?;
        // Marking this session's entries is a convenience: listing works
        // outside a session too.
        let me = self.me().ok().map(|l| l.session.id);
        let mut lines = Vec::new();
        if store.bindings.is_empty() && store.pending_claims.is_empty() {
            lines.push("No Inspect & Ask bindings.".to_string());
        }
        let mut bindings: Vec<&Binding> = store.bindings.iter().collect();
        bindings.sort_by(|a, b| (&a.origin, &a.path_scope).cmp(&(&b.origin, &b.path_scope)));
        for b in bindings {
            let source = match b.source {
                BindingSource::Confirmed => "confirmed",
                BindingSource::Linked => "linked",
            };
            let mine = me.is_some() && b.session_id == me;
            lines.push(format!(
                "{} [{source}]{}",
                describe(
                    &b.origin,
                    &b.path_scope,
                    b.session_label.as_deref(),
                    b.session_id.as_deref(),
                    &b.root
                ),
                if mine { " (this session)" } else { "" }
            ));
        }
        for c in &store.pending_claims {
            let takes = store
                .conflicting(&c.origin, &c.path_scope, &c.target())
                .len();
            lines.push(format!(
                "pending {} (claim {}, would take over {takes}){}",
                describe(
                    &c.origin,
                    &c.path_scope,
                    c.session_label.as_deref(),
                    Some(&c.session_id),
                    &c.root
                ),
                &c.claim_id[..8],
                if me.as_deref() == Some(c.session_id.as_str()) {
                    " (this session)"
                } else {
                    ""
                }
            ));
        }
        Ok(Done {
            code: 0,
            text: lines.join("\n"),
            data: json!({
                "bindings": store.bindings.iter().map(Binding::view).collect::<Vec<_>>(),
                "pendingClaims": store.pending_claims.iter().map(|c| store.claim_view(c)).collect::<Vec<_>>(),
            }),
            warnings: Vec::new(),
        })
    }
}

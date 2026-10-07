//! `chrome-use report` — turn the local friction log into a GitHub issue.
//!
//! The agents driving chrome-use are the ones who see it fail, and they almost
//! never told anyone: filing was many steps, the privacy cost was unclear, and
//! nothing said whether they were allowed to. This module makes the draft one
//! command, redacts it, finds an existing issue for the same failure, and files
//! (or +1s) it only once the user has said yes.
//!
//! - `report` prints a redacted markdown draft and where it would go. Read-only
//!   apart from an issue search.
//! - `report --submit` files it: `gh` when authenticated, else the user's
//!   logged-in Chrome (`site github/issue-create`), else a prefilled
//!   new-issue URL to open. Refused unless `--yes`, `AGENT_BROWSER_REPORT_AUTO=1`
//!   or `report.auto` in `~/.chrome-use/config.json`.

use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use regex_lite::Regex;
use serde_json::{json, Value};

use crate::friction::{categorize, read_records, signature_id, signature_key};

pub const REPO: &str = "leeguooooo/chrome-use";
const ISSUES_URL: &str = "https://github.com/leeguooooo/chrome-use/issues";
const ISSUES_NEW_URL: &str = "https://github.com/leeguooooo/chrome-use/issues/new";
const LABEL: &str = "from-agent";
/// GitHub answers 414 somewhere past 8 KB of URL; stay clear of it.
const MAX_PREFILL_URL: usize = 7_500;
/// Entries drafted from when `--last` is not given.
const DEFAULT_LAST: usize = 20;
/// "This session" means its entries from the last few hours: session names get
/// reused, and last week's run of `default` is not what the agent just saw.
const SESSION_WINDOW_SECS: u64 = 6 * 3600;
const AUTO_ENV: &str = "AGENT_BROWSER_REPORT_AUTO";

/// What the report contains and what it deliberately leaves out.
///
/// Stating the boundary is the point, not politeness: a person deciding
/// whether to post this publicly cannot verify it line by line, and
/// "de-identified" on its own is a claim they have to take on faith. The last
/// line matters most — text redaction says nothing about pixels, and assuming
/// otherwise is how a token ends up in a screenshot attached to a public issue
/// (issue #275).
pub const REPORT_SCOPE: &str = "\
_Included: chrome-use and extension versions, platform, connection mode, and the failed \
commands with their error messages, redacted, and **host name only**._\n\n\
_Excluded: full URLs and query strings, page content, form input, cookies, tokens, \
credentials, emails, home-directory paths, and anything about tabs other than the ones that \
failed. The local log this is built from (`~/.chrome-use/friction.jsonl`) never records page \
content or full URLs either._\n\n\
_**Screenshots are not included, and are a separate decision.** If you attach one, nothing \
above redacts it — check the image yourself for logged-in pages, tokens in the address bar, \
and other tabs._";

// ---------------------------------------------------------------------------
// Redaction
// ---------------------------------------------------------------------------

fn re(cell: &'static OnceLock<Regex>, pat: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pat).expect("valid redaction regex"))
}

/// Commands whose errors may quote what was typed. Every quoted string in
/// their errors is treated as a value, not a selector.
const TYPED_ACTIONS: &[&str] = &[
    "fill",
    "type",
    "inserttext",
    "keyboard",
    "input_keyboard",
    "select",
    "form_fill",
    "auth_save",
    "auth_login",
];

/// Redact one piece of text bound for a public issue. Keeps what a maintainer
/// needs (command, error wording, host) and drops what a user would not want
/// published: query strings and fragments, cookies/tokens/auth headers, typed
/// values, emails, home-directory paths, and anything shaped like a secret.
pub fn redact(text: &str, action: Option<&str>) -> String {
    let home = dirs::home_dir().map(|h| h.to_string_lossy().into_owned());
    redact_with_home(text, action, home.as_deref())
}

pub fn redact_with_home(text: &str, action: Option<&str>, home: Option<&str>) -> String {
    static HOME_UNIX: OnceLock<Regex> = OnceLock::new();
    static HOME_WIN: OnceLock<Regex> = OnceLock::new();
    static URL: OnceLock<Regex> = OnceLock::new();
    static COOKIE: OnceLock<Regex> = OnceLock::new();
    static KV: OnceLock<Regex> = OnceLock::new();
    static BEARER: OnceLock<Regex> = OnceLock::new();
    static EMAIL: OnceLock<Regex> = OnceLock::new();
    static TOKEN_SHAPES: OnceLock<Regex> = OnceLock::new();
    static LONG_RUN: OnceLock<Regex> = OnceLock::new();
    static QUOTED: OnceLock<Regex> = OnceLock::new();

    let mut s = text.to_string();

    // Home directory → `~`: the exact one first (custom homes), then any
    // user's, since an error can name another account's path.
    if let Some(h) = home.filter(|h| h.len() > 1) {
        s = s.replace(h, "~");
    }
    s = re(&HOME_UNIX, r"/(?:Users|home)/[^/\s'\x22`]+")
        .replace_all(&s, "~")
        .into_owned();
    s = re(&HOME_WIN, r"(?i)[a-z]:\\Users\\[^\\\s'\x22`]+")
        .replace_all(&s, "~")
        .into_owned();

    // URLs: keep scheme/host/path, drop credentials, query and fragment.
    s = re(&URL, r"[A-Za-z][A-Za-z0-9+.\-]*://[^\s'\x22<>`)\]]+")
        .replace_all(&s, |c: &regex_lite::Captures| strip_url(&c[0]))
        .into_owned();

    // Cookie and auth headers: everything after the colon is the secret.
    s = re(
        &COOKIE,
        r"(?i)\b((?:set-)?cookie|(?:proxy-)?authorization)(\s*[:=]\s*)[^\n]*",
    )
    .replace_all(&s, "$1$2[redacted]")
    .into_owned();
    // `name: value` / `name=value` for anything credential-like.
    s = re(
        &KV,
        r"(?i)\b(x-[a-z\-]*(?:token|key|auth|secret)[a-z\-]*|api[_\-]?key|apikey|access[_\-]?token|refresh[_\-]?token|id[_\-]?token|auth[_\-]?token|token|secret|client[_\-]?secret|password|passwd|pwd|passcode|otp|session[_\-]?id|sessionid|sid|csrf[_\-]?token|xsrf[_\-]?token)(\s*[:=]\s*)('[^']*'|\x22[^\x22]*\x22|[^\s,;&]+)",
    )
    .replace_all(&s, "$1$2[redacted]")
    .into_owned();
    s = re(&BEARER, r"(?i)\b(bearer|basic)\s+[A-Za-z0-9._~+/=\-]{8,}")
        .replace_all(&s, "$1 [redacted]")
        .into_owned();

    s = re(&EMAIL, r"[A-Za-z0-9._%+\-]+@[A-Za-z0-9.\-]+\.[A-Za-z]{2,}")
        .replace_all(&s, "[email]")
        .into_owned();

    // Well-known token shapes, then any long letter+digit run.
    s = re(
        &TOKEN_SHAPES,
        r"\b(?:gh[pousr]_[A-Za-z0-9]{20,}|github_pat_[A-Za-z0-9_]{20,}|sk-[A-Za-z0-9_\-]{16,}|xox[abprs]-[A-Za-z0-9\-]{10,}|AKIA[0-9A-Z]{16}|AIza[0-9A-Za-z_\-]{30,}|eyJ[A-Za-z0-9_\-]{8,}\.[A-Za-z0-9_\-]{8,}\.[A-Za-z0-9_\-]{4,})",
    )
    .replace_all(&s, "[secret]")
    .into_owned();
    s = re(&LONG_RUN, r"[A-Za-z0-9_+=\-]{32,}")
        .replace_all(&s, |c: &regex_lite::Captures| {
            let m = &c[0];
            let letters = m.chars().any(|ch| ch.is_ascii_alphabetic());
            let digits = m.chars().any(|ch| ch.is_ascii_digit());
            if letters && digits {
                "[secret]".to_string()
            } else {
                m.to_string()
            }
        })
        .into_owned();

    // What was typed: in a typing command's error every quoted string is the
    // value (or close enough that publishing it is the wrong default).
    if action.is_some_and(|a| TYPED_ACTIONS.contains(&a)) {
        s = re(&QUOTED, r"\x22[^\x22]*\x22|'[^']*'")
            .replace_all(&s, "\"[value]\"")
            .into_owned();
    }
    s
}

fn strip_url(raw: &str) -> String {
    match url::Url::parse(raw) {
        Ok(mut u) if u.has_host() => {
            let _ = u.set_username("");
            let _ = u.set_password(None);
            u.set_query(None);
            u.set_fragment(None);
            u.to_string()
        }
        _ => raw.split(['?', '#']).next().unwrap_or_default().to_string(),
    }
}

// ---------------------------------------------------------------------------
// Selecting entries and drafting
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Scope {
    /// This session's recent entries when it has any, else everything.
    Auto,
    ThisSession,
    Any,
}

/// The entries a report is drafted from, oldest first, at most `last`.
pub fn select_records(
    records: &[Value],
    session: &str,
    scope: Scope,
    last: usize,
    now: u64,
) -> Vec<Value> {
    let in_session = |r: &&Value| {
        let s = r
            .get("session")
            .and_then(|v| v.as_str())
            .unwrap_or("default");
        let ts = r.get("ts").and_then(|v| v.as_u64()).unwrap_or(0);
        s == session && ts + SESSION_WINDOW_SECS >= now
    };
    let picked: Vec<&Value> = match scope {
        Scope::Any => records.iter().collect(),
        Scope::ThisSession => records.iter().filter(in_session).collect(),
        Scope::Auto => {
            let mine: Vec<&Value> = records.iter().filter(in_session).collect();
            if mine.is_empty() {
                records.iter().collect()
            } else {
                mine
            }
        }
    };
    let skip = picked.len().saturating_sub(last);
    picked.into_iter().skip(skip).cloned().collect()
}

pub struct Draft {
    pub title: String,
    pub body: String,
    /// `cu-sig-…` of the most frequent failure, when there is one.
    pub signature: Option<String>,
    pub signature_key: Option<String>,
    /// Words of the main failure, for a looser related-issue search.
    pub search_words: String,
    pub entries: usize,
    /// How often the main failure appears in the entries.
    pub main_count: usize,
}

fn rec_str<'a>(r: &'a Value, k: &str) -> &'a str {
    r.get(k).and_then(|v| v.as_str()).unwrap_or("")
}

/// Most frequent signature (ties go to the most recent) among the entries.
fn main_failure(entries: &[Value]) -> Option<(String, usize, &Value)> {
    let mut best: Option<(String, usize, usize)> = None; // key, count, last index
    let mut counts: std::collections::HashMap<String, (usize, usize)> = Default::default();
    for (i, r) in entries.iter().enumerate() {
        let key = signature_key(rec_str(r, "action"), rec_str(r, "error"));
        let e = counts.entry(key).or_insert((0, 0));
        e.0 += 1;
        e.1 = i;
    }
    for (k, (n, last)) in counts {
        let better = match &best {
            None => true,
            Some((bk, bn, bl)) => n > *bn || (n == *bn && (last > *bl || (last == *bl && k < *bk))),
        };
        if better {
            best = Some((k, n, last));
        }
    }
    best.map(|(k, n, i)| (k, n, &entries[i]))
}

fn table_cell(s: &str, max: usize) -> String {
    let one_line = s.replace(['\n', '\r'], " ").replace('|', "\\|");
    if one_line.chars().count() > max {
        let cut: String = one_line.chars().take(max).collect();
        format!("{cut}…")
    } else {
        one_line
    }
}

fn when(ts: u64) -> String {
    chrono::DateTime::from_timestamp(ts as i64, 0)
        .map(|d| d.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|| "?".into())
}

/// Build the issue draft. Pure over its inputs (environment block passed in)
/// so the redaction and layout are unit-testable.
pub fn build_draft(
    entries: &[Value],
    title: Option<&str>,
    note: Option<&str>,
    environment: &str,
) -> Draft {
    let note = note.map(|n| redact(n, None));
    let main = main_failure(entries);
    let (signature, signature_key, search_words, main_count, auto_title, main_block) = match &main {
        Some((key, n, rec)) => {
            let action = rec_str(rec, "action");
            let error = rec_str(rec, "error");
            let words = crate::friction::error_words(error);
            let category = categorize(error);
            let shown = if words.is_empty() {
                category.to_string()
            } else {
                words.clone()
            };
            let block = format!(
                "`{action}` failed with **{category}** ({n} of the {} entries below).\n\n```\n{}\n```",
                entries.len(),
                redact(error, Some(action)),
            );
            (
                Some(signature_id(key)),
                Some(key.clone()),
                format!("{action} {words}"),
                *n,
                format!("`{action}`: {shown}"),
                block,
            )
        }
        None => (
            None,
            None,
            String::new(),
            0,
            note.as_deref()
                .map(|n| table_cell(n, 70))
                .unwrap_or_else(|| "Report from an agent".into()),
            "_No failed commands were recorded; see the note above._".to_string(),
        ),
    };
    let title = title
        .map(|t| redact(t, None))
        .unwrap_or_else(|| format!("[agent report] {auto_title}"));

    let mut rows = String::new();
    for r in entries {
        let action = rec_str(r, "action");
        rows.push_str(&format!(
            "| {} | `{}` | {} | {} | {} |\n",
            when(r.get("ts").and_then(|v| v.as_u64()).unwrap_or(0)),
            table_cell(action, 30),
            table_cell(rec_str(r, "category"), 30),
            table_cell(
                if rec_str(r, "host").is_empty() {
                    "-"
                } else {
                    rec_str(r, "host")
                },
                40
            ),
            table_cell(&redact(rec_str(r, "error"), Some(action)), 140),
        ));
    }
    let failures = if entries.is_empty() {
        "_none recorded_".to_string()
    } else {
        format!("| when (UTC) | command | category | host | error |\n|---|---|---|---|---|\n{rows}")
    };
    let sig_line = match &signature {
        Some(id) => format!(
            "\n\nsignature: `{id}` ({})",
            table_cell(signature_key.as_deref().unwrap_or(""), 100)
        ),
        None => String::new(),
    };
    let body = format!(
        "## What the agent was trying to do\n{note}\n\n\
         ## What went wrong\n{main_block}\n\n\
         ## Recent failures ({n})\n{failures}\n\
         ## Environment\n{environment}{sig_line}\n\n\
         ---\n_Drafted by an AI agent with `chrome-use report`, with the user's OK._\n\n{scope}\n",
        note = note
            .as_deref()
            .unwrap_or("_not given (add one with `--note`)_"),
        n = entries.len(),
        scope = REPORT_SCOPE,
    );
    Draft {
        title,
        body,
        signature,
        signature_key,
        search_words,
        entries: entries.len(),
        main_count,
    }
}

/// The `+1` comment for an issue that already tracks this failure.
pub fn comment_body(draft: &Draft, environment_line: &str, note: Option<&str>) -> String {
    let mut out = format!(
        "+1, also seen on {environment_line}: {} ({} time(s) in the last {} logged failure(s)).",
        draft
            .signature_key
            .as_deref()
            .map(|k| format!("`{}`", table_cell(k, 100)))
            .unwrap_or_else(|| "this failure".into()),
        draft.main_count,
        draft.entries,
    );
    if let Some(n) = note {
        out.push_str(&format!(
            "\n\nWhat the agent was trying to do: {}",
            redact(n, None)
        ));
    }
    if let Some(sig) = &draft.signature {
        out.push_str(&format!("\n\nsignature: `{sig}`"));
    }
    out.push_str("\n\n_Posted by an AI agent with `chrome-use report`, with the user's OK._");
    out
}

// ---------------------------------------------------------------------------
// Environment
// ---------------------------------------------------------------------------

/// Version and connection facts, gathered without touching a page.
fn environment_block(session: &str) -> String {
    let mut out = format!(
        "- chrome-use: {}\n- platform: {}/{}",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH,
    );
    match crate::connect::relay_ext_version_driving() {
        Some(v) => out.push_str(&format!(
            "\n- ab-connect: {v} (bundled with this CLI: {})",
            env!("AB_CONNECT_VERSION")
        )),
        None => out.push_str(&format!(
            "\n- ab-connect: not connected (this CLI bundles {})",
            env!("AB_CONNECT_VERSION")
        )),
    }
    out.push_str(&format!(
        "\n- extension relay: {}",
        if crate::connect::relay_url().is_some() {
            "up"
        } else {
            "down"
        }
    ));
    out.push_str(&format!(
        "\n- connection mode: {}",
        connection_mode(session).unwrap_or_else(|| "unknown".into())
    ));
    out
}

/// One-line version of the environment for a `+1` comment.
fn environment_line(session: &str) -> String {
    format!(
        "chrome-use {} on {}/{} (ab-connect {}, {})",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH,
        crate::connect::relay_ext_version_driving().unwrap_or_else(|| "not connected".into()),
        connection_mode(session).unwrap_or_else(|| "mode unknown".into()),
    )
}

/// How the session last connected (`relay`, `launched(debug-port)`, …), from
/// the line `log_connect_mode` writes per connection. Only the mode word is
/// read; the line's websocket URL never leaves this function.
fn connection_mode(session: &str) -> Option<String> {
    let path = dirs::home_dir()?
        .join(".chrome-use")
        .join("connect-mode.log");
    let text = std::fs::read_to_string(path).ok()?;
    let mode_of = |line: &str| {
        line.split_whitespace()
            .find_map(|t| t.strip_prefix("mode="))
            .map(String::from)
    };
    let tag = format!("session={session} ");
    text.lines()
        .rev()
        .find(|l| l.starts_with(&tag))
        .and_then(mode_of)
        .or_else(|| text.lines().last().and_then(mode_of))
}

// ---------------------------------------------------------------------------
// Consent
// ---------------------------------------------------------------------------

/// Whether filing is approved: `--yes` (the user said so just now), the env
/// var, or `report.auto` in `~/.chrome-use/config.json` (either
/// `{"report": {"auto": true}}` or `{"report.auto": true}`).
pub fn submit_allowed(yes: bool, env_auto: Option<&str>, config: Option<&Value>) -> bool {
    if yes {
        return true;
    }
    if env_auto.is_some_and(|v| matches!(v.trim(), "1" | "true" | "yes")) {
        return true;
    }
    config.is_some_and(|c| {
        c.get("report")
            .and_then(|r| r.get("auto"))
            .and_then(|v| v.as_bool())
            == Some(true)
            || c.get("report.auto").and_then(|v| v.as_bool()) == Some(true)
    })
}

pub(crate) fn user_config() -> Option<Value> {
    let path = dirs::home_dir()?.join(".chrome-use").join("config.json");
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

pub const CONSENT_REFUSAL: &str =
    "not submitted: filing a public GitHub issue needs the user's OK. \
Show the user the draft above (`chrome-use report` prints it) and ask whether to file it. \
If they agree, run `chrome-use report --submit --yes` with the same options. \
To pre-approve future reports, the user can set AGENT_BROWSER_REPORT_AUTO=1 \
or `\"report\": {\"auto\": true}` in ~/.chrome-use/config.json.";

// ---------------------------------------------------------------------------
// Prefilled URL fallback
// ---------------------------------------------------------------------------

const TRUNCATION_NOTE: &str =
    "\n\n_(truncated to fit a URL — run `chrome-use report` for the full draft)_";

/// `…/issues/new?title=…&body=…` within `max_len`, cutting the body (at a line
/// break when one is near) and saying so. Returns the URL and whether it cut.
pub fn prefilled_issue_url(title: &str, body: &str, max_len: usize) -> (String, bool) {
    let base = format!(
        "{ISSUES_NEW_URL}?labels={LABEL}&title={}&body=",
        urlencoding::encode(title)
    );
    let full = format!("{base}{}", urlencoding::encode(body));
    if full.len() <= max_len {
        return (full, false);
    }
    let chars: Vec<char> = body.chars().collect();
    let fits = |k: usize| {
        let cut: String = chars[..k].iter().collect();
        base.len() + urlencoding::encode(&format!("{cut}{TRUNCATION_NOTE}")).len() <= max_len
    };
    // Largest prefix that fits.
    let (mut lo, mut hi) = (0usize, chars.len());
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        if fits(mid) {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    let mut k = lo;
    // Prefer ending on a whole line if that loses less than a fifth.
    if let Some(nl) = chars[..k].iter().rposition(|c| *c == '\n') {
        if nl >= k * 4 / 5 {
            k = nl;
        }
    }
    let cut: String = chars[..k].iter().collect();
    (
        format!(
            "{base}{}",
            urlencoding::encode(&format!("{cut}{TRUNCATION_NOTE}"))
        ),
        true,
    )
}

// ---------------------------------------------------------------------------
// Running gh / curl / ourselves
// ---------------------------------------------------------------------------

/// Run a command with a deadline; `Some((success, stdout, stderr))`, or `None`
/// when it could not start or ran out of time (it is killed).
fn run_with_timeout(
    program: &str,
    args: &[&str],
    stdin: Option<&str>,
    timeout: Duration,
) -> Option<(bool, String, String)> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;
    if let (Some(input), Some(mut pipe)) = (stdin, child.stdin.take()) {
        let _ = pipe.write_all(input.as_bytes());
    }
    // Drain pipes on threads so a chatty child cannot block on a full pipe.
    let mut out_pipe = child.stdout.take()?;
    let mut err_pipe = child.stderr.take()?;
    let out_t = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = std::io::Read::read_to_string(&mut out_pipe, &mut s);
        s
    });
    let err_t = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = std::io::Read::read_to_string(&mut err_pipe, &mut s);
        s
    });
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break st,
            Ok(None) if start.elapsed() > timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(_) => return None,
        }
    };
    let out = out_t.join().unwrap_or_default();
    let err = err_t.join().unwrap_or_default();
    Some((status.success(), out, err))
}

fn gh_ready() -> bool {
    matches!(
        run_with_timeout(
            "gh",
            &["auth", "status", "--hostname", "github.com"],
            None,
            Duration::from_secs(8)
        ),
        Some((true, _, _))
    )
}

#[derive(Debug, Clone, PartialEq)]
pub struct IssueHit {
    pub number: u64,
    pub title: String,
    pub url: String,
    /// The issue carries this report's signature (same command + error).
    pub exact: bool,
}

/// Sort search results into exact (carries the signature, or the same title)
/// and related, dropping pull requests.
pub fn classify_hits(items: &[Value], signature: Option<&str>, title: &str) -> Vec<IssueHit> {
    let mut hits: Vec<IssueHit> = Vec::new();
    for it in items {
        if it.get("pull_request").is_some_and(|v| !v.is_null())
            || it.get("is_pr").and_then(|v| v.as_bool()) == Some(true)
        {
            continue;
        }
        let Some(number) = it.get("number").and_then(|v| v.as_u64()) else {
            continue;
        };
        if hits.iter().any(|h| h.number == number) {
            continue;
        }
        let t = it.get("title").and_then(|v| v.as_str()).unwrap_or("");
        let body = it.get("body").and_then(|v| v.as_str()).unwrap_or("");
        let exact = signature.is_some_and(|s| body.contains(s) || t.contains(s))
            || (!title.is_empty() && t.trim() == title.trim());
        hits.push(IssueHit {
            number,
            title: t.to_string(),
            url: it
                .get("html_url")
                .or_else(|| it.get("url"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            exact,
        });
    }
    hits.sort_by_key(|h| !h.exact);
    hits
}

fn search_queries(draft: &Draft) -> Vec<String> {
    let mut q = Vec::new();
    if let Some(sig) = &draft.signature {
        q.push(format!("repo:{REPO} is:issue is:open \"{sig}\""));
    }
    let words: Vec<&str> = draft.search_words.split_whitespace().take(5).collect();
    if words.len() >= 2 {
        q.push(format!(
            "repo:{REPO} is:issue is:open in:title {}",
            words.join(" ")
        ));
    }
    // The same title (an earlier agent report, or the `--title` the agent
    // chose to match an issue it already knows) counts as the same failure.
    let title: String = draft.title.chars().filter(|c| *c != '"').collect();
    if !title.trim().is_empty() {
        q.push(format!(
            "repo:{REPO} is:issue is:open in:title \"{}\"",
            title.trim()
        ));
    }
    q
}

/// Search open issues for this failure: `gh` when authenticated, else the
/// public search API (no auth), else the `github/issues` adapter in the user's
/// Chrome. Returns the hits and which way it searched (or why it could not).
fn search_existing(draft: &Draft, gh: bool, session: &str) -> (Vec<IssueHit>, String) {
    let queries = search_queries(draft);
    if queries.is_empty() {
        return (Vec::new(), "skipped (nothing to search for)".into());
    }
    let mut items: Vec<Value> = Vec::new();
    let mut via = String::new();
    for q in &queries {
        let got = if gh {
            run_with_timeout(
                "gh",
                &[
                    "api",
                    "-X",
                    "GET",
                    "search/issues",
                    "-f",
                    &format!("q={q}"),
                    "-f",
                    "per_page=10",
                ],
                None,
                Duration::from_secs(15),
            )
            .filter(|(ok, _, _)| *ok)
            .map(|(_, out, _)| (out, "gh"))
        } else {
            None
        };
        let got = got.or_else(|| {
            let url = format!(
                "https://api.github.com/search/issues?per_page=10&q={}",
                urlencoding::encode(q)
            );
            run_with_timeout(
                "curl",
                &[
                    "-fsSL",
                    "--max-time",
                    "10",
                    "-H",
                    "Accept: application/vnd.github+json",
                    &url,
                ],
                None,
                Duration::from_secs(12),
            )
            .filter(|(ok, _, _)| *ok)
            .map(|(_, out, _)| (out, "public GitHub search API"))
        });
        if let Some((out, how)) = got {
            via = how.to_string();
            if let Ok(v) = serde_json::from_str::<Value>(&out) {
                if let Some(arr) = v.get("items").and_then(|a| a.as_array()) {
                    items.extend(arr.iter().cloned());
                }
            }
        }
    }
    if via.is_empty() {
        // Last resort: the open-issue list through the user's Chrome. No body
        // there, so only a same-title match counts as exact.
        if let Some(v) = run_self(
            &["site", "github/issues", REPO, "--json"],
            session,
            Duration::from_secs(60),
        ) {
            if let Some(arr) = find_array(&v, "issues") {
                via = "site github/issues (your Chrome)".into();
                let words: Vec<&str> = draft.search_words.split_whitespace().collect();
                items.extend(
                    arr.iter()
                        .filter(|i| {
                            let t = i
                                .get("title")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_lowercase();
                            t.trim() == draft.title.to_lowercase().trim()
                                || (words.len() >= 2 && words.iter().all(|w| t.contains(w)))
                        })
                        .cloned(),
                );
            }
        }
    }
    if via.is_empty() {
        via = "unavailable (no gh, no network, no Chrome)".into();
    }
    (
        classify_hits(&items, draft.signature.as_deref(), &draft.title),
        via,
    )
}

/// Run this same binary (a `site` adapter) on the given session; parsed JSON.
fn run_self(args: &[&str], session: &str, timeout: Duration) -> Option<Value> {
    let exe = std::env::current_exe().ok()?;
    let exe = exe.to_string_lossy().into_owned();
    let mut all: Vec<&str> = args.to_vec();
    all.extend(["--session", session]);
    let (_, out, _) = run_with_timeout(&exe, &all, None, timeout)?;
    serde_json::from_str(out.trim()).ok()
}

fn find_array<'a>(v: &'a Value, key: &str) -> Option<&'a Vec<Value>> {
    match v {
        Value::Object(m) => m
            .get(key)
            .and_then(|x| x.as_array())
            .or_else(|| m.values().find_map(|x| find_array(x, key))),
        _ => None,
    }
}

fn find_issue_url(v: &Value) -> Option<String> {
    match v {
        Value::String(s) if s.starts_with(ISSUES_URL) => Some(s.clone()),
        Value::Object(m) => m.values().find_map(find_issue_url),
        Value::Array(a) => a.iter().find_map(find_issue_url),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Filing
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub enum Plan {
    Create,
    Comment(u64),
}

/// New issue, or a `+1` on the exact match (unless `--new`).
pub fn plan(hits: &[IssueHit], force_new: bool) -> Plan {
    match hits.iter().find(|h| h.exact) {
        Some(h) if !force_new => Plan::Comment(h.number),
        _ => Plan::Create,
    }
}

struct Filed {
    via: String,
    url: Option<String>,
    /// For the URL fallback: what the agent has to open (and paste).
    manual: Option<String>,
}

fn gh_create(title: &str, body: &str) -> Result<String, String> {
    let try_once = |label: bool| {
        let mut args = vec![
            "issue",
            "create",
            "-R",
            REPO,
            "--title",
            title,
            "--body-file",
            "-",
        ];
        if label {
            args.extend(["--label", LABEL]);
        }
        run_with_timeout("gh", &args, Some(body), Duration::from_secs(30))
    };
    // Only maintainers can label; without that permission (or without the
    // label existing) gh refuses the whole create, so retry plain.
    match try_once(true) {
        Some((true, out, _)) => Ok(out.trim().to_string()),
        _ => match try_once(false) {
            Some((true, out, _)) => Ok(out.trim().to_string()),
            Some((false, _, err)) => Err(err.trim().to_string()),
            None => Err("gh timed out".into()),
        },
    }
}

fn gh_comment(number: u64, body: &str) -> Result<String, String> {
    let n = number.to_string();
    match run_with_timeout(
        "gh",
        &["issue", "comment", &n, "-R", REPO, "--body-file", "-"],
        Some(body),
        Duration::from_secs(30),
    ) {
        Some((true, out, _)) => Ok(out.trim().to_string()),
        Some((false, _, err)) => Err(err.trim().to_string()),
        None => Err("gh timed out".into()),
    }
}

fn chrome_create(title: &str, body: &str, session: &str) -> Result<String, String> {
    let v = run_self(
        &[
            "site",
            "github/issue-create",
            REPO,
            "--title",
            title,
            "--body",
            body,
            "--json",
        ],
        session,
        Duration::from_secs(90),
    )
    .ok_or("the github/issue-create adapter did not answer")?;
    find_issue_url(&v).ok_or_else(|| {
        let err = v
            .get("error")
            .and_then(|e| e.as_str())
            .or_else(|| v.pointer("/data/result/error").and_then(|e| e.as_str()))
            .unwrap_or("no issue URL in the reply");
        format!("github/issue-create: {err}")
    })
}

fn file_it(
    draft: &Draft,
    plan: &Plan,
    comment: &str,
    gh: bool,
    session: &str,
    errors: &mut Vec<String>,
) -> Filed {
    if gh {
        let r = match plan {
            Plan::Create => gh_create(&draft.title, &draft.body),
            Plan::Comment(n) => gh_comment(*n, comment),
        };
        match r {
            Ok(url) => {
                return Filed {
                    via: "gh".into(),
                    url: Some(url),
                    manual: None,
                }
            }
            Err(e) => errors.push(format!("gh: {e}")),
        }
    }
    if *plan == Plan::Create {
        match chrome_create(&draft.title, &draft.body, session) {
            Ok(url) => {
                return Filed {
                    via: "site github/issue-create (your Chrome)".into(),
                    url: Some(url),
                    manual: None,
                }
            }
            Err(e) => errors.push(e),
        }
    }
    match plan {
        Plan::Create => {
            let (url, cut) = prefilled_issue_url(&draft.title, &draft.body, MAX_PREFILL_URL);
            Filed {
                via: "prefilled URL".into(),
                url: None,
                manual: Some(format!(
                    "open this URL and press \"Submit new issue\"{}:\n{url}",
                    if cut { " (body truncated to fit)" } else { "" }
                )),
            }
        }
        Plan::Comment(n) => Filed {
            via: "manual comment".into(),
            url: None,
            manual: Some(format!(
                "open {ISSUES_URL}/{n} and post this comment:\n\n{comment}"
            )),
        },
    }
}

// ---------------------------------------------------------------------------
// The command
// ---------------------------------------------------------------------------

#[derive(Debug, Default, PartialEq)]
pub struct Opts {
    pub last: Option<usize>,
    pub title: Option<String>,
    pub note: Option<String>,
    pub submit: bool,
    pub yes: bool,
    pub new: bool,
    pub dry_run: bool,
    pub open: bool,
    pub this_session: bool,
    pub any_session: bool,
    pub no_search: bool,
}

pub fn parse_opts(args: &[String], raw_args: &[String]) -> Result<Opts, String> {
    let mut o = Opts {
        // `--new` is also a global flag, so it is gone from the cleaned args.
        new: raw_args.iter().any(|a| a == "--new"),
        ..Default::default()
    };
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        let mut value = |name: &str| -> Result<String, String> {
            i += 1;
            args.get(i)
                .cloned()
                .ok_or_else(|| format!("{name} needs a value"))
        };
        match a {
            "--last" => {
                o.last = Some(
                    value("--last")?
                        .parse()
                        .map_err(|_| "--last needs a number".to_string())?,
                )
            }
            "--title" => o.title = Some(value("--title")?),
            "--note" => o.note = Some(value("--note")?),
            "--submit" => o.submit = true,
            "--yes" | "-y" => o.yes = true,
            "--new" => o.new = true,
            "--dry-run" => o.dry_run = true,
            "--open" => o.open = true,
            "--this-session" => o.this_session = true,
            "--any-session" | "--all" => o.any_session = true,
            "--no-search" => o.no_search = true,
            other => {
                return Err(format!(
                    "unknown report option: {other} (see `chrome-use report --help`)"
                ))
            }
        }
        i += 1;
    }
    Ok(o)
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// `chrome-use report [...]` — see the module docs and `report --help`.
pub fn run_report(args: &[String], raw_args: &[String], session: &str, json_out: bool) {
    let opts = match parse_opts(args, raw_args) {
        Ok(o) => o,
        Err(e) => {
            if json_out {
                println!("{}", json!({ "success": false, "error": e }));
            } else {
                eprintln!("{e}");
            }
            std::process::exit(2);
        }
    };
    let scope = if opts.any_session {
        Scope::Any
    } else if opts.this_session {
        Scope::ThisSession
    } else {
        Scope::Auto
    };
    let records = read_records();
    let entries = select_records(
        &records,
        session,
        scope,
        opts.last.unwrap_or(DEFAULT_LAST).max(1),
        unix_now(),
    );
    if entries.is_empty() && opts.note.is_none() {
        let msg = "nothing to report: the friction log has no matching failures. \
                   To report something that did not fail (a missing feature, a misleading result), \
                   describe it with `--note \"...\"`.";
        if json_out {
            println!("{}", json!({ "success": false, "error": msg }));
        } else {
            eprintln!("{msg}");
        }
        std::process::exit(1);
    }

    let environment = environment_block(session);
    let draft = build_draft(
        &entries,
        opts.title.as_deref(),
        opts.note.as_deref(),
        &environment,
    );
    let gh = gh_ready();
    let (hits, searched_via) = if opts.no_search {
        (Vec::new(), "skipped (--no-search)".to_string())
    } else {
        search_existing(&draft, gh, session)
    };
    let the_plan = plan(&hits, opts.new);
    let comment = comment_body(&draft, &environment_line(session), opts.note.as_deref());
    let target = match &the_plan {
        Plan::Create => format!("new issue in {REPO}"),
        Plan::Comment(n) => {
            format!("+1 comment on {ISSUES_URL}/{n} (same failure; `--new` files a separate issue)")
        }
    };
    let channel = if gh {
        "gh (authenticated)"
    } else if the_plan == Plan::Create {
        "site github/issue-create in your Chrome, else a prefilled URL"
    } else {
        "a comment for you to paste"
    };
    let hits_json: Vec<Value> = hits
        .iter()
        .map(|h| json!({ "number": h.number, "title": h.title, "url": h.url, "exact": h.exact }))
        .collect();
    let (prefill, _) = prefilled_issue_url(&draft.title, &draft.body, MAX_PREFILL_URL);
    let mut data = json!({
        "repo": REPO,
        "title": draft.title,
        "markdown": draft.body,
        "signature": draft.signature,
        "signatureKey": draft.signature_key,
        "entries": draft.entries,
        "matches": hits_json,
        "searchedVia": searched_via,
        "target": target,
        "channel": channel,
        "comment": if matches!(the_plan, Plan::Comment(_)) { Value::String(comment.clone()) } else { Value::Null },
        "issueUrl": prefill,
    });

    if !json_out {
        println!("# {}\n", draft.title);
        println!("{}", draft.body);
        println!("─────────────────────────────────────────────");
        if hits.is_empty() {
            println!("existing issues: none found ({searched_via})");
        } else {
            println!("existing issues ({searched_via}):");
            for h in &hits {
                println!(
                    "  #{} {}{}\n      {}",
                    h.number,
                    h.title,
                    if h.exact { "  [same failure]" } else { "" },
                    h.url
                );
            }
        }
        println!("would go to: {target}");
        println!("via: {channel}");
        if let Plan::Comment(_) = the_plan {
            println!("\ncomment that would be posted:\n{comment}\n");
        }
    }

    if opts.open && !opts.submit {
        let opener = if cfg!(target_os = "macos") {
            "open"
        } else {
            "xdg-open"
        };
        let _ = Command::new(opener).arg(&prefill).spawn();
    }

    if !opts.submit {
        if json_out {
            println!("{}", json!({ "success": true, "data": data }));
        } else {
            println!(
                "\nnothing was sent. Show this draft to the user; with their OK: \
                 `chrome-use report --submit --yes` (same options)."
            );
        }
        return;
    }

    let config = user_config();
    let auto = std::env::var(AUTO_ENV).ok();
    if !submit_allowed(opts.yes, auto.as_deref(), config.as_ref()) {
        if json_out {
            data["consentRequired"] = json!(true);
            println!(
                "{}",
                json!({ "success": false, "error": CONSENT_REFUSAL, "code": "consent_required", "data": data })
            );
        } else {
            eprintln!("\n{CONSENT_REFUSAL}");
        }
        std::process::exit(1);
    }

    if opts.dry_run {
        if json_out {
            data["dryRun"] = json!(true);
            println!("{}", json!({ "success": true, "data": data }));
        } else {
            println!(
                "\n--dry-run: consent ok; would file as {target} via {channel}. Nothing was sent."
            );
        }
        return;
    }

    let mut errors = Vec::new();
    let filed = file_it(&draft, &the_plan, &comment, gh, session, &mut errors);
    data["submittedVia"] = json!(filed.via);
    data["submittedUrl"] = json!(filed.url);
    if !errors.is_empty() {
        data["fallbackReasons"] = json!(errors);
    }
    if json_out {
        if let Some(m) = &filed.manual {
            data["manualStep"] = json!(m);
        }
        println!("{}", json!({ "success": true, "data": data }));
        return;
    }
    for e in &errors {
        eprintln!("  ({e})");
    }
    match (&filed.url, &filed.manual) {
        (Some(url), _) => println!("\n✓ filed via {}: {url}", filed.via),
        (None, Some(m)) => println!("\ncould not file automatically; {m}"),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOME: &str = "/Users/alice";

    fn red(s: &str) -> String {
        redact_with_home(s, None, Some(HOME))
    }

    #[test]
    fn the_report_states_what_it_leaves_out() {
        for excluded in [
            "full URLs",
            "cookies",
            "tokens",
            "credentials",
            "form input",
            "emails",
        ] {
            assert!(REPORT_SCOPE.contains(excluded), "missing: {excluded}");
        }
        assert!(REPORT_SCOPE.contains("Screenshots are not included"));
        assert!(
            REPORT_SCOPE.contains("nothing \nabove redacts it")
                || REPORT_SCOPE.contains("nothing above redacts it")
        );
    }

    #[test]
    fn the_environment_block_carries_the_extension_and_relay_state() {
        let env = environment_block("default");
        for k in [
            "chrome-use:",
            "ab-connect:",
            "extension relay:",
            "connection mode:",
        ] {
            assert!(env.contains(k), "{env}");
        }
    }

    #[test]
    fn redaction_strips_query_strings_and_fragments_but_keeps_hosts() {
        let out = red(
            "Navigation failed: https://shop.example.com/cart/42?session=abc&q=private#frag stop",
        );
        assert!(out.contains("https://shop.example.com/cart/42"), "{out}");
        assert!(!out.contains("session=abc"), "{out}");
        assert!(!out.contains("private"), "{out}");
        assert!(!out.contains("#frag"), "{out}");
        assert!(out.ends_with(" stop"), "{out}");
        let out = red("open https://user:pw@intra.example.org/x");
        assert!(!out.contains("user:pw"), "{out}");
        assert!(out.contains("intra.example.org"), "{out}");
    }

    #[test]
    fn redaction_removes_cookies_tokens_and_headers() {
        let out = red("Cookie: sid=123abc; theme=dark\nnext line");
        assert!(
            !out.contains("123abc") && !out.contains("theme=dark"),
            "{out}"
        );
        assert!(out.contains("next line"), "{out}");
        let out = red("Authorization: Bearer abcdefghijklmnop failed");
        assert!(!out.contains("abcdefghijklmnop"), "{out}");
        let out = red("x-api-key=SECRETVALUE token: hunter2 password=\"p a s s\"");
        for leaked in ["SECRETVALUE", "hunter2", "p a s s"] {
            assert!(!out.contains(leaked), "{leaked} in {out}");
        }
        for tok in [
            "ghp_abcdefghijklmnopqrstuvwxyz0123",
            "sk-abcdefghijklmnop1234",
            "xoxb-1234567890-abcdef",
            "AKIAABCDEFGHIJKLMNOP",
            "eyJhbGciOiJIUzI1.eyJzdWIiOiIxMjM0.SflKxwRJSMeKKF2QT4",
            "a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8",
        ] {
            let out = red(&format!("got {tok} here"));
            assert!(!out.contains(tok), "{tok} survived: {out}");
            assert!(
                out.contains("[secret]") || out.contains("[redacted]"),
                "{out}"
            );
        }
        // Ordinary words and selectors survive.
        let out = red("Element not found: #submit-button (stale sessionId cb-tab-1)");
        assert_eq!(
            out,
            "Element not found: #submit-button (stale sessionId cb-tab-1)"
        );
    }

    #[test]
    fn redaction_removes_emails_home_paths_and_typed_values() {
        let out = red("login as bob.smith+x@corp.example.com failed");
        assert!(!out.contains("bob.smith"), "{out}");
        assert!(out.contains("[email]"), "{out}");
        let out = red("could not read /Users/alice/Documents/tax.pdf or /home/bob/x");
        assert!(out.contains("~/Documents/tax.pdf"), "{out}");
        assert!(!out.contains("alice") && !out.contains("bob"), "{out}");
        let out = redact_with_home(
            "fill \"#password\" with \"hunter2!\" timed out",
            Some("fill"),
            Some(HOME),
        );
        assert!(!out.contains("hunter2"), "{out}");
        // Not a typing command: quoted selectors are kept.
        let out = redact_with_home("Element \"#go\" not visible", Some("click"), Some(HOME));
        assert!(out.contains("#go"), "{out}");
    }

    fn rec(ts: u64, session: &str, action: &str, error: &str) -> Value {
        json!({ "ts": ts, "session": session, "action": action, "error": error,
                "category": categorize(error), "host": "example.com" })
    }

    #[test]
    fn entries_default_to_this_sessions_recent_failures() {
        let now = 1_000_000;
        let recs = vec![
            rec(now - 100_000, "work", "click", "old"),
            rec(now - 50, "other", "click", "x"),
            rec(now - 40, "work", "fill", "Operation timed out"),
            rec(now - 30, "work", "click", "Element not found: #a"),
        ];
        let got = select_records(&recs, "work", Scope::Auto, 20, now);
        assert_eq!(got.len(), 2, "{got:?}");
        assert_eq!(select_records(&recs, "work", Scope::Any, 20, now).len(), 4);
        assert_eq!(select_records(&recs, "work", Scope::Any, 2, now).len(), 2);
        // A session with nothing recent falls back to everything (Auto) …
        assert_eq!(select_records(&recs, "nope", Scope::Auto, 20, now).len(), 4);
        // … but not when the caller asked for this session only.
        assert!(select_records(&recs, "nope", Scope::ThisSession, 20, now).is_empty());
    }

    #[test]
    fn the_draft_leads_with_the_most_frequent_failure_and_is_redacted() {
        let now = 1_700_000_000;
        let entries = vec![
            rec(
                now,
                "fill",
                "fill",
                "Timed out typing \"hunter2\" into https://a.example.com/login?next=/secret",
            ),
            rec(now + 1, "s", "click", "Element not found: #a"),
            rec(now + 2, "s", "click", "Element not found: #b"),
        ];
        let d = build_draft(
            &entries,
            None,
            Some("Checkout at a.example.com as me@x.com"),
            "- env",
        );
        assert_eq!(d.signature.as_deref(), Some("cu-sig-2b1cedf4"));
        assert_eq!(d.main_count, 2);
        assert!(
            d.title.contains("`click`: element not found"),
            "{}",
            d.title
        );
        assert!(d.body.contains("cu-sig-2b1cedf4"));
        assert!(d.body.contains("a.example.com"));
        for leaked in ["hunter2", "next=/secret", "me@x.com"] {
            assert!(!d.body.contains(leaked), "{leaked} leaked");
        }
        let c = comment_body(&d, "chrome-use 1.0 on macos/aarch64", None);
        assert!(c.starts_with("+1, also seen on chrome-use 1.0"), "{c}");
        assert!(c.contains("cu-sig-2b1cedf4"));
        // A note alone (missing feature) still drafts, without a signature.
        let d = build_draft(&[], None, Some("no way to drag between frames"), "- env");
        assert!(d.signature.is_none());
        assert!(d.title.contains("no way to drag"));
    }

    #[test]
    fn dedup_comments_on_an_exact_match_and_only_lists_related_ones() {
        let items = vec![
            json!({ "number": 7, "title": "click is flaky", "html_url": "u7", "body": "nothing" }),
            json!({ "number": 9, "title": "x", "html_url": "u9", "body": "signature: `cu-sig-2b1cedf4`" }),
            json!({ "number": 9, "title": "x", "html_url": "u9", "body": "dup from 2nd query" }),
            json!({ "number": 11, "title": "a PR", "html_url": "u11", "body": "cu-sig-2b1cedf4", "pull_request": {} }),
        ];
        let hits = classify_hits(&items, Some("cu-sig-2b1cedf4"), "[agent report] t");
        assert_eq!(hits.len(), 2, "{hits:?}");
        assert_eq!(hits[0].number, 9);
        assert!(hits[0].exact);
        assert!(!hits[1].exact);
        assert_eq!(plan(&hits, false), Plan::Comment(9));
        assert_eq!(
            plan(&hits, true),
            Plan::Create,
            "--new files a separate issue"
        );
        assert_eq!(
            plan(&hits[1..], false),
            Plan::Create,
            "related is not the same failure"
        );
        // The site adapter has no body: a same title is the exact match there.
        let hits = classify_hits(
            &[json!({ "number": 3, "title": "[agent report] t", "url": "u3" })],
            None,
            "[agent report] t",
        );
        assert!(hits[0].exact);
    }

    #[test]
    fn submitting_needs_the_users_ok() {
        assert!(!submit_allowed(false, None, None));
        assert!(!submit_allowed(
            false,
            Some("0"),
            Some(&json!({ "report": { "auto": false } }))
        ));
        assert!(submit_allowed(true, None, None));
        assert!(submit_allowed(false, Some("1"), None));
        assert!(submit_allowed(
            false,
            None,
            Some(&json!({ "report": { "auto": true } }))
        ));
        assert!(submit_allowed(
            false,
            None,
            Some(&json!({ "report.auto": true }))
        ));
        assert!(CONSENT_REFUSAL.contains("Show the user the draft"));
        assert!(CONSENT_REFUSAL.contains("--submit --yes"));
    }

    #[test]
    fn the_prefilled_url_fits_and_says_it_was_cut() {
        let (u, cut) = prefilled_issue_url("t", "short body", MAX_PREFILL_URL);
        assert!(!cut);
        assert!(u.starts_with(ISSUES_NEW_URL));
        assert!(u.contains("labels=from-agent"));
        assert!(u.contains("body=short%20body"));

        let body: String = (0..2000)
            .map(|i| format!("line {i} with ünïcode\n"))
            .collect();
        let (u, cut) = prefilled_issue_url("title", &body, MAX_PREFILL_URL);
        assert!(cut);
        assert!(u.len() <= MAX_PREFILL_URL, "{}", u.len());
        assert!(u.len() > MAX_PREFILL_URL - 400, "cut too much: {}", u.len());
        let decoded = urlencoding::decode(u.split("&body=").nth(1).unwrap()).unwrap();
        assert!(decoded.starts_with("line 0 with ünïcode"));
        assert!(decoded.ends_with(TRUNCATION_NOTE), "{decoded}");
        // Cut on a whole line.
        assert!(decoded
            .trim_end_matches(TRUNCATION_NOTE)
            .ends_with("ünïcode"));
    }

    #[test]
    fn options_parse_and_reject_unknowns() {
        let a = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let o = parse_opts(
            &a(&["--last", "5", "--note", "n", "--submit", "--yes"]),
            &a(&["report", "--new"]),
        )
        .unwrap();
        assert_eq!(o.last, Some(5));
        assert_eq!(o.note.as_deref(), Some("n"));
        assert!(o.submit && o.yes && o.new);
        assert!(parse_opts(&a(&["--bogus"]), &[]).is_err());
        assert!(parse_opts(&a(&["--last"]), &[]).is_err());
    }

    #[test]
    fn the_search_looks_for_the_signature_the_words_and_the_title() {
        let entries = vec![
            rec(1, "s", "click", "Element not found: #a"),
            rec(2, "s", "click", "Element not found: #b"),
        ];
        let d = build_draft(&entries, Some("My \"quoted\" title"), None, "- env");
        let q = search_queries(&d);
        assert_eq!(q.len(), 3, "{q:?}");
        assert!(q[0].ends_with("\"cu-sig-2b1cedf4\""), "{}", q[0]);
        assert!(
            q[1].ends_with("in:title click element not found"),
            "{}",
            q[1]
        );
        assert!(q[2].ends_with("in:title \"My quoted title\""), "{}", q[2]);
        assert!(q
            .iter()
            .all(|x| x.starts_with("repo:leeguooooo/chrome-use is:issue is:open")));
    }
}

use futures_util::StreamExt;
use reqwest::header::{ACCEPT, CONTENT_TYPE, USER_AGENT};
use reqwest::Client;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::time::Duration;
use url::Url;

const DEFAULT_TIMEOUT_MS: u64 = 10_000;
const BODY_LIMIT: usize = 2 * 1024 * 1024;
const READ_ACCEPT: &str = "text/markdown, text/plain;q=0.9, text/html;q=0.7, */*;q=0.1";
const USER_AGENT_VALUE: &str = concat!("agent-browser/", env!("CARGO_PKG_VERSION"), " read");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LlmsMode {
    Index,
    Full,
}

pub fn parse_llms_mode(raw: &str) -> Result<LlmsMode, String> {
    match raw {
        "index" => Ok(LlmsMode::Index),
        "full" => Ok(LlmsMode::Full),
        _ => Err(format!(
            "Invalid read --llms value '{}': expected index or full",
            raw
        )),
    }
}

#[derive(Debug, Clone)]
pub struct ReadOptions {
    /// Return the fetched response body without markdown or HTML extraction.
    pub raw: bool,
    /// Fail unless the selected response is served as Content-Type: text/markdown.
    pub require_md: bool,
    /// Return the nearest ancestor llms.txt or llms-full.txt view.
    pub llms: Option<LlmsMode>,
    /// Return a heading outline for the selected page content.
    pub outline: bool,
    /// Filter page sections, /llms.txt links, /llms-full.txt sections, or outline headings.
    pub filter: Option<String>,
    /// HTTP request timeout in milliseconds.
    pub timeout_ms: u64,
    /// Extra HTTP headers. A supplied Accept header disables markdown negotiation fallbacks.
    pub headers: HashMap<String, String>,
    /// Allowed domain patterns, using the same exact and wildcard semantics as --allowed-domains.
    pub allowed_domains: Vec<String>,
    /// Additional allowlists inherited from daemon state. URLs must match every non-empty allowlist.
    pub enforced_allowed_domains: Vec<Vec<String>>,
    /// `--links`: append the page's links as absolute URLs, at most this many (#503).
    pub links: Option<usize>,
}

impl Default for ReadOptions {
    fn default() -> Self {
        Self {
            raw: false,
            require_md: false,
            llms: None,
            outline: false,
            filter: None,
            timeout_ms: DEFAULT_TIMEOUT_MS,
            headers: HashMap::new(),
            allowed_domains: Vec::new(),
            enforced_allowed_domains: Vec::new(),
            links: None,
        }
    }
}

pub fn default_timeout_ms() -> u64 {
    DEFAULT_TIMEOUT_MS
}

pub fn parse_timeout_ms(raw: &str) -> Result<u64, String> {
    let ms = raw
        .parse::<u64>()
        .map_err(|_| format!("Invalid read timeout: {}", raw))?;
    if ms == 0 {
        return Err("Read timeout must be greater than 0".to_string());
    }
    Ok(ms)
}

pub fn options_from_command(cmd: &Value) -> Result<ReadOptions, String> {
    let timeout_ms = cmd
        .get("timeout")
        .and_then(|v| v.as_u64())
        .unwrap_or(DEFAULT_TIMEOUT_MS);
    if timeout_ms == 0 {
        return Err("Read timeout must be greater than 0".to_string());
    }
    let llms = cmd
        .get("llms")
        .and_then(|v| v.as_str())
        .map(parse_llms_mode)
        .transpose()?;
    let mut headers = HashMap::new();
    if let Some(value) = cmd.get("headers") {
        let map = value
            .as_object()
            .ok_or_else(|| "read headers must be a JSON object".to_string())?;
        for (key, value) in map {
            if let Some(value) = value.as_str() {
                headers.insert(key.to_string(), value.to_string());
            }
        }
    }
    let allowed_domains = cmd
        .get("allowedDomains")
        .and_then(|v| v.as_array())
        .map(|domains| {
            domains
                .iter()
                .filter_map(|domain| domain.as_str())
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    Ok(ReadOptions {
        raw: cmd.get("raw").and_then(|v| v.as_bool()).unwrap_or(false),
        require_md: cmd
            .get("requireMd")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        llms,
        outline: cmd
            .get("outline")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        filter: cmd
            .get("filter")
            .and_then(|v| v.as_str())
            .map(ToString::to_string),
        timeout_ms,
        headers,
        allowed_domains,
        enforced_allowed_domains: Vec::new(),
        links: cmd
            .get("links")
            .and_then(|v| v.as_u64())
            .map(|n| (n as usize).clamp(1, MAX_MAX_LINKS)),
    })
}

pub fn normalize_url(raw: &str) -> Result<Url, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("Read URL is empty".to_string());
    }

    let candidate = if trimmed.starts_with("http://")
        || trimmed.starts_with("https://")
        || trimmed.contains("://")
    {
        trimmed.to_string()
    } else {
        format!("https://{}", trimmed)
    };

    let mut url = Url::parse(&candidate).map_err(|e| format!("Invalid read URL: {}", e))?;
    match url.scheme() {
        "http" | "https" => {}
        scheme => {
            return Err(format!(
                "Unsupported read URL scheme '{}': use http or https",
                scheme
            ))
        }
    }
    if url.host_str().is_none() {
        return Err("Read URL must include a host".to_string());
    }
    url.set_fragment(None);
    Ok(url)
}

struct ReadFetch {
    final_url: String,
    status: u16,
    content_type: String,
    success: bool,
    body: String,
    truncated: bool,
}

#[derive(Clone)]
struct LlmsLink {
    title: String,
    url: Url,
}

pub async fn run_read(raw_url: &str, options: ReadOptions) -> Result<Value, String> {
    let target = normalize_url(raw_url)?;
    check_allowed_url_for_options(&target, &options)?;
    let redirect_allowed_domain_sets = allowed_domain_sets_for_options(&options);
    let redirect_policy = reqwest::redirect::Policy::custom(move |attempt| {
        if attempt.previous().len() > 10 {
            attempt.error("too many redirects")
        } else if let Err(e) = check_allowed_url_sets(attempt.url(), &redirect_allowed_domain_sets)
        {
            attempt.error(e)
        } else {
            attempt.follow()
        }
    });
    let client = Client::builder()
        .timeout(Duration::from_millis(options.timeout_ms))
        .redirect(redirect_policy)
        .build()
        .map_err(|e| format!("Failed to create HTTP client: {}", e))?;

    match options.llms {
        Some(LlmsMode::Index) => return run_llms_index(&client, &target, &options).await,
        Some(LlmsMode::Full) => return run_llms_full(&client, &target, &options).await,
        None => {}
    }

    let primary = fetch_read_url(&client, target.clone(), &options)
        .await
        .map_err(|e| format!("Read request failed: {}", e))?;

    if primary.success && direct_primary_response_is_usable(&primary, &options) {
        let (source, content) = content_from_fetch(&primary, &options)?;
        return Ok(read_json_from_content(
            &target, &primary, source, content, &options,
        ));
    }

    if !options.raw && !is_markdown_content_type(&primary.content_type) && should_try_md(&options) {
        if let Some(md_url) = markdown_fallback_url(&target) {
            match fetch_read_url(&client, md_url.clone(), &options).await {
                Ok(md)
                    if md.success
                        && markdown_fallback_content_type_is_usable(&md.content_type, &options) =>
                {
                    return Ok(read_json_from_content(
                        &target,
                        &md,
                        "path-markdown",
                        md.body.clone(),
                        &options,
                    ));
                }
                Ok(_) | Err(_) => {}
            }
        }
    }

    if primary.success && !options.require_md && is_plain_text_content_type(&primary.content_type) {
        let (source, content) = content_from_fetch(&primary, &options)?;
        return Ok(read_json_from_content(
            &target, &primary, source, content, &options,
        ));
    }

    if !options.raw && should_try_md(&options) {
        if let Some(llms) = try_llms_link(&client, &target, &options).await {
            return llms;
        }
    }

    if !primary.success {
        return Err(format!("Read failed with HTTP {}", primary.status));
    }

    let (source, content) = content_from_fetch(&primary, &options)?;
    if source == "html-fallback" {
        if let Some(problem) = app_shell_verdict(&primary.body, &content) {
            match problem {
                AppShell::Nothing(why) => {
                    return Err(format!(
                        "read got no readable text from {} — the HTML is a JavaScript app shell ({why}) \
                         that only renders in a browser, not an empty page. Open it instead: \
                         `chrome-use open <url>` then `text`, `snapshot`, or `read` with no url.",
                        primary.final_url
                    ));
                }
                AppShell::Little(why) => {
                    let mut value =
                        read_json_from_content(&target, &primary, source, content, &options);
                    value["warning"] = json!(format!(
                        "only {} characters of readable text — the HTML looks like a JavaScript app shell ({why}); \
                         the real page content may only render in a browser (`chrome-use open <url>` then `text`).",
                        value["content"].as_str().map(|c| c.trim().chars().count()).unwrap_or(0)
                    ));
                    return Ok(value);
                }
            }
        }
    }
    Ok(read_json_from_content(
        &target, &primary, source, content, &options,
    ))
}

/// What a fetch of a client-rendered page looks like from the outside.
enum AppShell {
    /// No readable text at all — the answer would have been an empty string,
    /// which reads as "this page is empty", not "this page needs a browser".
    Nothing(&'static str),
    /// A few words (a title, a `<noscript>` notice) — enough to look like an
    /// answer, not enough to be one.
    Little(&'static str),
}

/// Readable text under this many characters, on a page that carries an app
/// mount point, is treated as "the shell, not the page".
const APP_SHELL_LITTLE_TEXT: usize = 200;

/// Decide whether `html` is a JavaScript application shell whose content was
/// never in the response. `content` is the readable text already extracted.
///
/// `read` fetches with an HTTP client and does not run JavaScript, so a Nuxt
/// or Next page comes back as `<div id="__nuxt"></div>` plus scripts. The
/// extractor then returns "" — identical to what a genuinely empty page
/// returns, and the caller cannot tell the two apart. One person concluded
/// from that empty string (and from `curl` agreeing, which it always will)
/// that a public product page sat behind a login wall, and wrote up advice
/// on that basis (#255). Failing to render and having nothing to render must
/// not produce the same answer.
fn app_shell_verdict(html: &str, content: &str) -> Option<AppShell> {
    let lower = html.to_ascii_lowercase();
    if !lower.contains("<script") {
        return None;
    }
    let why = if lower.contains("id=\"__nuxt\"") || lower.contains("id=\"__nuxt\"") {
        "a Nuxt mount point"
    } else if lower.contains("id=\"__next\"") {
        "a Next.js mount point"
    } else if lower.contains("id=\"___gatsby\"") {
        "a Gatsby mount point"
    } else if lower.contains("id=\"root\"") || lower.contains("id=\"app\"") {
        "an empty app mount point"
    } else if lower.contains("<noscript") {
        "a <noscript> fallback"
    } else {
        // Scripts but no recognisable mount: only the empty case is confident.
        return content
            .trim()
            .is_empty()
            .then_some(AppShell::Nothing("scripts and no text"));
    };
    let chars = content.trim().chars().count();
    if chars == 0 {
        Some(AppShell::Nothing(why))
    } else if chars < APP_SHELL_LITTLE_TEXT {
        Some(AppShell::Little(why))
    } else {
        None
    }
}

async fn run_llms_index(
    client: &Client,
    target: &Url,
    options: &ReadOptions,
) -> Result<Value, String> {
    let fetch = fetch_first_llms_file(client, target, "llms.txt", options).await?;
    let content = format_llms_index(&fetch.body, &fetch.final_url, options.filter.as_deref())?;
    Ok(read_json(target, &fetch, "llms-index", content))
}

async fn run_llms_full(
    client: &Client,
    target: &Url,
    options: &ReadOptions,
) -> Result<Value, String> {
    let fetch = fetch_first_llms_file(client, target, "llms-full.txt", options).await?;
    let content = if let Some(filter) = options.filter.as_deref() {
        filter_markdown_sections(&fetch.body, filter, "No matching llms-full.txt sections")
    } else {
        fetch.body.clone()
    };
    Ok(read_json(target, &fetch, "llms-full", content))
}

async fn try_llms_link(
    client: &Client,
    target: &Url,
    options: &ReadOptions,
) -> Option<Result<Value, String>> {
    let (llms_url, llms) = fetch_optional_llms_file(client, target, "llms.txt", options).await?;
    let link = find_llms_link_for_target(&llms.body, &llms_url, target)?;
    let fetch = fetch_read_url(client, link.url.clone(), options)
        .await
        .ok()?;
    if !fetch.success {
        return None;
    }
    if options.require_md && !is_markdown_content_type(&fetch.content_type) {
        return None;
    }
    let content = if is_html_content_type(&fetch.content_type) {
        html_to_markdownish(&fetch.body)
    } else {
        fetch.body.clone()
    };
    Some(Ok(read_json_from_content(
        target,
        &fetch,
        "llms-link",
        content,
        options,
    )))
}

async fn fetch_read_url(
    client: &Client,
    target: Url,
    options: &ReadOptions,
) -> Result<ReadFetch, String> {
    check_allowed_url_for_options(&target, options)?;
    let mut request = client
        .get(target.clone())
        .header(USER_AGENT, USER_AGENT_VALUE);
    let has_accept_header = options
        .headers
        .keys()
        .any(|key| key.eq_ignore_ascii_case("accept"));
    if !has_accept_header {
        request = request.header(ACCEPT, READ_ACCEPT);
    }
    for (key, value) in &options.headers {
        request = request.header(key, value);
    }

    let response = request.send().await.map_err(format_reqwest_error)?;
    let status = response.status().as_u16();
    let final_url = response.url().to_string();
    let content_type = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let success = response.status().is_success();
    let (body, truncated) = read_limited_text(response).await?;

    Ok(ReadFetch {
        final_url,
        status,
        content_type,
        success,
        body,
        truncated,
    })
}

fn content_from_fetch(
    fetch: &ReadFetch,
    options: &ReadOptions,
) -> Result<(&'static str, String), String> {
    let content_type_base = fetch.content_type.split(';').next().unwrap_or("").trim();
    let content_type_lower = content_type_base.to_ascii_lowercase();
    if options.require_md && !is_markdown_content_type(&fetch.content_type) {
        Err(expected_markdown_error(&fetch.content_type))
    } else if options.raw {
        Ok(("raw", fetch.body.clone()))
    } else if is_markdown_like_content_type(&fetch.content_type) {
        Ok(("accept-markdown", fetch.body.clone()))
    } else if content_type_lower == "text/plain" {
        Ok(("text", fetch.body.clone()))
    } else if content_type_lower == "text/html" || content_type_lower == "application/xhtml+xml" {
        Ok(("html-fallback", html_to_markdownish(&fetch.body)))
    } else {
        Ok(("raw", fetch.body.clone()))
    }
}

fn format_reqwest_error(error: reqwest::Error) -> String {
    let mut message = error.to_string();
    let mut source = error.source();
    while let Some(err) = source {
        let part = err.to_string();
        if !part.is_empty() && !message.contains(&part) {
            message.push_str(": ");
            message.push_str(&part);
        }
        source = err.source();
    }
    message
}

fn read_json(target: &Url, fetch: &ReadFetch, source: &str, content: String) -> Value {
    json!({
        "url": target.to_string(),
        "finalUrl": fetch.final_url.clone(),
        "status": fetch.status,
        "contentType": fetch.content_type.clone(),
        "source": source,
        "truncated": fetch.truncated,
        "content": content,
    })
}

fn read_json_from_content(
    target: &Url,
    fetch: &ReadFetch,
    source: &str,
    content: String,
    options: &ReadOptions,
) -> Value {
    let mut value = if options.outline {
        let outline = format_page_outline(&content, &fetch.final_url, options.filter.as_deref());
        read_json(target, fetch, &format!("{}-outline", source), outline)
    } else if let Some(filter) = options.filter.as_deref() {
        let filtered = filter_page_sections(&content, filter);
        read_json(target, fetch, &format!("{}-filtered", source), filtered)
    } else {
        read_json(target, fetch, source, content)
    };
    if let Some(max) = options.links {
        match (source, Url::parse(&fetch.final_url)) {
            ("html-fallback", Ok(page)) => attach_links(
                &mut value,
                &collect_links(&fetch.body, &page, max, fetch.truncated),
            ),
            _ => note_links_unavailable(&mut value, source),
        }
    }
    value
}

pub fn read_json_from_active_html(active_url: &str, html: String, options: &ReadOptions) -> Value {
    let html_for_links = if options.links.is_some() && !options.raw {
        html.clone()
    } else {
        String::new()
    };
    let (source, content) = if options.raw {
        ("active-tab-raw", html)
    } else {
        ("active-tab-html", html_to_markdownish(&html))
    };
    let content = if options.outline {
        format_page_outline(&content, active_url, options.filter.as_deref())
    } else if let Some(filter) = options.filter.as_deref() {
        filter_page_sections(&content, filter)
    } else {
        content
    };
    let source = if options.outline {
        format!("{}-outline", source)
    } else if options.filter.is_some() {
        format!("{}-filtered", source)
    } else {
        source.to_string()
    };
    let mut value = json!({
        "url": active_url,
        "finalUrl": active_url,
        "contentType": "text/html",
        "source": source,
        "truncated": false,
        "content": content,
    });
    if let Some(max) = options.links {
        match (options.raw, Url::parse(active_url)) {
            (false, Ok(page)) => attach_links(
                &mut value,
                &collect_links(&html_for_links, &page, max, false),
            ),
            _ => note_links_unavailable(&mut value, &source),
        }
    }
    value
}

fn is_markdown_content_type(content_type: &str) -> bool {
    content_type_base(content_type).eq_ignore_ascii_case("text/markdown")
}

fn is_markdown_like_content_type(content_type: &str) -> bool {
    matches!(
        content_type_base(content_type).as_str(),
        "text/markdown" | "text/x-markdown" | "application/markdown"
    )
}

fn is_plain_text_content_type(content_type: &str) -> bool {
    content_type_base(content_type) == "text/plain"
}

fn content_type_base(content_type: &str) -> String {
    content_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase()
}

fn direct_primary_response_is_usable(fetch: &ReadFetch, options: &ReadOptions) -> bool {
    options.raw
        || is_markdown_content_type(&fetch.content_type)
        || (!options.require_md && is_markdown_like_content_type(&fetch.content_type))
}

fn markdown_fallback_content_type_is_usable(content_type: &str, options: &ReadOptions) -> bool {
    if options.require_md {
        is_markdown_content_type(content_type)
    } else {
        is_markdown_like_content_type(content_type) || is_plain_text_content_type(content_type)
    }
}

fn expected_markdown_error(content_type: &str) -> String {
    let base = content_type.split(';').next().unwrap_or("").trim();
    format!(
        "Expected text/markdown, got {}",
        if base.is_empty() {
            "unknown content type"
        } else {
            base
        }
    )
}

fn is_html_content_type(content_type: &str) -> bool {
    let base = content_type_base(content_type);
    base == "text/html" || base == "application/xhtml+xml"
}

fn should_try_md(options: &ReadOptions) -> bool {
    !options
        .headers
        .keys()
        .any(|key| key.eq_ignore_ascii_case("accept"))
}

fn check_allowed_url(url: &Url, allowed_domains: &[String]) -> Result<(), String> {
    if allowed_domains.is_empty() {
        return Ok(());
    }
    let hostname = url
        .host_str()
        .ok_or_else(|| format!("No hostname in URL: {}", url))?;
    let hostname_lower = hostname.to_ascii_lowercase();
    for pattern in allowed_domains {
        let pattern = pattern.trim().to_ascii_lowercase();
        if pattern.is_empty() {
            continue;
        }
        if let Some(suffix) = pattern.strip_prefix("*.") {
            if hostname_lower == suffix || hostname_lower.ends_with(&format!(".{}", suffix)) {
                return Ok(());
            }
        } else if hostname_lower == pattern {
            return Ok(());
        }
    }
    Err(format!(
        "Domain '{}' is not in the allowed domains list",
        hostname
    ))
}

fn allowed_domain_sets_for_options(options: &ReadOptions) -> Vec<Vec<String>> {
    let mut sets = Vec::new();
    if !options.allowed_domains.is_empty() {
        sets.push(options.allowed_domains.clone());
    }
    sets.extend(
        options
            .enforced_allowed_domains
            .iter()
            .filter(|domains| !domains.is_empty())
            .cloned(),
    );
    sets
}

fn check_allowed_url_for_options(url: &Url, options: &ReadOptions) -> Result<(), String> {
    check_allowed_url(url, &options.allowed_domains)?;
    for domains in &options.enforced_allowed_domains {
        check_allowed_url(url, domains)?;
    }
    Ok(())
}

fn check_allowed_url_sets(url: &Url, allowed_domain_sets: &[Vec<String>]) -> Result<(), String> {
    for domains in allowed_domain_sets {
        check_allowed_url(url, domains)?;
    }
    Ok(())
}

pub fn check_allowed_active_url_for_options(
    raw_url: &str,
    options: &ReadOptions,
) -> Result<(), String> {
    if options.allowed_domains.is_empty()
        && options
            .enforced_allowed_domains
            .iter()
            .all(|domains| domains.is_empty())
    {
        return Ok(());
    }

    let url = Url::parse(raw_url).map_err(|e| format!("Invalid active tab URL: {}", e))?;
    match url.scheme() {
        "http" | "https" => check_allowed_url_for_options(&url, options),
        scheme => Err(format!(
            "Active tab URL scheme '{}' is not allowed by domain filter",
            scheme
        )),
    }
}

fn markdown_fallback_url(url: &Url) -> Option<Url> {
    if url.path().ends_with(".md") {
        return None;
    }
    let mut md_url = url.clone();
    let path = url.path();
    let next_path = if path == "/" || path.is_empty() {
        "/index.md".to_string()
    } else {
        format!("{}.md", path.trim_end_matches('/'))
    };
    md_url.set_path(&next_path);
    Some(md_url)
}

async fn fetch_first_llms_file(
    client: &Client,
    target: &Url,
    filename: &str,
    options: &ReadOptions,
) -> Result<ReadFetch, String> {
    let mut last_status = None;
    for url in llms_file_candidates(target, filename) {
        let fetch = fetch_read_url(client, url, options)
            .await
            .map_err(|e| format!("Read request failed: {}", e))?;
        if fetch.success {
            if is_html_content_type(&fetch.content_type) {
                last_status = Some(fetch.status);
                continue;
            }
            if options.require_md && !is_markdown_content_type(&fetch.content_type) {
                return Err(expected_markdown_error(&fetch.content_type));
            }
            return Ok(fetch);
        }
        last_status = Some(fetch.status);
    }

    match last_status {
        Some(status) => Err(format!("{} failed with HTTP {}", filename, status)),
        None => Err(format!("{} not found", filename)),
    }
}

async fn fetch_optional_llms_file(
    client: &Client,
    target: &Url,
    filename: &str,
    options: &ReadOptions,
) -> Option<(Url, ReadFetch)> {
    for url in llms_file_candidates(target, filename) {
        let fetch = fetch_read_url(client, url.clone(), options).await.ok()?;
        if fetch.success && !is_html_content_type(&fetch.content_type) {
            return Some((url, fetch));
        }
    }
    None
}

fn llms_file_candidates(url: &Url, filename: &str) -> Vec<Url> {
    let mut candidates = Vec::new();
    let mut prefixes = Vec::new();
    let path = url.path().trim_matches('/');
    if !path.is_empty() {
        let segments = path.split('/').collect::<Vec<_>>();
        for len in (1..=segments.len()).rev() {
            prefixes.push(format!("/{}", segments[..len].join("/")));
        }
    }
    prefixes.push(String::new());

    for prefix in prefixes {
        let mut candidate = url.clone();
        let path = if prefix.is_empty() {
            format!("/{}", filename)
        } else {
            format!("{}/{}", prefix.trim_end_matches('/'), filename)
        };
        candidate.set_path(&path);
        candidate.set_query(None);
        candidate.set_fragment(None);
        if !candidates
            .iter()
            .any(|existing: &Url| existing == &candidate)
        {
            candidates.push(candidate);
        }
    }

    candidates
}

fn parse_llms_links(body: &str, base_url: &Url) -> Vec<LlmsLink> {
    let mut links = Vec::new();
    for line in body.lines() {
        let Some(line) = markdown_list_item_text(line) else {
            continue;
        };
        let mut cursor = 0;
        while let Some(label_start_rel) = line[cursor..].find('[') {
            let label_start = cursor + label_start_rel;
            if label_start > 0 && line.as_bytes().get(label_start - 1) == Some(&b'!') {
                cursor = label_start + 1;
                continue;
            }
            let Some(label_end_rel) = line[label_start + 1..].find("](") else {
                break;
            };
            let label_end = label_start + 1 + label_end_rel;
            let href_start = label_end + 2;
            let Some(href_end_rel) = line[href_start..].find(')') else {
                break;
            };
            let href_end = href_start + href_end_rel;
            let title = line[label_start + 1..label_end].trim();
            let href = line[href_start..href_end]
                .split_whitespace()
                .next()
                .unwrap_or("")
                .trim_matches('<')
                .trim_matches('>');
            if !title.is_empty() && !href.is_empty() {
                if let Ok(url) = base_url.join(href) {
                    links.push(LlmsLink {
                        title: title.to_string(),
                        url,
                    });
                }
            }
            cursor = href_end + 1;
        }
    }
    links
}

fn find_llms_link_for_target(body: &str, base_url: &Url, target: &Url) -> Option<LlmsLink> {
    let target_key = doc_match_key(target)?;
    let links = dedupe_llms_links(parse_llms_links(body, base_url));
    if let Some(exact) = links
        .iter()
        .find(|link| doc_match_key(&link.url).as_ref() == Some(&target_key))
    {
        return Some(exact.clone());
    }

    let target_origin = origin_key(target)?;
    let target_segment = last_doc_segment(target)?;
    let mut candidates = links
        .into_iter()
        .filter(|link| origin_key(&link.url).as_ref() == Some(&target_origin))
        .filter(|link| {
            last_doc_segment(&link.url).as_ref() == Some(&target_segment)
                || slugify_label(&link.title) == target_segment
        })
        .collect::<Vec<_>>();
    if candidates.len() == 1 {
        candidates.pop()
    } else {
        None
    }
}

fn markdown_list_item_text(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    for marker in ["- ", "* ", "+ "] {
        if let Some(rest) = trimmed.strip_prefix(marker) {
            return Some(rest);
        }
    }

    let marker_end = trimmed.find(['.', ')'])?;
    if marker_end == 0 || !trimmed[..marker_end].chars().all(|ch| ch.is_ascii_digit()) {
        return None;
    }
    trimmed[marker_end + 1..].strip_prefix(' ')
}

fn dedupe_llms_links(mut links: Vec<LlmsLink>) -> Vec<LlmsLink> {
    let mut seen = HashSet::new();
    links.retain(|link| {
        seen.insert(format!(
            "{}\0{}",
            link.title.to_ascii_lowercase(),
            link.url.as_str()
        ))
    });
    links
}

fn doc_match_key(url: &Url) -> Option<String> {
    Some(format!("{}{}", origin_key(url)?, normalized_doc_path(url)))
}

fn origin_key(url: &Url) -> Option<String> {
    let host = url.host_str()?;
    let authority = if let Some(port) = url.port() {
        format!("{}:{}", host, port)
    } else {
        host.to_string()
    };
    Some(format!("{}://{}", url.scheme(), authority))
}

fn normalized_doc_path(url: &Url) -> String {
    let mut path = url.path().trim_end_matches('/').to_string();
    if path.is_empty() {
        path = "/".to_string();
    }
    if let Some(stripped) = path.strip_suffix(".md") {
        path = stripped.to_string();
    }
    if path.ends_with("/index") {
        path.truncate(path.len() - "/index".len());
        if path.is_empty() {
            path = "/".to_string();
        }
    }
    path
}

fn last_doc_segment(url: &Url) -> Option<String> {
    normalized_doc_path(url)
        .trim_matches('/')
        .rsplit('/')
        .find(|segment| !segment.is_empty())
        .map(|segment| segment.to_ascii_lowercase())
}

fn slugify_label(label: &str) -> String {
    let mut slug = String::new();
    for ch in label.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    slug.trim_matches('-').to_string()
}

fn format_llms_index(body: &str, final_url: &str, filter: Option<&str>) -> Result<String, String> {
    let base = Url::parse(final_url).map_err(|e| format!("Invalid llms.txt URL: {}", e))?;
    let mut links = dedupe_llms_links(parse_llms_links(body, &base));
    if let Some(filter) = filter {
        let needle = filter.to_ascii_lowercase();
        links.retain(|link| {
            link.title.to_ascii_lowercase().contains(&needle)
                || link.url.as_str().to_ascii_lowercase().contains(&needle)
        });
    }
    if links.is_empty() {
        if filter.is_some() {
            return Ok("No matching llms.txt links".to_string());
        }
        return Ok(normalize_markdownish(body));
    }

    let mut out = format!("# llms.txt\n\nSource: {}\n", final_url);
    for link in links {
        out.push_str(&format!("\n- [{}]({})", link.title, link.url));
    }
    Ok(out)
}

struct Heading {
    level: usize,
    title: String,
}

fn format_page_outline(content: &str, final_url: &str, filter: Option<&str>) -> String {
    let mut headings = parse_markdown_headings(content);
    if let Some(filter) = filter {
        let needle = filter.to_ascii_lowercase();
        headings.retain(|heading| heading.title.to_ascii_lowercase().contains(&needle));
    }
    if headings.is_empty() {
        if filter.is_some() {
            return "No matching headings".to_string();
        }
        return "No headings found".to_string();
    }

    let mut out = format!("# Outline\n\nSource: {}\n", final_url);
    for heading in headings {
        out.push('\n');
        out.push_str(&"  ".repeat(heading.level.saturating_sub(1)));
        out.push_str("- ");
        out.push_str(&heading.title);
    }
    out
}

fn parse_markdown_headings(content: &str) -> Vec<Heading> {
    content.lines().filter_map(parse_markdown_heading).collect()
}

fn filter_page_sections(content: &str, filter: &str) -> String {
    let needle = filter.to_ascii_lowercase();
    let lines = content.lines().collect::<Vec<_>>();
    let headings = lines
        .iter()
        .enumerate()
        .filter_map(|(index, line)| parse_markdown_heading(line).map(|heading| (index, heading)))
        .collect::<Vec<_>>();

    let mut sections = Vec::new();
    let mut captured_until = 0;
    for (i, (start, heading)) in headings.iter().enumerate() {
        if *start < captured_until || !heading.title.to_ascii_lowercase().contains(&needle) {
            continue;
        }
        let end = headings[i + 1..]
            .iter()
            .find(|(_, next)| next.level <= heading.level)
            .map(|(index, _)| *index)
            .unwrap_or(lines.len());
        captured_until = end;
        sections.push(lines[*start..end].join("\n").trim().to_string());
    }

    if !sections.is_empty() {
        return sections.join("\n\n");
    }

    filter_markdown_sections(content, filter, "No matching page sections")
}

fn parse_markdown_heading(line: &str) -> Option<Heading> {
    let trimmed = line.trim_start();
    let level = trimmed.chars().take_while(|ch| *ch == '#').count();
    if level == 0 || level > 6 {
        return None;
    }
    let rest = &trimmed[level..];
    if !rest.is_empty()
        && !rest
            .chars()
            .next()
            .map(char::is_whitespace)
            .unwrap_or(false)
    {
        return None;
    }
    let title = rest.trim().trim_end_matches('#').trim();
    if title.is_empty() {
        None
    } else {
        Some(Heading {
            level,
            title: title.to_string(),
        })
    }
}

fn filter_markdown_sections(body: &str, filter: &str, no_match_message: &str) -> String {
    let needle = filter.to_ascii_lowercase();
    let mut sections: Vec<String> = Vec::new();
    let mut current = String::new();

    for line in body.lines() {
        if line.trim_start().starts_with('#') && !current.trim().is_empty() {
            if current.to_ascii_lowercase().contains(&needle) {
                sections.push(current.trim().to_string());
            }
            current.clear();
        }
        current.push_str(line);
        current.push('\n');
    }

    if !current.trim().is_empty() && current.to_ascii_lowercase().contains(&needle) {
        sections.push(current.trim().to_string());
    }

    if !sections.is_empty() {
        return sections.join("\n\n");
    }

    let matching_lines = body
        .lines()
        .filter(|line| line.to_ascii_lowercase().contains(&needle))
        .collect::<Vec<_>>();
    if matching_lines.is_empty() {
        no_match_message.to_string()
    } else {
        matching_lines.join("\n")
    }
}

async fn read_limited_text(response: reqwest::Response) -> Result<(String, bool), String> {
    let mut bytes = Vec::new();
    let mut truncated = false;
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| e.to_string())?;
        let remaining = BODY_LIMIT.saturating_sub(bytes.len());
        if remaining == 0 {
            truncated = true;
            break;
        }
        if chunk.len() > remaining {
            bytes.extend_from_slice(&chunk[..remaining]);
            truncated = true;
            break;
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok((String::from_utf8_lossy(&bytes).to_string(), truncated))
}

/// Default and ceiling for `read --links` (#503): enough for a front page
/// (Hacker News lists ~230 unique links), bounded so a link farm cannot
/// flood the reply.
pub const DEFAULT_MAX_LINKS: usize = 100;
pub const MAX_MAX_LINKS: usize = 1000;

/// Longest link text kept in the list; the URL is the point, not the prose.
const LINK_TEXT_MAX: usize = 80;
/// Most HTML bytes scanned for links. A longer page is scanned up to here,
/// and the count is then a lower bound.
const LINK_SCAN_MAX_BYTES: usize = 4 * 1024 * 1024;
/// Most anchors kept from the scan (before resolution). Beyond this the
/// count is a lower bound.
const LINK_RAW_MAX: usize = 10_000;
/// Longest URL listed (also the longest href or base resolved against). A
/// longer one is omitted and counted, never cut into a different address.
const LINK_URL_MAX_BYTES: usize = 2048;
/// Most distinct URLs remembered for de-duplication. Beyond this the count
/// is a lower bound.
const LINK_SEEN_MAX: usize = 5000;
/// Most bytes the listed links may take in the reply.
const LINK_OUTPUT_MAX_BYTES: usize = 256 * 1024;

pub fn parse_max_links(raw: &str) -> Result<usize, String> {
    let n = raw
        .parse::<usize>()
        .map_err(|_| format!("Invalid read --max-links value: {raw}"))?;
    if n == 0 || n > MAX_MAX_LINKS {
        return Err(format!(
            "read --max-links must be between 1 and {MAX_MAX_LINKS}, got {n}"
        ));
    }
    Ok(n)
}

/// The links of an HTML page, resolved to absolute URLs (#503).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PageLinks {
    /// `(text, absolute url)` in document order, unique by URL, at most the
    /// requested number and [`LINK_OUTPUT_MAX_BYTES`].
    pub shown: Vec<(String, String)>,
    /// Unique navigable links found.
    pub total: usize,
    /// False when a budget cut the scan or the de-duplication short, so
    /// `total` is a lower bound.
    pub total_exact: bool,
    /// Links left out because their URL (or the href or base it is resolved
    /// from) is longer than [`LINK_URL_MAX_BYTES`].
    pub omitted_too_long: usize,
    /// Which budgets cut something: `scan`, `anchors`, `dedup`, `output`.
    pub budgets_hit: Vec<&'static str>,
}

/// One anchor as the tokenizer saw it.
struct RawLink {
    href: String,
    text: String,
    label: Option<String>,
}

/// HTML elements whose content is not markup (and, for these, never holds
/// links the page shows): skipped whole, so an `<a>` or `<base>` written
/// inside a script string or a style block cannot be mistaken for one.
const RAW_TEXT: &[&str] = &[
    "script",
    "style",
    "textarea",
    "title",
    "xmp",
    "noembed",
    "noframes",
    "noscript",
    "template",
    "iframe",
    "plaintext",
];
const VOID: &[&str] = &[
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source",
    "track", "wbr",
];

/// Collect `<a href>` links from `html` with a quote-aware tokenizer:
/// comments, `<!…>` and raw-text elements (script, style, textarea, …) are
/// skipped, a `>` inside a quoted attribute does not end the tag, and an
/// anchor's text ends at `</a>`, at the next `<a>`, or where an element that
/// was open around it closes. The document's base is its first `<base
/// href>` (wherever it appears), else `page_url`.
///
/// Skipped: no href, a same-page `#fragment` and `javascript:`
/// pseudo-links. Every step is bounded (see the `LINK_*` budgets) and says
/// so in the result: an overlong URL is omitted, never truncated.
pub fn collect_links(html: &str, page_url: &Url, max: usize, input_truncated: bool) -> PageLinks {
    let mut out = PageLinks {
        total_exact: true,
        ..Default::default()
    };
    // The page itself was cut before it got here (the HTTP body limit): the
    // links after the cut are unknown, and the cut may fall inside a tag.
    if input_truncated {
        out.budgets_hit.push("input");
    }
    let scan = if html.len() > LINK_SCAN_MAX_BYTES {
        out.budgets_hit.push("scan");
        let mut end = LINK_SCAN_MAX_BYTES;
        while !html.is_char_boundary(end) {
            end -= 1;
        }
        &html[..end]
    } else {
        html
    };
    let (raw, base_href, anchors_cut) = tokenize_links(scan);
    if anchors_cut {
        out.budgets_hit.push("anchors");
    }

    // The base is entity-decoded like every href, and the length budget
    // applies to the decoded value.
    let base = match base_href.map(|h| decode_html_entities(h.trim())) {
        Some(h) if h.len() <= LINK_URL_MAX_BYTES => page_url.join(&h).ok(),
        Some(_) => None,
        None => Some(page_url.clone()),
    };
    let base_too_long = base
        .as_ref()
        .map(|b| b.as_str().len() > LINK_URL_MAX_BYTES)
        .unwrap_or(true);

    let mut seen: HashSet<String> = HashSet::new();
    let mut output_bytes = 0usize;
    let mut dedup_cut = false;
    for link in raw {
        let href = decode_html_entities(link.href.trim());
        let lower = href.to_ascii_lowercase();
        if href.is_empty() || href.starts_with('#') || lower.starts_with("javascript:") {
            continue;
        }
        if href.len() > LINK_URL_MAX_BYTES {
            out.omitted_too_long += 1;
            continue;
        }
        // An absolute href needs no base (and no join with an overlong one).
        let absolute = Url::parse(&href).ok();
        let resolved = match absolute {
            Some(u) => u,
            None => {
                // A relative href against an overlong (or unusable) base
                // would only produce an overlong URL: do not build it.
                if base_too_long {
                    out.omitted_too_long += 1;
                    continue;
                }
                match base.as_ref().and_then(|b| b.join(&href).ok()) {
                    Some(u) => u,
                    None => continue,
                }
            }
        };
        let url = resolved.to_string();
        if url.len() > LINK_URL_MAX_BYTES {
            out.omitted_too_long += 1;
            continue;
        }
        if seen.contains(&url) {
            continue;
        }
        if seen.len() >= LINK_SEEN_MAX {
            // Cannot tell new from repeated any more: stop counting.
            dedup_cut = true;
            break;
        }
        seen.insert(url.clone());
        out.total += 1;
        if out.shown.len() < max {
            let text = link_label(&link);
            let cost = text.len() + url.len() + 8;
            if output_bytes + cost <= LINK_OUTPUT_MAX_BYTES {
                output_bytes += cost;
                out.shown.push((text, url));
            } else if !out.budgets_hit.contains(&"output") {
                out.budgets_hit.push("output");
            }
        }
    }
    if dedup_cut {
        out.budgets_hit.push("dedup");
    }
    out.total_exact = !out
        .budgets_hit
        .iter()
        .any(|b| matches!(*b, "input" | "scan" | "anchors" | "dedup"));
    out
}

/// Walk `html` and return its anchors (up to [`LINK_RAW_MAX`]), the first
/// `<base href>`, and whether the anchor budget cut the list.
fn tokenize_links(html: &str) -> (Vec<RawLink>, Option<String>, bool) {
    let bytes = html.as_bytes();
    let lower = html.to_ascii_lowercase();
    let mut links: Vec<RawLink> = Vec::new();
    let mut base: Option<String> = None;
    let mut cut = false;
    // Open elements (names), and the anchor being read: (depth when it
    // opened, href, text so far, label).
    let mut stack: Vec<String> = Vec::new();
    let mut open: Option<(usize, String, String, Option<String>)> = None;
    let close_anchor = |open: &mut Option<(usize, String, String, Option<String>)>,
                        links: &mut Vec<RawLink>,
                        cut: &mut bool| {
        if let Some((_, href, text, label)) = open.take() {
            if links.len() < LINK_RAW_MAX {
                links.push(RawLink { href, text, label });
            } else {
                *cut = true;
            }
        }
    };
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'<' {
            // Text: only kept while inside an anchor, and bounded.
            let next = html[i..].find('<').map(|p| i + p).unwrap_or(bytes.len());
            if let Some((_, _, text, _)) = open.as_mut() {
                if text.len() < 1024 {
                    let take = &html[i..next];
                    let room = 1024 - text.len();
                    let mut end = take.len().min(room);
                    while !take.is_char_boundary(end) {
                        end -= 1;
                    }
                    text.push_str(&take[..end]);
                    text.push(' ');
                }
            }
            i = next;
            continue;
        }
        let rest = &lower[i..];
        if rest.starts_with("<!--") {
            i = lower[i + 4..]
                .find("-->")
                .map(|p| i + 4 + p + 3)
                .unwrap_or(bytes.len());
            continue;
        }
        if rest.starts_with("<!") || rest.starts_with("<?") {
            i = lower[i..]
                .find('>')
                .map(|p| i + p + 1)
                .unwrap_or(bytes.len());
            continue;
        }
        if rest.starts_with("</") {
            let name_start = i + 2;
            let mut j = name_start;
            while j < bytes.len() && bytes[j].is_ascii_alphanumeric() {
                j += 1;
            }
            let name = lower[name_start..j].to_string();
            i = lower[j..]
                .find('>')
                .map(|p| j + p + 1)
                .unwrap_or(bytes.len());
            if name == "a" {
                close_anchor(&mut open, &mut links, &mut cut);
                if let Some(pos) = stack.iter().rposition(|n| n == "a") {
                    stack.truncate(pos);
                }
                continue;
            }
            if let Some(pos) = stack.iter().rposition(|n| *n == name) {
                // An element that was open around the anchor closes: so does
                // the anchor's text.
                if open.as_ref().is_some_and(|o| pos < o.0) {
                    close_anchor(&mut open, &mut links, &mut cut);
                }
                stack.truncate(pos);
            }
            continue;
        }
        // A start tag: `<` followed by a letter.
        if !(i + 1 < bytes.len() && bytes[i + 1].is_ascii_alphabetic()) {
            if let Some((_, _, text, _)) = open.as_mut() {
                text.push('<');
            }
            i += 1;
            continue;
        }
        let name_start = i + 1;
        let mut j = name_start;
        while j < bytes.len()
            && !bytes[j].is_ascii_whitespace()
            && bytes[j] != b'>'
            && bytes[j] != b'/'
        {
            j += 1;
        }
        let name = lower[name_start..j].to_string();
        // Attributes, quote-aware: a `>` inside quotes does not end the tag.
        // A tag the input ends inside (a budget or body cut) is dropped and
        // the scan stops: a cut href is not a destination.
        let Some((attrs, end)) = parse_attributes(html, j) else {
            break;
        };
        i = end;
        let attr = |n: &str| attrs.iter().find(|(k, _)| k == n).map(|(_, v)| v.clone());
        match name.as_str() {
            "a" => {
                // HTML closes an open anchor when another one starts.
                close_anchor(&mut open, &mut links, &mut cut);
                if let Some(pos) = stack.iter().rposition(|n| n == "a") {
                    stack.truncate(pos);
                }
                if let Some(href) = attr("href") {
                    let label = attr("aria-label").or_else(|| attr("title"));
                    open = Some((stack.len(), href, String::new(), label));
                }
                stack.push(name);
            }
            "base" => {
                if base.is_none() {
                    base = attr("href");
                }
            }
            "img" => {
                if let (Some((_, _, text, label)), Some(alt)) = (open.as_mut(), attr("alt")) {
                    if label.is_none() && text.trim().is_empty() {
                        *label = Some(alt);
                    }
                }
            }
            // Raw text runs to its exact end tag. A self-closing flag on a
            // non-void element is ignored, as in HTML: `<script/>` still opens
            // a script.
            n if RAW_TEXT.contains(&n) => {
                i = raw_text_end(&lower, i, n);
            }
            n if VOID.contains(&n) => {}
            _ => stack.push(name),
        }
    }
    close_anchor(&mut open, &mut links, &mut cut);
    (links, base, cut)
}

/// The index of the end tag `</name` that closes a raw-text element opened
/// before `from` (the name followed by whitespace, `/` or `>`), or the end of
/// the input. `</scriptx>` does not end a script.
fn raw_text_end(lower: &str, from: usize, name: &str) -> usize {
    let close = format!("</{name}");
    let mut at = from;
    while let Some(p) = lower[at..].find(&close) {
        let start = at + p;
        let after = start + close.len();
        match lower.as_bytes().get(after) {
            None => return start,
            Some(b) if b.is_ascii_whitespace() || *b == b'/' || *b == b'>' => return start,
            _ => at = after,
        }
    }
    lower.len()
}

/// Parse attributes from `html[from..]` up to the end of the start tag.
/// Returns `(name, value)` pairs (names lowercased, values raw) and the index
/// just past the closing `>`; `None` when the input ends inside the tag or
/// inside a quoted value, so a cut fragment is never taken as complete.
fn parse_attributes(html: &str, from: usize) -> Option<(Vec<(String, String)>, usize)> {
    let bytes = html.as_bytes();
    let mut attrs = Vec::new();
    let mut i = from;
    while i < bytes.len() {
        match bytes[i] {
            b'>' => return Some((attrs, i + 1)),
            b'/' => {
                i += 1;
                continue;
            }
            c if c.is_ascii_whitespace() => {
                i += 1;
                continue;
            }
            _ => {}
        }
        let name_start = i;
        while i < bytes.len()
            && !bytes[i].is_ascii_whitespace()
            && !matches!(bytes[i], b'=' | b'>' | b'/')
        {
            i += 1;
        }
        let name = html[name_start..i].to_ascii_lowercase();
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        let mut value = String::new();
        if i < bytes.len() && bytes[i] == b'=' {
            i += 1;
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            if i < bytes.len() && (bytes[i] == b'"' || bytes[i] == b'\'') {
                let q = bytes[i];
                let v_start = i + 1;
                let v_end = html[v_start..]
                    .bytes()
                    .position(|b| b == q)
                    .map(|p| v_start + p)?;
                value = html[v_start..v_end].to_string();
                i = v_end + 1;
            } else {
                let v_start = i;
                while i < bytes.len() && !bytes[i].is_ascii_whitespace() && bytes[i] != b'>' {
                    i += 1;
                }
                value = html[v_start..i].to_string();
            }
        }
        if !name.is_empty() && !attrs.iter().any(|(k, _): &(String, String)| *k == name) {
            attrs.push((name, value));
        }
    }
    None
}

/// A one-line label for a link: its text, else `aria-label` / `title`, else
/// an image's `alt`.
fn link_label(link: &RawLink) -> String {
    let text = decode_html_entities(&link.text)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let text = if !text.is_empty() {
        text
    } else {
        link.label
            .as_deref()
            .map(|l| decode_html_entities(l.trim()))
            .filter(|l| !l.is_empty())
            .unwrap_or_else(|| "(no text)".to_string())
    };
    let text = text.replace(['[', ']'], "");
    if text.chars().count() > LINK_TEXT_MAX {
        let mut t: String = text.chars().take(LINK_TEXT_MAX - 1).collect();
        t.push('…');
        t
    } else {
        text
    }
}

/// Append the page's links to a `read` reply (#503): a `## Links` section at
/// the end of `content` (so text mode shows it inside the same content
/// boundaries as the page), plus `links`, `linksTotal`, `linksTotalExact`,
/// `linksShown`, `linksOmittedTooLong` and `linksBudgetsHit` in JSON. Every
/// cut says what was cut and whether the total is exact.
fn attach_links(value: &mut Value, links: &PageLinks) {
    let shown = links.shown.len();
    let total = if links.total_exact {
        format!("{}", links.total)
    } else {
        format!("at least {}", links.total)
    };
    let mut section = String::from("\n\n## Links\n");
    if links.total == 0 && links.omitted_too_long == 0 {
        section.push_str(if links.total_exact {
            "\n(no links on this page)\n"
        } else {
            "\n(no links found in the part of the page scanned)\n"
        });
    } else {
        section.push('\n');
        for (text, url) in &links.shown {
            section.push_str(&format!("- [{text}]({url})\n"));
        }
        if links.total > shown || !links.total_exact {
            section.push_str(&format!(
                "\n({shown} of {total} links shown; raise the cap with --max-links <n>, up to \
                 {MAX_MAX_LINKS})\n"
            ));
        }
    }
    if links.omitted_too_long > 0 {
        section.push_str(&format!(
            "({} links omitted: their URL is longer than {LINK_URL_MAX_BYTES} bytes)\n",
            links.omitted_too_long
        ));
    }
    if !links.budgets_hit.is_empty() {
        section.push_str(&format!(
            "(link scan cut by a budget: {}; the count is {})\n",
            links.budgets_hit.join(", "),
            if links.total_exact {
                "exact"
            } else {
                "a lower bound"
            }
        ));
    }
    if let Some(content) = value.get("content").and_then(Value::as_str) {
        value["content"] = json!(format!("{}{}", content.trim_end(), section.trim_end()));
    }
    value["links"] = json!(links
        .shown
        .iter()
        .map(|(text, url)| json!({ "text": text, "url": url }))
        .collect::<Vec<_>>());
    value["linksTotal"] = json!(links.total);
    value["linksTotalExact"] = json!(links.total_exact);
    value["linksShown"] = json!(shown);
    value["linksOmittedTooLong"] = json!(links.omitted_too_long);
    value["linksBudgetsHit"] = json!(links.budgets_hit);
}

/// `--links` on a response that is not an HTML page: nothing to collect,
/// and the reply says so instead of looking like a page without links.
fn note_links_unavailable(value: &mut Value, source: &str) {
    let note = format!(
        "--links applies to HTML pages; this response came from {source}, whose links (if any) \
         are inline in the content"
    );
    if value.get("warning").is_none() {
        value["warning"] = json!(note);
    }
}

fn html_to_markdownish(html: &str) -> String {
    let stripped = strip_ignored_html_blocks(html);
    let mut out = String::new();
    let mut chars = stripped.chars().peekable();
    let mut in_pre = false;

    while let Some(ch) = chars.next() {
        if ch != '<' {
            out.push(ch);
            continue;
        }

        let mut tag = String::new();
        for next in chars.by_ref() {
            if next == '>' {
                break;
            }
            tag.push(next);
        }
        handle_html_tag(&tag, &mut out, &mut in_pre);
    }

    let decoded = decode_html_entities(&out);
    if in_pre {
        normalize_markdownish(&format!("{}\n```", decoded))
    } else {
        normalize_markdownish(&decoded)
    }
}

fn strip_ignored_html_blocks(html: &str) -> String {
    let mut remaining = html.to_string();
    for tag in ["script", "style", "noscript", "svg", "head"] {
        remaining = strip_tag_block(&remaining, tag);
    }
    remaining
}

fn strip_tag_block(input: &str, tag: &str) -> String {
    let mut out = String::new();
    let mut cursor = 0;
    let lower = input.to_ascii_lowercase();
    let close = format!("</{}>", tag);

    while let Some(start) = find_open_tag(&lower, tag, cursor) {
        out.push_str(&input[cursor..start]);
        let after_start = lower[start..]
            .find('>')
            .map(|idx| start + idx + 1)
            .unwrap_or(input.len());
        if let Some(close_rel) = lower[after_start..].find(&close) {
            cursor = after_start + close_rel + close.len();
        } else {
            cursor = input.len();
        }
    }
    out.push_str(&input[cursor..]);
    out
}

fn find_open_tag(lower: &str, tag: &str, cursor: usize) -> Option<usize> {
    let needle = format!("<{}", tag);
    let mut search = cursor;
    while let Some(rel) = lower[search..].find(&needle) {
        let start = search + rel;
        let after_name = start + needle.len();
        let is_boundary = lower
            .as_bytes()
            .get(after_name)
            .map(|byte| matches!(byte, b' ' | b'\t' | b'\n' | b'\r' | b'\x0c' | b'/' | b'>'))
            .unwrap_or(true);
        if is_boundary {
            return Some(start);
        }
        search = after_name;
    }
    None
}

fn handle_html_tag(raw: &str, out: &mut String, in_pre: &mut bool) {
    let tag = raw.trim();
    if tag.is_empty() || tag.starts_with('!') || tag.starts_with('?') {
        return;
    }
    let closing = tag.starts_with('/');
    let name = tag
        .trim_start_matches('/')
        .split_whitespace()
        .next()
        .unwrap_or("")
        .trim_end_matches('/')
        .to_ascii_lowercase();

    if closing {
        match name.as_str() {
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "p" | "div" | "section" | "article"
            | "main" | "header" | "footer" | "nav" | "blockquote" | "li" | "tr" | "table"
            | "ul" | "ol" => out.push_str("\n\n"),
            "pre" => {
                out.push_str("\n```\n\n");
                *in_pre = false;
            }
            _ => {}
        }
        return;
    }

    match name.as_str() {
        "br" => out.push('\n'),
        "p" | "div" | "section" | "article" | "main" | "header" | "footer" | "nav"
        | "blockquote" | "table" | "tr" | "ul" | "ol" => out.push_str("\n\n"),
        "li" => out.push_str("\n- "),
        "h1" => out.push_str("\n\n# "),
        "h2" => out.push_str("\n\n## "),
        "h3" => out.push_str("\n\n### "),
        "h4" => out.push_str("\n\n#### "),
        "h5" => out.push_str("\n\n##### "),
        "h6" => out.push_str("\n\n###### "),
        "pre" => {
            out.push_str("\n\n```\n");
            *in_pre = true;
        }
        _ => {}
    }
}

fn decode_html_entities(input: &str) -> String {
    let mut out = String::new();
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '&' {
            out.push(ch);
            continue;
        }

        let mut entity = String::new();
        while let Some(&next) = chars.peek() {
            if next == ';' {
                chars.next();
                break;
            }
            if entity.len() >= 16 || next.is_whitespace() || next == '&' {
                break;
            }
            entity.push(next);
            chars.next();
        }

        match decode_entity(&entity) {
            Some(decoded) => out.push(decoded),
            None => {
                out.push('&');
                out.push_str(&entity);
            }
        }
    }
    out
}

fn decode_entity(entity: &str) -> Option<char> {
    match entity {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        "nbsp" => Some(' '),
        _ if entity.starts_with("#x") || entity.starts_with("#X") => {
            u32::from_str_radix(&entity[2..], 16)
                .ok()
                .and_then(char::from_u32)
        }
        _ if entity.starts_with('#') => entity[1..].parse::<u32>().ok().and_then(char::from_u32),
        _ => None,
    }
}

fn normalize_markdownish(input: &str) -> String {
    let mut lines = Vec::new();
    let mut blank_count = 0;
    let mut in_fence = false;

    for raw in input.lines() {
        let line = if in_fence {
            raw.trim_end().to_string()
        } else {
            raw.split_whitespace().collect::<Vec<_>>().join(" ")
        };

        if line.trim() == "```" {
            in_fence = !in_fence;
        }

        if line.trim().is_empty() {
            blank_count += 1;
            if blank_count <= 1 && !lines.is_empty() {
                lines.push(String::new());
            }
        } else {
            blank_count = 0;
            lines.push(line);
        }
    }

    lines.join("\n").trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    #[test]
    fn normalize_url_adds_https() {
        let url = normalize_url("example.com/docs").unwrap();
        assert_eq!(url.as_str(), "https://example.com/docs");
    }

    #[test]
    fn markdown_fallback_url_appends_md_before_query() {
        let url = normalize_url("https://example.com/docs/intro?lang=en").unwrap();
        let md = markdown_fallback_url(&url).unwrap();
        assert_eq!(md.as_str(), "https://example.com/docs/intro.md?lang=en");
    }

    #[test]
    fn llms_file_candidates_walks_to_origin_root() {
        let url = normalize_url("https://example.com/docs/organize/navigation?x=1").unwrap();
        let candidates = llms_file_candidates(&url, "llms.txt")
            .into_iter()
            .map(|url| url.to_string())
            .collect::<Vec<_>>();

        assert_eq!(
            candidates,
            vec![
                "https://example.com/docs/organize/navigation/llms.txt",
                "https://example.com/docs/organize/llms.txt",
                "https://example.com/docs/llms.txt",
                "https://example.com/llms.txt",
            ]
        );
    }

    #[test]
    fn html_to_markdownish_extracts_readable_text() {
        let html = r#"
          <html><head><title>Skip</title><style>.x{}</style></head>
          <body><main><h1>Docs &amp; API</h1><p>Hello <strong>world</strong>.</p><ul><li>One</li><li>Two</li></ul></main></body></html>
        "#;
        let text = html_to_markdownish(html);
        assert!(text.contains("# Docs & API"));
        assert!(text.contains("Hello world."));
        assert!(text.contains("- One"));
        assert!(!text.contains(".x{}"));
    }

    /// #503: `read --links` lists each link once, as an absolute URL, and
    /// skips what leads nowhere.
    #[test]
    fn collect_links_resolves_dedupes_and_skips_non_navigation() {
        let html = r##"<html><head><title>t</title></head><body>
          <a href="item?id=1">First <b>story</b></a>
          <a href='https://example.com/second'>Second story</a>
          <a href=../up/third.html>Third &amp; story</a>
          <a href="mailto:carol@example.com">carol</a>
          <a href="#top">fragment</a> <a href="javascript:void(0)">js</a> <a>no href</a>
          <a href="item?id=1">duplicate of first</a>
          <abbr title="x">not a link</abbr>
          <a href="/q?a=1&amp;b=2" aria-label="Query"></a>
          <a href="/img"><img src="x.png" alt="Logo"></a>
          <script>document.write('<a href="/hidden">x</a>')</script>
        </body></html>"##;
        let page = Url::parse("https://news.example.org/news/").unwrap();
        let links = collect_links(html, &page, 100, false);
        assert_eq!(
            links.shown,
            vec![
                (
                    "First story".to_string(),
                    "https://news.example.org/news/item?id=1".to_string()
                ),
                (
                    "Second story".to_string(),
                    "https://example.com/second".to_string()
                ),
                (
                    "Third & story".to_string(),
                    "https://news.example.org/up/third.html".to_string()
                ),
                ("carol".to_string(), "mailto:carol@example.com".to_string()),
                (
                    "Query".to_string(),
                    "https://news.example.org/q?a=1&b=2".to_string()
                ),
                (
                    "Logo".to_string(),
                    "https://news.example.org/img".to_string()
                ),
            ]
        );
        assert_eq!(links.total, 6);
    }

    #[test]
    fn collect_links_honours_base_href_and_the_cap() {
        let html = r#"<html><head><base href="https://cdn.example.com/docs/"></head><body>
          <a href="a">A</a><a href="b">B</a><a href="c">C</a></body></html>"#;
        let page = Url::parse("https://example.com/page").unwrap();
        let links = collect_links(html, &page, 2, false);
        assert_eq!(links.total, 3);
        assert_eq!(links.shown.len(), 2);
        assert_eq!(links.shown[0].1, "https://cdn.example.com/docs/a");
        let mut value = json!({ "content": "# Page" });
        attach_links(&mut value, &links);
        let content = value["content"].as_str().unwrap();
        assert!(content.contains("## Links"), "{content}");
        assert!(
            content.contains("- [A](https://cdn.example.com/docs/a)"),
            "{content}"
        );
        assert!(content.contains("(2 of 3 links shown"), "{content}");
        assert_eq!(value["linksTotalExact"], true);
        assert_eq!(value["linksTotal"], 3);
        assert_eq!(value["linksShown"], 2);
        assert_eq!(value["links"][1]["url"], "https://cdn.example.com/docs/b");
    }

    #[test]
    fn active_tab_read_appends_links_only_when_asked() {
        let html = r#"<html><body><p>Hi</p><a href="/x">X</a></body></html>"#;
        let plain =
            read_json_from_active_html("https://a.example/", html.into(), &ReadOptions::default());
        assert!(plain.get("links").is_none());
        let options = ReadOptions {
            links: Some(10),
            ..ReadOptions::default()
        };
        let with = read_json_from_active_html("https://a.example/", html.into(), &options);
        assert_eq!(with["links"][0]["url"], "https://a.example/x");
        assert!(with["content"]
            .as_str()
            .unwrap()
            .ends_with("- [X](https://a.example/x)"));
    }

    fn urls(links: &PageLinks) -> Vec<&str> {
        links.shown.iter().map(|(_, u)| u.as_str()).collect()
    }

    /// #503 review: a `>` inside a quoted attribute value does not end the
    /// start tag, so the href after it is still read.
    #[test]
    fn a_quoted_gt_does_not_end_the_tag() {
        let html = r#"<a title="1 > 0" href="/x">X</a><a data-x='a>b' href=/y>Y</a>"#;
        let page = Url::parse("https://a.example/").unwrap();
        let links = collect_links(html, &page, 10, false);
        assert_eq!(
            urls(&links),
            vec!["https://a.example/x", "https://a.example/y"]
        );
        assert_eq!(links.shown[0].0, "X");
    }

    /// #503 review: a `<base>` or `<a>` written inside a script string or a
    /// comment is not markup and changes nothing.
    #[test]
    fn fake_base_and_links_in_scripts_and_comments_are_ignored() {
        let html = r#"<html><head>
          <script>var s = '<base href="https://evil.example/">'; document.write('<a href="/in-script">s</a>');</script>
          <!-- <base href="https://comment.example/"> <a href="/in-comment">c</a> -->
          <style>a::after { content: "<a href='/in-style'>"; }</style>
          </head><body><a href="real">Real</a></body></html>"#;
        let page = Url::parse("https://a.example/dir/").unwrap();
        let links = collect_links(html, &page, 10, false);
        assert_eq!(urls(&links), vec!["https://a.example/dir/real"]);
        assert_eq!(links.total, 1);
    }

    /// #503 review: the document's base is its first `<base href>`, wherever
    /// it appears, and applies to links before it too.
    #[test]
    fn the_first_real_base_applies_to_every_link() {
        let html = r#"<body><a href="before">B</a>
          <base href="https://cdn.example/one/"><base href="https://cdn.example/two/">
          <a href="after">A</a></body>"#;
        let page = Url::parse("https://a.example/").unwrap();
        let links = collect_links(html, &page, 10, false);
        assert_eq!(
            urls(&links),
            vec![
                "https://cdn.example/one/before",
                "https://cdn.example/one/after"
            ]
        );
    }

    /// #503 review: an anchor's text ends where an element that was open
    /// around it closes, and at the next `<a>`.
    #[test]
    fn anchor_text_ends_at_the_enclosing_block() {
        let html = r#"<div><a href="/x">Alpha</div><p>after the block</p>
          <a href="/y">One<a href="/z">Two</a>"#;
        let page = Url::parse("https://a.example/").unwrap();
        let links = collect_links(html, &page, 10, false);
        let texts: Vec<&str> = links.shown.iter().map(|(t, _)| t.as_str()).collect();
        assert_eq!(texts, vec!["Alpha", "One", "Two"]);
    }

    /// #503 review: count, cap and omitted are reported, and the count is
    /// exact when no budget cut the scan.
    #[test]
    fn count_and_omitted_are_reported() {
        let long = format!("/{}", "p".repeat(LINK_URL_MAX_BYTES + 10));
        let html = format!(
            r#"<a href="/a">A</a><a href="/b">B</a><a href="/a">dup</a><a href="{long}">L</a>"#
        );
        let page = Url::parse("https://a.example/").unwrap();
        let links = collect_links(&html, &page, 1, false);
        assert_eq!(links.total, 2);
        assert!(links.total_exact);
        assert_eq!(links.shown.len(), 1);
        assert_eq!(links.omitted_too_long, 1);
        let mut value = json!({ "content": "x" });
        attach_links(&mut value, &links);
        let c = value["content"].as_str().unwrap();
        assert!(c.contains("(1 of 2 links shown"), "{c}");
        assert!(
            c.contains("1 links omitted: their URL is longer than"),
            "{c}"
        );
        assert_eq!(value["linksOmittedTooLong"], 1);
    }

    /// #503 review: a 1 MB base with 10,000 relative links must not build a
    /// 1 MB URL per link: they are omitted as too long, without joining, and
    /// said so. Bounded in time as a proxy for the allocations it skips.
    #[test]
    fn an_overlong_base_is_not_joined_10000_times() {
        let base = format!("https://a.example/{}/", "b".repeat(1024 * 1024));
        let mut html = format!(r#"<base href="{base}">"#);
        for i in 0..10_000 {
            html.push_str(&format!(r#"<a href="r{i}">{i}</a>"#));
        }
        let page = Url::parse("https://a.example/").unwrap();
        let started = std::time::Instant::now();
        let links = collect_links(&html, &page, 100, false);
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
        assert!(links.shown.is_empty());
        assert_eq!(links.total, 0);
        assert_eq!(links.omitted_too_long, 10_000);
    }

    /// #503 review: dedup storage, the anchor list and the output are all
    /// bounded, and a cut makes the total a lower bound, never "complete".
    #[test]
    fn budgets_bound_dedup_and_output_and_mark_the_total_as_a_lower_bound() {
        let mut html = String::new();
        for i in 0..(LINK_SEEN_MAX + 50) {
            html.push_str(&format!(r#"<a href="https://h.example/{i}">{i}</a>"#));
        }
        let page = Url::parse("https://a.example/").unwrap();
        let links = collect_links(&html, &page, 10, false);
        assert!(!links.total_exact);
        assert!(
            links.budgets_hit.contains(&"dedup"),
            "{:?}",
            links.budgets_hit
        );
        assert_eq!(links.total, LINK_SEEN_MAX);
        let mut value = json!({ "content": "x" });
        attach_links(&mut value, &links);
        let c = value["content"].as_str().unwrap();
        assert!(
            c.contains(&format!("of at least {LINK_SEEN_MAX} links shown")),
            "{c}"
        );
        assert!(c.contains("the count is a lower bound"), "{c}");

        // 1000 links of ~1.9 KB each: the output budget stops the list.
        let mut html = String::new();
        for i in 0..1000 {
            html.push_str(&format!(
                r#"<a href="https://h.example/{i}/{}">x</a>"#,
                "q".repeat(1900)
            ));
        }
        let links = collect_links(&html, &page, MAX_MAX_LINKS, false);
        let bytes: usize = links.shown.iter().map(|(t, u)| t.len() + u.len() + 8).sum();
        assert!(bytes <= LINK_OUTPUT_MAX_BYTES, "{bytes}");
        assert!(links.shown.len() < 1000);
        assert!(links.budgets_hit.contains(&"output"));
        assert_eq!(links.total, 1000);
        assert!(links.total_exact);
    }

    /// #503 review 3: the base href is entity-decoded like every href.
    #[test]
    fn the_base_href_is_entity_decoded() {
        let html = r#"<base href="https://a.example/a&amp;b/"><a href="x">X</a>"#;
        let page = Url::parse("https://p.example/").unwrap();
        let links = collect_links(html, &page, 10, false);
        assert_eq!(urls(&links), vec!["https://a.example/a&b/x"]);
    }

    /// #503 review 3: `</scriptx>` does not end a script, and `<script/>` /
    /// `<style/>` still open raw text (HTML ignores the self-closing flag on
    /// non-void elements). A fake base or anchor inside must not count.
    #[test]
    fn raw_text_ends_only_at_its_exact_end_tag() {
        let html = r#"<script>var a = '</scriptx><base href="https://evil.example/"><a href="/fake1">f</a>';</script>
          <script/><base href="https://evil2.example/"><a href="/fake2">f</a></script >
          <style/>a::before{content:'<a href="/fake3">'}</style>
          <style>x{}</stylex><a href="/fake4">f</a></style>
          <a href="real">Real</a>"#;
        let page = Url::parse("https://a.example/d/").unwrap();
        let links = collect_links(html, &page, 10, false);
        assert_eq!(urls(&links), vec!["https://a.example/d/real"]);
    }

    /// #503 review 3: a cut inside a quoted href (a budget or body limit) is
    /// never taken as a complete destination, and an input cut makes the
    /// count a lower bound.
    #[test]
    fn a_cut_inside_an_href_is_dropped_and_the_count_is_a_lower_bound() {
        let html = r#"<a href="/ok">OK</a><a href="/target-full"#;
        let page = Url::parse("https://a.example/").unwrap();
        let links = collect_links(html, &page, 10, false);
        assert_eq!(urls(&links), vec!["https://a.example/ok"]);
        let cut = collect_links(html, &page, 10, true);
        assert_eq!(urls(&cut), vec!["https://a.example/ok"]);
        assert!(!cut.total_exact);
        assert!(cut.budgets_hit.contains(&"input"));
        // An unquoted value cut at the end, and a tag cut before its `>`.
        for html in [
            r#"<a href=/ok>OK</a><a href=/targ"#,
            r#"<a href="/ok">OK</a><a href="/x" "#,
        ] {
            let links = collect_links(html, &page, 10, false);
            assert_eq!(urls(&links), vec!["https://a.example/ok"], "{html}");
        }
    }

    #[test]
    fn max_links_is_bounded() {
        assert_eq!(parse_max_links("50").unwrap(), 50);
        assert!(parse_max_links("0").is_err());
        assert!(parse_max_links("1001").is_err());
        assert!(parse_max_links("many").is_err());
    }

    #[test]
    fn html_to_markdownish_keeps_header_after_head() {
        let html = r#"
          <html><head><title>Skip</title></head><body>
          <header><a href="/">Home</a></header>
          <article><h1>agent-browser</h1><p>Browser automation CLI.</p></article>
          </body></html>
        "#;
        let text = html_to_markdownish(html);
        assert!(text.contains("Home"));
        assert!(text.contains("# agent-browser"));
        assert!(text.contains("Browser automation CLI."));
    }

    #[test]
    fn format_llms_index_uses_list_links_and_dedupes() {
        let body = r#"
# Docs

Inline [Authentication](/inline-auth) should not become a TOC item.

- [Authentication](/docs/auth)
- [Authentication](/docs/auth)
  - [Channels](/docs/channels)
1. [Introduction](/docs/introduction)
"#;
        let content =
            format_llms_index(body, "https://example.com/llms.txt", Some("auth")).unwrap();

        assert!(content.contains("[Authentication](https://example.com/docs/auth)"));
        assert!(!content.contains("inline-auth"));
        assert!(!content.contains("Introduction"));
        assert_eq!(content.matches("Authentication").count(), 1);
    }

    #[test]
    fn format_page_outline_extracts_and_filters_headings() {
        let content = "# Intro\n\nWelcome\n\n## Install\n\n### Token auth\n\n## Usage\n";
        let outline = format_page_outline(content, "https://example.com/docs", Some("auth"));

        assert!(outline.contains("# Outline"));
        assert!(outline.contains("Source: https://example.com/docs"));
        assert!(outline.contains("    - Token auth"));
        assert!(!outline.contains("Install"));
        assert!(!outline.contains("Usage"));
    }

    #[test]
    fn filter_page_sections_prefers_matching_headings() {
        let content = "# Guide\n\nIntro.\n\n## Setup\n\nInstall.\n\n## Response rendering\n\nRender JSON.\n\n### Custom renderer\n\nUse a component.\n\n## Further reading\n\nNext.";
        let filtered = filter_page_sections(content, "Response rendering");

        assert!(filtered.contains("## Response rendering"));
        assert!(filtered.contains("Render JSON."));
        assert!(filtered.contains("### Custom renderer"));
        assert!(filtered.contains("Use a component."));
        assert!(!filtered.contains("## Setup"));
        assert!(!filtered.contains("## Further reading"));
    }

    #[test]
    fn options_from_command_includes_headers_and_allowed_domains() {
        let cmd = json!({
            "action": "read",
            "timeout": 2500,
            "headers": {
                "Authorization": "Bearer token",
                "X-Trace": "abc"
            },
            "allowedDomains": ["example.com", "*.example.org"]
        });

        let options = options_from_command(&cmd).unwrap();

        assert_eq!(options.timeout_ms, 2500);
        assert_eq!(
            options.headers.get("Authorization").map(String::as_str),
            Some("Bearer token")
        );
        assert_eq!(
            options.headers.get("X-Trace").map(String::as_str),
            Some("abc")
        );
        assert_eq!(
            options.allowed_domains,
            vec!["example.com".to_string(), "*.example.org".to_string()]
        );
    }

    #[test]
    fn read_json_from_active_html_uses_current_dom() {
        let options = ReadOptions {
            filter: Some("Account".to_string()),
            ..ReadOptions::default()
        };
        let html = "<html><body><h1>Home</h1><p>Welcome.</p><h2>Account</h2><p>Signed in.</p></body></html>";

        let data =
            read_json_from_active_html("https://example.com/app", html.to_string(), &options);

        assert_eq!(data["source"], "active-tab-html-filtered");
        assert_eq!(data["finalUrl"], "https://example.com/app");
        let content = data["content"].as_str().unwrap();
        assert!(content.contains("## Account"));
        assert!(content.contains("Signed in."));
        assert!(!content.contains("# Home"));
    }

    #[test]
    fn content_from_fetch_require_md_checks_raw_response() {
        let fetch = ReadFetch {
            final_url: "https://example.com".to_string(),
            status: 200,
            content_type: "text/html; charset=utf-8".to_string(),
            success: true,
            body: "<h1>HTML</h1>".to_string(),
            truncated: false,
        };
        let options = ReadOptions {
            raw: true,
            require_md: true,
            ..ReadOptions::default()
        };

        let err = content_from_fetch(&fetch, &options).unwrap_err();
        assert_eq!(err, "Expected text/markdown, got text/html");
    }

    #[tokio::test]
    async fn run_read_blocks_disallowed_initial_url() {
        let options = ReadOptions {
            allowed_domains: vec!["example.com".to_string()],
            ..ReadOptions::default()
        };

        let err = run_read("https://not-example.com/docs", options)
            .await
            .unwrap_err();

        assert!(err.contains("not-example.com"));
        assert!(err.contains("allowed domains"));
    }

    #[tokio::test]
    async fn run_read_blocks_disallowed_redirect() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let base = format!("http://{}", addr);

        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = [0_u8; 2048];
            let _ = stream.read(&mut buf).await.unwrap_or(0);
            let response = "HTTP/1.1 302 Found\r\nLocation: https://example.com/docs\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
            let _ = stream.write_all(response.as_bytes()).await;
        });

        let options = ReadOptions {
            allowed_domains: vec!["127.0.0.1".to_string()],
            ..ReadOptions::default()
        };
        let err = run_read(&base, options).await.unwrap_err();

        assert!(err.contains("example.com"));
        assert!(err.contains("allowed domains"));
    }

    #[tokio::test]
    async fn run_read_blocks_enforced_disallowed_initial_url() {
        let options = ReadOptions {
            enforced_allowed_domains: vec![vec!["example.com".to_string()]],
            ..ReadOptions::default()
        };

        let err = run_read("https://not-example.com/docs", options)
            .await
            .unwrap_err();

        assert!(err.contains("not-example.com"));
        assert!(err.contains("allowed domains"));
    }

    #[tokio::test]
    async fn run_read_blocks_enforced_disallowed_redirect() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let base = format!("http://{}", addr);

        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = [0_u8; 2048];
            let _ = stream.read(&mut buf).await.unwrap_or(0);
            let response = "HTTP/1.1 302 Found\r\nLocation: https://example.com/docs\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
            let _ = stream.write_all(response.as_bytes()).await;
        });

        let options = ReadOptions {
            allowed_domains: vec!["127.0.0.1".to_string(), "example.com".to_string()],
            enforced_allowed_domains: vec![vec!["127.0.0.1".to_string()]],
            ..ReadOptions::default()
        };
        let err = run_read(&base, options).await.unwrap_err();

        assert!(err.contains("example.com"));
        assert!(err.contains("allowed domains"));
    }

    #[test]
    fn check_allowed_url_matches_wildcard_like_domain_filter() {
        let root = normalize_url("https://example.com/docs").unwrap();
        let subdomain = normalize_url("https://api.example.com/docs").unwrap();
        let other = normalize_url("https://badexample.com/docs").unwrap();
        let allowed = vec!["*.example.com".to_string()];

        assert!(check_allowed_url(&root, &allowed).is_ok());
        assert!(check_allowed_url(&subdomain, &allowed).is_ok());
        assert!(check_allowed_url(&other, &allowed).is_err());
    }

    #[test]
    fn check_allowed_active_url_blocks_disallowed_active_tab() {
        let options = ReadOptions {
            allowed_domains: vec!["example.com".to_string()],
            ..ReadOptions::default()
        };

        let err = check_allowed_active_url_for_options("https://evil.example/docs", &options)
            .unwrap_err();

        assert!(err.contains("evil.example"));
        assert!(err.contains("allowed domains"));
    }

    #[test]
    fn check_allowed_active_url_blocks_non_http_active_tab_when_filter_enabled() {
        let options = ReadOptions {
            allowed_domains: vec!["example.com".to_string()],
            ..ReadOptions::default()
        };

        let err = check_allowed_active_url_for_options("about:blank", &options).unwrap_err();

        assert!(err.contains("about"));
        assert!(err.contains("domain filter"));
    }

    #[test]
    fn check_allowed_active_url_for_options_uses_enforced_domains() {
        let options = ReadOptions {
            enforced_allowed_domains: vec![vec!["example.com".to_string()]],
            ..ReadOptions::default()
        };

        let err = check_allowed_active_url_for_options("https://evil.example/docs", &options)
            .unwrap_err();

        assert!(err.contains("evil.example"));
        assert!(err.contains("allowed domains"));
    }

    #[tokio::test]
    async fn run_read_prefers_markdown_accept() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let base = format!("http://{}", addr);

        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = [0_u8; 2048];
            let n = stream.read(&mut buf).await.unwrap_or(0);
            let request = String::from_utf8_lossy(&buf[..n]);
            assert!(request
                .to_ascii_lowercase()
                .contains("accept: text/markdown"));
            let body = "# Markdown\n";
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/markdown\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = stream.write_all(response.as_bytes()).await;
        });

        let data = run_read(&base, ReadOptions::default()).await.unwrap();
        assert_eq!(data["source"], "accept-markdown");
        assert_eq!(data["content"], "# Markdown\n");
    }

    #[tokio::test]
    async fn run_read_tries_md_suffix_after_html() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let base = format!("http://{}/docs/intro", addr);

        tokio::spawn(async move {
            for expected_path in ["/docs/intro", "/docs/intro.md"] {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut buf = [0_u8; 2048];
                let n = stream.read(&mut buf).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&buf[..n]).to_ascii_lowercase();
                assert!(request.starts_with(&format!("get {} ", expected_path)));
                assert!(request.contains("accept: text/markdown"));
                let (content_type, body) = if expected_path.ends_with(".md") {
                    ("text/plain", "# Markdown fallback\n")
                } else {
                    ("text/html", "<h1>HTML</h1>")
                };
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    content_type,
                    body.len(),
                    body
                );
                let _ = stream.write_all(response.as_bytes()).await;
            }
        });

        let data = run_read(&base, ReadOptions::default()).await.unwrap();
        assert_eq!(data["url"], base);
        assert_eq!(data["finalUrl"], format!("{}.md", base));
        assert_eq!(data["source"], "path-markdown");
        assert_eq!(data["content"], "# Markdown fallback\n");
    }

    #[tokio::test]
    async fn run_read_returns_primary_markdown_without_llms_override() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let base = format!("http://{}/docs/intro", addr);

        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    break;
                };
                let mut buf = [0_u8; 2048];
                let n = stream.read(&mut buf).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&buf[..n]).to_ascii_lowercase();
                let path = request
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or_default()
                    .to_string();
                let (status, content_type, body) = match path.as_str() {
                    "/docs/intro" => ("200 OK", "text/markdown", "# Primary markdown\n"),
                    "/docs/intro/llms.txt" => ("404 Not Found", "text/plain", "missing"),
                    "/docs/llms.txt" => {
                        ("200 OK", "text/markdown", "- [Intro](/markdown/intro.md)\n")
                    }
                    "/markdown/intro.md" => ("200 OK", "text/markdown", "# From llms\n"),
                    _ => ("404 Not Found", "text/plain", "missing"),
                };
                let response = format!(
                    "HTTP/1.1 {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    status,
                    content_type,
                    body.len(),
                    body
                );
                let _ = stream.write_all(response.as_bytes()).await;
            }
        });

        let data = run_read(&base, ReadOptions::default()).await.unwrap();

        assert_eq!(data["source"], "accept-markdown");
        assert_eq!(data["content"], "# Primary markdown\n");
    }

    #[tokio::test]
    async fn run_read_uses_llms_link_after_direct_fallbacks() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let base = format!("http://{}/docs/intro", addr);

        tokio::spawn(async move {
            for expected_path in [
                "/docs/intro",
                "/docs/intro.md",
                "/docs/intro/llms.txt",
                "/docs/llms.txt",
                "/markdown/intro.md",
            ] {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut buf = [0_u8; 2048];
                let n = stream.read(&mut buf).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&buf[..n]).to_ascii_lowercase();
                assert!(request.starts_with(&format!("get {} ", expected_path)));
                let (status, content_type, body) = match expected_path {
                    "/docs/intro" => ("200 OK", "text/html", "<h1>HTML</h1>"),
                    "/docs/intro.md" => ("404 Not Found", "text/html", "missing"),
                    "/docs/intro/llms.txt" => ("404 Not Found", "text/html", "missing"),
                    "/docs/llms.txt" => {
                        ("200 OK", "text/markdown", "- [Intro](/markdown/intro.md)\n")
                    }
                    "/markdown/intro.md" => ("200 OK", "text/markdown", "# Intro via llms\n"),
                    _ => unreachable!(),
                };
                let response = format!(
                    "HTTP/1.1 {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    status,
                    content_type,
                    body.len(),
                    body
                );
                let _ = stream.write_all(response.as_bytes()).await;
            }
        });

        let data = run_read(&base, ReadOptions::default()).await.unwrap();
        assert_eq!(data["source"], "llms-link");
        assert_eq!(
            data["finalUrl"],
            format!("http://{}/markdown/intro.md", addr)
        );
        assert_eq!(data["content"], "# Intro via llms\n");
    }

    #[tokio::test]
    async fn run_read_llms_index_filters_links() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let base = format!("http://{}/docs/intro", addr);

        tokio::spawn(async move {
            for expected_path in ["/docs/intro/llms.txt", "/docs/llms.txt"] {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut buf = [0_u8; 2048];
                let n = stream.read(&mut buf).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&buf[..n]).to_ascii_lowercase();
                assert!(request.starts_with(&format!("get {} ", expected_path)));
                let (status, content_type, body) = if expected_path == "/docs/llms.txt" {
                    (
                        "200 OK",
                        "text/markdown",
                        "- [Intro](/docs/intro)\n- [Authentication](/docs/auth)\n",
                    )
                } else {
                    ("200 OK", "text/html", "<h1>Not docs</h1>")
                };
                let response = format!(
                    "HTTP/1.1 {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    status,
                    content_type,
                    body.len(),
                    body
                );
                let _ = stream.write_all(response.as_bytes()).await;
            }
        });

        let options = ReadOptions {
            llms: Some(LlmsMode::Index),
            filter: Some("auth".to_string()),
            ..ReadOptions::default()
        };
        let data = run_read(&base, options).await.unwrap();
        let content = data["content"].as_str().unwrap();
        assert_eq!(data["source"], "llms-index");
        assert_eq!(data["finalUrl"], format!("http://{}/docs/llms.txt", addr));
        assert!(content.contains("Authentication"));
        assert!(!content.contains("Intro]"));
    }

    #[tokio::test]
    async fn run_read_llms_full_filters_sections() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let base = format!("http://{}/docs/intro", addr);

        tokio::spawn(async move {
            for expected_path in ["/docs/intro/llms-full.txt", "/docs/llms-full.txt"] {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut buf = [0_u8; 2048];
                let n = stream.read(&mut buf).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&buf[..n]).to_ascii_lowercase();
                assert!(request.starts_with(&format!("get {} ", expected_path)));
                let (status, content_type, body) = if expected_path == "/docs/llms-full.txt" {
                    (
                        "200 OK",
                        "text/markdown",
                        "# Intro\nWelcome.\n\n## Auth\nUse token auth.\n\n## Other\nNo match.\n",
                    )
                } else {
                    ("404 Not Found", "text/html", "missing")
                };
                let response = format!(
                    "HTTP/1.1 {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    status,
                    content_type,
                    body.len(),
                    body
                );
                let _ = stream.write_all(response.as_bytes()).await;
            }
        });

        let options = ReadOptions {
            llms: Some(LlmsMode::Full),
            filter: Some("token".to_string()),
            ..ReadOptions::default()
        };
        let data = run_read(&base, options).await.unwrap();
        let content = data["content"].as_str().unwrap();
        assert_eq!(data["source"], "llms-full");
        assert_eq!(
            data["finalUrl"],
            format!("http://{}/docs/llms-full.txt", addr)
        );
        assert!(content.contains("## Auth"));
        assert!(!content.contains("# Intro"));
        assert!(!content.contains("## Other"));
    }

    #[tokio::test]
    async fn run_read_llms_full_require_md_rejects_text_plain() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let base = format!("http://{}/docs/intro", addr);

        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = [0_u8; 2048];
            let n = stream.read(&mut buf).await.unwrap_or(0);
            let request = String::from_utf8_lossy(&buf[..n]).to_ascii_lowercase();
            assert!(request.starts_with("get /docs/intro/llms-full.txt "));
            let body = "# Full docs\n";
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = stream.write_all(response.as_bytes()).await;
        });

        let options = ReadOptions {
            llms: Some(LlmsMode::Full),
            require_md: true,
            ..ReadOptions::default()
        };
        let err = run_read(&base, options).await.unwrap_err();
        assert_eq!(err, "Expected text/markdown, got text/plain");
    }

    #[tokio::test]
    async fn run_read_outline_extracts_selected_page_headings() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let base = format!("http://{}/docs/intro", addr);

        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = [0_u8; 2048];
            let n = stream.read(&mut buf).await.unwrap_or(0);
            let request = String::from_utf8_lossy(&buf[..n]).to_ascii_lowercase();
            assert!(request.starts_with("get /docs/intro "));
            let body = "# Intro\n\n## Install\n\n### Token auth\n\n## Usage\n";
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/markdown\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = stream.write_all(response.as_bytes()).await;
        });

        let options = ReadOptions {
            outline: true,
            filter: Some("auth".to_string()),
            ..ReadOptions::default()
        };
        let data = run_read(&base, options).await.unwrap();
        let content = data["content"].as_str().unwrap();
        assert_eq!(data["source"], "accept-markdown-outline");
        assert!(content.contains("    - Token auth"));
        assert!(!content.contains("Install"));
        assert!(!content.contains("Usage"));
    }

    #[tokio::test]
    async fn run_read_filter_extracts_selected_page_section() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let base = format!("http://{}/docs/intro", addr);

        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = [0_u8; 2048];
            let n = stream.read(&mut buf).await.unwrap_or(0);
            let request = String::from_utf8_lossy(&buf[..n]).to_ascii_lowercase();
            assert!(request.starts_with("get /docs/intro "));
            let body = "# Intro\n\n## Setup\n\nInstall.\n\n## Response rendering\n\nRender JSON.\n\n### Custom renderer\n\nUse a component.\n\n## Further reading\n\nNext.\n";
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/markdown\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = stream.write_all(response.as_bytes()).await;
        });

        let options = ReadOptions {
            filter: Some("Response rendering".to_string()),
            ..ReadOptions::default()
        };
        let data = run_read(&base, options).await.unwrap();
        let content = data["content"].as_str().unwrap();
        assert_eq!(data["source"], "accept-markdown-filtered");
        assert!(content.contains("## Response rendering"));
        assert!(content.contains("### Custom renderer"));
        assert!(!content.contains("## Setup"));
        assert!(!content.contains("## Further reading"));
    }

    #[tokio::test]
    async fn run_read_allows_accept_override() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let base = format!("http://{}", addr);

        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = [0_u8; 2048];
            let n = stream.read(&mut buf).await.unwrap_or(0);
            let request = String::from_utf8_lossy(&buf[..n]).to_ascii_lowercase();
            assert!(request.contains("accept: application/json"));
            assert!(!request.contains("accept: text/markdown"));
            let body = "{\"ok\":true}\n";
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = stream.write_all(response.as_bytes()).await;
        });

        let mut options = ReadOptions::default();
        options
            .headers
            .insert("Accept".to_string(), "application/json".to_string());
        let data = run_read(&base, options).await.unwrap();
        assert_eq!(data["source"], "raw");
        assert_eq!(data["content"], "{\"ok\":true}\n");
    }

    // --- client-rendered pages (#255) --------------------------------------

    /// The whole point: an app shell must not come back as an empty answer.
    #[test]
    fn a_nuxt_shell_with_no_text_is_a_refusal_not_an_empty_page() {
        let html = r#"<!doctype html><html><head><title>x</title></head>
            <body><div id="__nuxt"></div><script src="/_nuxt/entry.js"></script></body></html>"#;
        let content = html_to_markdownish(html);
        match app_shell_verdict(html, &content) {
            Some(AppShell::Nothing(why)) => assert!(why.contains("Nuxt"), "{why}"),
            other => panic!(
                "expected Nothing, got {}",
                match other {
                    Some(AppShell::Little(w)) => format!("Little({w})"),
                    _ => "None".to_string(),
                }
            ),
        }
    }

    /// A title and a "please enable JavaScript" line is not the page either.
    #[test]
    fn a_shell_with_a_noscript_notice_is_flagged_not_trusted() {
        let html = r#"<html><body><div id="root"><h1>Loading…</h1></div>
            <noscript>You need to enable JavaScript to run this app.</noscript>
            <script src="/static/js/main.js"></script></body></html>"#;
        let content = html_to_markdownish(html);
        assert!(
            !content.trim().is_empty(),
            "the extractor dropped the placeholder text: {content:?}"
        );
        assert!(matches!(
            app_shell_verdict(html, &content),
            Some(AppShell::Little(_))
        ));
        // No placeholder at all: the extractor may drop <noscript>, and then
        // this is the empty case — still a refusal, never a pass.
        let bare = r#"<html><body><div id="root"></div>
            <noscript>enable JavaScript</noscript><script src="/x.js"></script></body></html>"#;
        assert!(app_shell_verdict(bare, &html_to_markdownish(bare)).is_some());
    }

    /// Server-rendered pages carry scripts too; text is what decides.
    #[test]
    fn a_rendered_page_with_scripts_is_left_alone() {
        let body = "<p>".to_string() + &"real content here. ".repeat(20) + "</p>";
        let html = format!(
            r#"<html><body><div id="app">{body}</div><script>init()</script></body></html>"#
        );
        let content = html_to_markdownish(&html);
        assert!(app_shell_verdict(&html, &content).is_none());
    }

    /// No scripts means no rendering step was skipped: an empty page is empty.
    #[test]
    fn an_empty_static_page_is_just_empty() {
        let html = "<html><body></body></html>";
        assert!(app_shell_verdict(html, "").is_none());
    }
}

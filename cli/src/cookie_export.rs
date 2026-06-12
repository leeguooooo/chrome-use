//! Offline export of a Chrome profile's cookies.
//!
//! Reads a profile's on-disk cookie store, decrypts the values with the OS
//! credential-store key, and returns CDP `Network.setCookie`-shaped objects —
//! the same shape `cookies set --curl` accepts. This is what powers
//! `cookies transfer`: it moves a logged-in session (whose auth cookies are
//! httpOnly + secure and span several hosts) from one profile to another
//! without the source profile being reachable over CDP, and without restarting
//! Chrome.
//!
//! Currently macOS-only. There, value encryption uses the `v10` scheme:
//! AES-128-CBC with a key derived (PBKDF2-HMAC-SHA1, 1003 iterations) from the
//! "Chrome Safe Storage" Keychain entry, shared by every profile of one Chrome
//! install. Other platforms return a clear error.

use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// Resolve, read, and decrypt a Chrome profile's cookies.
///
/// `profile` accepts a directory name ("Default", "Profile 14"), a display name
/// ("Davian", case-insensitive), or "auto" (last-used profile). `domain`, when
/// set, is a comma-separated host-suffix filter (e.g. "claude.ai,anthropic.com")
/// matched against `host_key`; pass `None` to export every cookie.
pub fn export_cookies(profile: &str, domain: Option<&str>) -> Result<Vec<Value>, String> {
    let db = resolve_cookie_db(profile)?;
    let rows = read_cookie_rows(&db, domain)?;
    let key = safe_storage_key()?;
    let mut out = Vec::with_capacity(rows.len());
    for r in &rows {
        if let Some(value) = decrypt_value(&r.encrypted_value, &key) {
            out.push(to_cdp_cookie(r, value));
        }
    }
    Ok(out)
}

fn resolve_cookie_db(profile: &str) -> Result<PathBuf, String> {
    use crate::native::cdp::chrome::{find_chrome_user_data_dir, resolve_chrome_profile};
    let udd = find_chrome_user_data_dir()
        .ok_or_else(|| "No Chrome user data directory found".to_string())?;
    let dir = resolve_chrome_profile(&udd, profile)?;
    let base = udd.join(&dir);
    // Chrome >=96 keeps cookies under Network/; older builds at the profile root.
    let net = base.join("Network").join("Cookies");
    if net.is_file() {
        return Ok(net);
    }
    let root = base.join("Cookies");
    if root.is_file() {
        return Ok(root);
    }
    Err(format!(
        "no cookie store found for profile \"{}\" (looked in {} and {})",
        profile,
        net.display(),
        root.display()
    ))
}

struct CookieRow {
    host_key: String,
    name: String,
    encrypted_value: Vec<u8>,
    path: String,
    is_secure: bool,
    is_httponly: bool,
    samesite: i64,
    expires_utc: i64,
}

/// Removes a temp directory when dropped.
struct TempGuard(PathBuf);
impl Drop for TempGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn read_cookie_rows(db: &Path, domain: Option<&str>) -> Result<Vec<CookieRow>, String> {
    // Copy the store (plus any -wal/-shm) to a temp file so a running Chrome's
    // lock / hot journal can't block the read or be disturbed by it.
    let tmp_dir = std::env::temp_dir().join(format!("chrome-use-cookies-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&tmp_dir).map_err(|e| format!("temp dir: {}", e))?;
    let _guard = TempGuard(tmp_dir.clone());
    let tmp_db = tmp_dir.join("Cookies");
    copy_db(db, &tmp_db)?;

    let where_clause = build_where(domain)?;
    let sql = format!(
        "SELECT json_group_array(json_object(\
           'h',host_key,'n',name,'e',hex(encrypted_value),'p',path,\
           'sec',is_secure,'ho',is_httponly,'ss',samesite,'x',expires_utc)) \
         FROM cookies{};",
        where_clause
    );
    let output = std::process::Command::new("sqlite3")
        .arg(tmp_db.to_string_lossy().to_string())
        .arg(&sql)
        .output()
        .map_err(|e| {
            format!(
                "could not run sqlite3 (required to read the cookie store): {}",
                e
            )
        })?;
    if !output.status.success() {
        return Err(format!(
            "sqlite3 failed reading the cookie store: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let trimmed = stdout.trim();
    if trimmed.is_empty() || trimmed == "null" {
        return Ok(Vec::new());
    }
    let arr: Vec<Value> =
        serde_json::from_str(trimmed).map_err(|e| format!("parsing cookie rows: {}", e))?;
    let mut rows = Vec::with_capacity(arr.len());
    for v in arr {
        let enc_hex = v.get("e").and_then(|x| x.as_str()).unwrap_or("");
        let path = v.get("p").and_then(|x| x.as_str()).unwrap_or("/");
        rows.push(CookieRow {
            host_key: v
                .get("h")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string(),
            name: v
                .get("n")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string(),
            encrypted_value: hex::decode(enc_hex).unwrap_or_default(),
            path: if path.is_empty() {
                "/".to_string()
            } else {
                path.to_string()
            },
            is_secure: v.get("sec").and_then(|x| x.as_i64()).unwrap_or(0) != 0,
            is_httponly: v.get("ho").and_then(|x| x.as_i64()).unwrap_or(0) != 0,
            samesite: v.get("ss").and_then(|x| x.as_i64()).unwrap_or(-1),
            expires_utc: v.get("x").and_then(|x| x.as_i64()).unwrap_or(0),
        });
    }
    Ok(rows)
}

fn copy_db(src: &Path, dst: &Path) -> Result<(), String> {
    std::fs::copy(src, dst).map_err(|e| format!("copying cookie store: {}", e))?;
    for suffix in ["-wal", "-shm"] {
        let s = path_with_suffix(src, suffix);
        if s.is_file() {
            let _ = std::fs::copy(&s, path_with_suffix(dst, suffix));
        }
    }
    Ok(())
}

fn path_with_suffix(p: &Path, suffix: &str) -> PathBuf {
    let mut s = p.as_os_str().to_os_string();
    s.push(suffix);
    PathBuf::from(s)
}

/// Build a `WHERE host_key LIKE '%domain'` clause from a comma-separated filter.
/// Domains are validated (alnum/./-) so they can be inlined without injection.
fn build_where(domain: Option<&str>) -> Result<String, String> {
    let Some(domain) = domain else {
        return Ok(String::new());
    };
    let mut clauses = Vec::new();
    for d in domain.split(',') {
        let d = d.trim();
        if d.is_empty() {
            continue;
        }
        if !d
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
        {
            return Err(format!("invalid domain filter \"{}\"", d));
        }
        clauses.push(format!("host_key LIKE '%{}'", d));
    }
    if clauses.is_empty() {
        Ok(String::new())
    } else {
        Ok(format!(" WHERE {}", clauses.join(" OR ")))
    }
}

fn to_cdp_cookie(r: &CookieRow, value: String) -> Value {
    let mut o = serde_json::Map::new();
    o.insert("name".into(), json!(r.name));
    o.insert("value".into(), json!(value));
    o.insert("domain".into(), json!(r.host_key));
    o.insert("path".into(), json!(r.path));
    o.insert("secure".into(), json!(r.is_secure));
    o.insert("httpOnly".into(), json!(r.is_httponly));
    // Chrome SameSite: -1 unspecified, 0 None, 1 Lax, 2 Strict.
    let same_site = match r.samesite {
        0 => Some("None"),
        1 => Some("Lax"),
        2 => Some("Strict"),
        _ => None,
    };
    if let Some(ss) = same_site {
        // CDP rejects SameSite=None without Secure; downgrade rather than fail.
        if ss == "None" && !r.is_secure {
            o.insert("sameSite".into(), json!("Lax"));
        } else {
            o.insert("sameSite".into(), json!(ss));
        }
    }
    if let Some(unix) = chrome_epoch_to_unix(r.expires_utc) {
        o.insert("expires".into(), json!(unix));
    }
    Value::Object(o)
}

/// Chrome stores `expires_utc` as microseconds since 1601-01-01 (0 = session
/// cookie). CDP wants seconds since the Unix epoch. Returns None for session
/// cookies and anything that converts to a non-positive time.
fn chrome_epoch_to_unix(expires_utc: i64) -> Option<f64> {
    if expires_utc <= 0 {
        return None;
    }
    let unix = expires_utc as f64 / 1_000_000.0 - 11_644_473_600.0;
    if unix > 0.0 {
        Some(unix)
    } else {
        None
    }
}

/// Decrypt a Chrome `v10` cookie value (AES-128-CBC, IV = 16 spaces, PKCS7).
/// Returns None for unrecognized schemes or undecryptable values.
fn decrypt_value(enc: &[u8], key: &[u8; 16]) -> Option<String> {
    if enc.len() < 3 || &enc[0..3] != b"v10" {
        return None;
    }
    use aes::cipher::{block_padding::Pkcs7, BlockDecryptMut, KeyIvInit};
    type Dec = cbc::Decryptor<aes::Aes128>;
    let iv = [0x20u8; 16];
    let mut buf = enc[3..].to_vec();
    let pt = Dec::new(key.into(), &iv.into())
        .decrypt_padded_mut::<Pkcs7>(&mut buf)
        .ok()?;
    // Chrome >=24 prepends a 32-byte SHA256(host) domain hash to the plaintext.
    match std::str::from_utf8(pt) {
        Ok(s) => Some(s.to_string()),
        Err(_) if pt.len() > 32 => Some(String::from_utf8_lossy(&pt[32..]).into_owned()),
        Err(_) => None,
    }
}

#[cfg(target_os = "macos")]
fn safe_storage_key() -> Result<[u8; 16], String> {
    use pbkdf2::pbkdf2_hmac;
    use sha1::Sha1;
    let out = std::process::Command::new("security")
        .args(["find-generic-password", "-ws", "Chrome Safe Storage"])
        .output()
        .map_err(|e| format!("could not read Keychain (security command): {}", e))?;
    if !out.status.success() {
        return Err(
            "could not read the 'Chrome Safe Storage' key from Keychain \
             (you may be prompted to allow access — approve it and retry)"
                .to_string(),
        );
    }
    let pw = String::from_utf8_lossy(&out.stdout);
    let pw = pw.trim_end_matches('\n');
    let mut key = [0u8; 16];
    pbkdf2_hmac::<Sha1>(pw.as_bytes(), b"saltysalt", 1003, &mut key);
    Ok(key)
}

#[cfg(not(target_os = "macos"))]
fn safe_storage_key() -> Result<[u8; 16], String> {
    Err("cookies export/transfer is currently supported on macOS only".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn where_clause_filters_and_validates() {
        assert_eq!(build_where(None).unwrap(), "");
        assert_eq!(
            build_where(Some("claude.ai")).unwrap(),
            " WHERE host_key LIKE '%claude.ai'"
        );
        assert_eq!(
            build_where(Some("claude.ai, anthropic.com")).unwrap(),
            " WHERE host_key LIKE '%claude.ai' OR host_key LIKE '%anthropic.com'"
        );
        assert!(build_where(Some("evil' OR 1=1 --")).is_err());
    }

    #[test]
    fn epoch_conversion() {
        assert_eq!(chrome_epoch_to_unix(0), None);
        assert_eq!(chrome_epoch_to_unix(-5), None);
        // 13380163200000000 us since 1601 == 2025-01-01T00:00:00Z (1735689600 unix)
        assert_eq!(
            chrome_epoch_to_unix(13_380_163_200_000_000),
            Some(1_735_689_600.0)
        );
    }

    #[test]
    fn to_cdp_downgrades_samesite_none_without_secure() {
        let row = CookieRow {
            host_key: ".claude.ai".into(),
            name: "x".into(),
            encrypted_value: vec![],
            path: "/".into(),
            is_secure: false,
            is_httponly: true,
            samesite: 0, // None
            expires_utc: 0,
        };
        let c = to_cdp_cookie(&row, "v".into());
        assert_eq!(c["sameSite"], "Lax");
        assert_eq!(c["httpOnly"], true);
        assert_eq!(c.get("expires"), None);
    }
}

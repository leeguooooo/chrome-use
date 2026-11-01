//! Native Document identity checks. Names or document counts alone confer no
//! authority: the page URL and the menu's native document ancestry must agree.

use serde::Deserialize;

const BITWARDEN_IDS: &[&str] = &[
    "nngceckbapebfimnlniiiahkandclblb", // Chrome Web Store
    "hmjfbpicbfcphbcmemmcghpkcdinoahh", // Official dist-chrome release archive
];

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Snapshot {
    pub pid: u32,
    pub hwnd: u64,
    pub read_only: bool,
    pub truncated: bool,
    pub failed_documents: usize,
    pub documents: Vec<Document>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Document {
    url: Option<String>,
    root_url: Option<String>,
    offscreen: bool,
}

/// The identity read was complete and included a visible Bitwarden menu under
/// the expected visible page. This does not establish desktop input authority,
/// ownership of a Chrome tab/window, or freshness at a later input attempt.
pub(super) struct DocumentProof {
    _private: (),
}

pub(super) fn verify(
    snapshot: &Snapshot,
    expected_pid: u32,
    expected_hwnd: u64,
    expected_url: &str,
) -> Result<DocumentProof, &'static str> {
    if expected_pid == 0
        || expected_hwnd == 0
        || snapshot.pid != expected_pid
        || snapshot.hwnd != expected_hwnd
        || !snapshot.read_only
        || snapshot.truncated
        || snapshot.failed_documents != 0
        || snapshot.documents.len() > 256
    {
        return Err("native document identity could not be verified");
    }
    let expected = url::Url::parse(expected_url)
        .ok()
        .filter(|url| matches!(url.scheme(), "http" | "https"))
        .ok_or("native recovery requires a web page URL")?;
    if expected.cannot_be_a_base() {
        return Err("native recovery requires a web page URL");
    }
    let same_page = |document: &Document| {
        !document.offscreen
            && document.root_url.as_deref() == Some(expected_url)
            && document.url.as_deref() == Some(expected_url)
    };
    if !snapshot.documents.iter().any(same_page) {
        return Err("the expected page was not found in the native window");
    }
    let menu = snapshot.documents.iter().any(|document| {
        if document.offscreen || document.root_url.as_deref() != Some(expected_url) {
            return false;
        }
        let Some(raw) = document.url.as_deref() else {
            return false;
        };
        let Ok(url) = url::Url::parse(raw) else {
            return false;
        };
        url.scheme() == "chrome-extension"
            && url.host_str().is_some_and(|id| BITWARDEN_IDS.contains(&id))
            && url.username().is_empty()
            && url.password().is_none()
            && url.port().is_none()
            && matches!(url.path(), "/overlay/menu.html" | "/overlay/menu-list.html")
    });
    if !menu {
        return Err("no verified Bitwarden inline menu belongs to the expected page");
    }
    Ok(DocumentProof { _private: () })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};
    const PAGE: &str = "https://example.invalid/login?case=780";

    fn observed() -> Value {
        json!({"pid": 42, "hwnd": 84, "readOnly": true, "truncated": false,
        "failedDocuments": 0, "documents": [
            {"url": PAGE, "rootUrl": PAGE, "offscreen": false},
            {"url": "chrome-extension://hmjfbpicbfcphbcmemmcghpkcdinoahh/overlay/menu.html", "rootUrl": PAGE, "offscreen": false}
        ]})
    }
    fn accepts(value: Value) -> bool {
        serde_json::from_value(value)
            .ok()
            .is_some_and(|snapshot| verify(&snapshot, 42, 84, PAGE).is_ok())
    }
    #[test]
    fn exact_page_and_owned_native_menu_are_required() {
        assert!(accepts(observed()));
        for (pointer, replacement) in [
            ("/pid", json!(43)),
            ("/hwnd", json!(85)),
            ("/readOnly", json!(false)),
            ("/truncated", json!(true)),
            ("/failedDocuments", json!(1)),
            ("/documents/0/url", json!("https://example.invalid/other")),
            (
                "/documents/1/rootUrl",
                json!("https://example.invalid/other"),
            ),
            ("/documents/1/rootUrl", Value::Null),
            ("/documents/1/offscreen", json!(true)),
            (
                "/documents/1/url",
                json!("chrome-extension://aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/overlay/menu.html"),
            ),
            (
                "/documents/1/url",
                json!("https://example.invalid/overlay/menu.html"),
            ),
            (
                "/documents/1/url",
                json!(
                    "chrome-extension://hmjfbpicbfcphbcmemmcghpkcdinoahh/overlay/menu-button.html"
                ),
            ),
            (
                "/documents/1/url",
                json!(
                    "chrome-extension://hmjfbpicbfcphbcmemmcghpkcdinoahh/overlay/menu.html/extra"
                ),
            ),
        ] {
            let mut value = observed();
            *value.pointer_mut(pointer).unwrap() = replacement;
            assert!(!accepts(value), "accepted changed identity at {pointer}");
        }
    }
    #[test]
    fn empty_browser_widgets_are_not_identity_proof() {
        let mut value = observed();
        value["documents"] = json!([
            {"url": "", "rootUrl": null, "offscreen": false},
            {"url": null, "rootUrl": null, "offscreen": false}
        ]);
        assert!(!accepts(value));
    }
    #[test]
    fn real_native_document_shape_keeps_the_menu_under_its_page() {
        // Run 41270621094, official Bitwarden 2026.10.1 in the CI fixture.
        // Keep native nesting and empty browser widgets; replace only the
        // disposable loopback URL and process/window identifiers.
        let snapshot: Snapshot =
            serde_json::from_str(include_str!("document-observation.json")).unwrap();
        assert!(verify(&snapshot, 42, 84, PAGE).is_ok());
        assert!(verify(&snapshot, 42, 84, "https://example.invalid/other").is_err());
    }
}

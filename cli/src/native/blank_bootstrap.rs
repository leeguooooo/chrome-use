use super::*;

fn blank(value: &Value) -> bool {
    matches!(value.as_str(), Some("") | Some("about:blank"))
}

fn listed(state: &Value, key: &str, tab: i64) -> Result<bool, String> {
    let ids = state[key]
        .as_array()
        .ok_or_else(|| format!("{key} is unavailable"))?;
    if ids.iter().any(|id| id.as_i64().is_none_or(|id| id < 0)) {
        return Err(format!("{key} contains an invalid tab id"));
    }
    Ok(ids.contains(&json!(tab)))
}

impl BrowserManager {
    pub(super) async fn enable_new_tab_domains(
        &self,
        target: &str,
        session: &str,
        initial_url: &str,
        activate: bool,
        deadline: tokio::time::Instant,
    ) -> Result<(), String> {
        if activate || initial_url != "about:blank" || !self.via_relay() {
            return self.enable_domains(session).await;
        }
        self.enable_created_domains(target, session, initial_url, deadline)
            .await
    }

    pub(super) async fn enable_created_domains(
        &self,
        target: &str,
        session: &str,
        initial_url: &str,
        deadline: tokio::time::Instant,
    ) -> Result<(), String> {
        if tokio::time::Instant::now() >= deadline {
            return Err("new tab initialization deadline expired".into());
        }
        let mut initial_error = None;
        let setup = async {
            match self.enable_domains(session).await {
                Ok(()) => return Ok(()),
                Err(error) => {
                    if !self.via_relay()
                        || initial_url != "about:blank"
                        || !self.created_targets.contains(target)
                        || self.adopted_targets.contains(target)
                        || !error.starts_with("CDP error (Page.enable): relay timeout after 8000ms: chrome.debugger.sendCommand(Page.enable).")
                    {
                        return Err(error);
                    }
                    initial_error = Some(error);
                }
            }
            self.initialize_blank_target(target, session).await?;
            self.enable_domains(session).await
        };
        match tokio::time::timeout_at(deadline, setup).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(error)) => match initial_error {
                Some(original) => Err(format!(
                    "{original} Fresh blank target recovery failed: {error}"
                )),
                None => Err(error),
            },
            Err(_) => match initial_error {
                Some(original) => Err(format!(
                    "{original} Fresh blank target recovery exceeded the initialization deadline."
                )),
                None => Err("new tab initialization deadline expired".into()),
            },
        }
    }

    async fn initialize_blank_target(&self, target: &str, session: &str) -> Result<(), String> {
        let tab = relay_chrome_tab_id(session).ok_or("new tab has no exact Chrome tab id")?;
        let presence = self
            .client
            .send_command(
                "ABExt.tabPresence",
                Some(json!({"targetId":target,"tabId":tab})),
                None,
            )
            .await?;
        if presence["tabPresenceVersion"] != 1
            || presence["targetId"] != target
            || presence["tabId"] != tab
            || presence["presence"] != "present"
        {
            return Err("new tab target identity could not be verified".into());
        }
        let state = self.client.send_command("ABExt.state", None, None).await?;
        if !listed(&state, "ownedTabs", tab)? {
            return Err("new tab is no longer owned by the extension".into());
        }
        if listed(&state, "userTouchedTabs", tab)? {
            return Err("new tab has a touch marker; recovery was not attempted".into());
        }
        let record = self
            .client
            .send_command(
                "ABExt.inspectTab",
                Some(json!({"targetId":target,"sessionId":session})),
                None,
            )
            .await?;
        if record["targetId"] != target
            || record["sessionId"] != session
            || record["chromeTabId"] != tab
            || record["active"] != false
            || record["debuggerAttached"] != true
            || record["discarded"] != false
            || record["frozen"] != false
            || !blank(&record["url"])
            || !blank(&record["committedUrl"])
            || !blank(&record["pendingUrl"])
        {
            return Err(
                "new tab is not the same inactive blank target; recovery was not attempted".into(),
            );
        }
        let updated = self
            .chrome_call("tabs", "update", json!([tab,{"url":"about:blank"}]))
            .await?;
        if updated["id"] != tab
            || updated["active"] != false
            || !blank(&updated["url"])
            || !updated.get("pendingUrl").is_none_or(blank)
        {
            return Err("blank initialization navigation was not confirmed".into());
        }
        Ok(())
    }
}

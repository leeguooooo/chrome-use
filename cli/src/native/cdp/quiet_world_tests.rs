use super::*;
use serde_json::json;
use tokio::net::TcpListener;

async fn fixture(session: String, context: i64) -> (CdpClient, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
        let mut document = 0;
        let mut creations = 0;
        let mut race_once = false;
        let mut race_always = false;
        let mut evaluations = 0;
        let mut refuse_first_world = false;
        while let Some(Ok(message)) = ws.next().await {
            let Message::Text(raw) = message else {
                continue;
            };
            let command: Value = serde_json::from_str(&raw).unwrap();
            let method = command["method"].as_str().unwrap();
            let mut event = None;
            let mut event_before_reply = false;
            if method == "Page.createIsolatedWorld" && refuse_first_world && document == 0 {
                ws.send(Message::Text(json!({"id":command["id"],"error":{"code":-32000,"message":"No frame for given id"}}).to_string().into())).await.unwrap();
                continue;
            }
            let result = match method {
                "Page.getFrameTree" => {
                    json!({"frameTree":{"frame":{"id":"main","loaderId":format!("loader-{document}"),"url":format!("https://fixture.test/{document}")}}})
                }
                "Page.createIsolatedWorld" => {
                    creations += 1;
                    let old_context = context + document;
                    if race_once || race_always {
                        race_once = false;
                        document += 1;
                        event = Some(
                            json!({"method":"Page.frameNavigated","sessionId":session,"params":{"frame":{"id":"main","loaderId":format!("loader-{document}"),"url":format!("https://fixture.test/{document}")}}}),
                        );
                        event_before_reply = true;
                    }
                    json!({"executionContextId":old_context})
                }
                "Page.navigate" => {
                    document += 1;
                    event = Some(
                        json!({"method":"Page.frameNavigated","sessionId":session,"params":{"frame":{"id":"main","loaderId":format!("loader-{document}"),"url":format!("https://fixture.test/{document}")}}}),
                    );
                    json!({"frameId":"main","loaderId":format!("loader-{document}")})
                }
                "Test.childNavigation" => {
                    event = Some(
                        json!({"method":"Page.frameNavigated","sessionId":session,"params":{"frame":{"id":"child","parentId":"main","loaderId":"child-loader"}}}),
                    );
                    json!({})
                }
                "Test.contextsCleared" => {
                    document += 1;
                    event = Some(
                        json!({"method":"Runtime.executionContextsCleared","sessionId":session,"params":{}}),
                    );
                    json!({})
                }
                "Test.armRace" => {
                    race_once = true;
                    json!({})
                }
                "Test.raceAlways" => {
                    race_always = true;
                    json!({})
                }
                "Test.refuseWorld" => {
                    refuse_first_world = true;
                    json!({})
                }
                "Test.stats" => json!({"creations":creations,"evaluations":evaluations}),
                "Runtime.evaluate" => {
                    evaluations += 1;
                    // Old numeric context ids remain valid but now identify a
                    // different document. There is deliberately no CDP error.
                    let value = if command["params"]["contextId"] == context + document {
                        format!("current-{document}")
                    } else {
                        "about:blank".to_string()
                    };
                    json!({"result":{"type":"string","value":value}})
                }
                other => panic!("unexpected CDP method: {other}"),
            };
            if event_before_reply {
                ws.send(Message::Text(event.take().unwrap().to_string().into()))
                    .await
                    .unwrap();
            }
            ws.send(Message::Text(
                json!({"id":command["id"],"result":result})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
            if let Some(event) = event {
                ws.send(Message::Text(event.to_string().into()))
                    .await
                    .unwrap();
            }
        }
    });
    (
        CdpClient::connect(&format!("ws://{address}"))
            .await
            .unwrap(),
        server,
    )
}

async fn read(client: &CdpClient, session: &str) -> Value {
    client
        .send_command(
            "Runtime.evaluate",
            Some(json!({"expression":"location.href","returnByValue":true})),
            Some(session),
        )
        .await
        .unwrap()["result"]["value"]
        .clone()
}

async fn event(client: &CdpClient, session: &str, command: &str) {
    let mut events = client.subscribe();
    client
        .send_command(
            command,
            Some(json!({"url":"https://fixture.test/next"})),
            Some(session),
        )
        .await
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(3), events.recv())
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn navigation_discards_a_valid_but_reused_numeric_context() {
    let session = uuid::Uuid::new_v4().to_string();
    let (client, server) = fixture(session.clone(), 10).await;
    assert_eq!(read(&client, &session).await, "current-0");
    event(&client, &session, "Page.navigate").await;
    assert_eq!(read(&client, &session).await, "current-1");
    drop(client);
    server.abort();
}

#[tokio::test]
async fn new_connections_do_not_share_contexts_with_the_same_session_id() {
    let session = uuid::Uuid::new_v4().to_string();
    let (first, first_server) = fixture(session.clone(), 20).await;
    assert_eq!(read(&first, &session).await, "current-0");
    let (second, second_server) = fixture(session.clone(), 30).await;
    assert_eq!(read(&second, &session).await, "current-0");
    drop(first);
    drop(second);
    first_server.abort();
    second_server.abort();
}

#[tokio::test]
async fn context_clear_events_discard_the_old_world() {
    let session = uuid::Uuid::new_v4().to_string();
    let (client, server) = fixture(session.clone(), 40).await;
    assert_eq!(read(&client, &session).await, "current-0");
    event(&client, &session, "Test.contextsCleared").await;
    assert_eq!(read(&client, &session).await, "current-1");
    drop(client);
    server.abort();
}

#[tokio::test]
async fn child_frame_navigation_preserves_the_top_level_world() {
    let session = uuid::Uuid::new_v4().to_string();
    let (client, server) = fixture(session.clone(), 50).await;
    assert_eq!(read(&client, &session).await, "current-0");
    event(&client, &session, "Test.childNavigation").await;
    assert_eq!(read(&client, &session).await, "current-0");
    let stats = client
        .send_command("Test.stats", None, Some(&session))
        .await
        .unwrap();
    assert_eq!(stats["creations"], 1);
    drop(client);
    server.abort();
}

#[tokio::test]
async fn navigation_during_world_creation_cannot_repopulate_the_old_context() {
    let session = uuid::Uuid::new_v4().to_string();
    let (client, server) = fixture(session.clone(), 60).await;
    client
        .send_command("Test.armRace", None, Some(&session))
        .await
        .unwrap();
    assert_eq!(read(&client, &session).await, "current-1");
    let stats = client
        .send_command("Test.stats", None, Some(&session))
        .await
        .unwrap();
    assert_eq!(stats["creations"], 2);
    drop(client);
    server.abort();
}

#[tokio::test]
async fn continuously_changing_documents_refuse_instead_of_falling_back_to_the_main_world() {
    let session = uuid::Uuid::new_v4().to_string();
    let (client, server) = fixture(session.clone(), 70).await;
    client
        .send_command("Test.raceAlways", None, Some(&session))
        .await
        .unwrap();
    let error = client
        .send_command(
            "Runtime.evaluate",
            Some(json!({"expression":"location.href"})),
            Some(&session),
        )
        .await
        .unwrap_err();
    assert!(
        error.contains(crate::native::quiet_dom::WORLD_CHANGED),
        "{error}"
    );
    let stats = client
        .send_command("Test.stats", None, Some(&session))
        .await
        .unwrap();
    assert_eq!(stats["creations"], 2);
    assert_eq!(stats["evaluations"], 0);
    drop(client);
    server.abort();
}

#[tokio::test]
async fn navigation_retries_a_world_previously_unavailable_on_the_session() {
    let session = uuid::Uuid::new_v4().to_string();
    let (client, server) = fixture(session.clone(), 80).await;
    client
        .send_command("Test.refuseWorld", None, Some(&session))
        .await
        .unwrap();
    assert_eq!(read(&client, &session).await, "about:blank");
    event(&client, &session, "Page.navigate").await;
    assert_eq!(read(&client, &session).await, "current-1");
    drop(client);
    server.abort();
}

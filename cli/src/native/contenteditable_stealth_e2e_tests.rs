//! Browser acceptance for contenteditable event trust and editor-owned state.
use super::*;

#[tokio::test]
#[ignore]
async fn e2e_contenteditable_fill_preserves_focus_and_native_events() {
    let html = r##"<!doctype html><meta charset="utf-8">
      <div id="editor" contenteditable="true" role="textbox">old text</div>
      <output id="model"></output><output id="events">[]</output>
      <script>
      const editor = document.getElementById('editor');
      const model = document.getElementById('model');
      const events = document.getElementById('events');
      for (const type of ['beforeinput','input','change','blur','focusout']) {
        editor.addEventListener(type, event => {
          const log = JSON.parse(events.textContent);
          log.push({type: event.type, trusted: event.isTrusted});
          events.textContent = JSON.stringify(log);
          if (type === 'input') {
            if (editor.dataset.reject === 'true') editor.textContent = '';
            if (event.isTrusted) model.textContent = editor.textContent;
          }
        });
      }
      </script>"##;
    let (port, server) = spawn_html_server(html.to_string()).await;
    let mut state = DaemonState::new();
    launch_on(port, &mut state).await;
    let response = run_cmd(
        &mut state,
        json!({"id":"fill","action":"fill","selector":"#editor","value":"hello world"}),
    )
    .await;
    assert_success(&response);
    assert_eq!(
        eval_in(&mut state, "document.getElementById('model').textContent").await,
        "hello world"
    );
    assert_eq!(
        eval_in(&mut state, "document.activeElement.id").await,
        "editor"
    );
    let events = eval_in(
        &mut state,
        "JSON.parse(document.getElementById('events').textContent)",
    )
    .await;
    let events = events.as_array().unwrap();
    assert!(events.iter().any(|e| e["type"] == "input"));
    assert!(events.iter().all(|e| e["trusted"] == true));
    assert!(events
        .iter()
        .all(|e| e["type"] != "change" && e["type"] != "focusout" && e["type"] != "blur"));

    eval_in(&mut state, "document.getElementById('editor').dataset.reject='true';document.getElementById('events').textContent='[]'").await;
    let rejected = run_cmd(
        &mut state,
        json!({"id":"reject","action":"fill","selector":"#editor","value":"rejected"}),
    )
    .await;
    assert_eq!(rejected["success"], false, "{rejected}");
    assert_eq!(
        eval_in(&mut state, "document.getElementById('editor').textContent").await,
        ""
    );
    assert_eq!(eval_in(&mut state, "JSON.parse(document.getElementById('events').textContent).filter(e=>!e.trusted).length").await, 0);
    run_cmd(&mut state, json!({"id":"close","action":"close"})).await;
    server.abort();
}

#[tokio::test]
#[ignore]
async fn e2e_readback_mismatch_does_not_infer_an_input_filter() {
    let html = r##"<!doctype html><meta charset="utf-8">
      <input id="plain">
      <input id="filtered">
      <div id="editor" contenteditable="true" role="textbox"></div>
      <script>
      document.getElementById('filtered').addEventListener('input', event => {
        event.target.value = event.target.value.replace(/[^\x00-\x7f]/g, '');
      });
      document.getElementById('editor').addEventListener('input', event => {
        const editor = event.target;
        const text = editor.textContent;
        const marker = String.fromCodePoint(0x1f642);
        const at = text.indexOf(marker);
        if (at < 0) return;
        const image = document.createElement('img');
        image.alt = marker;
        image.src = 'data:image/svg+xml,%3Csvg xmlns="http://www.w3.org/2000/svg" width="16" height="16"%3E%3Ccircle cx="8" cy="8" r="7" fill="gold"/%3E%3C/svg%3E';
        editor.replaceChildren(
          document.createTextNode(text.slice(0, at)),
          image,
          document.createTextNode(text.slice(at + marker.length))
        );
      });
      </script>"##;
    let (port, server) = spawn_html_server(html.to_string()).await;
    let mut state = DaemonState::new();
    launch_on(port, &mut state).await;
    let requested = "hello \u{1f642} world";
    let plain = run_cmd(
        &mut state,
        json!({"id":"plain","action":"fill","selector":"#plain","value":requested}),
    )
    .await;
    assert_success(&plain);
    assert_eq!(
        eval_in(&mut state, "document.getElementById('plain').value").await,
        requested
    );

    for action in ["fill", "type"] {
        eval_in(
            &mut state,
            "document.getElementById('editor').textContent = ''",
        )
        .await;
        let response = run_cmd(
            &mut state,
            json!({"id":action,"action":action,"selector":"#editor","value":requested,"text":requested}),
        ).await;
        let diagnostic = if action == "fill" {
            assert_eq!(response["success"], false, "{response}");
            response["error"].as_str().unwrap()
        } else {
            assert_success(&response);
            response["data"]["warning"].as_str().unwrap()
        };
        assert!(diagnostic.contains("read back"), "{diagnostic}");
        assert!(diagnostic.contains("Inspect the field"), "{diagnostic}");
        assert!(!diagnostic.contains("Latin-only"), "{diagnostic}");
        assert!(!diagnostic.contains("romanized"), "{diagnostic}");
        assert!(!diagnostic.contains("rewrote or rejected"), "{diagnostic}");
        assert_eq!(
            eval_in(&mut state, "document.querySelector('#editor img').alt").await,
            "\u{1f642}"
        );
        assert_eq!(
            eval_in(&mut state, "document.getElementById('editor').innerText").await,
            "hello  world"
        );
    }

    let filtered = run_cmd(
        &mut state,
        json!({"id":"filtered","action":"fill","selector":"#filtered","value":requested}),
    )
    .await;
    assert_eq!(filtered["success"], false, "{filtered}");
    assert!(
        filtered["error"]
            .as_str()
            .unwrap()
            .contains("fill verification failed"),
        "{filtered}"
    );
    assert_eq!(
        eval_in(&mut state, "document.getElementById('filtered').value").await,
        "hello  world"
    );
    run_cmd(&mut state, json!({"id":"close","action":"close"})).await;
    server.abort();
}

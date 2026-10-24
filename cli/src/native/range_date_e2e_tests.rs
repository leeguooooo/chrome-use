//! Trusted date-range confirmation, including the model behind the inputs.
use super::*;

#[tokio::test]
#[ignore]
async fn e2e_range_date_fill_commits_both_bounds_without_submitting_the_form() {
    for mode in ["tab", "button", "blur"] {
        let (port, server) = spawn_html_server(
            include_str!("test_fixtures/range_date_commit_probe.html").replace("__MODE__", mode),
        )
        .await;
        let mut state = DaemonState::new();
        launch_on(port, &mut state).await;
        let expected = json!(["2026-10-20 01:00:00", "2026-10-21 02:00:00"]);
        for (index, selector) in ["#start", "#end"].iter().enumerate() {
            let response = run_cmd(
                &mut state,
                json!({
                    "id": format!("fill-{index}"), "action": "fill", "selector": selector,
                    "value": expected[index],
                }),
            )
            .await;
            assert_success(&response);
        }
        assert_eq!(
            eval_in(
                &mut state,
                "JSON.parse(document.getElementById('model').textContent)"
            )
            .await,
            expected
        );
        assert_eq!(
            eval_in(
                &mut state,
                "[document.getElementById('start').value,document.getElementById('end').value]"
            )
            .await,
            expected
        );
        assert_success(
            &run_cmd(
                &mut state,
                json!({"id":"query", "action":"click", "selector":"#query"}),
            )
            .await,
        );
        assert_eq!(
            eval_in(
                &mut state,
                "JSON.parse(document.getElementById('result').textContent)"
            )
            .await,
            expected
        );
        assert_eq!(
            eval_in(&mut state, "document.getElementById('submits').textContent").await,
            "0"
        );
        assert_eq!(
            eval_in(
                &mut state,
                "document.getElementById('untrusted').textContent"
            )
            .await,
            "0"
        );
        assert_eq!(
            eval_in(&mut state, "document.getElementById('tabs').textContent").await,
            if mode == "tab" { "2" } else { "0" }
        );

        // Rejected and read-only controls cannot be forced with the native setter.
        let invalid = run_cmd(
            &mut state,
            json!({"id":"invalid", "action":"fill", "selector":"#start", "value":"not-a-date"}),
        )
        .await;
        assert_eq!(invalid["success"], false);
        assert_eq!(
            eval_in(
                &mut state,
                "JSON.parse(document.getElementById('model').textContent)"
            )
            .await,
            expected
        );
        eval_in(&mut state, "document.getElementById('end').dataset.locked = 'true'; document.getElementById('end').readOnly = true").await;
        let readonly = run_cmd(&mut state, json!({"id":"readonly", "action":"fill", "selector":"#end", "value":"2026-10-22 03:00:00"})).await;
        assert_eq!(readonly["success"], false);
        assert_eq!(
            eval_in(
                &mut state,
                "document.getElementById('untrusted').textContent"
            )
            .await,
            "0"
        );
        assert_eq!(
            eval_in(&mut state, "document.getElementById('tabs').textContent").await,
            if mode == "tab" { "2" } else { "0" }
        );
        assert_success(&run_cmd(&mut state, json!({"id":"ordinary", "action":"fill", "selector":"#ordinary", "value":"ordinary text"})).await);
        assert_eq!(
            eval_in(&mut state, "document.activeElement.id").await,
            "ordinary"
        );
        assert_eq!(
            eval_in(&mut state, "document.getElementById('tabs').textContent").await,
            if mode == "tab" { "2" } else { "0" }
        );
        close_state(&mut state).await;
        server.abort();
    }
}

#[tokio::test]
#[ignore]
async fn e2e_range_date_fill_refuses_an_invisible_confirmation_popup() {
    let (port, server) = spawn_html_server(
        include_str!("test_fixtures/range_date_commit_probe.html")
            .replace("__MODE__", "transparent"),
    )
    .await;
    let mut state = DaemonState::new();
    launch_on(port, &mut state).await;
    let response = run_cmd(&mut state, json!({"id":"invisible", "action":"fill", "selector":"#start", "value":"2026-10-20 01:00:00"})).await;
    assert_eq!(response["success"], false);
    assert_eq!(
        eval_in(&mut state, "document.getElementById('model').textContent").await,
        "[\"\",\"\"]"
    );
    assert_eq!(
        eval_in(&mut state, "document.getElementById('submits').textContent").await,
        "0"
    );
    close_state(&mut state).await;
    server.abort();
}

use super::*;
use crate::{
    auth::Credentials,
    store::{Session, Settings},
    test_support::server,
};
use std::sync::Mutex;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn fixture() -> (tempfile::TempDir, WebSearch, Arc<Mutex<String>>) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().into()).unwrap();
    store
        .write(
            "auth.json",
            &Credentials {
                access_token: "fixture-access".into(),
                refresh_token: "fixture-refresh".into(),
                expires_at: now() + 3600,
                account_id: Some("fixture-account".into()),
                email: None,
            },
        )
        .unwrap();
    let model = Arc::new(Mutex::new("gpt-6.1-sol".to_owned()));
    let selected = model.clone();
    let web = WebSearch::new(
        store.clone(),
        Auth::new(store).unwrap(),
        "fixture-session".into(),
        Arc::new(move || Ok(selected.lock().unwrap().clone())),
        WebSearch::client().unwrap(),
    );
    (dir, web, model)
}
fn mode(web: &WebSearch, mode: Mode) {
    let mut catalog = Catalog::default();
    catalog.settings.web_search = mode;
    web.store.save_catalog(&catalog).unwrap();
}

#[test]
fn commands_match_all_ten_operations_and_reject_invalid_arguments() {
    let args = json!({
        "search_query":[{"q":"compiler","recency":7,"domains":["rust-lang.org"]}],
        "image_query":[{"q":"waterfall"}],
        "open":[{"ref_id":"https://example.org/manual.pdf","lineno":37}],
        "click":[{"ref_id":"turn0view3","id":17}],
        "find":[{"ref_id":"turn2view1","pattern":"λ section"}],
        "screenshot":[{"ref_id":"turn1view0","pageno":0}],
        "finance":[{"ticker":"BTC","type":"crypto","market":""}],
        "weather":[{"location":"Finland, Uusimaa, Helsinki","start":"2026-11-15","duration":3}],
        "sports":[{"fn":"schedule","league":"nba","team":"LAL","opponent":"BOS","date_from":"2026-11-14","date_to":"2026-11-20","num_games":2,"locale":"en-US"}],
        "time":[{"utc_offset":"-03:30"}],
        "response_length":"long"
    });
    let parsed = commands::SearchCommands::parse(&args.to_string()).unwrap();
    let mut expected = args;
    expected["sports"][0]["tool"] = json!("sports");
    assert_eq!(serde_json::to_value(parsed).unwrap(), expected);
    for invalid in [
        json!({}),
        json!({"open":[]}),
        json!({"open":[{"ref_id":" "}]}),
        json!({"search_query":[{"q":"hello","domains":["https://example.org"]}]}),
        json!({"screenshot":[{"ref_id":"x","pageno":-1}]}),
        json!({"sports":[{"fn":"rankings","league":"nba"}]}),
        json!({"search_query":[{"q":"hi","typo":true}]}),
        json!({"literature_search_query":[{"q":"papers"}]}),
        json!({"search_query":[{"q":"a"},{"q":"b"},{"q":"c"},{"q":"d"}]}),
        json!({"search_query":[{"q":"a"},{"q":"b"},{"q":"c"},{"q":"d"},{"q":"e"}],"response_length":"long"}),
    ] {
        assert!(
            commands::SearchCommands::parse(&invalid.to_string()).is_err(),
            "{invalid}"
        );
    }
    assert!(commands::SearchCommands::parse(r#"{"search_query":[{"q":"a"},{"q":"b"},{"q":"c"},{"q":"d"}],"response_length":"medium"}"#).is_ok());
}

#[tokio::test]
async fn transport_preserves_metadata_and_snapshots_mode_model_without_conversation_context() {
    let (_dir, mut web, model) = fixture();
    let result = json!({"type":"text_result","ref_id":"turn0search0","title":"Manual","url":"https://example.org/manual","future_field":{"private_ui":"metadata-only"}});
    let reply = json!({"output":"Read https://example.org/manual (turn0search0)","encrypted_output":"not-agent-output","results":[result.clone()]}).to_string();
    let (url, mut requests, server) = server(vec![
        (200, reply),
        (200, json!({"output":"follow-up"}).to_string()),
    ])
    .await;
    web.endpoint = format!("{url}/alpha/search");
    let response = web
        .call(
            r#"{"search_query":[{"q":"manual","domains":["example.org"],"recency":14}]}"#,
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(
        response.text,
        "Read https://example.org/manual (turn0search0)"
    );
    assert_eq!(response.metadata.mode, Mode::Live);
    assert_eq!(
        serde_json::from_str::<Value>(&response.metadata.details).unwrap(),
        json!([result])
    );
    assert_eq!(
        response.metadata.sources,
        vec![Source {
            title: "Manual".into(),
            url: "https://example.org/manual".into()
        }]
    );
    let first = requests.recv().await.unwrap();
    assert!(first.starts_with("POST /alpha/search "));
    for header in [
        "authorization: Bearer fixture-access",
        "chatgpt-account-id: fixture-account",
        "originator: pico-evo",
        "user-agent: pico-evo/",
    ] {
        assert!(first.contains(header), "missing {header}");
    }
    let first: Value = serde_json::from_str(first.split_once("\r\n\r\n").unwrap().1).unwrap();
    assert_eq!(
        first,
        json!({"id":"fixture-session","model":"gpt-6.1-sol","commands":{"search_query":[{"q":"manual","domains":["example.org"],"recency":14}]},"settings":{"external_web_access":true,"allowed_callers":["direct"]},"max_output_tokens":10000})
    );
    mode(&web, Mode::Cached);
    *model.lock().unwrap() = "gpt-6-luna".into();
    let second = web
        .call(
            r#"{"open":[{"ref_id":"turn0search0"}]}"#,
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(second.metadata.mode, Mode::Cached);
    assert_eq!(second.metadata.details, "[]");
    let request = requests.recv().await.unwrap();
    let request: Value = serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap();
    assert_eq!(request["id"], first["id"]);
    assert_eq!(request["model"], "gpt-6-luna");
    assert_eq!(request["settings"]["external_web_access"], false);
    assert_eq!(request["commands"]["open"][0]["ref_id"], "turn0search0");
    assert!(request.get("input").is_none());
    server.await.unwrap();
}

#[tokio::test]
async fn invalid_disabled_and_cancelled_calls_never_dispatch() {
    let (_dir, mut web, _) = fixture();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    web.endpoint = format!("http://{}", listener.local_addr().unwrap());
    let args = r#"{"time":[{"utc_offset":"+00:00"}]}"#;
    assert!(web.call("{}", &CancellationToken::new()).await.is_err());
    mode(&web, Mode::Disabled);
    assert!(!web.enabled().unwrap());
    assert!(
        web.call(args, &CancellationToken::new())
            .await
            .err()
            .unwrap()
            .to_string()
            .contains("disabled")
    );
    mode(&web, Mode::Live);
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    assert!(
        web.call(args, &cancelled)
            .await
            .err()
            .unwrap()
            .to_string()
            .contains("cancelled")
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(30), listener.accept())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn http_and_malformed_failures_preserve_details_without_replay() {
    let (_dir, mut web, _) = fixture();
    let large_error = "service λ\n".repeat(1000);
    let (url, mut requests, server) = server(vec![
        (429, large_error.clone()),
        (200, "{broken".into()),
        (200, json!({"results":[]}).to_string()),
    ])
    .await;
    web.endpoint = url;
    let args = r#"{"time":[{"utc_offset":"+00:00"}]}"#;
    let error = web
        .call(args, &CancellationToken::new())
        .await
        .err()
        .unwrap();
    assert!(error.to_string().contains("429"));
    assert!(!error.to_string().contains("service λ"));
    let failure = error.downcast_ref::<Failure>().unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&failure.metadata.details).unwrap()["response_body"],
        large_error
    );
    for expected in ["Invalid web response", "Invalid web response"] {
        let error = web
            .call(args, &CancellationToken::new())
            .await
            .err()
            .unwrap()
            .to_string();
        assert!(error.contains(expected), "{error}");
    }
    server.await.unwrap();
    for _ in 0..3 {
        assert!(requests.recv().await.is_some());
    }
    assert!(requests.recv().await.is_none());
}

#[tokio::test]
async fn cancellation_drops_an_inflight_request() {
    let (_dir, mut web, _) = fixture();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    web.endpoint = format!("http://{}", listener.local_addr().unwrap());
    let cancel = CancellationToken::new();
    let stop = cancel.clone();
    let caller = tokio::spawn(async move {
        web.call(r#"{"time":[{"utc_offset":"+00:00"}]}"#, &cancel)
            .await
    });
    let (mut socket, _) = listener.accept().await.unwrap();
    let mut bytes = [0; 4096];
    assert!(socket.read(&mut bytes).await.unwrap() > 0);
    stop.cancel();
    let error = tokio::time::timeout(Duration::from_secs(2), caller)
        .await
        .unwrap()
        .unwrap()
        .err()
        .unwrap();
    assert!(error.to_string().contains("cancelled"));
    assert!(error.to_string().contains("usage"));
}

#[test]
fn large_metadata_is_durable_and_does_not_make_unsafe_links_clickable() {
    let results = vec![
        json!({"url":"javascript:alert(1)","title":"unsafe"}),
        json!({"url":"https://user:pass@example.org","title":"credentials"}),
        json!({"url":"https://example.org","title":"safe","new_kind":"retained λ".repeat(10000)}),
    ];
    let metadata = Metadata::new(Mode::Cached, 123, results.clone()).unwrap();
    assert_eq!(metadata.sources.len(), 1);
    assert_eq!(metadata.sources[0].title, "safe");
    let dir = tempfile::tempdir().unwrap();
    let session_id = {
        let store = Store::open(dir.path().into()).unwrap();
        let mut session = Session::new(dir.path().into(), &Settings::default());
        session.entries.push(crate::store::Entry::new(
            crate::store::EntryKind::Tool {
                name: "web__run".into(),
                input: "{}".into(),
                output: "text".into(),
                complete: true,
                parent: None,
                media: vec![],
                web: Some(metadata),
            },
            "",
        ));
        store.save_session(&session).unwrap();
        session.id
    };
    let store = Store::open(dir.path().into()).unwrap();
    let session = store.load_session(&session_id).unwrap();
    let crate::store::EntryKind::Tool {
        web: Some(metadata),
        ..
    } = &session.entries[0].kind
    else {
        panic!("missing persisted metadata")
    };
    assert_eq!(
        serde_json::from_str::<Value>(&metadata.details).unwrap(),
        json!(results)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn code_mode_returns_text_only_persists_metadata_and_respects_disabled_changes() {
    use crate::{
        code_mode::{CodeMode, Event},
        shell::Shell,
        store::{Entry, EntryKind},
    };
    let (dir, mut web, _) = fixture();
    let (url, mut requests, server) = server(vec![(200,json!({"output":"Visible result","results":[{"type":"new_kind","secret_marker":"ui-only"}]}).to_string()),(200,json!({"output":"valid text","results":"malformed-ui-only-marker"}).to_string())]).await;
    web.endpoint = url;
    let store = web.store.clone();
    let mut session = Session::new(dir.path().into(), &Settings::default());
    let session_id = session.id.clone();
    let state = Arc::new(Mutex::new(session.clone()));
    let observed = state.clone();
    let save = store.clone();
    let web = Arc::new(web);
    let code = CodeMode::new(
        Arc::new(Shell::with_runtime(
            dir.path().into(),
            tokio::runtime::Handle::current(),
        )),
        Arc::new(move |event| {
            if let Event::Tool(event) = event {
                let mut session = observed.lock().unwrap();
                if let Some(output) = event.output {
                    let entry = session.entries.by_id_mut(&event.id).unwrap();
                    if let EntryKind::Tool {
                        output: text,
                        complete,
                        web,
                        ..
                    } = &mut entry.kind
                    {
                        *text = output;
                        *complete = true;
                        *web = event.web;
                    }
                } else {
                    session.entries.push(Entry {
                        id: event.id,
                        text: String::new(),
                        kind: EntryKind::Tool {
                            name: event.name,
                            input: event.input,
                            output: String::new(),
                            complete: false,
                            parent: Some(event.parent),
                            media: vec![],
                            web: None,
                        },
                    });
                }
                save.save_session(&session)?;
            }
            Ok(())
        }),
    )
    .with_web(Some(web.clone()));
    let code = Arc::new(code);
    let exec = code.clone();
    let result = tokio::task::spawn_blocking(move || exec.exec(r#"const r = await tools.web__run({time:[{utc_offset:'+00:00'}]}); text(typeof r); text(r);"#,"outer",&CancellationToken::new())).await.unwrap().unwrap();
    assert!(result.contains("string\nVisible result"), "{result}");
    assert!(!result.contains("ui-only"));
    requests.recv().await.unwrap();
    let exec = code.clone();
    let error=tokio::task::spawn_blocking(move || exec.exec(r#"try { await tools.web__run({time:[{utc_offset:'+00:00'}]}); } catch(e) { text(String(e)); }"#,"bad-response",&CancellationToken::new())).await.unwrap().unwrap();
    assert!(error.contains("Invalid web response"));
    assert!(!error.contains("malformed-ui-only-marker"));
    requests.recv().await.unwrap();
    server.await.unwrap();
    session = store.load_session(&session_id).unwrap();
    assert!(
        matches!(&session.entries[1].kind,EntryKind::Tool{web:Some(metadata),output,complete:true,..} if metadata.details.contains("malformed-ui-only-marker") && !output.contains("malformed-ui-only-marker"))
    );
    assert!(
        matches!(&session.entries[0].kind,EntryKind::Tool{web:Some(metadata),complete:true,..} if metadata.details.contains("ui-only"))
    );
    mode(&web, Mode::Disabled);
    let result = tokio::task::spawn_blocking(move || code.exec(r#"text(ALL_TOOLS.some(t=>t.name==='web__run')); try { __call('web__run','{}'); } catch(e) { text(String(e)); }"#,"blocked",&CancellationToken::new())).await.unwrap().unwrap();
    assert!(result.contains("false"));
    assert!(result.contains("Unknown nested tool"));
    assert_eq!(store.load_session(&session_id).unwrap().entries.len(), 2);
}

#[tokio::test]
async fn redirects_do_not_forward_authentication() {
    let (_dir, mut web, _) = fixture();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let target = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    web.endpoint = format!("http://{}", listener.local_addr().unwrap());
    let location = format!("http://{}", target.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buffer = [0; 4096];
        assert!(socket.read(&mut buffer).await.unwrap() > 0);
        socket.write_all(format!("HTTP/1.1 307 Redirect\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").as_bytes()).await.unwrap();
    });
    let error = web
        .call(
            r#"{"time":[{"utc_offset":"+00:00"}]}"#,
            &CancellationToken::new(),
        )
        .await
        .err()
        .unwrap();
    assert!(error.to_string().contains("307"));
    server.await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(30), target.accept())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn definite_unauthorized_refreshes_once_and_preserves_request_identity() {
    let (_dir, mut web, _) = fixture();
    let (issuer, mut refreshes, refresh_server)=server(vec![(200,json!({"access_token":"rotated-access","refresh_token":"rotated-refresh","expires_in":3600}).to_string())]).await;
    web.auth = Auth::fixture(web.store.clone(), issuer);
    let (url, mut requests, server) = server(vec![
        (401, "expired".into()),
        (401, "still unauthorized".into()),
    ])
    .await;
    web.endpoint = url;
    let error = web
        .call(
            r#"{"time":[{"utc_offset":"+00:00"}]}"#,
            &CancellationToken::new(),
        )
        .await
        .err()
        .unwrap();
    assert!(error.to_string().contains("401"));
    let first = requests.recv().await.unwrap();
    let second = requests.recv().await.unwrap();
    assert!(first.contains("authorization: Bearer fixture-access"));
    assert!(second.contains("authorization: Bearer rotated-access"));
    assert_eq!(
        first.split_once("\r\n\r\n").unwrap().1,
        second.split_once("\r\n\r\n").unwrap().1
    );
    assert!(
        refreshes
            .recv()
            .await
            .unwrap()
            .contains("refresh_token=fixture-refresh")
    );
    let saved: Credentials = web.store.read_optional("auth.json").unwrap().unwrap();
    assert_eq!(saved.refresh_token, "rotated-refresh");
    server.await.unwrap();
    refresh_server.await.unwrap();
    assert!(refreshes.recv().await.is_none());
}

#[tokio::test]
async fn malformed_response_keeps_metadata_out_of_agent_errors() {
    let (_dir, mut web, _) = fixture();
    let body = json!({"output":123,"results":[{"private_ui":"metadata-only-marker"}]}).to_string();
    let (url, _requests, server) = server(vec![(200, body.clone())]).await;
    web.endpoint = url;
    let error = web
        .call(
            r#"{"time":[{"utc_offset":"+00:00"}]}"#,
            &CancellationToken::new(),
        )
        .await
        .err()
        .unwrap();
    assert!(!format!("{error:#}").contains("metadata-only-marker"));
    let metadata = &error.downcast_ref::<Failure>().unwrap().metadata;
    assert_eq!(
        serde_json::from_str::<Value>(&metadata.details).unwrap()["response_body"],
        body
    );
    server.await.unwrap();
}

#[tokio::test]
async fn truncated_response_preserves_received_bytes_only_in_ui_diagnostics() {
    let (_dir, mut web, _) = fixture();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    web.endpoint = format!("http://{}", listener.local_addr().unwrap());
    let partial = r#"{"output":"incomplete","results":[{"private":"partial-ui-only"}"#;
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buffer = [0; 4096];
        assert!(socket.read(&mut buffer).await.unwrap() > 0);
        socket
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{partial}",
                    partial.len() + 50
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        socket.shutdown().await.unwrap();
    });
    let error = web
        .call(
            r#"{"time":[{"utc_offset":"+00:00"}]}"#,
            &CancellationToken::new(),
        )
        .await
        .err()
        .unwrap();
    assert!(error.to_string().contains("interrupted"));
    assert!(!error.to_string().contains("partial-ui-only"));
    let failure = error.downcast_ref::<Failure>().unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&failure.metadata.details).unwrap()["response_body"],
        partial
    );
    server.await.unwrap();
}

use super::*;
use crate::test_support::{StreamingRequest, completed, sqlite, streaming_server, until};

struct Fixture {
    dir: tempfile::TempDir,
    h: Arc<Harness>,
    c: Arc<Conversation>,
    endpoint: String,
    requests: tokio::sync::mpsc::UnboundedReceiver<StreamingRequest>,
    server: tokio::task::JoinHandle<()>,
}
impl Fixture {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let store = crate::test_support::authenticated_store(dir.path().join("state"));
        let (endpoint, requests, server) = streaming_server().await;
        let h = Harness::with_provider(
            store.clone(),
            vec![],
            Provider::fixture(store, endpoint.clone()),
        )
        .unwrap();
        let mut session = Session::new(dir.path().into(), &Settings::default());
        session.rename("Early execution fixture".into());
        session.accept("Run the task".into());
        session.consume_pending();
        let c = h.insert(session);
        h.start(&c.id, Settings::default()).unwrap();
        Self {
            dir,
            h,
            c,
            endpoint,
            requests,
            server,
        }
    }
    async fn request(&mut self) -> StreamingRequest {
        tokio::time::timeout(Duration::from_secs(5), self.requests.recv())
            .await
            .unwrap()
            .unwrap()
    }
    fn saved(&self) -> Session {
        self.h.store.load_session(&self.c.id).unwrap()
    }
    async fn release_cell(&self) {
        let c = self.c.clone();
        tokio::task::spawn_blocking(move || {
            c.code.exec_output(
                "store('release', true)",
                "test-control",
                &CancellationToken::new(),
            )
        })
        .await
        .unwrap()
        .unwrap();
    }
    async fn finish(&mut self) {
        self.request()
            .await
            .reply
            .send(completed(json!([{
                "type":"message", "id":"final", "role":"assistant", "phase":"final_answer",
                "content":[{"type":"output_text", "text":"Finished"}]
            }])))
            .unwrap();
        until(|| !self.c.is_turn_active()).await;
        assert!(!self.c.snapshot().needs_attention);
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.h.stop_all();
        self.server.abort();
    }
}
fn call(id: &str, source: &str) -> Value {
    json!({"type":"custom_tool_call", "id":format!("item-{id}"), "call_id":id,
        "name":"exec", "input":source, "status":"completed"})
}
fn done(index: usize, item: &Value) -> String {
    format!(
        "data: {}\n\n",
        json!({"type":"response.output_item.done", "output_index":index, "item":item})
    )
}
fn result(session: &Session, id: &str) -> Option<String> {
    session
        .entries
        .by_id(id)
        .and_then(|entry| match &entry.kind {
            EntryKind::Tool {
                output,
                complete: true,
                ..
            } => Some(output.clone()),
            _ => None,
        })
}
const HOLD: &str = "await new Promise(resolve => { const poll = () => load('release') ? resolve() : setTimeout(poll, 2); poll(); });";

#[tokio::test]
async fn calls_execute_before_response_completion_exclusively_and_commit_in_model_order() {
    let mut f = Fixture::new().await;
    let stream = f.request().await;
    let a = call(
        "a",
        &format!(
            "// @exec: {{\"yield_time_ms\":300000}}\nnotify('A started'); {HOLD} store('a_finished',true); text('A result');"
        ),
    );
    stream.reply.send(done(0, &a)).unwrap();
    until(|| {
        f.saved()
            .pending_tool_outputs
            .iter()
            .any(|v| v["call_id"] == "a")
    })
    .await;
    // A is executing before even the rest of the calls have been delivered.
    assert!(result(&f.c.snapshot(), "a").is_none());
    let commentary = json!({"type":"message","id":"commentary","role":"assistant","content":[{"type":"output_text","text":"Still streaming"}]});
    let b = call("b", "text(load('a_finished')); text('B result');");
    stream
        .reply
        .send(format!(
            "{}{}{}",
            done(1, &commentary),
            done(2, &b),
            done(2, &b)
        ))
        .unwrap();
    until(|| {
        f.saved()
            .in_progress_response
            .as_ref()
            .unwrap()
            .items
            .contains_key(&2)
    })
    .await;
    assert!(f.c.snapshot().entries.by_id("b").is_none());
    f.release_cell().await;
    until(|| {
        f.saved()
            .in_progress_response
            .as_ref()
            .is_some_and(|progress| progress.results.len() == 2)
    })
    .await;
    assert!(
        result(&f.saved(), "b").unwrap().contains("true\nB result"),
        "B must observe A's completed work, not run alongside its invocation"
    );
    let output = json!([a, commentary, b]);
    stream.reply.send(completed(output.clone())).unwrap();
    let next = f.request().await;
    let input = next.body["input"].as_array().unwrap();
    assert_eq!(&input[1..4], output.as_array().unwrap());
    assert_eq!(input[4]["call_id"], "a");
    assert!(input[4]["output"].as_str().unwrap().contains("A result"));
    assert_eq!(input[5]["call_id"], "b");
    for id in ["a", "b"] {
        assert_eq!(
            f.c.snapshot()
                .entries
                .iter()
                .filter(|entry| entry.id == id)
                .count(),
            1
        );
    }
    next.reply.send(completed(json!([]))).unwrap();
    until(|| !f.c.is_turn_active()).await;
    assert!(f.saved().in_progress_response.is_none());
    assert!(!f.saved().needs_attention);
}

#[tokio::test]
async fn missing_earlier_items_cannot_reverse_execution_order() {
    let mut f = Fixture::new().await;
    let stream = f.request().await;
    let a = call("a", "store('ordered', 'first call finished');");
    let b = call("b", "text(load('ordered'));");
    stream.reply.send(done(1, &b)).unwrap();
    until(|| {
        f.saved()
            .in_progress_response
            .as_ref()
            .is_some_and(|progress| progress.items.contains_key(&1))
    })
    .await;
    assert!(f.c.snapshot().entries.by_id("b").is_none());
    // The final response supplies the missing earlier item, without repeating B.
    stream.reply.send(completed(json!([a, b]))).unwrap();
    let next = f.request().await;
    assert!(
        result(&f.saved(), "b")
            .unwrap()
            .contains("first call finished")
    );
    assert_eq!(next.body["input"][3]["call_id"], "a");
    assert_eq!(next.body["input"][4]["call_id"], "b");
    next.reply.send(completed(json!([]))).unwrap();
    until(|| !f.c.is_turn_active()).await;
    assert!(!f.saved().needs_attention);
}

#[tokio::test]
async fn yielding_releases_the_invocation_slot_without_stopping_the_cell() {
    let mut f = Fixture::new().await;
    let stream = f.request().await;
    let a = call(
        "a",
        &format!("yield_control(); {HOLD} text('background finished');"),
    );
    let b = call("b", "text('second invocation finished');");
    stream
        .reply
        .send(format!("{}{}", done(0, &a), done(1, &b)))
        .unwrap();
    until(|| {
        f.saved()
            .in_progress_response
            .as_ref()
            .is_some_and(|progress| progress.results.len() == 2)
    })
    .await;
    assert!(
        result(&f.saved(), "a")
            .unwrap()
            .contains("Script running with cell ID")
    );
    assert!(
        result(&f.saved(), "b")
            .unwrap()
            .contains("second invocation finished")
    );
    assert!(
        f.c.code.is_executing(),
        "yielded cell must still be running"
    );
    f.release_cell().await;
    until(|| !f.c.code.is_executing()).await;
    stream.reply.send(completed(json!([a, b]))).unwrap();
    f.finish().await;
}

#[tokio::test]
async fn stream_failure_stop_and_invalid_completion_preserve_results_without_replaying_calls() {
    for failure in [
        "disconnect",
        "context-limit",
        "stop",
        "stop-after-completion",
        "changed-call",
    ] {
        let mut f = Fixture::new().await;
        let stream = f.request().await;
        std::fs::write(
            f.dir.path().join("buffered.png"),
            crate::media::tests::png(),
        )
        .unwrap();
        let a = call(
            "a",
            "text(await tools.exec_command({cmd:\"printf 'once\\n' >> effects; printf 'saved result'\",login:false}));",
        );
        stream.reply.send(done(0, &a)).unwrap();
        until(|| {
            f.saved()
                .in_progress_response
                .as_ref()
                .is_some_and(|progress| progress.results.contains_key(&0))
        })
        .await;
        let expected = result(&f.saved(), "a").unwrap();
        assert!(expected.contains("saved result"));
        let b = call(
            "b",
            &format!(
                "// @exec: {{\"yield_time_ms\":300000}}\nimage(await tools.view_image({{path:'buffered.png'}})); text('partial recovery information'); notify('B started'); {HOLD} text('must not finish normally');"
            ),
        );
        stream.reply.send(done(1, &b)).unwrap();
        until(|| {
            f.saved()
                .pending_tool_outputs
                .iter()
                .any(|v| v["call_id"] == "b")
        })
        .await;
        let c = call(
            "c",
            "text(await tools.exec_command({cmd:'touch forbidden',login:false}));",
        );
        stream.reply.send(format!("{}data: {}\n\n", done(2, &c), json!({"type":"response.custom_tool_call_input.delta","item_id":"partial","delta":"text('never execute partial input')"}))).unwrap();
        until(|| {
            f.saved()
                .in_progress_response
                .as_ref()
                .unwrap()
                .items
                .contains_key(&2)
        })
        .await;
        let crash_checkpoint = f.saved();
        match failure {
            "disconnect" => drop(stream),
            "context-limit" => stream
                .reply
                .send(format!(
                    "data: {}\n\n",
                    json!({"type":"error","error":{"code":"context_length_exceeded"}})
                ))
                .unwrap(),
            "stop" => f.h.stop(&f.c.id),
            "stop-after-completion" => {
                // Capture the very first completion save, before any subsequent item save
                // can hide a checkpoint that recorded usage but omitted known output.
                f.h.store
                    .db
                    .lock()
                    .unwrap()
                    .execute_batch(
                        "CREATE TABLE completion_checkpoint(metadata TEXT);
                    CREATE TRIGGER capture_completion AFTER UPDATE ON sessions
                    WHEN json_extract(NEW.metadata, '$.usage.total_output') = 11
                        AND NOT EXISTS (SELECT 1 FROM completion_checkpoint)
                    BEGIN INSERT INTO completion_checkpoint VALUES (NEW.metadata); END;",
                    )
                    .unwrap();
                let reasoning = json!({"type":"reasoning","id":"late-reasoning","summary":[],"encrypted_content":"opaque-final-only-context"});
                stream
                    .reply
                    .send(completed(json!([a, b, c, reasoning])))
                    .unwrap();
                until(|| f.saved().usage.total_output == 11).await;
                let metadata: String =
                    f.h.store
                        .db
                        .lock()
                        .unwrap()
                        .query_row("SELECT metadata FROM completion_checkpoint", [], |row| {
                            row.get(0)
                        })
                        .unwrap();
                let checkpoint: Session = serde_json::from_str(&metadata).unwrap();
                assert_eq!(
                    checkpoint.in_progress_response.unwrap().items[&3],
                    reasoning
                );
                f.h.stop(&f.c.id);
            }
            "changed-call" => {
                let mut changed = a;
                changed["input"] = json!("text('different call')");
                stream
                    .reply
                    .send(completed(json!([changed, b, c])))
                    .unwrap();
            }
            _ => unreachable!(),
        }
        until(|| !f.c.is_turn_active() && !f.c.code.is_executing()).await;
        assert_eq!(
            std::fs::read_to_string(f.dir.path().join("effects")).unwrap(),
            "once\n"
        );
        assert!(!f.dir.path().join("forbidden").exists());
        assert!(
            f.requests.try_recv().is_err(),
            "failed response must not be retried or compacted from a stale snapshot"
        );
        assert_eq!(result(&f.saved(), "a").unwrap(), expected);
        assert!(
            result(&f.saved(), "b")
                .unwrap()
                .contains("partial recovery information")
        );
        assert!(matches!(&f.saved().entries.by_id("b").unwrap().kind,
            EntryKind::Tool { media, failed: true, .. } if media.len() == 1));
        assert!(f.saved().in_progress_response.is_none());
        assert_eq!(
            f.saved().needs_attention,
            !matches!(failure, "stop" | "stop-after-completion")
        );
        if failure == "stop-after-completion" {
            assert_eq!(
                f.saved().usage.total_output,
                11,
                "completed response usage survives cancellation while tools are active"
            );
        }

        // Reproduce an actual restart checkpoint, not just a gracefully failed turn.
        if failure == "disconnect" {
            f.h.store.save_session(&crash_checkpoint).unwrap();
        }
        let recovered = Harness::with_provider(
            f.h.store.clone(),
            vec![],
            Provider::fixture(f.h.store.clone(), f.endpoint.clone()),
        )
        .unwrap();
        let restored = recovered.load(&f.c.id).unwrap();
        let state = restored.snapshot();
        assert!(state.in_progress_response.is_none());
        assert_eq!(result(&state, "a").unwrap(), expected);
        for id in ["a", "b", "c"] {
            assert_eq!(
                state
                    .input
                    .iter()
                    .filter(|item| item["call_id"] == id && item.get("output").is_some())
                    .count(),
                1
            );
        }
        assert!(!state.input.iter().any(|item| item["id"] == "partial"));
        recovered.start(&restored.id, Settings::default()).unwrap();
        f.request().await.reply.send(completed(json!([]))).unwrap();
        until(|| !restored.is_turn_active()).await;
        assert_eq!(
            std::fs::read_to_string(f.dir.path().join("effects")).unwrap(),
            "once\n"
        );
        assert!(!f.dir.path().join("forbidden").exists());
    }
}

#[tokio::test]
async fn storage_failures_prevent_dispatch_and_retain_completed_results_for_save_retry() {
    for fail_after_start in [false, true] {
        let mut f = Fixture::new().await;
        let stream = f.request().await;
        let a = if fail_after_start {
            call(
                "a",
                &format!(
                    "// @exec: {{\"yield_time_ms\":300000}}\nnotify('started'); {HOLD} text('result retained despite storage failure');"
                ),
            )
        } else {
            call(
                "a",
                "text(await tools.exec_command({cmd:'touch forbidden',login:false}));",
            )
        };
        if fail_after_start {
            stream.reply.send(done(0, &a)).unwrap();
            until(|| !f.saved().pending_tool_outputs.is_empty()).await;
        }
        sqlite::fail_writes(
            &f.h.store.db.lock().unwrap(),
            "sessions",
            "injected disk failure",
        );
        if fail_after_start {
            f.release_cell().await;
        } else {
            stream.reply.send(done(0, &a)).unwrap();
        }
        until(|| !f.c.is_turn_active()).await;
        assert!(!f.dir.path().join("forbidden").exists());
        let state = f.c.snapshot();
        assert!(state.needs_attention);
        if fail_after_start {
            assert!(
                result(&state, "a")
                    .unwrap()
                    .contains("result retained despite storage failure")
            );
        } else {
            assert!(
                state.entries.by_id("a").is_none(),
                "failed call persistence must precede execution"
            );
        }
        sqlite::allow_writes(&f.h.store.db.lock().unwrap(), "sessions");
        f.h.flush().unwrap();
        assert!(f.saved().in_progress_response.is_none());
        if fail_after_start {
            assert!(f.saved().input.iter().any(|item| item["call_id"] == "a"
                && item["output"].as_str().is_some_and(|output| {
                    output.contains("result retained despite storage failure")
                })));
        }
    }
}

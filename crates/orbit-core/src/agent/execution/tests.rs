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
        "{}\n\n",
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
    assert_eq!(&input[2..5], output.as_array().unwrap());
    assert_eq!(input[5]["call_id"], "a");
    assert!(input[5]["output"].as_str().unwrap().contains("A result"));
    assert_eq!(input[6]["call_id"], "b");
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
    assert_eq!(next.body["input"][4]["call_id"], "a");
    assert_eq!(next.body["input"][5]["call_id"], "b");
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
        stream.reply.send(format!("{}{}\n\n", done(2, &c), json!({"type":"response.custom_tool_call_input.delta","item_id":"partial","delta":"text('never execute partial input')"}))).unwrap();
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
            "disconnect" => {
                drop(stream);
                until(|| {
                    f.c.snapshot()
                        .entries
                        .iter()
                        .any(|e| e.text.contains("Waiting for the running tool"))
                })
                .await;
                assert!(f.requests.try_recv().is_err());
                f.release_cell().await;
                let retry = f.request().await;
                assert!(retry.body.get("previous_response_id").is_none());
                let input = retry.body["input"].as_array().unwrap();
                assert!(input.iter().any(|i| i["call_id"] == "b"
                    && i["output"].to_string().contains("must not finish normally")));
                assert!(input.iter().any(
                    |i| i["call_id"] == "c" && i["output"].to_string().contains("Not executed")
                ));
                retry.reply.send(completed(json!([]))).unwrap();
            }
            "context-limit" => stream
                .reply
                .send(format!(
                    "{}\n\n",
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
            EntryKind::Tool { media, failed, .. } if media.len() == 1 && *failed == (failure != "disconnect")));
        assert!(f.saved().in_progress_response.is_none());
        assert_eq!(
            f.saved().needs_attention,
            !matches!(failure, "stop" | "stop-after-completion" | "disconnect")
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
async fn failed_tool_start_commit_rolls_back_without_dispatch() {
    let mut f = Fixture::new().await;
    let stream = f.request().await;
    // The raw call can commit. Writing its transcript entry succeeds too, but
    // the deferred constraint rejects COMMIT (including the terminal save).
    f.h.store.db.lock().unwrap().execute_batch("
        CREATE TABLE commit_guard (missing TEXT REFERENCES sessions(id) DEFERRABLE INITIALLY DEFERRED);
        CREATE TRIGGER reject_start AFTER INSERT ON entries WHEN json_extract(NEW.data,'$.id')='guarded'
        BEGIN INSERT INTO commit_guard VALUES ('absent'); END;
    ").unwrap();
    let call = call("guarded", "store('dispatched', true);");
    stream.reply.send(done(0, &call)).unwrap();
    until(|| !f.c.is_turn_active()).await;

    let durable = f.saved();
    assert_eq!(durable.in_progress_response.unwrap().items[&0], call);
    assert!(durable.entries.by_id("guarded").is_none());
    assert!(f.c.snapshot().needs_attention);

    f.h.store
        .db
        .lock()
        .unwrap()
        .execute_batch("DROP TRIGGER reject_start")
        .unwrap();
    f.h.flush().unwrap();
    let saved = f.saved();
    assert!(saved.in_progress_response.is_none());
    assert!(matches!(
        saved.entries.by_id("guarded").unwrap().kind,
        EntryKind::Tool {
            complete: true,
            failed: true,
            ..
        }
    ));
    let c = f.c.clone();
    let check = tokio::task::spawn_blocking(move || {
        c.code.exec(
            "text(load('dispatched') === true ? 'ran' : 'skipped');",
            "check",
            &CancellationToken::new(),
        )
    })
    .await
    .unwrap()
    .unwrap();
    assert!(check.contains("skipped"));
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

#[tokio::test]
async fn dropped_partial_output_retries_from_history_until_stopped_or_exhausted() {
    for stop in [true, false] {
        let mut f = Fixture::new().await;
        let first = f.request().await;
        let original = first.body["input"].clone();
        first.reply.send(json!({"type":"response.output_text.delta","item_id":"partial","delta":"unfinished sentence"}).to_string()).unwrap();
        drop(first);
        until(|| {
            f.saved()
                .entries
                .iter()
                .any(|e| e.text.contains("Reconnecting from saved progress"))
        })
        .await;
        if stop {
            f.h.stop(&f.c.id);
        } else {
            for _ in 0..MAX_RECOVERIES {
                let retry = f.request().await;
                assert_eq!(retry.body["input"], original);
                assert!(retry.body.get("previous_response_id").is_none());
                drop(retry);
            }
        }
        until(|| !f.c.is_turn_active()).await;
        assert!(f.requests.try_recv().is_err());
        let saved = f.saved();
        assert_eq!(saved.needs_attention, !stop);
        assert!(
            saved
                .entries
                .iter()
                .any(|e| matches!(e.kind, EntryKind::Notice)
                    && e.text.contains("unfinished sentence"))
        );
        assert_eq!(
            saved
                .entries
                .iter()
                .filter(|e| e.text.contains("exhausted"))
                .count(),
            usize::from(!stop)
        );
    }
}

/// Group makespan, not model latency: the peer replies immediately over loopback.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "offline release timing probe; run alone on the target filesystem"]
async fn local_turn_benchmark() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = crate::test_support::authenticated_store(dir.path().join("state"));
    let (tx, timings) = std::sync::mpsc::channel();
    store.save_trace = Some(tx);
    let (endpoint, mut requests, server) = streaming_server().await;
    let h =
        Harness::with_provider(store.clone(), vec![], Provider::fixture(store, endpoint)).unwrap();
    let source = call("audit-call", "text(42)");
    let first = json!({"type":"response.completed","response":{"id":"audit-response","status":"completed","output":[source]}}).to_string();
    let piece = "local stream λ ";
    let answer = piece.repeat(64);
    let delta = json!({"type":"response.output_text.delta","item_id":"answer","delta":piece});
    let final_batch = format!("{delta}\n").repeat(64)
        + &completed(json!([{
            "type":"message","role":"assistant","id":"answer","content":[{"type":"output_text","text":answer}]
        }]));
    for sample in 0..4 {
        for n in [1, 8] {
            let conversations: Vec<_> = (0..n)
                .map(|_| {
                    let mut session = Session::new(dir.path().into(), &Settings::default());
                    session.rename("Local benchmark".into());
                    session.accept("Run an offline tool".into());
                    h.insert(session)
                })
                .collect();
            let start = std::time::Instant::now();
            for c in &conversations {
                h.start(&c.id, Settings::default()).unwrap();
            }
            let mut chained = 0;
            tokio::time::timeout(Duration::from_secs(60), async {
                for _ in 0..2 * n {
                    let request = requests.recv().await.unwrap();
                    let reply = if request.body["previous_response_id"] == "audit-response" {
                        chained += 1;
                        assert!(
                            request.body["input"].as_array().unwrap().iter().any(
                                |item| item["call_id"] == "audit-call"
                                    && item["output"]
                                        .as_str()
                                        .is_some_and(|text| text.contains("42"))
                            )
                        );
                        &final_batch
                    } else {
                        &first
                    };
                    request.reply.send(reply.clone()).unwrap();
                }
                while conversations.iter().any(|c| c.is_turn_active()) {
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
            })
            .await
            .expect("local turns must finish");
            let elapsed = start.elapsed();
            assert_eq!(chained, n);
            // Drain/print after the timed interval. One session is exactly one turn here.
            for (id, timing) in timings.try_iter() {
                use crate::test_support::SaveTiming;
                let turn = conversations.iter().position(|c| c.id == id).unwrap();
                match timing {
                    SaveTiming::WorkerQueue(wait) => println!(
                        "SAVE sample={sample} conversations={n} turn={turn} worker_queue_us={:.3}",
                        wait.as_secs_f64() * 1e6
                    ),
                    SaveTiming::Database { wait, body, commit } => println!(
                        "TX sample={sample} conversations={n} turn={turn} db_wait_us={:.3} body_us={:.3} commit_us={:.3}",
                        wait.as_secs_f64() * 1e6,
                        body.as_secs_f64() * 1e6,
                        commit.as_secs_f64() * 1e6
                    ),
                }
            }
            for c in conversations {
                let persisted = h.store.load_session(&c.id).unwrap();
                assert_eq!(persisted.entries.by_id("answer").unwrap().text, answer);
            }
            println!(
                "TURN sample={sample} conversations={n} group_ms={:.3}",
                elapsed.as_secs_f64() * 1000.
            );
        }
    }
    server.abort();
}

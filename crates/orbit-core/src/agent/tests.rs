use super::*;
use std::time::Instant;
#[test]
#[ignore = "requires packaged CEF; run just browser-smoke"]
fn native_browser_tool_returns_session_media() {
    let dir = tempfile::tempdir().unwrap();
    let h = Harness::new(Store::open(dir.path().join("state")).unwrap(), vec![]).unwrap();
    let c = h.insert(Session::new(dir.path().into(), &Settings::default()));
    let out = c.code.exec_output(
        r#"// @exec: {"yield_time_ms":30000}
const v = await tools.browser({action:'viewport', width:391, height:843});
await tools.browser({action:'wait', url:'about:blank'});
const shot = await tools.browser({action:'screenshot'});
const ax = await tools.browser({action:'inspect'});
const measured = await tools.browser({action:'evaluate', expression:'({url:location.href,width:innerWidth})'});
if (!ax.nodes.some(n=>n.role==='document') || shot.width!==v.width || shot.height!==843 ||
measured.value.width!==shot.width || measured.value.url!==shot.url) throw Error('Unexpected browser result');
image(shot);"#, "browser-tool", &CancellationToken::new()).unwrap();
    assert_eq!(out.media().len(), 1, "{}", out.summary());
    assert!(out.media()[0].path.is_file());
    assert!(c.snapshot().entries.iter().any(|entry| matches!(&entry.kind,
        EntryKind::Tool { name,input,complete:true,failed:false,.. } if name=="browser" &&
          serde_json::from_str::<Value>(input).unwrap()["expression"] == "({url:location.href,width:innerWidth})")));
    h.browser.shutdown().unwrap();
}
#[test]
fn unrelated_send_and_stop_do_not_wait_for_session_persistence() {
    let dir = tempfile::tempdir().unwrap();
    let h = Harness::new(Store::open(dir.path().into()).unwrap(), vec![]).unwrap();
    let make = || {
        let mut s = Session::new(dir.path().into(), &Settings::default());
        s.rename("Probe".into());
        h.insert(s)
    };
    let a = make();
    let b = make();
    std::thread::scope(|scope| {
        let disk = a.persisted.lock().unwrap();
        let send = |id: &str| {
            crate::send_trace::run(crate::send_trace::queued(), || {
                h.send(id, "request", "Hello".into(), Settings::default())
            })
        };
        let a = &a;
        let first = scope.spawn(move || send(&a.id));
        let deadline = Instant::now() + Duration::from_secs(5);
        let family = h.family(&a.id);
        while family.try_lock().is_ok() {
            assert!(
                Instant::now() < deadline,
                "first send never entered acceptance"
            );
            std::thread::yield_now();
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let b = &b;
        let h = &h;
        let second = scope.spawn(move || {
            send(&b.id).unwrap();
            h.stop(&b.id);
            tx.send(()).unwrap();
        });
        let independent = rx.recv_timeout(Duration::from_secs(2));
        drop(disk);
        first.join().unwrap().unwrap();
        second.join().unwrap();
        independent.expect("unrelated submission and Stop should finish before releasing A");
    });
    for c in [a, b] {
        assert_eq!(
            h.store.load_session(&c.id).unwrap().entries[0].text,
            "Hello"
        );
    }
}

#[tokio::test]
async fn selected_skill_is_user_content_and_catalog_changes_append_after_reload() -> Result<()> {
    use crate::test_support::{authenticated_store, completed, streaming_server, until};
    let tmp = tempfile::tempdir()?;
    let dir = tmp.path().join(".agents/skills/release");
    std::fs::create_dir_all(&dir)?;
    let skill_file = dir.join("SKILL.md");
    std::fs::write(
        &skill_file,
        "---\nname: release\ndescription: Prepare a release\n---\nFirst workflow revision",
    )?;
    let mention = crate::skills::discover_with_home(tmp.path(), None)?.skills[0].mention();
    let store = authenticated_store(tmp.path().join("state"));
    let (url, mut requests, server) = streaming_server().await;
    let h = Harness::with_provider(store.clone(), vec![], Provider::fixture(store, url))?;
    let settings = Settings::default();
    let mut session = Session::new(tmp.path().into(), &settings);
    session.rename("Release fixture".into());
    let id = session.id.clone();
    h.send_new(
        session,
        "first",
        format!("Use {mention}").into(),
        settings.clone(),
    )?;
    let req = tokio::time::timeout(Duration::from_secs(5), requests.recv())
        .await?
        .unwrap();
    let prefix = req.body["input"].as_array().unwrap().clone();
    assert_eq!(prefix[0]["role"], "developer");
    assert!(!prefix[0].to_string().contains("Prepare a release"));
    assert_eq!(prefix[1]["role"], "developer");
    assert!(
        prefix[1]["content"]
            .as_str()
            .unwrap()
            .contains("Prepare a release")
    );
    assert!(!prefix[1].to_string().contains("First workflow revision"));
    assert_eq!(prefix[2]["role"], "user");
    assert!(
        prefix[2]["content"][1]["text"]
            .as_str()
            .unwrap()
            .contains("First workflow revision")
    );
    req.reply.send(completed(json!([]))).unwrap();
    until(|| !h.get(&id).unwrap().is_turn_active()).await;
    h.evict_inactive(None)?;
    std::fs::write(
        &skill_file,
        "---\nname: release\ndescription: Updated release checklist\n---\nNew workflow body",
    )?;
    h.send(&id, "second", "Continue".into(), settings)?;
    let req = tokio::time::timeout(Duration::from_secs(5), requests.recv())
        .await?
        .unwrap();
    let input = req.body["input"].as_array().unwrap();
    assert!(input.starts_with(&prefix));
    let suffix = &input[prefix.len()..];
    assert_eq!(suffix[0]["role"], "developer");
    assert!(
        suffix[0]["content"]
            .as_str()
            .unwrap()
            .contains("Updated release checklist")
    );
    assert_eq!(suffix[1]["content"], "Continue");
    assert!(!req.body.to_string().contains("New workflow body"));
    req.reply.send(completed(json!([]))).unwrap();
    until(|| !h.get(&id).unwrap().is_turn_active()).await;
    h.edit(&id, |s| s.usage.input = Settings::default().context_limit)?;
    h.send(
        &id,
        "third",
        format!("Now use {mention}").into(),
        Settings::default(),
    )?;
    let req = tokio::time::timeout(Duration::from_secs(5), requests.recv())
        .await?
        .unwrap();
    assert_eq!(
        req.body["input"].as_array().unwrap().last().unwrap()["type"],
        "compaction_trigger"
    );
    req.reply
        .send(completed(
            json!([{"type":"compaction","encrypted_content":"opaque"}]),
        ))
        .unwrap();
    let req = tokio::time::timeout(Duration::from_secs(5), requests.recv())
        .await?
        .unwrap();
    let fresh = req.body["input"].as_array().unwrap();
    assert!(fresh.iter().any(|item| item["role"] == "developer"
        && item.to_string().contains("Updated release checklist")));
    let latest = fresh.last().unwrap();
    assert_eq!(latest["role"], "user");
    assert_eq!(latest["content"][1]["type"], "input_text");
    assert!(
        latest["content"][1]["text"]
            .as_str()
            .unwrap()
            .contains("New workflow body")
    );
    req.reply.send(completed(json!([]))).unwrap();
    until(|| !h.get(&id).unwrap().is_turn_active()).await;
    server.abort();
    Ok(())
}

#[tokio::test]
async fn project_guidance_changes_append_and_compaction_keeps_only_the_current_version() {
    use crate::test_support::{authenticated_store, completed, streaming_server, until};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("AGENTS.md");
    let store = authenticated_store(dir.path().join("state"));
    let (url, mut requests, server) = streaming_server().await;
    let h = Harness::with_provider(store.clone(), vec![], Provider::fixture(store, url)).unwrap();
    let settings = Settings::default();
    let mut session = Session::new(dir.path().into(), &settings);
    session.rename("Guidance fixture".into());
    let id = session.id.clone();
    h.insert(session);

    let mut previous = Vec::new();
    for (step, (file, expected_update)) in [
        (Some("Use sapphire."), Some("Use sapphire.")),
        (Some("Use sapphire."), None),
        (Some("Use amber."), Some("Use amber.")),
        (None, Some("no longer apply")),
        (None, None),
    ]
    .into_iter()
    .enumerate()
    {
        if let Some(text) = file {
            std::fs::write(&path, text).unwrap();
        } else if path.exists() {
            std::fs::remove_file(&path).unwrap();
        }
        if step == 2 {
            h.edit(&id, |s| s.usage.input = settings.context_limit)
                .unwrap();
        }
        let submission = format!("turn-{step}");
        h.send(
            &id,
            &submission,
            submission.clone().into(),
            settings.clone(),
        )
        .unwrap();
        let request = tokio::time::timeout(Duration::from_secs(5), requests.recv())
            .await
            .unwrap()
            .unwrap();
        let input = request.body["input"].as_array().unwrap();
        assert_eq!(input[0]["role"], "developer");
        assert!(!input[0].to_string().contains("Use sapphire."));
        assert!(input.starts_with(&previous));
        let start = previous.len().max(1);
        let mut suffix = &input[start..];
        if let Some(update) = expected_update {
            assert_eq!(suffix[0]["role"], "user");
            assert!(suffix[0]["content"].as_str().unwrap().contains(update));
            suffix = &suffix[1..];
        }
        assert_eq!(suffix[0]["content"], submission);
        if step == 2 {
            assert_eq!(suffix.len(), 2);
            assert_eq!(suffix[1]["type"], "compaction_trigger");
            let compact = json!({"type":"compaction", "encrypted_content":"opaque"});
            request.reply.send(completed(json!([compact]))).unwrap();
            let next = tokio::time::timeout(Duration::from_secs(5), requests.recv())
                .await
                .unwrap()
                .unwrap();
            previous = next.body["input"].as_array().unwrap().clone();
            assert_eq!(previous.len(), 3);
            assert_eq!(previous[0], input[0]);
            assert_eq!(previous[1], compact);
            assert_eq!(previous[2], input[start]);
            next.reply.send(completed(json!([]))).unwrap();
        } else {
            assert_eq!(suffix.len(), 1);
            previous = input.clone();
            request.reply.send(completed(json!([]))).unwrap();
        }
        until(|| !h.get(&id).unwrap().is_turn_active()).await;
        // Exercise the durable context rather than an in-memory "already sent" flag.
        h.evict_inactive(None).unwrap();
    }
    server.abort();
}

#[tokio::test]
async fn effort_changes_chain_and_survive_replay_and_compaction() -> Result<()> {
    use crate::test_support::{authenticated_store, completed, streaming_server, until};
    let dir = tempfile::tempdir()?;
    let store = authenticated_store(dir.path().join("state"));
    let (url, mut requests, server) = streaming_server().await;
    let h = Harness::with_provider(store.clone(), vec![], Provider::fixture(store, url))?;
    let settings = Settings::default();
    let mut s = Session::new(dir.path().into(), &settings);
    s.rename("Effort fixture".into());
    s.model = crate::models::LUNA.id.into();
    s.effort = "low".into();
    let id = h.insert(s).id.clone();
    let update = json!({"type":"configuration_update","reasoning":{"effort":"none"}});
    let compact = json!({"type":"compaction","encrypted_content":"opaque"});
    for turn in 0..4 {
        let msg = format!("turn {turn}");
        if turn == 1 {
            h.edit(&id, |s| s.effort = "none".into())?;
        } else if turn == 2 {
            h.evict_inactive(None)?;
            h.edit(&id, |s| s.usage.input = settings.context_limit)?;
        } else if turn == 3 {
            h.edit(&id, |s| {
                s.model = crate::models::SOL.id.into();
                s.effort = "high".into();
            })?;
        }
        h.send(&id, &msg, msg.clone().into(), settings.clone())?;
        let req = tokio::time::timeout(Duration::from_secs(5), requests.recv())
            .await?
            .unwrap();
        let input = req.body["input"].as_array().unwrap();
        assert_eq!(
            req.body["reasoning"]["effort"],
            if turn == 3 { "high" } else { "low" }
        );
        match turn {
            0 => {
                assert_eq!(input.len(), 2);
                assert_eq!(input[1]["content"], msg);
                req.reply.send(json!({"type":"response.completed","response":{"status":"completed","id":"first","output":[]}}).to_string())?;
            }
            1 => {
                assert_eq!(req.body["previous_response_id"], "first");
                assert_eq!(
                    input,
                    &[
                        update.clone(),
                        json!({"type":"message","role":"user","content":msg})
                    ]
                );
                req.reply.send(completed(json!([])))?;
            }
            2 => {
                assert!(req.body.get("previous_response_id").is_none());
                assert_eq!(input.len(), 6);
                assert_eq!(input[1]["content"], "turn 0");
                assert_eq!(input[2], update);
                assert_eq!(input[3]["content"], "turn 1");
                assert_eq!(input[4]["content"], msg);
                assert_eq!(input[5]["type"], "compaction_trigger");
                req.reply.send(completed(json!([compact])))?;
                let next = tokio::time::timeout(Duration::from_secs(5), requests.recv())
                    .await?
                    .unwrap();
                assert_eq!(next.body["reasoning"]["effort"], "low");
                assert_eq!(next.body["input"], json!([input[0], compact, update]));
                next.reply.send(completed(json!([])))?;
            }
            _ => {
                // Luna's "none" cannot be sent to Sol, but its conversation is retained.
                assert_eq!(
                    req.body["input"],
                    json!([input[0],compact,{"type":"message","role":"user","content":msg}])
                );
                req.reply.send(completed(json!([])))?;
            }
        }
        until(|| !h.get(&id).unwrap().is_turn_active()).await;
    }
    server.abort();
    Ok(())
}

#[tokio::test]
async fn filing_persists_retries_and_only_accepted_messages_restore() {
    for kind in [FilingKind::Settled, FilingKind::Deferred] {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().into()).unwrap();
        let h = Harness::new(store.clone(), vec![]).unwrap();
        let mut session = Session::new(dir.path().into(), &Settings::default());
        session.updated = 77;
        session.draft = "Unsent draft λ".into();
        let c = h.insert(session);
        c.save().unwrap();
        let revision = h.catalog_revision();
        assert_eq!(
            h.file_session(&c.id, FilingAction::File(kind)).unwrap(),
            FilingOutcome::Applied
        );
        let filed = c.snapshot().filing.unwrap();
        assert_eq!(filed.kind, kind);
        assert!(h.catalog_revision() > revision);
        assert_eq!(store.load().unwrap().1[0].filing, Some(filed));
        assert_eq!(store.load_session(&c.id).unwrap().updated, 77);
        h.edit(&c.id, |s| s.filing = Some(Filing { kind, at: 123 }))
            .unwrap();
        assert_eq!(
            h.file_session(&c.id, FilingAction::File(kind)).unwrap(),
            FilingOutcome::Applied
        );
        h.evict_inactive(None).unwrap();
        let c = h.load(&c.id).unwrap();
        assert_eq!(c.snapshot().filing, Some(Filing { kind, at: 123 }));
        assert_eq!(c.snapshot().draft, "Unsent draft λ");

        assert_eq!(
            h.file_session(&c.id, FilingAction::Undo).unwrap(),
            FilingOutcome::Applied
        );
        assert_eq!(store.load().unwrap().1[0].filing, None);
        assert_eq!(c.snapshot().updated, 77);
        assert_eq!(
            h.file_session(&c.id, FilingAction::File(kind)).unwrap(),
            FilingOutcome::Applied
        );
        assert!(c.snapshot().filing.unwrap().at >= filed.at);

        let db = rusqlite::Connection::open(store.root().join("sessions.sqlite3")).unwrap();
        db.execute_batch("CREATE TRIGGER fail_settlement BEFORE UPDATE ON sessions BEGIN SELECT RAISE(FAIL,'disk fixture'); END;").unwrap();
        assert!(h.file_session(&c.id, FilingAction::Undo).is_err());
        assert!(
            c.snapshot().filing.is_none(),
            "retain the edit for save retry"
        );
        assert!(store.load_session(&c.id).unwrap().filing.is_some());
        db.execute_batch("DROP TRIGGER fail_settlement;").unwrap();
        h.flush().unwrap();
        assert!(store.load_session(&c.id).unwrap().filing.is_none());
        assert_eq!(
            h.file_session(&c.id, FilingAction::File(kind)).unwrap(),
            FilingOutcome::Applied
        );

        assert!(
            h.send(&c.id, "empty", "".into(), Settings::default())
                .is_err()
        );
        assert!(c.snapshot().filing.is_some());
        db.execute_batch("CREATE TRIGGER fail_send BEFORE UPDATE ON sessions BEGIN SELECT RAISE(FAIL,'disk fixture'); END;").unwrap();
        assert!(
            h.send(&c.id, "retry", "Continue".into(), Settings::default())
                .is_err()
        );
        assert!(
            c.snapshot().filing.is_some(),
            "failed acceptance must not unsettle"
        );
        db.execute_batch("DROP TRIGGER fail_send;").unwrap();
        // No credentials: acceptance is local, then the provider fails offline.
        h.send(&c.id, "retry", "Continue".into(), Settings::default())
            .unwrap();
        assert!(c.snapshot().filing.is_none());
        assert!(store.load_session(&c.id).unwrap().filing.is_none());
        let mut changes = h.subscribe();
        while c.is_executing() {
            tokio::time::timeout(Duration::from_secs(3), changes.changed())
                .await
                .unwrap()
                .unwrap();
        }
    }
}

#[tokio::test]
async fn settlement_rejects_unread_attention_work_questions_and_children() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().into()).unwrap();
    let h = Harness::new(store, vec![]).unwrap();
    let c = h.insert(Session::new(dir.path().into(), &Settings::default()));
    for flag in 0..5 {
        h.edit(&c.id, |s| {
            s.unchecked_completion = flag == 0;
            s.needs_attention = flag == 1;
            s.interrupted = flag == 2;
        })
        .unwrap();
        c.running.store(flag == 3, Ordering::Release);
        c.finishing.store(flag == 4, Ordering::Release);
        assert!(
            h.file_session(&c.id, FilingAction::File(FilingKind::Settled))
                .is_err(),
            "blocked state {flag}"
        );
        assert!(c.snapshot().filing.is_none());
    }
    c.finishing.store(false, Ordering::Release);
    let q = c.questionnaires.as_ref().unwrap().clone();
    let cancel = CancellationToken::new();
    let token = cancel.clone();
    let mut changes = h.subscribe();
    let pending = tokio::spawn(async move {
        q.ask(
            "settle-question",
            r#"{"questions":[{"id":"q","kind":"text","question":"Continue?"}]}"#,
            &token,
        )
        .await
    });
    while !c.questionnaires.as_ref().unwrap().has_pending() {
        tokio::time::timeout(Duration::from_secs(3), changes.changed())
            .await
            .unwrap()
            .unwrap();
    }
    assert!(
        h.file_session(&c.id, FilingAction::File(FilingKind::Settled))
            .is_err()
    );
    cancel.cancel();
    assert!(pending.await.unwrap().is_err());

    let mut child = Session::new(dir.path().into(), &Settings::default());
    child.child = Some(subagents::Child {
        parent_id: c.id.clone(),
        profile: subagents::Profile::Explore,
    });
    let child = h.insert(child);
    let mut run = subagents::RunInfo {
        configuration: subagents::Profile::Explore.defaults(),
        run_id: "child-run".into(),
        session_id: child.id.clone(),
        parent_id: c.id.clone(),
        request_id: "request".into(),
        profile: subagents::Profile::Explore,
        title: "Inspect".into(),
        status: subagents::RunStatus::Running,
        assignment: 1,
        started: 1,
        finished: None,
        notified: true,
        attention_acknowledged: false,
    };
    h.run_index().record(run.clone());
    assert!(
        h.file_session(&c.id, FilingAction::File(FilingKind::Settled))
            .is_err()
    );
    run.status = subagents::RunStatus::Failed;
    h.run_index().record(run.clone());
    assert!(
        h.file_session(&c.id, FilingAction::File(FilingKind::Settled))
            .is_err()
    );
    run.attention_acknowledged = true;
    h.run_index().record(run);
    child.finishing.store(true, Ordering::Release);
    assert!(
        h.file_session(&c.id, FilingAction::File(FilingKind::Settled))
            .is_err(),
        "terminal assignment still finalizing"
    );
    child.finishing.store(false, Ordering::Release);
    assert_eq!(
        h.file_session(&c.id, FilingAction::File(FilingKind::Settled))
            .unwrap(),
        FilingOutcome::Applied
    );
    assert!(c.snapshot().filing.is_some());
    assert!(
        h.file_session(&child.id, FilingAction::File(FilingKind::Settled))
            .is_err()
    );
}

#[test]
fn filing_confirms_only_lingering_commands_in_the_clicked_session() {
    for kind in [FilingKind::Settled, FilingKind::Deferred] {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().into()).unwrap();
        let h = Harness::new(store.clone(), vec![]).unwrap();
        let start_session = || {
            let c = h.insert(Session::new(dir.path().into(), &Settings::default()));
            let output = c.code.exec(
            r#"text(await tools.exec_command({cmd:"sleep 60 & wait", login:false, yield_time_ms:0}));"#,
            "outer", &CancellationToken::new(),
        ).unwrap();
            assert!(output.contains("Process running with session ID"));
            c
        };
        let clicked = start_session();
        let other = start_session();
        assert_eq!(
            h.file_session(&clicked.id, FilingAction::File(kind))
                .unwrap(),
            FilingOutcome::RunningCommands(kind)
        );
        assert!(clicked.snapshot().filing.is_none());
        assert!(clicked.code.shell().has_running_commands());

        // Confirmation cannot override activity that appeared with the dialog open.
        h.edit(&clicked.id, |s| s.unchecked_completion = true)
            .unwrap();
        assert!(
            h.file_session(&clicked.id, FilingAction::StopCommandsAndFile(kind))
                .is_err()
        );
        assert!(clicked.code.shell().has_running_commands());
        h.edit(&clicked.id, |s| s.unchecked_completion = false)
            .unwrap();
        assert_eq!(
            h.file_session(&clicked.id, FilingAction::StopCommandsAndFile(kind))
                .unwrap(),
            FilingOutcome::Applied
        );
        assert_eq!(
            store
                .load_session(&clicked.id)
                .unwrap()
                .filing
                .unwrap()
                .kind,
            kind
        );
        assert!(!clicked.code.shell().has_running_commands());
        assert!(other.code.shell().has_running_commands());
        assert!(other.snapshot().filing.is_none());
    }
}

#[test]
fn command_status_tracks_yielded_processes_without_polling() {
    use crate::shell::CommandStatus;
    for (tty, exit, stop) in [(false, 0, false), (true, 7, false), (false, 0, true)] {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().into()).unwrap();
        let h = Harness::new(store.clone(), vec![]).unwrap();
        let c = h.insert(Session::new(dir.path().into(), &Settings::default()));
        let args = json!({
            "cmd": format!("while [ ! -f release ]; do sleep 0.01; done; printf finished; exit {exit}"),
            "login": false, "tty": tty, "yield_time_ms": 0
        });
        let result = c
            .code
            .exec(
                &format!("text(await tools.exec_command({args}));"),
                "outer",
                &CancellationToken::new(),
            )
            .unwrap();
        assert!(
            result.contains("Process running with session ID"),
            "{result}"
        );
        let snapshot = c.snapshot();
        let entry = snapshot
            .entries
            .iter()
            .find(|e| matches!(&e.kind, EntryKind::Tool { name, .. } if name == "exec_command"))
            .unwrap();
        let entry_id = entry.id.clone();
        let EntryKind::Tool {
            complete,
            failed,
            output,
            command_status,
            ..
        } = &entry.kind
        else {
            unreachable!()
        };
        assert!(
            *complete && !failed,
            "invocation returned successfully while the process lives"
        );
        assert_eq!(*command_status, Some(CommandStatus::Running));
        let original_output = output.clone();
        h.evict_inactive(None).unwrap();
        assert!(
            c.loaded.load(Ordering::Acquire),
            "live commands must stay observable across session switches"
        );
        if stop {
            c.cancel();
        } else {
            std::fs::write(dir.path().join("release"), "").unwrap();
        }
        let expected = if stop || exit != 0 {
            CommandStatus::Failed
        } else {
            CommandStatus::Succeeded
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let saved = store.load_session(&c.id).unwrap();
            if matches!(&saved.entries.by_id(&entry_id).unwrap().kind,
                EntryKind::Tool { command_status: Some(status), .. } if *status == expected)
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "process status was not persisted automatically"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(
            matches!(&c.snapshot().entries.by_id(&entry_id).unwrap().kind,
            EntryKind::Tool { complete: true, failed: false, output, .. } if output == &original_output),
            "exit status must not rewrite the original tool result"
        );
    }
}

#[test]
fn restart_marks_only_live_command_tracking_interrupted() {
    use crate::shell::CommandStatus;
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().into()).unwrap();
    let mut session = Session::new(dir.path().into(), &Settings::default());
    for (id, status) in [
        ("running", CommandStatus::Running),
        ("success", CommandStatus::Succeeded),
        ("failed", CommandStatus::Failed),
    ] {
        session.entries.push(Entry {
            id: id.into(),
            text: String::new(),
            kind: EntryKind::Tool {
                name: "exec_command".into(),
                input: "{}".into(),
                output: "original result".into(),
                complete: true,
                failed: false,
                command_status: Some(status),
                cell_id: None,
                parent: Some("outer".into()),
                media: vec![],
                web: None,
            },
        });
    }
    store.save_session(&session).unwrap();
    let h = Harness::new(store.clone(), store.load().unwrap().1).unwrap();
    h.load(&session.id).unwrap();
    let saved = store.load_session(&session.id).unwrap();
    for (id, expected) in [
        ("running", CommandStatus::Interrupted),
        ("success", CommandStatus::Succeeded),
        ("failed", CommandStatus::Failed),
    ] {
        assert!(matches!(&saved.entries.by_id(id).unwrap().kind,
            EntryKind::Tool { command_status: Some(status), output, .. } if *status == expected && output == "original result"));
    }
}

#[test]
fn failed_restart_repair_retries_all_changes_before_publishing() {
    use crate::{shell::CommandStatus, store::InProgressResponse, test_support::sqlite};
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().into()).unwrap();
    let mut checkpoint = Session::new(dir.path().into(), &Settings::default());
    checkpoint.interrupted = true;
    checkpoint.in_progress_response = Some(InProgressResponse {
        items: [(
            0usize,
            json!({"type":"custom_tool_call","call_id":"outer","name":"exec","input":"work"}),
        )]
        .into_iter()
        .collect(),
        ..Default::default()
    });
    for (name, status) in [
        ("exec_command", Some(CommandStatus::Running)),
        ("web__run", None),
    ] {
        checkpoint.entries.push(Entry {
            id: name.into(),
            text: String::new(),
            kind: EntryKind::Tool {
                name: name.into(),
                input: "{}".into(),
                output: String::new(),
                complete: status.is_some(),
                command_status: status,
                failed: false,
                cell_id: None,
                parent: Some("outer".into()),
                media: vec![],
                web: None,
            },
        });
    }
    store.save_session(&checkpoint).unwrap();
    let id = &checkpoint.id;
    let harness = Harness::new(store.clone(), store.load().unwrap().1).unwrap();
    let db = rusqlite::Connection::open(store.root().join("sessions.sqlite3")).unwrap();
    // Fail first at the original early write, then at the context write
    // after the entry and metadata changes. Neither may publish partial state.
    for table in ["sessions", "context"] {
        sqlite::fail_writes(&db, table, "fixture recovery failure");
        assert!(harness.load(id).is_err());
        assert!(harness.get(id).is_none());
        let unchanged = store.load_session(id).unwrap();
        assert!(unchanged.interrupted && unchanged.in_progress_response.is_some());
        assert!(unchanged.entries.iter().eq(checkpoint.entries.iter()));
        sqlite::allow_writes(&db, table);
    }
    let recovered = harness.load(id).unwrap().snapshot();
    assert!(recovered.needs_attention && !recovered.interrupted);
    assert!(recovered.in_progress_response.is_none());
    assert!(matches!(
        recovered.entries[0].kind,
        EntryKind::Tool {
            command_status: Some(CommandStatus::Interrupted),
            ..
        }
    ));
    assert!(
        matches!(&recovered.entries[1].kind, EntryKind::Tool { complete: true, failed: true, output, .. } if output.contains("not automatically replayed"))
    );
    assert_eq!(recovered.input.len(), 2);
    assert_eq!(recovered.input[1]["type"], "custom_tool_call_output");
    assert_eq!(recovered.input[1]["call_id"], "outer");
    drop(harness);
    let restarted = Harness::new(store.clone(), store.load().unwrap().1).unwrap();
    let reopened = restarted.load(id).unwrap().snapshot();
    assert!(
        reopened.entries.iter().eq(recovered.entries.iter()),
        "no duplicate recovery notices"
    );
    assert!(reopened.input.iter().eq(recovered.input.iter()));
}

#[test]
fn restart_finishes_abandoned_web_calls_without_replay_or_metadata_loss() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().into()).unwrap();
    let mut session = Session::new(dir.path().into(), &Settings::default());
    let id = session.id.clone();
    let metadata = crate::web_search::Metadata {
        searched_at: 123,
        sources: vec![],
        details: "UI-only metadata".into(),
    };
    session.entries.push(Entry {
        id: "finished".into(),
        text: String::new(),
        kind: EntryKind::Tool {
            name: "web__run".into(),
            input: "{}".into(),
            output: "completed text".into(),
            complete: true,
            failed: false,
            command_status: None,
            cell_id: None,
            parent: Some("outer".into()),
            media: vec![],
            web: Some(metadata.clone()),
        },
    });
    session.entries.push(Entry {
        id: "abandoned".into(),
        text: String::new(),
        kind: EntryKind::Tool {
            name: "web__run".into(),
            input: "{}".into(),
            output: String::new(),
            complete: false,
            failed: false,
            command_status: None,
            cell_id: None,
            parent: Some("outer".into()),
            media: vec![],
            web: None,
        },
    });
    store.save_session(&session).unwrap();
    let harness = Harness::new(store.clone(), store.load().unwrap().1).unwrap();
    let conversation = harness.load(&id).unwrap();
    assert!(!conversation.is_executing());
    let saved = store.load_session(&id).unwrap();
    assert!(
        matches!(&saved.entries.by_id("finished").unwrap().kind,EntryKind::Tool{output,web:Some(actual),..} if output=="completed text" && actual==&metadata)
    );
    assert!(
        matches!(&saved.entries.by_id("abandoned").unwrap().kind,EntryKind::Tool{output,complete:true,failed:true,..} if output.contains("not automatically replayed"))
    );
    assert!(
        !serde_json::to_string(&plaintext_context(&saved))
            .unwrap()
            .contains("UI-only metadata")
    );
}

#[test]
fn failed_async_questionnaire_post_never_publishes_or_warns_of_uncertain_side_effects() -> Result<()>
{
    let dir = tempfile::tempdir()?;
    let store = Store::open(dir.path().into())?;
    let h = Harness::new(store.clone(), vec![])?;
    let c = h.insert(Session::new(dir.path().into(), &Settings::default()));
    c.save()?;
    let db = rusqlite::Connection::open(store.root().join("sessions.sqlite3"))?;
    crate::test_support::sqlite::fail_writes(&db, "entries", "fixture post failure");
    let result = c.code.exec(
        "text(await tools.ask_user_async({questions:[{id:'q',kind:'text',question:'Preference?'}]}));",
        "cell", &CancellationToken::new()
    )?;
    assert!(
        result.contains("Questionnaire not posted") && result.contains("fixture post failure"),
        "{result}"
    );
    assert!(
        !result.contains("Side effects may have occurred"),
        "{result}"
    );
    assert!(h.take_notifications().is_empty());
    assert!(c.snapshot().async_questions.is_empty());
    assert!(store.load_session(&c.id)?.async_questions.is_empty());
    crate::test_support::sqlite::allow_writes(&db, "entries");
    c.save()?;
    let saved = store.load_session(&c.id)?;
    assert!(saved.async_questions.is_empty());
    assert!(saved.entries.iter().any(|entry|matches!(&entry.kind,
        EntryKind::Tool {name, complete:true, failed:true, output,..} if name=="ask_user_async" && output.contains("fixture post failure"))));
    Ok(())
}

#[tokio::test]
async fn async_questionnaire_answers_steer_and_resume_without_replaying_the_tool() -> Result<()> {
    use crate::test_support::{completed, sqlite, streaming_server, until};

    let dir = tempfile::tempdir()?;
    let store = crate::test_support::authenticated_store(dir.path().into());
    let settings = Settings::default();
    let (url, mut requests, server) = streaming_server().await;
    let open = || {
        Harness::with_provider(
            store.clone(),
            store.load()?.1,
            Provider::fixture(store.clone(), url.clone()),
        )
    };
    let h = open()?;
    let mut session = Session::new(dir.path().into(), &settings);
    session.rename("Async questionnaire fixture".into());
    let c = h.insert(session);
    let id = c.id.clone();
    h.send(&id, "initial", "Draft a report".into(), settings.clone())?;
    let timeout = Duration::from_secs(5);
    let first = tokio::time::timeout(timeout, requests.recv())
        .await?
        .unwrap();
    let source = r#"for (const questions of [
        [{id:"audience",question:"Audience?",kind:"text"}, {id:"locale",question:"Which language?",kind:"text"}],
        [{id:"length",question:"Length?",kind:"text"}]
    ]) text(await tools.ask_user_async({questions}));"#;
    first.reply.send(completed(
        json!([{"type":"custom_tool_call","name":"exec","call_id":"ask","input":source}]),
    ))?;
    // A new model request, not just a yielded cell, before either answer.
    let working = tokio::time::timeout(timeout, requests.recv())
        .await?
        .unwrap();
    let out = working.body["input"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["call_id"] == "ask" && v.get("output").is_some())
        .unwrap();
    assert_eq!(
        out["output"]
            .as_str()
            .unwrap()
            .matches(r#"{"accepted":true}"#)
            .count(),
        2
    );
    let pending = store.load_session(&id)?.async_questions;
    let [early, late] = &pending[..] else {
        panic!("both questions should be durable")
    };
    for q in &pending {
        assert!(
            matches!(&c.snapshot().entries.by_id(&q.id).unwrap().kind, EntryKind::Tool {complete:true, failed:false, output, ..} if output == r#"{"accepted":true}"#)
        );
    }

    h.edit(&id, |s| s.draft = "Unrelated composer draft".into())?;
    let db = rusqlite::Connection::open(store.root().join("sessions.sqlite3"))?;
    sqlite::fail_writes(&db, "entries", "fixture answer write failure");
    std::fs::write(dir.path().join("private-notes"), "not an attachment")?;
    let answer = "  Engineers\n@private-notes worldwide\n\n[locale] Answer  ";
    let reply = || {
        h.answer_question(
            &id,
            &early.id,
            vec![answer.into(), "français".into()],
            settings.clone(),
        )
    };
    assert!(format!("{:#}", reply().unwrap_err()).contains("fixture answer write failure"));
    assert_eq!(c.snapshot().async_questions, pending);
    assert_eq!(store.load_session(&id)?.async_questions, pending);
    sqlite::allow_writes(&db, "entries");
    reply()?;
    assert!(reply().is_err(), "do not accept a stale submission");
    let state = store.load_session(&id)?;
    assert_eq!(state.draft, "Unrelated composer draft");
    assert_eq!(state.pending.len(), 1);
    let text = &state.pending[0].text;
    assert_eq!(
        text,
        &format!(
            "Answers to questionnaire {}:\n\n[audience] Audience?\n{answer}\n\n[locale] Which language?\nfrançais",
            early.id
        )
    );
    let display = &state
        .entries
        .by_id(&state.pending_submissions[0])
        .unwrap()
        .text;
    assert_eq!(
        display,
        &format!("Audience?\nAnswer\n{answer}\n\nWhich language?\nAnswer\nfrançais")
    );
    assert_eq!(
        plaintext_context(&state).last().unwrap()["content"],
        *text,
        "rebuilding agent context from a saved transcript retains the IDs"
    );
    working.reply.send(completed(json!([])))?;
    let steered = tokio::time::timeout(timeout, requests.recv())
        .await?
        .unwrap();
    let inputs = steered.body["input"].as_array().unwrap();
    assert_eq!(
        inputs
            .iter()
            .filter(|v| v["role"] == "user" && v["content"] == *text)
            .count(),
        1
    );
    steered.reply.send(completed(json!([])))?;
    until(|| !c.is_executing()).await;
    assert_eq!(c.status(), ConversationStatus::Completed);

    assert!(
        h.file_session(&id, FilingAction::File(FilingKind::Settled))
            .is_err(),
        "an otherwise idle session still has an unanswered questionnaire"
    );

    // Stop a later turn with a question outstanding, then reload as on restart.
    h.send(&id, "next", "Check sources".into(), settings.clone())?;
    let front = h.take_notifications().pop().expect("outstanding question");
    assert_eq!(front.kind, notifications::Kind::Question(late.id.clone()));
    let _stopped_request = tokio::time::timeout(timeout, requests.recv())
        .await?
        .unwrap();
    h.stop(&id);
    until(|| !c.is_executing()).await;
    assert!(h.notification_is_current(&front));
    drop(c);
    drop(h);
    let h = open()?;
    assert!(h.summaries()[0].has_async_question);
    let c = h.load(&id)?;
    assert!(!c.is_executing());
    assert!(h.take_notifications().is_empty());
    assert_eq!(
        c.snapshot().async_questions.as_slice(),
        std::slice::from_ref(late)
    );
    h.answer_question(&id, &late.id, vec!["Two pages".into()], settings)?;
    let resumed = tokio::time::timeout(timeout, requests.recv())
        .await?
        .unwrap();
    let message = resumed.body["input"].as_array().unwrap().last().unwrap();
    assert_eq!(message["role"], "user");
    let text = message["content"].as_str().unwrap();
    assert!(text.contains(&late.id) && text.contains("Length?") && text.ends_with("Two pages"));
    resumed.reply.send(completed(json!([])))?;
    until(|| !c.is_executing()).await;
    assert!(store.load_session(&id)?.async_questions.is_empty());
    server.abort();
    Ok(())
}

#[tokio::test]
async fn questionnaire_blocks_model_continuation_until_all_answers_or_stop() {
    use crate::test_support::{completed, server_with, until};

    for (tool, stop) in [
        ("exec", false),
        ("wait", false),
        ("exec", true),
        ("wait", true),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let store = crate::test_support::authenticated_store(dir.path().into());
        let source = r#"// @exec: {"yield_time_ms": 100}
text('Output before questionnaire');
await Promise.all(['first', 'second'].map(async id => {
text(await tools.ask_user({questions:[{id, question:id + '?', kind:'text'}]}));
}));"#;
        let initial_cell = Arc::new(Mutex::new(None::<String>));
        let cell = initial_cell.clone();
        let (url, mut requests, server) = server_with((0..3).map(move |step| {
            let cell = cell.clone();
            move |request: &str| {
                let body: Value = serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap();
                let output = match step {
                    0 if tool == "exec" => json!([{"type":"custom_tool_call","call_id":"ask","name":"exec","input":source}]),
                    0 => json!([{"type":"function_call","call_id":"ask","name":"wait","arguments":json!({"cell_id":cell.lock().unwrap().as_ref().unwrap(),"yield_time_ms":1}).to_string()}]),
                    1 => {
                        let result = body["input"].as_array().unwrap().iter().find(|v| v["call_id"] == "ask" && v.get("output").is_some()).unwrap();
                        let id = result["output"].as_str().unwrap().lines().next().unwrap().strip_prefix("Script running with cell ID ").unwrap();
                        json!([{"type":"function_call","call_id":"collect","name":"wait","arguments":json!({"cell_id":id}).to_string()}])
                    }
                    _ => {
                        let result = body["input"].as_array().unwrap().iter().find(|v| v["call_id"] == "collect" && v.get("output").is_some()).unwrap();
                        let output = result["output"].as_str().unwrap();
                        assert!(output.starts_with("Script completed"), "{output}");
                        assert!(output.contains("First answer λ") && output.contains("Second answer 界"), "{output}");
                        json!([{"type":"message","id":"done","phase":"final_answer","content":[{"type":"output_text","text":"Answers received"}]}])
                    }
                };
                (200, completed(output))
            }
        })).await;
        let h =
            Harness::with_provider(store.clone(), vec![], Provider::fixture(store, url)).unwrap();
        let mut session = Session::new(dir.path().into(), &Settings::default());
        session.rename("Questionnaire fixture".into());
        let c = h.insert(session);
        let questions = c.questionnaires.as_ref().unwrap();
        let mut changes = h.subscribe();
        if tool == "wait" {
            let worker = c.clone();
            let id = tokio::task::spawn_blocking(move || {
                worker
                    .code
                    .exec_output(source, "initial", &CancellationToken::new())
                    .unwrap()
                    .cell_id
                    .unwrap()
            })
            .await
            .unwrap();
            *initial_cell.lock().unwrap() = Some(id);
        }
        h.send(
            &c.id,
            "user",
            "Ask before proceeding".into(),
            Settings::default(),
        )
        .unwrap();
        tokio::time::timeout(Duration::from_secs(3), async {
            while questions.pending().len() != 2 {
                changes.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
        // Both questions can be visible before the first exec/wait drains
        // its output. Answer only after that yield: the final collect below
        // deliberately expects *both* answers in the unread output.
        until(|| {
            matches!(
                c.snapshot().entries.by_id("ask").map(|entry| &entry.kind),
                Some(EntryKind::Tool { complete: true, .. })
            )
        })
        .await;
        // The first request is the model asking. No second request may be made
        // even after the exec/wait yield deadline has elapsed.
        tokio::time::timeout(Duration::from_secs(3), requests.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(250), requests.recv())
                .await
                .is_err()
        );
        assert!(c.is_running());
        assert!(!c.snapshot().unchecked_completion);
        let pending = questions.pending();
        questions
            .submit(&pending[0].id, vec!["First answer λ".into()])
            .unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(100), requests.recv())
                .await
                .is_err()
        );
        assert!(
            c.is_running(),
            "one answered question must not release the queue"
        );
        if stop {
            h.stop(&c.id);
        } else {
            questions
                .submit(&pending[1].id, vec!["Second answer 界".into()])
                .unwrap();
        }
        // Cell and nested-tool workers can release their execution permits
        // after the last conversation change, especially on Stop.
        until(|| !c.is_executing()).await;
        assert!(!questions.has_pending());
        if stop {
            assert_eq!(c.status(), ConversationStatus::Stopped);
            assert!(!c.snapshot().unchecked_completion);
            if tool == "exec" {
                assert!(
                    matches!(
                        &c.snapshot().entries.by_id("ask").unwrap().kind,
                        EntryKind::Tool { output, complete: true, .. }
                            if output.contains("Output before questionnaire")
                    ),
                    "Stop must preserve output collected before the questionnaire"
                );
            }
            assert!(
                requests.try_recv().is_err(),
                "Stop must not resume the model"
            );
            server.abort();
        } else {
            server.await.unwrap();
            assert_eq!(c.status(), ConversationStatus::Completed);
            assert!(
                c.snapshot()
                    .entries
                    .iter()
                    .any(|e| e.text == "Answers received")
            );
            assert_eq!(
                c.snapshot()
                    .entries
                    .iter()
                    .filter(|e| matches!(e.kind, EntryKind::User { .. }))
                    .count(),
                1
            );
        }
    }
}

#[test]
fn questionnaire_persistence_failures_and_restart_do_not_lose_or_reopen_answers() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().into()).unwrap();
    let session = Session::new(dir.path().into(), &Settings::default());
    let id = session.id.clone();
    store.save_session(&session).unwrap();
    let h = Harness::new(store.clone(), store.load().unwrap().1).unwrap();
    let c = h.load(&id).unwrap();
    let q = c.questionnaires.as_ref().unwrap();
    let source = r#"// @exec: {"yield_time_ms": 1}
text(await tools.ask_user({questions:[{id:'choice',question:'Target?',kind:'text'}]}));"#;
    let db = rusqlite::Connection::open(store.root().join("sessions.sqlite3")).unwrap();
    crate::test_support::sqlite::fail_writes(&db, "sessions", "fixture disk failure");
    let result = c
        .code
        .exec(source, "failed-start", &CancellationToken::new())
        .unwrap();
    let cell = result
        .lines()
        .next()
        .unwrap()
        .strip_prefix("Script running with cell ID ");
    let result = cell.map_or(result.clone(), |id| {
        c.code
            .wait(
                &json!({"cell_id":id}).to_string(),
                &CancellationToken::new(),
            )
            .unwrap()
    });
    assert!(result.contains("fixture disk failure"), "{result}");
    assert!(
        !q.has_pending(),
        "do not display a request before tool-start persistence succeeds"
    );
    crate::test_support::sqlite::allow_writes(&db, "sessions");
    let running = c
        .code
        .exec(source, "working-start", &CancellationToken::new())
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while !q.has_pending() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    let pending = q.pending().pop().unwrap();
    h.edit(&id, |s| s.updated = 77).unwrap();
    let revision = h.catalog_revision();
    let answered_after = crate::store::now();
    assert!(q.submit(&pending.id, vec![]).is_err());
    assert_eq!(c.snapshot().updated, 77);
    h.evict_inactive(None).unwrap();
    assert!(h.get(&id).is_some());
    let stored = store.load_session(&id).unwrap();
    assert!(matches!(
        &stored.entries.by_id(&pending.id).unwrap().kind,
        EntryKind::Tool {
            complete: false,
            ..
        }
    ));
    crate::test_support::sqlite::fail_writes(&db, "sessions", "fixture result failure");
    q.submit(&pending.id, vec!["Retain this answer λ".into()])
        .unwrap();
    let cell = running
        .lines()
        .next()
        .unwrap()
        .strip_prefix("Script running with cell ID ")
        .unwrap();
    let output = c
        .code
        .wait(
            &json!({"cell_id":cell}).to_string(),
            &CancellationToken::new(),
        )
        .unwrap();
    assert!(output.contains("fixture result failure"), "{output}");
    let snapshot = c.snapshot();
    assert!((answered_after..=crate::store::now()).contains(&snapshot.updated));
    assert!(h.catalog_revision() > revision);
    assert!(
        matches!(&snapshot.entries.by_id(&pending.id).unwrap().kind, EntryKind::Tool { output, complete:true, .. } if output.contains("Retain this answer λ"))
    );
    crate::test_support::sqlite::allow_writes(&db, "sessions");
    h.flush().unwrap();
    assert_eq!(store.load_session(&id).unwrap().updated, snapshot.updated);
    assert!(
        matches!(&store.load_session(&id).unwrap().entries.by_id(&pending.id).unwrap().kind, EntryKind::Tool { output, complete:true, .. } if output.contains("Retain this answer λ"))
    );
    // Simulate the persisted state of an abrupt exit after the model finished.
    let mut abandoned = Session::new(dir.path().into(), &Settings::default());
    abandoned.entries.push(Entry {
        id: "abandoned-ask".into(),
        text: String::new(),
        kind: EntryKind::Tool {
            name: "ask_user".into(),
            input: "{}".into(),
            output: String::new(),
            complete: false,
            failed: false,
            command_status: None,
            cell_id: None,
            parent: Some("yielded-exec".into()),
            media: vec![],
            web: None,
        },
    });
    let abandoned_id = abandoned.id.clone();
    store.save_session(&abandoned).unwrap();
    let recovered = h.load(&abandoned_id).unwrap();
    assert!(!recovered.questionnaires.as_ref().unwrap().has_pending());
    assert!(
        matches!(&recovered.snapshot().entries.by_id("abandoned-ask").unwrap().kind, EntryKind::Tool { output, complete:true, .. } if output.contains("restart"))
    );
    assert!(store.load_session(&abandoned_id).unwrap().needs_attention);
}
#[test]
fn turn_usage_sums_only_response_streaming_time_and_resets_on_start() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().into()).unwrap();
    let session = Session::new(dir.path().into(), &Settings::default());
    let id = session.id.clone();
    let harness = Harness::new(store, vec![]).unwrap();
    harness.insert(session);
    let conversation = harness.get(&id).unwrap();
    {
        let mut usage = conversation.turn_usage.lock().unwrap();
        for (output, seconds) in [(17, 2), (43, 3)] {
            usage.record_response(&crate::provider::Response {
                output: vec![],
                usage: crate::store::Usage {
                    output,
                    ..Default::default()
                },
                first_output_latency: None,
                streaming_time: Duration::from_secs(seconds),
            });
        }
        assert_eq!(usage.output, 60);
        assert_eq!(usage.streaming_time, Duration::from_secs(5));
    }
    harness.start(&id, Settings::default()).unwrap();
    let usage = conversation.turn_usage.lock().unwrap();
    assert_eq!(usage.output, 0);
    assert_eq!(usage.streaming_time, Duration::ZERO);
}

#[test]
fn file_submissions_are_durable_idempotent_and_keep_failed_drafts() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("project");
    std::fs::create_dir(&project).unwrap();
    let store = Store::open(dir.path().join("state")).unwrap();
    let mut session = Session::new(project.clone(), &Settings::default());
    let text = "@@ -1 +1 @@\nRead @missing @\"bad\\q\" and @source.rs";
    session.draft = text.into();
    std::fs::write(project.join("source.rs"), "captured λ").unwrap();
    // A real attachment still needs durable storage before accepting the draft.
    let owner = store.root().join("media").join(&session.id);
    std::fs::create_dir_all(&owner).unwrap();
    let files_dir = owner.join("files");
    std::fs::write(&files_dir, "not a directory").unwrap();
    let id = session.id.clone();
    store.save_session(&session).unwrap();
    let h = Harness::new(store.clone(), store.load().unwrap().1).unwrap();
    assert!(
        h.send(
            &id,
            "failed",
            session.draft.clone().into(),
            Settings::default()
        )
        .is_err()
    );
    assert_eq!(h.get(&id).unwrap().snapshot().draft, text);
    assert!(store.load_session(&id).unwrap().entries.is_empty());
    std::fs::remove_file(files_dir).unwrap();
    h.send(&id, "files", text.into(), Settings::default())
        .unwrap();
    let c = h.get(&id).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while c.is_running() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    }
    let captured = c.snapshot().pending[0].clone();
    assert_eq!(captured.files.len(), 1);
    assert_eq!(captured.files[0].inline.as_deref(), Some("captured λ"));
    std::fs::remove_file(project.join("source.rs")).unwrap();
    h.edit(&id, |s| s.consume_pending()).unwrap();
    h.send(&id, "files", text.into(), Settings::default())
        .unwrap();
    let saved = store.load_session(&id).unwrap();
    assert_eq!(saved.entries.iter().filter(|e| e.id == "files").count(), 1);
    assert_eq!(saved.entries.by_id("files").unwrap().text, text);
    let mut replay = serde_json::json!(plaintext_context(&saved));
    crate::media::project(&mut replay, false).unwrap();
    assert!(replay.to_string().contains("captured λ"));
    while c.is_turn_active() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    }
    let fork = h.fork(&id, "files", &Settings::default()).unwrap();
    h.remove(&id).unwrap();
    let snapshot_path = &captured.files[0].snapshot;
    assert_eq!(
        std::fs::read_to_string(snapshot_path).unwrap(),
        "captured λ"
    );
    h.remove(&fork).unwrap();
    assert!(
        !snapshot_path.exists(),
        "last fork releases the durable capture"
    );
    // First-send retries use the accepted session, not the original unsaved draft.
    let mut fresh = Session::new(project.clone(), &Settings::default());
    fresh.title = "Fixture".into();
    std::fs::write(project.join("fresh"), "first version").unwrap();
    h.send_new(fresh.clone(), "first", "@fresh".into(), Settings::default())
        .unwrap();
    let new = h.get(&fresh.id).unwrap();
    while new.is_running() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    }
    h.edit(&fresh.id, |s| s.consume_pending()).unwrap();
    std::fs::remove_file(project.join("fresh")).unwrap();
    h.send_new(fresh.clone(), "first", "@fresh".into(), Settings::default())
        .unwrap();
    assert_eq!(
        store
            .load_session(&fresh.id)
            .unwrap()
            .entries
            .iter()
            .filter(|e| e.id == "first")
            .count(),
        1
    );
}
#[test]
fn failed_acceptance_can_be_edited_and_restarted_without_queuing_old_text() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().into()).unwrap();
    let session = Session::new(dir.path().into(), &Settings::default());
    let id = session.id.clone();
    store.save_session(&session).unwrap();
    let h = Harness::new(store.clone(), store.load().unwrap().1).unwrap();
    let db = rusqlite::Connection::open(store.root().join("sessions.sqlite3")).unwrap();
    crate::test_support::sqlite::fail_writes(&db, "sessions", "fixture failure");
    assert!(
        h.send(&id, "old", "old text".into(), Settings::default())
            .is_err()
    );
    crate::test_support::sqlite::allow_writes(&db, "sessions");
    h.edit(&id, |s| s.draft = "corrected text".into()).unwrap();
    h.flush().unwrap();
    drop(h);
    let h = Harness::new(store.clone(), store.load().unwrap().1).unwrap();
    h.send(&id, "new", "corrected text".into(), Settings::default())
        .unwrap();
    let c = h.get(&id).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while c.is_running() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(c.snapshot().pending, vec!["corrected text".into()]);
    assert!(
        matches!(
            c.snapshot().entries.by_id("new").unwrap().kind,
            EntryKind::User {
                steering: false,
                ..
            }
        ),
        "a normal send stays unlabelled even while pending"
    );
    assert!(c.snapshot().entries.by_id("old").is_none());
    h.edit(&id, |s| s.consume_pending()).unwrap();
    let revision = c.revision();
    h.send(&id, "new", "corrected text".into(), Settings::default())
        .unwrap();
    assert!(!c.is_running());
    assert_eq!(c.revision(), revision);
    h.evict_inactive(None).unwrap();
    assert!(
        h.summaries()
            .iter()
            .find(|s| s.id == id)
            .unwrap()
            .needs_attention
    );
}
#[test]
fn failed_submission_retry_is_idempotent_and_interrupted_load_keeps_positions() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().into()).unwrap();
    let mut session = Session::new(dir.path().into(), &Settings::default());
    let id = session.id.clone();
    session.interrupted = true;
    session.updated = 77;
    session
        .entries
        .push(Entry::new(EntryKind::Reply { final_at: None }, "original"));
    store.save_session(&session).unwrap();
    let h = Harness::new(store.clone(), store.load().unwrap().1).unwrap();
    let c = h.load(&id).unwrap();
    assert_eq!(c.snapshot().entries.len(), 2);
    let db = rusqlite::Connection::open(store.root().join("sessions.sqlite3")).unwrap();
    crate::test_support::sqlite::fail_writes(&db, "sessions", "fixture disk failure");
    assert!(
        h.send(
            &id,
            "submission-1",
            "one request".into(),
            Settings::default()
        )
        .is_err()
    );
    assert!(!c.is_running());
    assert_eq!(c.snapshot().updated, 77);
    assert!(
        c.snapshot().pending.is_empty(),
        "failed acceptance must not be published"
    );
    crate::test_support::sqlite::allow_writes(&db, "sessions");
    let sent_after = crate::store::now();
    h.send(
        &id,
        "submission-1",
        "one request".into(),
        Settings::default(),
    )
    .unwrap();
    assert!((sent_after..=crate::store::now()).contains(&c.snapshot().updated));
    assert_eq!(
        store.load_session(&id).unwrap().updated,
        c.snapshot().updated
    );
    let deadline = Instant::now() + Duration::from_secs(3);
    while c.is_running() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(
        c.snapshot()
            .entries
            .iter()
            .filter(|e| matches!(e.kind, EntryKind::User { .. }))
            .count(),
        1
    );
    h.evict_inactive(None).unwrap();
    let c = h.load(&id).unwrap();
    assert_eq!(c.snapshot().entries[0].text, "original");
    assert!(c.snapshot().entries[1].text.contains("Previous turn"));
    h.edit(&id, |s| s.entries[2].text = "edited request".into())
        .unwrap();
    let restored = store.load_session(&id).unwrap();
    assert_eq!(restored.entries[2].text, "edited request");
    assert_eq!(restored.entries[0].text, "original");
}
#[test]
fn evicting_transcript_keeps_completed_cell_results_and_shared_store() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().into()).unwrap();
    let h = Harness::new(store.clone(), vec![]).unwrap();
    let mut session = Session::new(dir.path().into(), &Settings::default());
    let id = session.id.clone();
    session.entries.push(Entry::new(
        EntryKind::User {
            questionnaire: None,
            skills: vec![],
            files: vec![],
            images: vec![],
            sent_at: 1_750_000_123,
            steering: false,
        },
        "preserved history",
    ));
    let c = h.insert(session);
    let result=c.code.exec("// @exec: {\"yield_time_ms\": 1}\nstore('key','retained');while (!load('release')) await new Promise(r=>setTimeout(r,1));text('unread result');","parent",&CancellationToken::new()).unwrap();
    let cell = result
        .lines()
        .find_map(|l| l.strip_prefix("Script running with cell ID "))
        .unwrap()
        .to_owned();
    c.code
        .exec(
            "store('release',true);",
            "release",
            &CancellationToken::new(),
        )
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while c.code.is_executing() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    }
    h.evict_inactive(None).unwrap();
    assert!(!c.loaded.load(Ordering::Acquire));
    assert!(c.snapshot().entries.is_empty());
    let loaded = h.load(&id).unwrap();
    assert_eq!(loaded.snapshot().entries[0].text, "preserved history");
    assert!(
        loaded
            .code
            .wait(
                &json!({"cell_id":cell}).to_string(),
                &CancellationToken::new()
            )
            .unwrap()
            .contains("unread result")
    );
    assert!(
        loaded
            .code
            .exec("text(load('key'));", "next", &CancellationToken::new())
            .unwrap()
            .contains("retained")
    );
}
#[tokio::test]
async fn revisions_notify_completion_and_background_changes_without_polling() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().into()).unwrap();
    let session = Session::new(dir.path().into(), &Settings::default());
    let id = session.id.clone();
    let harness = Harness::new(store, vec![]).unwrap();
    harness.insert(session);
    let conversation = harness.get(&id).unwrap();
    let mut changes = harness.subscribe();
    let initial = conversation.revision();
    harness
        .send(
            &id,
            "offline-submission",
            "Offline request".into(),
            Settings::default(),
        )
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            changes.changed().await.unwrap();
            if !conversation.is_running() {
                break;
            }
        }
    })
    .await
    .unwrap();
    assert!(conversation.revision() > initial);
    assert_eq!(conversation.status(), ConversationStatus::NeedsAttention);
    assert!(
        conversation
            .snapshot()
            .entries
            .iter()
            .any(|e| e.text.contains("Sign in"))
    );
    changes.borrow_and_update();
    let settled = conversation.revision();
    let worker = conversation.clone();
    tokio::task::spawn_blocking(move || {
        worker.code.exec(
            "notify('after foreground turn');",
            "parent",
            &CancellationToken::new(),
        )
    })
    .await
    .unwrap()
    .unwrap();
    tokio::time::timeout(Duration::from_secs(1), changes.changed())
        .await
        .unwrap()
        .unwrap();
    assert!(!conversation.is_running());
    assert!(conversation.revision() > settled);
    assert_eq!(
        harness
            .store
            .load_session(&id)
            .unwrap()
            .pending_tool_outputs[0]["output"],
        "after foreground turn"
    );
}
#[test]
fn deleting_a_session_stops_its_yielded_tool_without_recreating_the_session() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("state")).unwrap();
    let harness = Harness::new(store.clone(), vec![]).unwrap();
    let c = harness.insert(Session::new(dir.path().into(), &Settings::default()));
    // The marker is written by the nested command, not by the JS caller:
    // deletion must race with real work rather than an unstarted cell.
    c.code.exec(r#"// @exec: {"yield_time_ms": 1}
        text(await tools.exec_command({cmd:"touch started; exec sleep 60",login:false,yield_time_ms:60000}));"#,
        "background", &CancellationToken::new()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !dir.path().join("started").exists() {
        assert!(Instant::now() < deadline, "command did not start");
        std::thread::sleep(Duration::from_millis(2));
    }
    harness.remove(&c.id).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while c.is_executing() {
        assert!(Instant::now() < deadline, "deleted session still executing");
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(store.load().unwrap().1.is_empty());
}

#[test]
fn image_snapshots_persist_and_cleanup_failure_never_resurrects_deleted_sessions() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("state")).unwrap();
    let session = Session::new(dir.path().into(), &Settings::default());
    let id = session.id.clone();
    store.save_session(&session).unwrap();
    let h = Harness::new(store.clone(), vec![]).unwrap();
    let c = h.insert(session);
    let source = dir.path().join("image.png");
    std::fs::write(&source, crate::media::tests::png()).unwrap();
    let result = c
        .code
        .exec_output(
            "image(await tools.view_image({path:'image.png'}));",
            "view",
            &CancellationToken::new(),
        )
        .unwrap();
    let media = result.media().remove(0);
    let saved = store.load_session(&id).unwrap();
    assert!(saved.entries.iter().any(
        |e| matches!(&e.kind,EntryKind::Tool {media:m,complete:true,..} if m==&vec![media.clone()])
    ));
    std::fs::remove_file(source).unwrap();
    assert_eq!(
        std::fs::read(&media.path).unwrap(),
        crate::media::tests::png()
    );
    // Force cleanup failure after the SQL transaction commits.
    let media_dir = store.root().join("media").join(&id);
    std::fs::remove_dir_all(&media_dir).unwrap();
    std::fs::write(&media_dir, b"not a directory").unwrap();
    let error = h.remove(&id).unwrap_err().to_string();
    assert!(error.contains("was deleted"), "{error}");
    assert!(h.get(&id).is_none());
    c.save().unwrap();
    assert!(store.load_session(&id).is_err());
    assert!(store.load().unwrap().1.is_empty());
    assert!(
        c.code
            .exec_output(
                "image(await tools.view_image({path:'image.png'}));",
                "late",
                &CancellationToken::new()
            )
            .unwrap()
            .summary()
            .contains("Session deleted")
    );
}
#[test]
fn missing_login_preserves_a_message_across_retry_and_restart() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().into()).unwrap();
    let session = Session::new(dir.path().into(), &Settings::default());
    let id = session.id.clone();
    let harness = Harness::new(store.clone(), vec![]).unwrap();
    harness.insert(session);
    harness
        .send(
            &id,
            "preserved-submission",
            "Do not lose this request".into(),
            Settings::default(),
        )
        .unwrap();
    for attempt in 0..2 {
        let deadline = Instant::now() + Duration::from_secs(3);
        while harness.get(&id).unwrap().is_running() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        let snapshot = harness.get(&id).unwrap().snapshot();
        assert_eq!(snapshot.pending, vec!["Do not lose this request".into()]);
        assert_eq!(
            snapshot
                .entries
                .iter()
                .filter(|e| matches!(e.kind, EntryKind::User { .. }))
                .count(),
            1
        );
        assert!(snapshot.entries.iter().any(|e| e.text.contains("Sign in")));
        if attempt == 0 {
            harness.start(&id, Settings::default()).unwrap();
        }
    }
    let (_, sessions, _) = store.load().unwrap();
    assert_eq!(
        store.load_session(&sessions[0].id).unwrap().pending,
        vec!["Do not lose this request".into()]
    );
}
#[test]
fn deleting_a_cold_parent_does_not_deserialize_its_transcript() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().into()).unwrap();
    let mut session = Session::new(dir.path().into(), &Settings::default());
    session.accept("first".into());
    store.save_session(&session).unwrap();
    let h = Harness::new(store.clone(), store.load().unwrap().1).unwrap();
    store
        .db
        .lock()
        .unwrap()
        .execute(
            "UPDATE entries SET data='broken' WHERE session=?1",
            [&session.id],
        )
        .unwrap();
    assert!(h.load(&session.id).is_err());
    h.remove(&session.id).unwrap();
    assert!(!store.contains_session(&session.id).unwrap());
}
#[test]
fn repair_does_not_repeat_successful_calls_and_closes_orphans() {
    let mut s = Session::new(std::env::temp_dir(), &Settings::default());
    s.input = vec![
        json!({"type":"custom_tool_call","call_id":"done"}),
        json!({"type":"custom_tool_call_output","call_id":"done","output":"ok"}),
        json!({"type":"function_call","call_id":"interrupted"}),
    ]
    .into();
    s.entries.push(Entry {
        id: "done".into(),
        text: String::new(),
        kind: EntryKind::Tool {
            name: "exec".into(),
            input: "text(1)".into(),
            output: String::new(),
            complete: false,
            command_status: None,
            failed: false,
            cell_id: None,
            parent: None,
            media: vec![],
            web: None,
        },
    });
    repair_calls(&mut s);
    assert_eq!(s.input.len(), 4);
    assert_eq!(s.input[3]["call_id"], "interrupted");
    assert!(
        s.input[3]["output"]
            .as_str()
            .unwrap()
            .contains("side effects")
    );
    repair_calls(&mut s);
    assert_eq!(s.input.len(), 4);
}

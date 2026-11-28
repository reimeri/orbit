use super::*;
use crate::test_support::{StreamingRequest as Request, until};
use tokio::sync::mpsc;
struct Fixture {
    _dir: tempfile::TempDir,
    h: Arc<Harness>,
    parent: String,
    requests: mpsc::UnboundedReceiver<Request>,
    server: tokio::task::JoinHandle<()>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.h.stop_all();
        self.server.abort();
    }
}
impl Fixture {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("AGENTS.md"),
            "Project rule: preserve the sapphire invariant.",
        )
        .unwrap();
        let store = crate::test_support::authenticated_store(dir.path().join("state"));
        let (endpoint, requests, server) = crate::test_support::streaming_server().await;
        let provider = Provider::fixture(store.clone(), endpoint);
        let h = Harness::with_provider(store.clone(), vec![], provider).unwrap();
        let mut session = Session::new(dir.path().into(), &Settings::default());
        session.accept("PRIVATE PARENT CONTEXT".into());
        session.consume_pending();
        let parent = session.id.clone();
        store.save_session(&session).unwrap();
        h.insert(session);
        Self {
            _dir: dir,
            h,
            parent,
            requests,
            server,
        }
    }
    async fn request(&mut self) -> Request {
        tokio::time::timeout(Duration::from_secs(5), self.requests.recv())
            .await
            .unwrap()
            .unwrap()
    }
    fn spawn(&self, profile: Profile, session: Option<String>, request: &str) -> Result<RunInfo> {
        self.h.spawn_subagent(
            &self.parent,
            Spawn {
                profile,
                task: format!("Inspect {request}"),
                title: Some(request.into()),
                session_id: session,
            },
            request,
            &CancellationToken::new(),
        )
    }
    async fn wait_raw(&self, ids: Vec<String>, timeout: u64) -> String {
        let h = self.h.clone();
        let parent = self.parent.clone();
        tokio::task::spawn_blocking(move || {
            h.wait_subagent(
                &parent,
                Wait {
                    run_ids: ids,
                    timeout_ms: Some(timeout),
                },
                &CancellationToken::new(),
            )
        })
        .await
        .unwrap()
        .unwrap()
    }
    async fn wait(&self, ids: Vec<String>, timeout: u64) -> Value {
        serde_json::from_str(&self.wait_raw(ids, timeout).await).unwrap()
    }
}
async fn assert_tool_catalog(f: &Fixture, session: &str, request: &Value, expected: &[&str]) {
    let tools = request["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 2);
    let exec = tools.iter().find(|tool| tool["name"] == "exec").unwrap();
    let wait = tools.iter().find(|tool| tool["name"] == "wait").unwrap();
    assert_eq!(exec["type"], "custom");
    assert_eq!(exec["format"]["definition"], crate::code_mode::GRAMMAR);
    assert_eq!(wait["type"], "function");
    let description = exec["description"].as_str().unwrap();
    assert!(request.get("instructions").is_none());
    assert_eq!(request["input"][0]["role"], "developer");
    let instructions = request["input"][0]["content"][0]["text"].as_str().unwrap();
    let c = f.h.get(session).unwrap();
    let output = tokio::task::spawn_blocking(move || {
        c.code.exec(
            "text({names:Object.keys(tools), definitions:ALL_TOOLS});",
            "inspect-catalog",
            &CancellationToken::new(),
        )
    })
    .await
    .unwrap()
    .unwrap();
    let runtime: Value =
        serde_json::from_str(output.strip_prefix("Script completed\nOutput:\n").unwrap()).unwrap();
    let names = runtime["names"].as_array().unwrap();
    let actual: std::collections::BTreeSet<_> =
        names.iter().map(|name| name.as_str().unwrap()).collect();
    assert_eq!(names.len(), actual.len(), "tool names must be unique");
    assert_eq!(actual, expected.iter().copied().collect());
    let definitions = runtime["definitions"].as_array().unwrap();
    let documented: std::collections::BTreeSet<_> = definitions
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        definitions.len(),
        documented.len(),
        "definitions must be unique"
    );
    assert_eq!(
        documented, actual,
        "ALL_TOOLS must describe every callable tool"
    );
    for tool in definitions {
        let name = tool["name"].as_str().unwrap();
        assert!(description.contains(name));
        assert!(description.contains(tool["description"].as_str().unwrap()));
        assert!(tool["input_schema"].is_object());
        assert!(tool["output_schema"].is_object());
    }
    for name in [
        "exec_command",
        "write_stdin",
        "web__run",
        "view_image",
        "image_gen__imagegen",
        "ask_user",
        "ask_user_async",
        "spawn_subagent",
        "wait_subagent",
        "list_subagents",
    ] {
        if matches!(name, "ask_user" | "ask_user_async") {
            assert_eq!(
                instructions.contains(&format!("`tools.{name}`")),
                expected.contains(&name),
                "questionnaire guidance must match tool availability"
            );
        } else {
            assert!(
                !instructions.contains(name),
                "tool guidance leaked into instructions: {name}"
            );
        }
        if !expected.contains(&name) {
            assert!(
                !description.contains(name),
                "unavailable tool advertised: {name}"
            );
        }
    }
}
fn answer(text: &str) -> String {
    crate::test_support::completed(
        json!([{"type":"message","id":id(),"phase":"final_answer","content":[{"type":"output_text","text":text}]}]),
    )
}

#[tokio::test]
async fn child_activity_tracks_streamed_reasoning_and_message_text() {
    let mut f = Fixture::new().await;
    let run = f
        .spawn(Profile::Explore, None, "streaming activity")
        .unwrap();
    let request = f.request().await;
    let child = f.h.get(&run.session_id).unwrap();
    assert_eq!(child.status(), ConversationStatus::Thinking);

    for (event, entry_id, text, expected) in [
        (
            "response.reasoning_summary_text.delta",
            "reasoning-1",
            "Inspecting",
            ConversationStatus::Thinking,
        ),
        (
            "response.output_text.delta",
            "commentary",
            "Here is my update",
            ConversationStatus::Writing,
        ),
        (
            "response.reasoning_summary_text.delta",
            "reasoning-2",
            "Checking again",
            ConversationStatus::Thinking,
        ),
        (
            "response.output_text.delta",
            "answer",
            "Finished",
            ConversationStatus::Writing,
        ),
    ] {
        request
            .reply
            .send(format!(
                "{}\n\n",
                json!({"type": event, "item_id": entry_id, "summary_index": 0, "delta": text})
            ))
            .unwrap();
        until(|| child.snapshot().entries.by_id(entry_id).is_some()).await;
        assert_eq!(child.status(), expected);
        assert!(child.is_running());
    }
    request.reply.send(answer("Finished")).unwrap();
    until(|| !child.is_running()).await;
    assert_eq!(child.status(), ConversationStatus::Completed);
}

#[tokio::test]
async fn pending_question_prevents_completion_notification_across_a_new_main_turn() {
    let mut f = Fixture::new().await;
    let c = f.h.get(&f.parent).unwrap();
    let q = c.questionnaires.as_ref().unwrap().clone();
    let questions = q.clone();
    let ask = tokio::spawn(async move {
        questions
            .ask(
                "pending-ask",
                r#"{"questions":[{"id":"answer","question":"Private question","kind":"text"}]}"#,
                &CancellationToken::new(),
            )
            .await
    });
    until(|| q.has_pending()).await;
    f.h.start(&f.parent, Settings::default()).unwrap();
    f.request()
        .await
        .reply
        .send(answer("Main response ended, but input is still needed"))
        .unwrap();
    until(|| !c.is_running() && !c.finishing.load(Ordering::Acquire)).await;
    let events = f.h.take_notifications();
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0].kind,
        super::super::notifications::Kind::Question("pending-ask".into())
    );
    assert!(f.h.notification_is_current(&events[0]));
    assert!(!c.snapshot().unchecked_completion);
    assert!(
        !f.h.store
            .load_session(&f.parent)
            .unwrap()
            .unchecked_completion
    );
    q.submit("pending-ask", vec!["Answer".into()]).unwrap();
    ask.await.unwrap().unwrap();
    assert!(!f.h.notification_is_current(&events[0]));
    assert!(
        f.h.take_notifications().is_empty(),
        "answering must not replay a past completion"
    );
}

#[tokio::test]
async fn both_child_profiles_have_web_search_and_workspace_skill_metadata() {
    let mut f = Fixture::new().await;
    let dir = f._dir.path().join(".agents/skills/audit");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("SKILL.md"),
        "---\nname: audit\ndescription: Inspect test coverage\n---\nBody for selected skill only.",
    )
    .unwrap();
    let mut pending = vec![];
    for profile in [Profile::Explore, Profile::Review] {
        let child = f.spawn(profile, None, profile.label()).unwrap();
        let request = f.request().await;
        assert_tool_catalog(
            &f,
            &child.session_id,
            &request.body,
            &["exec_command", "write_stdin", "web__run", "view_image"],
        )
        .await;
        let input = request.body["input"].as_array().unwrap();
        assert!(!input[0].to_string().contains("Inspect test coverage"));
        assert!(input[1..].iter().any(|item| item["role"] == "developer"
            && item.to_string().contains("Inspect test coverage")));
        assert!(
            !request
                .body
                .to_string()
                .contains("Body for selected skill only.")
        );
        pending.push(request);
    }
}

#[tokio::test]
async fn parallel_children_isolate_context_wake_parent_and_resume_with_stable_results() {
    let mut f = Fixture::new().await;
    // Exercise the real QuickJS bridge. A completed spawning cell must not cancel its child.
    let c = f.h.get(&f.parent).unwrap();
    let script = "text(await tools.spawn_subagent({type:'explore',task:'\\nTrace the sapphire invariant\\nReview boundaries'}));";
    let out = tokio::task::spawn_blocking(move || {
        c.code.exec(script, "outer", &CancellationToken::new())
    })
    .await
    .unwrap()
    .unwrap();
    assert!(out.contains("run_id"));
    let explore = f.h.subagent_runs(&f.parent).pop().unwrap();
    assert_eq!(explore.title, "Trace the sapphire invariant");
    let explore_request = f.request().await;
    assert_tool_catalog(
        &f,
        &explore.session_id,
        &explore_request.body,
        &["exec_command", "write_stdin", "web__run", "view_image"],
    )
    .await;
    assert_eq!(
        explore_request.body["model"],
        Profile::Explore.defaults().model
    );
    assert_eq!(
        explore_request.body["reasoning"]["effort"],
        Profile::Explore.defaults().effort
    );
    assert!(
        !explore_request
            .body
            .to_string()
            .contains("PRIVATE PARENT CONTEXT")
    );
    assert!(
        explore_request.body["input"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["role"] == "user"
                && item["content"]
                    .as_str()
                    .is_some_and(|s| s.contains("sapphire invariant")))
    );
    assert!(
        explore_request.body["input"][0]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("READ-ONLY")
    );
    assert!(
        !explore_request.body["input"][0]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("tools.spawn_subagent")
    );
    let review = f.spawn(Profile::Review, None, "review").unwrap();
    let review_request = f.request().await;
    assert_eq!(
        review_request.body["model"],
        Profile::Review.defaults().model
    );
    assert_eq!(
        review_request.body["reasoning"]["effort"],
        Profile::Review.defaults().effort
    );
    assert_eq!(f.h.active_children(&f.parent), 2);
    assert_eq!(
        f.wait(vec![explore.run_id.clone(), review.run_id.clone()], 0)
            .await["runs"][0]["status"],
        "running"
    );
    assert!(
        f.spawn(Profile::Explore, Some(explore.session_id.clone()), "busy")
            .unwrap_err()
            .to_string()
            .contains("busy")
    );
    f.h.send(
        &f.parent,
        "parallel-parent",
        "Continue main work".into(),
        Settings::default(),
    )
    .unwrap();
    let main = f.request().await;
    assert_tool_catalog(
        &f,
        &f.parent,
        &main.body,
        &[
            "exec_command",
            "write_stdin",
            "web__run",
            "view_image",
            "image_gen__imagegen",
            "browser",
            "ask_user",
            "ask_user_async",
            "spawn_subagent",
            "wait_subagent",
            "list_subagents",
        ],
    )
    .await;
    assert_eq!(main.body["prompt_cache_key"], f.parent);
    main.reply
        .send(answer("Main work finished while children run"))
        .unwrap();
    until(|| !f.h.get(&f.parent).unwrap().is_executing()).await;
    assert!(
        f.h.take_notifications().is_empty(),
        "main is waiting for children, not finished"
    );
    assert!(!f.h.get(&f.parent).unwrap().snapshot().unchecked_completion);
    explore_request.reply.send(crate::test_support::completed(json!([
        {"type":"message","id":"progress","phase":"commentary","content":[{"type":"output_text","text":"intermediate secret"}]},
        {"type":"message","id":"final","phase":"final_answer","content":[{"type":"output_text","text":"Final sapphire finding"}]}
    ]))).unwrap();
    let wake = f.request().await;
    assert_eq!(
        f.h.store.activity_summary().unwrap().tokens[0].output,
        22,
        "one main response and one child response; waking the parent does not recount the child"
    );
    assert_eq!(wake.body["prompt_cache_key"], f.parent);
    assert!(wake.body["input"].to_string().contains(&explore.run_id));
    assert!(
        !wake.body["input"]
            .to_string()
            .contains("Final sapphire finding")
    );
    wake.reply.send(crate::test_support::completed(json!([{"type":"custom_tool_call","call_id":"retrieve","name":"exec","input":format!("text(await tools.wait_subagent({{run_ids:[{:?}]}}));",explore.run_id)}]))).unwrap();
    let acknowledged = f.request().await;
    assert!(
        acknowledged.body["input"]
            .to_string()
            .contains("Final sapphire finding")
    );
    assert!(
        !acknowledged.body["input"]
            .to_string()
            .contains("intermediate secret")
    );
    acknowledged.reply.send(answer("Acknowledged")).unwrap();
    until(|| !f.h.get(&f.parent).unwrap().is_executing()).await;
    assert!(
        f.h.take_notifications().is_empty(),
        "child completion and intermediate parent wake must not notify"
    );
    assert!(!f.h.get(&f.parent).unwrap().snapshot().unchecked_completion);
    assert!(
        !f.h.get(&explore.session_id)
            .unwrap()
            .snapshot()
            .unchecked_completion
    );
    let result = f
        .wait(vec![explore.run_id.clone(), review.run_id.clone()], 100)
        .await;
    assert_eq!(
        result["runs"][0]["final_response"],
        "Final sapphire finding"
    );
    assert_eq!(result["runs"][1]["status"], "running");
    let mut catalog = f.h.store.load_catalog().unwrap();
    let follow_up = SubagentSettings {
        model: crate::models::ASTRA.id.into(),
        effort: "low".into(),
        fast: true,
    };
    catalog
        .settings
        .subagents
        .insert(Profile::Explore, follow_up.clone());
    f.h.store.save_catalog(&catalog).unwrap();
    let resumed = f
        .spawn(
            Profile::Explore,
            Some(explore.session_id.clone()),
            "follow-up",
        )
        .unwrap();
    assert_ne!(resumed.run_id, explore.run_id);
    let continuation = f.request().await;
    assert_eq!(resumed.configuration.as_ref(), Some(&follow_up));
    assert_eq!(continuation.body["model"], follow_up.model);
    assert_eq!(continuation.body["reasoning"]["effort"], follow_up.effort);
    assert_eq!(continuation.body["service_tier"], "priority");
    assert!(
        continuation.body["input"]
            .to_string()
            .contains("Final sapphire finding")
    );
    assert!(
        continuation.body["input"]
            .to_string()
            .contains("Inspect follow-up")
    );
    assert!(
        !continuation.body["input"]
            .to_string()
            .contains("PRIVATE PARENT CONTEXT")
    );
    assert_eq!(
        f.wait(vec![explore.run_id.clone()], 0).await["runs"][0]["final_response"],
        "Final sapphire finding"
    );
    assert!(
        f.spawn(
            Profile::Review,
            Some(explore.session_id.clone()),
            "wrong-profile"
        )
        .is_err()
    );
    let nested = f.h.spawn_subagent(
        &review.session_id,
        Spawn {
            profile: Profile::Explore,
            task: "nested".into(),
            title: None,
            session_id: None,
        },
        "nested",
        &CancellationToken::new(),
    );
    assert!(nested.unwrap_err().to_string().contains("cannot delegate"));
    f.h.stop(&f.parent);
    until(|| f.h.active_children(&f.parent) == 0).await;
    assert_eq!(
        f.wait(vec![review.run_id.clone()], 0).await["runs"][0]["status"],
        "cancelled"
    );
    assert!(
        f.wait(vec![review.run_id], 0).await["runs"][0]
            .get("final_response")
            .is_none()
    );
    until(|| f.h.all().iter().all(|c| !c.is_executing())).await;
    assert!(
        f.requests.try_recv().is_err(),
        "explicit Stop must not restart parent"
    );
    drop((continuation, review_request));
}

#[tokio::test]
async fn concurrent_capacity_is_per_parent_and_single_child_stop_wakes_only_its_parent() {
    let mut f = Fixture::new().await;
    let mut tasks = vec![];
    for n in 0..9 {
        let h = f.h.clone();
        let parent = f.parent.clone();
        tasks.push(tokio::task::spawn_blocking(move || {
            h.spawn_subagent(
                &parent,
                Spawn {
                    profile: Profile::Explore,
                    task: format!("Task {n}"),
                    title: None,
                    session_id: None,
                },
                &format!("request-{n}"),
                &CancellationToken::new(),
            )
        }));
    }
    let mut accepted = vec![];
    let mut rejected = 0;
    for task in tasks {
        match task.await.unwrap() {
            Ok(run) => accepted.push(run),
            Err(e) => {
                assert!(e.to_string().contains("Six subagents"));
                rejected += 1;
            }
        }
    }
    assert_eq!(accepted.len(), 6);
    assert_eq!(rejected, 3);
    let mut held = vec![];
    for _ in 0..6 {
        held.push(f.request().await);
    }
    let other = Session::new(f._dir.path().into(), &Settings::default());
    let other_id = other.id.clone();
    f.h.store.save_session(&other).unwrap();
    f.h.insert(other);
    let seventh =
        f.h.spawn_subagent(
            &other_id,
            Spawn {
                profile: Profile::Review,
                task: "Independent seventh child".into(),
                title: None,
                session_id: None,
            },
            "seventh",
            &CancellationToken::new(),
        )
        .unwrap();
    let seventh_request = f.request().await;
    assert!(
        f.h.spawn_subagent(
            &other_id,
            Spawn {
                profile: Profile::Explore,
                task: "Wrong owner".into(),
                title: None,
                session_id: Some(accepted[0].session_id.clone())
            },
            "foreign",
            &CancellationToken::new()
        )
        .is_err()
    );
    f.h.stop(&accepted[0].session_id);
    let wake = f.request().await;
    assert_eq!(wake.body["prompt_cache_key"], f.parent);
    assert!(wake.body["input"].to_string().contains("Cancelled"));
    wake.reply
        .send(answer("Child cancellation acknowledged"))
        .unwrap();
    assert_eq!(f.h.active_children(&other_id), 1);
    assert_eq!(f.h.active_children(&f.parent), 5);
    assert!(f.h.remove(&f.parent).is_err());
    f.h.stop(&f.parent);
    until(|| f.h.active_children(&f.parent) == 0 && !f.h.get(&f.parent).unwrap().is_executing())
        .await;
    assert_eq!(f.h.active_children(&other_id), 1);
    seventh_request
        .reply
        .send(answer("Independent result"))
        .unwrap();
    let other_wake = f.request().await;
    assert_eq!(other_wake.body["prompt_cache_key"], other_id);
    other_wake
        .reply
        .send(answer("Other parent acknowledged"))
        .unwrap();
    until(|| f.h.all().iter().all(|c| !c.is_executing())).await;
    f.h.remove(&f.parent).unwrap();
    assert!(f.h.store.load_session(&f.parent).is_err());
    for run in accepted {
        assert!(f.h.store.load_session(&run.session_id).is_err());
    }
    assert!(f.h.store.load_session(&seventh.session_id).is_ok());
    assert!(f.requests.try_recv().is_err());
}

#[tokio::test]
async fn project_deletion_is_atomic_includes_children_and_preserves_other_projects() {
    let mut f = Fixture::new().await;
    let project = f.h.load(&f.parent).unwrap().snapshot().project;
    let second = Session::new(project.clone(), &Settings::default());
    let second_id = second.id.clone();
    f.h.store.save_session(&second).unwrap();
    f.h.insert(second);
    let other = Session::new(project.join("other-project"), &Settings::default());
    let other_id = other.id.clone();
    f.h.store.save_session(&other).unwrap();
    f.h.insert(other);

    let child = f
        .spawn(Profile::Explore, None, "delete project fixture")
        .unwrap();
    let _request = f.request().await;
    assert!(
        f.h.remove_project(&project)
            .unwrap_err()
            .to_string()
            .contains("Stop")
    );
    for id in [&f.parent, &second_id, &child.session_id, &other_id] {
        assert!(f.h.store.load_session(id).is_ok());
    }
    f.h.stop(&f.parent);
    until(|| f.h.all().iter().all(|c| !c.is_executing())).await;
    let background = f.h.get(&second_id).unwrap();
    let execution = background.clone();
    tokio::task::spawn_blocking(move || {
        execution.code.exec(
            r#"// @exec: {"yield_time_ms": 1}
await new Promise(r => setTimeout(r, 60000));"#,
            "background",
            &CancellationToken::new(),
        )
    })
    .await
    .unwrap()
    .unwrap();
    assert!(!background.is_running());
    assert!(background.is_executing());
    assert!(f.h.remove_project(&project).is_err());
    assert!(
        background.is_executing(),
        "refusal must not cancel outstanding work"
    );
    f.h.stop(&second_id);
    until(|| !background.is_executing()).await;
    // A yielded shell can outlive both its JavaScript cell and the model turn.
    let execution = background.clone();
    let output = tokio::task::spawn_blocking(move || execution.code.exec(
        r#"text(await tools.exec_command({cmd:"while [ ! -f release-shell ]; do sleep 0.01; done; printf done", login:false, yield_time_ms:1}));"#,
        "background-shell", &CancellationToken::new()
    )).await.unwrap().unwrap();
    assert!(output.contains("Script completed"), "{output}");
    assert!(
        output.contains("Process running with session ID"),
        "{output}"
    );
    assert!(!background.is_running());
    assert!(background.is_executing());
    assert!(f.h.remove_project(&project).is_err());
    assert!(background.is_executing());
    std::fs::write(project.join("release-shell"), "").unwrap();
    until(|| !background.is_executing()).await;
    assert!(
        background.code.has_work(),
        "completed, unread shell output is retained but must not block deletion"
    );
    let db = rusqlite::Connection::open(f.h.store.root().join("sessions.sqlite3")).unwrap();
    // Fail after a delete has already occurred inside the transaction.
    db.execute_batch(
        "CREATE TRIGGER reject_project_delete AFTER DELETE ON sessions
        WHEN (SELECT count(*) FROM sessions) < 3
        BEGIN SELECT RAISE(FAIL, 'fixture deletion failure'); END;",
    )
    .unwrap();
    assert!(f.h.remove_project(&project).is_err());
    for id in [&f.parent, &second_id, &child.session_id, &other_id] {
        assert!(f.h.get(id).is_some());
        assert!(f.h.store.load_session(id).is_ok());
    }
    db.execute_batch("DROP TRIGGER reject_project_delete")
        .unwrap();
    f.h.remove_project(&project).unwrap();
    for id in [&f.parent, &second_id, &child.session_id] {
        assert!(f.h.get(id).is_none());
        assert!(f.h.store.load_session(id).is_err());
    }
    assert!(f.h.subagent_runs(&f.parent).is_empty());
    let (_, summaries, _) = f.h.store.load().unwrap();
    assert_eq!(
        summaries.iter().map(|s| &s.id).collect::<Vec<_>>(),
        vec![&other_id]
    );
    f.h.remove_project(&project).unwrap();
    assert!(f.h.store.load_session(&other_id).is_ok());
}

#[tokio::test]
async fn acceptance_is_atomic_and_completion_survives_failed_storage_without_replay() {
    let mut f = Fixture::new().await;
    let db = rusqlite::Connection::open(f.h.store.root().join("sessions.sqlite3")).unwrap();
    crate::test_support::sqlite::fail_writes(&db, "subagent_runs", "disk fixture");
    assert!(f.spawn(Profile::Explore, None, "retry").is_err());
    assert_eq!(f.h.store.load().unwrap().1.len(), 1);
    assert!(f.h.subagent_runs(&f.parent).is_empty());
    assert!(f.requests.try_recv().is_err());
    crate::test_support::sqlite::allow_writes(&db, "subagent_runs");
    let run = f.spawn(Profile::Explore, None, "retry").unwrap();
    let child = f.request().await;
    assert_eq!(
        f.spawn(Profile::Explore, None, "retry").unwrap().run_id,
        run.run_id
    );
    assert_eq!(f.h.active_children(&f.parent), 1);
    crate::test_support::sqlite::fail_writes(&db, "subagent_runs", "disk fixture");
    child
        .reply
        .send(answer("Retain this full final answer λ"))
        .unwrap();
    until(|| {
        f.h.active_children(&f.parent) == 0 && !f.h.get(&run.session_id).unwrap().is_executing()
    })
    .await;
    let result = f.wait(vec![run.run_id.clone()], 0).await;
    assert_eq!(
        result["runs"][0]["final_response"],
        "Retain this full final answer λ"
    );
    assert!(f.h.flush().is_err());
    crate::test_support::sqlite::allow_writes(&db, "subagent_runs");
    f.h.flush().unwrap();
    let stored: String = db
        .query_row(
            "SELECT result FROM subagent_runs WHERE id=?1",
            [&run.run_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(stored, "Retain this full final answer λ");
    f.h.flush().unwrap();
    let parent = f.h.load(&f.parent).unwrap().snapshot();
    assert_eq!(parent.subagent_notices.len(), 1);
    assert_eq!(
        parent
            .entries
            .iter()
            .filter(|e| matches!(e.kind, EntryKind::Notice) && e.text.contains(&run.run_id))
            .count(),
        1
    );
    let notices: Vec<_> = parent
        .entries
        .iter()
        .filter(|e| matches!(e.kind, EntryKind::Notice) && e.text.contains(&run.run_id))
        .cloned()
        .collect();
    let reloaded = f.h.store.load_session(&f.parent).unwrap();
    assert_eq!(
        reloaded
            .entries
            .iter()
            .filter(|e| matches!(e.kind, EntryKind::Notice) && e.text.contains(&run.run_id))
            .cloned()
            .collect::<Vec<_>>(),
        notices
    );
    assert!(
        f.requests.try_recv().is_err(),
        "saving must not replay child or wake idle parent"
    );
}

#[tokio::test]
async fn wait_preserves_oversized_result_without_a_budget_argument() {
    let mut f = Fixture::new().await;
    let run = f.spawn(Profile::Explore, None, "large result").unwrap();
    let response = format!(
        "begin:{}:end",
        "λ".repeat(crate::output::character_budget(
            crate::output::DEFAULT_OUTPUT_TOKENS
        ))
    );
    f.request().await.reply.send(answer(&response)).unwrap();
    let notice = f.wait_raw(vec![run.run_id.clone()], 5000).await;
    let saved = crate::test_support::saved_output(&notice);
    assert_eq!(
        serde_json::from_slice::<Value>(&saved).unwrap(),
        json!({
            "runs": [{"run_id": run.run_id, "session_id": run.session_id,
                      "status": "completed", "final_response": response}]
        })
    );
}

#[tokio::test]
async fn failed_result_does_not_block_another_parents_send_or_completion() {
    let mut f = Fixture::new().await;
    let blocked = f.spawn(Profile::Review, None, "blocked").unwrap();
    let db = rusqlite::Connection::open(f.h.store.root().join("sessions.sqlite3")).unwrap();
    db.execute_batch(
        "CREATE TRIGGER reject_one_result BEFORE UPDATE ON subagent_runs
        WHEN OLD.request='blocked' BEGIN SELECT RAISE(FAIL,'result write failed'); END;",
    )
    .unwrap();
    f.request().await.reply.send(answer("retained")).unwrap();
    until(|| !f.h.get(&blocked.session_id).unwrap().is_executing()).await;

    // The owner still cannot advance past its uncommitted child result.
    assert!(
        f.h.send(&f.parent, "retry", "continue".into(), Settings::default())
            .is_err()
    );
    let mut other = Session::new(f._dir.path().into(), &Settings::default());
    other.rename("Other parent".into());
    let other = f.h.insert(other);
    other.save().unwrap();
    f.h.send(
        &other.id,
        "independent",
        "continue".into(),
        Settings::default(),
    )
    .unwrap();
    let turn = f.request().await;
    assert_eq!(turn.body["prompt_cache_key"], other.id);
    turn.reply.send(answer("accepted")).unwrap();
    until(|| !other.is_executing()).await;
    assert!(
        f.h.store
            .load_session(&other.id)
            .unwrap()
            .entries
            .by_id("independent")
            .is_some()
    );

    let healthy =
        f.h.spawn_subagent(
            &other.id,
            Spawn {
                profile: Profile::Review,
                session_id: None,
                task: "Check the other project".into(),
                title: None,
            },
            "healthy",
            &CancellationToken::new(),
        )
        .unwrap();
    f.request()
        .await
        .reply
        .send(answer("independent result"))
        .unwrap();
    let wake = f.request().await;
    assert_eq!(wake.body["prompt_cache_key"], other.id);
    assert!(wake.body["input"].to_string().contains(&healthy.run_id));
    wake.reply.send(answer("acknowledged")).unwrap();
    until(|| f.h.all().iter().all(|c| !c.is_executing())).await;

    assert!(
        f.h.flush().is_err(),
        "global flush must report the blocked family"
    );
    let result = |id: &str| {
        db.query_row("SELECT result FROM subagent_runs WHERE id=?1", [id], |r| {
            r.get::<_, Option<String>>(0)
        })
        .unwrap()
    };
    assert_eq!(
        result(&healthy.run_id).as_deref(),
        Some("independent result")
    );
    assert_eq!(result(&blocked.run_id), None);
    db.execute_batch("DROP TRIGGER reject_one_result").unwrap();
    f.h.flush().unwrap();
    assert_eq!(result(&blocked.run_id).as_deref(), Some("retained"));
    assert!(
        f.requests.try_recv().is_err(),
        "flushing must not rerun the child"
    );
}

#[test]
fn older_assignments_do_not_change_latest_child_status() {
    let mut coordinator = RunIndex::default();
    let mut old = RunInfo {
        configuration: None,
        run_id: "old".into(),
        session_id: "child".into(),
        parent_id: "parent".into(),
        request_id: "first".into(),
        profile: Profile::Review,
        title: "first".into(),
        status: RunStatus::Failed,
        assignment: 1,
        started: 0,
        finished: Some(1),
        notified: false,
        attention_acknowledged: false,
    };
    let latest = RunInfo {
        run_id: "latest".into(),
        assignment: 2,
        status: RunStatus::Running,
        finished: None,
        ..old.clone()
    };
    coordinator.record(latest.clone());
    coordinator.record(old.clone()); // Startup query is unordered.
    assert_eq!(
        coordinator.status("parent"),
        ChildStatus {
            running: 1,
            needs_attention: false
        }
    );
    let sibling = RunInfo {
        run_id: "sibling".into(),
        session_id: "second child".into(),
        ..old.clone()
    };
    coordinator.record(sibling);
    old.attention_acknowledged = true;
    coordinator.record(old); // A late acknowledgement for a historical run.
    coordinator.record(RunInfo {
        status: RunStatus::Completed,
        ..latest
    });
    assert_eq!(
        coordinator.status("parent"),
        ChildStatus {
            running: 0,
            needs_attention: true
        }
    );
    assert_eq!(coordinator.latest_runs("parent").len(), 2);
    assert!(
        coordinator
            .run("parent", "old")
            .unwrap()
            .attention_acknowledged
    );
}

#[tokio::test]
async fn sidebar_status_reads_do_not_wait_for_cold_session_filing() {
    let mut f = Fixture::new().await;
    let run = f
        .spawn(Profile::Review, None, "review the open session")
        .unwrap();
    let _pending_request = f.request().await;
    let h = &f.h;
    let parent = &f.parent;
    let cold = Session::new(f._dir.path().into(), &Settings::default());
    h.store.save_session(&cold).unwrap();

    // Stall the actual cold load rather than assuming a large transcript is slow.
    std::thread::scope(|scope| {
        let body = h.load_gate(&cold.id);
        let blocked_load = body.lock().unwrap();
        let family = h.family(&cold.id);
        let filing =
            scope.spawn(|| h.file_session(&cold.id, FilingAction::File(FilingKind::Settled)));
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while family.try_lock().is_ok() {
            assert!(
                std::time::Instant::now() < deadline,
                "filing did not enter the cold load"
            );
            std::thread::yield_now();
        }

        let (read, done) = std::sync::mpsc::channel();
        scope.spawn(move || {
            assert_eq!(h.active_children(parent), 1);
            assert_eq!(h.child_statuses()[parent].running, 1);
            assert_eq!(h.subagent_runs(parent)[0].run_id, run.run_id);
            assert_eq!(
                h.resident_subagent(parent, RunKey::Id(&run.run_id))
                    .unwrap()
                    .status,
                RunStatus::Running
            );
            assert!(h.attention(parent).is_none());
            read.send(()).unwrap();
        });
        let responsive = done.recv_timeout(Duration::from_secs(5));
        drop(blocked_load);
        assert_eq!(filing.join().unwrap().unwrap(), FilingOutcome::Applied);
        responsive.expect("UI reads blocked on storage for a different session");
    });
    assert_eq!(
        h.store.load_session(&cold.id).unwrap().filing.unwrap().kind,
        FilingKind::Settled
    );
}

#[tokio::test]
async fn attention_acknowledges_existing_child_failures_without_losing_results_or_new_alerts() {
    let mut f = Fixture::new().await;
    let first = f.spawn(Profile::Explore, None, "first failure").unwrap();
    let first_request = f.request().await;
    let second = f.spawn(Profile::Review, None, "later failure").unwrap();
    let second_request = f.request().await;
    assert!(f.h.attention(&first.session_id).is_none());
    let db = rusqlite::Connection::open(f.h.store.root().join("sessions.sqlite3")).unwrap();
    crate::test_support::sqlite::fail_writes(&db, "subagent_runs", "disk fixture");
    let failure = |message| {
        format!(
            "{}\n\n",
            json!({"type":"response.failed","response":{"status":"failed","error":{"message":message}}})
        )
    };
    first_request
        .reply
        .send(failure("First flagged request"))
        .unwrap();
    until(|| !f.h.get(&first.session_id).unwrap().is_executing()).await;
    let original = f.wait(vec![first.run_id.clone()], 0).await;
    assert_eq!(original["runs"][0]["status"], "failed");
    assert!(
        original["runs"][0]["error"]
            .as_str()
            .unwrap()
            .contains("First flagged request")
    );
    assert!(
        f.h.acknowledge_attention(f.h.attention(&f.parent).unwrap())
            .is_err()
    );
    assert!(f.h.child_statuses()[&f.parent].needs_attention);
    crate::test_support::sqlite::allow_writes(&db, "subagent_runs");

    let seen = f.h.attention(&f.parent).unwrap();
    second_request.reply.send(failure("Newer failure")).unwrap();
    let wake = f.request().await;
    wake.reply.send(answer("Observed both failures")).unwrap();
    until(|| f.h.all().iter().all(|c| !c.is_executing())).await;
    f.h.acknowledge_attention(seen).unwrap();
    assert!(
        f.h.subagent_run(&f.parent, RunKey::Id(&first.run_id))
            .unwrap()
            .unwrap()
            .attention_acknowledged
    );
    assert!(
        !f.h.subagent_run(&f.parent, RunKey::Id(&second.run_id))
            .unwrap()
            .unwrap()
            .attention_acknowledged
    );
    assert!(f.h.child_statuses()[&f.parent].needs_attention);
    f.h.acknowledge_attention(f.h.attention(&f.parent).unwrap())
        .unwrap();
    assert!(!f.h.child_statuses()[&f.parent].needs_attention);
    f.h.flush().unwrap();
    assert_eq!(f.wait(vec![first.run_id.clone()], 0).await, original);
    let (reloaded, _) = RunIndex::load(&f.h.store).unwrap();
    for id in [&first.run_id, &second.run_id] {
        assert!(reloaded.run(&f.parent, id).unwrap().attention_acknowledged);
        assert_eq!(
            reloaded.run(&f.parent, id).unwrap().status,
            RunStatus::Failed
        );
        assert!(reloaded.run(&f.parent, id).unwrap().notified);
    }
}

#[tokio::test]
async fn simultaneous_completion_and_failure_merge_with_user_steering_without_partial_results() {
    let mut f = Fixture::new().await;
    let explore = f.spawn(Profile::Explore, None, "explore").unwrap();
    let first = f.request().await;
    let review = f.spawn(Profile::Review, None, "review").unwrap();
    let second = f.request().await;
    f.h.send(
        &f.parent,
        "parent-work",
        "Work in parallel".into(),
        Settings::default(),
    )
    .unwrap();
    let parent = f.request().await;
    first.reply.send(format!("{}\n\n{}\n\n",
        json!({"type":"response.output_text.delta","item_id":"partial","delta":"Never return this partial answer"}),
        json!({"type":"response.failed","response":{"status":"failed","error":{"message":"Fixture failure"}}})
    )).unwrap();
    second.reply.send(answer("")).unwrap();
    until(|| f.h.active_children(&f.parent) == 0).await;
    f.h.send(
        &f.parent,
        "steering",
        "User steering takes precedence".into(),
        Settings::default(),
    )
    .unwrap();
    assert!(
        f.requests.try_recv().is_err(),
        "completion must not start overlapping parent requests"
    );
    let queued = f.h.load(&f.parent).unwrap().snapshot();
    assert!(matches!(
        queued.entries.by_id("parent-work").unwrap().kind,
        EntryKind::User {
            steering: false,
            ..
        }
    ));
    assert!(matches!(
        queued.entries.by_id("steering").unwrap().kind,
        EntryKind::User { steering: true, .. }
    ));
    parent.reply.send(answer("Reached boundary")).unwrap();
    let next = f.request().await;
    assert!(matches!(
        f.h.store
            .load_session(&f.parent)
            .unwrap()
            .entries
            .by_id("steering")
            .unwrap()
            .kind,
        EntryKind::User {
            steering: false,
            ..
        }
    ));
    let input = next.body["input"].to_string();
    assert!(input.contains(&explore.run_id) && input.contains(&review.run_id));
    assert!(input.contains("User steering takes precedence"));
    next.reply
        .send(answer("Acknowledged both results and steering"))
        .unwrap();
    until(|| f.h.all().iter().all(|c| !c.is_executing())).await;
    assert!(f.requests.try_recv().is_err());
    let results = f.wait(vec![explore.run_id, review.run_id], 0).await;
    assert_eq!(results["runs"][0]["status"], "failed");
    assert!(results["runs"][0].get("final_response").is_none());
    assert!(
        !results
            .to_string()
            .contains("Never return this partial answer")
    );
    assert_eq!(results["runs"][1]["final_response"], "");
}

#[test]
fn restart_keeps_history_cold_but_addressable_and_acknowledgeable() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().into()).unwrap();
    let parent = Session::new(dir.path().into(), &Settings::default());
    let mut child = Session::new(dir.path().into(), &Settings::default());
    child.child = Some(Child {
        parent_id: parent.id.clone(),
        profile: Profile::Review,
    });
    for session in [&parent, &child] {
        store.save_session(session).unwrap();
    }
    // Insert in reverse order: neither rowid nor timestamps identify the latest.
    // The newest row lacks its required result; the previous readable one wins.
    for n in (1..=32).rev() {
        let status = match n {
            2 => RunStatus::Running, // recovery is required even though superseded
            3 | 4 => RunStatus::Failed,
            _ => RunStatus::Completed,
        };
        let run = RunInfo {
            run_id: format!("run-{n}"),
            session_id: child.id.clone(),
            parent_id: parent.id.clone(),
            request_id: format!("request-{n}"),
            profile: Profile::Review,
            configuration: None,
            title: format!("Task {n}"),
            status,
            assignment: n,
            started: 0,
            finished: (n != 2).then_some(1),
            notified: n != 2,
            attention_acknowledged: false,
        };
        store
            .db
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO subagent_runs
            (id,parent,session,request,metadata,result) VALUES (?1,?2,?3,?4,?5,?6)",
                params![
                    run.run_id,
                    parent.id,
                    child.id,
                    run.request_id,
                    serde_json::to_string(&run).unwrap(),
                    (status == RunStatus::Completed && n != 32).then(|| format!("Finding {n}"))
                ],
            )
            .unwrap();
    }
    store
        .db
        .lock()
        .unwrap()
        .execute(
            "UPDATE subagent_runs SET request='damaged' WHERE id='run-4'",
            [],
        )
        .unwrap();
    let h = Harness::new(store.clone(), store.load().unwrap().1).unwrap();
    assert!(h.startup_warnings.iter().any(|w| w.contains("run-32")));
    assert_eq!(h.subagent_runs(&parent.id)[0].run_id, "run-31");
    assert_eq!(h.child_statuses()[&parent.id], ChildStatus::default());
    let lookup = |key| h.subagent_run(&parent.id, key).unwrap().unwrap();
    assert_eq!(lookup(RunKey::Id("run-2")).status, RunStatus::Interrupted);
    assert!(
        h.resident_subagent(&parent.id, RunKey::Id("run-3"))
            .is_none()
    );
    let results = h
        .wait_subagent(
            &parent.id,
            Wait {
                run_ids: vec!["run-1".into(), "run-2".into()],
                timeout_ms: Some(0),
            },
            &CancellationToken::new(),
        )
        .unwrap();
    let results: Value = serde_json::from_str(&results).unwrap();
    assert_eq!(results["runs"][0]["final_response"], "Finding 1");
    assert_eq!(results["runs"][1]["status"], "interrupted");
    let replay = h
        .subagent_tool(
            &parent.id,
            "spawn_subagent",
            r#"{"type":"review","task":"Do not run twice"}"#,
            "request-1",
            &CancellationToken::new(),
        )
        .unwrap();
    assert_eq!(
        serde_json::from_str::<RunInfo>(&replay).unwrap().run_id,
        "run-1"
    );
    for key in [RunKey::Id("run-1"), RunKey::Request("request-1")] {
        assert!(h.subagent_run(&child.id, key).unwrap().is_none());
        assert!(h.resident_subagent(&parent.id, key).is_none());
    }
    let seen = h.attention(&child.id).unwrap();
    // A failure from a subsequent assignment of the *same* child commits
    // while that acknowledgement is queued on the UI storage worker.
    let later = RunInfo {
        run_id: "later".into(),
        request_id: "later".into(),
        assignment: 33,
        status: RunStatus::Failed,
        ..lookup(RunKey::Id("run-31"))
    };
    store
        .save_session_transaction(
            Some(&child),
            &child,
            crate::store::Checkpoint::StartRun(&later),
        )
        .unwrap();
    h.run_index().record(later);
    let error = h.acknowledge_attention(seen).unwrap_err();
    assert!(error.to_string().contains("identity mismatch"));
    assert!(lookup(RunKey::Id("run-3")).attention_acknowledged);
    assert!(!lookup(RunKey::Id("later")).attention_acknowledged);
    assert!(h.attention(&parent.id).is_some());
    assert!(h.child_statuses()[&parent.id].needs_attention);
    h.flush().unwrap();
    h.flush().unwrap();
    let notices = store.load_session(&parent.id).unwrap().subagent_notices;
    assert_eq!(notices.len(), 1);
    assert!(notices[0].contains("run-2"));
    assert!(lookup(RunKey::Id("run-2")).notified);
    assert!(
        h.resident_subagent(&parent.id, RunKey::Id("run-2"))
            .is_none()
    );
}

#[test]
fn restart_skips_only_malformed_assignments_and_preserves_their_rows() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().into()).unwrap();
    let parent = Session::new(dir.path().into(), &Settings::default());
    let unrelated = Session::new(dir.path().into(), &Settings::default());
    for session in [&parent, &unrelated] {
        store.save_session(session).unwrap();
    }
    let mut children = vec![];
    for request in ["bad-json", "identity-mismatch", "missing-result", "valid"] {
        let mut child = Session::new(dir.path().into(), &Settings::default());
        child.child = Some(Child {
            parent_id: parent.id.clone(),
            profile: Profile::Review,
        });
        let run = RunInfo {
            configuration: None,
            run_id: id(),
            parent_id: parent.id.clone(),
            session_id: child.id.clone(),
            request_id: request.into(),
            title: request.into(),
            profile: Profile::Review,
            status: if request == "missing-result" {
                RunStatus::Completed
            } else {
                RunStatus::Running
            },
            assignment: 1,
            started: now(),
            finished: None,
            notified: false,
            attention_acknowledged: false,
        };
        store
            .save_session_transaction(None, &child, crate::store::Checkpoint::StartRun(&run))
            .unwrap();
        children.push((child, run));
    }
    let db = rusqlite::Connection::open(store.root().join("sessions.sqlite3")).unwrap();
    db.execute_batch(
        "UPDATE subagent_runs SET metadata='{' WHERE request='bad-json';
        UPDATE subagent_runs SET request='other' WHERE request='identity-mismatch';",
    )
    .unwrap();
    let stored_rows = || {
        db.prepare(
            "SELECT json_array(id,parent,session,request,metadata,result,error)
                    FROM subagent_runs WHERE request!='valid' ORDER BY id",
        )
        .unwrap()
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap()
    };
    let before = stored_rows();
    let h = Harness::new(store.clone(), store.load().unwrap().1).unwrap();
    assert_eq!(h.load(&unrelated.id).unwrap().id, unrelated.id);
    assert_eq!(h.load(&parent.id).unwrap().id, parent.id);
    assert_eq!(h.startup_warnings.len(), 3);
    for (_, run) in &children[..3] {
        assert!(h.startup_warnings.iter().any(|w| w.contains(&run.run_id)));
        assert!(h.subagent_run(&parent.id, RunKey::Id(&run.run_id)).is_err());
    }
    let runs = h.subagent_runs(&parent.id);
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].run_id, children[3].1.run_id);
    assert_eq!(runs[0].status, RunStatus::Interrupted);
    assert_eq!(stored_rows(), before);
    // Even a child with no readable assignments still belongs to the family.
    h.remove(&parent.id).unwrap();
    for (child, _) in children {
        assert!(!store.contains_session(&child.id).unwrap());
    }
    assert!(store.contains_session(&unrelated.id).unwrap());
}

#[tokio::test]
async fn restart_retains_results_and_interrupts_unfinished_work_without_waking_parent() {
    let mut f = Fixture::new().await;
    let run = f.spawn(Profile::Review, None, "unfinished").unwrap();
    let request = f.request().await;
    let parent_before_stop = f.h.store.load_session(&f.parent).unwrap();
    f.h.stop_all();
    until(|| f.h.all().iter().all(|c| !c.is_executing())).await;
    f.h.store.save_session(&parent_before_stop).unwrap();
    // Leave the durable record exactly as an interrupted process would have
    // left it, with the original accepted task and no terminal result.
    f.h.store
        .db
        .lock()
        .unwrap()
        .execute(
            "UPDATE subagent_runs SET metadata=?2,result=NULL,error=NULL WHERE id=?1",
            params![run.run_id, serde_json::to_string(&run).unwrap()],
        )
        .unwrap();
    let (endpoint, mut requests, server) = crate::test_support::ws_server(vec![(
        200,
        answer("Recovered parent acknowledges interruption"),
    )])
    .await;
    let recovered = Harness::with_provider(
        f.h.store.clone(),
        f.h.store.load().unwrap().1,
        Provider::fixture(f.h.store.clone(), endpoint),
    )
    .unwrap();
    let info = recovered.subagent_runs(&f.parent).pop().unwrap();
    assert_eq!(info.status, RunStatus::Interrupted);
    assert!(recovered.all().is_empty());
    let h = recovered.clone();
    let parent = f.parent.clone();
    let run_id = run.run_id;
    let out = tokio::task::spawn_blocking(move || {
        h.wait_subagent(
            &parent,
            Wait {
                run_ids: vec![run_id],
                timeout_ms: Some(0),
            },
            &CancellationToken::new(),
        )
    })
    .await
    .unwrap()
    .unwrap();
    assert!(!out.contains("final_response"));
    assert!(out.contains("interrupted"));
    assert!(
        recovered
            .load(&f.parent)
            .unwrap()
            .snapshot()
            .subagent_notices
            .is_empty()
    );
    recovered
        .send(
            &f.parent,
            "resume-parent",
            "What happened?".into(),
            Settings::default(),
        )
        .unwrap();
    until(|| !recovered.get(&f.parent).unwrap().is_executing()).await;
    let parent = recovered.load(&f.parent).unwrap().snapshot();
    assert!(
        parent
            .entries
            .iter()
            .any(|e| e.text.contains("Interrupted"))
    );
    assert_eq!(recovered.active_children(&f.parent), 0);
    server.await.unwrap();
    let request_body = requests.recv().await.unwrap();
    assert!(request_body.contains("What happened?"));
    recovered.stop_all();
    drop(request);
}

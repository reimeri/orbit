//! Best-effort naming runs independently of the main turn, once at first acceptance.
use super::*;
use crate::store::UserMessage;

impl Harness {
    pub(super) fn name_session(
        self: &Arc<Self>,
        conversation: &Arc<Conversation>,
        message: UserMessage,
    ) {
        // Set before spawning so an idle session cannot be evicted underneath the result.
        conversation.naming.store(true, Ordering::Release);
        let harness = self.clone();
        let conversation = conversation.clone();
        self.runtime.as_ref().expect("live runtime").spawn(async move {
            let result = tokio::time::timeout(
                Duration::from_secs(60),
                harness.provider.session_title(&conversation.id, &message, &conversation.naming_cancel),
            ).await.context("Session naming timed out").and_then(|result| result);
            match result {
                Ok(title) => {
                    let worker = conversation.clone();
                    let result = tokio::task::spawn_blocking(move || {
                        let mut saved = worker.persisted.lock().expect("persistence lock");
                        if worker.deleted.load(Ordering::Acquire) {
                            return Ok(());
                        }
                        {
                            let mut state = worker.state.lock().expect("session lock");
                            if state.title_manually_set {
                                return Ok(());
                            }
                            state.title = title;
                        }
                        worker.save_locked(&mut saved)
                    }).await.expect("session naming worker");
                    if let Err(error) = result {
                        conversation.state.lock().expect("session lock").entries.push(Entry::new(
                            EntryKind::Notice,
                            format!("Could not save the automatic session title: {error:#}. The title is retained in memory; keep Pico open, free disk space or fix permissions, then retry saving."),
                        ));
                    }
                }
                Err(error) => {
                    // The first-message fallback is already durable. Naming must not
                    // fail the main turn or put an otherwise healthy session in error.
                    if !conversation.naming_cancel.is_cancelled() {
                        eprintln!("Automatic session naming failed; keeping the first-message title: {error:#}");
                    }
                }
            }
            conversation.naming.store(false, Ordering::Release);
            harness.catalog_revision.fetch_add(1, Ordering::Release);
            conversation.changed();
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{completed, gated_server};

    fn answer(text: &str) -> (u16, String) {
        (
            200,
            completed(
                json!([{"type":"message", "role":"assistant", "id":"answer", "phase":"final_answer", "content":[{"type":"output_text", "text":text}]}]),
            ),
        )
    }

    async fn until(h: &Harness, condition: impl Fn() -> bool) {
        let mut changes = h.subscribe();
        tokio::time::timeout(Duration::from_secs(5), async {
            while !condition() {
                changes.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn first_message_naming_is_independent_durable_and_respects_late_changes() {
        // Both send paths, plus races that would lose user work or resurrect a row.
        for scenario in [
            "new",
            "existing",
            "rename",
            "delete",
            "storage failure",
            "provider failure",
            "invalid title",
        ] {
            let dir = tempfile::tempdir().unwrap();
            let store = Store::open(dir.path().join("state")).unwrap();
            store
                .write(
                    "auth.json",
                    &crate::auth::Credentials {
                        access_token: "fixture".into(),
                        refresh_token: "fixture".into(),
                        expires_at: crate::store::now() + 3600,
                        account_id: Some("fixture".into()),
                        email: None,
                    },
                )
                .unwrap();
            let (url, mut requests, server) = gated_server().await;
            let h = Harness::with_provider(
                store.clone(),
                vec![],
                Provider::fixture(store.clone(), url),
            )
            .unwrap();
            let session = Session::new(dir.path().into(), &Settings::default());
            let id = session.id.clone();
            let first = "Diagnose λ cache corruption\nKeep all of this second line.";
            if scenario == "existing" {
                h.insert(session);
                h.send(&id, "first", first.into(), Settings::default())
                    .unwrap();
            } else {
                h.send_new(session, "first", first.into(), Settings::default())
                    .unwrap();
            }
            let c = h.get(&id).unwrap();
            let mut naming_reply = None;
            // Requests may arrive in either order. Hold naming while the main turn finishes.
            for _ in 0..2 {
                let (request, reply) =
                    tokio::time::timeout(Duration::from_secs(5), requests.recv())
                        .await
                        .unwrap()
                        .unwrap();
                let body: Value =
                    serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap();
                if body["tools"] == json!([]) {
                    assert_eq!(body["model"], "gpt-6-luna");
                    assert_eq!(body["tool_choice"], "none");
                    assert_eq!(body["input"], json!([{"role":"user", "content":first}]));
                    assert!(body.get("service_tier").is_none());
                    naming_reply = Some(reply);
                } else {
                    reply.send(answer("Main response")).unwrap();
                }
            }
            until(&h, || !c.is_turn_active()).await;
            assert!(c.naming.load(Ordering::Acquire));
            assert_eq!(*c.status.lock().unwrap(), "Completed");
            let fallback = c.snapshot().title;
            assert_eq!(fallback, "Diagnose λ cache corruption");
            h.evict_inactive(None).unwrap();
            assert!(
                h.get(&id).is_some(),
                "pending naming must prevent stale detached writes"
            );
            // A repeated accepted submission must not create another naming request.
            h.send(&id, "first", first.into(), Settings::default())
                .unwrap();
            let db = rusqlite::Connection::open(store.root().join("sessions.sqlite3")).unwrap();
            match scenario {
                "rename" => h.edit(&id, |s| s.rename(fallback.clone())).unwrap(),
                "delete" => h.remove(&id).unwrap(),
                "storage failure" => {
                    crate::test_support::sqlite::fail_writes(&db, "sessions", "title write fixture")
                }
                _ => h
                    .edit(&id, |s| s.draft = "Unsent draft must survive".into())
                    .unwrap(),
            }
            naming_reply
                .unwrap()
                .send(match scenario {
                    "provider failure" => (400, "Naming unavailable".into()),
                    "invalid title" => answer("A title\nand unwanted explanation"),
                    _ => answer("Repair λ cache corruption"),
                })
                .unwrap();
            until(&h, || !c.naming.load(Ordering::Acquire)).await;
            if scenario == "delete" {
                assert!(store.load_session(&id).is_err());
                assert!(h.summaries().is_empty());
            } else {
                if scenario == "storage failure" {
                    assert_eq!(store.load_session(&id).unwrap().title, fallback);
                    assert_eq!(c.snapshot().title, "Repair λ cache corruption");
                    assert!(c.snapshot().entries.iter().any(|e| {
                        e.text
                            .contains("Could not save the automatic session title")
                    }));
                    crate::test_support::sqlite::allow_writes(&db, "sessions");
                    h.flush().unwrap();
                }
                let saved = store.load_session(&id).unwrap();
                let expected =
                    if matches!(scenario, "rename" | "provider failure" | "invalid title") {
                        fallback.as_str()
                    } else {
                        "Repair λ cache corruption"
                    };
                assert_eq!(saved.title, expected, "{scenario}");
                assert!(!saved.needs_attention);
                assert_eq!(
                    saved.entries.len(),
                    2 + usize::from(scenario == "storage failure")
                );
                assert_eq!(saved.entries[0].text, first);
                assert_eq!(saved.entries[1].text, "Main response");
                assert_eq!(
                    saved.input.len(),
                    2,
                    "naming must not enter the agent context"
                );
                if !matches!(scenario, "rename" | "storage failure") {
                    assert_eq!(saved.draft, "Unsent draft must survive");
                }
                h.evict_inactive(None).unwrap();
                let c = h.load(&id).unwrap();
                assert_eq!(c.snapshot().title, expected);
                h.send(
                    &id,
                    "second",
                    "A different topic".into(),
                    Settings::default(),
                )
                .unwrap();
                let (request, reply) =
                    tokio::time::timeout(Duration::from_secs(5), requests.recv())
                        .await
                        .unwrap()
                        .unwrap();
                let body: Value =
                    serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap();
                assert_ne!(body["tools"], json!([]), "later messages must not rename");
                reply.send(answer("Second response")).unwrap();
                until(&h, || !c.is_turn_active()).await;
                assert_eq!(store.load_session(&id).unwrap().title, expected);
            }
            assert!(requests.try_recv().is_err());
            server.abort();
        }
    }
}

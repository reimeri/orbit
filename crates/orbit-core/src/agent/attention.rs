//! Acknowledging terminal notifications is separate from stopping work or changing outcomes.
use super::*;

/// The attention visible at one user action. Later failures must not be acknowledged
/// by a storage job queued for an earlier view of the session.
pub struct Attention {
    conversation: Option<(Arc<Conversation>, u64)>,
    parent: String,
    runs: Vec<String>,
}

impl Harness {
    pub fn attention(&self, session: &str) -> Option<Attention> {
        let coordinator = self.subagents.lock().expect("subagent lock");
        let conversation = self.get(session).and_then(|c| {
            let state = c.state.lock().expect("session lock");
            let generation = c.turn_generation.load(Ordering::Acquire);
            let needs_attention = !c.is_executing()
                && (state.needs_attention
                    || state.interrupted
                    || c.status() == ConversationStatus::NeedsAttention);
            drop(state);
            needs_attention.then_some((c, generation))
        });
        let parent = self
            .summaries
            .lock()
            .expect("summary lock")
            .get(session)
            .and_then(|s| s.child.as_ref())
            .map(|c| c.parent_id.clone())
            .unwrap_or_else(|| session.to_owned());
        let runs: Vec<_> = coordinator
            .runs(&parent)
            .filter(|r| {
                (parent == session || r.session_id == session)
                    && r.status.needs_attention()
                    && !r.attention_acknowledged
            })
            .map(|r| r.run_id.clone())
            .collect();
        (conversation.is_some() || !runs.is_empty()).then_some(Attention {
            conversation,
            parent,
            runs,
        })
    }

    /// Called on the serialized storage worker, never the UI thread.
    pub fn acknowledge_attention(&self, attention: Attention) -> Result<()> {
        let mut coordinator = self.subagents.lock().expect("subagent lock");
        if !attention.runs.is_empty() {
            // Persist retained terminal results first: an acknowledgement must never
            // replace unsaved result metadata or be undone by its subsequent retry.
            self.retry_results(&mut coordinator, &attention.parent)?;
            let runs: Vec<_> = attention
                .runs
                .iter()
                .filter_map(|id| coordinator.run(&attention.parent, id))
                .filter(|r| !r.attention_acknowledged)
                .map(|r| {
                    let mut r = r.clone();
                    r.attention_acknowledged = true;
                    r
                })
                .collect();
            {
                let mut db = self.store.db.lock().expect("database lock");
                let transaction = db.transaction()?;
                for run in &runs {
                    transaction.execute(
                        "UPDATE subagent_runs SET metadata=?2 WHERE id=?1",
                        rusqlite::params![run.run_id, serde_json::to_string(run)?],
                    )?;
                }
                transaction.commit()?;
            }
            for run in runs {
                coordinator.record(run);
            }
            self.catalog_revision.fetch_add(1, Ordering::Release);
            self.changes.send_replace(());
        }
        if let Some((c, generation)) = attention.conversation {
            let mut saved = c.persisted.lock().expect("persistence lock");
            {
                let mut state = c.state.lock().expect("session lock");
                // Selection, deletion and new work may overtake a queued action.
                if c.turn_generation.load(Ordering::Acquire) != generation
                    || c.is_executing()
                    || !c.loaded.load(Ordering::Acquire)
                    || c.deleted.load(Ordering::Acquire)
                {
                    return Ok(());
                }
                state.needs_attention = false;
                state.interrupted = false;
                c.set_status(ConversationStatus::Ready);
                self.catalog_revision.fetch_add(1, Ordering::Release);
                c.changed();
            }
            // As with edits, retain unsaved changes in memory if storage fails.
            c.save_locked(&mut saved)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn attention_acknowledgement_preserves_new_failures_and_survives_reload() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().into()).unwrap();
        let h = Harness::new(store.clone(), vec![]).unwrap();
        let c = h.insert(Session::new(dir.path().into(), &Settings::default()));
        let mut changes = h.subscribe();
        let mut earlier = None;
        for submission in ["first", "second"] {
            h.send(
                &c.id,
                submission,
                "Offline request".into(),
                Settings::default(),
            )
            .unwrap();
            tokio::time::timeout(Duration::from_secs(3), async {
                while c.is_executing() {
                    changes.changed().await.unwrap();
                }
            })
            .await
            .unwrap();
            assert!(c.snapshot().needs_attention);
            if let Some(earlier) = earlier.take() {
                h.acknowledge_attention(earlier).unwrap();
                assert!(
                    c.snapshot().needs_attention,
                    "a newer failure must remain visible"
                );
            }
            earlier = h.attention(&c.id);
        }
        // Draft saves can already be queued when the user presses Escape. They
        // must not invalidate acknowledgement of the same stopped turn.
        h.edit(&c.id, |s| s.draft = "Keep this unsent draft λ".into())
            .unwrap();
        let before = c.snapshot();
        let db = rusqlite::Connection::open(store.root().join("sessions.sqlite3")).unwrap();
        db.execute_batch("CREATE TRIGGER fail_ack BEFORE UPDATE ON sessions BEGIN SELECT RAISE(FAIL,'disk fixture'); END;").unwrap();
        assert!(h.acknowledge_attention(earlier.unwrap()).is_err());
        assert!(store.load_session(&c.id).unwrap().needs_attention);
        db.execute_batch("DROP TRIGGER fail_ack;").unwrap();
        h.flush().unwrap();
        h.evict_inactive(None).unwrap();
        let reloaded = h.load(&c.id).unwrap();
        let state = reloaded.snapshot();
        assert!(!state.needs_attention && !state.interrupted);
        assert!(h.attention(&c.id).is_none());
        assert_eq!(state.draft, "Keep this unsent draft λ");
        assert!(state.entries.iter().eq(before.entries.iter()));
        assert!(state.input.iter().eq(before.input.iter()));
        assert_ne!(c.status(), ConversationStatus::NeedsAttention);
    }
}

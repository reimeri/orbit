//! Acknowledging terminal notifications is separate from stopping work or changing outcomes.
use super::*;

/// The attention visible at one user action. Later failures must not be acknowledged
/// by a storage job queued for an earlier view of the session.
pub struct Attention {
    conversation: Option<(Arc<Conversation>, u64, Arc<()>)>,
    parent: String,
    ranges: Vec<(String, usize)>,
}

impl Harness {
    pub fn attention(&self, session: &str) -> Option<Attention> {
        let conversation = self.get(session).and_then(|c| {
            let state = c.state.lock().expect("session lock");
            let generation = c.turn_generation.load(Ordering::Acquire);
            let needs_attention = c.loaded.load(Ordering::Acquire)
                && !c.is_executing()
                && (state.needs_attention
                    || state.interrupted
                    || c.status() == ConversationStatus::NeedsAttention);
            // Eviction checks this pin under the same state lock.
            needs_attention.then(|| (c.clone(), generation, c.attention_pin.clone()))
        });
        let parent = self.family_id(session);
        let ranges = self.run_index().attention_ranges(&parent, session);
        (conversation.is_some() || !ranges.is_empty()).then_some(Attention {
            conversation,
            parent,
            ranges,
        })
    }

    /// Ordered on the affected family’s storage lane, never on the UI thread.
    pub fn acknowledge_attention(&self, attention: Attention) -> Result<()> {
        use subagents::{ORDINAL, RUN_COLUMNS, RunInfo, UNACKNOWLEDGED};
        let (_library, family) = self.admit(&attention.parent);
        let mut coordinator = family.lock().expect("family lifecycle");
        let mut skipped = Vec::new();
        if !attention.ranges.is_empty() {
            // Unsaved terminal results must become durable before this query.
            self.retry_results(&mut coordinator, &attention.parent)?;
            let mut resident = Vec::new();
            {
                let mut db = self.store.db.lock().expect("database lock");
                let tx = db.transaction()?;
                let mut query = tx.prepare(&format!(
                    "SELECT {RUN_COLUMNS} FROM subagent_runs
                    WHERE parent=?1 AND session=?2 AND {UNACKNOWLEDGED} AND {ORDINAL} <= ?3"
                ))?;
                for (child, through) in &attention.ranges {
                    let mut rows = query.query(rusqlite::params![
                        attention.parent,
                        child,
                        i64::try_from(*through)?
                    ])?;
                    // Validate before touching metadata. Keep only rowids for cold
                    // history, and don't mutate the index while walking its cursor.
                    let mut rowids = Vec::new();
                    while let Some(row) = rows.next()? {
                        let rowid: i64 = row.get("rowid")?;
                        match RunInfo::from_row(row) {
                            Ok(mut run) => {
                                rowids.push(rowid);
                                if self
                                    .run_index()
                                    .run(&attention.parent, &run.run_id)
                                    .is_some()
                                {
                                    run.attention_acknowledged = true;
                                    resident.push(run);
                                }
                            }
                            Err(error) => skipped.push(format!("row {rowid}: {error}")),
                        }
                    }
                    let mut update = tx.prepare_cached("UPDATE subagent_runs SET metadata=json_set(metadata,'$.attention_acknowledged',json('true')) WHERE rowid=?1")?;
                    for rowid in rowids {
                        update.execute([rowid])?;
                    }
                }
                drop(query);
                tx.commit()?;
            }
            let mut index = self.run_index();
            for run in resident {
                index.record(run);
            }
            index.acknowledged(&attention.parent, &attention.ranges);
            drop(index);
            self.catalog_revision.fetch_add(1, Ordering::Release);
            self.changes.send_replace(());
        }
        let result = if skipped.is_empty() {
            Ok(())
        } else {
            Err(anyhow::anyhow!(
                "Readable assignments acknowledged. Skipped damaged subagent assignments ({}); rows preserved. Repair them or restore a backup and restart.",
                skipped.join("; ")
            ))
        };
        if let Some((c, generation, _pin)) = attention.conversation {
            let mut saved = c.persisted.lock().expect("persistence lock");
            {
                let mut state = c.state.lock().expect("session lock");
                // Selection, deletion and new work may overtake a queued action.
                if c.turn_generation.load(Ordering::Acquire) != generation
                    || c.is_executing()
                    || c.deleted.load(Ordering::Acquire)
                {
                    return result;
                }
                c.notifications.clear_terminal(&c.id);
                state.needs_attention = false;
                state.interrupted = false;
                c.set_status(ConversationStatus::Ready);
                self.catalog_revision.fetch_add(1, Ordering::Release);
                c.changed();
            }
            // As with edits, retain unsaved changes in memory if storage fails.
            c.save_locked(&mut saved)?;
        }
        result
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
        h.evict_idle(&c.id).unwrap(); // an already-queued acknowledgement must still apply
        let before = c.snapshot();
        let db = rusqlite::Connection::open(store.root().join("sessions.sqlite3")).unwrap();
        db.execute_batch("CREATE TRIGGER fail_ack BEFORE UPDATE ON sessions BEGIN SELECT RAISE(FAIL,'disk fixture'); END;").unwrap();
        assert!(h.acknowledge_attention(earlier.unwrap()).is_err());
        assert!(store.load_session(&c.id).unwrap().needs_attention);
        db.execute_batch("DROP TRIGGER fail_ack;").unwrap();
        assert!(h.take_notifications().is_empty());
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

//! Family admission, cancellation, and atomic deletion from the session library.
use super::*;

enum Deletion<'a> {
    Session(&'a str),
    Project(&'a std::path::Path),
}

impl Harness {
    pub fn stop(&self, session: &str) {
        let (_library, family) = self.admit(session);
        let mut coordinator = family.lock().expect("family lifecycle");
        if self.family_id(session) == session {
            coordinator.parent_stopped = true;
        }
        if let Some(c) = self.get(session) {
            c.cancel();
        }
        let children: Vec<_> = self.run_index().child_ids(session).cloned().collect();
        for child in children {
            if let Some(c) = self.get(&child) {
                if c.is_running() {
                    c.cancel();
                } else {
                    c.code.stop_all();
                }
            }
        }
        self.catalog_revision.fetch_add(1, Ordering::Release);
        self.changes.send_replace(());
    }
    pub fn remove(&self, session: &str) -> Result<()> {
        self.remove_sessions(Deletion::Session(session))
    }

    /// Delete every conversation in a project as one database transaction.
    /// The library barrier excludes creation of new families during discovery.
    pub fn remove_project(&self, project: &std::path::Path) -> Result<()> {
        self.remove_sessions(Deletion::Project(project))
    }

    fn remove_sessions(&self, target: Deletion<'_>) -> Result<()> {
        // A project spans an open-ended set of families; close admission while
        // discovering and deleting it. A single-family deletion is local.
        let project_deletion = matches!(target, Deletion::Project(_));
        let global = project_deletion.then(|| self.library.write().expect("library lock"));
        let local = (!project_deletion).then(|| self.library.read().expect("library lock"));
        let parents: HashSet<String> = match target {
            Deletion::Session(session) => {
                if !self
                    .summaries
                    .lock()
                    .expect("summary lock")
                    .contains_key(session)
                {
                    bail!("Session not found: {session}");
                }
                if self.family_id(session) != session {
                    bail!("Delete children together with their parent");
                }
                [session.to_owned()].into()
            }
            Deletion::Project(project) => self
                .summaries()
                .into_iter()
                .filter(|s| s.project == project && s.child.is_none())
                .map(|s| s.id)
                .collect(),
        };
        let mut ordered_parents: Vec<_> = parents.iter().collect();
        ordered_parents.sort_unstable();
        let gates: Vec<_> = ordered_parents
            .into_iter()
            .map(|id| self.family(id))
            .collect();
        let mut reservations: Vec<_> = gates
            .iter()
            .map(|f| f.lock().expect("family lifecycle"))
            .collect();
        let mut ids = parents.clone();
        let index = self.run_index();
        ids.extend(parents.iter().flat_map(|id| index.child_ids(id).cloned()));
        drop(index);
        // A child's only assignment may have been skipped during startup recovery.
        ids.extend(
            self.summaries
                .lock()
                .expect("summary lock")
                .values()
                .filter(|s| {
                    s.child
                        .as_ref()
                        .is_some_and(|child| parents.contains(&child.parent_id))
                })
                .map(|s| s.id.clone()),
        );
        if parents.iter().any(|id| self.active_children(id) > 0)
            || ids.iter().any(|id| {
                self.get(id).is_some_and(|c| {
                    if project_deletion {
                        c.is_executing()
                    } else {
                        c.is_turn_active()
                    }
                })
            })
        {
            bail!("Stop all running sessions and their subagents before deleting. Then retry.");
        }
        let mut order: Vec<_> = ids.iter().collect();
        order.sort_unstable();
        let bodies: Vec<_> = order.iter().map(|id| self.load_gate(id)).collect();
        let loading: Vec<_> = bodies
            .iter()
            .map(|gate| gate.lock().expect("loading lock"))
            .collect();
        let family: Vec<_> = order.into_iter().filter_map(|id| self.get(id)).collect();
        // Exclude late native callbacks: none can recreate a just-deleted row.
        let _saved: Vec<_> = family
            .iter()
            .map(|c| c.persisted.lock().expect("persistence lock"))
            .collect();
        // Serialize publication with logical deletion, not merely directory cleanup.
        let mut media_leases: Vec<_> = family
            .iter()
            .filter_map(|c| c.code.media_lifecycle())
            .collect();
        // Delete the whole family atomically before evicting runtime state.
        let unused_media = self.store.delete_sessions(&ids)?;
        for lease in &mut media_leases {
            **lease = false;
        }
        drop(media_leases);
        for id in &ids {
            if let Some(c) = self.get(id) {
                c.deleted.store(true, Ordering::Release);
                c.naming_cancel.cancel();
                c.cancel();
            }
            self.provider.discard_continuation(id);
            self.browser.forget(id);
            self.notifications.clear(id);
            self.conversations
                .lock()
                .expect("conversations lock")
                .remove(id);
            self.summaries.lock().expect("summary lock").remove(id);
        }
        for parent in &parents {
            self.run_index().forget_family(parent);
        }
        drop(_saved);
        drop(loading);
        for state in &mut reservations {
            **state = subagents::Coordinator::new(true);
        }
        drop(reservations);
        drop(gates);
        // Never replace a family gate underneath an already-admitted waiter.
        let mut registry = self.families.lock().expect("family registry");
        for parent in parents {
            if registry
                .gates
                .get(&parent)
                .is_some_and(|gate| Arc::strong_count(gate) == 1)
            {
                registry.gates.remove(&parent);
            }
        }
        drop(registry);
        drop(local);
        drop(global);
        let mut cleanup_errors = Vec::new();
        for owner in unused_media {
            if let Err(e) = crate::media::remove(&self.store.root().join("media").join(&owner)) {
                cleanup_errors.push(format!("{owner}: {e:#}"));
            }
        }
        self.catalog_revision.fetch_add(1, Ordering::Release);
        self.changes.send_replace(());
        if !cleanup_errors.is_empty() {
            bail!(
                "Session family was deleted and cannot be recreated by late callbacks, but private media cleanup needs attention under {}: {}",
                self.store.root().join("media").display(),
                cleanup_errors.join("; ")
            );
        }
        Ok(())
    }
}

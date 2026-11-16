//! Live desktop-attention events, independent of transcript replay and UI refresh coalescing.
use super::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Kind {
    Question(String),
    Finished,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Event {
    pub id: u64,
    pub session_id: String,
    pub title: String,
    pub kind: Kind,
}

#[derive(Default)]
struct State {
    serial: u64,
    pending: Vec<Event>,
    current: HashMap<String, Event>,
}

#[derive(Default)]
pub(super) struct Notifications(Mutex<State>);
impl Notifications {
    pub(super) fn publish(&self, session: &Session, kind: Kind) {
        let mut state = self.0.lock().expect("notifications");
        state.serial += 1;
        let event = Event {
            id: state.serial,
            session_id: session.id.clone(),
            title: session.title.clone(),
            kind,
        };
        state.current.insert(session.id.clone(), event.clone());
        state.pending.push(event);
    }
    pub(super) fn clear(&self, session: &str) {
        self.0
            .lock()
            .expect("notifications")
            .current
            .remove(session);
    }
    pub(super) fn clear_terminal(&self, session: &str) {
        let mut state = self.0.lock().expect("notifications");
        if state
            .current
            .get(session)
            .is_some_and(|e| !matches!(e.kind, Kind::Question(_)))
        {
            state.current.remove(session);
        }
    }
    pub(super) fn clear_question(&self, session: &str) {
        let mut state = self.0.lock().expect("notifications");
        if state
            .current
            .get(session)
            .is_some_and(|e| matches!(e.kind, Kind::Question(_)))
        {
            state.current.remove(session);
        }
    }
}
impl Harness {
    /// Single-consumer queue: refresh signals can coalesce without losing live events.
    /// Superseded events (answered questions, stopped/restarted/deleted work) are obsolete.
    pub fn take_notifications(&self) -> Vec<Event> {
        let mut state = self.notifications.0.lock().expect("notifications");
        std::mem::take(&mut state.pending)
            .into_iter()
            .filter(|e| state.current.get(&e.session_id) == Some(e))
            .collect()
    }
    pub fn notification_is_current(&self, event: &Event) -> bool {
        self.notifications
            .0
            .lock()
            .expect("notifications")
            .current
            .get(&event.session_id)
            == Some(event)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn settled(c: &Conversation, h: &Harness) {
        let mut changes = h.subscribe();
        tokio::time::timeout(Duration::from_secs(5), async {
            while c.is_executing() {
                changes.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn terminal_notifications_are_live_once_and_survive_eviction_not_restart() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().into()).unwrap();
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
        let (url, mut requests, server) = crate::test_support::gated_server().await;
        let h =
            Harness::with_provider(store.clone(), vec![], Provider::fixture(store.clone(), url))
                .unwrap();
        let mut session = Session::new(dir.path().into(), &Settings::default());
        session.title = "Asymmetric session λ".into();
        let id = session.id.clone();
        let c = h.insert(session);
        h.send(&id, "first", "Work".into(), Settings::default())
            .unwrap();
        let (_, reply) = tokio::time::timeout(Duration::from_secs(3), requests.recv())
            .await
            .unwrap()
            .unwrap();
        // Age the running session without sleeping across wall-clock seconds.
        h.edit(&id, |s| s.updated = 77).unwrap();
        let completed_after = crate::store::now();
        reply.send((200, crate::test_support::completed(json!([{"type":"message","id":"final","phase":"final_answer","content":[{"type":"output_text","text":"Private response, not notification content"}]}])))).unwrap();
        settled(&c, &h).await;
        let completed_at = c.snapshot().updated;
        assert!((completed_after..=crate::store::now()).contains(&completed_at));
        assert_eq!(h.summaries()[0].updated, completed_at);
        let events = h.take_notifications();
        assert_eq!(events.len(), 1);
        let done = &events[0];
        assert_eq!(done.kind, Kind::Finished);
        assert_eq!(done.title, "Asymmetric session λ");
        assert_eq!(done.session_id, id);
        assert!(c.snapshot().unchecked_completion);
        assert!(h.summaries()[0].unchecked_completion);
        assert!(h.take_notifications().is_empty());
        h.evict_inactive(None).unwrap();
        assert!(h.get(&id).is_none());
        // Ordinary eviction saves the sidebar hint; no completion-specific commit.
        assert!(store.load_session(&id).unwrap().unchecked_completion);
        assert_eq!(store.load_session(&id).unwrap().updated, completed_at);
        let restarted = Harness::new(store.clone(), store.load().unwrap().1).unwrap();
        assert!(restarted.summaries()[0].unchecked_completion);
        assert!(restarted.load(&id).unwrap().snapshot().unchecked_completion);
        assert!(h.notification_is_current(done));
        h.send(&id, "second", "More work".into(), Settings::default())
            .unwrap();
        assert!(!h.notification_is_current(done));
        assert!(!h.get(&id).unwrap().snapshot().unchecked_completion);
        assert!(!store.load_session(&id).unwrap().unchecked_completion);
        let (_, reply) = tokio::time::timeout(Duration::from_secs(3), requests.recv())
            .await
            .unwrap()
            .unwrap();
        h.edit(&id, |s| s.updated = 88).unwrap();
        reply.send((400, "Private provider error".into())).unwrap();
        settled(&h.get(&id).unwrap(), &h).await;
        assert_eq!(h.get(&id).unwrap().snapshot().updated, 88);
        let errors = h.take_notifications();
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].kind, Kind::Failed);
        assert_ne!(errors[0].id, done.id);
        assert!(h.get(&id).unwrap().snapshot().needs_attention);
        assert!(!h.get(&id).unwrap().snapshot().unchecked_completion);
        server.abort();
        let restarted = Harness::new(store.clone(), store.load().unwrap().1).unwrap();
        restarted.load(&id).unwrap();
        assert!(restarted.take_notifications().is_empty());
        h.evict_inactive(None).unwrap();
        h.remove(&id).unwrap();
        assert!(!h.notification_is_current(&errors[0]));
    }

    #[tokio::test]
    async fn questionnaires_notify_only_the_actionable_request_and_stop_withdraws_it() {
        let dir = tempfile::tempdir().unwrap();
        let h = Harness::new(Store::open(dir.path().into()).unwrap(), vec![]).unwrap();
        let c = h.insert(Session::new(dir.path().into(), &Settings::default()));
        let questions = c.questionnaires.as_ref().unwrap();
        let mut asks = vec![];
        for id in ["first", "second"] {
            let q = questions.clone();
            let cancel = c.cancel.lock().unwrap().clone();
            asks.push(tokio::spawn(async move { q.ask(id, r#"{"questions":[{"id":"a","question":"Private question","kind":"text"},{"id":"b","question":"Other private question","kind":"text"}]}"#, &cancel).await }));
            let mut changes = h.subscribe();
            tokio::time::timeout(Duration::from_secs(3), async {
                while questions.pending().iter().all(|q| q.id != id) {
                    changes.changed().await.unwrap();
                }
            })
            .await
            .unwrap();
        }
        let events = h.take_notifications();
        assert_eq!(
            events.len(),
            1,
            "one notification per actionable questionnaire, not per question or queued ask"
        );
        assert_eq!(events[0].kind, Kind::Question("first".into()));
        questions
            .submit("first", vec!["Answer one".into(), "Answer two".into()])
            .unwrap();
        assert!(!h.notification_is_current(&events[0]));
        let next = h.take_notifications();
        assert_eq!(next.len(), 1);
        assert_eq!(next[0].kind, Kind::Question("second".into()));
        assert!(asks.remove(0).await.unwrap().is_ok());
        h.stop(&c.id);
        assert!(!h.notification_is_current(&next[0]));
        let second = asks.pop().unwrap().await.unwrap();
        assert!(second.is_err());
        assert!(
            h.take_notifications().is_empty(),
            "user cancellation is not an error notification"
        );
    }
}

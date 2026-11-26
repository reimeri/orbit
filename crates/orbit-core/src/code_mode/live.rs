//! Transient stop handles, owned by the execution they control (not its transcript row).
use super::ToolStop;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
use tokio_util::sync::CancellationToken;

pub(crate) struct Calls {
    tokens: Mutex<HashMap<String, CancellationToken>>,
    changes: tokio::sync::watch::Sender<()>,
}

impl Calls {
    pub(crate) fn new(changes: tokio::sync::watch::Sender<()>) -> Self {
        Self {
            tokens: Mutex::default(),
            changes,
        }
    }

    pub(super) fn register(self: &Arc<Self>, id: String, token: &CancellationToken) -> Call {
        self.tokens
            .lock()
            .expect("live calls")
            .insert(id.clone(), token.clone());
        self.changes.send_replace(());
        Call {
            id,
            calls: self.clone(),
        }
    }

    pub(crate) fn state(&self, id: &str) -> Option<ToolStop> {
        self.tokens
            .lock()
            .expect("live calls")
            .get(id)
            .map(|token| {
                if token.is_cancelled() {
                    ToolStop::Stopping
                } else {
                    ToolStop::Available
                }
            })
    }

    pub(crate) fn stop(&self, id: &str) {
        if let Some(token) = self.tokens.lock().expect("live calls").get(id) {
            token.cancel();
            self.changes.send_replace(());
        }
    }
}

pub(super) struct Call {
    id: String,
    calls: Arc<Calls>,
}

impl Drop for Call {
    fn drop(&mut self) {
        self.calls
            .tokens
            .lock()
            .expect("live calls")
            .remove(&self.id);
        self.calls.changes.send_replace(());
    }
}

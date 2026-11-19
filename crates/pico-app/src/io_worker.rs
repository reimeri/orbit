//! Serialized blocking I/O, with results returned to the UI as messages.
use crate::ui::Message;
use iced::Task;
use pico_core::store::{Catalog, Settings, Store};
use std::sync::mpsc;

/// Immediate controls are independent of the preferences submitted by Save.
pub fn with_live_controls(staged: &Settings, live: &Settings) -> Settings {
    Settings {
        theme: live.theme.clone(),
        font_size: live.font_size,
        chat_width: live.chat_width,
        resume_last: live.resume_last,
        sidebar_width: live.sidebar_width,
        settings_width: live.settings_width,
        ..staged.clone()
    }
}

/// An autosave may already be queued before the UI learns whether Apply succeeded.
/// Compose it with the worker's last successful preferences, never a stale UI snapshot.
pub struct CatalogWriter {
    committed: Catalog,
}
impl CatalogWriter {
    pub fn save_state(
        &mut self,
        store: &Store,
        mut catalog: Catalog,
    ) -> Result<(), pico_core::Error> {
        catalog.settings = with_live_controls(&self.committed.settings, &catalog.settings);
        self.write(store, catalog)
    }

    pub fn apply(&mut self, store: &Store, submitted: Settings) -> Result<(), pico_core::Error> {
        let mut catalog = self.committed.clone();
        catalog.settings = with_live_controls(&submitted, &catalog.settings);
        self.write(store, catalog)
    }

    fn write(&mut self, store: &Store, catalog: Catalog) -> Result<(), pico_core::Error> {
        let result = store.save_catalog(&catalog);
        if result.is_ok()
            || result
                .as_ref()
                .is_err_and(|e| e.is::<pico_core::store::PublishedButNotSynced>())
        {
            self.committed = catalog;
        }
        result
    }
}

type Job = Box<dyn FnOnce(&mut CatalogWriter) + Send>;
pub struct Worker {
    sender: mpsc::Sender<Job>,
}
impl Worker {
    pub fn new(committed: Catalog) -> Self {
        let (sender, receiver) = mpsc::channel::<Job>();
        std::thread::Builder::new()
            .name("pico-ui-storage".into())
            .spawn(move || {
                let mut writer = CatalogWriter { committed };
                for job in receiver {
                    job(&mut writer);
                }
            })
            .expect("start storage worker");
        Self { sender }
    }
    pub fn run(&self, job: impl FnOnce() -> Message + Send + 'static) -> Task<Message> {
        self.run_catalog(move |_| job())
    }
    pub fn run_catalog(
        &self,
        job: impl FnOnce(&mut CatalogWriter) -> Message + Send + 'static,
    ) -> Task<Message> {
        let (reply, result) = tokio::sync::oneshot::channel();
        self.sender
            .send(Box::new(move |writer| {
                let _ = reply.send(job(writer));
            }))
            .expect("storage worker alive");
        Task::future(async move { result.await.expect("storage job completes") })
    }
}

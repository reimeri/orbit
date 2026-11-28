//! FIFO per resource. Independent session families may make progress concurrently.
use crate::ui::Message;
use iced::Task;
use iced::futures::{StreamExt, channel::mpsc, future::BoxFuture};
use orbit_core::store::{Catalog, Settings, Store};
use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
};
use tokio::sync::{oneshot, watch};

/// Immediate controls are independent of the preferences submitted by Save.
pub fn with_live_controls(staged: &Settings, live: &Settings) -> Settings {
    Settings {
        theme: live.theme.clone(),
        font_size: live.font_size,
        chat_width: live.chat_width,
        resume_last: live.resume_last,
        hide_working_sessions: live.hide_working_sessions,
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
    ) -> Result<(), orbit_core::Error> {
        catalog.settings = with_live_controls(&self.committed.settings, &catalog.settings);
        self.write(store, catalog)
    }

    pub fn apply(&mut self, store: &Store, submitted: Settings) -> Result<(), orbit_core::Error> {
        let mut catalog = self.committed.clone();
        catalog.settings = with_live_controls(&submitted, &catalog.settings);
        self.write(store, catalog)
    }

    fn write(&mut self, store: &Store, catalog: Catalog) -> Result<(), orbit_core::Error> {
        let result = store.save_catalog(&catalog);
        if result.is_ok()
            || result
                .as_ref()
                .is_err_and(|e| e.is::<orbit_core::store::PublishedButNotSynced>())
        {
            self.committed = catalog;
        }
        result
    }
}

#[derive(Clone, Hash, PartialEq, Eq)]
pub enum Lane {
    Family(String),
    NewDraft,
    Catalog,
    ApiKey,
    Maintenance,
}

#[derive(Default)]
struct Order {
    lanes: HashMap<Lane, watch::Receiver<()>>,
    barrier: Option<watch::Receiver<()>>,
}

pub struct Worker {
    sender: mpsc::UnboundedSender<BoxFuture<'static, ()>>,
    order: RefCell<Order>,
    catalog: Arc<Mutex<CatalogWriter>>,
}
impl Worker {
    pub fn new(committed: Catalog) -> Self {
        let (sender, receiver) = mpsc::unbounded::<BoxFuture<'static, ()>>();
        std::thread::Builder::new()
            .name("orbit-ui-storage".into())
            .spawn(move || {
                tokio::runtime::Builder::new_current_thread()
                    .build()
                    .expect("storage scheduler")
                    .block_on(receiver.for_each_concurrent(None, |job| job));
            })
            .expect("start storage worker");
        Self {
            sender,
            order: RefCell::default(),
            catalog: Arc::new(Mutex::new(CatalogWriter { committed })),
        }
    }

    pub fn run_in(
        &self,
        lanes: impl IntoIterator<Item = Lane>,
        job: impl FnOnce() -> Message + Send + 'static,
    ) -> Task<Message> {
        self.schedule(Some(lanes.into_iter().collect()), job)
    }

    /// Library-wide barrier, including all jobs submitted before the UI starts polling results.
    pub fn run(&self, job: impl FnOnce() -> Message + Send + 'static) -> Task<Message> {
        self.schedule(None, job)
    }

    pub fn run_catalog(
        &self,
        job: impl FnOnce(&mut CatalogWriter) -> Message + Send + 'static,
    ) -> Task<Message> {
        let catalog = self.catalog.clone();
        self.run_in([Lane::Catalog], move || {
            job(&mut catalog.lock().expect("catalog writer"))
        })
    }

    pub fn barrier_catalog(
        &self,
        job: impl FnOnce(&mut CatalogWriter) -> Message + Send + 'static,
    ) -> Task<Message> {
        let catalog = self.catalog.clone();
        self.run(move || job(&mut catalog.lock().expect("catalog writer")))
    }

    fn schedule(
        &self,
        lanes: Option<HashSet<Lane>>,
        job: impl FnOnce() -> Message + Send + 'static,
    ) -> Task<Message> {
        // Register dependencies synchronously in UI submission order. The last
        // sender drops on job completion, not when iced processes its result.
        let (done, tail) = watch::channel(());
        let mut order = self.order.borrow_mut();
        order.lanes.retain(|_, r| r.has_changed().is_ok());
        let mut before: Vec<_> = order.barrier.iter().cloned().collect();
        if let Some(lanes) = lanes {
            before.extend(
                lanes
                    .into_iter()
                    .filter_map(|lane| order.lanes.insert(lane, tail.clone())),
            );
        } else {
            before.extend(order.lanes.drain().map(|(_, r)| r));
            order.barrier = Some(tail);
        }
        let (reply, result) = oneshot::channel();
        self.sender
            .unbounded_send(Box::pin(async move {
                for mut prior in before {
                    let _ = prior.changed().await;
                }
                let result = tokio::task::spawn_blocking(job).await.unwrap_or_else(|e| {
                    Message::External(Err(format!("Storage operation did not finish normally: {e}. Check saved state before retrying; the operation was not replayed.")))
                });
                let _ = reply.send(result);
                drop(done);
            }))
            .expect("storage scheduler alive");
        Task::future(async move { result.await.expect("storage job completes") })
    }
}

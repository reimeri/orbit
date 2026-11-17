//! Serialized blocking I/O, with results returned to the UI as messages.
use crate::ui::Message;
use iced::Task;
use std::sync::mpsc;
type Job = Box<dyn FnOnce() + Send>;
pub struct Worker {
    sender: mpsc::Sender<Job>,
}
impl Worker {
    pub fn new() -> Self {
        let (sender, receiver) = mpsc::channel::<Job>();
        std::thread::Builder::new()
            .name("pico-ui-storage".into())
            .spawn(move || {
                for job in receiver {
                    job();
                }
            })
            .expect("start storage worker");
        Self { sender }
    }
    pub fn run(&self, job: impl FnOnce() -> Message + Send + 'static) -> Task<Message> {
        let (reply, result) = tokio::sync::oneshot::channel();
        self.sender
            .send(Box::new(move || {
                let _ = reply.send(job());
            }))
            .expect("storage worker alive");
        Task::future(async move { result.await.expect("storage job completes") })
    }
}

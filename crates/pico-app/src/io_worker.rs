//! Serialized blocking I/O, with results returned to the UI as messages.
use iced::Task;
use std::sync::mpsc;
type Job<M> = Box<dyn FnOnce() -> M + Send>;
pub struct Worker<M> {
    sender: mpsc::Sender<(Job<M>, tokio::sync::oneshot::Sender<M>)>,
}
impl<M: Send + 'static> Worker<M> {
    pub fn new() -> Self {
        let (sender, receiver) = mpsc::channel::<(Job<M>, tokio::sync::oneshot::Sender<M>)>();
        std::thread::Builder::new()
            .name("pico-ui-storage".into())
            .spawn(move || {
                for (job, reply) in receiver {
                    let _ = reply.send(job());
                }
            })
            .expect("start storage worker");
        Self { sender }
    }
    pub fn run(&self, job: impl FnOnce() -> M + Send + 'static) -> Task<M> {
        let (reply, result) = tokio::sync::oneshot::channel();
        self.sender
            .send((Box::new(job), reply))
            .expect("storage worker alive");
        Task::perform(
            async move { result.await.expect("storage job completes") },
            |message| message,
        )
    }
}

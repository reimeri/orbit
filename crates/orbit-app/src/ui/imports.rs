//! Only the front request prepares or publishes. A multi-file drop queues paths,
//! not open files, decoded pixels, blocking threads, or storage-worker jobs.
use super::*;
use crate::attachments::{Input, Source};
use std::collections::VecDeque;

enum Step {
    Prepare(Source),
    Publish(Input),
    SaveSession,
}

struct Import {
    owner: DraftKey,
    clipboard: bool,
    left_composer: bool,
    // None while the front request is running on a worker.
    next: Option<Step>,
}

#[derive(Default)]
pub(super) struct Imports {
    queue: VecDeque<Import>,
    pub(super) retry_on_save: bool,
}

impl Imports {
    pub(super) fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }
    pub(super) fn left_composer(&mut self) {
        for p in &mut self.queue {
            p.left_composer = true;
        }
    }
    pub(super) fn holds_paste_cursor(&self) -> bool {
        self.queue.iter().any(|p| p.clipboard && !p.left_composer)
    }
}

fn append(input: Input, text: &mut String, images: &mut Vec<orbit_core::media::Media>) {
    match input {
        Input::Text(s) => text.push_str(&s),
        Input::Image(m) => images.push(m),
        Input::Empty => {}
    }
}

impl App {
    pub(super) fn draft_key(&self) -> Option<DraftKey> {
        if self.draft_session.is_some() {
            Some(DraftKey::New)
        } else {
            self.selected.clone().map(DraftKey::Session)
        }
    }
    pub(super) fn composer_importing(&self) -> bool {
        !self.imports.is_empty()
            && self
                .draft_key()
                .is_some_and(|owner| self.imports.queue.iter().any(|p| p.owner == owner))
    }

    pub(super) fn import(&mut self, path: Option<PathBuf>) {
        if !self.can_attach() {
            return;
        }
        let clipboard = path.is_none();
        let source = match path {
            Some(path) => Source::File(path),
            None => {
                // Capture both offers at paste time. Each native reply can buffer MAX_BYTES.
                if self.imports.queue.iter().filter(|p| p.clipboard).count() >= 2 {
                    self.error = Some("Two clipboard pastes are pending. Wait for them to finish, then paste again.".into());
                    return;
                }
                let (send, text) = tokio::sync::oneshot::channel();
                let mut send = Some(send);
                self.tasks.push(iced::clipboard::read().then(move |text| {
                    // A deleted draft may already have released its pending request.
                    let _ = send.take().expect("one clipboard text reply").send(text);
                    Task::none()
                }));
                Source::clipboard(text)
            }
        };
        self.imports.queue.push_back(Import {
            owner: self.draft_key().expect("composer owner"),
            clipboard,
            left_composer: false,
            next: Some(Step::Prepare(source)),
        });
    }

    pub(super) fn import_loaded(&mut self, result: Result<Input, String>) {
        match result {
            Ok(Input::Empty) => {
                self.imports.queue.pop_front();
            }
            Ok(input) => {
                self.imports
                    .queue
                    .front_mut()
                    .expect("preparation in flight")
                    .next = Some(Step::Publish(input))
            }
            Err(error) => {
                self.imports.queue.pop_front();
                self.close_requested = false;
                self.error = Some(format!("Could not attach image: {error}"));
            }
        }
    }

    pub(super) fn import_published(&mut self, released: Result<(), String>) {
        self.imports.queue.pop_front();
        self.fail(released);
    }

    pub(super) fn import_save_failed(&mut self, error: String, pending: Option<Input>) {
        self.error = Some(error);
        self.close_requested = false;
        self.imports
            .queue
            .front_mut()
            .expect("publication in flight")
            .next = Some(pending.map_or(Step::SaveSession, Step::Publish));
        self.imports.retry_on_save = true;
    }

    pub(super) fn advance_imports(&mut self) {
        // Selection/deletion may have snapshotted the old composer. Decide where
        // to publish only after their UI commit; do not put a wait on storage.
        while !self.io_busy && !self.imports.retry_on_save {
            let Some(p) = self.imports.queue.front_mut() else {
                break;
            };
            let Some(step) = p.next.take() else { break };
            let h = self.harness.as_ref().expect("active harness");
            let current = match &p.owner {
                DraftKey::New => self.draft_session.is_some(),
                DraftKey::Session(id) => self.selected.as_ref() == Some(id),
            };
            if !current
                && let DraftKey::Session(id) = &p.owner
                && !self.sessions.iter().any(|s| &s.id == id)
            {
                // Owner explicitly deleted. Skip even the read for queued files.
                self.imports.queue.pop_front();
                continue;
            }
            match step {
                Step::Prepare(source) => {
                    let root = h.store.root().join("attachments");
                    self.tasks.push(Task::future(async move {
                        Message::AttachmentLoaded(
                            tokio::task::spawn_blocking(move || source.prepare(&root))
                                .await
                                .expect("attachment preparation"),
                        )
                    }));
                    break;
                }
                Step::Publish(input) if current => {
                    let append_text = p.left_composer;
                    self.apply_composer_input(input, append_text);
                }
                Step::Publish(input) if p.owner == DraftKey::New => {
                    append(
                        input,
                        &mut self.new_draft.message,
                        &mut self.new_draft.images,
                    );
                    self.new_draft_dirty = true;
                }
                Step::Publish(_) | Step::SaveSession => {
                    let DraftKey::Session(id) = &p.owner else {
                        unreachable!("session write")
                    };
                    let mut pending = if let Step::Publish(input) = step {
                        Some(input)
                    } else {
                        None
                    };
                    let id = id.clone();
                    let h = h.clone();
                    let selected = self.selected.clone();
                    self.tasks.push(self.io.run(move || {
                        let saved = h.edit(&id, |s| {
                            if let Some(input) = pending.take() {
                                append(input, &mut s.draft, &mut s.draft_images);
                            }
                        });
                        // On a failed load we still own the input. A failed save
                        // retains the edited session in memory: retry only its write.
                        match saved {
                            Ok(()) => Message::AttachmentPublished(h.evict_inactive(selected.as_deref())
                                .map_err(|e| format!("Could not release inactive session: {e:#}"))),
                            Err(e) => Message::AttachmentSaveFailed(
                                format!("Could not save imported input to session {id}: {e:#}. Keep Orbit open and fix storage to retry."), pending,
                            ),
                        }
                    }));
                    break;
                }
            }
            self.imports.queue.pop_front();
        }
    }
    fn apply_composer_input(&mut self, input: Input, append_text: bool) {
        match input {
            Input::Image(media) => {
                self.attachments.push(media);
                self.attachment_changed();
            }
            Input::Text(text) => {
                if append_text {
                    self.editor
                        .perform(text_editor::Action::Move(text_editor::Motion::DocumentEnd));
                }
                let before = spellcheck::BeforeEdit::replacement(&self.editor, text.len());
                self.histories.active.perform(
                    &mut self.editor,
                    text_editor::Action::Edit(text_editor::Edit::Paste(Arc::new(text))),
                );
                self.file_completion.edited(&self.editor);
                self.composer_edited(before, false);
            }
            Input::Empty => {}
        }
    }
}

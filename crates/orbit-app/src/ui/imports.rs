//! Only the front request prepares or publishes. A multi-file drop queues paths,
//! not open files, decoded pixels, blocking threads, or storage-worker jobs.
use super::*;
use crate::attachments::{Input, Source};
use crate::image_view::PendingAttachment;
use std::collections::VecDeque;

enum Step {
    Prepare(Source),
    Publish(Input),
    SaveSession,
}

#[derive(PartialEq)]
enum Tile {
    AwaitingImageOffer(u64),
    Visible,
    PublishingOffscreen,
}

struct Import {
    tile: Tile,
    owner: DraftKey,
    clipboard: bool,
    left_composer: bool,
    // None while the front request is running on a worker.
    next: Option<Step>,
}

#[derive(Default)]
pub(super) struct Imports {
    queue: VecDeque<Import>,
    next_paste: u64,
    pub(super) retry_on_save: bool,
}

impl Imports {
    pub(super) fn image_offered(&mut self, paste: u64) {
        // The transfer can fail or finish before iced delivers its offer notice.
        if let Some(p) = self
            .queue
            .iter_mut()
            .find(|p| p.tile == Tile::AwaitingImageOffer(paste))
        {
            p.tile = Tile::Visible;
        }
    }
    pub(super) fn tiles(
        &self,
        owner: Option<DraftKey>,
    ) -> impl Iterator<Item = PendingAttachment> + '_ {
        self.queue
            .iter()
            .enumerate()
            .filter(move |(_, p)| Some(&p.owner) == owner.as_ref() && p.tile == Tile::Visible)
            .map(|(i, _)| {
                if i == 0 {
                    PendingAttachment::Adding
                } else {
                    PendingAttachment::Queued
                }
            })
    }
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

    pub(super) fn cancel_import_send(&mut self, reason: &'static str) {
        if self.send_after_import.take().is_some() {
            self.notice = Some((reason.into(), std::time::Instant::now()));
        }
    }

    pub(super) fn import(&mut self, path: Option<PathBuf>) {
        if !self.composer_available() {
            return;
        }
        self.cancel_import_send("Pending send cancelled because another import was requested.");
        let clipboard = path.is_none();
        let (source, tile) = match path {
            Some(path) => (Source::File(path), Tile::Visible),
            None => {
                // Capture both offers at paste time. Each native reply can buffer MAX_BYTES.
                if self.imports.queue.iter().filter(|p| p.clipboard).count() >= 2 {
                    self.error = Some("Two clipboard pastes are pending. Wait for them to finish, then paste again.".into());
                    return;
                }
                let (send, text) = std::sync::mpsc::channel();
                let mut send = Some(send);
                self.tasks.push(iced::clipboard::read().then(move |text| {
                    // A deleted draft may already have released its pending request.
                    let _ = send.take().expect("one clipboard text reply").send(text);
                    Task::none()
                }));
                let paste = self.imports.next_paste;
                self.imports.next_paste += 1;
                let (notify, offered) = tokio::sync::oneshot::channel();
                let source = Source::clipboard(text, move || {
                    let _ = notify.send(());
                });
                self.tasks.push(
                    Task::future(async move { offered.await.ok() })
                        .and_then(move |()| Task::done(Message::ClipboardImageOffered(paste))),
                );
                (source, Tile::AwaitingImageOffer(paste))
            }
        };
        self.imports.queue.push_back(Import {
            tile,
            owner: self.draft_key().expect("composer owner"),
            clipboard,
            left_composer: false,
            next: Some(Step::Prepare(source)),
        });
    }

    pub(super) fn import_loaded(&mut self, result: Result<Input, String>) {
        if !matches!(result, Ok(Input::Text(_) | Input::Image(_)))
            && self
                .imports
                .queue
                .front()
                .is_some_and(|p| Some(&p.owner) == self.draft_key().as_ref())
        {
            self.cancel_import_send(
                "Not sent: an import did not complete. Review the draft and press Enter to send.",
            );
        }
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
        self.cancel_import_send(
            "Not sent: an import could not be saved. Review the draft and retry.",
        );
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
                                .unwrap_or_else(|e| {
                                    Err(format!("Attachment preparation failed: {e}"))
                                }),
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
                    // A switch back can restore the saved thumbnail before the
                    // publication acknowledgement arrives. Do not render both.
                    p.tile = Tile::PublishingOffscreen;
                    let id = id.clone();
                    let h = h.clone();
                    self.tasks.push(self.io.run_in([Lane::Family(h.family_id(&id))], move || {
                        let saved = h.edit(&id, |s| {
                            if let Some(input) = pending.take() {
                                append(input, &mut s.draft, &mut s.draft_images);
                            }
                        });
                        // On a failed load we still own the input. A failed save
                        // retains the edited session in memory: retry only its write.
                        match saved {
                            Ok(()) if current => Message::AttachmentPublished(Ok(())),
                            Ok(()) => Message::AttachmentPublished(h.evict_idle(&id)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::tests::{app, dispatch};

    #[tokio::test]
    async fn clipboard_tiles_follow_confirmed_offers_not_paste_or_reply_order() {
        let mut app = app();
        let root = app.smoke_dir.clone().unwrap();
        dispatch(&mut app, Message::Create(root)).await;

        // Stand in for two Wayland offers. Neither MIME has arrived yet.
        let mut paste = || {
            let _ = app.update(Message::Paste);
            let Tile::AwaitingImageOffer(id) = app.imports.queue.back().unwrap().tile else {
                panic!("paste should await its offer");
            };
            id
        };
        let text_paste = paste();
        let image_paste = paste();
        let tiles = |app: &App| app.imports.tiles(app.draft_key()).collect::<Vec<_>>();
        assert!(tiles(&app).is_empty());

        dispatch(&mut app, Message::ClipboardImageOffered(image_paste)).await;
        assert!(matches!(
            tiles(&app).as_slice(),
            [PendingAttachment::Queued]
        ));
        let _ = app.update(Message::AttachmentLoaded(Ok(Input::Text("hello".into()))));
        assert_eq!(app.editor.text(), "hello");
        assert!(matches!(
            tiles(&app).as_slice(),
            [PendingAttachment::Adding]
        ));
        dispatch(
            &mut app,
            Message::AttachmentLoaded(Err("transfer failed".into())),
        )
        .await;
        dispatch(&mut app, Message::ClipboardImageOffered(text_paste)).await;
        assert!(tiles(&app).is_empty());
    }
}

use super::*;

impl App {
    pub(super) fn take_draft(&mut self) -> Option<(String, UserMessage)> {
        if !self.draft_dirty {
            return None;
        }
        let id = self.selected.clone()?;
        self.draft_dirty = false;
        Some((id, self.composer_message()))
    }
    pub(super) fn save_draft(&mut self) {
        if self.io_busy {
            return;
        }
        if self.new_draft_dirty
            && !self.saving_new_draft
            && let Some(h) = &self.harness
        {
            let h = h.clone();
            let draft = self.new_draft.clone();
            self.new_draft_dirty = false;
            self.saving_new_draft = true;
            self.tasks.push(self.io.run_in([Lane::NewDraft], move || Message::NewDraftSaved(
                h.store.save_new_draft(&draft).map_err(|e| format!("Could not save new-session draft: {e:#}. Keep Orbit open and retry after fixing storage."))
            )));
        }
        if self.saving_session_draft.is_none()
            && let Some((id, draft)) = self.take_draft()
            && let Some(h) = &self.harness
        {
            let h = h.clone();
            self.saving_session_draft = Some(id.clone());
            self.tasks.push(self.io.run_in([Lane::Family(h.family_id(&id))], move || {
                let result = h.edit(&id, |s| {
                    s.draft = draft.text;
                    s.draft_images = draft.images;
                }).map_err(|e| {
                    format!("Could not save draft: {e:#}. Keep Orbit open and retry after fixing storage.")
                });
                Message::DraftSaved(id, result)
            }));
        }
    }
    pub(super) fn restore_draft(&mut self) {
        let workspace = self
            .current()
            .filter(|s| s.child.is_none())
            .map(|s| (s.id.clone(), s.project.clone()));
        let cache = if let Some(dir) = &self.smoke_dir {
            dir.join("file-cache")
        } else {
            orbit_core::file_index::cache_dir()
        };
        if let Err(error) = self.file_completion.workspace(workspace, cache) {
            self.error = Some(format!("Could not start file search: {error:#}"));
        }
        let editor = text_editor::Content::with_text(if self.draft_session.is_some() {
            &self.new_draft.message
        } else {
            self.current_session.as_ref().map_or("", |s| &s.draft)
        });
        let owner = self.draft_key();
        self.histories.switch(&self.editor, owner, &editor);
        self.editor = editor;
        self.attachment_epoch += 1;
        self.attachments = if self.draft_session.is_some() {
            self.new_draft.images.clone()
        } else {
            self.current_session
                .as_ref()
                .map_or_else(Vec::new, |s| s.draft_images.clone())
        };
        self.draft_dirty = false;
        self.spellcheck.clear();
        self.check_draft(false);
        self.prepare_visible();
    }
    pub(super) fn queue_spellcheck(&mut self, debounce: bool) {
        self.tasks
            .push(self.spellcheck.schedule(debounce).map(Message::Spellcheck));
    }
    pub(super) fn check_draft(&mut self, debounce: bool) {
        if self.catalog.settings.spellcheck.enabled && !self.editor.is_empty() {
            self.queue_spellcheck(debounce);
        } else {
            self.spellcheck.clear();
        }
    }
    pub(super) fn discover_dictionaries(&mut self) {
        if self.spellcheck.languages.is_none() {
            self.queue_spellcheck(false);
        }
    }
    pub(super) fn composer_edited(&mut self, before: spellcheck::BeforeEdit, typing: bool) {
        before.retain_untouched(&mut self.spellcheck.marks, &self.editor);
        self.spellcheck.typing_at = typing.then(|| self.editor.cursor().position);
        self.check_draft(true);
        if self.draft_session.is_some() {
            self.new_draft.message = self.editor.text();
            self.new_draft_dirty = true;
        } else {
            self.draft_dirty = true;
        }
    }
    pub(super) fn composer_message(&self) -> UserMessage {
        UserMessage {
            skills: vec![],
            files: vec![],
            text: self.editor.text(),
            images: self.attachments.clone(),
        }
    }
    pub(super) fn attachment_changed(&mut self) {
        self.attachment_epoch += 1;
        if self.draft_session.is_some() {
            self.new_draft.images = self.attachments.clone();
            self.new_draft_dirty = true;
        } else {
            self.draft_dirty = true;
        }
        self.prepare_visible();
    }
    pub(super) fn composer_available(&self) -> bool {
        self.current().is_some_and(|s| s.child.is_none())
            && !self.closing
            && !self.close_requested
            && !self.io_busy
            && self.modal.is_none()
            && self.active_questionnaire.is_none()
            && self.context_menu.is_none()
            && !self.footer_menu_open
            && self.worktrees.popup.is_none()
    }
    pub(super) fn send(&mut self, owner: String) {
        if self.current().is_none_or(|s| s.id != owner) {
            self.notice = Some((
                "Not sent: the active draft changed.".into(),
                std::time::Instant::now(),
            ));
            return;
        };
        if self.io_busy {
            if !self.sending_message {
                let reason = if self.switching_session {
                    "the session is changing"
                } else {
                    "another operation is in progress"
                };
                self.notice = Some((
                    format!("Not sent: {reason}. Press Enter again when ready."),
                    std::time::Instant::now(),
                ));
            }
            return;
        }
        if !self.composer_available() {
            self.notice = Some((
                "Not sent: return to the composer and press Enter again.".into(),
                std::time::Instant::now(),
            ));
            return;
        }
        if self.composer_importing() {
            self.send_after_import = Some(owner);
            self.file_completion.dismiss();
            return;
        }
        let message = self.composer_message();
        if message.is_empty() {
            self.notice = Some((
                "Not sent: type a message or attach an image first.".into(),
                std::time::Instant::now(),
            ));
            return;
        }
        let Some(h) = self.harness.clone() else {
            return;
        };
        let id = owner;
        let draft = self.draft_session.clone();
        self.file_completion.dismiss();
        let settings = self.catalog.settings.clone();
        self.io_busy = true;
        if message.text == crate::file_completion::COMPACT {
            if draft.is_some() || !message.images.is_empty() {
                self.io_busy = false;
                self.error = Some(
                    if !message.images.is_empty() {
                        "Remove the attachments before running /compact."
                    } else {
                        "There is no conversation context to compact yet."
                    }
                    .into(),
                );
                return;
            }
            self.tasks
                .push(self.io.run_in([Lane::Family(h.family_id(&id))], move || {
                    let result = h.compact(&id, settings).map_err(|e| format!("{e:#}"));
                    Message::Sent(id, message, result)
                }));
            return;
        }
        self.sending_message = true;
        let submission = self
            .submissions
            .entry(id.clone())
            .or_insert_with(|| (message.clone(), orbit_core::store::id()));
        if submission.0 != message {
            *submission = (message.clone(), orbit_core::store::id());
        }
        let submission = submission.1.clone();
        let queued = orbit_core::send_trace::queued();
        let mut lanes = vec![Lane::Family(h.family_id(&id))];
        if draft.is_some() {
            lanes.push(Lane::NewDraft);
        }
        self.tasks.push(self.io.run_in(lanes, move || {
            let result = orbit_core::send_trace::run(queued, || {
                if let Some(draft) = draft {
                    h.send_new(draft, &submission, message.clone(), settings)
                } else {
                    h.send(&id, &submission, message.clone(), settings)
                }
            }).map_err(|e| format!("Could not send message: {e:#}. Your message is preserved; fix the problem and retry."));
            Message::Sent(id, message, result)
        }));
    }
    pub(super) fn file_completion_enabled(&self) -> bool {
        !self.imports.holds_paste_cursor()
            && !self.io_busy
            && !self.closing
            && !self.close_requested
            && self.modal.is_none()
            && self.context_menu.is_none()
            && self.active_questionnaire.is_none()
            && !self.footer_menu_open
            && self.worktrees.popup.is_none()
    }
    pub(super) fn composer_editor<'a>(
        &'a self,
        owner: &'a str,
        running: bool,
    ) -> Element<'a, Message> {
        let line_height = self.catalog.settings.font_size * COMPOSER_LINE_HEIGHT_FACTOR;
        let editor = text_editor(&self.editor)
            .id("composer")
            .style(theme::inline_answer_editor)
            .placeholder(if running {
                "Steer the agent after its current tool batch…"
            } else {
                "Message Orbit…"
            })
            .on_action(Message::Editor)
            .key_binding(move |event| {
                if self.file_completion_enabled()
                    && self.file_completion.is_open()
                    && event.key == keyboard::Key::Named(keyboard::key::Named::Escape)
                {
                    // Let the central handler dismiss the picker without the
                    // native Unfocus binding taking focus away from the composer.
                    return None;
                }
                self.file_completion_enabled()
                    .then(|| self.file_completion.binding(&event))
                    .flatten()
                    .map(|message| text_editor::Binding::Custom(Message::FileCompletion(message)))
                    .or_else(|| composer_key_binding(owner, event))
            })
            .line_height(iced::widget::text::LineHeight::Absolute(line_height.into()))
            .min_height(COMPOSER_MIN_LINES as f32 * line_height)
            .size(self.catalog.settings.font_size);
        let revision = self.spellcheck.revision();
        let editor = crate::growing_editor::growing_editor(
            editor,
            &self.editor,
            Some(crate::growing_editor::Spelling {
                marks: &self.spellcheck.marks,
                typing_at: self.spellcheck.typing_at,
                on_press: Box::new(move |mark| {
                    Message::Press(Press::SpellingMenu(spellcheck::Target { revision, mark }))
                }),
            }),
            COMPOSER_MAX_LINES as f32 * line_height + 2. * crate::growing_editor::PADDING,
            Message::EditorScroll,
            |message| match message {
                Message::Editor(text_editor::Action::Scroll { lines }) => Some(*lines),
                _ => None,
            },
        );
        self.session_list_focus.composer(editor)
    }
}

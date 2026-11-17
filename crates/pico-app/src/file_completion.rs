//! Composer-local completion state; the shared index never runs on the UI worker.
use crate::theme;
use iced::futures::SinkExt;
use iced::{
    Element, Fill, Subscription, keyboard,
    widget::{column, container, row, text, text_editor, tooltip},
};
use pico_core::{
    file_index::{Response, Service},
    file_tags::{self, Token},
};
use std::{
    collections::VecDeque,
    hash::{Hash, Hasher},
    path::PathBuf,
    sync::Arc,
};

#[derive(Clone, Debug)]
pub enum Message {
    Results(Response),
    Move(bool),
    Accept {
        request: u64,
        generation: u64,
        path: Option<String>,
    },
    Refresh,
    Ignored,
}
#[derive(Default)]
pub struct Completion {
    services: VecDeque<Arc<Service>>,
    service: Option<Arc<Service>>,
    session: Option<String>,
    token: Option<(usize, Token)>,
    dismissed: Option<(usize, Token)>,
    request: u64,
    response: Response,
    selected: usize,
    ignored: bool,
}
impl Completion {
    pub fn workspace(
        &mut self,
        session: Option<(String, PathBuf)>,
        cache: PathBuf,
    ) -> Result<(), pico_core::Error> {
        if self.session.as_deref() == session.as_ref().map(|s| s.0.as_str()) {
            return Ok(());
        }
        self.session = session.as_ref().map(|s| s.0.clone());
        self.token = None;
        self.dismissed = None;
        self.response = Response::default();
        self.request += 1;
        self.ignored = false;
        if let Some(service) = &self.service {
            service.cancel_query();
        }
        self.service = None;
        if let Some((_, root)) = session {
            let service = if let Some(index) = self
                .services
                .iter()
                .position(|service| service.root == root)
            {
                self.services.remove(index).expect("cached service")
            } else {
                Service::open(root, cache)?
            };
            self.services.push_back(service.clone());
            self.service = Some(service);
            while self.services.len() > 2 {
                self.services.pop_front();
            }
        }
        Ok(())
    }
    pub fn edited(&mut self, editor: &text_editor::Content) {
        let cursor = editor.cursor();
        let next = if cursor.selection.is_none() && self.service.is_some() {
            editor
                .line(cursor.position.line)
                .and_then(|line| file_tags::active(&line.text, cursor.position.column))
                .map(|token| (cursor.position.line, token))
        } else {
            None
        };
        if self.token == next {
            return;
        }
        if self.dismissed != next {
            self.dismissed = None;
        }
        let same_token =
            self.token
                .as_ref()
                .zip(next.as_ref())
                .is_some_and(|((line, old), (new_line, new))| {
                    line == new_line && old.range.start == new.range.start
                });
        if !same_token {
            self.response = Response::default();
            self.selected = 0;
            self.ignored = false;
        }
        self.token = next;
        if self.is_open() {
            self.query();
        } else if let Some(service) = &self.service {
            service.cancel_query();
            self.request += 1;
        }
    }
    fn query(&mut self) {
        if let (Some(service), Some((_, token))) = (&self.service, &self.token) {
            self.request += 1;
            self.response.pending = true;
            service.query(self.request, token.query.clone(), self.ignored);
        }
    }
    pub fn is_open(&self) -> bool {
        self.token.is_some() && self.token != self.dismissed
    }
    pub fn dismiss(&mut self) {
        self.dismissed = self.token.clone();
        if let Some(service) = &self.service {
            service.cancel_query();
        }
        self.request += 1;
    }
    /// Returns whether the editor was mutated; drafts use the normal autosave path.
    pub fn update(&mut self, message: Message, editor: &mut text_editor::Content) -> bool {
        match message {
            Message::Results(response) => {
                if response.request != self.request || !self.is_open() {
                    return false;
                }
                let selected = self.response.paths.get(self.selected).cloned();
                if response.pending && response.paths.is_empty() {
                    self.response.pending = true;
                    self.response.status = response.status;
                    return false;
                }
                self.selected = selected
                    .and_then(|path| response.paths.iter().position(|p| p == &path))
                    .unwrap_or(0);
                self.response = response;
                // An exact accepted token does not reopen just because the cursor returned.
                if let Some((line, token)) = &self.token
                    && let Some(value) = editor.line(*line)
                    && self
                        .response
                        .paths
                        .iter()
                        .any(|p| p == &value.text[token.range.clone()])
                {
                    self.dismiss();
                }
            }
            Message::Move(down) if self.is_open() && !self.response.paths.is_empty() => {
                let n = self.response.paths.len();
                self.selected = if down {
                    (self.selected + 1) % n
                } else {
                    (self.selected + n - 1) % n
                };
            }
            Message::Accept {
                request,
                generation,
                path,
            } if self.is_open() => {
                // Never accept retained rows from an obsolete query.
                if self.response.pending
                    || request != self.request
                    || generation != self.response.generation
                    || self.response.request != self.request
                {
                    return false;
                }
                if let Some(path) = path.filter(|path| self.response.paths.contains(path))
                    && let Some((line, token)) = &self.token
                {
                    let mut cursor = editor.cursor();
                    cursor.position.line = *line;
                    cursor.position.column = token.range.end;
                    let mut start = cursor.position;
                    start.column = token.range.start;
                    cursor.selection = Some(start);
                    editor.move_to(cursor);
                    editor.perform(text_editor::Action::Edit(text_editor::Edit::Paste(
                        Arc::new(path),
                    )));
                    self.edited(editor);
                    self.dismiss();
                    return true;
                }
            }
            Message::Refresh => {
                if let Some(service) = &self.service {
                    service.refresh();
                }
                self.query();
            }
            Message::Ignored => {
                self.ignored = !self.ignored;
                self.query();
            }
            _ => {}
        }
        false
    }
    pub fn accept_selected(&self) -> Message {
        Message::Accept {
            request: self.request,
            generation: self.response.generation,
            path: self.response.paths.get(self.selected).cloned(),
        }
    }
    pub fn binding(&self, event: &text_editor::KeyPress) -> Option<Message> {
        if !self.is_open()
            || !matches!(event.status, text_editor::Status::Focused { .. })
            || event.modifiers.command()
            || event.modifiers.alt()
            || event.modifiers.shift()
        {
            return None;
        }
        match event.key {
            keyboard::Key::Named(keyboard::key::Named::ArrowDown) => Some(Message::Move(true)),
            keyboard::Key::Named(keyboard::key::Named::ArrowUp) => Some(Message::Move(false)),
            keyboard::Key::Named(keyboard::key::Named::Enter | keyboard::key::Named::Tab) => {
                Some(self.accept_selected())
            }
            _ => None,
        }
    }
    pub fn subscription(&self) -> Subscription<Message> {
        self.service
            .as_ref()
            .map_or_else(Subscription::none, |service| {
                Subscription::run_with(Events(service.clone()), events)
            })
    }
    pub fn view(&self) -> Element<'_, Message> {
        let mut rows = column![].spacing(2);
        let start = self.selected.saturating_sub(6);
        for (index, path) in self.response.paths.iter().enumerate().skip(start).take(8) {
            let label = crate::ellipsized_text::ellipsized_text(
                path,
                12.,
                Some(if index == self.selected {
                    theme::ACCENT
                } else {
                    theme::MUTED
                }),
            );
            rows = rows.push(
                tooltip(
                    theme::button(container(label).width(Fill).padding([3, 6]))
                        .style(theme::text_button)
                        .on_press(Message::Accept {
                            request: self.request,
                            generation: self.response.generation,
                            path: Some(path.clone()),
                        }),
                    text(path).size(12),
                    tooltip::Position::Top,
                )
                .style(theme::card),
            );
        }
        let status = if self.response.pending {
            "Searching workspace files…"
        } else if !self.response.status.is_empty() {
            &self.response.status
        } else if self.response.paths.is_empty() {
            "No matching files"
        } else {
            "↑↓ select · Tab/Enter attach · Esc dismiss"
        };
        let mut actions = row![
            text(status).size(11).color(theme::MUTED),
            theme::button(text("Refresh").size(11))
                .on_press(Message::Refresh)
                .style(theme::text_button)
        ]
        .spacing(8);
        if self
            .token
            .as_ref()
            .is_some_and(|(_, token)| token.query.contains(&b'/'))
        {
            actions = actions.push(
                theme::button(
                    text(if self.ignored {
                        "Hide ignored"
                    } else {
                        "Include ignored directory"
                    })
                    .size(11),
                )
                .on_press(Message::Ignored)
                .style(theme::text_button),
            );
        }
        container(rows.push(actions))
            .padding(8)
            .width(Fill)
            .style(theme::card)
            .into()
    }
}
#[derive(Clone)]
struct Events(Arc<Service>);
impl PartialEq for Events {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}
impl Eq for Events {}
impl Hash for Events {
    fn hash<H: Hasher>(&self, state: &mut H) {
        Arc::as_ptr(&self.0).hash(state);
    }
}
fn events(events: &Events) -> impl iced::futures::Stream<Item = Message> + use<> {
    let mut receiver = events.0.subscribe();
    iced::stream::channel(1, async move |mut output| {
        loop {
            let response = receiver.borrow_and_update().clone();
            if output.send(Message::Results(response)).await.is_err() {
                break;
            }
            if receiver.changed().await.is_err() {
                break;
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn completion_replaces_whole_quoted_token_and_rejects_stale_results() {
        let root = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let mut c = Completion::default();
        c.workspace(Some(("s".into(), root.path().into())), cache.path().into())
            .unwrap();
        let mut editor = text_editor::Content::with_text("before @\"some file\" after");
        let mut cursor = editor.cursor();
        cursor.position.column = 13;
        editor.move_to(cursor);
        c.edited(&editor);
        assert!(c.is_open());
        c.update(
            Message::Results(Response {
                request: c.request - 1,
                paths: vec!["@wrong".into()],
                ..Response::default()
            }),
            &mut editor,
        );
        assert!(c.response.paths.is_empty());
        c.update(
            Message::Results(Response {
                request: c.request,
                paths: vec!["@\"some replacement\"".into()],
                ..Response::default()
            }),
            &mut editor,
        );
        let stale = c.accept_selected();
        c.update(
            Message::Results(Response {
                request: c.request,
                generation: c.response.generation + 1,
                paths: vec!["@wrong".into(), "@\"some replacement\"".into()],
                ..Response::default()
            }),
            &mut editor,
        );
        assert!(!c.update(stale, &mut editor));
        assert_eq!(editor.text(), "before @\"some file\" after");
        assert!(c.update(c.accept_selected(), &mut editor));
        assert_eq!(editor.text(), "before @\"some replacement\" after");
        assert!(!c.is_open());
        c.edited(&editor);
        assert!(!c.is_open());
        c.workspace(
            Some(("other".into(), root.path().into())),
            cache.path().into(),
        )
        .unwrap();
        assert!(!c.is_open());
    }
}

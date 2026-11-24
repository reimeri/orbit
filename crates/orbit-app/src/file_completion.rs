//! Composer-local completion state; the shared index never runs on the UI worker.
use crate::{
    ellipsized_text::{ellipsized_text, shrinking_ellipsized_text},
    icons::{self, Icon},
    theme,
};
use iced::futures::SinkExt;
use iced::{
    Element, Fill, Subscription, keyboard,
    widget::{column, container, row, text, text_editor},
};
use orbit_core::{
    file_index::{Response, SearchPhase, Service},
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
    Skills(u64, Result<orbit_core::skills::Catalog, String>),
    Move(bool),
    Dismiss,
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
    token: Option<(usize, Trigger, Token)>,
    dismissed: Option<(usize, Trigger, Token)>,
    in_code: bool,
    awaiting_path_check: bool,
    skill_epoch: u64,
    catalog: Option<Result<orbit_core::skills::Catalog, String>>,
    skill_rows: Vec<usize>,
    request: u64,
    response: Response,
    selected: usize,
    ignored: bool,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Trigger {
    File,
    Skill,
    Command,
}

pub const COMPACT: &str = "/compact";

fn active(text: &str, cursor: usize) -> Option<(Trigger, Token)> {
    if let Some(token) = file_tags::active(text, cursor) {
        return Some((Trigger::File, token));
    }
    let start = text[..cursor].rfind('$')?;
    if start > 0 && !text[..start].ends_with(char::is_whitespace) {
        return None;
    }
    let is_name = |c: char| c.is_ascii_alphanumeric() || c == '-';
    let end = text[start + 1..]
        .find(|c| !is_name(c))
        .map_or(text.len(), |n| n + start + 1);
    (cursor <= end).then(|| {
        (
            Trigger::Skill,
            Token {
                range: start..end,
                full_path: None,
                query: text.as_bytes()[start + 1..cursor].to_vec(),
            },
        )
    })
}
impl Completion {
    pub fn workspace(
        &mut self,
        session: Option<(String, PathBuf)>,
        cache: PathBuf,
    ) -> Result<(), orbit_core::Error> {
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
        let next = if cursor.selection.is_none() {
            editor.line(cursor.position.line).and_then(|line| {
                let text = line.text.as_ref();
                if text.starts_with('/')
                    && text != COMPACT
                    && COMPACT.starts_with(text)
                    && cursor.position.column == text.len()
                    && editor.line_count() == 1
                {
                    return Some((
                        0,
                        Trigger::Command,
                        Token {
                            range: 0..text.len(),
                            full_path: None,
                            query: Vec::new(),
                        },
                    ));
                }
                self.service.as_ref()?;
                active(text, cursor.position.column)
                    .map(|(kind, token)| (cursor.position.line, kind, token))
            })
        } else {
            None
        };
        if self.token == next {
            return;
        }
        let was_open = self.is_open();
        if self.dismissed != next {
            self.dismissed = None;
        }
        let same_token = self.token.as_ref().zip(next.as_ref()).is_some_and(
            |((line, kind, old), (new_line, new_kind, new))| {
                kind == new_kind && line == new_line && old.range.start == new.range.start
            },
        );
        // Re-entering a complete filename must not expose the panel (or its
        // key bindings) while the background index checks the whole tag.
        self.awaiting_path_check = next
            .as_ref()
            .is_some_and(|(_, _, token)| token.full_path.is_some())
            && !(same_token && was_open);
        if !same_token {
            // Only parse Markdown when entering a new skill token, not on each
            // keystroke. A link embedded in a code example is not a selection.
            self.in_code = next.as_ref().is_some_and(|(line, kind, token)| {
                if *kind != Trigger::Skill {
                    return false;
                }
                let prefix: usize = editor
                    .lines()
                    .take(*line)
                    .map(|line| line.text.len() + line.ending.as_str().len().max(1))
                    .sum();
                let pos = prefix + token.range.start;
                pulldown_cmark::Parser::new(&editor.text())
                    .into_offset_iter()
                    .any(|(e, r)| {
                        r.contains(&pos)
                            && matches!(
                                e,
                                pulldown_cmark::Event::Code(_)
                                    | pulldown_cmark::Event::Start(pulldown_cmark::Tag::CodeBlock(
                                        _
                                    ))
                            )
                    })
            });
            self.skill_epoch += 1;
            self.catalog = None;
            self.skill_rows.clear();
            self.response = Response::default();
            self.selected = 0;
            self.ignored = false;
        }
        self.token = next;
        if self.active_query() {
            self.query();
        } else if let Some(service) = &self.service {
            service.cancel_query();
            self.request += 1;
        }
    }
    fn picking_command(&self) -> bool {
        matches!(self.token, Some((_, Trigger::Command, _)))
    }
    fn picking_skill(&self) -> bool {
        matches!(self.token, Some((_, Trigger::Skill, _)))
    }
    fn query(&mut self) {
        if self.picking_command() {
            self.request += 1;
            self.response = Response {
                request: self.request,
                phase: SearchPhase::Complete,
                paths: vec![COMPACT.to_owned()],
                ..Default::default()
            };
            return;
        }
        if let (Some(service), Some((_, kind, token))) = (&self.service, &self.token) {
            self.request += 1;
            self.response.phase = SearchPhase::Pending;
            if *kind == Trigger::Skill {
                self.response = Response {
                    request: self.request,
                    phase: SearchPhase::Pending,
                    ..Default::default()
                };
                if let Some(result) = &self.catalog {
                    self.response.phase = SearchPhase::Complete;
                    match result {
                        Ok(catalog) => {
                            let query = String::from_utf8_lossy(&token.query).to_ascii_lowercase();
                            self.skill_rows = catalog
                                .skills
                                .iter()
                                .enumerate()
                                .filter(|(_, skill)| skill.name.contains(&query))
                                .map(|(i, _)| i)
                                .collect();
                            self.response.paths = self
                                .skill_rows
                                .iter()
                                .map(|&i| catalog.skills[i].mention())
                                .collect();
                            self.response.status = catalog.warnings.join("\n");
                        }
                        Err(error) => self.response.status.clone_from(error),
                    }
                }
                self.selected = 0;
            } else {
                service.query(
                    self.request,
                    token.query.clone(),
                    token.full_path.clone(),
                    self.ignored,
                );
            }
        }
    }
    fn active_query(&self) -> bool {
        !self.in_code && self.token.is_some() && self.token != self.dismissed
    }
    pub fn is_open(&self) -> bool {
        self.active_query() && !self.awaiting_path_check
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
            Message::Dismiss => self.dismiss(),
            Message::Skills(epoch, result) => {
                if self.is_open() && self.picking_skill() && epoch == self.skill_epoch {
                    self.catalog = Some(result);
                    self.query();
                }
            }
            Message::Results(response) => {
                if self.picking_command()
                    || self.picking_skill()
                    || response.request != self.request
                    || !self.active_query()
                {
                    return false;
                }
                if response.exact_path {
                    self.dismiss();
                    return false;
                }
                let selected = self.response.paths.get(self.selected).cloned();
                if response.phase == SearchPhase::Pending {
                    self.response.phase = SearchPhase::Pending;
                    self.response.status = response.status;
                    return false;
                }
                self.selected = selected
                    .and_then(|path| response.paths.iter().position(|p| p == &path))
                    .unwrap_or(0);
                self.response = response;
                self.awaiting_path_check = false;
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
                if self.response.phase == SearchPhase::Pending
                    || request != self.request
                    || generation != self.response.generation
                    || self.response.request != self.request
                {
                    return false;
                }
                if let Some(path) = path.filter(|path| self.response.paths.contains(path))
                    && let Some((line, _, token)) = &self.token
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
                if self.picking_skill() {
                    self.skill_epoch += 1;
                    self.catalog = None;
                } else if let Some(service) = &self.service {
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
        if self.picking_command() {
            return (event.key == keyboard::Key::Named(keyboard::key::Named::Tab))
                .then(|| self.accept_selected());
        }
        if self.picking_skill() && self.response.paths.is_empty() {
            return None;
        }
        match event.key {
            keyboard::Key::Named(keyboard::key::Named::ArrowDown) => Some(Message::Move(true)),
            keyboard::Key::Named(keyboard::key::Named::ArrowUp) => Some(Message::Move(false)),
            // With no match to accept, Enter belongs to the composer (send).
            keyboard::Key::Named(keyboard::key::Named::Enter)
                if self.response.phase != SearchPhase::Pending
                    && self.response.paths.is_empty() =>
            {
                None
            }
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
                let files = Subscription::run_with(Events(service.clone()), events);
                if self.is_open() && self.picking_skill() && self.catalog.is_none() {
                    Subscription::batch([
                        files,
                        Subscription::run_with(
                            (self.skill_epoch, service.root.clone()),
                            skill_events,
                        ),
                    ])
                } else {
                    files
                }
            })
    }
    pub fn view(&self, colors: &theme::Colors) -> Element<'_, Message> {
        const VISIBLE: usize = 8;
        const INSET: f32 = 6.;
        const ROW_PADDING: [f32; 2] = [3., crate::growing_editor::PADDING - INSET];
        const LABEL_SIZE: f32 = 12.;
        let mut rows = column![].width(Fill);
        // Keep one result below the selection when possible, but never scroll
        // past the last full window of results.
        let start = self
            .selected
            .saturating_sub(VISIBLE - 2)
            .min(self.response.paths.len().saturating_sub(VISIBLE));
        for (index, path) in self
            .response
            .paths
            .iter()
            .enumerate()
            .skip(start)
            .take(VISIBLE)
        {
            let selected = index == self.selected;
            let label: Element<'_, Message> = if self.picking_command() {
                row![
                    text(COMPACT).size(LABEL_SIZE),
                    ellipsized_text(
                        "Compact the model’s context; keep the full chat.",
                        LABEL_SIZE - 1.,
                        Some(colors.muted)
                    )
                ]
                .align_y(iced::Center)
                .spacing(8)
                .into()
            } else if self.picking_skill() {
                let skill = &self
                    .catalog
                    .as_ref()
                    .expect("loaded skills")
                    .as_ref()
                    .expect("skill rows")
                    .skills[self.skill_rows[index]];
                let (icon, scope) = if skill.personal {
                    (Icon::User, "Personal skill")
                } else {
                    (Icon::FolderOpen, "Repo skill")
                };
                row![
                    icons::tip(icon.view(14.), scope),
                    shrinking_ellipsized_text(&skill.name, LABEL_SIZE, None),
                    ellipsized_text(&skill.description, LABEL_SIZE - 1., Some(colors.muted)),
                ]
                .align_y(iced::Center)
                .spacing(8)
                .into()
            } else {
                ellipsized_text(path, LABEL_SIZE, None)
            };
            rows = rows.push(
                theme::button(label)
                    .padding(ROW_PADDING)
                    .style(move |theme, status| theme::completion_item(theme, status, selected))
                    .on_press(Message::Accept {
                        request: self.request,
                        generation: self.response.generation,
                        path: Some(path.clone()),
                    }),
            );
        }
        let status = if !self.response.status.is_empty() {
            Some(self.response.status.as_str())
        } else if self.response.phase != SearchPhase::Pending && self.response.paths.is_empty() {
            Some(if self.picking_skill() {
                "No matching skills"
            } else {
                "No matching files"
            })
        } else {
            None
        };
        if let Some(status) = status {
            rows = rows.push(
                container(text(status).size(LABEL_SIZE).style(theme::muted)).padding(ROW_PADDING),
            );
        }
        if !self.picking_skill()
            && self
                .token
                .as_ref()
                .is_some_and(|(_, _, token)| token.query.contains(&b'/'))
        {
            rows = rows.push(
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
        let mut panel = row![rows].spacing(4);
        if !self.picking_command() {
            let refresh = icons::tip(
                theme::button(
                    Icon::RefreshCw.spinning(12., self.response.phase != SearchPhase::Complete),
                )
                .padding(4)
                .style(theme::sidebar_control)
                .on_press(Message::Refresh),
                if self.picking_skill() {
                    "Refresh skills"
                } else {
                    "Refresh workspace files"
                },
            );
            panel = panel.push(refresh);
        }
        container(panel)
            .padding(INSET)
            .width(Fill)
            .style(theme::card)
            .into()
    }
}
fn skill_events(
    (epoch, path): &(u64, PathBuf),
) -> impl iced::futures::Stream<Item = Message> + use<> {
    let (epoch, path) = (*epoch, path.clone());
    iced::futures::stream::once(async move {
        let result = tokio::task::spawn_blocking(move || orbit_core::skills::discover(&path))
            .await
            .expect("skill discovery worker")
            .map_err(|e| format!("Could not discover skills: {e:#}"));
        Message::Skills(epoch, result)
    })
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
    fn skill_picker_filters_and_inserts_a_path_qualified_selection() {
        let root = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let dir = root.path().join(".agents/skills/work flow");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("SKILL.md"),
            "---\nname: fixture-lookup\ndescription: Look it up\n---\nSteps",
        )
        .unwrap();
        let mut picker = Completion::default();
        picker
            .workspace(Some(("s".into(), root.path().into())), cache.path().into())
            .unwrap();
        let mut editor = text_editor::Content::with_text("λ $fixture-look later");
        let mut cursor = editor.cursor();
        cursor.position.column = "λ $fixture".len();
        editor.move_to(cursor);
        picker.edited(&editor);
        assert!(picker.is_open());
        let catalog = orbit_core::skills::discover(root.path()).unwrap();
        picker.update(
            Message::Skills(picker.skill_epoch, Ok(catalog)),
            &mut editor,
        );
        assert!(picker.update(picker.accept_selected(), &mut editor));
        let authored = editor.text();
        assert!(authored.starts_with("λ [$fixture-lookup](<skill:///"));
        assert!(authored.ends_with("work%20flow/SKILL.md>) later"));
        assert!(!picker.is_open());
        let mut example = text_editor::Content::with_text("```sh\n$fixture\n```");
        let mut cursor = example.cursor();
        cursor.position.line = 1;
        cursor.position.column = 1;
        example.move_to(cursor);
        picker.edited(&example);
        assert!(!picker.is_open());
    }
    #[test]
    fn loading_does_not_add_a_row_to_retained_matches() {
        use iced::{
            Size,
            advanced::{layout::Limits, widget::Tree},
        };

        let renderer = crate::test_renderer::new(16.);
        let mut completion = Completion::default();
        completion.response.paths = vec!["@src/lib.rs".into(), "@src/main.rs".into()];
        let height = |completion: &Completion| {
            let mut panel = completion.view(theme::default_colors());
            let mut tree = Tree::new(panel.as_widget());
            panel
                .as_widget_mut()
                .layout(
                    &mut tree,
                    &renderer,
                    &Limits::new(Size::ZERO, Size::new(400., 600.)),
                )
                .size()
                .height
        };
        let ready_height = height(&completion);
        for phase in [SearchPhase::Pending, SearchPhase::Partial] {
            completion.response.phase = phase;
            assert_eq!(height(&completion), ready_height);
        }
    }

    #[tokio::test]
    async fn reentering_an_exact_file_tag_stays_closed_until_the_path_changes() {
        let root = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("README.md"), "").unwrap();
        // Clicking immediately after @ produces an empty prefix; the exact
        // file need not fit in that prefix's limited suggestion rows.
        for n in 0..orbit_core::file_index::RESULTS {
            std::fs::write(root.path().join(format!("{n}.txt")), "").unwrap();
        }
        let mut picker = Completion::default();
        picker
            .workspace(Some(("s".into(), root.path().into())), cache.path().into())
            .unwrap();
        let mut responses = picker.service.as_ref().unwrap().subscribe();
        let mut settle = async |picker: &mut Completion, editor: &mut text_editor::Content| {
            let response = tokio::time::timeout(
                std::time::Duration::from_secs(10),
                responses
                    .wait_for(|r| r.request == picker.request && r.phase != SearchPhase::Pending),
            )
            .await
            .unwrap()
            .unwrap()
            .clone();
            picker.update(Message::Results(response), editor);
        };

        let mut editor = text_editor::Content::with_text("see @README.md ");
        let mut cursor = editor.cursor();
        cursor.position.column = editor.text().len();
        editor.move_to(cursor);
        editor.perform(text_editor::Action::Edit(text_editor::Edit::Backspace));
        picker.edited(&editor);
        assert!(!picker.is_open());
        settle(&mut picker, &mut editor).await;
        assert!(!picker.is_open());

        cursor.position.column = "see @".len();
        editor.move_to(cursor);
        picker.edited(&editor);
        assert!(!picker.is_open());
        settle(&mut picker, &mut editor).await;
        assert!(!picker.is_open());

        cursor.position.column = editor.text().len();
        editor.move_to(cursor);
        editor.perform(text_editor::Action::Edit(text_editor::Edit::Backspace));
        assert_eq!(editor.text(), "see @README.m");
        picker.edited(&editor);
        settle(&mut picker, &mut editor).await;
        assert!(picker.is_open());
        assert!(picker.update(picker.accept_selected(), &mut editor));
        assert_eq!(editor.text(), "see @README.md");
    }

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
        assert!(!c.is_open());
        c.update(
            Message::Results(Response {
                request: c.request - 1,
                paths: vec!["@wrong".into()],
                ..Response::default()
            }),
            &mut editor,
        );
        assert!(c.response.paths.is_empty());
        let enter = text_editor::KeyPress {
            key: keyboard::Key::Named(keyboard::key::Named::Enter),
            modified_key: keyboard::Key::Named(keyboard::key::Named::Enter),
            physical_key: keyboard::key::Physical::Code(keyboard::key::Code::Enter),
            modifiers: keyboard::Modifiers::empty(),
            text: None,
            status: text_editor::Status::Focused { is_hovered: false },
        };
        c.update(
            Message::Results(Response {
                request: c.request,
                ..Response::default()
            }),
            &mut editor,
        );
        assert!(c.is_open());
        assert!(c.binding(&enter).is_none());
        c.update(
            Message::Results(Response {
                request: c.request,
                paths: vec!["@\"some replacement\"".into()],
                ..Response::default()
            }),
            &mut editor,
        );
        assert!(matches!(c.binding(&enter), Some(Message::Accept { .. })));
        let stale = c.accept_selected();
        c.update(
            Message::Results(Response {
                request: c.request,
                generation: c.response.generation + 1,
                phase: SearchPhase::Partial,
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

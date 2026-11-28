use super::*;
use crate::worktrees::{Checkout, Repo};
use iced::widget::column;

#[derive(Default)]
pub(super) struct State {
    pub popup: Option<f32>, // horizontal offset from the chat lane's left edge
    pub slow_popup_load: bool,
    pub bases: bool,
    row: Option<PathBuf>,
    pub(super) confirm: Option<PathBuf>,
    pub retry: Option<(PathBuf, PathBuf)>, // originating project and prepared checkout
    pub repo: Option<Result<Arc<Repo>, String>>,
    generation: u64,
    cursor: usize,
}

#[derive(Clone, Debug)]
pub enum Action {
    Panel,
    Popup(f32),
    ClosePopup,
    RetryLoad,
    SlowPopupLoad(u64),
    Loaded(u64, String, Result<Arc<Repo>, String>),
    Bases,
    Row(PathBuf),
    Copy(PathBuf),
    AskRemove(PathBuf),
    CancelRemove,
    Run(Operation),
    Done(String, Result<Outcome, String>),
}

#[derive(Clone, Debug)]
pub enum Outcome {
    Opened(PathBuf),
    Created { path: PathBuf, origin: PathBuf },
    Removed(PathBuf),
}

#[derive(Clone, Debug)]
pub enum Operation {
    Open(PathBuf),
    Create(OsString),
    Remove(PathBuf),
}

fn msg(action: Action) -> Message {
    Message::Worktree(action)
}

impl App {
    pub(super) fn load_worktrees(&mut self) -> Task<Message> {
        self.worktrees.generation += 1;
        self.worktrees.repo = None;
        self.worktrees.slow_popup_load = false;
        self.worktrees.row = None;
        self.worktrees.confirm = None;
        self.worktrees.cursor = 0;
        let generation = self.worktrees.generation;
        let Some((s, h)) = self.current().zip(self.harness.as_ref()) else {
            return Task::none();
        };
        let (id, project, root) = (s.id.clone(), s.project.clone(), h.store.root().to_owned());
        let load = Task::future(async move {
            msg(Action::Loaded(
                generation,
                id,
                Repo::read(&project, &root)
                    .await
                    .map(Arc::new)
                    .map_err(|e| format!("{e:#}")),
            ))
        });
        if self.worktrees.popup.is_some() {
            let timer = Task::future(async move {
                tokio::time::sleep(Duration::from_millis(250)).await;
                msg(Action::SlowPopupLoad(generation))
            });
            Task::batch([load, timer])
        } else {
            load
        }
    }

    pub(super) fn worktree_action(&mut self, action: Action) -> Task<Message> {
        use Action::*;
        match action {
            SlowPopupLoad(generation) => {
                if generation == self.worktrees.generation
                    && self.worktrees.popup.is_some()
                    && self.worktrees.repo.is_none()
                {
                    self.worktrees.slow_popup_load = true;
                }
            }
            Loaded(generation, id, result) => {
                if generation == self.worktrees.generation
                    && self.current().is_some_and(|s| s.id == id)
                {
                    self.worktrees.repo = Some(result);
                    self.worktrees.slow_popup_load = false;
                }
            }
            Done(id, result) => {
                self.io_busy = false;
                let checkout = match result {
                    Ok(Outcome::Created { path, origin }) => {
                        // The checkout exists even if starting its draft fails.
                        self.catalog.worktree_origins.insert(path.clone(), origin);
                        self.save_catalog();
                        Some(path)
                    }
                    Ok(Outcome::Opened(path)) => Some(path),
                    Ok(Outcome::Removed(path)) => {
                        // Keep identity for historical sessions in a removed checkout.
                        if !self.sessions.iter().any(|s| s.project == path)
                            && self.catalog.worktree_origins.remove(&path).is_some()
                        {
                            self.save_catalog();
                        }
                        None
                    }
                    Err(e) => {
                        self.error = Some(e);
                        None
                    }
                };
                if self.current().is_none_or(|s| s.id != id) || self.close_requested || self.closing
                {
                    return Task::none();
                }
                if let Some(path) = checkout {
                    self.worktrees.retry = Some((
                        self.current().expect("originating session").project.clone(),
                        path.clone(),
                    ));
                    self.select(Selection::Draft(path));
                } else {
                    return self.load_worktrees();
                }
            }
            _ if self.io_busy || self.close_requested || self.closing => {}
            Panel => {
                self.worktrees.bases = false;
                self.toggle_right_panel(RightPanel::Worktrees);
                if self.right_panel == Some(RightPanel::Worktrees) {
                    return self.load_worktrees();
                }
            }
            Popup(offset) if self.draft_session.is_some() => {
                self.footer_menu_open = false;
                self.file_completion.dismiss();
                self.worktrees.popup = Some(offset);
                return self.load_worktrees();
            }
            Popup(_) => {}
            ClosePopup => self.worktrees.popup = None,
            RetryLoad => return self.load_worktrees(),
            Bases => {
                self.worktrees.bases = !self.worktrees.bases;
                self.worktrees.confirm = None;
                self.worktrees.row = None;
            }
            Row(path) => {
                self.worktrees.confirm = None;
                self.worktrees.row = (self.worktrees.row.as_ref() != Some(&path)).then_some(path);
            }
            Copy(path) => {
                self.worktrees.row = None;
                return iced::clipboard::write(path.display().to_string());
            }
            AskRemove(path) => self.worktrees.confirm = Some(path),
            CancelRemove => self.worktrees.confirm = None,
            Run(op) => {
                let Some((s, h)) = self.current().zip(self.harness.as_ref()) else {
                    return Task::none();
                };
                let Some(Ok(repo)) = self.worktrees.repo.as_ref() else {
                    return Task::none();
                };
                if self.composer_importing()
                    || (self.right_panel != Some(RightPanel::Worktrees)
                        && self.worktrees.popup.is_none())
                {
                    return Task::none();
                }
                let id = s.id.clone();
                let origin = self.catalog.project_origin(&s.project).to_owned();
                let h = h.clone();
                let repo = Arc::clone(repo);
                self.io_busy = true;
                return Task::future(async move {
                    let result = match op {
                        Operation::Open(path) => repo.open(&path).await.map(Outcome::Opened),
                        Operation::Create(base) => repo
                            .create(&base)
                            .await
                            .map(|path| Outcome::Created { path, origin }),
                        Operation::Remove(path) => {
                            if checkout_busy(&h, &path) {
                                Err(anyhow::anyhow!(
                                    "A running session is using {}.",
                                    path.display()
                                ))
                            } else {
                                repo.remove(&path, h.store.root())
                                    .await
                                    .map(|()| Outcome::Removed(path))
                            }
                        }
                    };
                    msg(Done(id, result.map_err(|e| format!("{e:#}"))))
                });
            }
        }
        Task::none()
    }

    pub(super) fn worktree_key(&mut self, key: keyboard::Key) -> Task<Message> {
        use keyboard::key::Named;
        if key == keyboard::Key::Named(Named::Escape) {
            self.worktrees.popup = None;
            return Task::none();
        }
        let Some(Ok(repo)) = &self.worktrees.repo else {
            return Task::none();
        };
        let n = repo.trees.len();
        let cursor = &mut self.worktrees.cursor;
        match key {
            keyboard::Key::Named(Named::ArrowDown) => *cursor = (*cursor + 1).min(n),
            keyboard::Key::Named(Named::ArrowUp) => *cursor = cursor.saturating_sub(1),
            keyboard::Key::Named(Named::Enter) => {
                let op = match repo.trees.get(*cursor) {
                    Some(t) => t.available.then(|| Operation::Open(t.path.clone())),
                    None => repo.base.clone().map(Operation::Create),
                };
                if let Some(op) = op {
                    return self.worktree_action(Action::Run(op));
                }
            }
            _ => return Task::none(),
        }
        widget::operation::scroll_to(
            "worktree-options",
            scrollable::AbsoluteOffset {
                x: 0.,
                y: (cursor.saturating_sub(2) as f32)
                    * (FOOTER_MENU_ROW_HEIGHT + FOOTER_MENU_ROW_GAP),
            },
        )
    }

    fn worktree_operations_enabled(&self) -> bool {
        !self.io_busy && !self.composer_importing()
    }

    pub(super) fn worktree_popup(&self, width: f32) -> Option<Element<'_, Message>> {
        let mut list = column![];
        match self.worktrees.repo.as_ref()? {
            Err(error) => {
                list = list.push(text(error).size(13)).push(
                    theme::button(text("Retry"))
                        .style(theme::text_button)
                        .on_press(msg(Action::RetryLoad)),
                )
            }
            Ok(repo) => {
                let enabled = self.worktree_operations_enabled();
                let option = |(i, tree): (usize, &Checkout)| {
                    self.footer_menu_choice(
                        &tree.label,
                        tree.path == repo.current,
                        i == self.worktrees.cursor,
                        (tree.available && enabled)
                            .then(|| msg(Action::Run(Operation::Open(tree.path.clone())))),
                    )
                };
                let pin = usize::from(
                    repo.base.is_some()
                        && repo.trees.first().is_some_and(|t| t.branch == repo.base),
                );
                let mut choices = column![].spacing(FOOTER_MENU_ROW_GAP);
                if pin != 0 {
                    choices = choices.push(
                        container(option((0, &repo.trees[0])))
                            .padding(iced::Padding::ZERO.right(crate::scrollbar::GUTTER)),
                    );
                }
                let rows = repo.trees.iter().enumerate().skip(pin).map(option);
                let visible = (repo.trees.len() - pin).min(5 - pin);
                if visible != 0 {
                    choices = choices.push(
                        scroll(column(rows).spacing(FOOTER_MENU_ROW_GAP))
                            .id("worktree-options")
                            .height(
                                FOOTER_MENU_ROW_HEIGHT * visible as f32
                                    + FOOTER_MENU_ROW_GAP * (visible - 1) as f32,
                            ),
                    );
                }
                let create = theme::button(Icon::Plus.label("New worktree", FOOTER_MENU_TEXT_SIZE))
                    .width(Fill)
                    .height(FOOTER_MENU_ROW_HEIGHT)
                    .padding([6, 8])
                    .on_press_maybe(
                        repo.base
                            .clone()
                            .filter(|_| enabled)
                            .map(|b| msg(Action::Run(Operation::Create(b)))),
                    )
                    .style(move |t, s| {
                        theme::menu_button(t, s, self.worktrees.cursor == repo.trees.len())
                    });
                list = list
                    .spacing(8)
                    .push(choices)
                    .push(widget::rule::horizontal(1))
                    .push(if repo.base.is_some() {
                        Element::from(create)
                    } else {
                        footer_tip(
                            create,
                            "No committed main or master. Choose a base in the Worktrees panel."
                                .into(),
                        )
                    });
            }
        }
        Some(opaque(container(list).width(width).padding(6).style(card)))
    }

    pub(super) fn worktrees_panel(&self) -> Element<'_, Message> {
        let mut content = column![right_panel_header("Worktrees")].spacing(12);
        let Some(s) = self.current() else {
            return self.panel_container(
                content.push(text("Select a project to see its worktrees.").size(13)),
            );
        };
        let repo = match &self.worktrees.repo {
            None => return self.panel_container(content.push(text("Loading worktrees…").size(13))),
            Some(Err(error)) => {
                return self.panel_container(
                    content.push(text(error).size(13)).push(
                        theme::button(text("Retry"))
                            .style(theme::text_button)
                            .on_press(msg(Action::RetryLoad)),
                    ),
                );
            }
            Some(Ok(repo)) => repo,
        };
        let enabled = self.worktree_operations_enabled();
        let mut rows = column![].spacing(8);
        if self.worktrees.bases {
            rows = rows.push(text("Base branch").size(14));
            if repo.branches.is_empty() {
                rows = rows.push(text("Make an initial commit to create a worktree.").size(13));
            }
            for branch in &repo.branches {
                let preferred = Some(branch) == repo.base.as_ref();
                rows = rows.push(
                    theme::button(ellipsized_text(&branch.to_string_lossy(), 13., None))
                        .width(Fill)
                        .on_press_maybe(
                            enabled.then(|| msg(Action::Run(Operation::Create(branch.clone())))),
                        )
                        .style(move |t, s| theme::option_button(t, s, preferred)),
                );
            }
        } else {
            rows = rows.push(
                text(format!("Checkouts   {}", repo.trees.len()))
                    .size(12)
                    .style(theme::muted),
            );
            for t in &repo.trees {
                let mut badges = Vec::new();
                if t.primary {
                    badges.push("Primary");
                }
                if t.managed {
                    badges.push("Managed");
                }
                if t.path == repo.current {
                    badges.push("This session");
                }
                if t.locked {
                    badges.push("Locked");
                }
                if !t.available {
                    badges.push("Missing");
                }
                let path_label = if t.managed {
                    // The on-disk namespace is a full digest; keep the actual
                    // checkout name visible in a narrow panel.
                    format!(
                        "…/worktrees/…/{}",
                        t.path
                            .file_name()
                            .expect("managed checkout")
                            .to_string_lossy()
                    )
                } else {
                    t.path.display().to_string()
                };
                let mut details = column![
                    ellipsized_text(&t.label, 14., None),
                    ellipsized_text(&path_label, 12., Some(self.preset().colors.muted))
                ]
                .spacing(4);
                if !badges.is_empty() {
                    details = details.push(text(badges.join(" · ")).size(11).style(theme::muted));
                }
                rows = rows.push(
                    row![
                        theme::button(details)
                            .padding([5, 0])
                            .width(Fill)
                            .style(theme::text_button)
                            .on_press_maybe(
                                (enabled && t.available)
                                    .then(|| msg(Action::Run(Operation::Open(t.path.clone()))))
                            ),
                        theme::button(text("⋯"))
                            .style(theme::text_button)
                            .on_press(msg(Action::Row(t.path.clone())))
                    ]
                    .align_y(iced::Center),
                );
                if self.worktrees.row.as_ref() == Some(&t.path) {
                    rows = rows.push(
                        theme::button(Icon::Plus.label("New session here", 13.))
                            .style(theme::text_button)
                            .on_press_maybe(
                                (enabled && t.available)
                                    .then(|| msg(Action::Run(Operation::Open(t.path.clone())))),
                            ),
                    );
                    rows = rows.push(
                        theme::button(Icon::Copy.label("Copy path", 13.))
                            .on_press(msg(Action::Copy(t.path.clone())))
                            .style(theme::text_button),
                    );
                    if enabled
                        && t.removable_from(&repo.current)
                        && self
                            .harness
                            .as_ref()
                            .is_some_and(|h| !checkout_busy(h, &t.path))
                    {
                        if self.worktrees.confirm.as_ref() == Some(&t.path) {
                            rows = rows
                                .push(
                                    text(format!(
                                        "Remove {}? Its branch, chat history and drafts stay. Chats using this folder cannot send until it is restored.",
                                        t.path.display()
                                    ))
                                    .size(13),
                                )
                                .push(
                                    row![
                                        theme::cancel("Cancel").on_press(msg(Action::CancelRemove)),
                                        theme::action("Remove").style(theme::danger).on_press(msg(
                                            Action::Run(Operation::Remove(t.path.clone()))
                                        ))
                                    ]
                                    .spacing(8),
                                );
                        } else {
                            rows = rows.push(
                                theme::button(Icon::Trash2.label("Remove worktree…", 13.))
                                    .on_press(msg(Action::AskRemove(t.path.clone())))
                                    .style(theme::danger_text),
                            );
                        }
                    }
                }
                rows = rows.push(widget::rule::horizontal(1));
            }
        }
        content = content.push(
            container(
                theme::button(Icon::Plus.label("New worktree", 14.))
                    .width(Fill)
                    .on_press_maybe(enabled.then_some(msg(Action::Bases))),
            )
            .padding(iced::Padding::ZERO.right(crate::scrollbar::GUTTER)),
        );
        if let Some((origin, path)) = &self.worktrees.retry
            && *origin == s.project
        {
            content = content.push(
                theme::button(text("Retry starting session")).on_press_maybe(
                    enabled.then(|| msg(Action::Run(Operation::Open(path.clone())))),
                ),
            );
        }
        self.panel_container(content.push(scroll(rows)))
    }
}

fn checkout_busy(h: &Harness, path: &Path) -> bool {
    h.all().iter().any(|c| {
        c.is_executing()
            && c.state
                .lock()
                .expect("session lock")
                .project
                .starts_with(path)
    })
}

use super::*;
use iced::widget::column;

impl App {
    pub(super) fn append_transcript(&mut self) {
        let Some(session) = &self.current_session else {
            return;
        };
        let Some(start) = self.activity.sync(&session.entries) else {
            return;
        };
        let position = self
            .transcript
            .order
            .partition_point(|&id| ActivityRow::from_id(id).source() < start);
        let rows = self.transcript_rows(start, session);
        let selection = self.chat_selection.borrow();
        let selected = selection
            .anchor
            .zip(selection.focus)
            .map(|(a, b)| a.entry.min(b.entry)..=a.entry.max(b.entry));
        drop(selection);
        if let Some(selected) = selected {
            let selected_entry = |id| match ActivityRow::from_id(id) {
                ActivityRow::Entry(i) if selected.contains(&i) => Some(i),
                _ => None,
            };
            if !self.transcript.order[position..]
                .iter()
                .copied()
                .filter_map(selected_entry)
                .eq(rows.iter().map(|(id, _)| *id).filter_map(selected_entry))
            {
                self.chat_selection.borrow_mut().clear();
            }
        }
        self.transcript.replace_tail(position, rows);
    }
    pub(super) fn rebuild_transcript(&mut self) {
        self.chat_selection.borrow_mut().clear();
        let Some(session) = &self.current_session else {
            return;
        };
        self.activity.sync(&session.entries);
        self.transcript.reset(self.transcript_rows(0, session));
    }
    fn transcript_rows(&self, start: usize, session: &Session) -> Vec<(usize, f32)> {
        let settings = &self.catalog.settings;
        let width = (self.center_dimensions().1 - 2. * self.chat_inset()).min(settings.chat_width);
        self.activity
            .rows(
                start,
                &self.details.groups,
                &self.details.execution,
                &self.details.expanded,
            )
            .into_iter()
            .map(|row| {
                let id = row.id();
                let height = self.transcript.position(id).map_or_else(
                    || row.estimate(&session.entries, settings.font_size, width),
                    |pos| self.transcript.height(pos),
                );
                (id, height)
            })
            .collect()
    }
    pub(super) fn jump_to_prompt(&mut self, entry: &str) -> Task<Message> {
        let Some(index) = self.current().and_then(|s| s.entries.index_of(entry)) else {
            return Task::none();
        };
        let Some(pos) = self.transcript.position(index) else {
            return Task::none();
        };
        // Near the tail the prompt may not reach the top of the pane. Mount
        // the rows for the reachable viewport: if already at the bottom, iced
        // won't report any scroll change to correct an overshooting row window.
        let max_scroll = (self.transcript.total() + 2. * CHAT_VERTICAL_PADDING
            - self.transcript.viewport)
            .max(0.);
        let scroll_y = (self.transcript.prefix(pos) + CHAT_VERTICAL_PADDING).min(max_scroll);
        if let Some(find) = &mut self.find {
            find.reveal = None;
        }
        self.hold_prompt_at_bottom = true;
        self.auto_scroll = false;
        self.transcript.offset = (scroll_y - CHAT_VERTICAL_PADDING).max(0.);
        self.prepare_visible();
        widget::operation::scroll_to("chat", scrollable::AbsoluteOffset { x: 0., y: scroll_y })
    }
    pub(super) fn measure_row(
        &mut self,
        index: usize,
        height: f32,
        inner: Option<f32>,
    ) -> Task<Message> {
        let spacing = self
            .current()
            .map_or(8., |s| ActivityRow::from_id(index).spacing(&s.entries));
        let visible = self.transcript.visible();
        // Decide which layer owns the anchor using the SAME pre-measurement
        // row bounds, including when a resizing row crosses the viewport edge.
        let Some(pos) = self.transcript.position(index) else {
            return Task::none();
        };
        let height = height + spacing;
        let size_changed = (height - self.transcript.height(pos)).abs() >= 0.5;
        let inside = if (self.transcript.prefix(pos)..self.transcript.prefix(pos + 1))
            .contains(&self.transcript.offset)
        {
            inner.unwrap_or(0.)
        } else {
            0.
        };
        let corrected = self.transcript.measure(index, height);
        if self.auto_scroll && size_changed {
            self.transcript.end();
            self.prepare_visible();
            return bottom();
        }
        // The same redraw can reveal a Find match and measure chunks above it.
        // Do not undo the reveal with a correction to the old reading position.
        let revealing = self.find.as_ref().is_some_and(find::Find::revealing_match);
        if !self.auto_scroll && !revealing && (corrected || inside != 0.) {
            self.transcript.offset = (self.transcript.offset + inside).max(0.);
            self.prepare_visible();
            return widget::operation::scroll_to(
                "chat",
                scrollable::AbsoluteOffset {
                    x: 0.,
                    y: self.transcript.offset + CHAT_VERTICAL_PADDING,
                },
            );
        }
        if inner.is_some() || self.transcript.visible() != visible {
            self.prepare_visible();
        }
        Task::none()
    }
    pub(super) fn prepare_visible(&mut self) {
        let mut visible_media = self.attachments.clone();
        let mut image_urls = HashMap::new();
        if self.current_session.is_none() {
            self.tool_previews.clear();
        }
        if let Some(session) = &self.current_session {
            let visible: Vec<_> = self
                .chat_range()
                .filter_map(
                    |pos| match ActivityRow::from_id(self.transcript.order[pos]) {
                        ActivityRow::Entry(i) => Some((pos, i)),
                        _ => None,
                    },
                )
                .collect();
            let ids: HashSet<_> = visible
                .iter()
                .map(|&(_, i)| session.entries[i].id.as_str())
                .collect();
            for (id, prompt) in self
                .prompt_windows
                .extract_if(|id, _| !ids.contains(id.as_str()))
            {
                if !prompt.window.known.is_empty() {
                    self.message_heights.insert(id, prompt.window.known);
                }
            }
            for (id, markdown) in self.markdown.extract_if(|id, _| !ids.contains(id.as_str())) {
                // Keep only numerical measurements in this session, not the
                // parsed items, text, images or widget trees.
                if let Some(window) = markdown.window
                    && !window.known.is_empty()
                {
                    self.message_heights.insert(id, window.known);
                }
            }
            self.tool_previews.retain(|id, _| ids.contains(id.as_str()));
            self.outputs
                .retain(|id, _| ids.contains(id.as_str()) && self.details.expanded.contains(id));
            self.historical_runs
                .retain(|id, _| ids.contains(id.as_str()));
            let settings = &self.catalog.settings;
            let lane =
                (self.center_dimensions().1 - 2. * self.chat_inset()).min(settings.chat_width);
            for &(position, index) in &visible {
                let entry = &session.entries[index];
                if let EntryKind::Tool { name, input, .. } = &entry.kind
                    && self
                        .tool_previews
                        .get(&entry.id)
                        .is_none_or(|(old, _)| !std::ptr::eq(old.as_ref(), entry))
                {
                    self.tool_previews.insert(
                        entry.id.clone(),
                        (
                            session.entries.shared(index),
                            activity::tool_preview(name, input),
                        ),
                    );
                }
                if !self.historical_read_pending
                    && (matches!(entry.kind, EntryKind::Notice)
                        || (self.details.expanded.contains(&entry.id)
                            && matches!(entry.kind, EntryKind::Tool { complete: true, .. })))
                    && let Some(key) = transcript_run_key(entry)
                    && let Some(h) = &self.harness
                    && !self.historical_runs.contains_key(&entry.id)
                    && h.resident_subagent(&session.id, key).is_none()
                {
                    // Bound historical reads independently of the foreground job lanes.
                    self.historical_read_pending = true;
                    let parent = session.id.clone();
                    let entry = session.entries.shared(index);
                    let h = h.clone();
                    self.tasks.push(Task::future(async move {
                        tokio::task::spawn_blocking(move || {
                            let result = h
                                .subagent_run(
                                    &parent,
                                    transcript_run_key(&entry).expect("assignment row"),
                                )
                                .map_err(|e| format!("{e:#}"));
                            Message::HistoricalRun(parent, entry.id.clone(), result)
                        })
                        .await
                        .expect("historical run reader")
                    }));
                }
                if let EntryKind::User { images, files, .. } = &entry.kind {
                    if entry.text.len() >= crate::message_window::Prompt::MIN_BYTES {
                        self.prompt_windows
                            .entry(entry.id.clone())
                            .or_insert_with(|| {
                                // User cards occupy the right two thirds with 12px padding on either side.
                                let mut prompt = crate::message_window::Prompt::new(
                                    session.entries.shared(index),
                                    lane * 2. / 3. - 24.,
                                    settings.font_size,
                                );
                                prompt.window.restore(
                                    self.message_heights.remove(&entry.id).unwrap_or_default(),
                                    &self.transcript,
                                    position,
                                    self.auto_scroll,
                                );
                                prompt
                            });
                    }
                    visible_media.extend(images.iter().cloned());
                    visible_media.extend(files.iter().filter_map(|file| file.image.clone()));
                }
                // User text is literal, including newlines and Markdown syntax.
                if matches!(entry.kind, EntryKind::Assistant | EntryKind::Reasoning) {
                    let markdown = self.markdown.entry(entry.id.clone()).or_insert_with(|| {
                        let content = markdown::Content::parse(&entry.text);
                        let mut markdown = Markdown {
                            eager_heights: Default::default(),
                            images: HashMap::new(),
                            window: None,
                            source: session.entries.shared(index),
                            content,
                        };
                        markdown.sync_window(self.catalog.settings.font_size, 0);
                        if let Some(window) = &mut markdown.window {
                            window.restore(
                                self.message_heights.remove(&entry.id).unwrap_or_default(),
                                &self.transcript,
                                position,
                                self.auto_scroll,
                            );
                        }
                        markdown.refresh_images();
                        markdown
                    });
                    // A value-equal final reconciliation is invisible to the
                    // history diff, but geometry messages carry the Arc identity.
                    if !std::ptr::eq(entry, markdown.source.as_ref()) {
                        markdown.source = session.entries.shared(index);
                    }
                    // Rows are visited in transcript order, so the newest link wins.
                    image_urls.extend(
                        markdown
                            .images
                            .iter()
                            .map(|(url, &position)| (url.clone(), (index, position))),
                    );
                }
                if let EntryKind::Tool {
                    name,
                    input,
                    output,
                    media,
                    ..
                } = &entry.kind
                    && self.details.expanded.contains(&entry.id)
                {
                    visible_media.extend(media.iter().cloned());
                    self.outputs.entry(entry.id.clone()).or_insert_with(|| {
                        tool_output::Output::new(
                            name,
                            input,
                            output,
                            self.details.raw_tool_inputs.contains(&entry.id),
                        )
                    });
                }
            }
        }
        self.tasks.push(
            self.markdown_images
                .prepare(
                    &mut self.recent_previews,
                    self.current_session
                        .as_ref()
                        .map(|s| (s.id.as_str(), s.project.as_path())),
                    image_urls,
                )
                .map(Message::MarkdownImage),
        );
        self.tasks.push(
            self.image_view
                .prepare(&mut self.recent_previews, &visible_media)
                .map(Message::Image),
        );
    }
    pub(super) fn activity_row(&self, projected: ActivityRow) -> Element<'_, Message> {
        let colors = &self.preset().colors;
        let session = self.current().expect("visible transcript");
        let index = projected.source();
        let entry = &session.entries[index];
        match projected {
            ActivityRow::Entry(index) => self.entry_view(entry, index),
            ActivityRow::Activity(_) => {
                let block = self.activity.block(index);
                let summary = block.summary.as_ref().expect("activity summary");
                let active = block.range.end == session.entries.len() && self.running();
                let working = active || summary.running > 0;
                let mut header = row![space().width(12).height(12)]
                    .spacing(8)
                    .align_y(iced::Center);
                header = header.push(ellipsized_text(
                    &summary.label(active),
                    12.,
                    Some(colors.muted),
                ));
                if working && summary.tools > 0 {
                    let progress = if summary.running > 0 {
                        format!(
                            "{} completed · {} running",
                            summary.completed, summary.running
                        )
                    } else {
                        format!("{} completed", summary.completed)
                    };
                    header = header.push(text(progress).size(11).style(theme::muted));
                }
                if summary.show_failures() {
                    header = header.push(
                        text(format!("{} failed", summary.failed))
                            .size(12)
                            .color(colors.danger),
                    );
                }
                theme::button(stack![
                    container(header).padding([6, 2]).width(Fill),
                    crate::chevron_icon::activity_disclosure(
                        self.details.groups.contains(&entry.id),
                        working,
                        self.tick,
                    ),
                ])
                .padding(0)
                .width(Fill)
                .style(theme::text_button)
                .on_press(Message::ToggleGroup(entry.id.clone()))
                .into()
            }
            ActivityRow::Execution(_) => {
                let wrappers = self.activity.block(index).folded.len();
                container(
                    theme::button(
                        row![
                            chevron_icon(self.details.execution.contains(&entry.id)),
                            text(format!(
                                "Execution details · {wrappers} code-mode call{}",
                                if wrappers == 1 { "" } else { "s" }
                            ))
                            .size(11)
                            .style(theme::muted)
                        ]
                        .spacing(8)
                        .align_y(iced::Center),
                    )
                    .padding([4, 2])
                    .style(theme::text_button)
                    .on_press(Message::ToggleExecution(entry.id.clone())),
                )
                .padding([0, 12])
                .into()
            }
            ActivityRow::Thinking(_) => {
                let range = &self.activity.block(index).thinking[&index];
                let preview = range
                    .clone()
                    .rev()
                    .map(|i| activity::reasoning_preview(&session.entries[i].text))
                    .find(|s| !s.is_empty())
                    .unwrap_or_else(|| "Thinking".into());
                container(
                    theme::button(
                        row![
                            chevron_icon(self.details.expanded.contains(&entry.id)),
                            ellipsized_text(&preview, 12., Some(colors.muted))
                        ]
                        .spacing(8)
                        .align_y(iced::Center),
                    )
                    .padding([4, 2])
                    .width(Fill)
                    .style(theme::text_button)
                    .on_press(Message::ToggleEntry(entry.id.clone())),
                )
                .padding([0, 12])
                .width(Fill)
                .into()
            }
        }
    }
    pub(super) fn transcript_run(&self, entry: &Entry) -> Option<RunInfo> {
        self.harness
            .as_ref()?
            .resident_subagent(&self.current()?.id, transcript_run_key(entry)?)
            .or_else(|| self.historical_runs.get(&entry.id)?.clone())
    }
    pub(super) fn subagent_notice_view<'a>(
        &'a self,
        entry: &'a Entry,
        index: usize,
    ) -> Element<'a, Message> {
        let colors = &self.preset().colors;
        let open = self.details.expanded.contains(&entry.id);
        let run = self.transcript_run(entry);
        let (label, color) = match &run {
            Some(run) => (
                format!(
                    "{} · {} · {}",
                    run.profile.label(),
                    run.title,
                    run.status.label()
                ),
                if run.status.needs_attention() {
                    colors.danger
                } else if run.status == orbit_core::agent::subagents::RunStatus::Completed {
                    colors.success
                } else {
                    colors.muted
                },
            ),
            None => ("Subagent update".into(), colors.muted),
        };
        let mut block = column![
            theme::button(
                row![
                    chevron_icon(open),
                    text("●").size(11).color(color),
                    ellipsized_text(&label, 12., Some(colors.muted))
                ]
                .spacing(8)
                .align_y(iced::Center)
            )
            .on_press(Message::ToggleEntry(entry.id.clone()))
            .padding([4, 2])
            .style(theme::text_button)
            .width(Fill)
        ]
        .spacing(6);
        if open {
            if let Some(run) = run {
                block = block.push(
                    theme::button(Icon::ArrowUpRight.label("Open subagent", 16.))
                        .style(theme::text_button)
                        .on_press(Message::Select(run.session_id)),
                );
            }
            block = block.push(chat_selection::plain(
                &entry.text,
                12.,
                Some(colors.muted),
                self.chat_selection.clone(),
                Position {
                    entry: index,
                    block: 0,
                    byte: 0,
                },
            ));
        }
        container(block)
            .padding(if open { [8, 12] } else { [0, 12] })
            .width(Fill)
            .style(if open { card } else { container::transparent })
            .into()
    }

    pub(super) fn entry_view<'a>(&'a self, entry: &'a Entry, index: usize) -> Element<'a, Message> {
        let colors = &self.preset().colors;
        match &entry.kind {
            EntryKind::Tool {
                name,
                input,
                complete,
                parent,
                failed,
                media,
                web,
                command_status,
                ..
            } => {
                let open = self.details.expanded.contains(&entry.id);
                let preview = self.tool_previews.get(&entry.id).map_or_else(
                    || std::borrow::Cow::Owned(activity::tool_preview(name, input)),
                    |(_, preview)| std::borrow::Cow::Borrowed(preview.as_str()),
                );
                let status = activity::tool_status(*complete, *failed, *command_status);
                let status_color = match status {
                    CommandStatus::Running => colors.warning,
                    CommandStatus::Succeeded => colors.success,
                    CommandStatus::Failed | CommandStatus::Interrupted => colors.danger,
                };
                let mut block = column![
                    theme::button(
                        row![
                            chevron_icon(open),
                            text("●").size(11).color(status_color),
                            text(activity::tool_name(name)).size(12).color(colors.text),
                            ellipsized_text(&preview, 12., Some(colors.muted)),
                            text(match status {
                                CommandStatus::Running => "running",
                                CommandStatus::Succeeded => "",
                                CommandStatus::Failed => "failed",
                                CommandStatus::Interrupted => "interrupted",
                            })
                            .size(11)
                            .color(
                                if matches!(
                                    status,
                                    CommandStatus::Failed | CommandStatus::Interrupted
                                ) {
                                    colors.danger
                                } else {
                                    colors.muted
                                }
                            )
                        ]
                        .spacing(9)
                        .align_y(iced::Center)
                    )
                    .on_press(Message::ToggleEntry(entry.id.clone()))
                    .style(theme::text_button)
                    .padding([4, 2])
                    .width(Fill)
                ]
                .spacing(4);
                let mut details = column![].spacing(8);
                if open
                    && name == "spawn_subagent"
                    && let Some(run) = self.transcript_run(entry)
                {
                    details = details.push(
                        theme::button(
                            text(format!(
                                "{} · {} · {} ↗",
                                run.profile.label(),
                                run.title,
                                run.status.label()
                            ))
                            .size(12),
                        )
                        .on_press(Message::Select(run.session_id))
                        .style(theme::text_button),
                    );
                }
                if open {
                    let stop = self.current().and_then(|session| {
                        let state = self
                            .harness
                            .as_ref()?
                            .get(&session.id)?
                            .tool_stop_state(&entry.id)?;
                        Some((session.id.as_str(), state))
                    });
                    details = details.push(tool_actions(
                        &entry.id,
                        self.details.raw_tool_inputs.contains(&entry.id),
                        stop,
                    ));
                    if let Some(metadata) = web {
                        details = details.push(
                            text(format!("Web access · {}", age(metadata.searched_at)))
                                .size(11)
                                .style(theme::muted),
                        );
                        for source in &metadata.sources {
                            details = details.push(
                                theme::button(ellipsized_text(
                                    &source.title,
                                    12.,
                                    Some(colors.accent),
                                ))
                                .on_press(Message::Link(source.url.clone()))
                                .style(theme::text_button),
                            );
                        }
                    }
                    for item in media {
                        if item.thumbnail.is_some() {
                            details = details.push(self.image_view.thumbnail(item));
                        }
                    }
                    if let Some(view) = self.outputs.get(&entry.id) {
                        details = details.push(
                            container(view.view(&entry.id, self.chat_selection.clone(), index))
                                .padding(8)
                                .width(Fill)
                                .style(theme::tool_code),
                        );
                    }
                    details = details.push(chat_selection::plain(
                        &tool_reference(&entry.id, parent.as_deref()),
                        11.,
                        Some(colors.muted),
                        self.chat_selection.clone(),
                        Position {
                            entry: index,
                            block: 1,
                            byte: 0,
                        },
                    ));
                    block = block.push(
                        container(container(details).padding(12).width(Fill).style(card))
                            .padding(iced::Padding {
                                left: 20.,
                                ..iced::Padding::ZERO
                            })
                            .width(Fill),
                    );
                }
                container(block).padding([0, 12]).width(Fill).into()
            }
            EntryKind::Notice if activity::subagent_notice(entry).is_some() => {
                self.subagent_notice_view(entry, index)
            }
            EntryKind::Notice => container(chat_selection::plain(
                &entry.text,
                12.,
                Some(colors.warning),
                self.chat_selection.clone(),
                Position {
                    entry: index,
                    block: 0,
                    byte: 0,
                },
            ))
            .padding(12)
            .style(card)
            .into(),
            EntryKind::Reasoning => {
                let content: Element<'_, Message> =
                    if let Some(markdown) = self.markdown.get(&entry.id) {
                        markdown.view(
                            markdown::Settings::with_text_size(
                                12.,
                                theme::markdown_style(&self.preset().theme),
                            ),
                            &chat_selection::Viewer::new(self.chat_selection.clone(), index)
                                .with_images(&self.markdown_images)
                                .with_equations(&self.equations, &entry.id),
                        )
                    } else {
                        chat_selection::plain(
                            &entry.text,
                            12.,
                            Some(colors.muted),
                            self.chat_selection.clone(),
                            Position {
                                entry: index,
                                block: 0,
                                byte: 0,
                            },
                        )
                    };
                container(content)
                    .padding([6, 24])
                    .style(card)
                    .width(Fill)
                    .into()
            }
            _ => {
                let mut content = column![].spacing(8);
                if let EntryKind::User {
                    images,
                    files,
                    steering,
                    ..
                } = &entry.kind
                {
                    if *steering {
                        content = content.push(
                            text("Steering · waiting for the next boundary")
                                .size(11)
                                .color(colors.accent),
                        );
                    }
                    for media in images {
                        content = content.push(self.image_view.thumbnail(media));
                    }
                    for file in files {
                        content = content.push(
                            text(format!("{} · {} bytes captured", file.name, file.bytes))
                                .size(11)
                                .style(theme::muted),
                        );
                        if let Some(image) = &file.image {
                            content = content.push(self.image_view.thumbnail(image));
                        }
                    }
                }
                let paint = self
                    .find
                    .as_ref()
                    .map(|f| f.paint(index, entry))
                    .unwrap_or_default();
                if let Some(markdown) = self.markdown.get(&entry.id) {
                    let mut viewer =
                        chat_selection::Viewer::new(self.chat_selection.clone(), index)
                            .with_images(&self.markdown_images)
                            .with_equations(&self.equations, &entry.id);
                    viewer.find = paint;
                    content = content.push(markdown.view(
                        markdown::Settings::with_text_size(
                            self.catalog.settings.font_size,
                            theme::markdown_style(&self.preset().theme),
                        ),
                        &viewer,
                    ));
                } else if let Some(prompt) = self.prompt_windows.get(&entry.id) {
                    content = content.push(prompt.view(
                        index,
                        self.catalog.settings.font_size,
                        self.chat_selection.clone(),
                        paint,
                    ));
                } else {
                    content = content.push(chat_selection::plain_find(
                        &entry.text,
                        self.catalog.settings.font_size,
                        None,
                        self.chat_selection.clone(),
                        Position {
                            entry: index,
                            block: 0,
                            byte: 0,
                        },
                        paint,
                    ));
                }
                if matches!(entry.kind, EntryKind::User { .. }) {
                    container(content).padding(12).style(card).into()
                } else {
                    container(content).padding([6, 12]).width(Fill).into()
                }
            }
        }
    }
}

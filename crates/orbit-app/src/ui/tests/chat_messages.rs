use super::*;

#[tokio::test]
async fn compaction_progress_follows_the_chat_tail_until_the_completion_notice() {
    let mut app = app();
    let path = app.smoke_dir.clone().unwrap();
    stored_session(&mut app, path).await;
    let h = app.harness.clone().unwrap();
    let id = app.selected.clone().unwrap();
    h.edit(&id, |s| s.accept("Review this change".into()))
        .unwrap();
    app.refresh();

    let progress = "Compacting context…";
    assert!(!chat_text(&app).contains(progress));
    for status in [
        ConversationStatus::CompactingContext,
        ConversationStatus::RecoveringContext,
    ] {
        app.current_status = status;
        let chat = chat_text(&app);
        assert!(
            chat.split_once("Review this change")
                .unwrap()
                .1
                .contains(progress)
        );
    }

    h.edit(&id, |s| {
        s.entries
            .push(Entry::new(EntryKind::Notice, "Context compacted."))
    })
    .unwrap();
    app.refresh();
    let chat = chat_text(&app);
    assert!(chat.contains("Context compacted."));
    assert!(!chat.contains(progress));
}

#[tokio::test]
async fn long_user_prompts_wrap_within_the_right_two_thirds_only() {
    let mut app = app();
    let mut session = Session::new("/chat".into(), &app.catalog.settings);
    let long = "Compare the wrapping of this user prompt and the assistant response. ".repeat(12);
    // A user prompt without a fork point must use the same width limit.
    session.accept(long.clone().into());
    session
        .entries
        .push(Entry::new(EntryKind::Reply { final_at: None }, &long));
    app.current_session = Some(session);
    let session = app.current().unwrap();
    let mut renderer = header_renderer();
    for width in [330., 810.] {
        let rows = [0, 1].map(|i| {
            let mut ui = iced_runtime::UserInterface::build(
                app.message_row(session, &session.entries[i], ActivityRow::Entry(i)),
                iced::Size::new(width, 2000.),
                iced_runtime::user_interface::Cache::new(),
                &mut renderer,
            );
            let mut bounds = UiBounds::default();
            ui.operate(&renderer, &mut bounds);
            bounds.label(&long)
        });
        let [prompt, reply] = rows;
        assert!(prompt.x >= width / 3. && prompt.x + prompt.width <= width);
        assert!(reply.x < prompt.x && reply.width > prompt.width && reply.height < prompt.height);
        // Long text should actually use the available two-thirds, rather
        // than passing the upper-bound check by wrapping at a smaller width.
        assert!((prompt.x - width / 3. - reply.x).abs() < 32.);
    }
}

#[tokio::test]
async fn user_messages_render_and_copy_literally_after_newline_and_paste() {
    use iced::advanced::widget::Operation;
    use iced_runtime::{UserInterface, user_interface::Cache};

    #[derive(Default)]
    struct TextContent(Vec<String>);
    impl Operation for TextContent {
        fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
            operate(self);
        }
        fn text(&mut self, _: Option<&iced::advanced::widget::Id>, _: iced::Rectangle, text: &str) {
            self.0.push(text.to_owned());
        }
    }

    let mut app = app();
    let root = app.smoke_dir.clone().unwrap();
    stored_session(&mut app, root).await;
    type_message(&mut app, "First line λ").await;
    dispatch(
        &mut app,
        Message::Editor(text_editor::Action::Edit(text_editor::Edit::Enter)),
    )
    .await;
    let _ = app.update(Message::Paste);
    dispatch(
        &mut app,
        Message::AttachmentLoaded(Ok(crate::attachments::Input::Text(
            "**literal**\n\n  indented\n![not an image](missing.png)".into(),
        ))),
    )
    .await;
    let expected = "First line λ\n**literal**\n\n  indented\n![not an image](missing.png)";
    assert_eq!(app.editor.text(), expected);
    send_current(&mut app).await;
    let id = app.selected.clone().unwrap();
    let h = app.harness.as_ref().unwrap().clone();
    h.edit(&id, |s| {
        s.entries.push(Entry::new(
            EntryKind::Reply { final_at: None },
            "**Still Markdown**",
        ));
    })
    .unwrap();
    dispatch(&mut app, Message::Changed).await;
    let saved = h.store.load_session(&id).unwrap();
    assert_eq!(saved.entries[0].text, expected);

    let mut renderer = crate::test_renderer::new(16.);
    let assistant = saved
        .entries
        .iter()
        .position(|e| matches!(e.kind, EntryKind::Reply { .. }))
        .unwrap();
    for (index, expected_text) in [(0, expected), (assistant, "Still Markdown")] {
        let mut ui = UserInterface::build(
            app.entry_view(&saved.entries[index], index),
            iced::Size::new(800., 600.),
            Cache::new(),
            &mut renderer,
        );
        let mut text = TextContent::default();
        ui.operate(&renderer, &mut text);
        assert_eq!(text.0.concat(), expected_text);
    }

    // Copy must match the literal display, even after its row is unmounted.
    app.transcript.viewport = 1.;
    app.transcript.measure(assistant, 2000.);
    app.transcript.end();
    app.prepare_visible();
    assert!(!app.transcript.order[app.transcript.visible()].contains(&0));
    {
        let mut selection = app.chat_selection.borrow_mut();
        selection.anchor = Some(Position {
            entry: 0,
            block: 0,
            byte: 0,
        });
        selection.focus = Some(Position {
            entry: 0,
            block: 0,
            byte: expected.len(),
        });
    }
    assert_eq!(
        copied_text(app.update_inner(Message::CopySelection)).await,
        expected
    );
}

#[test]
fn chat_link_destinations_resolve_local_paths_and_restrict_schemes() {
    let project = Path::new("/work/project");
    for (link, expected) in [
        ("docs/report%20λ.pdf", "/work/project/docs/report λ.pdf"),
        ("../shared/readme.md", "/work/shared/readme.md"),
        ("./-report.txt", "/work/project/-report.txt"),
        ("/tmp/report%20one.pdf", "/tmp/report one.pdf"),
        ("file:///tmp/report%20two.pdf", "/tmp/report two.pdf"),
        ("file://localhost/tmp/report.pdf", "/tmp/report.pdf"),
        (
            "https://example.org/docs?q=one#section",
            "https://example.org/docs?q=one#section",
        ),
        ("http://example.org/", "http://example.org/"),
    ] {
        assert_eq!(
            link_destination(link, Some(project)).unwrap(),
            OsString::from(expected),
            "{link}"
        );
    }
    assert_eq!(
        link_destination("/tmp/absolute.txt", None).unwrap(),
        OsString::from("/tmp/absolute.txt")
    );
    assert!(link_destination("relative.txt", None).is_err());
    for link in [
        "",
        "javascript:alert(1)",
        "data:text/plain,hello",
        "mailto:user@example.org",
        "file://remote-host/tmp/report.pdf",
        "//remote-host/tmp/report.pdf",
    ] {
        assert!(link_destination(link, Some(project)).is_err(), "{link}");
    }
}

#[tokio::test]
async fn local_chat_links_keep_confirmation_and_invalid_links_report_errors() {
    let mut app = app();
    dispatch(&mut app, Message::Link("docs/report.pdf".into())).await;
    assert!(matches!(&app.modal, Some(Modal::Link(link)) if link == "docs/report.pdf"));
    dispatch(&mut app, Message::Dismiss).await;
    assert!(app.modal.is_none());
    assert!(app.error.is_none());

    dispatch(&mut app, Message::Link("javascript:alert(1)".into())).await;
    dispatch(&mut app, Message::Confirm).await;
    assert!(app.modal.is_none());
    assert!(app.error.as_ref().unwrap().contains("Use a local path"));
}

#[tokio::test]
async fn steering_labels_live_only_in_queued_bubbles_not_the_composer() {
    let mut app = app();
    let root = app.smoke_dir.clone().unwrap();
    stored_session(&mut app, root).await;
    let h = app.harness.as_ref().unwrap().clone();
    let id = app.selected.clone().unwrap();
    let conversation = h.get(&id).unwrap();
    // Reproduce the interval after a normal submission starts a turn but
    // before the model boundary consumes the shared pending queue.
    conversation.running.store(true, Ordering::Release);
    h.edit(&id, |s| {
        s.accept_submission("normal".into(), "Same text".into(), false)
    })
    .unwrap();
    app.refresh();
    assert!(!chat_text(&app).to_lowercase().contains("steering"));

    h.edit(&id, |s| {
        for submission in ["steer-1", "steer-2"] {
            s.accept_submission(submission.into(), "Same text".into(), true);
        }
    })
    .unwrap();
    app.refresh();
    assert!(!entry_text(&app, 0).contains("Steering"));
    for index in [1, 2] {
        assert!(entry_text(&app, index).contains("Steering"));
    }
    assert_eq!(chat_text(&app).matches("Steering").count(), 2);
    assert!(!chat_text(&app).contains("steering message(s)"));

    h.edit(&id, Session::consume_pending).unwrap();
    app.refresh();
    assert!(!chat_text(&app).contains("Steering"));
    for index in 0..3 {
        assert_eq!(entry_text(&app, index), "Same text");
    }
    conversation.running.store(false, Ordering::Release);
}

fn ctrl_f() -> Message {
    Message::Event(iced::Event::Keyboard(keyboard::Event::KeyPressed {
        key: keyboard::Key::Character("f".into()),
        modified_key: keyboard::Key::Character("f".into()),
        physical_key: keyboard::key::Physical::Code(keyboard::key::Code::KeyF),
        location: keyboard::Location::Standard,
        modifiers: keyboard::Modifiers::CTRL,
        text: None,
        repeat: false,
    }))
}

#[tokio::test]
async fn find_discards_obsolete_queries_and_sessions_without_touching_drafts() {
    let mut app = app();
    let path = app.smoke_dir.clone().unwrap();
    stored_session(&mut app, path.clone()).await;
    let id = app.selected.clone().unwrap();
    let h = app.harness.clone().unwrap();
    h.edit(&id, |s| {
        s.entries.push(Entry::new(
            EntryKind::Reply { final_at: None },
            "old **needle** new",
        ))
    })
    .unwrap();
    dispatch(&mut app, Message::Changed).await;
    type_message(&mut app, "keep this draft").await;
    dispatch(&mut app, ctrl_f()).await;
    let old = app.update(Message::Find(find::Action::Query("old".into())));
    dispatch(
        &mut app,
        Message::Find(find::Action::Query("needle".into())),
    )
    .await;
    drain(&mut app, old).await;
    let session = app.current().unwrap();
    let paint = app.find.as_ref().unwrap().paint(0, &session.entries[0]);
    let active = paint.active.unwrap();
    assert_eq!((active.start, active.end), (4, 10));
    h.edit(&id, |s| s.entries[0].text.push_str(" needle"))
        .unwrap();
    dispatch(&mut app, Message::Changed).await;
    let paint = app
        .find
        .as_ref()
        .unwrap()
        .paint(0, &app.current().unwrap().entries[0]);
    assert_eq!(paint.block(0).len(), 2);
    assert_eq!(paint.active, Some(active));
    dispatch(&mut app, footer_key(keyboard::key::Named::Escape)).await;
    assert!(app.find.is_none());
    assert_eq!(app.editor.text(), "keep this draft");
    dispatch(&mut app, ctrl_f()).await;
    let stale = app.update(Message::Find(find::Action::Query("needle".into())));
    stored_session(&mut app, path).await;
    dispatch(&mut app, ctrl_f()).await;
    drain(&mut app, stale).await;
    let mut renderer = header_renderer();
    let mut ui = iced_runtime::UserInterface::build(
        app.center(),
        Size::new(650., 420.),
        Default::default(),
        &mut renderer,
    );
    let mut labels = UiBounds::default();
    ui.operate(&renderer, &mut labels);
    assert!(
        !labels
            .labels
            .iter()
            .any(|(text, _)| text.contains(" of ") || text == "No matches"),
        "a newly opened Find has no previous results"
    );
}

#[tokio::test]
async fn find_keeps_last_count_and_only_animates_slow_queries() {
    use iced::{advanced::clipboard, time::Instant, window::RedrawRequest};
    use iced_runtime::{UserInterface, user_interface::State};

    let mut app = app();
    let mut session = Session::new("/chat".into(), &app.catalog.settings);
    session.accept("cat cat cat".to_owned().into());
    app.current_session = Some(session);
    app.rebuild_transcript();
    let query = |s: &str| Message::Find(find::Action::Query(s.into()));
    dispatch(&mut app, Message::Find(find::Action::Open)).await;
    dispatch(&mut app, query("cat")).await;
    dispatch(&mut app, Message::Find(find::Action::Step(false))).await;

    let mut renderer = header_renderer();
    let mut frame = |app: &App, now| {
        let mut ui = UserInterface::build(
            app.find.as_ref().unwrap().popup(),
            Size::new(600., 90.),
            Default::default(),
            &mut renderer,
        );
        let mut labels = UiBounds::default();
        ui.operate(&renderer, &mut labels);
        let (State::Updated { redraw_request, .. }, _) = ui.update(
            &[iced::Event::Window(window::Event::RedrawRequested(now))],
            mouse::Cursor::Unavailable,
            &mut renderer,
            &mut clipboard::Null,
            &mut vec![],
        ) else {
            panic!("popup should not need rebuilding")
        };
        (
            labels
                .labels
                .into_iter()
                .map(|(s, _)| s)
                .collect::<String>(),
            redraw_request,
        )
    };

    // Hold the worker task and drive redraw time directly instead of racing a sleep.
    let delay = Duration::from_millis(250);
    let start = Instant::now();
    let worker = app.update(query("absent"));
    let issued = Instant::now();
    let (count, RedrawRequest::At(deadline)) = frame(&app, start) else {
        panic!("expected a single delayed redraw")
    };
    assert!((start + delay..=issued + delay).contains(&deadline));
    assert_eq!(count, "2 of 3");
    assert_eq!(
        frame(&app, deadline - Duration::from_millis(1)).1,
        RedrawRequest::At(deadline)
    );
    assert_eq!(frame(&app, deadline), (count, RedrawRequest::NextFrame));
    drain(&mut app, worker).await;
    assert_eq!(
        frame(&app, deadline),
        ("No matches".into(), RedrawRequest::Wait)
    );

    let cancelled = app.update(query("cat"));
    dispatch(&mut app, query("")).await;
    drain(&mut app, cancelled).await;
    assert_eq!(
        frame(&app, deadline + delay),
        (String::new(), RedrawRequest::Wait)
    );
}

#[tokio::test]
async fn settings_and_find_can_open_in_either_order() {
    let mut app = app();
    let path = app.smoke_dir.clone().unwrap();
    stored_session(&mut app, path).await;
    app.harness
        .as_ref()
        .unwrap()
        .edit(app.selected.as_ref().unwrap(), |s| {
            s.entries.push(Entry::new(
                EntryKind::Reply { final_at: None },
                "needle and needle",
            ));
        })
        .unwrap();
    dispatch(&mut app, Message::Changed).await;
    let size = Size::new(app.window_width, app.window_height);
    let mut cache = Default::default();
    let mut renderer = header_renderer();
    let enter = footer_key(keyboard::key::Named::Enter);
    let escape = footer_key(keyboard::key::Named::Escape);

    // Opening Settings must not discard an existing query.
    dispatch(&mut app, ctrl_f()).await;
    dispatch(
        &mut app,
        Message::Find(find::Action::Query("needle".into())),
    )
    .await;
    dispatch(&mut app, Message::ToggleSettings).await;
    assert!(chat_text(&app).contains("1 of 2"));
    dispatch(&mut app, escape.clone()).await;
    assert!(app.find.is_none() && app.right_panel == Some(RightPanel::Settings));

    // Ctrl+F from Settings focuses Find, so Enter advances its match.
    drive_ui_messages(
        App::view,
        &mut app,
        &mut renderer,
        &mut cache,
        size,
        vec![
            ctrl_f(),
            Message::Find(find::Action::Query("needle".into())),
            enter,
        ],
    )
    .await;
    assert!(chat_text(&app).contains("2 of 2"));
    dispatch(&mut app, escape).await;
    assert!(app.find.is_none() && app.right_panel == Some(RightPanel::Settings));
}

#[tokio::test]
async fn message_footers_copy_source_and_only_date_recorded_replies() {
    use chrono::TimeZone as _;
    let mut app = app();
    let mut session = Session::new("/chat".into(), &app.catalog.settings);
    session.accept("First prompt".into());
    session
        .entries
        .push(Entry::new(EntryKind::Reply { final_at: None }, "An update"));
    let image = orbit_core::media::MediaStore::new(app.smoke_dir.as_ref().unwrap().join("images"))
        .image(include_bytes!("../../fixtures/media.png"))
        .unwrap();
    session.accept(orbit_core::store::UserMessage {
        images: vec![image],
        ..Default::default()
    });
    let sent_at = chrono::Local
        .with_ymd_and_hms(2025, 9, 15, 22, 34, 0)
        .single()
        .unwrap()
        .timestamp() as u64;
    session.entries.push(Entry::new(
        EntryKind::Reply {
            final_at: Some(sent_at),
        },
        "A **new reply**",
    ));
    app.current_session = Some(session);
    app.rebuild_transcript();
    app.prepare_visible();

    let session = app.current().unwrap();
    let mut renderer = header_renderer();
    for i in 0..session.entries.len() {
        let entry = &session.entries[i];
        let mut ui = iced_runtime::UserInterface::build(
            app.message_row(session, entry, ActivityRow::Entry(i)),
            iced::Size::new(600., 160.),
            iced_runtime::user_interface::Cache::new(),
            &mut renderer,
        );
        let mut bounds = UiBounds::default();
        ui.operate(&renderer, &mut bounds);
        let body = bounds.labels[0].1;
        // The only action in these rows is Copy (this prompt has no fork point).
        let footer = bounds
            .containers
            .iter()
            .filter(|r| r.y >= body.y + body.height && r.width == r.height && r.width > 0.)
            .max_by(|a, b| a.x.total_cmp(&b.x));
        if i == 1 || i == 2 {
            assert!(
                footer.is_none(),
                "no copy for progress or image-only prompts"
            );
            continue;
        }
        let copy = footer.expect("copy control below the message");
        if i == 3 {
            let time = bounds.label("Monday 10:34 PM");
            assert!(time.y > body.y && time.x + time.width < copy.x);
            let width = time.x + copy.width + time.width / 2.;
            let mut narrow = iced_runtime::UserInterface::build(
                app.message_row(session, entry, ActivityRow::Entry(i)),
                iced::Size::new(width, 160.),
                iced_runtime::user_interface::Cache::new(),
                &mut renderer,
            );
            let mut compact = UiBounds::default();
            narrow.operate(&renderer, &mut compact);
            let date = compact.label("Monday 10:34 PM");
            let button = compact.rightmost_control_at(date.center_y());
            assert!(date.width < time.width && date.x + date.width <= button.x);
            assert_eq!(button.width, copy.width);
            assert!(button.x + button.width <= width);
        }
        assert!(
            matches!(click_at(&mut ui, &mut renderer, copy.center()).as_slice(),
            [Message::CopyTranscript { entry_id, equation_block: None, source }]
                if entry_id == &entry.id && source == &entry.text)
        );
    }
}

#[tokio::test]
async fn copying_again_keeps_the_check_until_the_new_timeout() {
    use iced::futures::StreamExt as _;
    let mut app = app();
    let click = |id: &str| Message::CopyTranscript {
        entry_id: id.into(),
        equation_block: None,
        source: "**Same text**, different rows".into(),
    };
    let clicks = ["prompt", "reply", "prompt"].map(|id| app.update_inner(click(id)));

    // Deliver each click's real timeout in order. The first must not clear
    // the renewed prompt, and clearing the reply must leave the prompt alone.
    for (i, task) in clicks.into_iter().enumerate() {
        let mut actions = iced_runtime::task::into_stream(task).unwrap();
        let mut clipboard = None;
        while let Some(action) = actions.next().await {
            match action {
                iced_runtime::Action::Clipboard(iced_runtime::clipboard::Action::Write {
                    contents,
                    ..
                }) => {
                    clipboard = Some(contents);
                }
                iced_runtime::Action::Output(message) => {
                    let _ = app.update_inner(message);
                }
                _ => (),
            }
        }
        assert_eq!(clipboard.as_deref(), Some("**Same text**, different rows"));
        assert_eq!(app.copied_messages.contains_key("reply"), i == 0);
        assert_eq!(app.copied_messages.contains_key("prompt"), i < 2);
    }
}

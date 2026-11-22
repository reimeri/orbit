use super::*;

#[tokio::test]
async fn long_user_prompts_wrap_within_the_right_two_thirds_only() {
    let mut app = app();
    let mut session = Session::new("/chat".into(), &app.catalog.settings);
    let long = "Compare the wrapping of this user prompt and the assistant response. ".repeat(12);
    // A user prompt without a fork point must use the same width limit.
    session.accept(long.clone().into());
    session
        .entries
        .push(Entry::new(EntryKind::Assistant, &long));
    app.current_session = Some(session);
    let session = app.current().unwrap();
    let mut renderer = header_renderer();
    for width in [330., 810.] {
        let rows = [0, 1].map(|i| {
            let mut ui = iced_runtime::UserInterface::build(
                app.fork_row(session, &session.entries[i], ActivityRow::Entry(i)),
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
    dispatch(&mut app, Message::Send).await;
    let id = app.selected.clone().unwrap();
    let h = app.harness.as_ref().unwrap().clone();
    h.edit(&id, |s| {
        s.entries
            .push(Entry::new(EntryKind::Assistant, "**Still Markdown**"));
    })
    .unwrap();
    dispatch(&mut app, Message::Changed).await;
    let saved = h.store.load_session(&id).unwrap();
    assert_eq!(saved.entries[0].text, expected);

    let mut renderer = crate::test_renderer::new(16.);
    let assistant = saved
        .entries
        .iter()
        .position(|e| matches!(e.kind, EntryKind::Assistant))
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

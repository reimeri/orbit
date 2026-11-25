use super::*;
use crate::ui::sidebar::{FILED_KINDS, FILED_VISIBLE_ROWS, filed_scroll_id};

#[tokio::test]
async fn session_row_press_and_pending_load_keep_the_composer_focused() {
    use iced::advanced::widget::{Id, operation::focusable};
    use iced::futures::StreamExt;
    use iced_runtime::{UserInterface, user_interface::Cache};

    let mut app = app();
    let project = app.smoke_dir.clone().unwrap();
    stored_session(&mut app, project.clone()).await;
    let target = app.selected.clone().unwrap();
    app.harness
        .as_ref()
        .unwrap()
        .edit(&target, |s| {
            s.title = "Destination".into();
            s.draft = "incoming draft".into();
        })
        .unwrap();
    stored_session(&mut app, project).await;
    let outgoing = app.selected.clone().unwrap();
    type_message(&mut app, "outgoing draft").await;

    let mut renderer = header_renderer();
    let size = iced::Size::new(app.window_width, app.window_height);
    let mut ui = UserInterface::build(app.view(), size, Cache::new(), &mut renderer);
    ui.operate(&renderer, &mut focusable::focus("composer".into()));
    let mut bounds = UiBounds::default();
    ui.operate(&renderer, &mut bounds);
    let cursor = mouse::Cursor::Available(bounds.label("Destination").center());
    let mut messages = vec![];
    for event in [
        mouse::Event::ButtonPressed(mouse::Button::Left),
        mouse::Event::ButtonReleased(mouse::Button::Left),
    ] {
        ui.update(
            &[iced::Event::Mouse(event)],
            cursor,
            &mut renderer,
            &mut iced::advanced::clipboard::Null,
            &mut messages,
        );
        if matches!(event, mouse::Event::ButtonPressed(_)) {
            assert!(messages.is_empty(), "holding the row must not activate it");
        }
        let mut bounds = UiBounds::default();
        ui.operate(&renderer, &mut bounds);
        assert_eq!(bounds.focused, Some(Id::new("composer")));
    }
    let cache = ui.into_cache();
    let [select @ Message::Select(id)] = messages.as_slice() else {
        panic!("expected a row selection: {messages:?}");
    };
    assert_eq!(id, &target);
    let pending = app.update(select.clone());
    // A core change can arrive before the UI receives the selection result. It must not unmount the outgoing editor.
    let mut actions = iced_runtime::task::into_stream(pending).unwrap();
    let selected = loop {
        if let iced_runtime::Action::Output(message @ Message::Selected(_)) =
            actions.next().await.unwrap()
        {
            break message;
        }
    };
    dispatch(&mut app, Message::Changed).await;
    assert_eq!(app.current().unwrap().id, outgoing);
    assert_eq!(app.editor.text(), "outgoing draft");
    let mut ui = UserInterface::build(app.view(), size, cache, &mut renderer);
    let mut bounds = UiBounds::default();
    ui.operate(&renderer, &mut bounds);
    assert_eq!(bounds.focused, Some(Id::new("composer")));
    let cache = ui.into_cache();
    dispatch(&mut app, selected).await;
    assert_eq!(app.editor.text(), "incoming draft");
    let mut ui = UserInterface::build(app.view(), size, cache, &mut renderer);
    let mut bounds = UiBounds::default();
    ui.operate(&renderer, &mut bounds);
    assert_eq!(bounds.focused, Some(Id::new("composer")));

    // The section header captures presses too, but is not part of a session list.
    let messages = click_at(&mut ui, &mut renderer, bounds.label("Deferred").center());
    assert!(matches!(
        messages.as_slice(),
        [Message::ToggleFiled(FilingKind::Deferred)]
    ));
    let mut bounds = UiBounds::default();
    ui.operate(&renderer, &mut bounds);
    assert_eq!(bounds.focused, None);

    let messages = click_at(&mut ui, &mut renderer, bounds.label("Destination").center());
    assert!(matches!(messages.as_slice(), [Message::Select(id)] if id == &target));
    let mut bounds = UiBounds::default();
    ui.operate(&renderer, &mut bounds);
    assert_eq!(
        bounds.focused, None,
        "a row must not give an unfocused composer focus"
    );
}

#[tokio::test]
async fn filed_list_scrollbars_do_not_blur_the_composer() {
    use iced::advanced::widget::{Id, operation::focusable};
    use iced_runtime::{UserInterface, user_interface::Cache};

    let mut app = app();
    let project = app.smoke_dir.clone().unwrap();
    stored_session(&mut app, project.clone()).await;
    for kind in FILED_KINDS {
        app.filed[kind as usize].open = true;
        for _ in 0..FILED_VISIBLE_ROWS * 2 {
            let mut session = Session::new(project.clone(), &app.catalog.settings);
            session.filing = Some(Filing {
                kind,
                at: session.updated,
            });
            app.sessions.push(SessionSummary::from(&session));
        }
    }
    app.prepare_sidebar();
    let mut renderer = header_renderer();
    let size = iced::Size::new(app.window_width, app.window_height);
    let mut ui = UserInterface::build(app.view(), size, Cache::new(), &mut renderer);
    ui.operate(&renderer, &mut focusable::focus("composer".into()));
    for kind in FILED_KINDS {
        let mut bounds = UiBounds::default();
        ui.operate(&renderer, &mut bounds);
        let list = bounds.scroll(filed_scroll_id(kind));
        let track = iced::Point::new(
            list.x + list.width - theme::SCROLLBAR_WIDTH / 2.,
            list.y + list.height - 1.,
        );
        let messages = click_at(&mut ui, &mut renderer, track);
        assert!(
            messages.iter().any(|message| {
                matches!(message, Message::FiledScroll(k, _, offset, _) if *k == kind && *offset > 0.)
            }),
            "track click must scroll {kind:?}: {messages:?}"
        );
        let mut bounds = UiBounds::default();
        ui.operate(&renderer, &mut bounds);
        assert_eq!(bounds.focused, Some(Id::new("composer")));
    }
}

#[tokio::test]
async fn image_paste_preserves_composer_focus_across_attachment_changes() {
    use iced::advanced::widget::operation::focusable;
    use iced_runtime::{UserInterface, user_interface::Cache};

    let mut app = app();
    let root = app.smoke_dir.clone().unwrap();
    dispatch(&mut app, Message::Create(root.clone())).await;
    let image = orbit_core::media::MediaStore::new(root.join("focus-images"))
        .image(include_bytes!("../../fixtures/media.png"))
        .unwrap();
    let mut renderer = header_renderer();
    let size = iced::Size::new(930., 800.);
    let mut ui = UserInterface::build(app.center(), size, Cache::new(), &mut renderer);
    ui.operate(&renderer, &mut focusable::focus("composer".into()));
    let mut cache = ui.into_cache();

    // Keep the real widget cache through the first image, another image,
    // and removal of the strip. Typing must work without refocusing.
    let mut expected = String::new();
    for (change, keys) in ["@x ", "y", "z", "w"].into_iter().enumerate() {
        match change {
            0 | 1 => {
                let _ = app.update(Message::Paste);
                dispatch(
                    &mut app,
                    Message::AttachmentLoaded(Ok(crate::attachments::Input::Image(image.clone()))),
                )
                .await;
            }
            _ => dispatch(&mut app, Message::RemoveAttachment(0)).await,
        }
        assert_eq!(app.attachments.len(), [1, 2, 1, 0][change]);
        for ch in keys.chars() {
            let mut ui = UserInterface::build(app.center(), size, cache, &mut renderer);
            let mut messages = Vec::new();
            ui.update(
                &[iced::Event::Keyboard(keyboard::Event::KeyPressed {
                    key: keyboard::Key::Character(ch.to_string().into()),
                    modified_key: keyboard::Key::Character(ch.to_string().into()),
                    physical_key: keyboard::key::Physical::Unidentified(
                        keyboard::key::NativeCode::Unidentified,
                    ),
                    location: keyboard::Location::Standard,
                    modifiers: keyboard::Modifiers::empty(),
                    text: Some(ch.to_string().into()),
                    repeat: false,
                })],
                mouse::Cursor::Unavailable,
                &mut renderer,
                &mut iced::advanced::clipboard::Null,
                &mut messages,
            );
            cache = ui.into_cache();
            assert!(
                matches!(messages.as_slice(), [Message::Editor(_)]),
                "composer must still accept typing after attachment change {change}"
            );
            for message in messages {
                dispatch(&mut app, message).await;
            }
            expected.push(ch);
            assert_eq!(app.editor.text(), expected);
            assert_eq!(app.file_completion.is_open(), matches!(ch, '@' | 'x'));
        }
    }
}

#[tokio::test]
async fn file_picker_escape_is_local_but_hidden_picker_does_not_capture_modal_escape() {
    use iced::advanced::widget::{Operation, operation::Focusable};
    use iced::futures::{StreamExt, stream};
    use iced_runtime::futures::subscription;
    use iced_runtime::{UserInterface, user_interface::Cache};
    struct Focus;
    impl Operation for Focus {
        fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
            operate(self);
        }
        fn focusable(
            &mut self,
            _: Option<&iced::advanced::widget::Id>,
            _: iced::Rectangle,
            state: &mut dyn Focusable,
        ) {
            state.focus();
        }
    }
    let mut app = app();
    let root = app.smoke_dir.clone().unwrap();
    dispatch(&mut app, Message::Create(root)).await;
    dispatch(
        &mut app,
        Message::Editor(text_editor::Action::Edit(text_editor::Edit::Insert('@'))),
    )
    .await;
    let mut renderer = header_renderer();
    let mut cache = Cache::new();
    for modal in [true, false] {
        app.modal = modal.then_some(Modal::AddProject);
        let field: Element<'_, Message> = if modal {
            text_input("Modal input", "fixture")
                .id("modal-input")
                .on_input(Message::ModalInput)
                .into()
        } else {
            app.composer_editor("fixture", false)
        };
        let mut ui = UserInterface::build(field, iced::Size::new(600., 200.), cache, &mut renderer);
        ui.operate(&renderer, &mut Focus);
        let Message::Event(event) = footer_key(keyboard::key::Named::Escape) else {
            unreachable!()
        };
        let mut messages = Vec::new();
        let (_, statuses) = ui.update(
            std::slice::from_ref(&event),
            mouse::Cursor::Unavailable,
            &mut renderer,
            &mut iced::advanced::clipboard::Null,
            &mut messages,
        );
        cache = ui.into_cache();
        assert!(messages.is_empty(), "the central handler owns Escape");
        assert_eq!(
            statuses,
            vec![if modal {
                iced::event::Status::Captured
            } else {
                iced::event::Status::Ignored
            }]
        );
        let mut routed = Vec::new();
        for recipe in subscription::into_recipes(window_events()) {
            let input = stream::iter([subscription::Event::Interaction {
                window: window::Id::unique(),
                event: event.clone(),
                status: statuses[0],
            }])
            .boxed();
            routed.extend(recipe.stream(input).collect::<Vec<_>>().await);
        }
        assert!(
            matches!(routed.as_slice(), [Message::Event(_)]),
            "captured text-input Escape must still reach the central handler"
        );
        for message in routed {
            dispatch(&mut app, message).await;
        }
        assert!(app.modal.is_none());
        assert_eq!(
            app.file_completion.is_open(),
            modal,
            "hidden picker must not consume modal Escape; visible picker must dismiss before app-wide actions"
        );
    }
}

#[test]
fn composer_history_shortcuts_require_focused_editor_and_control_only() {
    for (key, direction) in [("z", Direction::Undo), ("y", Direction::Redo)] {
        for (modifiers, status, expected) in [
            (
                keyboard::Modifiers::CTRL,
                text_editor::Status::Active,
                false,
            ),
            (
                keyboard::Modifiers::CTRL,
                text_editor::Status::Focused { is_hovered: false },
                true,
            ),
            (
                keyboard::Modifiers::CTRL | keyboard::Modifiers::ALT,
                text_editor::Status::Focused { is_hovered: true },
                false,
            ),
        ] {
            let binding = composer_key_binding(
                "fixture",
                text_editor::KeyPress {
                    key: keyboard::Key::Character(key.into()),
                    modified_key: keyboard::Key::Character(key.into()),
                    physical_key: keyboard::key::Physical::Code(keyboard::key::Code::KeyZ),
                    modifiers,
                    text: None,
                    status,
                },
            );
            assert_eq!(
                matches!(binding, Some(text_editor::Binding::Custom(Message::ComposerHistory(actual))) if actual == direction),
                expected
            );
        }
    }
}

#[tokio::test]
async fn composer_tabs_indent_at_caret_or_across_selection_as_one_edit() {
    use text_editor::{Cursor, Position};

    let tab = |modifiers| {
        let key = keyboard::Key::Named(keyboard::key::Named::Tab);
        let Some(text_editor::Binding::Custom(message)) = composer_key_binding(
            "fixture",
            text_editor::KeyPress {
                key: key.clone(),
                modified_key: key,
                physical_key: keyboard::key::Physical::Code(keyboard::key::Code::Tab),
                modifiers,
                text: None,
                status: text_editor::Status::Focused { is_hovered: false },
            },
        ) else {
            panic!("expected composer Tab binding")
        };
        message
    };
    let mut app = app();
    let root = app.smoke_dir.clone().unwrap();
    dispatch(&mut app, Message::Create(root)).await;
    let original = "  λ z\r\n\t界\n\nlast";
    app.editor = text_editor::Content::with_text(original);
    let pos = |line, column| Position { line, column };
    app.editor.move_to(Cursor {
        position: pos(0, "  λ ".len()),
        selection: None,
    });
    dispatch(&mut app, tab(keyboard::Modifiers::NONE)).await;
    assert_eq!(app.new_draft.message, "  λ     z\r\n\t界\n\nlast");
    assert_eq!(app.editor.cursor().position, pos(0, "  λ     ".len()));
    dispatch(&mut app, Message::ComposerHistory(Direction::Undo)).await;

    // Backward selection starts inside a partial indent and ends at column zero.
    let selected = Cursor {
        position: pos(0, 1),
        selection: Some(pos(3, 0)),
    };
    app.editor.move_to(selected);
    dispatch(&mut app, tab(keyboard::Modifiers::NONE)).await;
    assert_eq!(app.editor.text(), "      λ z\r\n    \t界\n    \nlast");
    assert_eq!(
        app.editor.cursor(),
        Cursor {
            position: pos(0, 5),
            ..selected
        }
    );
    dispatch(&mut app, tab(keyboard::Modifiers::SHIFT)).await;
    assert_eq!(app.editor.text(), original);
    assert_eq!(app.editor.cursor(), selected);
    dispatch(&mut app, tab(keyboard::Modifiers::SHIFT)).await;
    assert_eq!(app.editor.text(), "λ z\r\n界\n\nlast");
    assert_eq!(
        app.editor.cursor(),
        Cursor {
            position: pos(0, 0),
            ..selected
        }
    );
    dispatch(&mut app, Message::ComposerHistory(Direction::Undo)).await;
    assert_eq!(app.editor.text(), original);
    assert_eq!(app.editor.cursor(), selected);
}

#[tokio::test]
async fn slash_picker_completes_without_sending_and_only_exact_compact_is_local() {
    let mut app = app();
    let root = app.smoke_dir.clone().unwrap();
    stored_session(&mut app, root).await;
    type_message(&mut app, "/com").await;
    assert!(app.file_completion.is_open());
    let accept = app.file_completion.accept_selected();
    dispatch(&mut app, Message::FileCompletion(accept)).await;
    assert_eq!(app.editor.text(), "/compact");
    assert!(!app.file_completion.is_open());
    assert!(app.current().unwrap().entries.is_empty());
    send_current(&mut app).await;
    assert!(
        app.error
            .as_deref()
            .unwrap()
            .contains("no conversation context")
    );
    assert!(app.current().unwrap().entries.is_empty());
    assert_eq!(app.editor.text(), "/compact");

    type_message(&mut app, "\n/tmp/notes").await;
    send_current(&mut app).await;
    let entry = &app.current().unwrap().entries[0];
    assert!(matches!(entry.kind, EntryKind::User { .. }));
    assert_eq!(entry.text, "/compact\n/tmp/notes");
}

#[tokio::test]
async fn completed_tag_for_a_deleted_file_sends_as_authored_text() {
    use iced::futures::{StreamExt, stream};
    use iced_runtime::futures::subscription;
    let mut app = app();
    let root = app.smoke_dir.clone().unwrap();
    std::fs::write(root.join("source notes.md"), "tagged contents").unwrap();
    dispatch(&mut app, Message::Create(root.clone())).await;
    // The native widget can produce the edit and Enter from the same view.
    let enter_before_rebuild = composer_enter(&app);
    dispatch(
        &mut app,
        Message::Editor(text_editor::Action::Edit(text_editor::Edit::Paste(
            Arc::new("Compare @source".into()),
        ))),
    )
    .await;
    // Before the first index response, the complete-looking tag is still
    // being checked. Don't send it and don't accept a stale suggestion.
    dispatch(&mut app, enter_before_rebuild).await;
    assert_eq!(app.editor.text(), "Compare @source");
    assert!(
        app.notice
            .as_ref()
            .is_some_and(|(text, _)| text.contains("file search"))
    );
    let mut messages = stream::select_all(
        subscription::into_recipes(app.file_completion.subscription())
            .into_iter()
            .map(|recipe| recipe.stream(stream::pending().boxed())),
    );
    let mut wait_for_picker = async |app: &mut App| {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let message = messages.next().await.unwrap();
                let ready = matches!(&message, crate::file_completion::Message::Results(r)
                    if r.phase != orbit_core::file_index::SearchPhase::Pending
                        && r.paths.contains(&"@\"source notes.md\"".to_owned()));
                dispatch(app, Message::FileCompletion(message)).await;
                if ready && app.file_completion.is_open() {
                    break;
                }
            }
        })
        .await
        .unwrap();
    };
    wait_for_picker(&mut app).await;
    dispatch(&mut app, Message::ToggleSearch).await;
    // Clicking a picker row takes focus away from the editor before acceptance.
    // Exercise the returned focus operation on the retained native editor tree.
    use iced_runtime::{UserInterface, user_interface::Cache};
    let size = iced::Size::new(600., 200.);
    let mut renderer = header_renderer();
    let mut ui = UserInterface::build(
        app.composer_editor("fixture", false),
        size,
        Cache::new(),
        &mut renderer,
    );
    ui.operate(
        &renderer,
        &mut iced::advanced::widget::operation::focusable::focus("composer".into()),
    );
    // With search still open, Enter in the focused composer must accept the
    // file, never submit the unfinished message.
    let Message::Event(enter) = footer_key(keyboard::key::Named::Enter) else {
        unreachable!()
    };
    let mut messages = Vec::new();
    ui.update(
        &[enter],
        mouse::Cursor::Unavailable,
        &mut renderer,
        &mut iced::advanced::clipboard::Null,
        &mut messages,
    );
    let [accept @ Message::FileCompletion(crate::file_completion::Message::Accept { .. })] =
        messages.as_slice()
    else {
        panic!("expected file acceptance, got {messages:?}");
    };
    ui.update(
        &[iced::Event::Mouse(mouse::Event::ButtonPressed(
            mouse::Button::Left,
        ))],
        mouse::Cursor::Available(iced::Point::new(size.width + 1., size.height + 1.)),
        &mut renderer,
        &mut iced::advanced::clipboard::Null,
        &mut Vec::new(),
    );
    let cache = ui.into_cache();
    let task = app.update(accept.clone());
    let mut ui = UserInterface::build(
        app.composer_editor("fixture", false),
        size,
        cache,
        &mut renderer,
    );
    if let Some(mut stream) = iced_runtime::task::into_stream(task) {
        while let Some(action) = stream.next().await {
            if let iced_runtime::Action::Widget(mut operation) = action {
                ui.operate(&renderer, operation.as_mut());
            }
        }
    }
    let mut messages = Vec::new();
    ui.update(
        &[iced::Event::Keyboard(keyboard::Event::KeyPressed {
            key: keyboard::Key::Character(" ".into()),
            modified_key: keyboard::Key::Character(" ".into()),
            physical_key: keyboard::key::Physical::Code(keyboard::key::Code::Space),
            location: keyboard::Location::Standard,
            modifiers: keyboard::Modifiers::empty(),
            text: Some(" ".into()),
            repeat: false,
        })],
        mouse::Cursor::Unavailable,
        &mut renderer,
        &mut iced::advanced::clipboard::Null,
        &mut messages,
    );
    drop(ui);
    assert!(
        matches!(messages.as_slice(), [Message::Editor(_)]),
        "typing must continue after a mouse acceptance"
    );
    assert_eq!(app.editor.text(), "Compare @\"source notes.md\"");
    assert!(!app.file_completion.is_open());
    dispatch(&mut app, Message::ComposerHistory(Direction::Undo)).await;
    assert_eq!(app.editor.text(), "Compare @source");
    wait_for_picker(&mut app).await;
    dispatch(&mut app, Message::ComposerHistory(Direction::Redo)).await;
    assert_eq!(app.editor.text(), "Compare @\"source notes.md\"");
    for message in messages {
        dispatch(&mut app, message).await;
    }
    assert!(app.editor.text().ends_with(' '));
    let draft = app.editor.text();
    std::fs::remove_file(root.join("source notes.md")).unwrap();
    send_current(&mut app).await;
    dispatch(&mut app, Message::ComposerHistory(Direction::Undo)).await;
    assert!(app.editor.text().is_empty());
    let saved = app
        .harness
        .as_ref()
        .unwrap()
        .store
        .load_session(app.selected.as_ref().unwrap())
        .unwrap();
    assert_eq!(saved.entries[0].text, draft);
    let EntryKind::User { files, .. } = &saved.entries[0].kind else {
        panic!("user entry")
    };
    assert!(files.is_empty());
}

#[test]
fn composer_grows_and_shrinks_within_its_configured_limits() {
    use iced::advanced::{layout, widget::Tree};

    let mut app = app();
    let renderer = header_renderer();
    let measure = |app: &App, width| {
        let mut editor = app.composer_editor("fixture", false);
        let mut tree = Tree::new(editor.as_widget());
        editor
            .as_widget_mut()
            .layout(
                &mut tree,
                &renderer,
                &layout::Limits::new(iced::Size::ZERO, iced::Size::new(width, f32::INFINITY)),
            )
            .size()
            .height
    };
    let assert_height = |actual: f32, expected: f32| {
        assert!(
            (actual - expected).abs() < 0.01,
            "height {actual}, expected {expected}"
        );
    };
    for font_size in [app.catalog.settings.font_size, 20.] {
        app.catalog.settings.font_size = font_size;
        app.editor = text_editor::Content::new();
        let min_height = measure(&app, 400.);
        let line_height = app
            .editor
            .with_editor(|editor| editor.buffer().metrics().line_height);
        assert_eq!(line_height, font_size * COMPOSER_LINE_HEIGHT_FACTOR);
        assert_height(
            min_height,
            COMPOSER_MIN_LINES as f32 * line_height + 2. * crate::growing_editor::PADDING,
        );
        let max_height =
            min_height + (COMPOSER_MAX_LINES - COMPOSER_MIN_LINES) as f32 * line_height;
        let overflow_lines = COMPOSER_MAX_LINES + 7;
        for lines in [
            0,
            1,
            COMPOSER_MIN_LINES,
            COMPOSER_MIN_LINES + 1,
            COMPOSER_MAX_LINES - 1,
            COMPOSER_MAX_LINES,
            overflow_lines,
        ] {
            let value = vec!["line"; lines].join("\n");
            app.editor = text_editor::Content::with_text(&value);
            let visible_lines = lines.clamp(COMPOSER_MIN_LINES, COMPOSER_MAX_LINES);
            assert_height(
                measure(&app, 400.),
                min_height + (visible_lines - COMPOSER_MIN_LINES) as f32 * line_height,
            );
        }
        app.editor
            .perform(text_editor::Action::Move(text_editor::Motion::DocumentEnd));
        assert_height(measure(&app, 400.), max_height);
        assert_eq!(app.editor.line_count(), overflow_lines);
        app.editor.perform(text_editor::Action::Select(
            text_editor::Motion::DocumentStart,
        ));
        app.editor
            .perform(text_editor::Action::Edit(text_editor::Edit::Backspace));
        assert!(app.editor.is_empty());
        assert_height(measure(&app, 400.), min_height);

        let wrapped = "Words that wrap naturally. ".repeat(8);
        app.editor = text_editor::Content::with_text(&wrapped);
        let height = measure(&app, 400.);
        let narrow = measure(&app, 100.);
        let wide = measure(&app, wrapped.chars().count() as f32 * font_size);
        if COMPOSER_MAX_LINES > COMPOSER_MIN_LINES {
            assert!(
                narrow > wide,
                "wrapping grows the composer when its limits allow growth"
            );
        }
        assert!(
            min_height <= wide && wide <= height && height <= narrow && narrow <= max_height + 0.01
        );
        assert_height(wide, min_height);
        assert_eq!(app.editor.text(), wrapped);
    }
}

fn word_delete_events(ctrl_w: bool) -> [iced::Event; 2] {
    let (key, code) = if ctrl_w {
        (
            keyboard::Key::Character("w".into()),
            keyboard::key::Code::KeyW,
        )
    } else {
        (
            keyboard::Key::Named(keyboard::key::Named::Backspace),
            keyboard::key::Code::Backspace,
        )
    };
    [
        iced::Event::Keyboard(keyboard::Event::ModifiersChanged(keyboard::Modifiers::CTRL)),
        iced::Event::Keyboard(keyboard::Event::KeyPressed {
            key: key.clone(),
            modified_key: key,
            physical_key: keyboard::key::Physical::Code(code),
            location: keyboard::Location::Standard,
            modifiers: keyboard::Modifiers::CTRL,
            text: None,
            repeat: false,
        }),
    ]
}

#[test]
fn text_fields_delete_words_or_selection_with_both_shortcuts() {
    use iced::advanced::widget::operation::{focusable, text_input as input};
    let mut renderer = header_renderer();
    for ctrl_w in [false, true] {
        // Cursor/selection columns here are grapheme indices, not bytes.
        for (value, start, end, expected) in [
            ("prefix foo/bar tail", 14, 14, "prefix foo/ tail"),
            ("prefix foo/ tail", 11, 11, "prefix foo tail"),
            ("אחד שני שלישי", 8, 8, "אחד שלישי"),
            ("keep cafe\u{301}  tail", 11, 11, "keep tail"),
            ("prefix foobar tail", 9, 11, "prefix foar tail"),
            ("unchanged", 0, 0, "unchanged"),
        ] {
            let view = pointer_input(
                text_input("", value)
                    .id("field")
                    .on_input(Message::Search)
                    .into(),
                false,
                Default::default(),
            );
            let mut ui = iced_runtime::UserInterface::build(
                view,
                iced::Size::new(400., 40.),
                iced_runtime::user_interface::Cache::new(),
                &mut renderer,
            );
            ui.operate(&renderer, &mut focusable::focus("field".into()));
            ui.operate(
                &renderer,
                &mut input::select_range("field".into(), start, end),
            );
            let mut messages = Vec::new();
            ui.update(
                &word_delete_events(ctrl_w),
                mouse::Cursor::Unavailable,
                &mut renderer,
                &mut iced::advanced::clipboard::Null,
                &mut messages,
            );
            assert!(
                matches!(messages.as_slice(), [Message::Search(text)] if text == expected),
                "Ctrl+{} on {value:?}: {messages:?}",
                if ctrl_w { "W" } else { "Backspace" }
            );
        }
    }
}

#[tokio::test]
async fn ctrl_w_follows_focus_and_repeats_without_rewriting_keys() {
    use iced::advanced::widget::operation::{focusable, text_input as input};
    use keyboard::{
        Key, Modifiers,
        key::{Code, Physical},
    };

    let app = app();
    let content = text_editor::Content::new();
    // Non-Latin layout: shortcut lookup falls back to the physical W key.
    let key = Key::Character("ц".into());
    let physical = Physical::Code(Code::KeyW);
    let recorder = text_editor(&content)
        .id("recorder")
        .on_action(Message::Editor)
        .key_binding(|press| {
            if !matches!(press.status, text_editor::Status::Focused { .. }) {
                return None;
            }
            assert_eq!(press.key, key);
            assert_eq!(press.physical_key, physical);
            Some(text_editor::Binding::Custom(Message::Changed))
        });
    let view = pointer_input(
        widget::column![
            text_input("", "one two three")
                .id("field")
                .on_input(Message::Search),
            app.composer_editor("fixture", false),
            recorder
        ]
        .into(),
        false,
        Default::default(),
    );
    let mut renderer = header_renderer();
    let mut ui = iced_runtime::UserInterface::build(
        view,
        iced::Size::new(400., 300.),
        iced_runtime::user_interface::Cache::new(),
        &mut renderer,
    );
    ui.operate(&renderer, &mut input::move_cursor_to_end("field".into()));
    for focus in ["field", "composer", "recorder"] {
        ui.operate(&renderer, &mut focusable::focus(focus.into()));
        for (extra, repeat) in [
            (Modifiers::NONE, false),
            (Modifiers::NONE, true),
            (Modifiers::SHIFT, false),
            (Modifiers::ALT, false),
            (Modifiers::LOGO, false),
        ] {
            let modifiers = Modifiers::CTRL | extra;
            let mut messages = Vec::new();
            ui.update(
                &[
                    iced::Event::Keyboard(keyboard::Event::ModifiersChanged(modifiers)),
                    iced::Event::Keyboard(keyboard::Event::KeyPressed {
                        key: key.clone(),
                        modified_key: key.clone(),
                        physical_key: physical,
                        location: keyboard::Location::Standard,
                        modifiers,
                        text: None,
                        repeat,
                    }),
                ],
                mouse::Cursor::Unavailable,
                &mut renderer,
                &mut iced::advanced::clipboard::Null,
                &mut messages,
            );
            match focus {
                "recorder" => assert!(matches!(messages.as_slice(), [Message::Changed])),
                _ if extra != Modifiers::NONE => assert!(messages.is_empty(), "{messages:?}"),
                "field" => {
                    let expected = if repeat { "one " } else { "one two " };
                    assert!(matches!(messages.as_slice(), [Message::Search(s)] if s == expected));
                }
                _ => assert!(matches!(messages.as_slice(), [Message::DeletePreviousWord])),
            }
        }
    }
}

fn delete_word_in_composer(app: &mut App, ctrl_w: bool) {
    let mut renderer = header_renderer();
    let messages = {
        let view = pointer_input(
            app.composer_editor("fixture", false),
            false,
            Default::default(),
        );
        let mut ui = iced_runtime::UserInterface::build(
            view,
            iced::Size::new(400., 100.),
            iced_runtime::user_interface::Cache::new(),
            &mut renderer,
        );
        // Focus the real widget, but keep the cursor/selection set by the case.
        ui.update(
            &[iced::Event::Mouse(mouse::Event::ButtonPressed(
                mouse::Button::Left,
            ))],
            mouse::Cursor::Available(iced::Point::new(10., 10.)),
            &mut renderer,
            &mut iced::advanced::clipboard::Null,
            &mut Vec::new(),
        );
        let mut messages = Vec::new();
        ui.update(
            &word_delete_events(ctrl_w),
            mouse::Cursor::Unavailable,
            &mut renderer,
            &mut iced::advanced::clipboard::Null,
            &mut messages,
        );
        messages
    };
    for message in messages {
        let _ = app.update(message);
    }
}

#[tokio::test]
async fn composer_word_deletion_preserves_suffix_selection_and_draft_saving() {
    let mut app = app();
    let root = app.smoke_dir.clone().unwrap();
    stored_session(&mut app, root).await;
    for ctrl_w in [false, true] {
        for (value, line, column, selection, expected) in [
            ("prefix foo/bar tail", 0, 14, None, "prefix foo/ tail"),
            ("prefix foo/ tail", 0, 11, None, "prefix foo tail"),
            ("אחד שני שלישי", 0, "אחד שני ".len(), None, "אחד שלישי"),
            (
                "first\nkeep cafe\u{301}  tail",
                1,
                13,
                None,
                "first\nkeep tail",
            ),
            (
                "prefix foobar tail",
                0,
                9,
                Some((0, 11)),
                "prefix foar tail",
            ),
            ("first\nsecond tail", 1, 0, None, "firstsecond tail"),
            ("unchanged", 0, 0, None, "unchanged"),
            ("", 0, 0, None, ""),
        ] {
            app.editor = text_editor::Content::with_text(value);
            app.editor.move_to(text_editor::Cursor {
                position: text_editor::Position { line, column },
                selection: selection.map(|(line, column)| text_editor::Position { line, column }),
            });
            app.draft_dirty = false;
            delete_word_in_composer(&mut app, ctrl_w);
            assert_eq!(
                app.editor.text(),
                expected,
                "Ctrl+{} on {value:?}",
                if ctrl_w { "W" } else { "Backspace" }
            );
            if expected != value {
                dispatch(&mut app, Message::SaveDraft).await;
                let saved = app
                    .harness
                    .as_ref()
                    .unwrap()
                    .store
                    .load_session(app.selected.as_ref().unwrap())
                    .unwrap();
                assert_eq!(saved.draft, expected, "real edits must survive saving");
            }
        }
    }
    app.editor = text_editor::Content::with_text("must stay");
    app.editor
        .perform(text_editor::Action::Move(text_editor::Motion::DocumentEnd));
    app.io_busy = true;
    delete_word_in_composer(&mut app, true);
    assert_eq!(
        app.editor.text(),
        "must stay",
        "saving must still block edits"
    );
}

#[tokio::test]
async fn composer_toolbar_background_focuses_without_intercepting_buttons() {
    let mut app = app();
    dispatch(&mut app, Message::Create(PathBuf::from("/toolbar-project"))).await;
    type_message(&mut app, "Ready to send").await;
    let mut renderer = header_renderer();
    let mut ui = iced_runtime::UserInterface::build(
        app.center(),
        iced::Size::new(800., 600.),
        iced_runtime::user_interface::Cache::new(),
        &mut renderer,
    );
    let mut bounds = UiBounds::default();
    ui.operate(&renderer, &mut bounds);
    let session = app.current().unwrap();
    let picker = bounds
        .label(&format!(
            "{} {}",
            model(&session.model).unwrap().name,
            effort_name(&session.effort)
        ))
        .center();
    let send = bounds.rightmost_control_at(picker.y).center();
    let background = iced::Point::new((picker.x + send.x) / 2., picker.y);
    assert!(matches!(
        click_at(&mut ui, &mut renderer, background).as_slice(),
        [Message::FocusComposer]
    ));
    assert!(matches!(
        click_at(&mut ui, &mut renderer, picker).as_slice(),
        [Message::ToggleFooterMenu]
    ));
    assert!(matches!(
        click_at(&mut ui, &mut renderer, send).as_slice(),
        [Message::Send(owner)] if owner == &session.id
    ));
}

#[tokio::test]
async fn empty_workspace_welcome_centers_above_composer_and_fits_long_names() {
    use iced_runtime::{UserInterface, user_interface::Cache};

    let mut app = app();
    let path = PathBuf::from("/workspace-path");
    app.catalog.projects = vec![Project {
        path: path.clone(),
        display_name: Some("Custom workspace".into()),
        favorite: false,
        stashed: false,
    }];
    dispatch(&mut app, Message::Create(path)).await;
    let mut renderer = header_renderer();
    for (width, height, name, draft) in [
        (900., 700., "Custom workspace", ""),
        (900., 1000., "Custom workspace", ""),
        (
            360.,
            700.,
            "An unusually long workspace display name with Unicode λ",
            "",
        ),
        (
            900.,
            700.,
            "Custom workspace",
            "A growing draft\nwith multiple\nlines of text\nabove the footer",
        ),
    ] {
        app.catalog.projects[0].display_name = Some(name.into());
        app.editor = text_editor::Content::with_text(draft);
        let mut ui = UserInterface::build(
            app.center(),
            iced::Size::new(width, height),
            Cache::new(),
            &mut renderer,
        );
        let mut bounds = UiBounds::default();
        ui.operate(&renderer, &mut bounds);
        let title = bounds.label("What are we building?");
        let workspace = bounds.label(name);
        let initials = bounds.label(&project_initials(name));
        let badge = *bounds
            .containers
            .iter()
            .filter(|rect| rect.width == rect.height && rect.contains(initials.center()))
            .min_by(|a, b| a.width.total_cmp(&b.width))
            .expect("workspace badge");
        let welcome = bounds
            .containers
            .iter()
            .find(|rect| rect.y == 0. && rect.width == width && rect.height < height)
            .expect("welcome area above the composer");
        let row_left = badge.x.min(workspace.x);
        let row_right = (badge.x + badge.width).max(workspace.x + workspace.width);
        let block_bottom = (badge.y + badge.height).max(workspace.y + workspace.height);
        assert!((title.center_x() - width / 2.).abs() < 1.);
        assert!(((row_left + row_right) / 2. - width / 2.).abs() < 1.);
        assert!(
            ((title.y + block_bottom) / 2. - welcome.center_y()).abs() < 1.,
            "the whole welcome block must center in the space above the composer"
        );
        assert!(workspace.y > title.y + title.height);
        assert!((badge.center_y() - workspace.center_y()).abs() < 1.);
        assert_eq!(initials.center(), badge.center());
        assert!(
            title.height > workspace.height,
            "workspace is a smaller subtitle"
        );
        assert!(row_left >= 0. && row_right <= width, "long names must fit");
    }

    app.draft_session.as_mut().unwrap().entries.push(Entry::new(
        EntryKind::User {
            skills: vec![],
            files: vec![],
            images: vec![],
            sent_at: None,
            steering: false,
        },
        "Start building",
    ));
    let text = displayed_text(app.center(), iced::Size::new(900., 700.));
    assert!(
        !text.contains("What are we building?"),
        "the welcome must disappear once the conversation starts"
    );
}

#[tokio::test]
async fn sidebar_search_shares_the_window_with_composer_completions() {
    let mut app = app();
    let root = app.smoke_dir.clone().unwrap();
    stored_session(&mut app, root).await;
    type_message(&mut app, "/com").await;
    let size = Size::new(app.window_width, app.window_height);
    let mut renderer = header_renderer();
    let mut cache = Default::default();
    let escape = footer_key(keyboard::key::Named::Escape);

    // Escape in the search input closes search, not the visible suggestion.
    drive_ui_messages(
        App::view,
        &mut app,
        &mut renderer,
        &mut cache,
        size,
        vec![Message::ToggleSearch, escape.clone()],
    )
    .await;
    assert!(!app.search_active);
    assert!(app.file_completion.is_open());

    drive_ui_messages(
        App::view,
        &mut app,
        &mut renderer,
        &mut cache,
        size,
        vec![Message::ToggleSearch],
    )
    .await;
    let clicks = {
        let mut ui = iced_runtime::UserInterface::build(app.view(), size, cache, &mut renderer);
        let mut bounds = UiBounds::default();
        ui.operate(&renderer, &mut bounds);
        assert_eq!(bounds.focused, Some("session-search".into()));
        let messages = click_at(&mut ui, &mut renderer, bounds.label("/compact").center());
        cache = ui.into_cache();
        messages
    };
    drive_ui_messages(App::view, &mut app, &mut renderer, &mut cache, size, clicks).await;
    assert_eq!(app.editor.text(), "/compact");
    assert!(app.search_active);

    // Undo restores the partial command. The clicked suggestion returned focus
    // to the composer, so this Escape dismisses only its picker.
    dispatch(&mut app, Message::ComposerHistory(Direction::Undo)).await;
    assert_eq!(app.editor.text(), "/com");
    assert!(app.file_completion.is_open());
    drive_ui_messages(
        App::view,
        &mut app,
        &mut renderer,
        &mut cache,
        size,
        vec![escape],
    )
    .await;
    assert!(app.search_active);
    assert!(!app.file_completion.is_open());
}

use super::*;

#[tokio::test]
async fn delayed_harness_changes_refresh_only_live_parts_of_a_smoke_frame() {
    let mut app = app();
    let root = app.smoke_dir.clone().unwrap();
    stored_session(&mut app, root.clone()).await;
    let owner = app.selected.clone().unwrap();
    let harness = app.harness.as_ref().unwrap().clone();
    // The capture index is for sequencing screenshots, not choosing a data source.
    app.smoke_stage = usize::MAX;

    harness
        .edit(&owner, |s| s.title = "Live edit".into())
        .unwrap();
    let request = r#"{"questions":[{"id":"q","kind":"text","question":"Which project?"}]}"#;
    let conversation = harness.get(&owner).unwrap();
    let queue = conversation.questionnaires.as_ref().unwrap();
    let cancel = CancellationToken::new();
    let mut ask = std::pin::pin!(queue.ask("real-ask", request, &cancel));
    // Register the real question, but deliver its notice after staging the fixture.
    assert!(iced::futures::poll!(&mut ask).is_pending());
    app.stage_smoke_questionnaire(orbit_core::questionnaire::Request::parse(request).unwrap());
    let question = app.active_questionnaire.clone().unwrap();
    dispatch(&mut app, Message::Changed).await;
    assert_eq!(app.active_questionnaire.as_ref(), Some(&question));
    assert!(app.questionnaires.contains_key(&question));
    assert_eq!(app.current().unwrap().title, "Live edit");

    let fixture = Session::new(root, &app.catalog.settings);
    let fixture_id = fixture.id.clone();
    app.stage_smoke_transcript(fixture);
    harness
        .edit(&owner, |s| s.title = "New sidebar title".into())
        .unwrap();
    dispatch(&mut app, Message::Changed).await;
    assert_eq!(app.current().unwrap().id, fixture_id);
    assert!(
        app.sessions
            .iter()
            .any(|s| s.id == owner && s.title == "New sidebar title")
    );

    dispatch(&mut app, Message::Select(owner.clone())).await;
    assert_eq!(app.current().unwrap().id, owner);
    assert_eq!(app.current().unwrap().title, "New sidebar title");
    assert_eq!(app.active_questionnaire.as_deref(), Some("real-ask"));
    assert!(!app.questionnaires.contains_key(&question));
    queue.submit("real-ask", vec!["Orbit".into()]).unwrap();
    ask.await.unwrap();
}

#[tokio::test]
async fn window_subscription_ignores_motion_and_redraws() {
    use iced::futures::{StreamExt, stream};
    use iced_runtime::futures::subscription;
    let events = [
        iced::Event::Mouse(mouse::Event::CursorMoved {
            position: iced::Point::new(125., 77.),
        }),
        iced::Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
        iced::Event::Window(window::Event::RedrawRequested(std::time::Instant::now())),
        iced::Event::Window(window::Event::CloseRequested),
    ];
    let mut messages = Vec::new();
    for recipe in subscription::into_recipes(window_events()) {
        let input = stream::iter(
            events
                .clone()
                .map(|event| subscription::Event::Interaction {
                    window: window::Id::unique(),
                    event,
                    status: iced::event::Status::Ignored,
                }),
        )
        .boxed();
        messages.extend(recipe.stream(input).collect::<Vec<_>>().await);
    }
    assert!(matches!(
        messages.as_slice(),
        [Message::Event(iced::Event::Window(
            window::Event::CloseRequested
        ))]
    ));
}

#[tokio::test]
async fn escape_dismisses_ui_before_stopping_only_the_selected_running_agent() {
    let mut app = app();
    let path = app.smoke_dir.clone().unwrap();
    stored_session(&mut app, path.clone()).await;
    let h = app.harness.as_ref().unwrap();
    let selected = h.get(app.selected.as_ref().unwrap()).unwrap();
    let other = h.insert(Session::new(path, &app.catalog.settings));
    let escape = Message::Event(iced::Event::Keyboard(keyboard::Event::KeyPressed {
        key: keyboard::Key::Named(keyboard::key::Named::Escape),
        modified_key: keyboard::Key::Named(keyboard::key::Named::Escape),
        physical_key: keyboard::key::Physical::Code(keyboard::key::Code::Escape),
        location: keyboard::Location::Standard,
        modifiers: keyboard::Modifiers::empty(),
        text: None,
        repeat: false,
    }));
    let idle_status = selected.status();
    dispatch(&mut app, escape.clone()).await;
    assert_eq!(selected.status(), idle_status);

    selected.running.store(true, Ordering::Release);
    other.running.store(true, Ordering::Release);
    app.settings_open = true;
    dispatch(&mut app, Message::ToggleSearch).await;
    dispatch(&mut app, Message::Search("no matches".into())).await;
    app.modal = Some(Modal::AddProject);
    app.context_menu = Some(ContextMenu(
        MenuTarget::Session(other.id.clone()),
        iced::Point::ORIGIN,
    ));
    dispatch(&mut app, escape.clone()).await;
    assert!(app.context_menu.is_none());
    assert!(app.modal.is_some());
    assert!(app.settings_open);
    assert_eq!(selected.status(), idle_status);
    dispatch(&mut app, escape.clone()).await;
    assert!(app.modal.is_none());
    assert!(app.search_active);
    assert!(app.settings_open);
    assert_eq!(selected.status(), idle_status);
    dispatch(&mut app, escape.clone()).await;
    assert!(!app.search_active);
    assert!(app.search.is_empty());
    assert!(app.settings_open);
    assert_eq!(selected.status(), idle_status);
    dispatch(&mut app, escape.clone()).await;
    assert!(!app.settings_open);
    assert_eq!(selected.status(), idle_status);
    let other_status = other.status();
    // Hold an earlier session job open. Both stop controls must return to
    // the event loop without overtaking it or disabling unrelated input.
    for (message, target) in [
        (escape, &selected),
        (Message::StopAgent(other.id.clone()), &other),
    ] {
        let (release, gate) = std::sync::mpsc::channel();
        let barrier = app.io.run(move || {
            gate.recv_timeout(Duration::from_secs(5))
                .expect("UI stayed responsive");
            Message::External(Ok(()))
        });
        let status = target.status();
        let stop = app.update(message);
        let was_open = app.search_active;
        dispatch(&mut app, Message::ToggleSearch).await;
        assert_ne!(app.search_active, was_open);
        assert_eq!(target.status(), status, "Stop overtook an earlier job");
        release.send(()).unwrap();
        drain(&mut app, Task::batch([barrier, stop])).await;
        assert_eq!(target.status(), ConversationStatus::Stopping);
        if target.id == selected.id {
            assert_eq!(other.status(), other_status);
        }
    }
    selected.running.store(false, Ordering::Release);
    other.running.store(false, Ordering::Release);
}

#[tokio::test]
async fn pending_transitions_block_stale_edits_autosaves_and_measurements() {
    let mut app = app();
    let path = app.smoke_dir.clone().unwrap();
    stored_session(&mut app, path.clone()).await;
    let first = app.selected.clone().unwrap();
    dispatch(
        &mut app,
        Message::Editor(text_editor::Action::Edit(text_editor::Edit::Insert('a'))),
    )
    .await;
    let task = app.update(Message::Create(path));
    assert!(app.io_busy);
    dispatch(
        &mut app,
        Message::Editor(text_editor::Action::Edit(text_editor::Edit::Insert('b'))),
    )
    .await;
    assert_eq!(app.editor.text(), "a");
    let old_epoch = app.view_epoch;
    drain(&mut app, task).await;
    let offset = app.transcript.offset;
    dispatch(&mut app, Message::Scroll(old_epoch, 200000., 300., false)).await;
    dispatch(&mut app, Message::RowSize(old_epoch, 0, 100000.)).await;
    assert_eq!(app.transcript.offset, offset);
    dispatch(&mut app, Message::Select(first.clone())).await;
    assert_eq!(app.editor.text(), "a");
    let send = app.update(Message::Send);
    dispatch(&mut app, Message::SaveDraft).await;
    dispatch(
        &mut app,
        Message::Editor(text_editor::Action::Edit(text_editor::Edit::Insert('c'))),
    )
    .await;
    assert_eq!(app.editor.text(), "a");
    drain(&mut app, send).await;
    assert!(app.editor.text().is_empty());
    assert!(
        app.harness
            .as_ref()
            .unwrap()
            .store
            .load_session(&first)
            .unwrap()
            .draft
            .is_empty()
    );
}
#[tokio::test]
async fn shutdown_waits_for_submission_and_reports_catalog_failure() {
    let mut app = app();
    let path = app.smoke_dir.clone().unwrap();
    dispatch(&mut app, Message::Create(path)).await;
    dispatch(
        &mut app,
        Message::Editor(text_editor::Action::Edit(text_editor::Edit::Insert('x'))),
    )
    .await;
    let send = app.update(Message::Send);
    let _ = app.quit();
    assert!(app.close_requested);
    let settings = app
        .harness
        .as_ref()
        .unwrap()
        .store
        .root()
        .join("settings.json");
    std::fs::remove_file(&settings).unwrap();
    std::fs::create_dir(&settings).unwrap();
    drain(&mut app, send).await;
    assert!(!app.closing);
    assert!(
        app.error
            .as_ref()
            .unwrap()
            .contains("Could not finish saving")
    );
    assert!(
        app.harness
            .as_ref()
            .unwrap()
            .all()
            .iter()
            .all(|c| !c.is_running())
    );
    std::fs::remove_dir(settings).unwrap();
    let quit = app.quit();
    drain(&mut app, quit).await;
    assert!(app.closing);
}

#[tokio::test]
async fn change_stream_delivers_pre_subscription_and_coalesced_updates() {
    use iced::futures::StreamExt;
    let mut app = app();
    let path = app.smoke_dir.clone().unwrap();
    stored_session(&mut app, path).await;
    let id = app.selected.clone().unwrap();
    let h = app.harness.as_ref().unwrap().clone();
    h.edit(&id, |s| s.title = "before subscription".into())
        .unwrap();
    let mut stream = updates(&Updates(h.clone()));
    let event = tokio::time::timeout(Duration::from_secs(1), stream.next())
        .await
        .unwrap()
        .unwrap();
    dispatch(&mut app, event).await;
    assert_eq!(app.current().unwrap().title, "before subscription");
    h.edit(&id, |s| s.title = "first change".into()).unwrap();
    let event = tokio::time::timeout(Duration::from_secs(1), stream.next())
        .await
        .unwrap()
        .unwrap();
    h.edit(&id, |s| s.title = "latest change while event queued".into())
        .unwrap();
    dispatch(&mut app, event).await;
    assert_eq!(
        app.current().unwrap().title,
        "latest change while event queued"
    );
    // A refresh may acknowledge the newer revision too. Permit either
    // coalescing it away or delivering one already-queued notification.
    if let Ok(Some(event)) = tokio::time::timeout(Duration::from_millis(80), stream.next()).await {
        dispatch(&mut app, event).await;
    }
    assert!(
        !app.refresh(),
        "unchanged sessions should not be resnapshotted"
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(80), stream.next())
            .await
            .is_err(),
        "idle subscription must stay asleep"
    );
}

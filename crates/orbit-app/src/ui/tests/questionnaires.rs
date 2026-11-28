use super::*;

#[tokio::test]
async fn questionnaires_preserve_session_drafts_queue_and_modal_priority() {
    let mut app = app();
    let path = app.smoke_dir.clone().unwrap();
    stored_session(&mut app, path.clone()).await;
    type_message(&mut app, "Unsent composer draft").await;
    let owner = app.selected.clone().unwrap();
    let h = app.harness.as_ref().unwrap().clone();
    let service = h.get(&owner).unwrap().questionnaires.clone().unwrap();
    let mut tasks = vec![];
    let cancel = CancellationToken::new();
    for id in ["first-ask", "second-ask"] {
        let service = service.clone();
        let cancel = cancel.clone();
        tasks.push(tokio::spawn(async move {
            service
                .ask(
                    id,
                    r#"{"questions":[{"id":"q","question":"Required answer?","kind":"text"}]}"#,
                    &cancel,
                )
                .await
        }));
        tokio::time::timeout(Duration::from_secs(2), async {
            while !app.questionnaires.contains_key(id) {
                dispatch(&mut app, Message::Changed).await;
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }
    assert_eq!(app.active_questionnaire.as_deref(), Some("first-ask"));
    assert!(
        app.sidebar_rows
            .iter()
            .any(|row| matches!(row, SidebarRow::Heading("NEEDS ATTENTION", 1)))
    );
    let q = |id: &str, action| {
        Message::Questionnaire(
            id.into(),
            questionnaire::Message {
                question: 0,
                action,
            },
        )
    };
    dispatch(
        &mut app,
        q(
            "first-ask",
            questionnaire::Action::Editor(text_editor::Action::Edit(text_editor::Edit::Paste(
                Arc::new("Answer draft λ".into()),
            ))),
        ),
    )
    .await;
    dispatch(&mut app, q("first-ask", questionnaire::Action::Toggle)).await;
    stored_session(&mut app, path).await;
    assert!(app.active_questionnaire.is_none());
    type_message(&mut app, "Another session's draft").await;
    dispatch(&mut app, Message::Select(owner.clone())).await;
    assert_eq!(app.editor.text(), "Unsent composer draft");
    assert!(app.questionnaires["first-ask"].collapsed);
    dispatch(&mut app, q("first-ask", questionnaire::Action::Toggle)).await;
    // The ordinary composer must neither mutate nor submit while an ask is open.
    type_message(&mut app, "Should not be pasted").await;
    send_current(&mut app).await;
    assert_eq!(app.editor.text(), "Unsent composer draft");
    app.modal = Some(Modal::Rename(owner.clone()));
    dispatch(&mut app, q("first-ask", questionnaire::Action::Next)).await;
    assert_eq!(service.pending().len(), 2);
    dispatch(
        &mut app,
        Message::Event(iced::Event::Keyboard(keyboard::Event::KeyPressed {
            key: keyboard::Key::Named(keyboard::key::Named::Escape),
            modified_key: keyboard::Key::Named(keyboard::key::Named::Escape),
            physical_key: keyboard::key::Physical::Code(keyboard::key::Code::Escape),
            location: keyboard::Location::Standard,
            modifiers: keyboard::Modifiers::empty(),
            text: None,
            repeat: false,
        })),
    )
    .await;
    assert!(app.modal.is_none());
    assert_eq!(service.pending().len(), 2);
    // Opening Find by shortcut (rather than clicking outside the panel) must
    // release questionnaire keyboard focus without disabling its buttons.
    let mut renderer = header_renderer();
    let size = Size::new(830., 600.);
    let cache =
        iced_runtime::UserInterface::build(app.center(), size, Default::default(), &mut renderer)
            .into_cache();
    let open = app.update(Message::Find(find::Action::Open));
    let messages = {
        use iced::futures::StreamExt;
        let mut ui = iced_runtime::UserInterface::build(app.center(), size, cache, &mut renderer);
        let mut stream = iced_runtime::task::into_stream(open).unwrap();
        while let Some(action) = stream.next().await {
            if let iced_runtime::Action::Widget(mut op) = action {
                ui.operate(&renderer, op.as_mut());
            }
        }
        let mut bounds = UiBounds::default();
        ui.operate(&renderer, &mut bounds);
        assert_eq!(bounds.focused, Some("conversation-find".into()));
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
        assert!(
            !messages
                .iter()
                .any(|m| matches!(m, Message::Questionnaire(..))),
            "Find's Enter must not submit an answer"
        );
        click_at(&mut ui, &mut renderer, bounds.label("Submit").center())
    };
    for message in messages {
        dispatch(&mut app, message).await
    }
    assert!(
        app.find.is_some(),
        "answering doesn't dismiss the floating popup"
    );
    assert_eq!(app.active_questionnaire.as_deref(), Some("second-ask"));
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&tasks.remove(0).await.unwrap().unwrap())
            .unwrap(),
        serde_json::json!({"answers":[{"id":"q","answer":"Answer draft λ"}]})
    );
    cancel.cancel();
    assert!(tasks.remove(0).await.unwrap().is_err());
    dispatch(&mut app, Message::Changed).await;
    assert!(app.active_questionnaire.is_none());
    assert!(app.questionnaires.is_empty());
    assert_eq!(app.editor.text(), "Unsent composer draft");
}

#[tokio::test]
async fn sidebar_search_leaves_questionnaire_interactive() {
    let mut app = app();
    stored_session(&mut app, PathBuf::from("/project")).await;
    dispatch(&mut app, Message::ToggleSearch).await;
    app.stage_smoke_questionnaire(
        orbit_core::questionnaire::Request::parse(
            r#"{"questions":[{"id":"target","question":"Which target?","kind":"select","options":["Desktop","Mobile"]},{"id":"notes","question":"Any constraints?","kind":"text"}]}"#,
        )
        .unwrap(),
    );

    let mut renderer = header_renderer();
    let size = Size::new(830., 600.);
    let (cache, selection) = {
        let mut ui = iced_runtime::UserInterface::build(
            app.center(),
            size,
            Default::default(),
            &mut renderer,
        );
        let mut bounds = UiBounds::default();
        ui.operate(&renderer, &mut bounds);
        let selection = click_at(&mut ui, &mut renderer, bounds.label("Mobile").center());
        (ui.into_cache(), selection)
    };
    for message in selection {
        dispatch(&mut app, message).await;
    }

    // Enter now belongs to the clicked panel, not the still-open sidebar.
    let mut messages = Vec::new();
    {
        let mut ui = iced_runtime::UserInterface::build(app.center(), size, cache, &mut renderer);
        let Message::Event(enter) = footer_key(keyboard::key::Named::Enter) else {
            unreachable!()
        };
        ui.update(
            &[enter],
            mouse::Cursor::Unavailable,
            &mut renderer,
            &mut iced::advanced::clipboard::Null,
            &mut messages,
        );
    }
    for message in messages {
        dispatch(&mut app, message).await;
    }
    assert!(app.search_active);
    assert!(chat_text(&app).contains("Any constraints?"));
}
#[tokio::test]
async fn blocking_questionnaire_preempts_an_async_draft_and_dismiss_does_not_send() {
    use orbit_core::questionnaire::{Pending, Request};
    let mut app = app();
    stored_session(&mut app, PathBuf::from("/project")).await;
    type_message(&mut app, "Unsent composer text").await;
    let owner = app.selected.clone().unwrap();
    let h = app.harness.clone().unwrap();
    let args = r#"{"questions":[{"id":"notes","question":"Any notes?","kind":"text"}]}"#;
    h.edit(&owner, |s| {
        // Fixture for a session with an already accepted tool call.
        let entry = Entry::new(
            EntryKind::Tool {
                name: "ask_user_async".into(),
                input: args.into(),
                output: r#"{"accepted":true}"#.into(),
                complete: true,
                failed: false,
                command_status: None,
                cell_id: None,
                parent: None,
                media: vec![],
                web: None,
            },
            "",
        );
        for entry in [Entry::new(entry.kind.clone(), ""), entry] {
            s.async_questions.push(Pending {
                id: entry.id.clone(),
                request: Arc::new(Request::parse(args).unwrap()),
            });
            s.entries.push(entry);
        }
    })
    .unwrap();
    dispatch(&mut app, Message::Changed).await;
    let id = app.active_questionnaire.clone().unwrap();
    let action = |action| {
        Message::Questionnaire(
            id.clone(),
            questionnaire::Message {
                question: 0,
                action,
            },
        )
    };
    dispatch(
        &mut app,
        action(questionnaire::Action::Editor(text_editor::Action::Edit(
            text_editor::Edit::Paste(Arc::new("Draft answer".into())),
        ))),
    )
    .await;
    dispatch(&mut app, action(questionnaire::Action::Toggle)).await;
    stored_session(&mut app, PathBuf::from("/elsewhere")).await;
    assert!(
        h.get(&owner).is_none(),
        "keeping a UI draft must not pin the runtime"
    );
    dispatch(&mut app, Message::Select(owner.clone())).await;
    let c = h.get(&owner).unwrap();
    let q = c.questionnaires.clone().unwrap();
    let cancel = CancellationToken::new();
    let mut ask = Box::pin(q.ask("blocking", args, &cancel));
    assert!(iced::futures::poll!(&mut ask).is_pending());
    dispatch(&mut app, Message::Changed).await;
    assert_eq!(app.active_questionnaire.as_deref(), Some("blocking"));
    dispatch(&mut app, action(questionnaire::Action::Dismiss)).await; // stale panel event
    cancel.cancel();
    assert!(ask.await.is_err());
    dispatch(&mut app, Message::Changed).await;
    assert_eq!(app.active_questionnaire.as_ref(), Some(&id));
    assert!(app.questionnaires[&id].collapsed);
    assert!(chat_text(&app).contains("Dismiss"));
    dispatch(&mut app, action(questionnaire::Action::Toggle)).await;
    dispatch(&mut app, action(questionnaire::Action::Next)).await;
    let second = app
        .active_questionnaire
        .clone()
        .expect("second async request");
    assert_ne!(second, id);
    dispatch(
        &mut app,
        Message::Questionnaire(
            second,
            questionnaire::Message {
                question: 0,
                action: questionnaire::Action::Dismiss,
            },
        ),
    )
    .await;
    assert!(app.active_questionnaire.is_none());
    assert_eq!(app.editor.text(), "Unsent composer text");
    let saved = h.store.load_session(&owner).unwrap();
    assert!(saved.async_questions.is_empty());
    let replies: Vec<_> = saved
        .entries
        .iter()
        .filter(|e| matches!(e.kind, EntryKind::User { .. }))
        .collect();
    assert_eq!(replies.len(), 1, "dismiss must not send another message");
    let index = saved.entries.index_of(&replies[0].id).unwrap();
    let body = "Any notes?\nAnswer\nDraft answer";
    let card = displayed_text(app.entry_view(replies[0], index), Size::new(320., 600.));
    assert!(card.contains("Your answers") && card.contains(body));
    assert!(!card.contains(&id) && !card.contains("[notes]"));
    dispatch(&mut app, Message::Find(find::Action::Open)).await;
    for (query, expected) in [(&*id, None), ("Draft answer", body.find("Draft answer"))] {
        dispatch(&mut app, Message::Find(find::Action::Query(query.into()))).await;
        let paint = app
            .find
            .as_ref()
            .unwrap()
            .paint(index, &app.current().unwrap().entries[index]);
        assert_eq!(paint.active.map(|hit| hit.start), expected);
    }
    {
        let mut selection = app.chat_selection.borrow_mut();
        let at = |byte| Position {
            entry: index,
            block: 0,
            byte,
        };
        selection.anchor = Some(at(0));
        selection.focus = Some(at(body.len()));
    }
    assert_eq!(
        copied_text(app.update_inner(Message::CopySelection)).await,
        body
    );
}

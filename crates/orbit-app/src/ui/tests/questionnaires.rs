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
    dispatch(&mut app, Message::Send).await;
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

use super::*;

#[tokio::test]
async fn completion_dot_preserves_background_sessions_and_clears_on_open() {
    use iced_runtime::{UserInterface, user_interface::Cache};
    use orbit_core::agent::subagents::{Child, Profile};
    let mut renderer = header_renderer();
    let mut has_age_label = |app: &App| {
        let mut ui = UserInterface::build(
            app.sidebar(),
            iced::Size::new(346., 400.),
            Cache::new(),
            &mut renderer,
        );
        let mut bounds = UiBounds::default();
        ui.operate(&renderer, &mut bounds);
        bounds
            .labels
            .iter()
            .any(|(text, _)| text == &age(app.sessions[0].updated))
    };
    let mut app = app();
    let path = app.smoke_dir.clone().unwrap();
    stored_session(&mut app, path.clone()).await;
    let parent = app.selected.clone().unwrap();
    let h = app.harness.as_ref().unwrap().clone();
    h.edit(&parent, |s| s.unchecked_completion = true).unwrap();
    dispatch(&mut app, Message::Changed).await;
    assert_eq!(
        app.session_status(&app.sessions[0]),
        SessionStatus::Unchecked,
        "refresh while unfocused must retain the completion flag"
    );

    assert!(
        !has_age_label(&app),
        "the completion dot replaces the age label"
    );

    let mut child = Session::new(path, &app.catalog.settings);
    child.child = Some(Child {
        parent_id: parent.clone(),
        profile: Profile::Explore,
    });
    let child_id = child.id.clone();
    h.store.save_session(&child).unwrap();
    h.insert(child);
    dispatch(&mut app, Message::Select(child_id)).await;
    dispatch(
        &mut app,
        Message::Event(iced::Event::Window(window::Event::Focused)),
    )
    .await;
    assert!(
        h.store.load_session(&parent).unwrap().unchecked_completion,
        "opening or focusing a child does not check its parent"
    );
    dispatch(
        &mut app,
        Message::Event(iced::Event::Window(window::Event::Unfocused)),
    )
    .await;
    dispatch(&mut app, Message::Select(parent.clone())).await;
    assert!(
        !h.store.load_session(&parent).unwrap().unchecked_completion,
        "opening clears and saves the dot, regardless of keyboard focus"
    );
    let summary = app.sessions.iter().find(|s| s.id == parent).unwrap();
    assert_eq!(app.session_status(summary), SessionStatus::Idle);
    assert!(
        has_age_label(&app),
        "opening the session restores its age label"
    );
    let restarted = Harness::new(h.store.clone(), h.store.load().unwrap().1).unwrap();
    assert!(
        !restarted
            .load(&parent)
            .unwrap()
            .snapshot()
            .unchecked_completion
    );
}

#[tokio::test]
async fn completion_dot_clears_on_focus_and_when_completing_while_focused() {
    let mut app = app();
    let path = app.smoke_dir.clone().unwrap();
    stored_session(&mut app, path.clone()).await;
    let background = app.selected.clone().unwrap();
    stored_session(&mut app, path).await;
    let selected = app.selected.clone().unwrap();
    let h = app.harness.as_ref().unwrap().clone();
    for id in [&background, &selected] {
        h.edit(id, |s| s.unchecked_completion = true).unwrap();
    }
    dispatch(&mut app, Message::Changed).await;
    assert!(app.current().unwrap().unchecked_completion);
    dispatch(
        &mut app,
        Message::Event(iced::Event::Window(window::Event::Focused)),
    )
    .await;
    assert!(!app.current().unwrap().unchecked_completion);
    assert!(
        !h.store
            .load_session(&selected)
            .unwrap()
            .unchecked_completion
    );
    assert!(
        h.store
            .load_session(&background)
            .unwrap()
            .unchecked_completion
    );

    h.edit(&selected, |s| s.unchecked_completion = true)
        .unwrap();
    dispatch(&mut app, Message::Changed).await;
    assert!(!app.current().unwrap().unchecked_completion);
    assert!(
        !h.store
            .load_session(&selected)
            .unwrap()
            .unchecked_completion
    );
    assert!(
        h.store
            .load_session(&background)
            .unwrap()
            .unchecked_completion
    );
    let summary = app.sessions.iter().find(|s| s.id == selected).unwrap();
    assert_eq!(app.session_status(summary), SessionStatus::Idle);

    dispatch(
        &mut app,
        Message::Event(iced::Event::Window(window::Event::Unfocused)),
    )
    .await;
    h.edit(&selected, |s| s.unchecked_completion = true)
        .unwrap();
    dispatch(&mut app, Message::Changed).await;
    assert!(app.current().unwrap().unchecked_completion);
    assert!(
        h.store
            .load_session(&selected)
            .unwrap()
            .unchecked_completion
    );
}

#[tokio::test]
async fn stopped_attention_clears_on_escape_or_revisit_not_on_refresh() {
    let mut app = app();
    let path = app.smoke_dir.clone().unwrap();
    stored_session(&mut app, path.clone()).await;
    let id = app.selected.clone().unwrap();
    let h = app.harness.as_ref().unwrap().clone();
    let fail = async || {
        // The isolated store has no login: exercise a real terminal transition.
        let mut changes = h.subscribe();
        h.start(&id, Settings::default()).unwrap();
        let c = h.get(&id).unwrap();
        while c.is_executing() {
            tokio::time::timeout(Duration::from_secs(3), changes.changed())
                .await
                .unwrap()
                .unwrap();
        }
        assert_eq!(c.status(), ConversationStatus::NeedsAttention);
    };
    fail().await;
    let notice = h
        .get(&id)
        .unwrap()
        .snapshot()
        .entries
        .iter()
        .last()
        .unwrap()
        .text
        .clone();
    assert!(notice.contains("Sign in"));
    dispatch(&mut app, Message::Changed).await;
    assert!(app.current().unwrap().needs_attention);
    assert!(
        app.sidebar_rows
            .iter()
            .any(|row| matches!(row, SidebarRow::Heading("NEEDS ATTENTION", 1)))
    );
    app.modal = Some(Modal::Rename(id.clone()));
    dispatch(&mut app, footer_key(keyboard::key::Named::Escape)).await;
    assert!(app.modal.is_none());
    assert!(h.get(&id).unwrap().snapshot().needs_attention);
    dispatch(&mut app, footer_key(keyboard::key::Named::Escape)).await;
    assert!(!app.current().unwrap().needs_attention);
    assert!(
        !app.sidebar_rows
            .iter()
            .any(|row| matches!(row, SidebarRow::Heading("NEEDS ATTENTION", _)))
    );
    assert!(!h.store.load_session(&id).unwrap().needs_attention);
    assert_eq!(
        app.current().unwrap().entries.iter().last().unwrap().text,
        notice
    );

    fail().await;
    dispatch(&mut app, Message::Changed).await;
    dispatch(&mut app, Message::Selected(Err("Selection failed".into()))).await;
    assert!(h.get(&id).unwrap().snapshot().needs_attention);
    stored_session(&mut app, path).await;
    assert!(h.store.load_session(&id).unwrap().needs_attention);
    dispatch(&mut app, Message::Select(id.clone())).await;
    assert!(!app.current().unwrap().needs_attention);
    assert!(!h.store.load_session(&id).unwrap().needs_attention);
    assert!(
        !app.sidebar_rows
            .iter()
            .any(|row| matches!(row, SidebarRow::Heading("NEEDS ATTENTION", _)))
    );
}

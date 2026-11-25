use super::*;
use orbit_core::models::ASTRA;

#[tokio::test]
async fn composer_history_follows_drafts_and_survives_failed_send() {
    let mut app = app();
    let root = app.smoke_dir.clone().unwrap();
    dispatch(&mut app, Message::Create(root.clone())).await;
    let insert = |c| Message::Editor(text_editor::Action::Edit(text_editor::Edit::Insert(c)));
    dispatch(&mut app, insert('λ')).await;
    stored_session(&mut app, root.clone()).await;
    let id = app.selected.clone().unwrap();
    dispatch(&mut app, insert('界')).await;
    dispatch(&mut app, Message::Create(root.join("other"))).await;
    assert_eq!(app.editor.text(), "λ");
    dispatch(&mut app, Message::ComposerHistory(Direction::Undo)).await;
    assert!(app.editor.is_empty());
    dispatch(&mut app, Message::Select(id.clone())).await;
    assert_eq!(app.editor.text(), "界");
    let sent = app.composer_message();
    dispatch(
        &mut app,
        Message::Sent(id, sent, Err("fixture error".into())),
    )
    .await;
    dispatch(&mut app, Message::ComposerHistory(Direction::Undo)).await;
    assert!(app.editor.is_empty());
    dispatch(&mut app, Message::Create(root)).await;
    dispatch(&mut app, Message::ComposerHistory(Direction::Redo)).await;
    assert_eq!(app.editor.text(), "λ");
    assert_eq!(app.new_draft.message, "λ");
}

#[tokio::test]
async fn image_drafts_survive_navigation_storage_and_failed_image_only_send() {
    let mut app = app();
    let root = app.smoke_dir.clone().unwrap();
    let h = app.harness.as_ref().unwrap().clone();
    dispatch(&mut app, Message::Create(root.clone())).await;
    let source = root.join("pasted.png");
    std::fs::write(&source, include_bytes!("../../fixtures/media.png")).unwrap();
    dispatch(
        &mut app,
        Message::Event(iced::Event::Window(window::Event::FileDropped(
            source.clone(),
        ))),
    )
    .await;
    std::fs::remove_file(&source).unwrap();
    let images = app.attachments.clone();
    assert_eq!(images.len(), 1);
    assert!(
        images[0].path.is_file(),
        "attachment is independent of its source"
    );
    dispatch(&mut app, Message::SaveDraft).await;
    assert_eq!(h.store.load_new_draft().unwrap().images, images);
    stored_session(&mut app, root.clone()).await;
    let existing = app.selected.clone().unwrap();
    assert!(app.attachments.is_empty());
    dispatch(&mut app, Message::Create(root.clone())).await;
    assert_eq!(app.attachments, images);
    let _ = app.update(Message::Paste);
    dispatch(
        &mut app,
        Message::AttachmentLoaded(Ok(crate::attachments::Input::Image(images[0].clone()))),
    )
    .await;
    assert_eq!(app.attachments.len(), 2);
    dispatch(&mut app, Message::RemoveAttachment(0)).await;
    assert_eq!(app.attachments, images);
    dispatch(&mut app, Message::OpenImage(images[0].clone())).await;
    assert!(matches!(app.modal, Some(Modal::Image(_))));
    dispatch(&mut app, Message::RemoveAttachment(0)).await;
    assert_eq!(app.attachments, images, "modal blocks background edits");
    dispatch(&mut app, Message::Dismiss).await;
    let db = rusqlite::Connection::open(h.store.root().join("sessions.sqlite3")).unwrap();
    sqlite::fail_writes(&db, "sessions", "image fixture failure");
    send_current(&mut app).await;
    assert!(
        app.error
            .as_ref()
            .unwrap()
            .contains("image fixture failure")
    );
    assert_eq!(app.attachments, images);
    sqlite::allow_writes(&db, "sessions");
    send_current(&mut app).await;
    assert!(app.attachments.is_empty());
    let saved = h
        .store
        .load_session(app.selected.as_ref().unwrap())
        .unwrap();
    assert_eq!(saved.entries[0].text, "");
    assert!(
        matches!(&saved.entries[0].kind, EntryKind::User { images: saved_images, .. } if saved_images == &images)
    );
    assert!(h.store.load_new_draft().unwrap().images.is_empty());
    dispatch(&mut app, Message::Select(existing.clone())).await;
    let _ = app.update(Message::Paste);
    dispatch(
        &mut app,
        Message::AttachmentLoaded(Ok(crate::attachments::Input::Image(images[0].clone()))),
    )
    .await;
    dispatch(&mut app, Message::New).await;
    assert_eq!(
        h.store.load_session(&existing).unwrap().draft_images,
        images
    );
    dispatch(&mut app, Message::Select(existing)).await;
    assert_eq!(app.attachments, images);
    dispatch(&mut app, Message::RemoveAttachment(0)).await;
    dispatch(&mut app, Message::SaveDraft).await;
    assert!(
        h.store
            .load_session(app.selected.as_ref().unwrap())
            .unwrap()
            .draft_images
            .is_empty()
    );
}

#[tokio::test]
async fn multiple_file_drops_are_ordered_and_cannot_leak_into_another_session() {
    let mut app = app();
    let root = app.smoke_dir.clone().unwrap();
    stored_session(&mut app, root.clone()).await;
    let id = app.selected.clone();
    let png = root.join("first.png");
    let gif = root.join("second.gif");
    std::fs::write(&png, include_bytes!("../../fixtures/media.png")).unwrap();
    std::fs::write(&gif, include_bytes!("../../fixtures/media.gif")).unwrap();
    let first = app.update(Message::Event(iced::Event::Window(
        window::Event::FileDropped(png),
    )));
    let second = app.update(Message::Event(iced::Event::Window(
        window::Event::FileDropped(gif),
    )));
    let mut renderer = header_renderer();
    let mut composer_bounds = |app: &App| {
        let mut ui = iced_runtime::UserInterface::build(
            app.center(),
            Size::new(930., 800.),
            Default::default(),
            &mut renderer,
        );
        let mut bounds = UiBounds::default();
        ui.operate(&renderer, &mut bounds);
        bounds
    };
    let bounds = composer_bounds(&app);
    let queued = bounds.label("Queued").center();
    let queued_tile = bounds
        .containers
        .iter()
        .filter(|r| r.contains(queued) && r.width == r.height)
        .min_by(|a, b| a.area().total_cmp(&b.area()))
        .expect("queued attachment tile");
    let visible_tiles = |bounds: &UiBounds| {
        bounds
            .containers
            .iter()
            .filter(|r| r.size() == queued_tile.size())
            .copied()
            .collect::<Vec<_>>()
    };
    let tiles = visible_tiles(&bounds);
    assert!(matches!(&tiles[..], [active, waiting]
        if waiting.contains(queued) && active.x + active.width <= waiting.x && active.y == waiting.y));
    assert!(
        !bounds
            .labels
            .iter()
            .any(|(_, label)| tiles[0].contains(label.center())),
        "the active tile shows only the spinner"
    );
    // Autosave and navigation complete even with both results outstanding.
    type_message(&mut app, "owner draft").await;
    dispatch(&mut app, Message::SaveDraft).await;
    let store = app.harness.as_ref().unwrap().store.clone();
    let id = id.unwrap();
    assert_eq!(store.load_session(&id).unwrap().draft, "owner draft");
    send_current(&mut app).await;
    assert!(store.load_session(&id).unwrap().entries.is_empty());
    dispatch(&mut app, Message::Create(root.clone())).await;
    assert!(
        visible_tiles(&composer_bounds(&app)).is_empty(),
        "other drafts do not inherit the placeholders"
    );
    dispatch(&mut app, Message::Select(id.clone())).await;
    assert_eq!(
        visible_tiles(&composer_bounds(&app)).len(),
        2,
        "the originating draft still has both"
    );
    let switch = app.update(Message::Create(root.clone()));
    // Deliver the first result during the switch; the queued drop must follow it.
    drain(&mut app, first).await;
    drain(&mut app, switch).await;
    assert!(app.draft_session.is_some());
    drain(&mut app, second).await;
    assert!(app.attachments.is_empty());
    assert_eq!(
        store
            .load_session(&id)
            .unwrap()
            .draft_images
            .iter()
            .map(|m| m.mime.as_str())
            .collect::<Vec<_>>(),
        ["image/png", "image/gif"]
    );
    dispatch(&mut app, Message::Select(id)).await;
    assert!(
        store
            .load_session(app.selected.as_ref().unwrap())
            .unwrap()
            .entries
            .is_empty(),
        "navigation cancelled the waiting send"
    );
    let invalid = root.join("not-an-image.png");
    std::fs::write(&invalid, b"invalid image bytes").unwrap();
    dispatch(
        &mut app,
        Message::Event(iced::Event::Window(window::Event::FileDropped(invalid))),
    )
    .await;
    assert!(
        app.error
            .as_ref()
            .unwrap()
            .contains("Could not attach image")
    );
    assert_eq!(app.attachments.len(), 2);
    assert!(!app.io_busy);
}

#[tokio::test]
async fn offscreen_paste_retries_save_without_appending_twice_or_quitting_early() {
    use crate::attachments::Input;
    let mut app = app();
    let root = app.smoke_dir.clone().unwrap();
    stored_session(&mut app, root.clone()).await;
    let id = app.selected.clone().unwrap();
    let store = app.harness.as_ref().unwrap().store.clone();
    type_message(&mut app, "original").await;
    let _ = app.update(Message::Paste);
    dispatch(&mut app, Message::Create(root)).await;
    type_message(&mut app, "unrelated").await;
    let db = rusqlite::Connection::open(store.root().join("sessions.sqlite3")).unwrap();
    sqlite::fail_writes(&db, "sessions", "offline draft fault");
    let _ = app.quit();
    assert!(app.close_requested);
    dispatch(
        &mut app,
        Message::AttachmentLoaded(Ok(Input::Text(" + paste".into()))),
    )
    .await;
    assert!(app.error.as_ref().unwrap().contains("offline draft fault"));
    assert!(!app.closing && !app.close_requested);
    assert_eq!(app.editor.text(), "unrelated");
    assert_eq!(store.load_session(&id).unwrap().draft, "original");
    // Loading the owner's in-memory draft must not turn a failed write into
    // a successful no-op. The next autosave retries persistence, not append.
    dispatch(&mut app, Message::Select(id.clone())).await;
    assert_eq!(app.editor.text(), "original + paste");
    sqlite::allow_writes(&db, "sessions");
    dispatch(&mut app, Message::SaveDraft).await;
    assert_eq!(store.load_session(&id).unwrap().draft, "original + paste");
    assert_eq!(store.load_new_draft().unwrap().message, "unrelated");
    dispatch(&mut app, Message::Changed).await;
    assert_eq!(
        app.current().unwrap().id,
        id,
        "retry must not evict the open draft"
    );
    assert_eq!(app.editor.text(), "original + paste");
}

#[tokio::test]
async fn late_inputs_follow_the_shared_new_draft_but_not_a_deleted_session() {
    use crate::attachments::Input;
    let mut app = app();
    let root = app.smoke_dir.clone().unwrap();
    stored_session(&mut app, root.clone()).await;
    let id = app.selected.clone().unwrap();
    let store = app.harness.as_ref().unwrap().store.clone();
    dispatch(&mut app, Message::Create(root.clone())).await;
    type_message(&mut app, "shared").await;
    let _ = app.update(Message::Paste);
    dispatch(&mut app, Message::Select(id.clone())).await;
    dispatch(
        &mut app,
        Message::AttachmentLoaded(Ok(Input::Text(" draft".into()))),
    )
    .await;
    assert!(app.editor.is_empty());
    let _ = app.update(Message::Paste);
    dispatch(&mut app, Message::Delete(id.clone())).await;
    dispatch(&mut app, Message::Confirm).await;
    dispatch(&mut app, Message::Create(root)).await;
    let _ = app.quit();
    dispatch(
        &mut app,
        Message::AttachmentLoaded(Ok(Input::Text("wrong owner".into()))),
    )
    .await;
    assert!(app.closing);
    assert_eq!(store.load_new_draft().unwrap().message, "shared draft");
    assert!(store.load_session(&id).is_err());
}

#[tokio::test]
async fn new_session_draft_follows_projects_but_not_existing_sessions() {
    let mut app = app();
    let h = app.harness.as_ref().unwrap().clone();
    dispatch(&mut app, Message::Create(PathBuf::from("/project-one"))).await;
    send_current(&mut app).await;
    assert!(app.selected.is_none());
    assert!(h.summaries().is_empty());
    assert!(h.store.load().unwrap().1.is_empty());

    let message = "Investigate λ
Then implement the fix.";
    type_message(&mut app, message).await;
    dispatch(&mut app, Message::SessionModel(ASTRA.id.into())).await;
    dispatch(&mut app, Message::SessionEffort("high".into())).await;
    dispatch(&mut app, Message::Create(PathBuf::from("/project-two"))).await;
    assert_eq!(app.editor.text(), message);
    assert_eq!(app.current().unwrap().model, app.catalog.settings.model);
    assert_eq!(
        app.current().unwrap().effort,
        app.catalog.settings.models[&app.catalog.settings.model].effort
    );
    assert!(h.summaries().is_empty());
    assert!(h.store.load().unwrap().1.is_empty());

    stored_session(&mut app, PathBuf::from("/existing")).await;
    let existing = app.selected.clone().unwrap();
    type_message(&mut app, "Existing conversation follow-up").await;
    dispatch(&mut app, Message::Create(PathBuf::from("/project-three"))).await;
    assert_eq!(app.editor.text(), message);
    dispatch(&mut app, Message::Select(existing)).await;
    assert_eq!(app.editor.text(), "Existing conversation follow-up");
    dispatch(&mut app, Message::New).await;
    assert!(app.current().is_none());
    dispatch(&mut app, Message::Create(PathBuf::from("/project-two"))).await;
    assert_eq!(app.editor.text(), message);
    dispatch(&mut app, Message::SaveDraft).await;
    assert_eq!(h.store.load_new_draft().unwrap(), app.new_draft);

    dispatch(&mut app, Message::SessionEffort("low".into())).await;
    send_current(&mut app).await;
    let id = app.selected.as_ref().unwrap();
    let saved = h.store.load_session(id).unwrap();
    assert_eq!(saved.project, Path::new("/project-two"));
    assert_eq!(saved.effort, "low");
    let users: Vec<_> = saved
        .entries
        .iter()
        .filter(|e| matches!(e.kind, EntryKind::User { .. }))
        .collect();
    assert_eq!(users.len(), 1);
    assert_eq!(users[0].text, message);
    assert_eq!(h.store.load().unwrap().1.len(), 2);
    assert_eq!(
        h.store.load_new_draft().unwrap(),
        NewSessionDraft::default()
    );
    assert!(app.editor.text().is_empty());
    dispatch(&mut app, Message::Create(PathBuf::from("/project-one"))).await;
    assert!(app.editor.text().is_empty());
}

#[tokio::test]
async fn failed_shared_draft_save_and_first_send_keep_message_for_retry() {
    let mut app = app();
    let path = app.smoke_dir.clone().unwrap();
    let h = app.harness.as_ref().unwrap().clone();
    dispatch(&mut app, Message::Create(path)).await;
    type_message(&mut app, "  \n").await;
    send_current(&mut app).await;
    assert!(h.store.load().unwrap().1.is_empty());
    type_message(&mut app, "Do not lose this message").await;
    let message = app.editor.text();
    let db = rusqlite::Connection::open(h.store.root().join("sessions.sqlite3")).unwrap();
    sqlite::fail_writes(&db, "new_session_draft", "fixture draft failure");
    dispatch(&mut app, Message::SaveDraft).await;
    assert!(app.new_draft_dirty);
    assert_eq!(app.editor.text(), message);
    assert!(
        app.error
            .as_ref()
            .unwrap()
            .contains("fixture draft failure")
    );
    sqlite::allow_writes(&db, "new_session_draft");
    dispatch(&mut app, Message::SaveDraft).await;
    assert!(!app.new_draft_dirty);
    assert_eq!(h.store.load_new_draft().unwrap().message, message);

    sqlite::fail_writes(&db, "sessions", "fixture session failure");
    send_current(&mut app).await;
    assert!(app.selected.is_none());
    assert_eq!(app.editor.text(), message);
    assert!(h.summaries().is_empty());
    assert!(h.store.load().unwrap().1.is_empty());
    assert_eq!(h.store.load_new_draft().unwrap().message, message);
    assert!(
        app.error
            .as_ref()
            .unwrap()
            .contains("fixture session failure")
    );
    sqlite::allow_writes(&db, "sessions");
    send_current(&mut app).await;
    assert!(app.selected.is_some());
    assert!(app.editor.text().is_empty());
    assert_eq!(h.store.load().unwrap().1.len(), 1);
    assert_eq!(
        h.store.load_new_draft().unwrap(),
        NewSessionDraft::default()
    );
}

#[tokio::test]
async fn modal_blocks_composer_edits_and_failed_draft_save_keeps_text() {
    let mut app = app();
    let path = app.smoke_dir.clone().unwrap();
    stored_session(&mut app, path).await;
    dispatch(
        &mut app,
        Message::Editor(text_editor::Action::Edit(text_editor::Edit::Insert('a'))),
    )
    .await;
    let selected = app.selected.clone().unwrap();
    dispatch(&mut app, Message::Rename(selected)).await;
    dispatch(
        &mut app,
        Message::Editor(text_editor::Action::Edit(text_editor::Edit::Insert('x'))),
    )
    .await;
    assert_eq!(app.editor.text(), "a");
    dispatch(&mut app, Message::Dismiss).await;
    let session = app.current().unwrap().id.clone();
    let db = rusqlite::Connection::open(
        app.harness
            .as_ref()
            .unwrap()
            .store
            .root()
            .join("sessions.sqlite3"),
    )
    .unwrap();
    sqlite::fail_writes(&db, "sessions", "fixture disk failure");
    dispatch(&mut app, Message::SaveDraft).await;
    assert_eq!(app.editor.text(), "a");
    assert!(app.draft_dirty);
    assert!(app.error.as_ref().unwrap().contains("fixture disk failure"));
    sqlite::allow_writes(&db, "sessions");
    dispatch(&mut app, Message::SaveDraft).await;
    assert!(!app.draft_dirty);
    assert_eq!(
        app.harness
            .as_ref()
            .unwrap()
            .get(&session)
            .unwrap()
            .snapshot()
            .draft,
        "a"
    );
}

#[tokio::test]
async fn enter_during_import_sends_the_complete_draft_once() {
    let mut app = app();
    let root = app.smoke_dir.clone().unwrap();
    dispatch(&mut app, Message::Create(root.clone())).await;
    let id = app.current().unwrap().id.clone();
    let store = app.harness.as_ref().unwrap().store.clone();
    let png = root.join("first.png");
    let gif = root.join("second.gif");
    std::fs::write(&png, include_bytes!("../../fixtures/media.png")).unwrap();
    std::fs::write(&gif, include_bytes!("../../fixtures/media.gif")).unwrap();
    let drop_file = |path| Message::Event(iced::Event::Window(window::Event::FileDropped(path)));
    let first = app.update(drop_file(png));
    let second = app.update(drop_file(gif));

    // Image-only new draft. Repeated Enter must neither send an incomplete
    // message nor enqueue a second submission while the files are preparing.
    press_composer_enter(&mut app).await;
    press_composer_enter(&mut app).await;
    drain(&mut app, second).await;
    assert!(store.load().unwrap().1.is_empty());
    drain(&mut app, first).await;
    let saved = store.load_session(&id).unwrap();
    let users: Vec<_> = saved
        .entries
        .iter()
        .filter_map(|entry| match &entry.kind {
            EntryKind::User { images, .. } => Some(images),
            _ => None,
        })
        .collect();
    assert_eq!(users.len(), 1);
    assert_eq!(
        users[0].iter().map(|m| m.mime.as_str()).collect::<Vec<_>>(),
        ["image/png", "image/gif"]
    );
    assert!(app.attachments.is_empty());
}

#[tokio::test]
async fn changes_and_failed_imports_cancel_a_waiting_send_without_losing_the_draft() {
    let mut app = app();
    let root = app.smoke_dir.clone().unwrap();
    stored_session(&mut app, root.clone()).await;
    let id = app.selected.clone().unwrap();
    let store = app.harness.as_ref().unwrap().store.clone();
    let png = root.join("ok.png");
    std::fs::write(&png, include_bytes!("../../fixtures/media.png")).unwrap();
    let drop_file = |path| Message::Event(iced::Event::Window(window::Event::FileDropped(path)));

    let pending = app.update(drop_file(png));
    type_message(&mut app, "before").await;
    press_composer_enter(&mut app).await;
    type_message(&mut app, " edit").await;
    drain(&mut app, pending).await;
    assert!(store.load_session(&id).unwrap().entries.is_empty());
    assert_eq!(app.editor.text(), "before edit");
    assert_eq!(app.attachments.len(), 1);

    let failed = app.update(drop_file(root.join("missing.png")));
    press_composer_enter(&mut app).await;
    drain(&mut app, failed).await;
    assert!(app.error.is_some());
    assert!(store.load_session(&id).unwrap().entries.is_empty());
    assert_eq!(app.editor.text(), "before edit");
    assert_eq!(app.attachments.len(), 1);
}

#[tokio::test]
async fn enter_waits_for_pasted_text_before_submitting() {
    let mut app = app();
    let root = app.smoke_dir.clone().unwrap();
    stored_session(&mut app, root).await;
    let id = app.selected.clone().unwrap();
    type_message(&mut app, "Review: ").await;
    let _ = app.update(Message::Paste);
    press_composer_enter(&mut app).await;
    assert_eq!(app.editor.text(), "Review: ");
    dispatch(
        &mut app,
        Message::AttachmentLoaded(Ok(crate::attachments::Input::Text("pasted prompt".into()))),
    )
    .await;
    let saved = app
        .harness
        .as_ref()
        .unwrap()
        .store
        .load_session(&id)
        .unwrap();
    assert_eq!(saved.entries[0].text, "Review: pasted prompt");
}

#[tokio::test]
async fn pending_file_search_does_not_arm_a_send_on_paste_completion() {
    let mut app = app();
    let root = app.smoke_dir.clone().unwrap();
    stored_session(&mut app, root).await;
    type_message(&mut app, "Look at @file").await;
    let _ = app.update(Message::Paste);
    press_composer_enter(&mut app).await;
    dispatch(
        &mut app,
        Message::AttachmentLoaded(Ok(crate::attachments::Input::Text(" later".into()))),
    )
    .await;
    assert_eq!(app.editor.text(), "Look at @file later");
    assert!(app.current().unwrap().entries.is_empty());
    assert!(
        app.notice
            .as_ref()
            .is_some_and(|(text, _)| text.contains("file search"))
    );
}

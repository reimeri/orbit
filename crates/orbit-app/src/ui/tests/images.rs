use super::*;

#[tokio::test]
async fn image_zoom_keeps_modal_until_dismiss_or_escape_animation_finishes() {
    let mut app = app();
    let root = app.smoke_dir.as_ref().unwrap().clone();
    let media = orbit_core::media::MediaStore::new(root.join("zoom-media"))
        .image(include_bytes!("../../fixtures/media.png"))
        .unwrap();
    for dismiss in [Message::Dismiss, footer_key(keyboard::key::Named::Escape)] {
        let handle = widget::image::Handle::from_rgba(120, 40, vec![255; 120 * 40 * 4]);
        let message = crate::image_transition::tests::activate(
            crate::image_transition::preview(
                theme::button(widget::image(handle.clone()))
                    .padding(0)
                    .on_press(Message::OpenImage(media.clone())),
                handle,
            ),
            iced::Point::new(30., 20.),
        );
        dispatch(&mut app, message).await;
        assert!(matches!(app.modal, Some(Modal::Image(_))));
        // Advance far beyond the transition, as if the window was suspended.
        dispatch(
            &mut app,
            Message::Image(crate::image_view::Message::Tick(
                std::time::Instant::now() + Duration::from_secs(86400),
            )),
        )
        .await;
        assert_eq!(app.image_view.backdrop_opacity(), 1.);
        dispatch(&mut app, dismiss.clone()).await;
        assert!(
            app.modal.is_some(),
            "closing must keep the modal and its input barrier alive"
        );
        dispatch(&mut app, dismiss).await;
        dispatch(
            &mut app,
            Message::Image(crate::image_view::Message::Tick(
                std::time::Instant::now() + Duration::from_secs(86400),
            )),
        )
        .await;
        assert!(app.modal.is_none());
        assert!(
            iced_runtime::futures::subscription::into_recipes(app.image_view.subscription())
                .is_empty()
        );
    }
}

#[tokio::test]
async fn image_settings_credentials_and_modal_survive_real_ui_updates() {
    let mut app = app();
    dispatch(&mut app, Message::ToggleSettings).await;
    dispatch(&mut app, Message::SettingsTab(SettingsTab::Images)).await;
    dispatch(&mut app, Message::ImageModel("custom-image-model".into())).await;
    dispatch(
        &mut app,
        Message::ImageBackend(orbit_core::image_tools::Backend::Api),
    )
    .await;
    dispatch(&mut app, Message::ImageQuality("high".into())).await;
    dispatch(&mut app, Message::ApplySettings).await;
    let saved = app.harness.as_ref().unwrap().store.load().unwrap().0;
    assert_eq!(saved.settings.images.model, "custom-image-model");
    assert_eq!(saved.settings.images.quality, "high");
    dispatch(&mut app, Message::ImageModel("discard-me".into())).await;
    dispatch(&mut app, Message::CancelSettings).await;
    assert_eq!(app.settings.images.model, "custom-image-model");
    // Reset is explicit and staged; it never changes backend or quality.
    dispatch(&mut app, Message::ToggleSettings).await;
    dispatch(&mut app, Message::ResetImageModel).await;
    assert_eq!(
        app.settings.images.model,
        orbit_core::image_tools::DEFAULT_API_MODEL
    );
    let saved = app.harness.as_ref().unwrap().store.load().unwrap().0;
    assert_eq!(saved.settings.images.model, "custom-image-model");
    dispatch(&mut app, Message::ApplySettings).await;
    let saved = app.harness.as_ref().unwrap().store.load().unwrap().0;
    assert_eq!(
        saved.settings.images.model,
        orbit_core::image_tools::DEFAULT_API_MODEL
    );
    assert_eq!(
        saved.settings.images.backend,
        orbit_core::image_tools::Backend::Api
    );
    assert_eq!(saved.settings.images.quality, "high");
    dispatch(&mut app, Message::ApiKey("private-test-key".into())).await;
    dispatch(&mut app, Message::SaveApiKey).await;
    assert!(app.api_key_configured);
    assert!(app.api_key_draft.is_empty());
    assert_eq!(
        app.harness
            .as_ref()
            .unwrap()
            .store
            .api_key()
            .unwrap()
            .as_deref(),
        Some("private-test-key")
    );
    dispatch(&mut app, Message::RemoveApiKey).await;
    assert!(!app.api_key_configured);
    let root = app.smoke_dir.as_ref().unwrap().clone();
    let media = orbit_core::media::MediaStore::new(root.join("test-media"))
        .image(include_bytes!("../../fixtures/media.gif"))
        .unwrap();
    stored_session(&mut app, root).await;
    let id = app.selected.clone().unwrap();
    let entry_id = orbit_core::store::id();
    let h = app.harness.as_ref().unwrap().clone();
    h.edit(&id, |s| {
        s.entries.push(Entry {
            id: entry_id.clone(),
            text: String::new(),
            kind: EntryKind::Tool {
                name: "image_gen__imagegen".into(),
                input: "generate".into(),
                output: media.description(),
                complete: true,
                failed: false,
                command_status: None,
                cell_id: None,
                parent: None,
                media: vec![media.clone()],
                web: None,
            },
        })
    })
    .unwrap();
    dispatch(&mut app, Message::Changed).await;
    dispatch(&mut app, Message::ToggleEntry(entry_id)).await;
    dispatch(&mut app, Message::OpenImage(media.clone())).await;
    assert!(matches!(&app.modal,Some(Modal::Image(m)) if m==&media));
    dispatch(&mut app, Message::Dismiss).await;
    assert!(app.modal.is_none());
    let reloaded = h.store.load_session(&id).unwrap();
    assert!(matches!(&reloaded.entries[0].kind,EntryKind::Tool {media:m,..} if m==&vec![media]));
}
#[tokio::test]
async fn markdown_images_load_in_chat_open_modal_and_retry_after_source_disappears() {
    let mut app = app();
    let root = app.smoke_dir.clone().unwrap();
    std::fs::write(
        root.join("chat.gif"),
        include_bytes!("../../fixtures/media.gif"),
    )
    .unwrap();
    stored_session(&mut app, root.clone()).await;
    let id = app.selected.clone().unwrap();
    let h = app.harness.as_ref().unwrap().clone();
    h.edit(&id, |s| {
        s.entries.push(Entry {
            id: "markdown-image".into(),
            text: "Before\n\n![Caption](chat.gif)\n\nAfter".into(),
            kind: EntryKind::Reply { final_at: None },
        })
    })
    .unwrap();
    dispatch(&mut app, Message::Changed).await;
    let loaded = app
        .markdown_images
        .loaded("chat.gif")
        .expect("visible image decoded");
    assert_eq!((loaded.width, loaded.height), (96, 64));
    dispatch(&mut app, Message::OpenMarkdownImage("chat.gif".into())).await;
    let Some(Modal::MarkdownImage(image)) = &app.modal else {
        panic!("image modal must open")
    };
    assert_eq!((image.width, image.height), (loaded.width, loaded.height));
    assert_eq!(image.bytes, loaded.bytes);
    dispatch(&mut app, Message::Dismiss).await;
    assert!(app.modal.is_none());
    std::fs::remove_file(root.join("chat.gif")).unwrap();
    stored_session(&mut app, root.clone()).await;
    assert!(app.markdown_images.loaded("chat.gif").is_none());
    dispatch(&mut app, Message::Select(id.clone())).await;
    assert!(
        app.markdown_images.loaded("chat.gif").is_none(),
        "reopening must read the source, not a saved copy"
    );
    std::fs::write(
        root.join("chat.gif"),
        include_bytes!("../../fixtures/media.gif"),
    )
    .unwrap();
    dispatch(&mut app, Message::RetryMarkdownImage("chat.gif".into())).await;
    assert!(app.markdown_images.loaded("chat.gif").is_some());
    let saved = h.store.load_session(&id).unwrap();
    assert_eq!(
        saved.entries[0].text,
        "Before\n\n![Caption](chat.gif)\n\nAfter"
    );
    assert!(matches!(saved.entries[0].kind, EntryKind::Reply { .. }));
}

#[tokio::test]
async fn relinking_a_replaced_image_refreshes_the_shared_chat_preview() {
    let mut app = app();
    let root = app.smoke_dir.clone().unwrap();
    let path = root.join("plot.png");
    let original = include_bytes!("../../fixtures/media.png");
    let replacement = include_bytes!("../../fixtures/large-transparent.png");
    stored_session(&mut app, root).await;
    let session = app.selected.clone().unwrap();
    let harness = app.harness.as_ref().unwrap().clone();
    for (id, bytes) in [
        ("first", original.as_slice()),
        ("second", replacement.as_slice()),
    ] {
        std::fs::write(&path, bytes).unwrap();
        harness
            .edit(&session, |s| {
                s.entries.push(Entry {
                    id: id.into(),
                    text: format!("![{id} plot](plot.png)"),
                    kind: EntryKind::Reply { final_at: None },
                });
            })
            .unwrap();
        dispatch(&mut app, Message::Changed).await;
        let preview = app.markdown_images.loaded("plot.png").unwrap();
        assert_eq!(preview.bytes.as_slice(), bytes);
    }
    dispatch(&mut app, Message::OpenMarkdownImage("plot.png".into())).await;
    let Some(Modal::MarkdownImage(image)) = &app.modal else {
        panic!("the shared preview must open")
    };
    assert_eq!(image.bytes.as_slice(), replacement);
}

use super::*;
use orbit_core::models::{ASTRA, LUNA};

#[tokio::test]
async fn dismissing_settings_discards_drafts_but_keeps_saved_values() {
    for dismiss in [
        Message::CancelSettings,
        Message::ToggleSettings,
        Message::ToggleAgents,
        footer_key(keyboard::key::Named::Escape),
    ] {
        let mut app = app();
        let model = ASTRA.id;
        for saved_fast in [false, true] {
            dispatch(&mut app, Message::ToggleSettings).await;
            dispatch(&mut app, Message::ModelFast(model.into(), saved_fast)).await;
            dispatch(&mut app, Message::Context("128000".into())).await;
            dispatch(&mut app, Message::Threshold("75".into())).await;
            dispatch(&mut app, Message::ApplySettings).await;
            assert!(!app.settings_open);

            dispatch(&mut app, Message::ToggleSettings).await;
            dispatch(&mut app, Message::ModelFast(model.into(), !saved_fast)).await;
            dispatch(&mut app, Message::Context("invalid".into())).await;
            dispatch(&mut app, Message::Threshold("42".into())).await;
            dispatch(&mut app, Message::ApiKey("unsaved-key".into())).await;
            dispatch(&mut app, Message::Font(19.)).await;
            assert_eq!(app.catalog.settings.models[model].fast, saved_fast);

            dispatch(&mut app, dismiss.clone()).await;
            assert!(!app.settings_open);
            assert!(app.api_key_draft.is_empty());
            dispatch(&mut app, Message::ToggleSettings).await;
            assert!(app.settings_open);
            assert_eq!(app.settings.models[model].fast, saved_fast);
            assert_eq!(app.context_text, "128000");
            assert_eq!(app.threshold_text, "75");
            assert_eq!(app.settings.font_size, 19.);
            let saved = app.harness.as_ref().unwrap().store.load().unwrap().0;
            assert_eq!(saved.settings.models[model].fast, saved_fast);
            assert_eq!(saved.settings.font_size, 19.);
            dispatch(&mut app, Message::CancelSettings).await;
        }
    }
}

#[tokio::test]
async fn save_closes_settings_only_after_success_and_autosave_keeps_it_open() {
    let mut app = app();
    app.settings_open = true;
    dispatch(&mut app, Message::Font(17.)).await;
    assert!(app.settings_open);
    dispatch(&mut app, Message::Context("invalid".into())).await;
    dispatch(&mut app, Message::ApplySettings).await;
    assert!(app.settings_open);
    assert!(
        app.error
            .as_ref()
            .unwrap()
            .contains("numeric context limit")
    );
    dispatch(&mut app, Message::Context("128000".into())).await;
    let settings_path = app
        .harness
        .as_ref()
        .unwrap()
        .store
        .root()
        .join("settings.json");
    std::fs::remove_file(&settings_path).unwrap();
    std::fs::create_dir(&settings_path).unwrap();
    dispatch(&mut app, Message::ApplySettings).await;
    assert!(app.settings_open);
    assert!(
        app.error
            .as_ref()
            .unwrap()
            .contains("Could not save settings")
    );
    std::fs::remove_dir(&settings_path).unwrap();
    dispatch(&mut app, Message::ErrorDismiss).await;
    let epoch = app.view_epoch;
    let save = app.update(Message::ApplySettings);
    assert!(app.settings_open, "wait for persistence before closing");
    drain(&mut app, save).await;
    assert!(!app.settings_open);
    assert_ne!(app.view_epoch, epoch);
    assert!(app.error.is_none());
    assert_eq!(
        app.harness
            .as_ref()
            .unwrap()
            .store
            .load()
            .unwrap()
            .0
            .settings
            .context_limit,
        128000
    );
}

#[tokio::test]
async fn delayed_settings_reply_respects_new_edits_and_overlapping_autosaves() {
    // Hold the real worker's reply, not the write itself. No timing sleeps.
    for (fail, reopen) in [(false, false), (false, true), (true, true)] {
        let mut app = app();
        let store = app.harness.as_ref().unwrap().store.clone();
        let path = store.root().join("settings.json");
        let original = app.catalog.settings.context_limit;
        dispatch(&mut app, Message::ToggleSettings).await;
        dispatch(&mut app, Message::Context("96000".into())).await;
        if fail {
            std::fs::create_dir(&path).unwrap(); // Atomic rename must fail.
        }
        let apply = app.update(Message::ApplySettings);
        assert_eq!(app.catalog.settings.context_limit, original);
        if reopen {
            dispatch(&mut app, Message::CancelSettings).await;
            dispatch(&mut app, Message::ToggleSettings).await;
            assert_eq!(app.context_text, "96000", "reopen the submitted draft");
        }
        dispatch(&mut app, Message::Context("144000".into())).await;
        if fail {
            let barrier = app.io.run(|| Message::External(Ok(())));
            drain(&mut app, barrier).await;
            std::fs::remove_dir(&path).unwrap();
        }
        // The autosave runs after Apply on disk, but before its reply in the UI.
        dispatch(&mut app, Message::Font(21.)).await;
        dispatch(&mut app, Message::Theme("Solarized Light")).await;
        let expected = if fail { original } else { 96_000 };
        let saved = store.load().unwrap().0.settings;
        assert_eq!((saved.context_limit, saved.font_size), (expected, 21.));
        assert_eq!(saved.theme, "Solarized Light");
        drain(&mut app, apply).await;
        assert!(
            app.settings_open,
            "the reply must not close the current edit"
        );
        assert_eq!(app.context_text, "144000");
        assert_eq!(app.catalog.settings.context_limit, expected);
        assert_eq!(app.catalog.settings.font_size, 21.);
        assert_eq!(app.theme(), Theme::SolarizedLight);
        if fail {
            assert!(
                app.error
                    .as_deref()
                    .unwrap()
                    .contains("Could not save settings")
            );
        }
        dispatch(&mut app, Message::CancelSettings).await;
        dispatch(&mut app, Message::ToggleSettings).await;
        assert_eq!(app.context_text, expected.to_string());
    }
}

#[tokio::test]
async fn successful_settings_save_does_not_close_an_untouched_reopened_panel() {
    let mut app = app();
    dispatch(&mut app, Message::ToggleSettings).await;
    dispatch(&mut app, Message::Threshold("65".into())).await;
    let apply = app.update(Message::ApplySettings);
    dispatch(&mut app, Message::CancelSettings).await;
    dispatch(&mut app, Message::ToggleSettings).await;
    drain(&mut app, apply).await;
    assert!(app.settings_open);
    assert_eq!(app.threshold_text, "65");
    assert_eq!(app.catalog.settings.compact_percent, 65);
}

#[tokio::test]
async fn subagent_edits_are_staged_and_saved_independently() {
    let mut app = app();
    let review = Profile::Review;
    dispatch(&mut app, Message::ToggleSettings).await;
    dispatch(&mut app, Message::SubagentModel(review, LUNA)).await;
    dispatch(&mut app, Message::SubagentEffort(review, "none".into())).await;
    dispatch(&mut app, Message::SubagentFast(review, true)).await;
    dispatch(&mut app, Message::SubagentModel(review, ASTRA)).await;
    assert!(
        ASTRA
            .efforts
            .contains(&app.settings.subagents[&review].effort.as_str())
    );
    dispatch(&mut app, Message::SubagentEffort(review, "max".into())).await;
    assert_eq!(app.catalog.settings.subagents[&review], review.defaults());
    dispatch(&mut app, Message::ApplySettings).await;
    let saved = app
        .harness
        .as_ref()
        .unwrap()
        .store
        .load()
        .unwrap()
        .0
        .settings;
    let config = &saved.subagents[&review];
    assert_eq!(
        (config.model.as_str(), config.effort.as_str(), config.fast),
        (ASTRA.id, "max", true)
    );
    assert_eq!(
        saved.subagents[&Profile::Explore],
        Profile::Explore.defaults()
    );
    assert_eq!(saved.models, Settings::default().models);
}

#[tokio::test]
async fn model_defaults_footer_fast_and_retired_session_recovery_persist() {
    let mut app = app();
    let h = app.harness.as_ref().unwrap().clone();
    dispatch(
        &mut app,
        Message::ModelEffort(LUNA.id.into(), "medium".into()),
    )
    .await;
    dispatch(&mut app, Message::ModelFast(LUNA.id.into(), false)).await;
    dispatch(
        &mut app,
        Message::ModelEffort(ASTRA.id.into(), "max".into()),
    )
    .await;
    dispatch(&mut app, Message::ModelFast(ASTRA.id.into(), true)).await;
    dispatch(&mut app, Message::Model(ASTRA.id.into())).await;
    dispatch(&mut app, Message::ApplySettings).await;
    let (catalog, _, _) = h.store.load().unwrap();
    assert_eq!(catalog.settings.model, ASTRA.id);
    assert!(catalog.settings.models[ASTRA.id].fast);
    dispatch(&mut app, Message::Create(PathBuf::from("/models"))).await;
    assert_eq!(app.current().unwrap().effort, "max");
    assert!(app.current().unwrap().fast);
    dispatch(&mut app, Message::SessionModel(LUNA.id.into())).await;
    assert_eq!(app.current().unwrap().effort, "medium");
    assert!(!app.current().unwrap().fast);
    dispatch(&mut app, Message::SessionFast(true)).await;
    assert!(app.current().unwrap().fast);
    assert_eq!(app.current().unwrap().effort, "medium");
    // The persisted session uses a retired model and must recover without losing history.
    let mut session = Session::new(PathBuf::from("/models"), &catalog.settings);
    session.model = "gpt-6.1-sol".into();
    session.effort = "low".into();
    session.fast = false;
    session.entries.push(Entry::new(
        EntryKind::User {
            skills: vec![],
            files: vec![],
            images: vec![],
            sent_at: None,
            steering: false,
        },
        "Keep this transcript",
    ));
    session.draft = "Keep this draft".into();
    let id = session.id.clone();
    h.store.save_session(&session).unwrap();
    dispatch(&mut app, Message::Select(id.clone())).await;
    let saved = h.store.load_session(&id).unwrap();
    assert_eq!(saved.model, ASTRA.id);
    assert_eq!(saved.effort, "max");
    assert!(saved.fast);
    assert_eq!(saved.entries[0].text, "Keep this transcript");
    assert_eq!(app.editor.text(), "Keep this draft");
    let notice = &app.notice.as_ref().unwrap().0;
    assert!(notice.contains(ASTRA.name));
    assert!(!notice.contains("gpt-"));
    dispatch(&mut app, Message::SessionEffort("high".into())).await;
    dispatch(&mut app, Message::SessionFast(false)).await;
    assert!(!h.store.load_session(&id).unwrap().fast);
    dispatch(&mut app, Message::NoticeDismiss).await;
    dispatch(&mut app, Message::New).await;
    dispatch(&mut app, Message::Select(id)).await;
    assert!(app.notice.is_none());
    assert_eq!(app.current().unwrap().effort, "high");
    assert!(!app.current().unwrap().fast);
}

#[tokio::test]
async fn retired_model_recovery_retries_failed_selection_without_losing_notice() {
    let mut app = app();
    let h = app.harness.as_ref().unwrap().clone();
    let defaults = app.catalog.settings.models[&app.catalog.settings.model].clone();
    let mut session = Session::new(PathBuf::from("/recovery"), &app.catalog.settings);
    session.model = "retired-model".into();
    session.effort = "low".into();
    session.fast = true;
    let id = session.id.clone();
    h.store.save_session(&session).unwrap();
    let db = rusqlite::Connection::open(h.store.root().join("sessions.sqlite3")).unwrap();
    for table in ["new_session_draft", "sessions"] {
        sqlite::fail_writes(&db, table, "fixture failure");
        dispatch(&mut app, Message::Select(id.clone())).await;
        assert!(app.error.as_ref().unwrap().contains("fixture failure"));
        assert!(app.selected.is_none());
        assert!(app.notice.is_none());
        assert_eq!(h.store.load_session(&id).unwrap().model, "retired-model");
        assert_eq!(h.get(&id).unwrap().snapshot().model, "retired-model");
        sqlite::allow_writes(&db, table);
    }
    dispatch(&mut app, Message::ErrorDismiss).await;
    dispatch(&mut app, Message::Select(id.clone())).await;
    assert_eq!(app.selected.as_ref(), Some(&id));
    assert!(app.error.is_none());
    assert!(app.notice.as_ref().unwrap().0.contains("previous model"));
    let saved = h.store.load_session(&id).unwrap();
    assert_eq!(saved.model, app.catalog.settings.model);
    assert_eq!(saved.effort, defaults.effort);
    assert_eq!(saved.fast, defaults.fast);
    // Deterministic expiry, without sleeping or an always-on timer.
    app.notice.as_mut().unwrap().1 = std::time::Instant::now() - NOTICE_DURATION;
    dispatch(&mut app, Message::NoticeTick).await;
    assert!(app.notice.is_none());
}

#[tokio::test]
async fn billing_choices_link_to_account_only_when_a_key_is_needed() {
    use orbit_core::image_tools::Backend;
    let mut app = app();
    app.settings_tab = SettingsTab::Images;
    let mut renderer = header_renderer();
    for (backend, configured, needs_key) in [
        (Backend::Codex, false, false),
        (Backend::Api, false, true),
        (Backend::Api, true, false),
    ] {
        app.settings.images.backend = backend;
        app.api_key_configured = configured;
        let mut ui = iced_runtime::UserInterface::build(
            app.settings_panel(),
            iced::Size::new(app.panel_width(), 740.),
            iced_runtime::user_interface::Cache::new(),
            &mut renderer,
        );
        let mut bounds = UiBounds::default();
        ui.operate(&renderer, &mut bounds);
        for (label, target) in [("Codex", Backend::Codex), ("OpenAI API", Backend::Api)] {
            let messages = click_at(&mut ui, &mut renderer, bounds.label(label).center());
            assert!(matches!(messages.as_slice(), [Message::ImageBackend(b)] if *b == target));
        }
        let link = bounds
            .labels
            .iter()
            .find(|(s, _)| s == "Add key in Account");
        assert_eq!(link.is_some(), needs_key);
        if let Some((_, rect)) = link {
            assert!(matches!(
                click_at(&mut ui, &mut renderer, rect.center()).as_slice(),
                [Message::SettingsTab(SettingsTab::Account)]
            ));
        }
    }
}

#[tokio::test]
async fn model_switches_align_with_header_and_edit_their_own_row() {
    let mut app = app();
    app.settings_tab = SettingsTab::Models;
    let mut renderer = header_renderer();
    app.window_width = 1800.;
    for width in [260., 700.] {
        app.catalog.settings.settings_width = width;
        let mut ui = iced_runtime::UserInterface::build(
            app.settings_panel(),
            iced::Size::new(width, 760.),
            iced_runtime::user_interface::Cache::new(),
            &mut renderer,
        );
        let mut bounds = UiBounds::default();
        ui.operate(&renderer, &mut bounds);
        let fast_x = bounds.label("FAST").center_x();
        assert!(fast_x < width);
        let name_x = bounds.label(MODELS[0].name).x;
        for model in MODELS {
            let name = bounds.label(model.name);
            assert_eq!(name.x, name_x);
            let id = bounds.label(model.id);
            let row_y = (name.y + id.y + id.height) / 2.;
            let messages = click_at(&mut ui, &mut renderer, iced::Point::new(fast_x, row_y));
            assert!(
                matches!(messages.as_slice(), [Message::ModelFast(id, on)]
                if id == model.id && *on != app.settings.models[model.id].fast),
                "wrong switch at {width} for {}: {messages:?}",
                model.name
            );
        }
        for profile in Profile::ALL {
            let name = bounds.label(profile.label());
            assert_eq!(name.x, name_x);
            let y = name.center_y();
            let messages = click_at(&mut ui, &mut renderer, iced::Point::new(fast_x, y));
            assert!(
                matches!(messages.as_slice(), [Message::SubagentFast(p, on)]
                if *p == profile && *on != app.settings.subagents[&profile].fast),
                "wrong switch at {width} for {profile:?}: {messages:?}"
            );
        }
    }
}

#[test]
fn settings_dropdown_outside_click_activates_controls() {
    use iced_runtime::{UserInterface, user_interface::Cache};
    let mut renderer = header_renderer();
    // Exercise targets before and after the dropdown in widget traversal:
    // capturing the close event must not suppress either target's press.
    for target in ["Before", "After"] {
        let view = row![
            theme::button(text("Before")).on_press(Message::ToggleSettings),
            theme::dropdown(["Selected", "Other"], Some("Selected"), |value| {
                Message::ImageQuality(value.into())
            })
            .on_open(Message::ToggleFooterMenu)
            .on_close(Message::CloseMenu)
            .width(iced::Shrink),
            theme::button(text("After")).on_press(Message::ToggleSettings),
        ]
        .spacing(20);
        let mut ui = UserInterface::build(
            view,
            iced::Size::new(600., 300.),
            Cache::new(),
            &mut renderer,
        );
        let mut bounds = UiBounds::default();
        ui.operate(&renderer, &mut bounds);
        let before = bounds.label("Before");
        let after = bounds.label("After");
        let selector =
            iced::Point::new((before.x + before.width + after.x) / 2., before.center_y());
        assert!(matches!(
            click_at(&mut ui, &mut renderer, selector).as_slice(),
            [Message::ToggleFooterMenu]
        ));
        let messages = click_at(&mut ui, &mut renderer, bounds.label(target).center());
        assert!(
            matches!(
                messages.as_slice(),
                [Message::CloseMenu, Message::ToggleSettings]
            ),
            "{target}: {messages:?}"
        );
        assert!(matches!(
            click_at(&mut ui, &mut renderer, selector).as_slice(),
            [Message::ToggleFooterMenu]
        ));
        assert!(matches!(
            click_at(&mut ui, &mut renderer, selector).as_slice(),
            [Message::CloseMenu]
        ));
    }
}

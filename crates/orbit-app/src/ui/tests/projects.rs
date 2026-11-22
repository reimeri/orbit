use super::*;

#[tokio::test]
async fn project_management_persists_names_stash_and_retries_deletion_without_touching_files() {
    let mut app = app();
    let root = app.smoke_dir.clone().unwrap();
    let path = root.join("managed-project");
    std::fs::create_dir(&path).unwrap();
    let file = path.join("keep.txt");
    std::fs::write(&file, "project source").unwrap();
    dispatch(&mut app, Message::AddProject).await;
    dispatch(&mut app, Message::ModalInput(path.display().to_string())).await;
    dispatch(&mut app, Message::Confirm).await;
    stored_session(&mut app, path.clone()).await;
    let first = app.selected.clone().unwrap();
    stored_session(&mut app, path.clone()).await;
    let second = app.selected.clone().unwrap();
    stored_session(&mut app, root.join("other-project")).await;
    let other = app.selected.clone().unwrap();
    let h = app.harness.as_ref().unwrap().clone();

    dispatch(&mut app, Message::RenameProject(path.clone())).await;
    dispatch(&mut app, Message::ModalInput("   ".into())).await;
    dispatch(&mut app, Message::Confirm).await;
    assert!(app.modal.is_some());
    assert!(app.error.is_some());
    dispatch(&mut app, Message::ErrorDismiss).await;
    dispatch(&mut app, Message::ModalInput("  My workspace λ  ".into())).await;
    dispatch(&mut app, Message::Confirm).await;
    assert!(displayed_text(app.sidebar(), iced::Size::new(300., 700.)).contains("My workspace λ"));
    dispatch(&mut app, Message::Favorite(path.clone())).await;
    dispatch(&mut app, Message::StashProject(path.clone())).await;
    let saved = h.store.load().unwrap().0.projects.remove(0);
    assert_eq!(saved.name(), "My workspace λ");
    assert_eq!(saved.path, path);
    assert!(saved.stashed && saved.favorite);
    assert_eq!(
        app.sessions.len(),
        3,
        "stashing must not hide conversations"
    );
    assert_eq!(app.selected.as_ref(), Some(&other));
    dispatch(&mut app, Message::StashProject(path.clone())).await;
    assert!(!h.store.load().unwrap().0.projects[0].stashed);

    dispatch(&mut app, Message::DeleteProject(path.clone())).await;
    dispatch(&mut app, Message::Dismiss).await;
    assert!(
        h.store.load_session(&first).is_ok(),
        "cancel is non-destructive"
    );
    let db = rusqlite::Connection::open(h.store.root().join("sessions.sqlite3")).unwrap();
    sqlite::fail_writes(&db, "sessions", "fixture project delete failure");
    dispatch(&mut app, Message::DeleteProject(path.clone())).await;
    dispatch(&mut app, Message::Confirm).await;
    assert!(
        app.error
            .as_ref()
            .unwrap()
            .contains("fixture project delete failure")
    );
    assert!(app.modal.is_some());
    assert_eq!(h.store.load().unwrap().0.projects.len(), 1);
    assert!(h.store.load_session(&first).is_ok());
    assert!(h.store.load_session(&second).is_ok());
    sqlite::allow_writes(&db, "sessions");
    dispatch(&mut app, Message::ErrorDismiss).await;
    dispatch(&mut app, Message::Confirm).await;
    assert!(app.catalog.projects.is_empty());
    assert!(h.store.load().unwrap().0.projects.is_empty());
    assert!(h.store.load_session(&first).is_err());
    assert!(h.store.load_session(&second).is_err());
    assert!(h.store.load_session(&other).is_ok());
    assert_eq!(app.selected.as_ref(), Some(&other));
    assert_eq!(std::fs::read_to_string(file).unwrap(), "project source");
    assert!(app.error.is_none());
}

#[tokio::test]
async fn project_deletion_recovers_catalog_failure_and_detaches_saved_draft() {
    let mut app = app();
    let root = app.smoke_dir.clone().unwrap();
    dispatch(&mut app, Message::AddProject).await;
    dispatch(&mut app, Message::ModalInput(root.display().to_string())).await;
    dispatch(&mut app, Message::Confirm).await;
    dispatch(&mut app, Message::Create(root.clone())).await;
    dispatch(
        &mut app,
        Message::Editor(text_editor::Action::Edit(text_editor::Edit::Paste(
            Arc::new("Unsent draft".into()),
        ))),
    )
    .await;
    dispatch(&mut app, Message::SaveDraft).await;
    stored_session(&mut app, root.clone()).await;
    let id = app.selected.clone().unwrap();
    let h = app.harness.as_ref().unwrap().clone();
    let settings = h.store.root().join("settings.json");
    let backup = h.store.root().join("settings.backup");
    std::fs::rename(&settings, &backup).unwrap();
    std::fs::create_dir(&settings).unwrap();
    dispatch(&mut app, Message::DeleteProject(root.clone())).await;
    dispatch(&mut app, Message::Confirm).await;
    assert!(app.error.is_some());
    assert!(app.modal.is_some(), "keep confirmation available for retry");
    assert_eq!(app.catalog.projects.len(), 1);
    assert!(h.store.load_session(&id).is_err());
    assert!(app.selected.is_none());
    assert!(app.current_session.is_none());
    assert!(app.editor.text().is_empty());
    std::fs::remove_dir(&settings).unwrap();
    std::fs::rename(&backup, &settings).unwrap();
    dispatch(&mut app, Message::ErrorDismiss).await;
    dispatch(&mut app, Message::Confirm).await;
    assert!(app.error.is_none());
    assert!(app.catalog.projects.is_empty());
    assert!(app.new_draft.project.is_none());
    let draft = h.store.load_new_draft().unwrap();
    assert!(draft.project.is_none());
    assert_eq!(
        draft.message, "Unsent draft",
        "deletion must not discard unsent text"
    );
    assert!(h.store.load().unwrap().0.selected.is_none());
}

#[tokio::test]
async fn compact_project_rows_route_clicks_and_restore_stashed_projects() {
    use iced_runtime::{UserInterface, user_interface::Cache};
    let mut app = app();
    app.account = Some("fixture account".into());
    app.catalog.projects = ["Alpha project", "Beta project", "Stashed project", "A long project name with asymmetric Unicode λ that must remain on a single row even in a narrow picker"]
        .into_iter()
        .enumerate()
        .map(|(i, name)| Project {
            path: PathBuf::from(format!("/project-{i}")),
            display_name: Some(name.into()),
            favorite: i == 1,
            stashed: i == 2,
        })
        .collect();
    let mut renderer = header_renderer();
    let mut heading_y = None;
    let mut summary = orbit_core::store::activity::Summary::default();
    let recent = summary.days.last_mut().unwrap();
    recent.total = 1200;
    recent.models.insert("fixture-model".into(), 1200);
    for period in &mut summary.tokens {
        period.input = 1000;
        period.output = 200;
        period.cached = 500;
    }
    summary
        .generation
        .push(orbit_core::store::activity::Generation {
            model: "fixture-model".into(),
            fast: true,
            tokens_per_second: Some(50.),
            latency_seconds: Some(1.25),
        });
    app.usage_summary = Some(Ok(Arc::new(summary)));
    let mut cache = Cache::new();
    for (width, height, wide) in [
        (1000., 700., true),
        (350., 700., false),
        (650., 1000., false),
        (1100., 700., true),
    ] {
        let mut ui = UserInterface::build(
            pointer_input(app.new_session(), false, app.chat_selection.clone()),
            iced::Size::new(width, height),
            cache,
            &mut renderer,
        );
        let mut bounds = UiBounds::default();
        ui.operate(&renderer, &mut bounds);
        let heading = bounds.label("New session");
        let activity = bounds.labels.iter().find(|(s, _)| s == "ACTIVITY");
        assert_eq!(activity.is_some(), wide);
        if let Some((_, activity)) = activity {
            assert!(
                activity.x > bounds.label("Alpha project").x + bounds.label("Alpha project").width
            );
            assert!((activity.y - heading.y).abs() < 1.);
            assert!(bounds.label("Last 30 days").x + bounds.label("Last 30 days").width <= width);
            assert!(bounds.label("50 tok/s").x > activity.x);
            // An empty day's entire slot, not just its one-pixel baseline, opens a tip.
            // Iced tooltips do not expose their contents to widget operations.
            let caption = bounds.label("Daily usage");
            let cursor =
                mouse::Cursor::Available(iced::Point::new(caption.x + 2., caption.y - 20.));
            let mut captures = Vec::new();
            for cursor in [mouse::Cursor::Unavailable, cursor] {
                ui.update(
                    &[iced::Event::Window(window::Event::RedrawRequested(
                        iced::time::Instant::now(),
                    ))],
                    cursor,
                    &mut renderer,
                    &mut iced::advanced::clipboard::Null,
                    &mut vec![],
                );
                let colors = theme::default_colors();
                ui.draw(
                    &mut renderer,
                    &theme::app(),
                    &iced::advanced::renderer::Style {
                        text_color: colors.text,
                    },
                    cursor,
                );
                let pixels = crate::test_renderer::screenshot(
                    &mut renderer,
                    iced::Size::new(width as u32, height as u32),
                    1.,
                    colors.background,
                );
                captures.push(pixels);
            }
            assert!(
                captures[0] != captures[1],
                "hovering empty chart space must paint details"
            );
        }
        if let Some(y) = heading_y {
            assert_eq!(
                heading.y, y,
                "the picker stays top-aligned as the window grows"
            );
        }
        heading_y = Some(heading.y);
        let alpha = bounds.label("Alpha project");
        let beta = bounds.label("Beta project");
        assert!(beta.y < alpha.y, "favorites remain first");
        assert!(
            alpha.y - beta.y < beta.height * 2.5,
            "rows must stay single-line and compact"
        );
        assert!(alpha.x >= 0. && alpha.x + alpha.width <= width);
        let long = bounds.label(&app.catalog.projects[3].name());
        assert_eq!(long.height, alpha.height, "long names must not grow a row");
        assert!(long.x + long.width <= width);
        assert!(
            !bounds
                .labels
                .iter()
                .any(|(label, _)| label == "Stashed project")
        );
        assert!(
            matches!(click_at(&mut ui, &mut renderer, beta.center()).as_slice(),
            [Message::Create(path)] if path == Path::new("/project-1"))
        );
        let star = bounds.rightmost_control_at(beta.center_y());
        assert!((star.center_y() - beta.center_y()).abs() < 1.);
        assert!(
            matches!(click_at(&mut ui, &mut renderer, star.center()).as_slice(),
            [Message::Favorite(path)] if path == Path::new("/project-1"))
        );
        let mut messages = Vec::new();
        ui.update(
            &[iced::Event::Mouse(mouse::Event::ButtonPressed(
                mouse::Button::Right,
            ))],
            mouse::Cursor::Available(alpha.center()),
            &mut renderer,
            &mut iced::advanced::clipboard::Null,
            &mut messages,
        );
        assert!(matches!(messages.as_slice(),
            [Message::ProjectMenu(path, position)] if path == Path::new("/project-0") && *position == alpha.center()));
        cache = ui.into_cache();
    }
    dispatch(&mut app, Message::ToggleStashed).await;
    assert!(
        displayed_text(app.new_session(), iced::Size::new(500., 700.)).contains("Stashed project")
    );
    dispatch(
        &mut app,
        Message::ProjectMenu(PathBuf::from("/project-2"), iced::Point::new(300., 200.)),
    )
    .await;
    let menu = displayed_text(app.view(), iced::Size::new(1000., 800.));
    assert!(menu.contains("Restore") && menu.contains("Rename") && menu.contains("Delete"));
    dispatch(&mut app, Message::StashProject(PathBuf::from("/project-2"))).await;
    assert!(app.context_menu.is_none());
    assert!(!app.catalog.projects[2].stashed);
}

#[tokio::test]
async fn session_menu_targets_the_clicked_session_and_releases_deleted_output() {
    let mut app = app();
    let path = app.smoke_dir.clone().unwrap();
    stored_session(&mut app, path.clone()).await;
    let first = app.selected.clone().unwrap();
    stored_session(&mut app, path).await;
    let second = app.selected.clone().unwrap();
    let h = app.harness.as_ref().unwrap().clone();
    h.edit(&first, |s| {
        s.entries.push(Entry {
            id: "long-tool".into(),
            text: String::new(),
            kind: EntryKind::Tool {
                name: "exec".into(),
                input: "code".into(),
                output: "long output".repeat(10000),
                complete: true,
                failed: false,
                command_status: None,
                cell_id: None,
                parent: None,
                media: vec![],
                web: None,
            },
        })
    })
    .unwrap();
    dispatch(&mut app, Message::Select(first.clone())).await;
    dispatch(&mut app, Message::ToggleEntry("long-tool".into())).await;
    assert!(app.outputs.contains_key("long-tool"));
    dispatch(&mut app, Message::ToggleEntry("long-tool".into())).await;
    assert!(!app.outputs.contains_key("long-tool"));
    dispatch(&mut app, Message::ToggleEntry("long-tool".into())).await;
    dispatch(&mut app, Message::Select(second.clone())).await;
    assert!(app.outputs.is_empty());
    dispatch(
        &mut app,
        Message::SessionMenu(first.clone(), iced::Point::new(120., 230.)),
    )
    .await;
    dispatch(
        &mut app,
        Message::Editor(text_editor::Action::Edit(text_editor::Edit::Insert('x'))),
    )
    .await;
    assert!(app.editor.text().is_empty());
    dispatch(&mut app, Message::Rename(first.clone())).await;
    dispatch(
        &mut app,
        Message::ModalInput("Renamed other session".into()),
    )
    .await;
    dispatch(&mut app, Message::Confirm).await;
    assert_eq!(app.selected.as_deref(), Some(second.as_str()));
    assert_eq!(
        h.load(&first).unwrap().snapshot().title,
        "Renamed other session"
    );
    dispatch(
        &mut app,
        Message::SessionMenu(first.clone(), iced::Point::new(120., 230.)),
    )
    .await;
    assert_eq!(
        copied_text(app.update(Message::CopySessionId(first.clone()))).await,
        first
    );
    assert!(app.context_menu.is_none());
    assert_eq!(app.selected.as_deref(), Some(second.as_str()));
    dispatch(
        &mut app,
        Message::SessionMenu(first.clone(), iced::Point::new(120., 230.)),
    )
    .await;
    dispatch(&mut app, Message::CloseMenu).await;
    assert!(app.context_menu.is_none());
    dispatch(
        &mut app,
        Message::SessionMenu(first.clone(), iced::Point::new(120., 230.)),
    )
    .await;
    dispatch(&mut app, Message::Delete(first.clone())).await;
    dispatch(&mut app, Message::Confirm).await;
    assert!(h.get(&first).is_none());
    assert_eq!(app.selected.as_deref(), Some(second.as_str()));
    assert!(app.outputs.is_empty());
    dispatch(
        &mut app,
        Message::Editor(text_editor::Action::Edit(text_editor::Edit::Insert('d'))),
    )
    .await;
    dispatch(&mut app, Message::Delete(second)).await;
    dispatch(&mut app, Message::Confirm).await;
    assert!(app.selected.is_none());
    assert!(app.editor.text().is_empty());
    assert!(app.error.is_none());
}

#[tokio::test]
async fn invalid_project_stays_in_modal_without_creating_state() {
    let mut app = app();
    dispatch(&mut app, Message::AddProject).await;
    app.modal_input = app
        .smoke_dir
        .as_ref()
        .unwrap()
        .join("does-not-exist")
        .display()
        .to_string();
    dispatch(&mut app, Message::Confirm).await;
    assert!(matches!(app.modal, Some(Modal::AddProject)));
    assert!(app.error.as_ref().unwrap().contains("Cannot open"));
    assert!(app.catalog.projects.is_empty());
}

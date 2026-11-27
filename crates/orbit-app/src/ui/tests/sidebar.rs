use super::*;

#[tokio::test]
async fn sidebar_status_rows_are_visible_and_do_not_overlap() {
    use iced_runtime::{UserInterface, user_interface::Cache};
    let mut app = app();
    let root = app.smoke_dir.clone().unwrap();
    stored_session(&mut app, root.clone()).await;
    let size = iced::Size::new(346., 600.);
    let mut renderer = header_renderer();
    let cache = UserInterface::build(app.sidebar(), size, Cache::new(), &mut renderer).into_cache();
    let statuses = [
        ("Awaiting an answer", SessionStatus::Question),
        ("Failed task", SessionStatus::Error),
        ("Running task", SessionStatus::Working),
        ("Unread task", SessionStatus::Unchecked),
        ("Idle task", SessionStatus::Idle),
    ];
    app.sessions = statuses
        .iter()
        .map(|(title, _)| {
            let mut session = Session::new(root.clone(), &app.catalog.settings);
            session.title = title.to_string();
            SessionSummary::from(&session)
        })
        .collect();
    app.prepare_sidebar();
    for row in &mut app.sidebar_rows {
        if let SidebarRow::Session(index, status) = row {
            *status = statuses[*index].1;
        }
    }
    let mut ui = UserInterface::build(app.sidebar(), size, cache, &mut renderer);
    let mut bounds = UiBounds::default();
    ui.operate(&renderer, &mut bounds);
    let mut bottom = 0.;
    for (title, _) in statuses {
        let label = bounds.label(title);
        assert!(label.width > 0. && label.height > 0.);
        assert!(label.y >= bottom && label.y + label.height <= size.height);
        bottom = label.y + label.height;
    }
}

#[tokio::test]
async fn sidebar_header_centers_only_favorites_and_opens_draft_in_clicked_project() {
    let mut app = app();
    let path = app.smoke_dir.clone().unwrap().join("favorite-project");
    app.catalog.projects = vec![
        Project {
            path: PathBuf::from("/not-starred"),
            favorite: false,
            display_name: None,
            stashed: false,
        },
        Project {
            path: path.clone(),
            favorite: true,
            display_name: None,
            stashed: false,
        },
    ];
    let mut renderer = header_renderer();
    let create = {
        let mut ui = iced_runtime::UserInterface::build(
            app.sidebar_header(),
            iced::Size::new(346., 100.),
            iced_runtime::user_interface::Cache::new(),
            &mut renderer,
        );
        let mut bounds = UiBounds::default();
        ui.operate(&renderer, &mut bounds);
        assert!(matches!(
            click_at(&mut ui, &mut renderer, bounds.leftmost_control()).as_slice(),
            [Message::ToggleSearch]
        ));
        assert!(matches!(
            click_at(
                &mut ui,
                &mut renderer,
                bounds
                    .rightmost_control_at(bounds.leftmost_control().y)
                    .center()
            )
            .as_slice(),
            [Message::New]
        ));
        let messages = click_at(
            &mut ui,
            &mut renderer,
            bounds
                .label(&project_initials(
                    &path.file_name().unwrap().to_string_lossy(),
                ))
                .center(),
        );
        assert!(matches!(messages.as_slice(), [Message::Create(p)] if p == &path));
        messages.into_iter().next().unwrap()
    };
    dispatch(&mut app, create).await;
    assert_eq!(app.current().unwrap().project, path);
    assert!(app.selected.is_none());
    assert!(app.sessions.is_empty());
    dispatch(&mut app, Message::New).await;
    assert!(chat_text(&app).contains("Choose a project to get started"));
}

#[tokio::test]
async fn project_badge_labels_are_centered_in_header_and_session_rows() {
    use iced::advanced::widget::{Id, Operation};
    #[derive(Default)]
    struct BadgeBounds {
        containers: Vec<iced::Rectangle>,
        labels: Vec<(iced::Rectangle, iced::Rectangle)>,
    }
    impl Operation for BadgeBounds {
        fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
            operate(self);
        }
        fn container(&mut self, _id: Option<&Id>, bounds: iced::Rectangle) {
            self.containers.push(bounds);
        }
        fn text(&mut self, _id: Option<&Id>, bounds: iced::Rectangle, text: &str) {
            if text == "OT" {
                let badge = self
                    .containers
                    .iter()
                    .rev()
                    .find(|badge| badge.contains(bounds.center()))
                    .unwrap();
                self.labels.push((*badge, bounds));
            }
        }
    }
    let mut app = app();
    let path = PathBuf::from("/orbit");
    app.catalog.projects.push(Project {
        path: path.clone(),
        favorite: true,
        display_name: None,
        stashed: false,
    });
    stored_session(&mut app, path).await;
    let mut renderer = header_renderer();
    let mut ui = iced_runtime::UserInterface::build(
        app.sidebar(),
        iced::Size::new(240., 640.),
        iced_runtime::user_interface::Cache::new(),
        &mut renderer,
    );
    let mut bounds = BadgeBounds::default();
    ui.operate(&renderer, &mut bounds);
    assert_eq!(
        bounds.labels.len(),
        2,
        "header and session badges must both be checked"
    );
    for (badge, label) in bounds.labels {
        assert_eq!(
            label.center(),
            badge.center(),
            "initials must stay centered inside the badge"
        );
    }
}

#[tokio::test]
async fn sidebar_favorites_scroll_without_moving_header_controls() {
    let mut app = app();
    app.catalog.projects = (0..12)
        .map(|i| Project {
            path: PathBuf::from(format!("/project-{i}")),
            favorite: true,
            display_name: None,
            stashed: false,
        })
        .collect();
    let mut renderer = header_renderer();
    let mut ui = iced_runtime::UserInterface::build(
        app.sidebar_header(),
        iced::Size::new(160., 100.),
        iced_runtime::user_interface::Cache::new(),
        &mut renderer,
    );
    let mut bounds = UiBounds::default();
    ui.operate(&renderer, &mut bounds);
    let favorites = bounds
        .scrolls
        .iter()
        .find(|(id, _)| id == &iced::advanced::widget::Id::new("favorite-projects"))
        .unwrap()
        .1;
    let search = bounds.leftmost_control();
    let new = bounds
        .rightmost_control_at(bounds.leftmost_control().y)
        .center();
    assert!(
        matches!(click_at(&mut ui, &mut renderer, iced::Point::new(favorites.x + 1., favorites.center_y())).as_slice(), [Message::Create(p)] if p == Path::new("/project-0"))
    );
    ui.update(
        &[iced::Event::Mouse(mouse::Event::WheelScrolled {
            delta: mouse::ScrollDelta::Pixels { x: -1000., y: 0. },
        })],
        mouse::Cursor::Available(favorites.center()),
        &mut renderer,
        &mut iced::advanced::clipboard::Null,
        &mut Vec::new(),
    );
    assert!(
        matches!(click_at(&mut ui, &mut renderer, iced::Point::new(favorites.x + favorites.width - 1., favorites.center_y())).as_slice(), [Message::Create(p)] if p == Path::new("/project-11"))
    );
    assert!(matches!(
        click_at(&mut ui, &mut renderer, search).as_slice(),
        [Message::ToggleSearch]
    ));
    assert!(matches!(
        click_at(&mut ui, &mut renderer, new).as_slice(),
        [Message::New]
    ));
}

#[tokio::test]
async fn sidebar_follows_top_but_preserves_browsing_anchor() {
    use iced::advanced::widget::{Id, Operation, operation::Scrollable};
    use iced::futures::StreamExt;
    use iced_runtime::{UserInterface, user_interface::Cache};

    #[derive(Default)]
    struct Offset(Option<f32>);
    impl Operation for Offset {
        fn traverse(&mut self, visit: &mut dyn FnMut(&mut dyn Operation)) {
            visit(self);
        }
        fn scrollable(
            &mut self,
            id: Option<&Id>,
            _: iced::Rectangle,
            _: iced::Rectangle,
            translation: iced::Vector,
            _: &mut dyn Scrollable,
        ) {
            if id == Some(&Id::new("sessions")) {
                self.0 = Some(translation.y);
            }
        }
    }

    // Check the real scroll widget as well as the virtual list, retaining
    // widget state so a missing scroll correction cannot pass unnoticed.
    async fn check(app: &mut App, task: Task<Message>, cache: Cache, expected: f32) -> Cache {
        assert_eq!(app.sidebar_list.offset, expected);
        let task = Task::batch(std::iter::once(task).chain(app.tasks.drain(..)));
        let mut renderer = header_renderer();
        let mut ui = UserInterface::build(
            app.sidebar(),
            iced::Size::new(346., 340.),
            cache,
            &mut renderer,
        );
        if let Some(mut actions) = iced_runtime::task::into_stream(task) {
            while let Some(action) = actions.next().await {
                if let iced_runtime::Action::Widget(mut operation) = action {
                    ui.operate(&renderer, operation.as_mut());
                }
            }
        }
        let mut offset = Offset::default();
        ui.operate(&renderer, &mut offset);
        assert_eq!(offset.0, Some(expected));
        ui.into_cache()
    }

    let mut app = app();
    app.tasks.clear();
    app.sessions = (0..40)
        .map(|i| SessionSummary {
            has_async_question: false,
            origin: None,
            child: None,
            id: format!("sticky-session-{i}"),
            title: format!("Session {i}"),
            project: PathBuf::from("/project"),
            updated: 0,
            interrupted: false,
            needs_attention: false,
            unchecked_completion: false,
            filing: None,
        })
        .collect();
    app.prepare_sidebar();
    let mut cache = check(&mut app, Task::none(), Cache::new(), 0.).await;

    // Inserting a new section used to anchor the old SESSIONS heading,
    // hiding the new top row even though the user had not scrolled.
    let mut new = app.sessions[0].clone();
    new.id = "new-session".into();
    new.needs_attention = true;
    app.sessions.insert(0, new);
    app.prepare_sidebar();
    cache = check(&mut app, Task::none(), cache, 0.).await;

    let offset = app.sidebar_list.prefix(12) + 7.;
    let anchor = app.sidebar_list.order[12];
    let epoch = app.sidebar_epoch;
    let _ = app.update(Message::SidebarScroll(epoch, offset, 300.));
    assert!(!app.sidebar_follow_top);
    let task =
        widget::operation::scroll_to("sessions", scrollable::AbsoluteOffset { x: 0., y: offset });
    cache = check(&mut app, task, cache, offset).await;

    let mut moved = app.sessions.pop().unwrap();
    moved.needs_attention = true;
    app.sessions.insert(0, moved);
    app.prepare_sidebar();
    assert_eq!(app.sidebar_list.order[13], anchor);
    let anchor_index = app
        .sidebar_list
        .order
        .iter()
        .position(|&id| id == anchor)
        .unwrap();
    let moved_offset = app.sidebar_list.prefix(anchor_index) + 7.;
    assert!(
        moved_offset > offset,
        "inserting above the anchor advances its scroll offset"
    );
    cache = check(&mut app, Task::none(), cache, moved_offset).await;

    // Late scroll notifications from the old layout must not re-enable
    // following. A measured height above the anchor still corrects it.
    let _ = app.update(Message::SidebarScroll(epoch, 0., 300.));
    assert!(!app.sidebar_follow_top);
    let previous_height = app.sidebar_list.prefix(1) - app.sidebar_list.prefix(0);
    let task = app.update(Message::SidebarSize(
        app.sidebar_epoch,
        1,
        previous_height * 1.5,
    ));
    let measured_offset = app.sidebar_list.prefix(anchor_index) + 7.;
    assert!(
        measured_offset > moved_offset,
        "a taller row above the anchor advances it"
    );
    cache = check(&mut app, task, cache, measured_offset).await;

    // Returning within one pixel of the top resumes following. Removing
    // the first section and later moving a session back into it stay pinned.
    let _ = app.update(Message::SidebarScroll(app.sidebar_epoch, 0.5, 300.));
    assert!(app.sidebar_follow_top);
    for session in &mut app.sessions {
        session.needs_attention = false;
    }
    app.prepare_sidebar();
    cache = check(&mut app, Task::none(), cache, 0.).await;
    app.sessions.last_mut().unwrap().needs_attention = true;
    app.prepare_sidebar();
    cache = check(&mut app, Task::none(), cache, 0.).await;
    let task = app.update(Message::SidebarSize(app.sidebar_epoch, 1, 63.));
    let _ = check(&mut app, task, cache, 0.).await;
}

#[tokio::test]
async fn sidebar_search_focuses_filters_and_clears_on_close() {
    use iced::futures::StreamExt;
    let mut app = app();
    for (path, title) in [("/alpha", "Zulu notes"), ("/beta", "Control")] {
        stored_session(&mut app, PathBuf::from(path)).await;
        app.harness
            .as_ref()
            .unwrap()
            .edit(app.selected.as_ref().unwrap(), |s| s.title = title.into())
            .unwrap();
    }
    app.refresh();
    assert!(!app.search_active);
    let task = app.update(Message::ToggleSearch);
    assert!(app.search_active);
    let mut renderer = header_renderer();
    let input = {
        let mut ui = iced_runtime::UserInterface::build(
            app.sidebar_header(),
            iced::Size::new(346., 100.),
            iced_runtime::user_interface::Cache::new(),
            &mut renderer,
        );
        let mut actions = iced_runtime::task::into_stream(task).unwrap();
        while let Some(action) = actions.next().await {
            if let iced_runtime::Action::Widget(mut operation) = action {
                ui.operate(&renderer, operation.as_mut());
            }
        }
        let mut messages = Vec::new();
        ui.update(
            &[iced::Event::Keyboard(keyboard::Event::KeyPressed {
                key: keyboard::Key::Character("z".into()),
                modified_key: keyboard::Key::Character("z".into()),
                physical_key: keyboard::key::Physical::Code(keyboard::key::Code::KeyZ),
                location: keyboard::Location::Standard,
                modifiers: keyboard::Modifiers::empty(),
                text: Some("z".into()),
                repeat: false,
            })],
            mouse::Cursor::Unavailable,
            &mut renderer,
            &mut iced::advanced::clipboard::Null,
            &mut messages,
        );
        assert!(matches!(messages.as_slice(), [Message::Search(query)] if query == "z"));
        let mut bounds = UiBounds::default();
        ui.operate(&renderer, &mut bounds);
        assert!(matches!(
            click_at(&mut ui, &mut renderer, bounds.leftmost_control()).as_slice(),
            [Message::ToggleSearch]
        ));
        messages.into_iter().next().unwrap()
    };
    dispatch(&mut app, input).await;
    let visible: Vec<_> = app
        .sidebar_rows
        .iter()
        .filter_map(|r| match r {
            SidebarRow::Session(i, _) => Some(app.sessions[*i].title.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(visible, ["Zulu notes"]);
    let epoch = app.sidebar_epoch;
    dispatch(&mut app, Message::SidebarScroll(epoch, 100., 300.)).await;
    assert!(!app.sidebar_follow_top);
    dispatch(&mut app, Message::ToggleSearch).await;
    assert!(!app.search_active);
    assert!(app.search.is_empty());
    assert_eq!(app.sidebar_list.offset, 0.);
    assert!(app.sidebar_follow_top);
    assert_eq!(
        app.sidebar_rows
            .iter()
            .filter(|r| matches!(r, SidebarRow::Session(..)))
            .count(),
        2
    );
    dispatch(&mut app, Message::ToggleSearch).await;
    dispatch(&mut app, Message::Search("BETA".into())).await;
    assert_eq!(
        app.sidebar_rows
            .iter()
            .filter(|r| matches!(r, SidebarRow::Session(..)))
            .count(),
        1
    );
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
    assert!(!app.search_active);
    assert!(app.search.is_empty());
    assert_eq!(
        app.sidebar_rows
            .iter()
            .filter(|r| matches!(r, SidebarRow::Session(..)))
            .count(),
        2
    );
}

#[test]
fn project_badge_initials_follow_name_words() {
    for (name, expected) in [
        ("orbit-app", "OA"),
        ("Orbit", "OT"),
        ("a", "A"),
        ("  rust__language-server ", "RL"),
        ("my project tools", "MP"),
        ("éclair", "ÉR"),
        ("界", "界"),
        ("a\u{301}", "A\u{301}"),
        ("", ""),
    ] {
        assert_eq!(project_initials(name), expected, "{name:?}");
    }
}

async fn assert_activity_clock(app: &App, expected: bool) {
    use iced::futures::{StreamExt, stream};
    use iced_runtime::futures::subscription;

    let messages = stream::select_all(
        subscription::into_recipes(app.subscription())
            .into_iter()
            .map(|recipe| recipe.stream(stream::pending().boxed())),
    );
    let mut ticks = messages.filter(|m| std::future::ready(matches!(m, Message::Tick)));
    if expected {
        assert!(matches!(
            tokio::time::timeout(ACTIVITY_INTERVAL + Duration::from_secs(1), ticks.next()).await,
            Ok(Some(Message::Tick))
        ));
    } else {
        assert!(
            tokio::time::timeout(ACTIVITY_INTERVAL * 3, ticks.next())
                .await
                .is_err()
        );
    }
}

#[tokio::test]
async fn sidebar_work_keeps_activity_ticks_until_frozen_or_idle() {
    let mut app = app();
    let session = Session::new(app.smoke_dir.clone().unwrap(), &app.catalog.settings);
    app.sessions.push(SessionSummary::from(&session));
    app.prepare_sidebar();
    assert!(app.harness.as_ref().unwrap().all().is_empty());
    // Working rows can outlive their own turn or represent running children.
    for status in [
        SessionStatus::Working,
        SessionStatus::Question,
        SessionStatus::Idle,
        SessionStatus::Working,
    ] {
        *app.sidebar_rows.last_mut().unwrap() = SidebarRow::Session(0, status);
        assert_activity_clock(&app, status == SessionStatus::Working).await;
    }
}

#[tokio::test]
async fn hidden_background_turn_animates_only_when_shown_and_returns_when_finished() {
    let mut app = app();
    let root = app.smoke_dir.clone().unwrap();
    stored_session(&mut app, root.clone()).await;
    let mut session = Session::new(root, &app.catalog.settings);
    session.title = "Background task".into();
    let h = app.harness.as_ref().unwrap().clone();
    let other = h.insert(session);
    other.running.store(true, Ordering::Release);
    app.refresh();
    app.child_statuses
        .entry(other.id.clone())
        .or_default()
        .needs_attention = true;
    dispatch(&mut app, Message::HideWorking(true)).await;

    let sidebar = |app: &App| displayed_text(app.sidebar(), Size::new(320., 600.));
    assert!(!sidebar(&app).contains("Background task"));
    assert_activity_clock(&app, false).await;

    dispatch(
        &mut app,
        Message::ToggleCollapsed(CollapsedSection::Working),
    )
    .await;
    let shown = sidebar(&app);
    assert!(shown.contains("Background task") && shown.contains("! · "));
    assert_activity_clock(&app, true).await;

    dispatch(
        &mut app,
        Message::ToggleCollapsed(CollapsedSection::Working),
    )
    .await;
    dispatch(&mut app, Message::Select(other.id.clone())).await;
    assert_activity_clock(&app, true).await;
    other.running.store(false, Ordering::Release);
    h.edit(&other.id, |s| s.unchecked_completion = true)
        .unwrap();
    dispatch(&mut app, Message::Changed).await;
    assert!(sidebar(&app).contains("Background task"));
}

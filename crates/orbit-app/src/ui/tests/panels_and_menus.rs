use super::*;

#[tokio::test]
async fn context_menu_icons_keep_labels_aligned_and_actions_reachable() {
    let mut app = app();
    let project = PathBuf::from("/icon-menu-project");
    app.catalog.projects = vec![Project {
        path: project.clone(),
        display_name: None,
        favorite: false,
        stashed: false,
    }];
    let mut renderer = header_renderer();
    for (width, height) in [(900., 620.), (360., 280.)] {
        app.window_width = width;
        app.window_height = height;
        for kind in 0..3 {
            app.catalog.projects[0].stashed = kind == 2;
            let target = if kind == 0 {
                MenuTarget::Session("icon-menu-session".into())
            } else {
                MenuTarget::Project(project.clone())
            };
            let middle = match kind {
                0 => "Copy session ID",
                1 => "Stash",
                _ => "Restore",
            };
            app.context_menu = Some(ContextMenu(
                target,
                iced::Point::new(width - 1., height - 1.),
            ));
            let mut ui = iced_runtime::UserInterface::build(
                app.view(),
                iced::Size::new(width, height),
                iced_runtime::user_interface::Cache::new(),
                &mut renderer,
            );
            let mut bounds = UiBounds::default();
            ui.operate(&renderer, &mut bounds);
            use iced::advanced::Renderer as _;
            ui.update(
                &[iced::Event::Window(window::Event::RedrawRequested(
                    iced::time::Instant::now(),
                ))],
                mouse::Cursor::Unavailable,
                &mut renderer,
                &mut iced::advanced::clipboard::Null,
                &mut Vec::new(),
            );
            renderer.reset(iced::Rectangle::with_size(iced::Size::new(width, height)));
            ui.draw(
                &mut renderer,
                &theme::app(),
                &iced::advanced::renderer::Style {
                    text_color: theme::default_colors().text,
                },
                mouse::Cursor::Unavailable,
            );
            let pixels = crate::test_renderer::screenshot(
                &mut renderer,
                iced::Size::new(width as u32, height as u32),
                1.,
                theme::default_colors().background,
            );
            let rename = bounds.label("Rename");
            for label in ["Rename", middle, "Delete"] {
                let text = bounds.label(label);
                assert_eq!(text.x, rename.x, "menu label column");
                assert_eq!(text.height, rename.height, "menu labels must not wrap");
                assert!(text.x >= 0. && text.x + text.width <= width);
                assert!(text.y >= 0. && text.y + text.height <= height);
                // Click the icon side of the action rather than only its label.
                let button = bounds
                    .containers
                    .iter()
                    .filter(|b| {
                        b.x < text.x
                            && b.x + b.width >= text.x + text.width
                            && (b.center_y() - text.center_y()).abs() < 0.5
                    })
                    .min_by(|a, b| a.width.total_cmp(&b.width))
                    .unwrap();
                assert!(button.x >= 0. && button.x + button.width <= width);
                assert!(
                    button.y >= 0. && button.y + button.height <= height,
                    "menu {kind} {label}: button {button:?}, text {text:?}, viewport {width}x{height}"
                );
                // Both the icon column and the text must actually paint, not
                // merely report correct layout bounds to widget operations.
                for region in [
                    text,
                    iced::Rectangle {
                        x: button.x,
                        y: text.y,
                        width: text.x - button.x,
                        height: text.height,
                    },
                ] {
                    let mut colors = (region.y.ceil() as u32
                        ..(region.y + region.height).floor() as u32)
                        .flat_map(|y| {
                            (region.x.ceil() as u32..(region.x + region.width).floor() as u32)
                                .map(move |x| ((y * width as u32 + x) * 4) as usize)
                        })
                        .map(|offset| &pixels[offset..offset + 3]);
                    let first = colors.next().expect("nonempty painted region");
                    assert!(
                        colors.any(|color| color != first),
                        "menu {kind}: {label} must paint text and icon"
                    );
                }
                let messages = click_at(
                    &mut ui,
                    &mut renderer,
                    iced::Point::new((button.x + text.x) / 2., text.center_y()),
                );
                match (kind, label, messages.as_slice()) {
                    (0, "Rename", [Message::Rename(id)])
                    | (0, "Copy session ID", [Message::CopySessionId(id)])
                    | (0, "Delete", [Message::Delete(id)]) => {
                        assert_eq!(id, "icon-menu-session")
                    }
                    (1 | 2, "Rename", [Message::RenameProject(path)])
                    | (1 | 2, "Stash" | "Restore", [Message::StashProject(path)])
                    | (1 | 2, "Delete", [Message::DeleteProject(path)]) => {
                        assert_eq!(path, &project)
                    }
                    _ => panic!("wrong action for menu {kind}: {label}"),
                }
            }
        }
    }
}

#[tokio::test]
async fn modal_backdrop_masks_controls_but_keeps_dialog_interactive() {
    use iced_runtime::{
        UserInterface,
        user_interface::{Cache, State},
    };
    let mut app = app();
    app.settings_open = true;
    app.modal = Some(Modal::Quit);
    let mut renderer = header_renderer();
    let size = iced::Size::new(app.window_width, app.window_height);
    let mut ui = UserInterface::build(app.view(), size, Cache::new(), &mut renderer);
    let mut bounds = UiBounds::default();
    ui.operate(&renderer, &mut bounds);
    let covered = bounds
        .rightmost_control_at(bounds.label("Settings").center_y())
        .center();
    let confirm = bounds.label("Stop and quit").center();
    for (point, interaction) in [
        (covered, mouse::Interaction::Idle),
        (confirm, mouse::Interaction::Pointer),
    ] {
        let (state, _) = ui.update(
            &[iced::Event::Mouse(mouse::Event::CursorMoved {
                position: point,
            })],
            mouse::Cursor::Available(point),
            &mut renderer,
            &mut iced::advanced::clipboard::Null,
            &mut Vec::new(),
        );
        assert!(
            matches!(state, State::Updated { mouse_interaction, .. } if mouse_interaction == interaction),
            "{state:?}"
        );
    }
    assert!(matches!(
        click_at(&mut ui, &mut renderer, covered).as_slice(),
        [Message::Dismiss]
    ));
    assert!(matches!(
        click_at(&mut ui, &mut renderer, confirm).as_slice(),
        [Message::Confirm]
    ));
}

#[tokio::test]
async fn context_menu_outside_click_activates_controls_and_retargets_menu() {
    use iced_runtime::{UserInterface, user_interface::Cache};
    let mut app = app();
    app.account = Some("fixture account".into());
    let project = PathBuf::from("/outside-click-project");
    app.catalog.projects = vec![Project {
        path: project.clone(),
        display_name: Some("Outside click project".into()),
        favorite: false,
        stashed: false,
    }];
    let mut renderer = header_renderer();
    let size = iced::Size::new(app.window_width, app.window_height);
    for target in [
        MenuTarget::Session("context-session".into()),
        MenuTarget::Project(project.clone()),
    ] {
        // Keep the menu away from the project row and Settings header.
        app.context_menu = Some(ContextMenu(target, iced::Point::ORIGIN));
        app.settings_open = true;
        let mut ui = UserInterface::build(app.view(), size, Cache::new(), &mut renderer);
        let mut bounds = UiBounds::default();
        ui.operate(&renderer, &mut bounds);
        let close = bounds.rightmost_control_at(bounds.label("Settings").center_y());
        let messages = click_at(&mut ui, &mut renderer, close.center());
        assert!(
            matches!(
                messages.as_slice(),
                [Message::CloseMenu, Message::ToggleSettings]
            ),
            "{messages:?}"
        );
        drop(ui);
        let original = app.context_menu.clone();
        for message in messages {
            dispatch(&mut app, message).await;
        }
        assert!(app.context_menu.is_none());
        assert!(!app.settings_open);

        app.context_menu = original;
        let mut ui = UserInterface::build(app.view(), size, Cache::new(), &mut renderer);
        let mut bounds = UiBounds::default();
        ui.operate(&renderer, &mut bounds);
        let point = bounds.label("Outside click project").center();
        let mut messages = Vec::new();
        ui.update(
            &[iced::Event::Mouse(mouse::Event::ButtonPressed(
                mouse::Button::Right,
            ))],
            mouse::Cursor::Available(point),
            &mut renderer,
            &mut iced::advanced::clipboard::Null,
            &mut messages,
        );
        assert!(
            matches!(messages.as_slice(),
            [Message::CloseMenu, Message::ProjectMenu(path, position)]
            if path == &project && *position == point),
            "{messages:?}"
        );
        drop(ui);
        for message in messages {
            dispatch(&mut app, message).await;
        }
        assert!(matches!(&app.context_menu,
            Some(ContextMenu(MenuTarget::Project(path), position)) if path == &project && *position == point));
    }
}

#[tokio::test]
async fn right_panel_headers_match_and_close_their_own_panel() {
    let mut app = app();
    let mut renderer = header_renderer();
    for width in [1200., 860.] {
        app.window_width = width;
        let mut headers = Vec::new();
        for agents in [false, true] {
            let (title, panel) = if agents {
                ("Agents", app.agents_panel())
            } else {
                ("Settings", app.settings_panel())
            };
            let mut ui = iced_runtime::UserInterface::build(
                panel,
                iced::Size::new(width, 560.),
                iced_runtime::user_interface::Cache::new(),
                &mut renderer,
            );
            let mut bounds = UiBounds::default();
            ui.operate(&renderer, &mut bounds);
            let title = bounds.label(title);
            let close = bounds.rightmost_control_at(title.center_y());
            assert_eq!(title.x, RIGHT_PANEL_PADDING);
            assert!(title.y >= RIGHT_PANEL_PADDING);
            assert_eq!(title.center_y(), close.center_y());
            headers.push((title, close));

            let mut messages = Vec::new();
            ui.update(
                &[
                    iced::Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
                    iced::Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
                ],
                mouse::Cursor::Available(close.center()),
                &mut renderer,
                &mut iced::advanced::clipboard::Null,
                &mut messages,
            );
            assert!(matches!(
                (agents, messages.as_slice()),
                (true, [Message::ToggleAgents]) | (false, [Message::ToggleSettings])
            ));
        }
        assert_eq!(headers[0], headers[1], "headers differ at width {width}");
    }
}

#[tokio::test]
async fn resizing_uses_press_position_and_saves_both_panel_widths() {
    let mut app = app();
    app.catalog.settings.sidebar_width = 260.;
    app.catalog.settings.settings_width = 340.;
    for (right, start, end) in [(false, 262., 315.), (true, 808., 777.)] {
        dispatch(
            &mut app,
            Message::ResizeStart(right, iced::Point::new(start, 50.)),
        )
        .await;
        dispatch(
            &mut app,
            Message::Event(iced::Event::Mouse(mouse::Event::CursorMoved {
                position: iced::Point::new(end, 70.),
            })),
        )
        .await;
        dispatch(
            &mut app,
            Message::Event(iced::Event::Mouse(mouse::Event::ButtonReleased(
                mouse::Button::Left,
            ))),
        )
        .await;
        assert!(app.drag.is_none());
    }
    assert_eq!(app.catalog.settings.sidebar_width, 313.);
    assert_eq!(app.catalog.settings.settings_width, 371.);
    let (saved, _, _) = app.harness.as_ref().unwrap().store.load().unwrap();
    assert_eq!(saved.settings.sidebar_width, 313.);
    assert_eq!(saved.settings.settings_width, 371.);
}

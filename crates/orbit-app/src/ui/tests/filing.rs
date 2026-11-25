use super::*;
use crate::ui::sidebar::{
    FILED_ROW_GAP, FILED_ROW_HEIGHT, FILED_VISIBLE_ROWS, SESSION_METADATA_GAP,
};

#[tokio::test]
async fn deferring_keeps_selection_and_draft_and_requires_opening_unread_completion() {
    let mut app = app();
    let path = app.smoke_dir.clone().unwrap();
    stored_session(&mut app, path.clone()).await;
    let id = app.selected.clone().unwrap();
    stored_session(&mut app, path).await;
    let other = app.selected.clone();
    let h = app.harness.as_ref().unwrap().clone();
    h.edit(&id, |s| s.unchecked_completion = true).unwrap();
    dispatch(&mut app, Message::Changed).await;
    dispatch(
        &mut app,
        Message::FileSession(id.clone(), FilingAction::File(FilingKind::Deferred)),
    )
    .await;
    assert!(app.error.is_some());
    assert!(app.filed[FilingKind::Deferred as usize].rows.is_empty());
    assert_eq!(app.selected, other);
    app.error = None;

    dispatch(&mut app, Message::Select(id.clone())).await;
    assert!(!app.current().unwrap().unchecked_completion);
    app.editor = text_editor::Content::with_text("Keep my draft λ");
    app.draft_dirty = true;
    let updated = app.current().unwrap().updated;
    dispatch(
        &mut app,
        Message::FileSession(id.clone(), FilingAction::File(FilingKind::Deferred)),
    )
    .await;
    assert_eq!(app.selected.as_deref(), Some(id.as_str()));
    assert_eq!(app.editor.text(), "Keep my draft λ");
    assert_eq!(app.current().unwrap().updated, updated);
    assert_eq!(
        app.current().unwrap().filing.unwrap().kind,
        FilingKind::Deferred
    );
    assert!(app.filed[FilingKind::Settled as usize].rows.is_empty());
    assert!(!app.filed_expanded(FilingKind::Deferred));
    assert_eq!(app.filed[FilingKind::Deferred as usize].rows.len(), 1);
    assert!(!app.sidebar_rows.iter().any(|row| {
        matches!(row, SidebarRow::Session(index, _) if app.sessions[*index].id == id)
    }));
    dispatch(&mut app, Message::ToggleFiled(FilingKind::Deferred)).await;
    assert!(app.filed_expanded(FilingKind::Deferred));
    dispatch(
        &mut app,
        Message::FileSession(id.clone(), FilingAction::Undo),
    )
    .await;
    assert!(app.filed[FilingKind::Deferred as usize].rows.is_empty());
    assert_eq!(app.current().unwrap().updated, updated);
    assert_eq!(app.selected.as_deref(), Some(id.as_str()));

    dispatch(
        &mut app,
        Message::FileSession(id.clone(), FilingAction::File(FilingKind::Deferred)),
    )
    .await;
    send_current(&mut app).await;
    assert!(app.current().unwrap().filing.is_none());
    assert!(app.filed[FilingKind::Deferred as usize].rows.is_empty());
    assert_eq!(app.selected.as_deref(), Some(id.as_str()));
    let c = h.get(&id).unwrap();
    let mut changes = h.subscribe();
    while c.is_executing() {
        tokio::time::timeout(Duration::from_secs(3), changes.changed())
            .await
            .unwrap()
            .unwrap();
    }
}

#[tokio::test]
async fn defer_confirmation_uses_clicked_row_not_selected_session() {
    let mut app = app();
    let path = app.smoke_dir.clone().unwrap();
    stored_session(&mut app, path.clone()).await;
    let clicked = app.selected.clone().unwrap();
    stored_session(&mut app, path).await;
    let selected = app.selected.clone();
    let h = app.harness.clone().unwrap();
    dispatch(
        &mut app,
        Message::RunningCommandsAtFile(clicked.clone(), FilingKind::Deferred),
    )
    .await;
    assert!(
        matches!(&app.modal, Some(Modal::StopCommandsAndFile(id, FilingKind::Deferred)) if *id == clicked)
    );
    dispatch(&mut app, Message::Dismiss).await;
    assert!(h.load(&clicked).unwrap().snapshot().filing.is_none());
    // The command may already be gone by the time the user confirms.
    dispatch(
        &mut app,
        Message::RunningCommandsAtFile(clicked.clone(), FilingKind::Deferred),
    )
    .await;
    dispatch(&mut app, Message::Confirm).await;
    assert!(app.modal.is_none());
    assert_eq!(
        h.load(&clicked).unwrap().snapshot().filing.unwrap().kind,
        FilingKind::Deferred
    );
    assert_eq!(app.selected, selected);
    assert!(app.current().unwrap().filing.is_none());
}

#[tokio::test]
async fn settlement_buttons_swap_locally_and_capture_only_their_own_clicks() {
    use iced::advanced::Renderer as _;
    use iced_runtime::{UserInterface, user_interface::Cache};
    let mut app = app();
    let path = app.smoke_dir.clone().unwrap();
    stored_session(&mut app, path.clone()).await;
    const BRANCH: &str = "fix/sidebar-session-row-hover-truncation";
    app.sidebar_branches.insert(path, BRANCH.into());
    let id = app.selected.clone().unwrap();
    let mut renderer = header_renderer();
    let size = iced::Size::new(180., 600.);
    for settled in [false, true] {
        if settled {
            dispatch(
                &mut app,
                Message::FileSession(id.clone(), FilingAction::File(FilingKind::Settled)),
            )
            .await;
            dispatch(&mut app, Message::ToggleFiled(FilingKind::Settled)).await;
        }
        let title = app.current().unwrap().title.clone();
        let mut ui = UserInterface::build(app.sidebar(), size, Cache::new(), &mut renderer);
        let mut bounds = UiBounds::default();
        ui.operate(&renderer, &mut bounds);
        // Undo occupies the same right-aligned slot as the age it replaces.
        let action = if settled {
            bounds.label("now")
        } else {
            let label = bounds.label("Settle");
            // Tooltip reports the actual button bounds, including its SVG.
            *bounds
                .containers
                .iter()
                .filter(|b| {
                    b.x < label.x
                        && b.y <= label.y
                        && b.x + b.width >= label.x + label.width
                        && b.y + b.height >= label.y + label.height
                })
                .min_by(|a, b| a.width.total_cmp(&b.width))
                .expect("Settle button")
        };
        let label = bounds.label(&title);
        assert!(action.x >= label.x);
        assert!(action.x + action.width <= size.width);
        if settled {
            let gap = action.x - (label.x + label.width);
            assert!(
                gap >= 0. && gap < action.width,
                "compact titles should reach the age slot with only a small gap, not reserve Settle-label space: {gap}"
            );
        }
        let mut images = vec![];
        for (phase, cursor) in [
            mouse::Cursor::Unavailable,
            mouse::Cursor::Available(label.center()),
            mouse::Cursor::Available(action.center()),
            mouse::Cursor::Unavailable,
        ]
        .into_iter()
        .enumerate()
        {
            let mut messages = vec![];
            ui.update(
                &[iced::Event::Mouse(mouse::Event::CursorMoved {
                    position: cursor.position().unwrap_or(iced::Point::new(400., 0.)),
                })],
                cursor,
                &mut renderer,
                &mut iced::advanced::clipboard::Null,
                &mut messages,
            );
            assert!(messages.is_empty(), "row hover must remain widget-local");
            // Iced resolves button styles on redraw. Layout sensors may also
            // publish their initial measurement here, unrelated to hovering.
            ui.update(
                &[iced::Event::Window(window::Event::RedrawRequested(
                    iced::time::Instant::now(),
                ))],
                cursor,
                &mut renderer,
                &mut iced::advanced::clipboard::Null,
                &mut messages,
            );
            renderer.reset(iced::Rectangle::with_size(size));
            ui.draw(
                &mut renderer,
                &theme::app(),
                &iced::advanced::renderer::Style {
                    text_color: theme::default_colors().text,
                },
                cursor,
            );
            let pixels = crate::test_renderer::screenshot(
                &mut renderer,
                iced::Size::new(size.width as u32, size.height as u32),
                1.,
                theme::default_colors().background,
            );
            if !settled && matches!(phase, 0 | 3) {
                // The long project/branch should paint text in the space
                // normally occupied by Settle, without counting the timer.
                let left = action.x.ceil() as usize;
                let right = bounds.label("now").x.floor() as usize;
                assert!(
                    (action.y.ceil() as usize..(action.y + action.height).floor() as usize).any(
                        |y| {
                            let row = &pixels[(y * size.width as usize + left) * 4
                                ..(y * size.width as usize + right) * 4];
                            let (pixels, _) = row.as_chunks::<4>();
                            pixels.iter().any(|pixel| pixel != &pixels[0])
                        }
                    ),
                    "hidden controls must leave room for project text"
                );
            }
            if !settled && matches!(phase, 1 | 2) {
                let expected = if phase == 1 {
                    theme::default_colors().muted
                } else {
                    theme::default_colors().text
                }
                .into_rgba8();
                let icon_right = bounds.label("Settle").x.floor() as usize;
                let closest = (action.y.ceil() as usize
                    ..(action.y + action.height).floor() as usize)
                    .flat_map(|y| {
                        (action.x.ceil() as usize..icon_right)
                            .map(move |x| (y * size.width as usize + x) * 4)
                    })
                    .map(|offset| {
                        (0..3)
                            .map(|c| pixels[offset + c].abs_diff(expected[c]))
                            .max()
                            .unwrap()
                    })
                    .min()
                    .unwrap();
                assert!(
                    closest < 30,
                    "SVG must inherit the whole button's hover color"
                );
            }
            // Sample just the trailing slot, not the row's hover background.
            let left = action.x.floor() as usize;
            let right = (action.x + action.width).ceil() as usize;
            let top = action.y.floor() as usize;
            let bottom = (action.y + action.height).ceil() as usize;
            images.push(
                (top..bottom)
                    .flat_map(|y| {
                        pixels[(y * size.width as usize + left) * 4
                            ..(y * size.width as usize + right) * 4]
                            .iter()
                            .copied()
                    })
                    .collect::<Vec<_>>(),
            );
        }
        assert!(images[0] != images[1], "row hover replaces age with action");
        assert!(images[1] != images[2], "button hover brightens the action");
        assert!(images[0] == images[3], "leaving restores the age");
        // Reproduce hovering the reserved area beside the button, including
        // leaving the button without leaving its row.
        let button_left = if settled {
            action.x + action.width - crate::session_action::UNDO_SIZE
        } else {
            action.x
        };
        let beside = iced::Point::new(button_left - 1., action.center_y());
        let defer = (!settled).then(|| {
            let clock = bounds
                .containers
                .iter()
                .filter(|b| b.center_y() == action.center_y() && b.x + b.width < action.x)
                .max_by(|a, b| (a.x + a.width).total_cmp(&(b.x + b.width)))
                .expect("defer icon button");
            assert!(
                bounds
                    .labels
                    .iter()
                    .any(|(text, label)| text.ends_with(BRANCH)
                        && (clock.x - label.x - label.width - SESSION_METADATA_GAP).abs() < 0.01),
                "hovered project text should reach the Defer button with only the row gap"
            );
            clock.center()
        });
        let mut no_tip = None;
        for (point, show_tip) in [
            (label.center(), false),
            (beside, false),
            (action.center(), true),
            (beside, false),
        ]
        .into_iter()
        .chain(defer.map(|point| (point, true)))
        {
            let mut messages = vec![];
            ui.update(
                &[
                    iced::Event::Mouse(mouse::Event::CursorMoved { position: point }),
                    iced::Event::Window(window::Event::RedrawRequested(iced::time::Instant::now())),
                ],
                mouse::Cursor::Available(point),
                &mut renderer,
                &mut iced::advanced::clipboard::Null,
                &mut messages,
            );
            ui.draw(
                &mut renderer,
                &theme::app(),
                &iced::advanced::renderer::Style {
                    text_color: theme::default_colors().text,
                },
                mouse::Cursor::Available(point),
            );
            let mut pixels = crate::test_renderer::screenshot(
                &mut renderer,
                iced::Size::new(size.width as u32, size.height as u32),
                1.,
                theme::default_colors().background,
            );
            // Tooltips do not expose text operations. Compare the painted
            // area above the slot, excluding the button's hover-color change.
            let top = (action.center_y() - crate::session_action::HEIGHT / 2.).floor() as usize;
            pixels.truncate(top * size.width as usize * 4);
            if let Some(reference) = &no_tip {
                assert_eq!(
                    pixels != *reference,
                    show_tip,
                    "tooltip must follow the button, not the row or trailing slot"
                );
            } else {
                no_tip = Some(pixels);
            }
        }
        let messages = click_at(&mut ui, &mut renderer, action.center());
        assert!(
            matches!(messages.as_slice(), [Message::FileSession(clicked, action)] if clicked == &id && *action == if settled {FilingAction::Undo} else {FilingAction::File(FilingKind::Settled)}),
            "action must not select the row: {messages:?}"
        );
        if let Some(clock) = defer {
            let messages = click_at(&mut ui, &mut renderer, clock);
            assert!(
                matches!(messages.as_slice(), [Message::FileSession(clicked, FilingAction::File(FilingKind::Deferred))] if clicked == &id)
            );
        }
        let messages = click_at(&mut ui, &mut renderer, label.center());
        assert!(matches!(messages.as_slice(), [Message::Select(clicked)] if clicked == &id));
    }
}

#[tokio::test]
async fn filed_lists_cap_height_scrolls_and_search_reveals_matches_temporarily() {
    use iced_runtime::{UserInterface, user_interface::Cache};
    let mut app = app();
    let mut renderer = header_renderer();
    let count = FILED_VISIBLE_ROWS * 30;
    let pitch = FILED_ROW_HEIGHT + FILED_ROW_GAP;
    let now = orbit_core::store::now();
    app.sessions = (0..count)
        .map(|i| {
            let mut session = Session::new(PathBuf::from("/project"), &app.catalog.settings);
            session.title = format!("Filed session {i}");
            session.updated = now;
            session.filing = Some(Filing {
                kind: FilingKind::Settled,
                at: now - i as u64 * 86400,
            });
            SessionSummary::from(&session)
        })
        .collect();
    for i in (0..count / 2).rev() {
        let mut session = Session::new(PathBuf::from("/later"), &app.catalog.settings);
        session.title = format!("Later session {i}");
        session.filing = Some(Filing {
            kind: FilingKind::Deferred,
            at: now - i as u64 * 3600,
        });
        app.sessions.push(SessionSummary::from(&session));
    }
    app.prepare_sidebar();
    assert_eq!(
        app.filed[FilingKind::Settled as usize].rows,
        (0..count).collect::<Vec<_>>()
    );
    assert!(app.sidebar_rows.is_empty());
    assert!(!app.filed_expanded(FilingKind::Settled));
    assert!(!app.filed_expanded(FilingKind::Deferred));
    dispatch(&mut app, Message::Search("session 1".into())).await;
    for (kind, first) in [
        (FilingKind::Deferred, "Later session 1"),
        (FilingKind::Settled, "Filed session 1"),
    ] {
        assert!(app.filed_expanded(kind));
        let rows = &app.filed[kind as usize].rows;
        assert_eq!(app.sessions[rows[0]].title, first);
        assert!(
            rows.iter()
                .all(|&i| app.sessions[i].title.contains("session 1"))
        );
    }
    dispatch(&mut app, Message::ToggleFiled(FilingKind::Settled)).await;
    assert!(!app.filed_expanded(FilingKind::Settled));
    assert!(app.filed_expanded(FilingKind::Deferred));
    dispatch(&mut app, Message::Search(String::new())).await;
    assert!(!app.filed_expanded(FilingKind::Deferred));
    dispatch(&mut app, Message::ToggleFiled(FilingKind::Deferred)).await;
    assert!(
        !app.filed_expanded(FilingKind::Settled),
        "search must not change the normal expansion preference"
    );
    dispatch(&mut app, Message::ToggleFiled(FilingKind::Settled)).await;

    let mut cache = Cache::new();
    for height in [800., 560.] {
        app.window_height = height;
        app.filed[FilingKind::Settled as usize].list.offset = 0.;
        dispatch(&mut app, Message::ToggleFiled(FilingKind::Deferred)).await;
        let mut ui = UserInterface::build(
            app.sidebar(),
            iced::Size::new(300., height),
            cache,
            &mut renderer,
        );
        let mut before = UiBounds::default();
        ui.operate(&renderer, &mut before);
        cache = ui.into_cache();
        dispatch(&mut app, Message::ToggleFiled(FilingKind::Deferred)).await;
        let mut ui = UserInterface::build(
            app.sidebar(),
            iced::Size::new(300., height),
            cache,
            &mut renderer,
        );
        ui.operate(
            &renderer,
            &mut iced::advanced::widget::operation::scrollable::scroll_to::<()>(
                iced::advanced::widget::Id::new("settled-sessions"),
                scrollable::AbsoluteOffset {
                    x: None,
                    y: Some(0.),
                },
            ),
        );
        let mut bounds = UiBounds::default();
        ui.operate(&renderer, &mut bounds);
        let viewport = bounds.scroll("settled-sessions");
        let previous_viewport = before.scroll("settled-sessions");
        assert_eq!(
            viewport.height, previous_viewport.height,
            "opening Deferred resized Settled"
        );
        // Flex layout can differ by a few f32 rounding bits.
        assert!((viewport.y - previous_viewport.y).abs() < 0.01);
        assert!((bounds.label("Settled").y - before.label("Settled").y).abs() < 0.01);
        assert!(bounds.label("Deferred").y < before.label("Deferred").y);
        let main = bounds.scroll("sessions");
        assert!(viewport.height <= pitch * FILED_VISIBLE_ROWS as f32);
        assert!(viewport.height > pitch && main.height > pitch);
        let deferred = bounds.scroll("deferred-sessions");
        assert!(main.y + main.height <= bounds.label("Deferred").y);
        assert!(deferred.height > pitch && deferred.intersects(&bounds.label("Later session 0")));
        assert!(deferred.y + deferred.height <= bounds.label("Settled").y);
        assert!(viewport.y + viewport.height <= height);
        let settled_section = bounds
            .containers
            .iter()
            .filter(|b| {
                b.contains(bounds.label("Settled").center()) && b.contains(viewport.center())
            })
            .min_by(|a, b| a.height.total_cmp(&b.height))
            .expect("Settled header and list");
        let gap = settled_section.y - (deferred.y + deferred.height);
        let bottom_inset = height - (settled_section.y + settled_section.height);
        assert!(
            (gap - bottom_inset).abs() < 1.,
            "gap={gap}, bottom inset={bottom_inset}"
        );
        assert!(viewport.intersects(&bounds.label("Filed session 0")));
        assert!(bounds.labels.iter().any(|(label, _)| label == "now"));
        assert!(
            app.filed[FilingKind::Settled as usize].list.visible().len() < count / 2,
            "virtualization must bound mounted work"
        );
        let mut messages = vec![];
        let cursor = mouse::Cursor::Available(viewport.center());
        ui.update(
            &[iced::Event::Window(window::Event::RedrawRequested(
                iced::time::Instant::now(),
            ))],
            cursor,
            &mut renderer,
            &mut iced::advanced::clipboard::Null,
            &mut messages,
        );
        messages.clear();
        ui.update(
            &[iced::Event::Mouse(mouse::Event::WheelScrolled {
                delta: mouse::ScrollDelta::Lines { x: 0., y: -3. },
            })],
            cursor,
            &mut renderer,
            &mut iced::advanced::clipboard::Null,
            &mut messages,
        );
        assert!(messages.iter().any(
            |m| matches!(m, Message::FiledScroll(FilingKind::Settled, _, offset, _) if *offset > 0.)
        ));
        assert!(
            !messages
                .iter()
                .any(|m| matches!(m, Message::SidebarScroll(..))),
            "wheel input over settled sessions must not scroll the main list"
        );
        cache = ui.into_cache();

        let offset = app.filed[FilingKind::Settled as usize].list.total() - viewport.height;
        let epoch = app.sidebar_epoch;
        dispatch(
            &mut app,
            Message::FiledScroll(FilingKind::Settled, epoch, offset, viewport.height),
        )
        .await;
        let mut ui = UserInterface::build(
            app.sidebar(),
            iced::Size::new(300., height),
            cache,
            &mut renderer,
        );
        ui.operate(
            &renderer,
            &mut iced::advanced::widget::operation::scrollable::scroll_to::<()>(
                iced::advanced::widget::Id::new("settled-sessions"),
                scrollable::AbsoluteOffset {
                    x: None,
                    y: Some(offset),
                },
            ),
        );
        let mut bounds = UiBounds::default();
        ui.operate(&renderer, &mut bounds);
        let last = bounds.label(&format!("Filed session {}", count - 1));
        assert!(viewport.intersects(&(last - iced::Vector::new(0., offset))));
        assert!(
            bounds
                .labels
                .iter()
                .any(|(label, _)| label == &format!("{}d", count - 1)),
            "age must use the filing time, not the last submission"
        );
        cache = ui.into_cache();
    }
    app.sessions.truncate(2);
    app.prepare_sidebar();
    let mut ui = UserInterface::build(
        app.sidebar(),
        iced::Size::new(300., 560.),
        cache,
        &mut renderer,
    );
    let mut bounds = UiBounds::default();
    ui.operate(&renderer, &mut bounds);
    let viewport = bounds.scroll("settled-sessions");
    assert!((viewport.height - 2. * pitch).abs() < 1.);
}

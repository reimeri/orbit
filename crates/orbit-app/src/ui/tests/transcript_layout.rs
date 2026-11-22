use super::*;

#[tokio::test]
async fn streaming_reparse_invalidates_even_a_zero_length_selection_anchor() {
    let mut app = app();
    let path = app.smoke_dir.clone().unwrap();
    stored_session(&mut app, path).await;
    let h = app.harness.as_ref().unwrap().clone();
    let id = app.selected.clone().unwrap();
    h.edit(&id, |s| {
        s.entries.push(Entry::new(EntryKind::Assistant, "*é"))
    })
    .unwrap();
    dispatch(&mut app, Message::Changed).await;
    // Press between the literal '*' and 'é', before the first drag motion.
    let at = Position {
        entry: 0,
        block: 0,
        byte: 1,
    };
    {
        let mut selection = app.chat_selection.borrow_mut();
        selection.anchor = Some(at);
        selection.focus = Some(at);
        selection.dragging = true;
    }
    h.edit(&id, |s| {
        s.entries.push(Entry::new(
            EntryKind::Reasoning,
            "**Unrelated later activity**",
        ));
        s.entries
            .push(Entry::new(EntryKind::Assistant, "Later response"));
    })
    .unwrap();
    dispatch(&mut app, Message::Changed).await;
    assert_eq!(app.chat_selection.borrow().anchor, Some(at));
    assert!(
        app.chat_selection.borrow().dragging,
        "appends must not interrupt selection of unchanged earlier content"
    );
    h.edit(&id, |s| s.entries[0].text.push('*')).unwrap();
    dispatch(&mut app, Message::Changed).await;
    // The closing '*' reparses the block to just 'é'. Byte 1 is now inside
    // its UTF-8 encoding, so retaining that anchor would panic on Copy.
    assert!(app.chat_selection.borrow().anchor.is_none());
    assert!(!app.chat_selection.borrow().dragging);
}
#[tokio::test]
async fn transcript_layout_work_is_bounded_across_scroll_resize_and_identity_changes() {
    use crate::chat_selection::tests::shaped_paragraphs;
    use iced::advanced::Renderer as _;
    use iced_runtime::{UserInterface, user_interface::Cache};
    let mut app = app();
    let mut session = Session::new(app.smoke_dir.clone().unwrap(), &app.catalog.settings);
    for i in 0..64 {
        session.entries.push(Entry::new(
            EntryKind::Assistant,
            format!("Message {i:03}: asymmetric café text."),
        ));
    }
    app.selected = Some(session.id.clone());
    app.current_session = Some(session);
    app.rebuild_transcript();
    app.transcript.viewport = 600.;
    app.transcript.offset = 1200.;
    app.prepare_visible();
    let mut renderer = crate::test_renderer::new(16.);
    let mut build = |app: &App, cache, width| {
        let before = shaped_paragraphs();
        let mut ui = UserInterface::build(
            app.view(),
            iced::Size::new(width, 900.),
            cache,
            &mut renderer,
        );
        let mut bounds = UiBounds::default();
        ui.operate(&renderer, &mut bounds);
        for &index in &app.transcript.order[app.transcript.visible()] {
            assert!(
                bounds
                    .labels
                    .iter()
                    .any(|(text, _)| text == &app.current().unwrap().entries[index].text),
                "the current viewport must display the current entries"
            );
        }
        ui.operate(
            &renderer,
            &mut iced::advanced::widget::operation::scrollable::scroll_to::<()>(
                iced::advanced::widget::Id::new("chat"),
                scrollable::AbsoluteOffset {
                    x: None,
                    y: Some(app.transcript.offset),
                },
            ),
        );
        renderer.reset(iced::Rectangle::with_size(iced::Size::new(width, 900.)));
        ui.draw(
            &mut renderer,
            &theme::app(),
            &iced::advanced::renderer::Style {
                text_color: theme::default_colors().text,
            },
            mouse::Cursor::Unavailable,
        );
        let shaped = shaped_paragraphs() - before;
        let viewport = bounds
            .scrolls
            .iter()
            .find(|(id, _)| id == &iced::advanced::widget::Id::new("chat"))
            .unwrap()
            .1;
        let text_region = bounds
            .labels
            .iter()
            .filter(|(text, _)| text.starts_with("Message "))
            .find_map(|(_, bounds)| {
                let bounds = *bounds - iced::Vector::new(0., app.transcript.offset);
                if bounds.y < viewport.y || bounds.y + bounds.height > viewport.y + viewport.height
                {
                    return None;
                }
                bounds.intersection(&viewport)
            })
            .expect("a complete transcript message must be visible");
        crate::test_renderer::assert_matches_fresh(
            &mut renderer,
            app.view(),
            iced::Size::new(width, 900.),
            "chat",
            app.transcript.offset,
            text_region,
        );
        (ui.into_cache(), shaped)
    };
    let (mut cache, count) = build(&app, Cache::new(), 1280.);
    assert!(
        count <= app.transcript.visible().len(),
        "shape only mounted entries"
    );
    let visible = app.transcript.visible();
    let row_height =
        app.transcript.prefix(visible.start + 1) - app.transcript.prefix(visible.start);
    app.transcript.offset += row_height;
    app.prepare_visible();
    assert_eq!(app.transcript.visible(), visible.start + 1..visible.end + 1);
    let (next, count) = build(&app, cache, 1280.);
    cache = next;
    assert!(count <= 1, "scrolling reshapes at most the entering entry");
    app.transcript.offset -= row_height;
    app.prepare_visible();
    let (next, count) = build(&app, cache, 1280.);
    cache = next;
    assert!(
        count <= 1,
        "returning to a viewport reuses unchanged survivors"
    );
    assert!(
        app.markdown.len() <= 2 * app.transcript.visible().len(),
        "retained text remains proportional to the viewport, not the history"
    );
    app.view_epoch += 1;
    let (next, count) = build(&app, cache, 1280.);
    cache = next;
    assert_eq!(
        count, 0,
        "measurement identity must not destroy text caches"
    );
    let (next, count) = build(&app, cache, 900.);
    cache = next;
    assert_eq!(count, 0, "width changes resize existing paragraphs");
    let index = app.transcript.order[app.transcript.visible().start];
    let session = app.current_session.as_mut().unwrap();
    session.entries = session
        .entries
        .iter()
        .enumerate()
        .map(|(i, entry)| {
            if i == index {
                Entry::new(
                    entry.kind.clone(),
                    format!("Message {i:03}: replacement café text."),
                )
            } else {
                entry.clone()
            }
        })
        .collect();
    app.prepare_visible();
    let (next, count) = build(&app, cache, 900.);
    cache = next;
    assert!(
        count <= 1,
        "only the replaced entry can require new shaping"
    );
    let id = orbit_core::store::id();
    app.current_session.as_mut().unwrap().id = id.clone();
    app.selected = Some(id);
    let (_, count) = build(&app, cache, 900.);
    assert!(
        count <= app.transcript.visible().len(),
        "shape only mounted entries"
    );
}

// Drive real redraw feedback and scroll operations, without wheel input.
async fn settle_chat(
    app: &mut App,
    mut cache: iced_runtime::user_interface::Cache,
    renderer: &mut iced::Renderer,
    size: iced::Size,
) -> iced_runtime::user_interface::Cache {
    use iced::futures::StreamExt;
    use iced_runtime::UserInterface;
    let mut operations: Vec<Box<dyn iced::advanced::widget::Operation>> = vec![];
    for _ in 0..32 {
        let mut ui = UserInterface::build(app.center(), size, cache, renderer);
        for mut operation in operations.drain(..) {
            ui.operate(renderer, operation.as_mut());
        }
        let mut messages = vec![];
        ui.update(
            &[iced::Event::Window(window::Event::RedrawRequested(
                iced::time::Instant::now(),
            ))],
            mouse::Cursor::Unavailable,
            renderer,
            &mut iced::advanced::clipboard::Null,
            &mut messages,
        );
        cache = ui.into_cache();
        if messages.is_empty() {
            return cache;
        }
        let mut tasks: std::collections::VecDeque<_> = messages
            .into_iter()
            .map(|message| app.update_inner(message))
            .collect();
        while let Some(task) = tasks.pop_front() {
            if let Some(mut stream) = iced_runtime::task::into_stream(task) {
                while let Some(action) = stream.next().await {
                    match action {
                        iced_runtime::Action::Widget(operation) => operations.push(operation),
                        iced_runtime::Action::Output(message) => {
                            tasks.push_back(app.update_inner(message))
                        }
                        _ => panic!("geometry feedback must not cause external side effects"),
                    }
                }
            }
        }
    }
    panic!("transcript geometry did not settle");
}

#[tokio::test]
async fn fitting_chat_expansions_keep_prose_mounted_with_real_viewport_feedback() {
    use iced_runtime::user_interface::Cache;

    let mut app = app();
    let mut session = Session::new(app.smoke_dir.clone().unwrap(), &app.catalog.settings);
    session.entries.push(Entry::new(
        EntryKind::User {
            skills: vec![],
            files: vec![],
            images: vec![],
            sent_at: None,
            steering: false,
        },
        "Please examine these changes.\n\nKeep the first message visible.",
    ));
    session.entries.push(Entry::new(
        EntryKind::Assistant,
        "I will inspect the **implementation**.\n\nThen I will verify the behavior.",
    ));
    let mut groups = vec![];
    let mut prose = vec![0, 1];
    for count in [13, 7] {
        let head = session.entries.len();
        for i in 0..count {
            session.entries.push(Entry::new(
                EntryKind::Reasoning,
                format!("**Checking item {i}**"),
            ));
            session.entries.push(Entry::new(
                EntryKind::Tool {
                    name: "exec_command".into(),
                    input: format!(r#"{{"cmd":"check item {i}"}}"#),
                    output: "Checked".into(),
                    complete: true,
                    failed: false,
                    command_status: None,
                    cell_id: None,
                    parent: None,
                    media: vec![],
                    web: None,
                },
                "",
            ));
        }
        groups.push(session.entries[head].id.clone());
        prose.push(session.entries.len());
        session.entries.push(Entry::new(EntryKind::Assistant,
            "Checks completed.\n\n- First result\n- Second result\nAll expected content remains visible."));
    }
    app.selected = Some(session.id.clone());
    app.current_session = Some(session);
    app.rebuild_transcript();
    app.prepare_visible();
    let mut renderer = crate::test_renderer::new(16.);
    let tall = iced::Size::new(930., 1350.);
    let mut cache = settle_chat(&mut app, Cache::new(), &mut renderer, tall).await;
    assert!(app.transcript.viewport > tall.height / 2. && app.transcript.viewport < tall.height);
    for (group, follow) in groups.iter().zip([false, true]) {
        app.auto_scroll = follow;
        let _ = app.update_inner(Message::ToggleGroup(group.clone()));
        cache = settle_chat(&mut app, cache, &mut renderer, tall).await;
        assert!(
            app.transcript.total() > 720.,
            "fixture must exceed the old fallback window"
        );
        assert!(
            app.transcript.total() < app.transcript.viewport,
            "total={} viewport={}",
            app.transcript.total(),
            app.transcript.viewport
        );
        assert_eq!(app.transcript.offset, 0.);
        assert_eq!(app.transcript.visible(), 0..app.transcript.order.len());
        for &index in &prose {
            assert!(app.transcript.order[app.transcript.visible()].contains(&index));
            let rendered = entry_text(&app, index);
            let entry = &app.current().unwrap().entries[index];
            if matches!(entry.kind, EntryKind::User { .. }) {
                assert_eq!(rendered, entry.text);
            } else {
                assert!(rendered.contains("implementation") || rendered.contains("First result"));
                assert!(
                    !rendered.contains("**") && !rendered.contains("- First result"),
                    "prepared prose must render Markdown, not the raw-source fallback"
                );
            }
        }
        cache = settle_chat(&mut app, cache, &mut renderer, iced::Size::new(930., 500.)).await;
        assert!(
            app.transcript.total() > app.transcript.viewport,
            "the narrow pane overflows"
        );
        cache = settle_chat(&mut app, cache, &mut renderer, tall).await;
        assert_eq!(app.transcript.offset, 0.);
        assert_eq!(app.transcript.visible(), 0..app.transcript.order.len());
        let _ = app.update_inner(Message::ToggleGroup(group.clone()));
        cache = settle_chat(&mut app, cache, &mut renderer, tall).await;
    }
    // Session changes reset the index but can retain equal-sized widget trees.
    app.transcript = crate::virtual_list::List::with_overscan(CHAT_OVERSCAN);
    app.view_epoch += 1;
    let id = orbit_core::store::id();
    app.current_session.as_mut().unwrap().id = id.clone();
    app.selected = Some(id);
    app.rebuild_transcript();
    app.prepare_visible();
    let _ = settle_chat(&mut app, cache, &mut renderer, tall).await;
    assert!(app.transcript.viewport > tall.height / 2. && app.transcript.viewport < tall.height);
    assert_eq!(app.transcript.visible(), 0..app.transcript.order.len());
}

#[tokio::test]
async fn opening_child_at_bottom_measures_offscreen_mounted_rows_without_scrolling() {
    use iced_runtime::{UserInterface, user_interface::Cache};
    use orbit_core::agent::subagents::{Child, Profile};

    let mut app = app();
    let mut session = Session::new(app.smoke_dir.clone().unwrap(), &app.catalog.settings);
    session.child = Some(Child {
        parent_id: "parent".into(),
        profile: Profile::Review,
    });
    // Keep a large history unmounted while the final exchange is measured.
    for i in 0..200 {
        session.entries.push(Entry::new(
            EntryKind::Assistant,
            format!("Historical finding {i}"),
        ));
    }
    for (kind, text) in [
        (
            EntryKind::User {
                skills: vec![],
                files: vec![],
                images: vec![],
                sent_at: None,
                steering: false,
            },
            "Inspect these changes.\n".repeat(6),
        ),
        (EntryKind::Reasoning, "Reviewing the implementation".into()),
        (
            EntryKind::Assistant,
            "An earlier finding with **details**.\n\n".repeat(20),
        ),
        (
            EntryKind::User {
                skills: vec![],
                files: vec![],
                images: vec![],
                sent_at: None,
                steering: false,
            },
            "Please verify the result.\n".repeat(7),
        ),
        (EntryKind::Reasoning, "Checking the regression".into()),
        (
            EntryKind::Assistant,
            "Verification completed.\n\nThe result is consistent.".into(),
        ),
        (EntryKind::Reasoning, "Preparing the final report".into()),
        (
            EntryKind::Assistant,
            "Final report with **verified results**.\n\n".repeat(30),
        ),
    ] {
        session.entries.push(Entry::new(kind, text));
    }
    app.selected = Some(session.id.clone());
    app.current_session = Some(session);
    app.auto_scroll = true;
    app.transcript = crate::virtual_list::List::with_overscan(256.);
    app.rebuild_transcript();
    // Explicit fixture: this mounted reply was estimated too short.
    app.transcript.measure(202, 1.);
    app.transcript.end();
    app.prepare_visible();
    assert!(app.transcript.visible().contains(&202));
    assert!(
        app.transcript.visible().start > 0,
        "historical entries remain virtualized"
    );

    let estimate = app.transcript.prefix(203) - app.transcript.prefix(202);
    let mut renderer = header_renderer();
    let size = iced::Size::new(1462., 1320.);
    let mut ui = UserInterface::build(app.center(), size, Cache::new(), &mut renderer);
    // Opening a child snaps the actual scrollable to the bottom before its
    // first redraw. The earlier tall reply is mounted but above the viewport.
    ui.operate(
        &renderer,
        &mut iced::advanced::widget::operation::scrollable::snap_to::<()>(
            widget::Id::new("chat"),
            scrollable::RelativeOffset::END.into(),
        ),
    );
    let cache = ui.into_cache();
    let _ = settle_chat(&mut app, cache, &mut renderer, size).await;
    assert!(app.auto_scroll);
    assert!(
        app.transcript.prefix(203) - app.transcript.prefix(202) > estimate,
        "the tall offscreen mounted reply must be measured rather than retain its estimate"
    );
    let visible = app.transcript.visible();
    assert_eq!(visible.end, app.transcript.order.len());
    assert!(visible.start > 0, "measurement must retain virtualization");
    assert!(entry_text(&app, 207).contains("Final report with verified results."));
}

#[tokio::test]
async fn chat_measurements_prepare_newly_exposed_rows_without_a_scroll() {
    let mut app = app();
    let mut session = Session::new(app.smoke_dir.clone().unwrap(), &app.catalog.settings);
    for i in 0..30 {
        session.entries.push(Entry::new(
            EntryKind::Assistant,
            format!("**Row {i}** with λ"),
        ));
    }
    app.current_session = Some(session);
    app.auto_scroll = false;
    app.rebuild_transcript();
    app.transcript.viewport = 240.;
    app.prepare_visible();
    let epoch = app.view_epoch;
    dispatch(&mut app, Message::RowSize(epoch, 0, 1000.)).await;
    assert_eq!(app.transcript.visible(), 0..1);
    assert_eq!(entry_text(&app, 0), "Row 0 with λ");
    // Shrinking an entry at the viewport start does not move the anchor,
    // but must still prepare Markdown for the rows that just became visible.
    dispatch(&mut app, Message::RowSize(epoch, 0, 20.)).await;
    assert_eq!(app.transcript.offset, 0.);
    assert!(app.transcript.visible().end > 1);

    for &i in &app.transcript.order[app.transcript.visible()] {
        assert_eq!(entry_text(&app, i), format!("Row {i} with λ"));
    }
    // A scrollbar jump refreshes the narrow window in one update, and end
    // anchoring uses the actual small viewport rather than the 600px fallback.
    dispatch(&mut app, Message::Scroll(epoch, 2020., 240., false)).await;
    assert!(app.transcript.visible().start > 1);
    for &i in &app.transcript.order[app.transcript.visible()] {
        assert_eq!(entry_text(&app, i), format!("Row {i} with λ"));
    }

    app.transcript.end();
    app.prepare_visible();
    assert_eq!(app.transcript.visible().end, app.transcript.order.len());
    assert_eq!(entry_text(&app, 29), "Row 29 with λ");
}

#[tokio::test]
async fn long_transcript_only_caches_viewport_and_appends_preserve_heights() {
    let mut app = app();
    let path = app.smoke_dir.clone().unwrap();
    stored_session(&mut app, path).await;
    let h = app.harness.as_ref().unwrap().clone();
    let id = app.selected.clone().unwrap();
    h.edit(&id, |s| {
        for i in 0..20000 {
            s.entries.push(Entry::new(
                EntryKind::Assistant,
                format!("## Message {i}\n\nDistinct body"),
            ));
        }
    })
    .unwrap();
    dispatch(&mut app, Message::Changed).await;
    assert_eq!(app.transcript.order.len(), 20000);
    assert!(app.markdown.len() < 100);
    app.transcript.measure(19999, 333.);
    let before = app.transcript.total();
    h.edit(&id, |s| {
        s.entries
            .push(Entry::new(EntryKind::Assistant, "new entry"))
    })
    .unwrap();
    dispatch(&mut app, Message::Changed).await;
    assert_eq!(
        app.transcript.prefix(20000),
        before,
        "existing row heights survive append"
    );
    assert_eq!(
        app.transcript.prefix(20000) - app.transcript.prefix(19999),
        333.
    );
    assert!(
        app.transcript.total() > before,
        "appended entry contributes height"
    );
    assert!(app.markdown.len() < 100);
}
#[tokio::test]
async fn popup_transitions_preserve_sidebar_and_chat_scroll() {
    use iced::advanced::widget::{Id, Operation, operation::Scrollable};
    use iced_runtime::{UserInterface, user_interface::Cache};
    #[derive(Default)]
    struct Offsets(Vec<(String, f32)>);
    impl Operation for Offsets {
        fn traverse(&mut self, f: &mut dyn FnMut(&mut dyn Operation)) {
            f(self);
        }
        fn scrollable(
            &mut self,
            id: Option<&Id>,
            _: iced::Rectangle,
            _: iced::Rectangle,
            translation: iced::Vector,
            _: &mut dyn Scrollable,
        ) {
            for name in ["sessions", "chat"] {
                if id == Some(&Id::new(name)) {
                    self.0.push((name.into(), translation.y));
                }
            }
        }
    }
    let mut app = app();
    let mut session = Session::new(app.smoke_dir.clone().unwrap(), &app.catalog.settings);
    for i in 0..64 {
        session.entries.push(Entry::new(
            EntryKind::Assistant,
            format!("Message {i}: asymmetric café text."),
        ));
        app.sessions.push(SessionSummary {
            origin: None,
            child: None,
            id: format!("scroll-session-{i}"),
            title: format!("Session {i}"),
            project: app.smoke_dir.clone().unwrap(),
            updated: 0,
            interrupted: false,
            needs_attention: false,
            unchecked_completion: false,
            filing: None,
        });
    }
    app.selected = Some(session.id.clone());
    app.current_session = Some(session);
    app.rebuild_transcript();
    app.prepare_visible();
    app.prepare_sidebar();
    let mut renderer = header_renderer();
    let size = iced::Size::new(1280., 900.);
    let mut ui = UserInterface::build(app.view(), size, Cache::new(), &mut renderer);
    for (name, y) in [("sessions", 370.), ("chat", 810.)] {
        ui.operate(
            &renderer,
            &mut iced::advanced::widget::operation::scrollable::scroll_to::<()>(
                Id::new(name),
                scrollable::AbsoluteOffset {
                    x: None,
                    y: Some(y),
                },
            ),
        );
    }
    let mut before = Offsets::default();
    ui.operate(&renderer, &mut before);
    assert_eq!(
        before.0,
        vec![("sessions".into(), 370.), ("chat".into(), 810.)]
    );
    let mut cache = ui.into_cache();
    // Keep the real widget cache across every transition. Application-level
    // viewport offsets alone cannot detect scrollable state being discarded.
    for (message, expected_popups) in [
        (
            Message::SessionMenu("scroll-session-0".into(), iced::Point::new(120., 230.)),
            (true, false, false),
        ),
        (Message::CloseMenu, (false, false, false)),
        (
            Message::SessionMenu("scroll-session-0".into(), iced::Point::new(120., 230.)),
            (true, false, false),
        ),
        (
            Message::Rename("scroll-session-0".into()),
            (false, true, false),
        ),
        (Message::Dismiss, (false, false, false)),
        (
            Message::Delete("scroll-session-0".into()),
            (false, true, false),
        ),
        (Message::Dismiss, (false, false, false)),
        (Message::ToggleFooterMenu, (false, false, true)),
        (Message::CloseFooterMenu, (false, false, false)),
    ] {
        let transition = format!("{message:?}");
        let _ = app.update(message);
        assert_eq!(
            (
                app.context_menu.is_some(),
                app.modal.is_some(),
                app.footer_menu_open
            ),
            expected_popups,
            "{transition}",
        );
        let mut ui = UserInterface::build(app.view(), size, cache, &mut renderer);
        let mut offsets = Offsets::default();
        ui.operate(&renderer, &mut offsets);
        assert_eq!(
            offsets.0, before.0,
            "scroll positions changed on {transition}"
        );
        cache = ui.into_cache();
    }
}

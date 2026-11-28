use super::*;

#[tokio::test]
async fn fork_action_opens_an_unsent_draft_and_links_both_sessions() {
    let mut app = app();
    let root = app.smoke_dir.clone().unwrap();
    stored_session(&mut app, root).await;
    let source = app.selected.clone().unwrap();
    type_message(&mut app, "Explore the parser").await;
    send_current(&mut app).await;
    let entry = app.current().unwrap().entries[0].id.clone();
    type_message(&mut app, "Keep this source draft").await;
    let mut renderer = header_renderer();
    let [_, fork_action] = [720., 1800.].map(|width| {
        app.window_width = width;
        let mut ui = iced_runtime::UserInterface::build(
            app.center(),
            iced::Size::new(app.center_dimensions().1, 800.),
            iced_runtime::user_interface::Cache::new(),
            &mut renderer,
        );
        let mut bounds = UiBounds::default();
        ui.operate(&renderer, &mut bounds);
        let prompt = bounds.label("Explore the parser");
        let bubble = bounds
            .containers
            .iter()
            .filter(|b| b.contains(prompt.center()) && b.height > prompt.height)
            .min_by(|a, b| a.height.total_cmp(&b.height))
            .unwrap();
        let composer = bounds.composer.unwrap();
        assert!(
            (bubble.x + bubble.width - composer.x - composer.width).abs() <= 1.,
            "width={width} prompt={prompt:?} bubble={bubble:?} composer={composer:?}"
        );
        assert!(bubble.width < composer.width * 2. / 3.);
        assert!(
            (prompt.x - bubble.x - (bubble.x + bubble.width - prompt.x - prompt.width)).abs() <= 1.,
            "a short prompt should have only its padding on either side"
        );
        let action = bounds
            .containers
            .iter()
            .filter(|b| {
                b.y >= bubble.y + bubble.height
                    && (b.x + b.width - bubble.x - bubble.width).abs() <= 1.
                    && b.width < bubble.width / 2.
            })
            .min_by(|a, b| a.y.total_cmp(&b.y))
            .expect("fork control below the right edge of the prompt");
        let messages = click_at(&mut ui, &mut renderer, action.center());
        assert!(
            matches!(messages.as_slice(), [Message::Fork(s,e)] if s == &source && e == &entry),
            "one click on the controls row should fork: {messages:?}"
        );
        messages.into_iter().next().unwrap()
    });
    dispatch(&mut app, fork_action).await;
    let fork = app.selected.clone().unwrap();
    assert_ne!(source, fork);
    assert_eq!(app.editor.text(), "Explore the parser");
    assert!(app.current().unwrap().entries.is_empty());
    assert!(!app.running());
    let back = {
        let mut renderer = header_renderer();
        let mut ui = iced_runtime::UserInterface::build(
            app.view(),
            iced::Size::new(1200., 800.),
            iced_runtime::user_interface::Cache::new(),
            &mut renderer,
        );
        let mut bounds = UiBounds::default();
        ui.operate(&renderer, &mut bounds);
        let target = bounds.label("View parent");
        click_at(&mut ui, &mut renderer, target.center())
            .into_iter()
            .find(|m| matches!(m,Message::ViewOriginal(s,e) if s==&source && e==&entry))
            .expect("first-prompt fork divider opens the source prompt")
    };
    dispatch(&mut app, back).await;
    assert_eq!(app.selected.as_ref(), Some(&source));
    assert_eq!(app.editor.text(), "Keep this source draft");
    let show_forks = {
        use chrono::TimeZone as _;
        use iced::advanced::Renderer as _;
        let size = iced::Size::new(400., 150.);
        let session = app.current().unwrap();
        let mut example = session.entries[0].clone();
        let EntryKind::User { sent_at, .. } = &mut example.kind else {
            panic!("expected user prompt")
        };
        *sent_at = chrono::Local
            .with_ymd_and_hms(2025, 9, 15, 22, 34, 0)
            .single()
            .unwrap()
            .timestamp() as u64;
        let mut ui = iced_runtime::UserInterface::build(
            app.message_row(session, &example, ActivityRow::Entry(0)),
            size,
            iced_runtime::user_interface::Cache::new(),
            &mut renderer,
        );
        let mut bounds = UiBounds::default();
        ui.operate(&renderer, &mut bounds);
        let count = bounds.label("1 fork");
        let prompt = bounds.label("Explore the parser");
        assert!(count.y >= prompt.y + prompt.height);
        assert!(
            count.x >= size.width / 3. && count.x < prompt.x,
            "the count starts in the two-thirds lane, not at the short bubble's edge"
        );
        let time = bounds.label("Monday 10:34 PM");
        let button = bounds.rightmost_control_at(count.center_y());
        assert!(count.x + count.width <= time.x && time.x + time.width <= button.x);
        assert!((time.center_y() - button.center_y()).abs() <= 1.);
        let bubble = bounds
            .containers
            .iter()
            .filter(|b| b.contains(prompt.center()) && b.height > prompt.height)
            .min_by(|a, b| a.height.total_cmp(&b.height))
            .unwrap();
        assert!(
            (bubble.y - button.height).abs() <= 1.,
            "top margin matches controls row"
        );
        // Inspect the controls band, not the bubble's own hover styling.
        let mut controls_pixels = |cursor| {
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
            let stride = size.width as usize * 4;
            pixels[count.y.ceil() as usize * stride
                ..(count.y + count.height).floor() as usize * stride]
                .to_vec()
        };
        let hidden = controls_pixels(mouse::Cursor::Unavailable);
        let over_prompt = controls_pixels(mouse::Cursor::Available(prompt.center()));
        let over_row = controls_pixels(mouse::Cursor::Available(iced::Point::new(
            size.width / 2.,
            count.center_y(),
        )));
        assert!(
            hidden != over_prompt && over_prompt == over_row,
            "the controls appear in place over either the prompt or the row"
        );
        // A panel resize can leave room for the two icons but not the metadata.
        let narrow = iced::Size::new(3.5 * button.width, 400.);
        let mut compact = iced_runtime::UserInterface::build(
            app.message_row(session, &example, ActivityRow::Entry(0)),
            narrow,
            iced_runtime::user_interface::Cache::new(),
            &mut renderer,
        );
        let mut compact_bounds = UiBounds::default();
        compact.operate(&renderer, &mut compact_bounds);
        let y = compact_bounds.label("Monday 10:34 PM").center_y();
        let target =
            |from_right: f32| iced::Point::new(narrow.width - from_right * button.width, y);
        assert!(
            matches!(click_at(&mut compact, &mut renderer, target(0.5)).as_slice(),
            [Message::Fork(s, e)] if s == &source && e == &entry)
        );
        assert!(
            matches!(click_at(&mut compact, &mut renderer, target(1.5)).as_slice(),
            [Message::Copy(value)] if value == "Explore the parser")
        );
        click_at(&mut ui, &mut renderer, count.center())
            .into_iter()
            .find(|m| matches!(m, Message::ShowForks(Some(e)) if e == &entry))
            .expect("fork count opens the branches from this prompt")
    };
    dispatch(&mut app, show_forks).await;
    assert!(matches!(app.right_panel, Some(RightPanel::Forks(_))) && app.modal.is_none());
    let open = {
        let mut renderer = header_renderer();
        let mut ui = iced_runtime::UserInterface::build(
            app.view(),
            iced::Size::new(1200., 800.),
            iced_runtime::user_interface::Cache::new(),
            &mut renderer,
        );
        let mut bounds = UiBounds::default();
        ui.operate(&renderer, &mut bounds);
        assert!(
            bounds
                .labels
                .iter()
                .any(|(label, _)| label == "From this prompt")
        );
        assert!(bounds.labels.iter().any(|(label, _)| label == "Original"));
        let picker = bounds
            .scrolls
            .iter()
            .find(|(id, _)| *id == iced::advanced::widget::Id::new("fork-list"))
            .expect("fork picker");
        let target = bounds
            .labels
            .iter()
            .find(|(label, rect)| {
                label == "Fork · Explore the parser" && picker.1.contains(rect.center())
            })
            .expect("branch in picker")
            .1;
        click_at(&mut ui, &mut renderer, target.center())
            .into_iter()
            .find(|m| matches!(m, Message::Select(id) if id == &fork))
            .expect("click on branch opens fork")
    };
    dispatch(&mut app, open).await;
    assert_eq!(app.selected.as_ref(), Some(&fork));
    assert_eq!(app.editor.text(), "Explore the parser");
    assert!(
        matches!(app.right_panel, Some(RightPanel::Forks(_))),
        "stay open while switching within the family"
    );
    let other = app.smoke_dir.clone().unwrap();
    stored_session(&mut app, other).await;
    assert!(!matches!(app.right_panel, Some(RightPanel::Forks(_))) && app.fork_family().is_none());
}

#[tokio::test]
async fn fork_separator_marks_the_inherited_boundary_with_and_without_new_messages() {
    let mut app = app();
    let root = app.smoke_dir.clone().unwrap();
    stored_session(&mut app, root.clone()).await;
    let parent = app.selected.clone().unwrap();
    let h = app.harness.as_ref().unwrap().clone();
    let mut fork = Session::new(root, &app.catalog.settings);
    fork.entries.push(Entry::new(
        EntryKind::Reply { final_at: None },
        "Inherited reply",
    ));
    fork.origin = Some(orbit_core::store::ForkOrigin {
        family: parent.clone(),
        parent: parent.clone(),
        entry: "parent-prompt".into(),
        excerpt: "Choose an approach".into(),
        inherited: 1,
    });
    let id = fork.id.clone();
    h.store.save_session(&fork).unwrap();
    h.insert(fork);
    dispatch(&mut app, Message::Select(id.clone())).await;
    let transcript = chat_text(&app);
    assert!(transcript.find("Inherited reply").unwrap() < transcript.find("Forked here").unwrap());
    h.edit(&id, |s| s.accept("Only on this branch".into()))
        .unwrap();
    dispatch(&mut app, Message::Changed).await;
    let transcript = chat_text(&app);
    let marker = transcript.find("Forked here").unwrap();
    assert_eq!(transcript.matches("Forked here").count(), 1);
    assert!(
        transcript.find("Inherited reply").unwrap() < marker
            && marker < transcript.find("Only on this branch").unwrap()
    );
    dispatch(&mut app, Message::ShowForks(None)).await;
    h.remove(&parent).unwrap();
    dispatch(&mut app, Message::Changed).await;
    assert!(!matches!(app.right_panel, Some(RightPanel::Forks(_))));
    assert!(chat_text(&app).contains("Parent deleted"));
}

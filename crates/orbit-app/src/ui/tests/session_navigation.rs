use super::*;
use orbit_core::models::{ASTRA, LUNA};

#[tokio::test]
async fn deleting_populated_session_then_opening_draft_clears_transcript() {
    let mut app = app();
    let path = app.smoke_dir.clone().unwrap();
    stored_session(&mut app, path.clone()).await;
    let id = app.selected.clone().unwrap();
    app.harness
        .as_ref()
        .unwrap()
        .edit(&id, |s| {
            s.entries.push(Entry::new(
                EntryKind::User {
                    skills: vec![],
                    files: vec![],
                    images: vec![],
                    sent_at: None,
                    steering: false,
                },
                "Old message",
            ));
        })
        .unwrap();
    dispatch(&mut app, Message::Changed).await;
    assert!(chat_text(&app).contains("Old message"));
    dispatch(&mut app, Message::Delete(id)).await;
    dispatch(&mut app, Message::Confirm).await;
    dispatch(&mut app, Message::Create(path.clone())).await;
    let draft = chat_text(&app);
    assert!(!draft.contains("Old message"));
    assert_eq!(app.current().unwrap().project, path);
}

#[tokio::test]
async fn child_navigation_preserves_parent_draft_scroll_and_disables_child_edits() {
    use orbit_core::agent::subagents::{Child, Profile, RunInfo, RunStatus};
    let mut app = app();
    let path = app.smoke_dir.clone().unwrap();
    stored_session(&mut app, path.clone()).await;
    let parent = app.selected.clone().unwrap();
    let h = app.harness.as_ref().unwrap().clone();
    h.edit(&parent, |s| {
        s.title = "Parent task".into();
        s.entries.push(Entry {
            id: "subagent-earlier".into(),
            kind: EntryKind::Notice,
            text: "Earlier completion".into(),
        });
        for n in 0..60 {
            s.entries.push(Entry::new(
                EntryKind::Assistant,
                format!("Historical parent message {n}"),
            ));
        }
    })
    .unwrap();
    dispatch(&mut app, Message::Changed).await;
    type_message(&mut app, "Keep this unsent parent draft λ").await;
    let epoch = app.view_epoch;
    dispatch(&mut app, Message::Scroll(epoch, 437., 300., false)).await;
    let parent_offset = app.transcript.offset;
    let mut child = Session::new(path, &app.catalog.settings);
    child.child = Some(Child {
        parent_id: parent.clone(),
        profile: Profile::Explore,
    });
    child.title = "Inspect cancellation".into();
    child.model = ASTRA.id.into();
    child.effort = "max".into();
    child.entries.push(Entry::new(
        EntryKind::User {
            skills: vec![],
            files: vec![],
            images: vec![],
            sent_at: None,
            steering: false,
        },
        "Inspect without edits",
    ));
    child
        .entries
        .push(Entry::new(EntryKind::Assistant, "Final inspection result"));
    let child_id = child.id.clone();
    h.store.save_session(&child).unwrap();
    let finished = orbit_core::store::now();
    let run = RunInfo {
        configuration: Some(orbit_core::store::SubagentSettings {
            model: child.model.clone(),
            effort: child.effort.clone(),
            fast: child.fast,
        }),
        run_id: orbit_core::store::id(),
        session_id: child_id.clone(),
        parent_id: parent.clone(),
        request_id: "fixture-spawn".into(),
        profile: Profile::Explore,
        title: child.title.clone(),
        status: RunStatus::Completed,
        assignment: 2,
        started: finished - 17,
        finished: Some(finished),
        notified: true,
        attention_acknowledged: false,
    };
    let db = rusqlite::Connection::open(h.store.root().join("sessions.sqlite3")).unwrap();
    let earlier = RunInfo {
        assignment: 1,
        run_id: "earlier".into(),
        request_id: "earlier-spawn".into(),
        title: "Inspect retry".into(),
        ..run.clone()
    };
    for run in [earlier, run] {
        db.execute(
            "INSERT INTO subagent_runs (id,parent,session,request,metadata,result)
             VALUES (?1,?2,?3,?4,?5,?6)",
            rusqlite::params![
                run.run_id,
                parent,
                child_id,
                run.request_id,
                serde_json::to_string(&run).unwrap(),
                "Final inspection result"
            ],
        )
        .unwrap();
    }
    app.harness = Some(Harness::new(h.store.clone(), h.store.load().unwrap().1).unwrap());
    app.catalog_revision = u64::MAX;
    dispatch(&mut app, Message::ToggleAgents).await;
    assert!(app.agents_open && !app.settings_open);
    // A terminal run must not display stale runtime activity. The entire
    // card, including its padding outside the labels, opens the child.
    let inactive_child = app.harness.as_ref().unwrap().load(&child_id).unwrap();
    inactive_child.cancel();
    assert_eq!(inactive_child.status(), ConversationStatus::Stopped);
    let mut renderer = header_renderer();
    let messages = {
        let mut ui = iced_runtime::UserInterface::build(
            app.agents_panel(),
            iced::Size::new(480., 560.),
            iced_runtime::user_interface::Cache::new(),
            &mut renderer,
        );
        let mut bounds = UiBounds::default();
        ui.operate(&renderer, &mut bounds);
        assert!(
            !bounds
                .labels
                .iter()
                .any(|(label, _)| { matches!(label.as_str(), "Open" | "Stop" | "Stopped") })
        );
        let profile = bounds.label("Explore");
        let title = bounds.label("Inspect cancellation");
        let card = bounds
            .containers
            .iter()
            .filter(|b| b.contains(profile.center()) && b.contains(title.center()))
            .min_by(|a, b| (a.width * a.height).total_cmp(&(b.width * b.height)))
            .unwrap();
        let padding = iced::Point::new((card.x + profile.x) / 2., (card.y + profile.y) / 2.);
        let mut messages = Vec::new();
        ui.update(
            &[
                iced::Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
                iced::Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
            ],
            mouse::Cursor::Available(padding),
            &mut renderer,
            &mut iced::advanced::clipboard::Null,
            &mut messages,
        );
        messages
    };
    assert!(matches!(messages.as_slice(), [Message::Select(id)] if id == &child_id));
    for message in messages {
        dispatch(&mut app, message).await;
    }
    assert_eq!(app.parent_id(), Some(parent.as_str()));
    assert!(
        app.sidebar_rows
            .iter()
            .filter_map(|row| match row {
                SidebarRow::Session(i, _) => Some(&app.sessions[*i]),
                _ => None,
            })
            .all(|s| s.child.is_none())
    );
    assert_eq!(
        app.harness
            .as_ref()
            .unwrap()
            .store
            .load_session(&parent)
            .unwrap()
            .draft,
        "Keep this unsent parent draft λ"
    );
    let original = app.current().unwrap().clone();
    type_message(&mut app, "Forbidden direct child edit").await;
    dispatch(&mut app, Message::SessionModel(LUNA.id.into())).await;
    send_current(&mut app).await;
    assert!(app.editor.text().is_empty());
    assert_eq!(app.current().unwrap().model, original.model);
    assert_eq!(app.current().unwrap().entries.len(), original.entries.len());
    let mut renderer = header_renderer();
    for (width, height) in [(1200., 800.), (860., 560.)] {
        app.window_width = width;
        app.window_height = height;
        let mut ui = iced_runtime::UserInterface::build(
            app.view(),
            iced::Size::new(width, height),
            iced_runtime::user_interface::Cache::new(),
            &mut renderer,
        );
        let mut bounds = UiBounds::default();
        ui.operate(&renderer, &mut bounds);
        let profile = bounds.label("Explore");
        let status = bounds.label("Completed");
        let title = bounds.label("Inspect cancellation");
        let metadata = bounds.label("Astra · max · 17s");
        assert!((profile.center_y() - status.center_y()).abs() <= 1.);
        assert!((title.center_y() - metadata.center_y()).abs() <= 1.);
        assert!(title.y > profile.y);
        assert!(title.x + title.width <= metadata.x);
        assert!((status.x + status.width - metadata.x - metadata.width).abs() <= 1.);
        for label in ["Agents", "Completed", "Explore", "Inspect cancellation"] {
            let rect = bounds.label(label);
            assert!(
                rect.x >= 0. && rect.x + rect.width <= width,
                "{label} clipped at {width}: {rect:?}"
            );
            assert!(
                rect.y >= 0. && rect.y + rect.height <= height,
                "{label} clipped vertically"
            );
        }
    }
    // Measure the actual available center width, independently of side panels.
    let conversation = app.harness.as_ref().unwrap().get(&child_id).unwrap();
    for (width, running, wide) in [
        (1000., false, true),
        (1000., true, true),
        (320., true, false),
        (320., false, false),
    ] {
        conversation.running.store(running, Ordering::Release);
        let mut ui = iced_runtime::UserInterface::build(
            app.center(),
            iced::Size::new(width, 560.),
            iced_runtime::user_interface::Cache::new(),
            &mut renderer,
        );
        let mut bounds = UiBounds::default();
        ui.operate(&renderer, &mut bounds);
        let status = bounds.label("●");
        let stop = bounds.rightmost_control_at(status.center_y());
        assert_eq!(status.center_y(), stop.center_y());
        assert!(status.x + status.width < stop.x);
        assert!(stop.x + stop.width <= width);
        let title = "Explore · Inspect cancellation";
        for label in [title, "Back to main agent"] {
            assert_eq!(bounds.labels.iter().any(|(s, _)| s == label), wide);
        }
        if wide {
            let title = bounds.label(title);
            let back = bounds.label("Back to main agent");
            assert!((title.center_x() - width / 2.).abs() < 1.);
            assert!((title.center_y() - stop.center_y()).abs() < 1.);
            assert!((back.center_y() - stop.center_y()).abs() < 1.);
            assert!(back.x + back.width < title.x);
            assert!(title.x + title.width < status.x);
        }
        assert!(!bounds.labels.iter().any(|(s, _)| {
            s == "Completed"
                || s == "Stop"
                || s.contains("Read-only profile instructions")
                || s.contains("Parent task")
        }));
        let back = bounds
            .containers
            .iter()
            .filter(|rect| rect.center_y() == stop.center_y() && rect.x + rect.width < status.x)
            .min_by(|a, b| a.width.total_cmp(&b.width))
            .expect("back button content");
        for (point, back) in [(back.center(), true), (stop.center(), false)] {
            let mut messages = Vec::new();
            ui.update(
                &[
                    iced::Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
                    iced::Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
                ],
                mouse::Cursor::Available(point),
                &mut renderer,
                &mut iced::advanced::clipboard::Null,
                &mut messages,
            );
            if back {
                assert!(matches!(messages.as_slice(), [Message::Select(id)] if id == &parent));
            } else if running {
                assert!(matches!(messages.as_slice(), [Message::StopAgent(id)] if id == &child_id));
            } else {
                assert!(messages.is_empty(), "completed child must not be stoppable");
            }
        }
    }
    conversation.running.store(false, Ordering::Release);
    dispatch(&mut app, Message::Select(parent.clone())).await;
    assert_eq!(app.editor.text(), "Keep this unsent parent draft λ");
    assert!(!app.auto_scroll);
    assert_eq!(app.transcript.offset, parent_offset);
    // Expanding a historical notice resolves that assignment, not its successor.
    let epoch = app.view_epoch;
    dispatch(&mut app, Message::Scroll(epoch, 0., 300., false)).await;
    dispatch(&mut app, Message::ToggleEntry("subagent-earlier".into())).await;
    let labels = chat_text(&app);
    assert!(labels.contains("Inspect retry") && labels.contains("Open subagent"));
    dispatch(&mut app, Message::ToggleSettings).await;
    assert!(app.settings_open && !app.agents_open);
    dispatch(&mut app, Message::ToggleAgents).await;
    assert!(app.agents_open && !app.settings_open);
    dispatch(&mut app, Message::Select(child_id.clone())).await;
    dispatch(&mut app, Message::Delete(parent.clone())).await;
    dispatch(&mut app, Message::Confirm).await;
    assert!(app.selected.is_none());
    assert!(app.current().is_none());
    assert!(
        app.harness
            .as_ref()
            .unwrap()
            .store
            .load_session(&child_id)
            .is_err()
    );
}

#[tokio::test]
async fn clean_cold_switch_does_not_wait_for_a_database_writer() {
    let mut app = app();
    let store = app.harness.as_ref().unwrap().store.clone();
    let root = app.smoke_dir.clone().unwrap();
    let outgoing = Session::new(root.clone(), &app.catalog.settings);
    let mut incoming = Session::new(root, &app.catalog.settings);
    incoming.draft = "destination draft".into();
    store.save_session(&outgoing).unwrap();
    store.save_session(&incoming).unwrap();
    dispatch(&mut app, Message::Select(outgoing.id)).await;

    // WAL readers can see the last committed version while another writer is active.
    let writer = rusqlite::Connection::open(store.root().join("sessions.sqlite3")).unwrap();
    writer.execute_batch("BEGIN IMMEDIATE").unwrap();
    let start = std::time::Instant::now();
    let pending = app.update(Message::Select(incoming.id.clone()));
    use iced::futures::StreamExt;
    let mut actions = iced_runtime::task::into_stream(pending).unwrap();
    while let Some(action) = actions.next().await {
        if let iced_runtime::Action::Output(message) = action {
            let selected = matches!(message, Message::Selected(_));
            let task = app.update(message);
            if selected {
                assert_eq!(app.selected.as_ref(), Some(&incoming.id), "{:?}", app.error);
                eprintln!(
                    "cold switch to destination composer with SQLite writer held: {:?}",
                    start.elapsed()
                );
            }
            drain(&mut app, task).await;
        }
    }
    assert_eq!(app.editor.text(), "destination draft");
    assert!(app.error.is_none(), "{:?}", app.error);
}

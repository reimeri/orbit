use super::*;

#[tokio::test]
async fn activity_disclosure_swaps_on_header_hover_and_completion() {
    use iced::advanced::Renderer as _;
    use iced_runtime::{UserInterface, user_interface::Cache};
    let mut app = app();
    let path = app.smoke_dir.clone().unwrap();
    stored_session(&mut app, path).await;
    let id = app.selected.clone().unwrap();
    let harness = app.harness.as_ref().unwrap().clone();
    let mut renderer = header_renderer();
    for width in [360., 860.] {
        let mut frames = Vec::new();
        let mut cache = Cache::new();
        // Sample activity over time without specifying its exact waveform/period.
        for tick in 0..16 {
            app.tick = tick;
            let mut icons = Vec::new();
            for open in [false, true] {
                for complete in [false, true] {
                    harness
                        .edit(&id, |s| {
                            s.entries = vec![Entry {
                                id: "group".into(),
                                text: String::new(),
                                kind: EntryKind::Tool {
                                    name: "exec_command".into(),
                                    input: r#"{"cmd":"sleep 1"}"#.into(),
                                    output: String::new(),
                                    complete,
                                    failed: false,
                                    command_status: None,
                                    cell_id: None,
                                    parent: None,
                                    media: vec![],
                                    web: None,
                                },
                            }]
                            .into();
                        })
                        .unwrap();
                    dispatch(&mut app, Message::Changed).await;
                    if open {
                        app.details.groups.insert("group".into());
                    } else {
                        app.details.groups.remove("group");
                    }
                    let size = iced::Size::new(width, 100.);
                    let mut ui = UserInterface::build(
                        app.activity_row(ActivityRow::Activity(0)),
                        size,
                        cache,
                        &mut renderer,
                    );
                    let mut bounds = UiBounds::default();
                    ui.operate(&renderer, &mut bounds);
                    // Compare only the disclosure area before the first label,
                    // at the actual header height, not a fixed icon-size crop.
                    let icon_width = bounds
                        .labels
                        .iter()
                        .map(|(_, b)| b.x)
                        .min_by(f32::total_cmp)
                        .unwrap()
                        .floor() as usize;
                    let header_height = bounds
                        .containers
                        .iter()
                        .map(|b| b.y + b.height)
                        .max_by(f32::total_cmp)
                        .unwrap()
                        .ceil() as usize;
                    for cursor in [
                        mouse::Cursor::Unavailable,
                        // Hover the far edge, including the header's padding, not the icon.
                        mouse::Cursor::Available(iced::Point::new(width - 1., 1.)),
                        mouse::Cursor::Unavailable,
                    ] {
                        let mut messages = Vec::new();
                        ui.update(
                            &[iced::Event::Mouse(mouse::Event::CursorMoved {
                                position: cursor
                                    .position()
                                    .unwrap_or(iced::Point::new(width + 1., 1.)),
                            })],
                            cursor,
                            &mut renderer,
                            &mut iced::advanced::clipboard::Null,
                            &mut messages,
                        );
                        assert!(messages.is_empty(), "hover must remain widget-local");
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
                            iced::Size::new(width as u32, size.height as u32),
                            1.,
                            theme::default_colors().background,
                        );
                        icons.push(
                            pixels
                                .chunks_exact(width as usize * 4)
                                .take(header_height)
                                .flat_map(|row| row[..icon_width * 4].iter().copied())
                                .collect::<Vec<_>>(),
                        );
                    }
                    assert!(matches!(
                        click_at(&mut ui, &mut renderer, iced::Point::new(width - 1., 1.)).as_slice(),
                        [Message::ToggleGroup(id)] if id == "group"
                    ));
                    cache = ui.into_cache();
                }
            }
            for states in icons.as_chunks::<6>().0 {
                assert_ne!(states[0], states[1], "working circle must become a chevron");
                assert_eq!(states[0], states[2], "leaving restores the circle");
                assert_eq!(states[1], states[3], "completion shows the same chevron");
                assert_eq!(
                    states[3], states[4],
                    "completed hover does not change the icon"
                );
                assert_eq!(states[3], states[5]);
            }
            assert_ne!(icons[3], icons[9], "open and closed chevrons differ");
            frames.push(icons);
        }
        for index in [0, 2, 6, 8] {
            assert!(
                frames.iter().any(|frame| frame[index] != frames[0][index]),
                "working circle animates"
            );
        }
        for index in [1, 3, 4, 5, 7, 9, 10, 11] {
            assert!(
                frames.iter().all(|frame| frame[index] == frames[0][index]),
                "hovered and completed chevrons must not animate"
            );
        }
    }
}

#[test]
fn tool_dots_share_status_style_and_track_command_processes() {
    use iced::advanced::Renderer;
    use iced_runtime::{UserInterface, user_interface::Cache};
    let app = app();
    let mut renderer = header_renderer();
    let colors = theme::default_colors();
    for (name, status, complete, failed, color, label) in [
        (
            "image_gen__imagegen",
            None,
            false,
            false,
            colors.warning,
            "running",
        ),
        ("image_gen__imagegen", None, true, false, colors.success, ""),
        (
            "image_gen__imagegen",
            None,
            true,
            true,
            colors.danger,
            "failed",
        ),
        ("web__run", None, false, false, colors.warning, "running"),
        ("exec", None, false, false, colors.warning, "running"),
        (
            "exec_command",
            None,
            false,
            false,
            colors.warning,
            "running",
        ),
        (
            "exec_command",
            Some(CommandStatus::Running),
            true,
            false,
            colors.warning,
            "running",
        ),
        (
            "exec_command",
            Some(CommandStatus::Succeeded),
            true,
            false,
            colors.success,
            "",
        ),
        (
            "exec_command",
            Some(CommandStatus::Failed),
            true,
            false,
            colors.danger,
            "failed",
        ),
        (
            "exec_command",
            Some(CommandStatus::Interrupted),
            true,
            false,
            colors.danger,
            "interrupted",
        ),
    ] {
        let entry = Entry {
            id: "command".into(),
            text: String::new(),
            kind: EntryKind::Tool {
                name: name.into(),
                input: r#"{"cmd":"build"}"#.into(),
                output: "Process running with session ID 123".into(),
                complete,
                failed,
                command_status: status,
                cell_id: None,
                parent: Some("outer".into()),
                media: vec![],
                web: None,
            },
        };
        let size = iced::Size::new(400., 80.);
        let mut ui =
            UserInterface::build(app.entry_view(&entry, 0), size, Cache::new(), &mut renderer);
        let mut bounds = UiBounds::default();
        ui.operate(&renderer, &mut bounds);
        let dot = bounds.label("●");
        if !label.is_empty() {
            bounds.label(label);
        }
        renderer.reset(iced::Rectangle::with_size(size));
        ui.draw(
            &mut renderer,
            &theme::app(),
            &iced::advanced::renderer::Style {
                text_color: colors.text,
            },
            mouse::Cursor::Unavailable,
        );
        let pixels = crate::test_renderer::screenshot(
            &mut renderer,
            iced::Size::new(400, 80),
            1.,
            colors.background,
        );
        let expected = color.into_rgba8();
        assert!(
            (dot.y.floor() as usize..(dot.y + dot.height).ceil() as usize).any(|y| {
                (dot.x.floor() as usize..(dot.x + dot.width).ceil() as usize).any(|x| {
                    let pixel = &pixels[(y * 400 + x) * 4..][..3];
                    pixel
                        .iter()
                        .zip(&expected[..3])
                        .all(|(a, b)| a.abs_diff(*b) <= 10)
                })
            }),
            "{name} dot must use the theme's {label:?} color"
        );
    }
}

#[test]
fn tool_header_stays_fixed_above_padded_details_when_toggled() {
    use iced_runtime::{UserInterface, user_interface::Cache};
    let mut app = app();
    let entry = Entry {
        id: "stationary-tool".into(),
        text: String::new(),
        kind: EntryKind::Tool {
            name: "exec_command".into(),
            input: r#"{"cmd":"printf 'λ'"}"#.into(),
            output: "λ".into(),
            complete: true,
            failed: false,
            command_status: None,
            cell_id: None,
            parent: Some("parent-call".into()),
            media: vec![],
            web: None,
        },
    };
    let mut renderer = header_renderer();
    for width in [360., 860.] {
        let mut cache = Cache::new();
        let mut header = None;
        for open in [false, true, false] {
            if open {
                app.details.expanded.insert(entry.id.clone());
                app.outputs.insert(
                    entry.id.clone(),
                    tool_output::Output::new("exec_command", r#"{"cmd":"printf 'λ'"}"#, "λ", false),
                );
            } else {
                app.details.expanded.remove(&entry.id);
                app.outputs.remove(&entry.id);
            }
            let mut ui = UserInterface::build(
                app.entry_view(&entry, 0),
                iced::Size::new(width, 600.),
                cache,
                &mut renderer,
            );
            let mut bounds = UiBounds::default();
            ui.operate(&renderer, &mut bounds);
            let current = (bounds.label("exec_command"), bounds.label("●"));
            if let Some(header) = header {
                assert_eq!(
                    current, header,
                    "expansion must not shift or resize the header"
                );
            } else {
                header = Some(current);
            }
            if open {
                assert!(bounds.label("Command").y > current.0.y + current.0.height);
                assert!(bounds.label("Code execution: parent-call").y > bounds.label("Output").y);
            } else {
                assert!(!bounds.labels.iter().any(|(label, _)| label == "Command"));
            }
            assert!(
                matches!(click_at(&mut ui, &mut renderer, current.0.center()).as_slice(), [Message::ToggleEntry(id)] if id == &entry.id)
            );
            cache = ui.into_cache();
        }
    }
}

#[tokio::test]
async fn formatted_tool_copy_and_raw_disclosure_survive_viewport_eviction() {
    let mut app = app();
    let path = app.smoke_dir.clone().unwrap();
    stored_session(&mut app, path).await;
    let id = app.selected.clone().unwrap();
    let h = app.harness.as_ref().unwrap().clone();
    let input = r#"{ "cmd": "printf 'λ\n'", "workdir": "/tmp/a b" }"#;
    let output = "  result β\tend\r\n";
    h.edit(&id, |s| {
        s.entries = vec![
            Entry {
                id: "formatted-tool".into(),
                text: String::new(),
                kind: EntryKind::Tool {
                    name: "exec_command".into(),
                    input: input.into(),
                    output: output.into(),
                    complete: true,
                    failed: false,
                    command_status: None,
                    cell_id: None,
                    parent: Some("outer-call".into()),
                    media: vec![],
                    web: None,
                },
            },
            Entry::new(EntryKind::Assistant, "After the tool."),
        ]
        .into();
    })
    .unwrap();
    dispatch(&mut app, Message::Changed).await;
    dispatch(&mut app, Message::ToggleEntry("formatted-tool".into())).await;
    let formatted =
        "Command\nprintf 'λ\n'\n\nArguments\nworkdir: /tmp/a b\n\nOutput\n  result β\tend\r\n";
    for raw in [false, true, false] {
        if app.details.raw_tool_inputs.contains("formatted-tool") != raw {
            dispatch(&mut app, Message::ToggleToolRaw("formatted-tool".into())).await;
            assert!(
                app.chat_selection.borrow().range().is_none(),
                "format changes invalidate old byte positions"
            );
        }
        let expected = if raw {
            format!("Raw input\n{input}\n\n{formatted}")
        } else {
            formatted.into()
        };
        // Evict the rendered tool cache: Copy must reconstruct the same document.
        app.transcript.viewport = 1.;
        app.transcript.measure(1, 2000.);
        app.transcript.end();
        app.prepare_visible();
        assert!(!app.outputs.contains_key("formatted-tool"));
        {
            let mut selection = app.chat_selection.borrow_mut();
            selection.anchor = Some(Position {
                entry: 0,
                block: 0,
                byte: 0,
            });
            selection.focus = Some(Position {
                entry: 0,
                block: 1,
                byte: "Code execution: outer-call".len(),
            });
        }
        assert_eq!(
            copied_text(app.update_inner(Message::CopySelection)).await,
            format!("{expected}\n\nCode execution: outer-call")
        );
    }
    let saved = h.store.load_session(&id).unwrap();
    assert!(
        matches!(&saved.entries[0].kind, EntryKind::Tool { input: original, output: result, .. } if original == input && result == output)
    );
}

#[tokio::test]
async fn activity_expansion_streaming_selection_and_details_preserve_source_entries() {
    let mut app = app();
    let path = app.smoke_dir.clone().unwrap();
    stored_session(&mut app, path).await;
    let id = app.selected.clone().unwrap();
    let h = app.harness.as_ref().unwrap().clone();
    h.edit(&id, |s| {
        s.entries = vec![
            Entry {
                id: "r1".into(),
                kind: EntryKind::Reasoning,
                text: "**First title**\n\nFirst full body λ".into(),
            },
            Entry {
                id: "r2".into(),
                kind: EntryKind::Reasoning,
                text: "**Second title**\n\nSecond full body β".into(),
            },
            Entry {
                id: "outer".into(),
                text: String::new(),
                kind: EntryKind::Tool {
                    name: "exec".into(),
                    input: "await tools.exec_command({cmd:'exit 7'})".into(),
                    output: "Script completed".into(),
                    complete: true,
                    failed: false,
                    command_status: None,
                    cell_id: None,
                    parent: None,
                    media: vec![],
                    web: None,
                },
            },
            Entry {
                id: "shell".into(),
                text: String::new(),
                kind: EntryKind::Tool {
                    name: "exec_command".into(),
                    input: r#"{"cmd":"exit 7"}"#.into(),
                    output: String::new(),
                    complete: false,
                    failed: false,
                    command_status: None,
                    cell_id: None,
                    parent: Some("outer".into()),
                    media: vec![],
                    web: None,
                },
            },
            Entry::new(EntryKind::Assistant, "I will examine the result."),
        ]
        .into();
    })
    .unwrap();
    dispatch(&mut app, Message::Changed).await;
    let collapsed = chat_text(&app);
    assert!(collapsed.contains("I will examine the result."));
    assert!(!collapsed.contains("First full body λ") && !collapsed.contains("Second full body β"));
    dispatch(&mut app, Message::ToggleGroup("r1".into())).await;
    dispatch(&mut app, Message::ToggleEntry("r1".into())).await;
    let expanded = chat_text(&app);
    assert!(expanded.contains("First full body λ") && expanded.contains("Second full body β"));
    assert!(
        !expanded.contains("await tools.exec_command"),
        "successful wrapper is secondary"
    );
    {
        let mut selection = app.chat_selection.borrow_mut();
        selection.anchor = Some(Position {
            entry: 0,
            block: 0,
            byte: 0,
        });
        selection.focus = Some(Position {
            entry: 1,
            block: 1,
            byte: "Second full body β".len(),
        });
    }
    let copied = copied_text(app.update_inner(Message::CopySelection)).await;
    assert!(copied.contains("First full body λ") && copied.contains("Second full body β"));
    dispatch(&mut app, Message::ToggleEntry("shell".into())).await;
    assert!(chat_text(&app).contains("Waiting for output"));
    h.edit(&id, |s| {
        if let EntryKind::Tool {
            output,
            complete,
            failed,
            ..
        } = &mut s.entries[3].kind
        {
            *output = "Process exited with code 7\ncomplete output β".into();
            *complete = true;
            *failed = true;
        }
    })
    .unwrap();
    dispatch(&mut app, Message::Changed).await;
    assert!(
        chat_text(&app).contains("complete output β"),
        "streaming must preserve expanded details"
    );
    assert_eq!(app.activity.block(0).summary.as_ref().unwrap().failed, 1);
    dispatch(&mut app, Message::ToggleExecution("r1".into())).await;
    assert!(chat_text(&app).contains("await tools.exec_command"));
    dispatch(&mut app, Message::ToggleGroup("r1".into())).await;
    let collapsed = chat_text(&app);
    assert!(!collapsed.contains("First full body λ") && !collapsed.contains("complete output β"));
    assert!(collapsed.contains("I will examine the result."));
    assert!(app.outputs.is_empty());
    assert!(app.chat_selection.borrow().range().is_none());
    dispatch(&mut app, Message::ToggleGroup("r1".into())).await;
    assert!(
        chat_text(&app).contains("complete output β"),
        "manual detail state survives group collapse"
    );
    let saved = h.store.load_session(&id).unwrap();
    assert_eq!(saved.entries.len(), 5);
    assert!(
        matches!(&saved.entries[3].kind, EntryKind::Tool { failed: true, parent: Some(parent), output, .. } if parent == "outer" && output.ends_with("complete output β"))
    );
}

#[tokio::test]
async fn web_transcript_details_survive_reopen() {
    use orbit_core::web_search::{Metadata, Source};
    let mut app = app();
    let root = app.smoke_dir.clone().unwrap();
    stored_session(&mut app, root).await;
    let id = app.selected.clone().unwrap();
    let h = app.harness.as_ref().unwrap().clone();
    let metadata = Metadata {
        searched_at: 123,
        sources: vec![Source {
            title: "Official source".into(),
            url: "https://example.org/docs".into(),
        }],
        details: r#"[{"new_field":"complete UI-only metadata"}]"#.into(),
    };
    h.edit(&id, |s| {
        s.entries.push(Entry {
            id: "web-result".into(),
            text: String::new(),
            kind: EntryKind::Tool {
                name: "web__run".into(),
                input: "{}".into(),
                output: "Agent text".into(),
                complete: true,
                failed: false,
                command_status: None,
                cell_id: None,
                parent: None,
                media: vec![],
                web: Some(metadata.clone()),
            },
        })
    })
    .unwrap();
    dispatch(&mut app, Message::Changed).await;
    dispatch(&mut app, Message::ToggleEntry("web-result".into())).await;
    let loaded = h.store.load_session(&id).unwrap();
    assert!(
        matches!(&loaded.entries[0].kind,EntryKind::Tool{web:Some(actual),output,..} if actual==&metadata && output=="Agent text")
    );
    let link = {
        let mut renderer = header_renderer();
        let mut ui = iced_runtime::UserInterface::build(
            app.entry_view(&app.current().unwrap().entries[0], 0),
            iced::Size::new(600., 800.),
            iced_runtime::user_interface::Cache::new(),
            &mut renderer,
        );
        let mut bounds = UiBounds::default();
        ui.operate(&renderer, &mut bounds);
        let mut messages = click_at(
            &mut ui,
            &mut renderer,
            bounds.label("Official source").center(),
        );
        assert!(matches!(messages.as_slice(), [Message::Link(_)]));
        messages.pop().unwrap()
    };
    dispatch(&mut app, link).await;
    assert!(matches!(&app.modal,Some(Modal::Link(url)) if url=="https://example.org/docs"));
    dispatch(&mut app, Message::Dismiss).await;
    dispatch(&mut app, Message::ToggleEntry("web-result".into())).await;
    assert!(!entry_text(&app, 0).contains("Official source"));
}

#[test]
fn tool_panel_stop_targets_its_owner_once_without_toggling_raw_input() {
    use iced_runtime::{UserInterface, user_interface::Cache};
    let mut renderer = header_renderer();
    for state in [Some(ToolStop::Available), Some(ToolStop::Stopping), None] {
        let mut ui = UserInterface::build(
            tool_actions("call-b", false, state.map(|s| ("session-a", s))),
            iced::Size::new(360., 80.),
            Cache::new(),
            &mut renderer,
        );
        let mut bounds = UiBounds::default();
        ui.operate(&renderer, &mut bounds);
        let raw = bounds.label("Raw input");
        assert!(
            matches!(click_at(&mut ui, &mut renderer, raw.center()).as_slice(), [Message::ToggleToolRaw(entry)] if entry == "call-b")
        );
        match state {
            Some(ToolStop::Available) => {
                let stop = bounds.label("Stop");
                assert!(raw.x + raw.width < stop.x);
                assert!(
                    matches!(click_at(&mut ui, &mut renderer, stop.center()).as_slice(),
                    [Message::StopTool { session, entry }] if session == "session-a" && entry == "call-b")
                );
            }
            Some(ToolStop::Stopping) => assert!(
                click_at(&mut ui, &mut renderer, bounds.label("Stopping…").center()).is_empty()
            ),
            None => assert!(
                !bounds
                    .labels
                    .iter()
                    .any(|(label, _)| label.starts_with("Stop"))
            ),
        }
    }
}

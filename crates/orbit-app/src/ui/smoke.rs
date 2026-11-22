//! Native window capture workflow for smoke checks and marketing screenshots.
use super::*;

impl App {
    pub(super) fn start_capture(&mut self, id: window::Id) -> Task<Message> {
        match self.launch {
            crate::Launch::Normal => Task::none(),
            crate::Launch::Marketing(_) => {
                self.stage_marketing_session();
                delayed_capture()
            }
            crate::Launch::Smoke => {
                // Change the view and request capture in the same update;
                // no timer can hide an ordering bug in the screenshot API.
                self.screenshot_check = Some(Default::default());
                window::screenshot(id).map(Message::CheckScreenshot)
            }
        }
    }

    pub(super) fn check_screenshot(&mut self, screenshot: window::Screenshot) -> Task<Message> {
        use crate::screenshot_check::Progress;
        match self
            .screenshot_check
            .as_mut()
            .expect("capture check is active")
            .receive(screenshot)
        {
            Ok(Progress::CaptureUpdatedPair) => {
                let id = self.window_id.expect("smoke window is open");
                Task::batch((0..2).map(|_| window::screenshot(id).map(Message::CheckScreenshot)))
            }
            Ok(Progress::AwaitSecond) => Task::none(),
            Ok(Progress::Complete) => {
                self.screenshot_check = None;
                println!("SCREENSHOT_CHECK_OK: fresh text and content in queued native captures");
                delayed_capture()
            }
            Err(error) => {
                eprintln!("Screenshot ordering check failed: {error}");
                iced::exit()
            }
        }
    }

    pub(super) fn smoke_open_tool(&self, session_id: &str) -> Task<Message> {
        let index = self
            .current_session
            .as_ref()
            .filter(|session| session.id == session_id)
            .and_then(|session| session.entries.index_of("smoke-tool"))
            .expect("selected smoke fixture must contain the tool");
        assert!(
            self.transcript
                .position(ActivityRow::Activity(index).id())
                .is_some(),
            "selected smoke tool must have a projected activity row"
        );
        Task::done(Message::ToggleEntry("smoke-tool".into())).chain(delayed_capture())
    }

    pub(super) fn capture_frame(&self) -> Task<Message> {
        if self.io_busy {
            delayed_capture()
        } else if let Some(id) = self.window_id {
            window::screenshot(id).map(Message::Screenshot)
        } else {
            Task::none()
        }
    }

    pub(super) fn stage_smoke_questionnaire(
        &mut self,
        request: orbit_core::questionnaire::Request,
    ) {
        let id = "smoke-questionnaire".to_owned();
        self.questionnaires.insert(
            id.clone(),
            questionnaire::Draft::new(orbit_core::questionnaire::Pending {
                id: id.clone(),
                request: Arc::new(request),
            }),
        );
        self.smoke_fixtures.questionnaire = Some(id.clone());
        self.active_questionnaire = Some(id);
    }
    pub(super) fn stage_smoke_transcript(&mut self, session: Session) {
        self.smoke_fixtures.transcript = true;
        self.current_session = Some(session);
        self.rebuild_transcript();
        self.prepare_visible();
    }
    pub(super) fn smoke_frame(&mut self, screenshot: window::Screenshot) -> Task<Message> {
        let pixels = &screenshot.rgba;
        let valid = screenshot.size.width > 0
            && screenshot.size.height > 0
            && pixels.len() == screenshot.size.width as usize * screenshot.size.height as usize * 4
            && pixels.as_chunks::<4>().0.iter().any(|p| p != &pixels[..4]);
        if !valid {
            eprintln!("Window capture returned an empty or uniform frame.");
            return iced::exit();
        }
        if let crate::Launch::Marketing(path) = &self.launch {
            match image::save_buffer_with_format(
                path,
                pixels,
                screenshot.size.width,
                screenshot.size.height,
                image::ColorType::Rgba8,
                image::ImageFormat::Png,
            ) {
                Ok(()) => {
                    self.smoke_passed.store(true, Ordering::Relaxed);
                    println!(
                        "Saved {} × {} window to {}",
                        screenshot.size.width,
                        screenshot.size.height,
                        path.display()
                    );
                }
                Err(error) => eprintln!("Cannot save {}: {error}", path.display()),
            }
            return iced::exit();
        }
        if let Some(path) = std::env::var_os("ORBIT_SMOKE_RGBA") {
            let path = PathBuf::from(path);
            let _ = std::fs::write(
                path.with_extension(format!("{}.rgba", self.smoke_stage)),
                pixels,
            );
            println!(
                "SMOKE_FRAME {} {} {}",
                self.smoke_stage, screenshot.size.width, screenshot.size.height
            );
        }
        self.smoke_stage += 1;
        match self.smoke_stage {
            1 => {
                let h = self.harness.as_ref().expect("smoke storage");
                let path = self.smoke_dir.clone().expect("smoke path");
                self.catalog.projects.push(Project {
                    path: path.clone(),
                    favorite: true,
                    display_name: None,
                    stashed: false,
                });
                let mut s = Session::new(path, &self.catalog.settings);
                s.title = "Phase 1 rendered-window check".into();
                s.usage.input = 60_721;
                s.usage.total_input = 160_721;
                s.usage.cached = 56_470;
                s.usage.total_cached = 126_470;
                s.usage.total_output = 1_237;
                s.entries = vec![
                    Entry::new(EntryKind::User { skills: vec![], files: vec![],  images: vec![], sent_at: None, steering: false }, "Inspect this project and run its tests."),
                    Entry::new(
                        EntryKind::Assistant,
                        "## Project checked\n\nThe **agent loop** is ready.\n\n- Streaming Markdown\n- Durable sessions\n\n```rust\nfn main() { println!(\"Orbit\"); }\n```\n\n[Documentation](https://openai.com)",
                    ),
                ].into();
                let tool = Entry {
                    id: "smoke-tool".into(),
                    text: String::new(),
                    kind: EntryKind::Tool {
                        name: "exec".into(),
                        input: "text(await tools.exec_command({cmd: 'cargo test'}));".into(),
                        output: "Script completed\n14 tests passed".into(),
                        complete: true,
                        failed: false,
                        command_status: None,
                        cell_id: None,
                        parent: None,
                        media: vec![],
                        web: None,
                    },
                };
                s.entries.push(tool);
                let id = s.id.clone();
                h.insert(s);
                self.select(Selection::Session(id.clone()));
                // Deliver Selected (which projects the transcript) before opening the tool.
                return Task::batch(self.tasks.drain(..))
                    .chain(Task::done(Message::SmokeOpenTool(id)));
            }
            2 => {
                // Render a cross-message selection through rich text and lists,
                // including after the settings panel reflows the transcript.
                {
                    let mut selection = self.chat_selection.borrow_mut();
                    selection.anchor = Some(Position {
                        entry: 0,
                        block: 0,
                        byte: 8,
                    });
                    selection.focus = Some(Position {
                        entry: 1,
                        block: 2,
                        byte: 7,
                    });
                }
                self.git.insert(
                    self.current().unwrap().project.clone(),
                    crate::git::Info::Changes {
                        branch: "main".into(),
                        added: 123,
                        removed: 45,
                    },
                );
                self.settings_open = true;
                self.settings_tab = SettingsTab::General;
                let _ = self.update_inner(Message::Theme("Solarized Light"));
                return Task::batch([
                    self.update_inner(Message::ToggleSearch),
                    bottom(),
                    delayed_capture(),
                ]);
            }
            3 => {
                // A populated chat and its sidebar must actually repaint, not just
                // update the selected name on disk.
                for color in [self.preset().colors.panel, self.preset().colors.background] {
                    let expected = color.into_rgba8();
                    assert!(
                        pixels
                            .as_chunks::<4>()
                            .0
                            .iter()
                            .filter(|p| { (0..3).all(|i| p[i].abs_diff(expected[i]) <= 1) })
                            .take(1024)
                            .count()
                            == 1024,
                        "missing switched theme surface"
                    );
                }
                self.chat_selection.borrow_mut().clear();
                let _ = self.update_inner(Message::ToggleSearch);
                self.modal = Some(Modal::AddProject);
                self.modal_input = "/tmp/example-project".into();
            }
            4 => {
                let _ = self.update_inner(Message::Theme(orbit_core::store::DEFAULT_THEME));
                self.settings_tab = SettingsTab::Account;
                self.modal = None;
                self.settings_open = false;
                self.context_menu = self
                    .selected
                    .clone()
                    .map(|id| ContextMenu(MenuTarget::Session(id), iced::Point::new(160., 260.)));
            }
            5 => {
                self.context_menu = None;
                let output = format!(
                    "{}\n{SMOKE_OUTPUT_TAIL}",
                    "long output row with Unicode λ界 and asymmetric data 0123456789\n"
                        .repeat(16000)
                );
                self.harness
                    .as_ref()
                    .expect("smoke harness")
                    .edit(self.selected.as_ref().expect("smoke session"), |s| {
                        if let EntryKind::Tool { output: target, .. } =
                            &mut s.entries.last_mut().expect("smoke tool").kind
                        {
                            *target = output;
                        }
                    })
                    .expect("save smoke fixture");
                self.refresh();
                return Task::batch([bottom(), delayed_capture()]);
            }
            6 => {
                return Task::batch([
                    widget::operation::snap_to(
                        "tool-output-smoke-tool",
                        scrollable::RelativeOffset { x: 0., y: 0.5 },
                    ),
                    delayed_capture(),
                ]);
            }
            7 => {
                return Task::batch([
                    widget::operation::snap_to(
                        "tool-output-smoke-tool",
                        scrollable::RelativeOffset::END,
                    ),
                    delayed_capture(),
                ]);
            }
            8 => {
                // The virtualized tail row only mounts after the inner scrollable reaches it.
                return iced::advanced::widget::operate(FindSmokeTail(false)).then(|found| {
                    if !found {
                        eprintln!("Scrolled smoke tool output did not show its tail row.");
                        return iced::exit();
                    }
                    Task::done(Message::ToggleSettings)
                        .chain(Task::batch([bottom(), delayed_capture()]))
                });
            }
            9 => {
                return Task::batch([
                    widget::operation::snap_to(
                        "tool-output-smoke-tool",
                        scrollable::RelativeOffset::END,
                    ),
                    delayed_capture(),
                ]);
            }
            10 => {
                let h = self.harness.as_ref().expect("smoke harness");
                h.edit(self.selected.as_ref().expect("smoke session"),|s| {
                    for i in 0..20000 {s.entries.push(Entry::new(EntryKind::Assistant,format!("### Historical message {i}\n\nAsymmetric **Markdown** content with λ and 界.")));}
                }).expect("large smoke transcript");
                self.refresh();
                self.smoke_fixtures.sidebar = true;
                for i in 0..10000 {
                    self.sessions.push(SessionSummary {
                        origin: None,
                        child: None,
                        id: format!("smoke-sidebar-{i}"),
                        title: format!("Historical session {i} with wrapping title"),
                        project: self.smoke_dir.clone().expect("smoke path"),
                        updated: 0,
                        interrupted: false,
                        needs_attention: false,
                        unchecked_completion: false,
                        filing: None,
                    });
                }
                self.prepare_sidebar();
                self.auto_scroll = false;
                self.transcript.offset = self.transcript.prefix(10000);
                self.prepare_visible();
                return Task::batch([
                    widget::operation::scroll_to(
                        "chat",
                        scrollable::AbsoluteOffset {
                            x: 0.,
                            y: self.transcript.offset + 20.,
                        },
                    ),
                    widget::operation::scroll_to(
                        "sessions",
                        scrollable::AbsoluteOffset {
                            x: 0.,
                            y: self.sidebar_list.prefix(5000),
                        },
                    ),
                    delayed_capture(),
                ]);
            }
            11 => {
                assert!(self.markdown.len() < 100, "bounded visible Markdown cache");
                assert!(
                    self.transcript.visible().start > 1000,
                    "middle of long transcript is reachable"
                );
                self.auto_scroll = true;
                self.transcript.end();
                self.prepare_visible();
                return Task::batch([bottom(), delayed_capture()]);
            }
            12 => {
                assert_eq!(
                    self.transcript.visible().end,
                    self.transcript.order.len(),
                    "auto-scroll reaches history tail"
                );
                self.settings_open = false;
                self.view_epoch = self.view_epoch.wrapping_add(1);
                return Task::batch([bottom(), delayed_capture()]);
            }
            13 => {
                self.view_epoch = self.view_epoch.wrapping_add(1);
                self.transcript = crate::virtual_list::List::with_overscan(CHAT_OVERSCAN);
                self.activity = activity::Projection::default();
                self.append_transcript();
                self.transcript.end();
                self.prepare_visible();
                return Task::batch([bottom(), delayed_capture()]);
            }
            14 => {
                // Return the virtual sidebar to the top before moving surviving
                // visible rows with a search, then restore the full large list.
                return Task::batch([
                    self.update_inner(Message::Search(String::new())),
                    delayed_capture(),
                ]);
            }
            15 => {
                return Task::batch([
                    self.update_inner(Message::Search("Historical session 1".into())),
                    delayed_capture(),
                ]);
            }
            16 => {
                return Task::batch([
                    self.update_inner(Message::Search(String::new())),
                    delayed_capture(),
                ]);
            }
            17 => {
                self.settings_open = false;
                let _ = self.update_inner(Message::ToggleFooterMenu);
            }
            18 => {} // Keep the picker open across another redraw.
            19 => {
                self.footer_menu_open = false;
                self.settings_open = true;
                self.settings_tab = SettingsTab::General;
            }
            20 => self.settings_tab = SettingsTab::Models,
            21 => {
                self.harness
                    .as_ref()
                    .expect("smoke harness")
                    .edit(self.selected.as_ref().expect("smoke session"), |s| {
                        s.fast = true
                    })
                    .expect("smoke fast");
                self.refresh();
                let _ = self.update_inner(Message::ToggleFooterMenu);
            }
            22 => {
                self.footer_menu_open = false;
                self.settings_open = false;
                self.agents_open = true;
            }
            23 => {
                self.agents_open = false;
                let parent = self.selected.clone().expect("smoke parent");
                let mut child = Session::new(
                    self.smoke_dir.clone().expect("smoke path"),
                    &self.catalog.settings,
                );
                child.child = Some(orbit_core::agent::subagents::Child {
                    parent_id: parent,
                    profile: orbit_core::agent::subagents::Profile::Explore,
                });
                child.title = "Trace completion notifications and cancellation".into();
                child.model = orbit_core::models::LUNA.id.into();
                child.effort = "high".into();
                child.entries.push(Entry::new(
                    EntryKind::User {
                        skills: vec![],
                        files: vec![],
                        images: vec![],
                        sent_at: None,
                        steering: false,
                    },
                    "Inspect the completion notification path. Do not modify files.",
                ));
                child.entries.push(Entry::new(EntryKind::Assistant,"## Exploration result\n\nThe **parent wake-up** path preserves cancellation and uses durable completion notices.\n\n- Separate conversation context\n- Repeatable final results\n- No nested delegation"));
                let id = child.id.clone();
                self.harness.as_ref().expect("smoke harness").insert(child);
                self.select(Selection::Session(id));
            }
            24 => {
                self.agents_open = true;
                self.window_width = 860.;
                self.window_height = 560.;
                self.view_epoch = self.view_epoch.wrapping_add(1);
                if let Some(window) = self.window_id {
                    return Task::batch([
                        window::resize(window, iced::Size::new(860., 560.)),
                        delayed_capture(),
                    ]);
                }
            }
            25 => {
                let parent = self
                    .current()
                    .and_then(|s| s.child.as_ref())
                    .expect("smoke child")
                    .parent_id
                    .clone();
                self.agents_open = false;
                self.select(Selection::Session(parent));
                if let Some(window) = self.window_id {
                    return Task::batch([
                        window::resize(window, iced::Size::new(1200., 800.)),
                        delayed_capture(),
                    ]);
                }
            }
            26 => {
                let request = orbit_core::questionnaire::Request::parse(r#"{"questions":[{"id":"project","question":"Here’s a demo of a single-choice question. What kind of project would you like to build? You can also enter your own answer.","kind":"select","options":["A website","A mobile app","A command-line tool"]},{"id":"details","question":"Describe the constraints and any additional requirements.","kind":"text"}]}"#).expect("smoke questionnaire");
                self.stage_smoke_questionnaire(request);
            }
            27..=30 => {
                let draft = self
                    .questionnaires
                    .get_mut("smoke-questionnaire")
                    .expect("smoke questionnaire draft");
                let action = match self.smoke_stage {
                    27 => questionnaire::Action::Edit(1),
                    28 => questionnaire::Action::Edit(0),
                    29 => questionnaire::Action::Toggle,
                    30 => {
                        draft.update(questionnaire::Message {
                            question: 0,
                            action: questionnaire::Action::Toggle,
                        });
                        draft.update(questionnaire::Message {
                            question: 0,
                            action: questionnaire::Action::Replace(false),
                        });
                        draft.update(questionnaire::Message {
                            question: 0,
                            action: questionnaire::Action::Next,
                        });
                        questionnaire::Action::Editor(text_editor::Action::Edit(
                            text_editor::Edit::Paste(Arc::new(
                                "Long answer with Unicode λ界 and preserved newlines.\n"
                                    .repeat(700),
                            )),
                        ))
                    }
                    _ => unreachable!(),
                };
                let task = draft
                    .update(questionnaire::Message {
                        question: draft.current,
                        action,
                    })
                    .task
                    .map(|message| Message::Questionnaire("smoke-questionnaire".into(), message));
                return Task::batch([task, delayed_capture()]);
            }
            31 => {
                let request = serde_json::json!({"questions":[{"id":"many","question":"A long question that wraps rather than overflowing the available space. Choose an option, scroll through the complete list, or provide a custom answer.","kind":"select","options":(1..=20).map(|i| format!("Option {i}: preserve this complete descriptive label even in a narrow window with settings open.")).collect::<Vec<_>>()}]});
                self.stage_smoke_questionnaire(
                    orbit_core::questionnaire::Request::parse(&request.to_string()).unwrap(),
                );
                self.settings_open = true;
                self.settings_tab = SettingsTab::General;
                if let Some(window) = self.window_id {
                    return Task::batch([
                        window::resize(window, iced::Size::new(860., 560.)),
                        delayed_capture(),
                    ]);
                }
            }
            32 => {
                self.smoke_fixtures.questionnaire = None;
                self.sync_questionnaires();
                self.settings_open = true;
                self.settings_tab = SettingsTab::Images;
            }
            33 => {
                self.settings_open = false;
                let media = orbit_core::media::MediaStore::new(
                    self.smoke_dir
                        .as_ref()
                        .expect("smoke directory")
                        .join("image-smoke"),
                )
                .image(include_bytes!("../fixtures/media.png"))
                .expect("valid PNG fixture");
                let mut session = Session::new(
                    self.smoke_dir.as_ref().unwrap().clone(),
                    &self.catalog.settings,
                );
                let entry = Entry {
                    id: "smoke-image".into(),
                    text: String::new(),
                    kind: EntryKind::Tool {
                        name: "view_image".into(),
                        input: "{path:example.png}".into(),
                        output: media.description(),
                        complete: true,
                        failed: false,
                        command_status: None,
                        cell_id: None,
                        parent: None,
                        media: vec![media],
                        web: None,
                    },
                };
                session.entries.push(entry);
                self.expanded.insert("smoke-image".into());
                self.groups.insert("smoke-image".into());
                self.stage_smoke_transcript(session);
                return Task::batch(std::iter::once(delayed_capture()).chain(self.tasks.drain(..)));
            }
            34 => {
                let media = orbit_core::media::MediaStore::new(
                    self.smoke_dir
                        .as_ref()
                        .expect("smoke directory")
                        .join("image-smoke"),
                )
                .image(include_bytes!("../fixtures/media.gif"))
                .expect("valid GIF fixture");
                let task = self.image_view.open(&media).map(Message::Image);
                self.modal = Some(Modal::Image(media));
                return Task::batch([task, delayed_capture()]);
            }
            35 => {
                self.image_view.update(crate::image_view::Message::Toggle);
            }
            36 => {
                let media = orbit_core::media::MediaStore::new(
                    self.smoke_dir
                        .as_ref()
                        .expect("smoke directory")
                        .join("image-smoke"),
                )
                .image(include_bytes!("../fixtures/media.png"))
                .expect("valid PNG fixture");
                let task = self.image_view.open(&media).map(Message::Image);
                self.modal = Some(Modal::Image(media));
                return Task::batch([task, delayed_capture()]);
            }
            37 => {
                self.modal = None;
                self.image_view.close();
                let root = self.smoke_dir.as_ref().expect("smoke directory");
                std::fs::write(
                    root.join("markdown.gif"),
                    include_bytes!("../fixtures/media.gif"),
                )
                .expect("GIF fixture");
                let mut session = Session::new(root.clone(), &self.catalog.settings);
                session.entries.push(Entry {
                    id: "smoke-markdown-image".into(),
                    text: "![Markdown GIF preview](markdown.gif \"Click to inspect\")\n\n![Missing image](missing.png)".into(),
                    kind: EntryKind::Assistant,
                });
                self.markdown.clear();
                self.markdown_heights.clear();
                self.stage_smoke_transcript(session);
                return Task::batch(std::iter::once(delayed_capture()).chain(self.tasks.drain(..)));
            }
            38 => {
                let image = self
                    .markdown_images
                    .loaded("markdown.gif")
                    .expect("Markdown GIF preview loaded");
                let task = self
                    .image_view
                    .open_bytes(&image.url, image.bytes.clone())
                    .map(Message::Image);
                self.modal = Some(Modal::MarkdownImage(image));
                return Task::batch([task, delayed_capture()]);
            }
            39 => {
                self.modal = None;
                self.image_view.close();
                self.settings_open = true;
                self.settings_tab = SettingsTab::General;
                let mut session = Session::new(
                    self.smoke_dir.as_ref().unwrap().clone(),
                    &self.catalog.settings,
                );
                session.entries.push(Entry {id:"smoke-web".into(),text:String::new(),kind:EntryKind::Tool {
                    name:"web__run".into(),input:r#"{"search_query":[{"q":"Rust documentation","domains":["rust-lang.org"]}]}"#.into(),
                    command_status: None,
                    output:"Rust documentation (https://doc.rust-lang.org/book/)\nThe Rust Programming Language".into(),complete:true,failed:false, cell_id: None,parent:None,media:vec![],
                    web:Some(orbit_core::web_search::Metadata{mode:orbit_core::web_search::Mode::Live,searched_at:orbit_core::store::now(),sources:vec![orbit_core::web_search::Source{title:"The Rust Programming Language — a long source title that should fit in a constrained chat column".into(),url:"https://doc.rust-lang.org/book/".into()}],details:r#"[{"type":"text_result","url":"https://doc.rust-lang.org/book/"}]"#.into()})
                }});
                self.expanded.insert("smoke-web".into());
                self.groups.insert("smoke-web".into());
                self.stage_smoke_transcript(session);
            }
            40 => {
                self.settings_open = false;
                self.attachments = [
                    include_bytes!("../fixtures/media.png").as_slice(),
                    include_bytes!("../fixtures/media.gif").as_slice(),
                ]
                .into_iter()
                .map(|bytes| {
                    orbit_core::media::MediaStore::new(
                        self.smoke_dir.as_ref().unwrap().join("attachment-smoke"),
                    )
                    .image(bytes)
                    .unwrap()
                })
                .collect();
                self.editor = text_editor::Content::with_text(
                    &(0..16)
                        .map(|line| {
                            format!("Draft line {line}: images are attached above this field.")
                        })
                        .collect::<Vec<_>>()
                        .join("\n"),
                );
                self.prepare_visible();
                return Task::batch(std::iter::once(delayed_capture()).chain(self.tasks.drain(..)));
            }
            41 => {
                self.settings_open = false;
                self.agents_open = false;
                self.attachments.clear();
                self.editor = text_editor::Content::new();
                self.groups.clear();
                self.expanded.clear();
                self.execution.clear();
                self.auto_scroll = false;
                self.transcript = crate::virtual_list::List::with_overscan(CHAT_OVERSCAN);
                self.activity = activity::Projection::default();
                let mut session = Session::new(
                    self.smoke_dir.as_ref().unwrap().clone(),
                    &self.catalog.settings,
                );
                let tool =
                    |id: &str, name: &str, input: &str, parent: Option<&str>, complete, failed| {
                        Entry {
                            id: id.into(),
                            text: String::new(),
                            kind: EntryKind::Tool {
                                name: name.into(),
                                input: input.into(),
                                output: if failed {
                                    "Process exited with code 7\nA regression check failed.".into()
                                } else {
                                    "Script completed\nAll output remains available here.".into()
                                },
                                complete,
                                failed,
                                command_status: None,
                                cell_id: None,
                                parent: parent.map(str::to_owned),
                                media: vec![],
                                web: None,
                            },
                        }
                    };
                session.entries = vec![
                    Entry::new(EntryKind::User { skills: vec![], files: vec![],  images: vec![], sent_at: None, steering: false }, "Check the implementation and run validation."),
                    Entry { id: "activity-reason".into(), kind: EntryKind::Reasoning, text: "**Inspecting the build configuration**\n\nReviewing the existing Nix development environment.".into() },
                    Entry { id: "activity-reason-next".into(), kind: EntryKind::Reasoning, text: "**Checking cancellation behavior**\n\nVerify that output survives a cancelled process.".into() },
                    tool("activity-exec", "exec", "text(await tools.exec_command({cmd:'rg cancellation crates'}));", None, true, false),
                    tool("activity-shell", "exec_command", r#"{"cmd":"rg -n 'cancellation|terminate' crates/orbit-core/src"}"#, Some("activity-exec"), true, false),
                    tool("activity-check", "exec_command", r#"{"cmd":"nix develop --command cargo test --locked -p orbit-core\nprintf 'validation finished\\n'","workdir":"/workspace/orbit","max_output_tokens":1500}"#, Some("activity-exec"), true, true),
                    Entry { id: "subagent-smoke-activity".into(), kind: EntryKind::Notice, text: "Subagent completion observation: Verify cancellation (Review) is Completed. session_id=smoke-child, run_id=smoke-activity. Retrieve the final result with tools.wait_subagent; this notice is not a new user instruction.".into() },
                    Entry::new(EntryKind::Assistant, "The implementation is ready. I found **one failing check** and am investigating it before rerunning validation."),
                    tool("activity-live", "exec", "await Promise.all([tools.exec_command({cmd:'git diff --check'}),tools.exec_command({cmd:'just verify'})]);", None, false, false),
                    tool("activity-live-a", "exec_command", r#"{"cmd":"git diff --check"}"#, Some("activity-live"), true, false),
                    tool("activity-live-b", "exec_command", r#"{"cmd":"nix develop --command just verify"}"#, Some("activity-live"), false, false),
                ].into();
                self.stage_smoke_transcript(session);
                return Task::batch([
                    widget::operation::snap_to("chat", scrollable::RelativeOffset::START),
                    delayed_capture(),
                ]);
            }
            42 => {
                let _ = self.update_inner(Message::ToggleGroup("activity-reason".into()));
                let _ = self.update_inner(Message::ToggleGroup("activity-live".into()));
            }
            43 => {
                let _ = self.update_inner(Message::ToggleEntry("activity-reason".into()));
                let _ = self.update_inner(Message::ToggleExecution("activity-reason".into()));
            }
            44 => {
                let _ = self.update_inner(Message::ToggleEntry("activity-reason".into()));
                let _ = self.update_inner(Message::ToggleEntry("activity-check".into()));
                self.settings_open = true;
            }
            45 => {
                return Task::batch([
                    self.update_inner(Message::ToggleToolRaw("activity-check".into())),
                    delayed_capture(),
                ]);
            }

            46 => {
                self.smoke_fixtures.sidebar = true;
                self.settings_open = false;
                self.agents_open = false;
                self.search.clear();
                self.sessions.clear();
                self.sidebar_rows.clear();
                for (title, status) in [
                    (
                        "Questionnaire — frozen yellow wave",
                        SessionStatus::Question,
                    ),
                    ("Error — frozen red wave", SessionStatus::Error),
                    ("Working — animated blue wave", SessionStatus::Working),
                    ("Completed — not yet checked", SessionStatus::Unchecked),
                    ("Checked — ordinary session age", SessionStatus::Idle),
                ] {
                    let mut session =
                        Session::new(self.smoke_dir.clone().unwrap(), &self.catalog.settings);
                    session.title = title.into();
                    let index = self.sessions.len();
                    self.sessions.push(SessionSummary::from(&session));
                    if index != 1 && index != 4 {
                        let heading = ["NEEDS ATTENTION", "WORKING", "SESSIONS"][status.section()];
                        self.sidebar_rows
                            .push(SidebarRow::Heading(heading, if index == 2 { 1 } else { 2 }));
                    }
                    self.sidebar_rows.push(SidebarRow::Session(index, status));
                }
                self.sidebar_epoch = self.sidebar_epoch.wrapping_add(1);
                // These are new sessions, not movements of previously mounted rows.
                let key_base = self.sidebar_keys.len() + 3;
                self.sidebar_list
                    .reset(self.sidebar_rows.iter().enumerate().map(|(key, row)| {
                        (
                            key_base + key,
                            if matches!(row, SidebarRow::Heading(..)) {
                                36.
                            } else {
                                48.
                            },
                        )
                    }));
                self.sidebar_list.offset = 0.;
                return Task::batch([
                    widget::operation::scroll_to(
                        "sessions",
                        scrollable::AbsoluteOffset { x: 0., y: 0. },
                    ),
                    delayed_capture(),
                ]);
            }

            47 => {
                self.sessions.clear();
                for i in 0..(FILED_VISIBLE_ROWS * 3) {
                    let mut session =
                        Session::new(self.smoke_dir.clone().unwrap(), &self.catalog.settings);
                    session.title = format!("Settled session {i} — a long title fits on one row");
                    session.filing = Some(Filing {
                        kind: FilingKind::Settled,
                        at: orbit_core::store::now() - i as u64 * 86400,
                    });
                    self.sessions.push(SessionSummary::from(&session));
                }
                let mut idle =
                    Session::new(self.smoke_dir.clone().unwrap(), &self.catalog.settings);
                idle.title = "Idle session — hover to settle".into();
                self.sessions.push(SessionSummary::from(&idle));
                self.filed[FilingKind::Settled as usize].open = true;
                self.prepare_sidebar();
                return Task::batch(std::iter::once(delayed_capture()).chain(self.tasks.drain(..)));
            }
            48 => {
                self.filed[FilingKind::Settled as usize].list.offset =
                    self.filed[FilingKind::Settled as usize].list.total()
                        - self.filed_list_height(FilingKind::Settled);
                return Task::batch([
                    widget::operation::scroll_to(
                        "settled-sessions",
                        scrollable::AbsoluteOffset {
                            x: 0.,
                            y: self.filed[FilingKind::Settled as usize].list.offset,
                        },
                    ),
                    delayed_capture(),
                ]);
            }

            49 => {
                self.current_session = None;
                self.selected = None;
                self.draft_session = None;
                self.modal = None;
                self.context_menu = None;
                self.account = Some("Smoke fixture".into());
                self.catalog.projects = (0..12)
                    .map(|i| Project {
                        path: PathBuf::from(format!("/tmp/smoke-project-{i}")),
                        display_name: Some(format!("Project {i}")),
                        favorite: i % 3 == 0,
                        stashed: i >= 9,
                    })
                    .collect();
            }
            50 => {
                self.stashed_open = true;
                self.context_menu = Some(ContextMenu(
                    MenuTarget::Project(PathBuf::from("/tmp/smoke-project-9")),
                    iced::Point::new(660., 400.),
                ));
            }
            51 => {
                self.context_menu = None;
                self.modal = Some(Modal::DeleteProject(PathBuf::from("/tmp/smoke-project-9")));
            }

            52 => {
                self.modal = None;
                self.context_menu = None;
                self.stashed_open = false;
                self.filed[FilingKind::Settled as usize].open = false;
                self.settings_open = false;
                self.agents_open = false;
                self.sessions.clear();
                let root = self.smoke_dir.as_ref().unwrap().clone();
                for name in ["smoke-file one.rs", "smoke-file two.rs", "smoke-file λ.md"] {
                    std::fs::write(root.join(name), "file tag smoke fixture").unwrap();
                }
                let mut session = Session::new(root, &self.catalog.settings);
                session.draft = "Compare @smoke-file".into();
                self.selected = Some(session.id.clone());
                self.sessions.push(SessionSummary::from(&session));
                self.prepare_sidebar();
                self.stage_smoke_transcript(session);
                self.restore_draft();
                self.editor
                    .perform(text_editor::Action::Move(text_editor::Motion::DocumentEnd));
                self.file_completion.edited(&self.editor);
            }

            53 => {
                let mut session =
                    Session::new(self.smoke_dir.clone().unwrap(), &self.catalog.settings);
                session.entries.push(Entry::new(
                    EntryKind::Assistant,
                    include_str!("../fixtures/search-chance.md"),
                ));
                self.selected = Some(session.id.clone());
                self.stage_smoke_transcript(session);
                self.restore_draft();
                return Task::batch(std::iter::once(delayed_capture()).chain(self.tasks.drain(..)));
            }
            54 if self
                .equations
                .get(include_str!("../fixtures/search-chance.md").trim_end())
                .is_none() =>
            {
                self.smoke_stage -= 1;
                return delayed_capture();
            }

            _ => {
                self.smoke_passed.store(true, Ordering::Relaxed);
                println!(
                    "SMOKE_OK: native Wayland rendered formatted tool panels/raw input, compact activity groups/thinking/execution details/failures, composer attachments/overflow scrollbar/file-tag completion, web source links/details, Markdown display equations/images/error fallbacks/GIF modal, image thumbnails/modal/GIF play-pause/Images settings, compact project picker/stash/context menu/delete confirmation, sidebar status waves/unchecked completion/favorites/search/reflow animations/compact settled sessions (top/end), chat, settings, modal, session/footer menus, Agents panel, read-only child navigation, questionnaires (select/edit/confirmation/collapse/large text/narrow scrolling), virtualized large output and 20,000-entry/10,000-session lists (middle/end/reflow/identity)"
                );
                return iced::exit();
            }
        }
        delayed_capture()
    }
}

const SMOKE_OUTPUT_TAIL: &str = "TAIL";
struct FindSmokeTail(bool);
impl iced::advanced::widget::Operation<bool> for FindSmokeTail {
    fn traverse(
        &mut self,
        visit: &mut dyn FnMut(&mut dyn iced::advanced::widget::Operation<bool>),
    ) {
        visit(self);
    }
    fn text(&mut self, _: Option<&widget::Id>, _: iced::Rectangle, text: &str) {
        self.0 |= text == SMOKE_OUTPUT_TAIL;
    }
    fn finish(&self) -> iced::advanced::widget::operation::Outcome<bool> {
        iced::advanced::widget::operation::Outcome::Some(self.0)
    }
}
fn delayed_capture() -> Task<Message> {
    Task::perform(
        async { tokio::time::sleep(Duration::from_millis(250)).await },
        |_| Message::Capture,
    )
}

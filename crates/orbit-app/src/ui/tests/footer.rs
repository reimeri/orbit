use super::*;
use orbit_core::models::{ASTRA, LUNA};

#[test]
fn token_counts_use_three_digits_and_promote_rounded_units() {
    for (tokens, expected) in [
        (0, "0"),
        (9, "9"),
        (121, "121"),
        (999, "999"),
        (1000, "1.00k"),
        (1201, "1.20k"),
        (22_600, "22.6k"),
        (754_600, "755k"),
        (4_299_000, "4.30M"),
        (100_000, "100k"),
        (9_994, "9.99k"),
        (9_995, "10.0k"),
        (9_999, "10.0k"),
        (99_949, "99.9k"),
        (99_950, "100k"),
        (999_499, "999k"),
        (999_500, "1.00M"),
        (999_500_000, "1.00B"),
        (1_234_000_000_000, "1.23T"),
        (1_235_000_000_000_000, "1.24Q"),
        (999_500_000_000_000_000, "1.00Qi"),
        (u64::MAX, "18.4Qi"),
    ] {
        assert_eq!(compact_tokens(tokens), expected, "{tokens}");
    }
}

#[tokio::test]
async fn git_refreshes_on_selection_and_idle_ticks() {
    let mut app = app();
    let repo = tempfile::tempdir().unwrap();
    let output = std::process::Command::new("git")
        .args(["init", "--template=", "--initial-branch=main"])
        .current_dir(repo.path())
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let file = repo.path().join("new.txt");
    std::fs::write(&file, "one\ntwo\n").unwrap();

    dispatch(&mut app, Message::Create(repo.path().to_owned())).await;
    assert!(!app.running());
    assert!(matches!(
        app.git_info(),
        Some(crate::git::Info::Changes {
            added: 2,
            removed: 0,
            ..
        })
    ));

    std::fs::write(&file, "one\ntwo\nthree\n").unwrap();
    dispatch(&mut app, Message::GitTick).await;
    assert!(matches!(
        app.git_info(),
        Some(crate::git::Info::Changes {
            added: 3,
            removed: 0,
            ..
        })
    ));
}

#[tokio::test]
async fn git_cache_follows_projects_and_retains_background_results() {
    let mut app = app();
    let a = PathBuf::from("/project-a");
    let b = PathBuf::from("/project-b");
    let changes = |added| {
        Some(crate::git::Info::Changes {
            branch: "main".into(),
            added,
            removed: 0,
        })
    };

    // Drop capture tasks and deliver their results explicitly to model
    // switching projects while Git is still running, without timing races.
    let _ = app.update(Message::Selected(Ok((Selection::Draft(a.clone()), None))));
    assert!(app.git_pending.contains(&a));
    assert!(app.git_info().is_none());
    let _ = app.update(Message::Git(a.clone(), changes(2)));
    let _ = app.update(Message::GitTick);
    assert!(app.git_pending.contains(&a));
    if let Some(mut actions) = iced_runtime::task::into_stream(app.update(Message::GitTick)) {
        use iced::futures::StreamExt;
        while let Some(action) = actions.next().await {
            assert!(
                !matches!(action, iced_runtime::Action::Output(Message::Git(..))),
                "a pending capture must not be duplicated"
            );
        }
    }
    assert_eq!(app.git_info().unwrap().to_string(), "main  +2 −0");

    let _ = app.update(Message::Selected(Ok((Selection::Draft(b.clone()), None))));
    assert!(
        app.git_pending.contains(&b),
        "project A must not block B's refresh"
    );
    assert!(
        app.git_info().is_none(),
        "never display another project's counts"
    );
    let _ = app.update(Message::Git(b.clone(), changes(7)));
    let _ = app.update(Message::Git(a.clone(), changes(3)));
    assert_eq!(app.git_info().unwrap().to_string(), "main  +7 −0");

    let _ = app.update(Message::Selected(Ok((Selection::Draft(a.clone()), None))));
    assert_eq!(
        app.git_info().unwrap().to_string(),
        "main  +3 −0",
        "returning to a project immediately uses its latest cached result"
    );
    assert!(
        app.git_pending.contains(&a),
        "cached projects also refresh on selection"
    );
    let _ = app.update(Message::Selected(Ok((Selection::Draft(a.clone()), None))));
    assert_eq!(app.git_info().unwrap().to_string(), "main  +3 −0");
    let _ = app.update(Message::Git(a, None));
    assert!(
        app.git_info().is_none(),
        "a non-repository result clears the cache"
    );
    let _ = app.update(Message::Selected(Ok((Selection::Draft(b), None))));
    assert_eq!(app.git_info().unwrap().to_string(), "main  +7 −0");
}

#[tokio::test]
async fn footer_omits_absent_git_and_zero_diff_counts() {
    let mut app = app();
    dispatch(&mut app, Message::Create(PathBuf::from("/footer-project"))).await;
    let mut renderer = header_renderer();
    for (status, expected_branch, expected_counts) in [
        (
            Some(crate::git::Info::Changes {
                branch: "main".into(),
                added: 0,
                removed: 0,
            }),
            true,
            None,
        ),
        (
            Some(crate::git::Info::Changes {
                branch: "main".into(),
                added: 0,
                removed: 3,
            }),
            true,
            Some(("+0", "−3")),
        ),
        (None, false, None),
    ] {
        let path = app.current().unwrap().project.clone();
        let _ = app.update(Message::Git(path, status));
        let mut ui = iced_runtime::UserInterface::build(
            app.footer_content(1400.),
            iced::Size::new(1400., 100.),
            iced_runtime::user_interface::Cache::new(),
            &mut renderer,
        );
        let mut bounds = UiBounds::default();
        ui.operate(&renderer, &mut bounds);
        assert_eq!(
            bounds.labels.iter().any(|(label, _)| label == "main"),
            expected_branch
        );
        let text = bounds
            .labels
            .iter()
            .map(|(label, _)| label.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        let counts: Vec<_> = text
            .split_whitespace()
            .filter(|token| token.starts_with('+') || token.starts_with('−'))
            .collect();
        assert_eq!(
            counts,
            expected_counts.map_or_else(Vec::new, |(added, removed)| vec![added, removed])
        );
    }
}

#[tokio::test]
async fn footer_hides_whole_items_in_priority_order_without_wrapping() {
    let mut app = app();
    let path = PathBuf::from("/footer-project");
    dispatch(&mut app, Message::Create(path.clone())).await;
    app.git.insert(
        path,
        crate::git::Info::Changes {
            branch: "main".into(),
            added: 0,
            removed: 0,
        },
    );
    let mut renderer = header_renderer();
    let mut counts = Vec::new();
    for width in (20..700).step_by(7) {
        let width = width as f32;
        let mut ui = iced_runtime::UserInterface::build(
            app.footer_content(width),
            iced::Size::new(width, 100.),
            Default::default(),
            &mut renderer,
        );
        let mut bounds = UiBounds::default();
        ui.operate(&renderer, &mut bounds);
        let has = |suffix: &str| bounds.labels.iter().any(|(s, _)| s.ends_with(suffix));
        let visible = [
            has(" tokens"),
            has("main"),
            has("footer-project"),
            has("% cache"),
            has("TPS: 0"),
        ];
        assert_eq!(
            has("FP"),
            visible[2],
            "project badge/name must hide together"
        );
        let count = visible.iter().take_while(|&&v| v).count();
        assert!(visible[count..].iter().all(|v| !v));
        if counts.last() != Some(&count) {
            counts.push(count);
        }
        assert!(!has("●"));
        if count == 0 {
            assert!(bounds.labels.is_empty());
            continue;
        }
        let baseline = bounds.labels[0].1.center_y();
        assert!(
            bounds
                .labels
                .iter()
                .all(|(_, b)| (b.center_y() - baseline).abs() < 1. && b.x + b.width <= width + 1.)
        );
    }
    assert_eq!(counts, (0..=5).collect::<Vec<_>>());
}

#[tokio::test]
async fn model_selector_centers_chevron_in_button() {
    use iced::advanced::{Layout, layout, widget::Tree};

    let mut app = app();
    dispatch(&mut app, Message::Create(PathBuf::from("/toolbar-project"))).await;
    let renderer = header_renderer();
    for open in [false, true] {
        app.footer_menu_open = open;
        let mut button = app.model_selector(app.current().unwrap(), false);
        let mut tree = Tree::new(button.as_widget());
        let node = button.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &layout::Limits::new(Size::ZERO, Size::INFINITE),
        );
        let button = Layout::new(&node);
        let row = button.children().next().unwrap();
        let chevron = row.children().last().unwrap();
        assert_eq!(chevron.bounds().center_y(), button.bounds().center_y());
    }
}

#[tokio::test]
async fn footer_stays_at_chat_bottom_and_controls_fit_with_both_sidebars() {
    let mut app = app();
    dispatch(&mut app, Message::Create(PathBuf::from("/footer-project"))).await;
    app.git.insert(
        app.current().unwrap().project.clone(),
        crate::git::Info::Changes {
            branch: "long-feature-branch-name".into(),
            added: 12345,
            removed: 678,
        },
    );
    app.settings_open = true;
    let mut renderer = header_renderer();
    let mut wide_popup_height = None;
    for (width, height, left, right, fast) in [
        (1600., 800., 260., 320., false),
        (1600., 1400., 260., 320., true),
        (860., 560., 344., 344., true),
    ] {
        dispatch(&mut app, Message::SessionFast(fast)).await;
        app.window_width = width;
        app.window_height = height;
        app.catalog.settings.sidebar_width = left;
        app.catalog.settings.settings_width = right;
        let mut ui = iced_runtime::UserInterface::build(
            app.view(),
            iced::Size::new(width, height),
            iced_runtime::user_interface::Cache::new(),
            &mut renderer,
        );
        let mut bounds = UiBounds::default();
        ui.operate(&renderer, &mut bounds);
        let session = app.current().unwrap();
        let label = bounds.label(&format!(
            "{} {}",
            model(&session.model).unwrap().name,
            effort_name(&session.effort)
        ));
        let context = bounds
            .labels
            .iter()
            .find(|(text, b)| text.ends_with(" tokens") && b.y > label.y && b.x > left)
            .expect("context usage remains visible at these widths")
            .1;
        let footer = bounds
            .containers
            .iter()
            .filter(|b| b.y + b.height == height && b.contains(context.center()))
            .min_by(|a, b| {
                a.height
                    .total_cmp(&b.height)
                    .then(a.width.total_cmp(&b.width))
            })
            .expect("bottom-anchored status row");
        assert!(footer.height < height / 4.);
        assert!(footer.x > left && footer.x + footer.width < width - right);
        let settled = bounds.label("Settled");
        let deferred = bounds.label("Deferred");
        let label_gap = settled.y - (deferred.y + deferred.height);
        let bottom_gap = height - (settled.y + settled.height);
        assert!(
            label_gap > 0. && label_gap <= bottom_gap,
            "label gap {label_gap} vs bottom gap {bottom_gap}"
        );
        assert!(
            (settled.center_y() - context.center_y()).abs() < 1.,
            "settled={settled:?} context={context:?} footer={footer:?}"
        );
        assert!(label.width > 0. && label.height > 0.);
        let composer = bounds
            .containers
            .iter()
            .find(|b| {
                b.contains(label.center())
                    && b.x == footer.x
                    && b.width == footer.width
                    && b.y + b.height == footer.y
            })
            .expect("selector inside composer aligned with the footer");
        assert!(composer.y < label.y && label.y + label.height <= footer.y);
        assert!(
            bounds
                .containers
                .iter()
                .any(|b| b.x == 0. && b.width == left && b.height == height)
        );
        assert!(
            bounds
                .containers
                .iter()
                .any(|b| b.x > footer.x + footer.width
                    && b.x + b.width == width
                    && b.y == 0.
                    && b.height == height)
        );
        let has_tps = bounds
            .labels
            .iter()
            .any(|(label, _)| label.starts_with("TPS:"));
        assert_eq!(
            has_tps,
            width == 1600.,
            "metrics collapse using the available center width"
        );
        if width == 1600. {
            let cache = bounds.label("0% cache");
            assert!((cache.x + cache.width - (composer.x + composer.width)).abs() < 1.);
            let added = bounds.label("+12345");
            let removed = bounds.label("−678");
            assert!(added.y >= footer.y && removed.y >= footer.y);
            assert!(added.x + added.width <= removed.x);
            assert!(removed.x + removed.width <= footer.x + footer.width);
        } else {
            assert!(!bounds.labels.iter().any(|(label, _)| label == "+12345"));
        }
        for fraction in [0.25, 0.75] {
            let point = iced::Point::new(label.x + label.width * fraction, label.center_y());
            let messages = click_at(&mut ui, &mut renderer, point);
            assert!(
                matches!(messages.as_slice(), [Message::ToggleFooterMenu]),
                "{messages:?}"
            );
        }
        drop(ui);
        let session_id = app.selected.take();
        let current_session = app.current_session.take();
        let draft_session = app.draft_session.take();
        for (account, status) in [
            (None, "Not signed in"),
            (Some("fixture account".into()), "ChatGPT connected"),
        ] {
            app.account = account;
            let mut ui = iced_runtime::UserInterface::build(
                app.view(),
                iced::Size::new(width, height),
                iced_runtime::user_interface::Cache::new(),
                &mut renderer,
            );
            let mut picker_bounds = UiBounds::default();
            ui.operate(&renderer, &mut picker_bounds);
            for text in ["Orbit", status] {
                let idle_label = picker_bounds.label(text);
                assert!(
                    (idle_label.center_y() - context.center_y()).abs() < 1.,
                    "no-session footer must align with session footer: {idle_label:?}, {label:?}"
                );
            }
        }
        app.selected = session_id;
        app.current_session = current_session;
        app.draft_session = draft_session;
        app.account = None;
        dispatch(&mut app, Message::ToggleFooterMenu).await;
        let mut ui = iced_runtime::UserInterface::build(
            app.view(),
            iced::Size::new(width, height),
            iced_runtime::user_interface::Cache::new(),
            &mut renderer,
        );
        let mut menu_bounds = UiBounds::default();
        ui.operate(&renderer, &mut menu_bounds);
        let luna = LUNA;
        let option = menu_bounds.label(luna.name);
        assert!(
            option.width >= text_width(luna.name, FOOTER_MENU_TEXT_SIZE),
            "popup options must stay readable even with a narrow chat"
        );
        let reasoning = menu_bounds.label("Reasoning");
        let speed = menu_bounds.label("Standard");
        assert!(option.x + option.width <= reasoning.x);
        assert!(speed.y > reasoning.y);
        assert!(
            menu_bounds
                .containers
                .iter()
                .any(|b| b.contains(option.center())
                    && b.contains(reasoning.center())
                    && b.contains(speed.center())
                    && b.x >= 0.
                    && b.y >= 0.
                    && b.x + b.width <= width
                    && b.y + b.height <= height
                    && b.width < width
                    && b.height < height)
        );
        let popup = menu_bounds
            .containers
            .iter()
            .filter(|b| {
                b.contains(option.center())
                    && b.contains(reasoning.center())
                    && b.contains(speed.center())
            })
            .min_by(|a, b| a.height.total_cmp(&b.height))
            .unwrap();
        if width == 1600. {
            if let Some(previous) = wide_popup_height {
                assert_eq!(
                    popup.height, previous,
                    "once choices fit, a taller window must not stretch the popup"
                );
            }
            wide_popup_height = Some(popup.height);
        }
        let messages = click_at(&mut ui, &mut renderer, label.center());
        assert!(
            matches!(messages.as_slice(), [Message::CloseFooterMenu]),
            "{messages:?}"
        );
        drop(ui);
        for message in messages {
            dispatch(&mut app, message).await;
        }
        assert!(!app.footer_menu_open);
    }
}

#[tokio::test]
async fn footer_selector_ignores_navigation_keys_and_selects_with_mouse() {
    use keyboard::key::Named;
    let mut app = app();
    dispatch(&mut app, Message::Create(PathBuf::from("/footer-project"))).await;
    app.settings_open = true;
    dispatch(&mut app, Message::ToggleFooterMenu).await;
    let session = app.current().unwrap();
    let before = (session.model.clone(), session.effort.clone(), session.fast);
    for key in [
        Named::Tab,
        Named::ArrowDown,
        Named::Enter,
        Named::ArrowUp,
        Named::ArrowLeft,
        Named::ArrowRight,
        Named::Space,
    ] {
        dispatch(&mut app, footer_key(key)).await;
        let session = app.current().unwrap();
        assert_eq!(
            (&session.model, &session.effort, session.fast),
            (&before.0, &before.1, before.2)
        );
        assert!(app.footer_menu_open);
    }
    let mut reverse_tab = footer_key(Named::Tab);
    if let Message::Event(iced::Event::Keyboard(keyboard::Event::KeyPressed {
        modifiers, ..
    })) = &mut reverse_tab
    {
        *modifiers = keyboard::Modifiers::SHIFT;
    }
    dispatch(&mut app, reverse_tab).await;
    dispatch(&mut app, footer_key(Named::Enter)).await;
    let session = app.current().unwrap();
    assert_eq!(
        (&session.model, &session.effort, session.fast),
        (&before.0, &before.1, before.2)
    );
    let luna = LUNA;
    for label in [luna.name, "None", "High", "Fast", "Standard"] {
        let messages = {
            let mut renderer = header_renderer();
            let mut ui = iced_runtime::UserInterface::build(
                app.view(),
                iced::Size::new(1200., 800.),
                iced_runtime::user_interface::Cache::new(),
                &mut renderer,
            );
            let mut bounds = UiBounds::default();
            ui.operate(&renderer, &mut bounds);
            click_at(&mut ui, &mut renderer, bounds.label(label).center())
        };
        assert_eq!(messages.len(), 1, "{messages:?}");
        dispatch(&mut app, messages.into_iter().next().unwrap()).await;
        let session = app.current().unwrap();
        match label {
            name if name == luna.name => assert_eq!(session.model, luna.id),
            "None" => assert_eq!(session.effort, "none"),
            "High" => assert_eq!(session.effort, "high"),
            "Fast" => assert!(session.fast),
            "Standard" => assert!(!session.fast),
            _ => unreachable!(),
        }
        assert!(app.footer_menu_open);
    }
    dispatch(&mut app, footer_key(Named::Escape)).await;
    assert!(!app.footer_menu_open);
    assert!(app.settings_open);
    dispatch(&mut app, Message::ToggleFooterMenu).await;
    let messages = {
        let mut renderer = header_renderer();
        let mut ui = iced_runtime::UserInterface::build(
            app.view(),
            iced::Size::new(1200., 800.),
            iced_runtime::user_interface::Cache::new(),
            &mut renderer,
        );
        click_at(&mut ui, &mut renderer, iced::Point::ORIGIN)
    };
    assert!(matches!(messages.as_slice(), [Message::CloseFooterMenu]));
    dispatch(&mut app, messages.into_iter().next().unwrap()).await;
    assert!(!app.footer_menu_open);
    assert!(app.settings_open);
}

#[tokio::test]
async fn footer_selector_outside_click_focuses_editor_and_activates_buttons() {
    use iced::advanced::widget::{Operation, operation::Focusable};
    use iced_runtime::{UserInterface, user_interface::Cache};

    #[derive(Default)]
    struct FocusBounds(Vec<iced::Rectangle>);
    impl Operation for FocusBounds {
        fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
            operate(self);
        }
        fn focusable(
            &mut self,
            _: Option<&iced::advanced::widget::Id>,
            bounds: iced::Rectangle,
            _: &mut dyn Focusable,
        ) {
            self.0.push(bounds);
        }
    }

    let mut app = app();
    dispatch(&mut app, Message::Create(PathBuf::from("/footer-project"))).await;
    dispatch(&mut app, Message::ToggleFooterMenu).await;
    let mut renderer = header_renderer();
    let size = iced::Size::new(app.window_width, app.window_height);
    let mut ui = UserInterface::build(app.view(), size, Cache::new(), &mut renderer);
    let mut bounds = FocusBounds::default();
    ui.operate(&renderer, &mut bounds);
    let [editor] = bounds.0.as_slice() else {
        panic!("expected only the composer to be focusable: {:?}", bounds.0);
    };
    // Click the editor on the side opposite the toolbar's model picker.
    let point = iced::Point::new(
        editor.x + editor.width - crate::growing_editor::PADDING,
        editor.center_y(),
    );
    let messages = click_at(&mut ui, &mut renderer, point);
    let cache = ui.into_cache();
    assert!(
        matches!(messages.first(), Some(Message::CloseFooterMenu)),
        "{messages:?}"
    );
    for message in messages {
        dispatch(&mut app, message).await;
    }
    assert!(!app.footer_menu_open);

    // Rebuild as the application does after dismissal, keeping widget focus.
    // Typing must work without a second click or a programmatic focus operation.
    let mut ui = UserInterface::build(app.view(), size, cache, &mut renderer);
    let mut messages = Vec::new();
    ui.update(
        &[iced::Event::Keyboard(keyboard::Event::KeyPressed {
            key: keyboard::Key::Character("x".into()),
            modified_key: keyboard::Key::Character("x".into()),
            physical_key: keyboard::key::Physical::Code(keyboard::key::Code::KeyX),
            location: keyboard::Location::Standard,
            modifiers: keyboard::Modifiers::empty(),
            text: Some("x".into()),
            repeat: false,
        })],
        mouse::Cursor::Unavailable,
        &mut renderer,
        &mut iced::advanced::clipboard::Null,
        &mut messages,
    );
    let cache = ui.into_cache();
    for message in messages {
        dispatch(&mut app, message).await;
    }
    assert_eq!(app.editor.text(), "x");

    // A real event loop may rebuild between the selector press and release.
    dispatch(&mut app, Message::ToggleFooterMenu).await;
    let mut ui = UserInterface::build(app.view(), size, cache, &mut renderer);
    let mut bounds = UiBounds::default();
    ui.operate(&renderer, &mut bounds);
    let session = app.current().unwrap();
    let selector = bounds
        .label(&format!(
            "{} {}",
            model(&session.model).unwrap().name,
            effort_name(&session.effort)
        ))
        .center();
    let mut cache = ui.into_cache();
    for event in [
        mouse::Event::ButtonPressed(mouse::Button::Left),
        mouse::Event::ButtonReleased(mouse::Button::Left),
    ] {
        let mut ui = UserInterface::build(app.view(), size, cache, &mut renderer);
        let mut messages = Vec::new();
        ui.update(
            &[iced::Event::Mouse(event)],
            mouse::Cursor::Available(selector),
            &mut renderer,
            &mut iced::advanced::clipboard::Null,
            &mut messages,
        );
        cache = ui.into_cache();
        for message in messages {
            dispatch(&mut app, message).await;
        }
        assert!(!app.footer_menu_open);
    }

    dispatch(&mut app, Message::ToggleSettings).await;
    dispatch(&mut app, Message::ToggleFooterMenu).await;
    let mut ui = UserInterface::build(app.view(), size, cache, &mut renderer);
    let mut bounds = UiBounds::default();
    ui.operate(&renderer, &mut bounds);
    let close = bounds.rightmost_control_at(bounds.label("Settings").center_y());
    let messages = click_at(&mut ui, &mut renderer, close.center());
    assert!(
        matches!(
            messages.as_slice(),
            [Message::CloseFooterMenu, Message::ToggleSettings]
        ),
        "{messages:?}"
    );
    drop(ui);
    for message in messages {
        dispatch(&mut app, message).await;
    }
    assert!(!app.footer_menu_open);
    assert!(!app.settings_open);
}

#[tokio::test]
async fn footer_selector_persists_changes_and_refreshes_model_defaults() {
    let mut app = app();
    app.catalog.settings.models.get_mut(LUNA.id).unwrap().effort = "none".into();
    app.catalog.settings.models.get_mut(LUNA.id).unwrap().fast = false;
    app.catalog
        .settings
        .models
        .get_mut(ASTRA.id)
        .unwrap()
        .effort = "max".into();
    app.catalog.settings.models.get_mut(ASTRA.id).unwrap().fast = true;
    stored_session(&mut app, PathBuf::from("/footer-persist")).await;
    dispatch(&mut app, Message::ToggleFooterMenu).await;
    dispatch(&mut app, Message::SessionModel(LUNA.id.into())).await;
    assert_eq!(app.current().unwrap().effort, "none");
    assert!(!app.current().unwrap().fast);
    dispatch(&mut app, Message::SessionEffort("max".into())).await;
    dispatch(&mut app, Message::SessionModel(ASTRA.id.into())).await;
    assert_eq!(app.current().unwrap().effort, "max");
    assert!(app.current().unwrap().fast);
    let menu = displayed_text(app.footer_popup(500.), iced::Size::new(500., 400.));
    assert!(menu.contains("Reasoning") && !menu.contains("None"));
    dispatch(&mut app, Message::SessionEffort("low".into())).await;
    dispatch(&mut app, Message::SessionFast(false)).await;
    dispatch(&mut app, Message::SessionFast(false)).await;
    assert!(app.footer_menu_open);
    let saved = app
        .harness
        .as_ref()
        .unwrap()
        .store
        .load_session(app.selected.as_ref().unwrap())
        .unwrap();
    assert_eq!(saved.model, ASTRA.id);
    assert_eq!(saved.effort, "low");
    assert!(!saved.fast);
    assert_eq!(app.catalog.settings.models[ASTRA.id].effort, "max");
    assert!(app.catalog.settings.models[ASTRA.id].fast);
}

#[tokio::test]
async fn footer_selector_recovers_failed_write_and_can_dismiss_during_save() {
    use keyboard::key::Named;
    let mut app = app();
    stored_session(&mut app, PathBuf::from("/footer-write-failure")).await;
    let h = app.harness.as_ref().unwrap().clone();
    let id = app.selected.clone().unwrap();
    let db = rusqlite::Connection::open(h.store.root().join("sessions.sqlite3")).unwrap();
    dispatch(&mut app, Message::ToggleFooterMenu).await;
    sqlite::fail_writes(&db, "sessions", "fixture footer write failure");
    dispatch(&mut app, Message::SessionModel(LUNA.id.into())).await;
    assert!(!app.io_busy);
    assert!(app.footer_menu_open);
    assert!(
        app.error
            .as_ref()
            .unwrap()
            .contains("fixture footer write failure")
    );
    // Unsaved choices remain in memory; another selection retries persistence.
    assert_eq!(app.current().unwrap().model, LUNA.id);
    sqlite::allow_writes(&db, "sessions");
    dispatch(&mut app, Message::ErrorDismiss).await;
    let save = app.update(Message::SessionEffort("none".into()));
    assert!(app.io_busy);
    dispatch(&mut app, footer_key(Named::Tab)).await;
    assert!(app.footer_menu_open);
    dispatch(&mut app, footer_key(Named::Escape)).await;
    assert!(!app.footer_menu_open);
    drain(&mut app, save).await;
    assert!(!app.io_busy);
    assert!(
        !app.footer_menu_open,
        "finishing a save must not reopen a dismissed popup"
    );
    let saved = h.store.load_session(&id).unwrap();
    assert_eq!(saved.model, LUNA.id);
    assert_eq!(saved.effort, "none");
    assert!(app.error.is_none());
}

#[tokio::test]
async fn quit_confirmation_releases_footer_keyboard_ownership() {
    let mut app = app();
    stored_session(&mut app, PathBuf::from("/running")).await;
    let running = app
        .harness
        .as_ref()
        .unwrap()
        .get(app.selected.as_ref().unwrap())
        .unwrap();
    running.running.store(true, Ordering::Release);
    stored_session(&mut app, PathBuf::from("/idle")).await;
    dispatch(&mut app, Message::ToggleFooterMenu).await;
    assert!(app.footer_menu_open);
    dispatch(
        &mut app,
        Message::Event(iced::Event::Window(window::Event::CloseRequested)),
    )
    .await;
    assert!(matches!(app.modal, Some(Modal::Quit)));
    assert!(!app.footer_menu_open);
    dispatch(&mut app, footer_key(keyboard::key::Named::Escape)).await;
    assert!(app.modal.is_none());
    running.running.store(false, Ordering::Release);
}

#[tokio::test]
async fn changes_counts_toggle_panel_and_refresh_expanded_file_after_same_size_edit() {
    use super::changes::Action;
    let mut app = app();
    let repo = tempfile::tempdir().unwrap();
    let root = repo.path();
    assert!(
        std::process::Command::new("git")
            .args(["init", "-q", "--template="])
            .current_dir(root)
            .status()
            .unwrap()
            .success()
    );
    let directory = "src/nested/component/with/a/long/directory/name";
    std::fs::create_dir_all(root.join(directory)).unwrap();
    let path = Path::new(directory).join("new.txt");
    std::fs::write(root.join(&path), "first version\nline 2\n").unwrap();
    dispatch(&mut app, Message::Create(root.to_owned())).await;
    let click_count = |app: &App, label: &str| {
        let mut renderer = header_renderer();
        let mut ui = iced_runtime::UserInterface::build(
            app.footer_content(900.),
            Size::new(900., 100.),
            Default::default(),
            &mut renderer,
        );
        let mut bounds = UiBounds::default();
        ui.operate(&renderer, &mut bounds);
        click_at(&mut ui, &mut renderer, bounds.label(label).center())
            .pop()
            .unwrap()
    };
    for (label, expected_open) in [("+2", true), ("−0", false), ("+2", true)] {
        let click = click_count(&app, label);
        dispatch(&mut app, click).await;
        assert_eq!(app.changes.open, expected_open);
    }
    let expand = {
        use iced::advanced::Renderer as _;
        let size = Size::new(app.panel_width(), 440.);
        let mut renderer = header_renderer();
        let mut ui = iced_runtime::UserInterface::build(
            app.changes_panel(),
            size,
            Default::default(),
            &mut renderer,
        );
        let mut bounds = UiBounds::default();
        ui.operate(&renderer, &mut bounds);
        let badge = bounds.label("A");
        let name = bounds.label("new.txt");
        let parent = bounds.label(directory);
        let count = bounds
            .labels
            .iter()
            .find(|(value, rect)| {
                value == "+2" && rect.contains(iced::Point::new(rect.center_x(), name.center_y()))
            })
            .unwrap()
            .1;
        assert!(
            badge.x + badge.width <= name.x
                && name.x + name.width <= parent.x
                && parent.x + parent.width <= count.x
        );
        assert!(
            [badge, parent, count]
                .iter()
                .all(|rect| (rect.center_y() - name.center_y()).abs() <= 1.)
        );
        let mut painted_badge = |cursor| {
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
                Size::new(size.width as u32, size.height as u32),
                1.,
                theme::default_colors().background,
            );
            (badge.y.ceil() as usize..(badge.y + badge.height).floor() as usize)
                .flat_map(|y| {
                    let start = (y * size.width as usize + badge.x.ceil() as usize) * 4;
                    &pixels[start..start + badge.width.floor() as usize * 4]
                })
                .copied()
                .collect::<Vec<_>>()
        };
        assert_ne!(
            painted_badge(mouse::Cursor::Unavailable),
            painted_badge(mouse::Cursor::Available(count.center())),
            "hovering the far end of the row must swap the badge glyph"
        );
        click_at(&mut ui, &mut renderer, count.center())
            .pop()
            .unwrap()
    };
    assert!(matches!(&expand,Message::Changes(Action::File(clicked)) if clicked == &path));
    dispatch(&mut app, expand).await;
    let panel = |app: &App| displayed_text(app.changes_panel(), Size::new(480., 800.));
    assert!(panel(&app).contains("first version"));
    let mut watcher = inotify::Inotify::init().unwrap();
    watcher
        .watches()
        .add(root.join(&path), inotify::WatchMask::ACCESS)
        .unwrap();
    dispatch(&mut app, Message::GitTick).await;
    assert!(
        matches!(watcher.read_events(&mut [0;1024]),Err(e) if e.kind()==std::io::ErrorKind::WouldBlock),
        "an unchanged open patch and its cached line count need no content read"
    );
    std::fs::write(root.join(&path), "other version\nline 2\n").unwrap();
    dispatch(&mut app, Message::GitTick).await;
    assert!(
        panel(&app).contains("other version"),
        "expanded patch must follow edits even with identical counts"
    );
    std::fs::remove_file(root.join(path)).unwrap();
    dispatch(&mut app, Message::GitTick).await;
    assert!(panel(&app).contains("No changed files"));
    dispatch(&mut app, Message::ToggleSettings).await;
    assert!(!app.changes.open && app.settings_open);
}

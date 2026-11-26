use super::*;
use browser::Action;
use iced::advanced::Renderer as _;
use iced_runtime::{UserInterface, user_interface::Cache};

#[tokio::test]
#[ignore = "requires packaged CEF; run just browser-smoke"]
async fn native_browser_panel_renders_and_routes_input_after_session_switch() {
    let mut app = app();
    let root = app.smoke_dir.clone().unwrap();
    stored_session(&mut app, root.clone()).await;
    let first = app.selected.clone().unwrap();
    stored_session(&mut app, root).await;
    let second = app.selected.clone().unwrap();
    dispatch(&mut app, Message::Select(first.clone())).await;
    let b = app.harness.as_ref().unwrap().browser.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/page", listener.local_addr().unwrap());
    let http = tokio::spawn(async move {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            tokio::spawn(async move {
                let mut line = String::new();
                if BufReader::new(&mut socket)
                    .read_line(&mut line)
                    .await
                    .unwrap_or(0)
                    == 0
                {
                    return;
                }
                let html = r#"<body style="background:#de317a"><input style="position:absolute;right:10px;top:10px;width:60px;height:40px" onkeydown="if(event.keyCode===46)window.bad=true" oninput="if(this.value==='.'&&!window.bad)document.body.style.background='#30c974'">"#;
                let _=socket.write_all(format!("HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Type: text/html\r\nContent-Length: {}\r\n\r\n{html}",html.len()).as_bytes()).await;
            });
        }
    });
    let mut renderer = header_renderer();
    let size = Size::new(1200., 800.);
    let mut cache = Cache::default();
    dispatch(&mut app, Message::Browser(Action::Toggle)).await;
    dispatch(&mut app, Message::Browser(Action::Location(url))).await;
    dispatch(&mut app, Message::Browser(Action::Go)).await;
    // Keep the same iced tree and bounds across switches. Each remounted CEF
    // surface needs its own resize, not just the first one measured by iced.
    let mut switched = false;
    let mut clicked = false;
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    loop {
        app.refresh_browser();
        let mut ui = UserInterface::build(app.view(), size, cache, &mut renderer);
        let mut layout = UiBounds::default();
        ui.operate(&renderer, &mut layout);
        let page = layout.browser_page.expect("active page");
        let mut messages = Vec::new();
        ui.update(
            &[iced::Event::Window(window::Event::RedrawRequested(
                iced::time::Instant::now(),
            ))],
            mouse::Cursor::Unavailable,
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
            mouse::Cursor::Unavailable,
        );
        let pixels = crate::test_renderer::screenshot(
            &mut renderer,
            Size::new(1200, 800),
            1.,
            iced::Color::BLACK,
        );
        let i = 4 * (page.center_y() as usize * 1200 + page.center_x() as usize);
        let pixel = &pixels[i..i + 3];
        if std::time::Instant::now() >= deadline {
            let frame = b
                .frame(&first, None)
                .unwrap()
                .map(|f| (f.width, f.height, f.bgra[0..4].to_vec()));
            panic!(
                "page={page:?} pixel={pixel:?} status={:?} CEF={frame:?} error={:?} switched={switched} clicked={clicked}",
                b.status(&first),
                app.error
            );
        }
        if pixel == [48, 201, 116] {
            assert!(clicked);
            break;
        }
        let pink = pixel == [222, 49, 122];
        if switched && !clicked && pink {
            messages.extend(click_at(
                &mut ui,
                &mut renderer,
                iced::Point::new(page.x + page.width - 36., page.y + 27.),
            ));
            let key = iced::keyboard::Key::Character(".".into());
            ui.update(
                &[iced::Event::Keyboard(keyboard::Event::KeyPressed {
                    key: key.clone(),
                    modified_key: key,
                    physical_key: keyboard::key::Physical::Code(keyboard::key::Code::Period),
                    location: keyboard::Location::Standard,
                    modifiers: keyboard::Modifiers::NONE,
                    text: Some(".".into()),
                    repeat: false,
                })],
                mouse::Cursor::Unavailable,
                &mut renderer,
                &mut iced::advanced::clipboard::Null,
                &mut messages,
            );
            clicked = true;
        }
        cache = ui.into_cache();
        drive_ui_messages(
            App::view,
            &mut app,
            &mut renderer,
            &mut cache,
            size,
            messages,
        )
        .await;
        if pink && !switched {
            dispatch(&mut app, Message::Select(second.clone())).await;
            let mut other = UserInterface::build(app.view(), size, cache, &mut renderer);
            let mut m = vec![];
            other.update(
                &[iced::Event::Window(window::Event::RedrawRequested(
                    iced::time::Instant::now(),
                ))],
                mouse::Cursor::Unavailable,
                &mut renderer,
                &mut iced::advanced::clipboard::Null,
                &mut m,
            );
            cache = other.into_cache();
            drive_ui_messages(App::view, &mut app, &mut renderer, &mut cache, size, m).await;
            dispatch(&mut app, Message::Select(first.clone())).await;
            switched = true;
        }
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    dispatch(&mut app, Message::ToggleAgents).await;
    assert!(!app.browser.open && app.agents_open);
    b.shutdown().unwrap();
    http.abort();
}

// Drive the toolbar through the rendered UI; menu overlays and inline Find must
// not displace the page, even when a narrow panel hides the driver's label.
#[tokio::test]
#[ignore = "requires packaged CEF; run just browser-smoke"]
async fn native_browser_toolbar_preserves_page_until_close() {
    let mut app = app();
    let project = app.smoke_dir.clone().unwrap();
    stored_session(&mut app, project).await;
    let id = app.selected.clone().unwrap();
    let browser = app.harness.as_ref().unwrap().browser.clone();
    let size = Size::new(1600., 720.);
    app.window_width = size.width;
    app.catalog.settings.settings_width = 600.;
    dispatch(&mut app, Message::Browser(Action::Toggle)).await;
    let tab = browser.status(&id).surface_id.unwrap();
    let mut renderer = header_renderer();
    let mut cache = Cache::default();
    let mut probe = |app: &App, point: Option<iced::Point>| {
        let mut ui =
            UserInterface::build(app.view(), size, std::mem::take(&mut cache), &mut renderer);
        let mut bounds = UiBounds::default();
        ui.operate(&renderer, &mut bounds);
        let messages = point
            .map(|p| click_at(&mut ui, &mut renderer, p))
            .unwrap_or_default();
        cache = ui.into_cache();
        (bounds, messages)
    };
    let wide = probe(&app, None).0;
    let driver = wide.label("Agent drives");
    let address = wide.browser_field.unwrap();
    assert!(driver.y <= address.center_y() && address.center_y() <= driver.y + driver.height);
    for m in probe(&app, Some(driver.center())).1 {
        dispatch(&mut app, m).await;
    }
    let handoff = probe(&app, None).0;
    assert_eq!(wide.browser_page, handoff.browser_page);
    for m in probe(&app, Some(handoff.label("Take control").center())).1 {
        dispatch(&mut app, m).await;
    }
    assert!(browser.status(&id).user_control);

    probe(&app, Some(address.center()));
    app.catalog.settings.settings_width = 260.;
    let narrow = probe(&app, None).0;
    assert_eq!(
        narrow.focused,
        Some(iced::advanced::widget::Id::new("browser-location"))
    );
    let field = narrow.browser_field.unwrap();
    let page = narrow.browser_page.unwrap();
    assert_eq!(page.y, wide.browser_page.unwrap().y);
    assert!(field.width > 0. && field.width < address.width);
    assert!(narrow.labels.iter().all(|(label, _)| label != "You drive"));
    // The hide and options buttons are the last two toolbar controls before
    // the app rail. Locate their centers in layout rather than hardcoding pixels.
    let mut controls: Vec<_> = narrow
        .containers
        .iter()
        .filter(|r| {
            r.x > field.x + field.width
                && r.x + r.width <= page.x + page.width
                && r.height > 0.
                && r.width > 1.
                && (r.center_y() - field.center_y()).abs() < 1.
        })
        .map(|r| r.center())
        .collect();
    controls.sort_by(|a, b| b.x.total_cmp(&a.x));
    controls.dedup();
    let (hide, options) = (controls[0], controls[1]);
    for m in probe(&app, Some(options)).1 {
        dispatch(&mut app, m).await;
    }
    let menu = probe(&app, None).0;
    assert_eq!(Some(page), menu.browser_page);
    for m in probe(&app, Some(menu.label("Find on page").center())).1 {
        dispatch(&mut app, m).await;
    }
    assert!(app.browser.find_open());
    let find = probe(&app, None).0;
    assert_eq!(find.browser_page, Some(page));
    assert_eq!(find.browser_field.unwrap().center_y(), field.center_y());
    for m in probe(&app, Some(page.center())).1 {
        dispatch(&mut app, m).await;
    }
    assert!(app.browser.page_focused());
    let Message::Event(escape) = footer_key(keyboard::key::Named::Escape) else {
        unreachable!()
    };
    let mut renderer = header_renderer();
    let mut ui = UserInterface::build(app.view(), size, Cache::default(), &mut renderer);
    let mut keys = Vec::new();
    ui.update(
        &[escape],
        mouse::Cursor::Unavailable,
        &mut renderer,
        &mut iced::advanced::clipboard::Null,
        &mut keys,
    );
    drop(ui);
    assert!(matches!(
        keys.as_slice(),
        [Message::Browser(Action::EndFind)]
    ));
    for m in keys {
        dispatch(&mut app, m).await;
    }
    assert!(!app.browser.find_open());
    for m in probe(&app, Some(hide)).1 {
        dispatch(&mut app, m).await;
    }
    assert!(!app.browser.open);
    assert_eq!(browser.status(&id).surface_id, Some(tab));
    dispatch(&mut app, Message::Browser(Action::Toggle)).await;
    for m in probe(&app, Some(options)).1 {
        dispatch(&mut app, m).await;
    }
    let close = probe(&app, None).0.label("Close tab");
    for m in probe(&app, Some(close.center())).1 {
        dispatch(&mut app, m).await;
    }
    assert!(!app.browser.open && browser.status(&id).surface_id.is_none());
    browser.shutdown().unwrap();
}

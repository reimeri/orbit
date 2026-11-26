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

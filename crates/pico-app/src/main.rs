use std::path::Path;
use std::process::ExitCode;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use iced::widget::{button, column, container, row, text, text_input};
use iced::{Element, Fill, Subscription, Task, Theme, window};
use pico_core::APP_ID;

fn main() -> ExitCode {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let smoke_test = match args.as_slice() {
        [] => false,
        [arg] if arg == "--smoke-test" => true,
        [arg] if arg == "--help" || arg == "-h" => {
            println!(
                "Usage: pico-evo [--smoke-test]\n\nRequires a native Wayland session.\n--smoke-test  Render a window, verify its screenshot, and exit."
            );
            return ExitCode::SUCCESS;
        }
        _ => {
            eprintln!("Unknown arguments. Run pico-evo --help for usage.");
            return ExitCode::FAILURE;
        }
    };

    if std::env::var_os("WAYLAND_DISPLAY").is_none_or(|value| value.is_empty()) {
        eprintln!(
            "Pico V2 requires a native Wayland session. Start it from your Wayland desktop,\nthen run `nix develop --command cargo run`. For a headless check, use `just smoke` inside nix develop."
        );
        return ExitCode::FAILURE;
    }

    let smoke_passed = Arc::new(AtomicBool::new(false));
    let result = iced::application(
        {
            let smoke_passed = Arc::clone(&smoke_passed);
            move || App::new(smoke_test, Arc::clone(&smoke_passed))
        },
        App::update,
        App::view,
    )
    .title("Pico V2 — Phase 0")
    .theme(Theme::Dark)
    .subscription(App::subscription)
    .window(window::Settings {
        size: iced::Size::new(900.0, 560.0),
        min_size: Some(iced::Size::new(600.0, 400.0)),
        platform_specific: window::settings::PlatformSpecific {
            application_id: APP_ID.to_owned(),
            ..Default::default()
        },
        ..Default::default()
    })
    .run();

    if let Err(error) = result {
        eprintln!(
            "Pico could not start: {error}\nCheck WAYLAND_DISPLAY and XDG_RUNTIME_DIR. For GPU issues, retry with\n`ICED_BACKEND=tiny-skia cargo run` inside nix develop."
        );
        return ExitCode::FAILURE;
    }
    if smoke_test && !smoke_passed.load(Ordering::Relaxed) {
        eprintln!("Window smoke test failed: no valid rendered screenshot was received.");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

struct App {
    input: String,
    clicks: u32,
    config_path: String,
    smoke_test: bool,
    smoke_passed: Arc<AtomicBool>,
}

#[derive(Debug, Clone)]
enum Message {
    InputChanged(String),
    Increment,
    Reset,
    WindowOpened(window::Id),
    Screenshot(window::Screenshot),
}

impl App {
    fn new(smoke_test: bool, smoke_passed: Arc<AtomicBool>) -> Self {
        let xdg = std::env::var_os("XDG_CONFIG_HOME");
        let home = std::env::var_os("HOME");
        let config_path = pico_core::config_dir(
            xdg.as_deref().map(Path::new),
            home.as_deref().map(Path::new),
        )
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "Unavailable — set HOME or XDG_CONFIG_HOME".to_owned());
        Self {
            input: String::new(),
            clicks: 0,
            config_path,
            smoke_test,
            smoke_passed,
        }
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::InputChanged(input) => self.input = input,
            Message::Increment => self.clicks = self.clicks.saturating_add(1),
            Message::Reset => {
                self.input.clear();
                self.clicks = 0;
            }
            Message::WindowOpened(id) => return window::screenshot(id).map(Message::Screenshot),
            Message::Screenshot(screenshot) => {
                let pixels = &screenshot.rgba;
                let valid = screenshot.size.width > 0
                    && screenshot.size.height > 0
                    && pixels.len()
                        == screenshot.size.width as usize * screenshot.size.height as usize * 4
                    && pixels
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .any(|pixel| pixel != &pixels[..4]);
                self.smoke_passed.store(valid, Ordering::Relaxed);
                if valid {
                    println!(
                        "SMOKE_OK: native Wayland window rendered ({}x{}, non-uniform RGBA screenshot)",
                        screenshot.size.width, screenshot.size.height
                    );
                }
                return iced::exit();
            }
        }
        Task::none()
    }

    fn subscription(&self) -> Subscription<Message> {
        if self.smoke_test {
            window::open_events().map(Message::WindowOpened)
        } else {
            Subscription::none()
        }
    }

    fn view(&self) -> Element<'_, Message> {
        let content = column![
            text("PICO / V2").size(16),
            text("The foundation is ready.").size(34),
            text("Phase 0 · Rust + iced · Native Wayland").size(18),
            text("This is a development smoke window, not the Phase 1 harness UI."),
            text_input("Type here to test keyboard input…", &self.input)
                .on_input(Message::InputChanged)
                .padding(12),
            row![
                button("Test interaction")
                    .padding(12)
                    .on_press(Message::Increment),
                text(format!("Clicks: {}", self.clicks)),
                button("Reset").padding(12).on_press(Message::Reset),
            ]
            .spacing(16)
            .align_y(iced::Center),
            text(format!("Future configuration: {}", self.config_path)).size(14),
            text("No configuration or session files are written in Phase 0.").size(14),
        ]
        .spacing(20)
        .max_width(720);
        container(content)
            .padding(32)
            .center_x(Fill)
            .center_y(Fill)
            .into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interaction_and_reset() {
        let mut app = App::new(false, Arc::new(AtomicBool::new(false)));
        let _ = app.update(Message::InputChanged("Hello Wayland".into()));
        let _ = app.update(Message::Increment);
        assert_eq!(app.input, "Hello Wayland");
        assert_eq!(app.clicks, 1);
        let _ = app.update(Message::Reset);
        assert!(app.input.is_empty());
        assert_eq!(app.clicks, 0);
    }
}

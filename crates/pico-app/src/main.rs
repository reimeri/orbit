mod agent_icon;
mod animated_column;
mod chat_selection;
mod ellipsized_text;
mod git;
mod io_worker;
mod pointer_input;
mod questionnaire;
mod search_icon;
mod tool_output;
mod ui;
mod virtual_list;
use iced::{Theme, window};
use pico_core::APP_ID;
use std::{
    process::ExitCode,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use ui::App;
fn main() -> ExitCode {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let smoke = match args.as_slice() {
        [] => false,
        [arg] if arg == "--smoke-test" => true,
        [arg] if arg == "--help" || arg == "-h" => {
            println!(
                "Usage: pico-evo [--smoke-test]\n\nNative Wayland coding harness. Settings and credentials: $XDG_CONFIG_HOME/pico-evo (or ~/.config/pico-evo).\n--smoke-test uses isolated temporary state and never connects to OpenAI."
            );
            return ExitCode::SUCCESS;
        }
        _ => {
            eprintln!("Unknown arguments. Run pico-evo --help.");
            return ExitCode::FAILURE;
        }
    };
    if std::env::var_os("WAYLAND_DISPLAY").is_none_or(|v| v.is_empty()) {
        eprintln!(
            "Pico V2 requires native Wayland. Run from a Wayland desktop, or use `nix develop --command just smoke` for a headless check."
        );
        return ExitCode::FAILURE;
    }
    let passed = Arc::new(AtomicBool::new(false));
    let flag = passed.clone();
    let result = iced::application(
        move || App::new(smoke, flag.clone()),
        App::update,
        App::view,
    )
    .title("Pico V2")
    .theme(Theme::custom(
        "Pico",
        iced::theme::Palette {
            background: iced::Color::from_rgb8(24, 26, 30),
            text: iced::Color::from_rgb8(225, 228, 234),
            primary: iced::Color::from_rgb8(137, 175, 210),
            success: iced::Color::from_rgb8(125, 192, 154),
            danger: iced::Color::from_rgb8(224, 130, 130),
            warning: iced::Color::from_rgb8(224, 186, 120),
        },
    ))
    .subscription(App::subscription)
    .window(window::Settings {
        size: iced::Size::new(1200., 800.),
        min_size: Some(iced::Size::new(860., 560.)),
        exit_on_close_request: false,
        platform_specific: window::settings::PlatformSpecific {
            application_id: APP_ID.into(),
            ..Default::default()
        },
        ..Default::default()
    })
    .run();
    if let Err(error) = result {
        eprintln!(
            "Pico could not start: {error}\nCheck your Wayland session. For GPU trouble retry `ICED_BACKEND=tiny-skia cargo run` inside nix develop."
        );
        return ExitCode::FAILURE;
    }
    if smoke && !passed.load(Ordering::Relaxed) {
        eprintln!("Window smoke test failed.");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

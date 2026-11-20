mod activity;
mod agent_icon;
mod attachments;
mod chat_selection;
mod chevron_icon;
mod ellipsized_text;
mod equations;
mod file_completion;
mod git;
mod growing_editor;
mod icons;
mod image_transition;
mod image_view;
mod io_worker;
mod keyed_column;
mod markdown_images;
mod notifications;
mod pointer_input;
mod questionnaire;
mod recent_previews;
mod screenshot_check;
mod scrollbar;
mod session_action;
mod session_wave;
mod spellcheck;
mod status_circle;
#[cfg(test)]
mod test_renderer;
mod theme;
mod tool_output;
mod transcript_viewport;
mod ui;
mod virtual_list;
use iced::window;
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
    .theme(App::theme)
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
            "Pico could not start: {error}\nCheck your native Wayland session and Vulkan/graphics driver installation, then restart Pico. Re-enter `nix develop` if loader libraries are missing. Pico requires wgpu; there is no software-renderer fallback."
        );
        return ExitCode::FAILURE;
    }
    if smoke && !passed.load(Ordering::Relaxed) {
        eprintln!("Window smoke test failed.");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

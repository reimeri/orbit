mod activity;
mod agent_icon;
mod attachments;
mod chat_selection;
mod chevron_icon;
mod editor_history;
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
mod markdown_window;
mod notifications;
mod pointer_input;
mod questionnaire;
mod recent_previews;
mod screenshot_check;
mod scrollbar;
mod session_action;
mod session_list_focus;
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
mod worktrees;
use iced::window;
use orbit_core::APP_ID;
use std::{
    process::ExitCode,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use ui::App;
#[derive(Clone)]
enum Launch {
    Normal,
    Smoke,
    Marketing(std::path::PathBuf),
}

fn main() -> ExitCode {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let launch = match args.as_slice() {
        [] => Launch::Normal,
        [arg] if arg == "--smoke-test" => Launch::Smoke,
        [arg, path] if arg == "--marketing-shot" => Launch::Marketing(path.into()),
        [arg] if arg == "--help" || arg == "-h" => {
            println!(
                "Usage: orbit [--smoke-test | --marketing-shot OUTPUT.png]\n\nNative Wayland coding harness. Settings and credentials: $XDG_CONFIG_HOME/orbit (or ~/.config/orbit).\nCapture modes use isolated temporary state and never connect to OpenAI. --marketing-shot saves a staged window as a PNG."
            );
            return ExitCode::SUCCESS;
        }
        _ => {
            eprintln!("Unknown arguments. Run orbit --help.");
            return ExitCode::FAILURE;
        }
    };
    if std::env::var_os("WAYLAND_DISPLAY").is_none_or(|v| v.is_empty()) {
        eprintln!(
            "Orbit requires native Wayland. Run from a Wayland desktop, or use `nix develop --command just smoke` for a headless check."
        );
        return ExitCode::FAILURE;
    }
    let capturing = !matches!(launch, Launch::Normal);
    let passed = Arc::new(AtomicBool::new(false));
    let flag = passed.clone();
    let result = iced::application(
        move || App::new(launch.clone(), flag.clone()),
        App::update,
        App::view,
    )
    .title("Orbit")
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
            "Orbit could not start: {error}\nCheck your native Wayland session and Vulkan/graphics driver installation, then restart Orbit. Re-enter `nix develop` if loader libraries are missing. Orbit requires wgpu; there is no software-renderer fallback."
        );
        return ExitCode::FAILURE;
    }
    if capturing && !passed.load(Ordering::Relaxed) {
        eprintln!("Window capture did not complete.");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

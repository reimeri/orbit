//! UI-independent Pico V2 foundations. Never depend on iced in this crate.

pub use anyhow::Error;
pub use tokio_util::sync::CancellationToken;
pub mod agent;
pub mod auth;
pub mod code_mode;
pub mod history;
pub mod image_tools;
pub mod markdown_image;
pub mod media;
pub mod models;
mod output;
pub mod provider;
pub mod questionnaire;
pub mod shell;
pub mod store;
pub mod web_search;

use std::path::{Path, PathBuf};

/// V2 must not read or overwrite the previous harness's configuration.
pub const APP_ID: &str = "pico-evo";

/// Resolve the configuration directory without creating or modifying anything.
/// Relative XDG paths are invalid according to the XDG Base Directory spec.
pub fn config_dir(xdg_config_home: Option<&Path>, home: Option<&Path>) -> Option<PathBuf> {
    xdg_config_home
        .filter(|path| path.is_absolute())
        .map(Path::to_path_buf)
        .or_else(|| {
            home.filter(|path| path.is_absolute())
                .map(|path| path.join(".config"))
        })
        .map(|path| path.join(APP_ID))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uses_xdg_config_home() {
        assert_eq!(
            config_dir(Some(Path::new("/config")), Some(Path::new("/home/user"))),
            Some(PathBuf::from("/config/pico-evo"))
        );
    }

    #[test]
    fn falls_back_to_home_for_missing_or_invalid_xdg() {
        for xdg in [None, Some(Path::new("relative")), Some(Path::new(""))] {
            assert_eq!(
                config_dir(xdg, Some(Path::new("/home/user"))),
                Some(PathBuf::from("/home/user/.config/pico-evo"))
            );
        }
    }

    #[test]
    fn does_not_invent_a_path_without_an_absolute_base() {
        assert_eq!(config_dir(None, None), None);
        assert_eq!(config_dir(None, Some(Path::new("relative"))), None);
        assert_eq!(
            config_dir(Some(Path::new("/config")), None),
            Some(PathBuf::from("/config/pico-evo"))
        );
    }
}

#[cfg(test)]
mod test_support;

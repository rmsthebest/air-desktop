//! Where air-desktop keeps its files.
//!
//! `AIR_DESKTOP_CONFIG_DIR` overrides the config location, e.g. to run two
//! instances side by side during development.

use std::path::PathBuf;

use directories::ProjectDirs;

pub fn config_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("AIR_DESKTOP_CONFIG_DIR") {
        return Some(dir.into());
    }
    ProjectDirs::from("", "", "air-desktop").map(|dirs| dirs.config_dir().to_path_buf())
}

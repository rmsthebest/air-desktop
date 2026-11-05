//! Persisted app preferences (`settings.json` in the config dir).
//!
//! Unknown or invalid fields fall back to defaults rather than failing, so a
//! hand-edited file never prevents startup. Writes are atomic.

use std::path::{Path, PathBuf};

use anyhow::Context as _;
use gpui::{App, Global};
use serde::{Deserialize, Serialize};

use crate::theme;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Appearance {
    #[default]
    System,
    Light,
    Dark,
}

impl Appearance {
    pub fn next(self) -> Self {
        match self {
            Self::System => Self::Light,
            Self::Light => Self::Dark,
            Self::Dark => Self::System,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub appearance: Appearance,
    pub light_theme: String,
    pub dark_theme: String,
    pub details_open: bool,
    pub chat_list_width: f32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            appearance: Appearance::System,
            light_theme: theme::DEFAULT_LIGHT.into(),
            dark_theme: theme::DEFAULT_DARK.into(),
            details_open: true,
            chat_list_width: 320.,
        }
    }
}

impl Settings {
    /// Reads settings from `path`. A missing file yields defaults; an
    /// unreadable or invalid one yields defaults plus an error to report.
    pub fn load(path: &Path) -> (Self, Option<String>) {
        let source = match std::fs::read_to_string(path) {
            Ok(source) => source,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return (Self::default(), None);
            }
            Err(err) => return (Self::default(), Some(format!("{}: {err}", path.display()))),
        };
        match serde_json::from_str(&source) {
            Ok(settings) => (settings, None),
            Err(err) => (
                Self::default(),
                Some(format!("{}: {err}; using defaults", path.display())),
            ),
        }
    }

    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        let dir = path.parent().context("settings path has no parent")?;
        std::fs::create_dir_all(dir)?;
        let json = serde_json::to_vec_pretty(self)?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }
}

/// The live settings plus where to persist them. `path` is `None` when there
/// is no config dir (and in tests), in which case changes stay in memory.
pub struct SettingsStore {
    path: Option<PathBuf>,
    settings: Settings,
}

impl Global for SettingsStore {}

impl SettingsStore {
    pub fn init(path: Option<PathBuf>, settings: Settings, cx: &mut App) {
        cx.set_global(Self { path, settings });
    }

    pub fn get(cx: &App) -> &Settings {
        &cx.global::<Self>().settings
    }

    /// Applies `edit` and persists the result if anything changed.
    pub fn update(cx: &mut App, edit: impl FnOnce(&mut Settings)) {
        let store = cx.global_mut::<Self>();
        let before = store.settings.clone();
        edit(&mut store.settings);
        if store.settings == before {
            return;
        }
        if let Some(path) = &store.path
            && let Err(err) = store.settings.save(path)
        {
            tracing::warn!("failed to save settings: {err:#}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/settings.json");
        let settings = Settings {
            appearance: Appearance::Dark,
            chat_list_width: 280.,
            ..Settings::default()
        };
        settings.save(&path).unwrap();
        assert_eq!(Settings::load(&path), (settings, None));
    }

    #[test]
    fn missing_file_is_silent_defaults() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            Settings::load(&dir.path().join("settings.json")),
            (Settings::default(), None)
        );
    }

    #[test]
    fn partial_file_keeps_defaults_for_missing_fields() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(&path, r#"{ "appearance": "light", "future_field": 1 }"#).unwrap();
        let (settings, error) = Settings::load(&path);
        assert_eq!(error, None);
        assert_eq!(settings.appearance, Appearance::Light);
        assert_eq!(settings.dark_theme, theme::DEFAULT_DARK);
    }

    #[test]
    fn corrupt_file_reports_and_uses_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(&path, r#"{ "appearance": "purple" }"#).unwrap();
        let (settings, error) = Settings::load(&path);
        assert_eq!(settings, Settings::default());
        assert!(error.is_some());
    }
}

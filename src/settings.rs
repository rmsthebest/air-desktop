//! Persisted app preferences (`settings.json` in the config dir).
//!
//! Missing and unknown fields are fine. A file that doesn't parse is never
//! fatal: the app runs on defaults and, so the user's hand edits aren't
//! lost, stops saving until the file is fixed. Writes are atomic.

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

/// Which keymap layers are active. Helix adds modal, Helix-style bindings
/// on top of the standard ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeymapMode {
    #[default]
    Standard,
    Helix,
}

/// What desktop notifications show. Peers choose names and text, and a
/// notification can appear over a shared screen, so this is the user's call.
/// Ordered from most to least revealing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Notifications {
    /// The chat's name and the message.
    #[default]
    Full,
    /// That something arrived, not where from or what.
    Private,
    Off,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub keymap_mode: KeymapMode,
    pub appearance: Appearance,
    pub light_theme: String,
    pub dark_theme: String,
    pub details_open: bool,
    pub chat_list_width: f32,
    pub notifications: Notifications,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            keymap_mode: KeymapMode::Standard,
            appearance: Appearance::System,
            light_theme: theme::DEFAULT_LIGHT.into(),
            dark_theme: theme::DEFAULT_DARK.into(),
            details_open: true,
            chat_list_width: 320.,
            notifications: Notifications::Full,
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
    #[cfg(test)]
    pub fn init(path: Option<PathBuf>, settings: Settings, cx: &mut App) {
        cx.set_global(Self { path, settings });
    }

    /// Loads settings from `path` and installs the store. Returns a problem
    /// to report, if any.
    pub fn open(path: Option<PathBuf>, cx: &mut App) -> Option<String> {
        let (store, warning) = Self::load(path);
        cx.set_global(store);
        warning
    }

    fn load(path: Option<PathBuf>) -> (Self, Option<String>) {
        let Some(path) = path else {
            return (
                Self {
                    path: None,
                    settings: Settings::default(),
                },
                None,
            );
        };
        match Settings::load(&path) {
            (settings, None) => (
                Self {
                    path: Some(path),
                    settings,
                },
                None,
            ),
            // Don't save over a file we couldn't read.
            (settings, Some(error)) => (
                Self {
                    path: None,
                    settings,
                },
                Some(format!(
                    "{error}. Settings changes won't be saved until it is fixed."
                )),
            ),
        }
    }

    pub fn get(cx: &App) -> &Settings {
        &cx.global::<Self>().settings
    }

    /// Applies `edit` and persists the result if anything changed. The
    /// change applies in memory even if saving fails.
    pub fn update(cx: &mut App, edit: impl FnOnce(&mut Settings)) -> anyhow::Result<()> {
        cx.global_mut::<Self>().apply(edit)
    }

    fn apply(&mut self, edit: impl FnOnce(&mut Settings)) -> anyhow::Result<()> {
        let before = self.settings.clone();
        edit(&mut self.settings);
        match &self.path {
            Some(path) if self.settings != before => {
                self.settings.save(path).context("couldn't save settings")
            }
            _ => Ok(()),
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
    fn store_never_overwrites_a_file_it_could_not_parse() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let hand_edited = r#"{ "appearance": "purple", "chat_list_width": 300 }"#;
        std::fs::write(&path, hand_edited).unwrap();

        let (mut store, warning) = SettingsStore::load(Some(path.clone()));
        assert!(warning.is_some());
        store
            .apply(|settings| settings.details_open = false)
            .unwrap();
        assert!(!store.settings.details_open);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), hand_edited);
    }

    #[test]
    fn store_saves_changes_to_a_valid_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let (mut store, warning) = SettingsStore::load(Some(path.clone()));
        assert_eq!(warning, None);
        store
            .apply(|settings| settings.details_open = false)
            .unwrap();
        assert_eq!(Settings::load(&path).0, store.settings);
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

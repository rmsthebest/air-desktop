//! Theme setup: the bundled Air themes and any user themes, applied on top of
//! gpui-component's theme system so every component picks them up.
//!
//! User themes are `*.json` files in `<config>/themes`, in gpui-component's
//! theme format. They are read at startup. (gpui-component's own directory
//! watcher is not used: its reload drops themes registered from strings,
//! including ours.)

use std::path::Path;
use std::rc::Rc;

use anyhow::Context as _;
use gpui::{App, Window};
use gpui_kit::component::{Theme, ThemeConfig, ThemeMode, ThemeRegistry};

use crate::settings::{Appearance, SettingsStore};

const BUNDLED_THEMES: &str = include_str!("../assets/themes/air.json");

pub const DEFAULT_LIGHT: &str = "Air Light";
pub const DEFAULT_DARK: &str = "Air Dark";

/// Registers bundled and user themes and applies the configured pair.
/// Must run after `gpui_kit::init` and `SettingsStore::init`. Returns
/// problems to report; none of them are fatal.
pub fn init(user_dir: Option<&Path>, cx: &mut App) -> anyhow::Result<Vec<String>> {
    ThemeRegistry::global_mut(cx)
        .load_themes_from_str(BUNDLED_THEMES)
        .context("bundled theme file is invalid")?;

    let mut errors = user_dir
        .map(|dir| load_user_themes(dir, cx))
        .unwrap_or_default();

    let settings = SettingsStore::get(cx).clone();
    if let Err(err) = set_theme_pair(&settings.light_theme, &settings.dark_theme, cx) {
        errors.push(format!("{err:#}; using the default themes"));
        set_theme_pair(DEFAULT_LIGHT, DEFAULT_DARK, cx)?;
    }
    Ok(errors)
}

fn load_user_themes(dir: &Path, cx: &mut App) -> Vec<String> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(err) => return vec![format!("{}: {err}", dir.display())],
    };
    let mut errors = Vec::new();
    for path in entries.filter_map(|entry| Some(entry.ok()?.path())) {
        if path.extension().is_none_or(|ext| ext != "json") {
            continue;
        }
        let result = std::fs::read_to_string(&path)
            .map_err(anyhow::Error::from)
            .and_then(|source| ThemeRegistry::global_mut(cx).load_themes_from_str(&source));
        if let Err(err) = result {
            errors.push(format!("{}: {err}", path.display()));
        }
    }
    errors
}

/// Uses the named registered themes for light and dark mode, then re-applies
/// the current mode so the change is visible immediately.
pub fn set_theme_pair(light: &str, dark: &str, cx: &mut App) -> anyhow::Result<()> {
    let light = find(light, cx)?;
    let dark = find(dark, cx)?;
    anyhow::ensure!(
        !light.mode.is_dark(),
        "theme {:?} is not a light theme",
        light.name
    );
    anyhow::ensure!(
        dark.mode.is_dark(),
        "theme {:?} is not a dark theme",
        dark.name
    );
    let theme = Theme::global_mut(cx);
    theme.light_theme = light;
    theme.dark_theme = dark;
    let mode = theme.mode;
    Theme::change(mode, None, cx);
    Ok(())
}

/// Switches to the named theme: it becomes the light or dark theme (by its
/// mode), and the appearance follows it so the choice is visible.
pub fn select_theme(name: &str, window: &mut Window, cx: &mut App) -> anyhow::Result<()> {
    let config = find(name, cx)?;
    let is_dark = config.mode.is_dark();
    let name = config.name.to_string();
    SettingsStore::update(cx, |settings| {
        if is_dark {
            settings.dark_theme = name;
            settings.appearance = Appearance::Dark;
        } else {
            settings.light_theme = name;
            settings.appearance = Appearance::Light;
        }
    });
    let settings = SettingsStore::get(cx).clone();
    set_theme_pair(&settings.light_theme, &settings.dark_theme, cx)?;
    apply_appearance(window, cx);
    Ok(())
}

/// Registered theme names, sorted.
pub fn available(cx: &App) -> Vec<String> {
    let mut themes: Vec<_> = ThemeRegistry::global(cx)
        .themes()
        .keys()
        .map(|name| name.to_string())
        .collect();
    themes.sort();
    themes
}

/// Applies the configured appearance: an explicit choice, or the system's.
pub fn apply_appearance(window: &mut Window, cx: &mut App) {
    match SettingsStore::get(cx).appearance {
        Appearance::System => Theme::sync_system_appearance(Some(window), cx),
        Appearance::Light => Theme::change(ThemeMode::Light, Some(window), cx),
        Appearance::Dark => Theme::change(ThemeMode::Dark, Some(window), cx),
    }
    window.refresh();
}

fn find(name: &str, cx: &App) -> anyhow::Result<Rc<ThemeConfig>> {
    ThemeRegistry::global(cx)
        .themes()
        .get(name)
        .cloned()
        .with_context(|| format!("theme {name:?} is not registered"))
}

#[cfg(test)]
mod tests {
    use gpui::TestAppContext;
    use gpui_kit::component::ActiveTheme as _;

    use super::*;
    use crate::settings::Settings;

    fn setup(settings: Settings, cx: &mut App) {
        gpui_kit::init(cx);
        SettingsStore::init(None, settings, cx);
    }

    #[gpui::test]
    fn bundled_themes_load_and_apply(cx: &mut TestAppContext) {
        cx.update(|cx| {
            setup(Settings::default(), cx);
            assert_eq!(init(None, cx).unwrap(), Vec::<String>::new());

            // The active colors must come from our theme, not the library default.
            Theme::change(ThemeMode::Dark, None, cx);
            let expected: gpui::Hsla = gpui::rgb(0x18181b).into();
            assert_eq!(cx.theme().background, expected);
        });
    }

    #[gpui::test]
    fn user_themes_load_and_bad_files_are_reported(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let mut user_theme: serde_json::Value = serde_json::from_str(BUNDLED_THEMES).unwrap();
        user_theme["themes"][0]["name"] = "Paper".into();
        user_theme["themes"][1]["name"] = "Midnight".into();
        std::fs::write(dir.path().join("mine.json"), user_theme.to_string()).unwrap();
        std::fs::write(dir.path().join("broken.json"), "{").unwrap();
        std::fs::write(dir.path().join("notes.txt"), "ignored").unwrap();

        cx.update(|cx| {
            setup(
                Settings {
                    dark_theme: "Midnight".into(),
                    ..Settings::default()
                },
                cx,
            );
            let errors = init(Some(dir.path()), cx).unwrap();
            assert_eq!(errors.len(), 1, "{errors:?}");
            assert!(errors[0].contains("broken.json"));
            assert_eq!(Theme::global(cx).dark_theme.name, "Midnight");
        });
    }

    #[gpui::test]
    fn configured_theme_with_wrong_mode_falls_back(cx: &mut TestAppContext) {
        cx.update(|cx| {
            setup(
                Settings {
                    light_theme: DEFAULT_DARK.into(),
                    ..Settings::default()
                },
                cx,
            );
            let errors = init(None, cx).unwrap();
            assert_eq!(errors.len(), 1);
            assert_eq!(Theme::global(cx).light_theme.name, DEFAULT_LIGHT);
        });
    }
}

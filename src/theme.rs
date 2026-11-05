//! Theme setup: the bundled Air themes, applied on top of gpui-component's
//! theme system so every component picks them up.

use std::rc::Rc;

use anyhow::Context as _;
use gpui_kit::App;
use gpui_kit::component::{Theme, ThemeConfig, ThemeRegistry};

const BUNDLED_THEMES: &str = include_str!("../assets/themes/air.json");

pub const DEFAULT_LIGHT: &str = "Air Light";
pub const DEFAULT_DARK: &str = "Air Dark";

/// Loads the bundled themes and makes them the active light/dark pair.
/// Must run after `gpui_kit::init`.
pub fn init(cx: &mut App) -> anyhow::Result<()> {
    ThemeRegistry::global_mut(cx)
        .load_themes_from_str(BUNDLED_THEMES)
        .context("bundled theme file is invalid")?;
    set_theme_pair(DEFAULT_LIGHT, DEFAULT_DARK, cx)
}

/// Uses the named registered themes for light and dark mode, then re-applies
/// the current mode so the change is visible immediately.
pub fn set_theme_pair(light: &str, dark: &str, cx: &mut App) -> anyhow::Result<()> {
    let light = find(light, cx)?;
    let dark = find(dark, cx)?;
    let theme = Theme::global_mut(cx);
    theme.light_theme = light;
    theme.dark_theme = dark;
    let mode = theme.mode;
    Theme::change(mode, None, cx);
    Ok(())
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

    #[gpui::test]
    fn bundled_themes_load_and_apply(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            init(cx).unwrap();

            for (mode, name) in [("light", DEFAULT_LIGHT), ("dark", DEFAULT_DARK)] {
                let config = find(name, cx).unwrap();
                assert_eq!(
                    config.mode.is_dark(),
                    mode == "dark",
                    "{name} has wrong mode"
                );
            }

            // The active colors must come from our theme, not the library default.
            Theme::change(gpui_kit::component::ThemeMode::Dark, None, cx);
            let background = cx.theme().background;
            let expected: gpui_kit::Hsla = gpui_kit::rgb(0x18181b).into();
            assert_eq!(background, expected);
        });
    }
}

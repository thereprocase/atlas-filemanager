//! Theme application glue. The active profile stores a theme *name*; here we map
//! it onto gpui-component's global `Theme` via the `ThemeRegistry`. Applying needs
//! `&mut gpui::App` + a `Window`, which `process_actions` doesn't have, so callers
//! (palette, settings, profile switch) just set the name and flip `theme_dirty`;
//! the next render calls [`App::apply_active_theme`].

use crate::app::App;
use atlas_model::profiles::DEFAULT_THEME;
use gpui::{App as GpuiApp, SharedString, Window, WindowBackgroundAppearance};
use gpui_component::scroll::ScrollbarShow;
use gpui_component::{Theme, ThemeColor, ThemeMode, ThemeRegistry};

/// Retired presets stay out of every picker, including copies in custom theme files.
pub(crate) fn theme_is_available(name: &str) -> bool {
    !matches!(
        name,
        "Default Light"
            | "Default Dark"
            | "Atlas Frost"
            | "Atlas Frost Glass"
            | "Atlas Figma Glass"
            | "Atlas Glass"
    )
}

fn migrated_theme_name(name: &str) -> &str {
    match name {
        "Default Dark" => "Atlas Graphite Glass",
        name if !theme_is_available(name) => DEFAULT_THEME,
        name => name,
    }
}

fn persisted_theme_name<'a>(stored: &str, wanted: &'a str, resolved: &'a str, custom_themes_loaded: bool) -> Option<&'a str> {
    // Migrations are authoritative immediately. Unknown custom themes are only
    // normalized after the asynchronous user-theme directory has been loaded.
    let permanent = custom_themes_loaded || wanted == resolved || wanted != stored;
    (permanent && resolved != stored).then_some(resolved)
}

/// Assign alpha once, after loading a pristine palette. Multiplying the JSON's
/// existing alpha makes materials vary by preset and compounds on each update.
fn material_color(mut color: gpui::Hsla, alpha: f32) -> gpui::Hsla {
    color.alpha = alpha;
    color
}

fn apply_frosted_material(colors: &mut ThemeColor, mode: ThemeMode, opacity: f32) {
    let opacity = if opacity.is_finite() {
        opacity.clamp(0.20, 1.0)
    } else {
        atlas_model::settings::default_glass_opacity()
    };
    // One backing tint; panes are the main material. Internal strips and controls
    // use small overlay weights so nested backgrounds don't wash out the frost.
    colors.background = material_color(colors.background, opacity * 0.18);
    let content_opacity = if mode.is_dark() {
        opacity
    } else {
        opacity.max(0.80)
    };
    colors.list = material_color(colors.list, content_opacity);
    colors.table = material_color(colors.table, content_opacity);
    colors.sidebar = material_color(
        colors.sidebar,
        if mode.is_dark() {
            opacity * 0.85
        } else {
            content_opacity
        },
    );
    colors.title_bar = material_color(colors.title_bar, opacity * 0.18);
    colors.tab_bar = material_color(colors.tab_bar, opacity * 0.12);
    colors.tab_active = material_color(colors.tab_active, opacity * 0.32);
    colors.tab_bar_segmented = material_color(colors.tab_bar_segmented, opacity * 0.08);
    colors.list_head = material_color(colors.list_head, opacity * 0.14);
    colors.table_head = material_color(colors.table_head, opacity * 0.14);
    colors.list_even = material_color(colors.list_even, opacity * 0.035);
    colors.table_even = material_color(colors.table_even, opacity * 0.035);
    colors.secondary = material_color(colors.secondary, opacity * 0.20);
    colors.secondary_hover = material_color(colors.secondary_hover, opacity * 0.28);
    colors.secondary_active = material_color(colors.secondary_active, opacity * 0.36);
    colors.muted = material_color(colors.muted, opacity * 0.18);
    // State overlays have their own contrast, independent of material transparency.
    colors.list_hover = material_color(colors.foreground, 0.07);
    colors.table_hover = colors.list_hover;
    // Menus/dialogs need a dense surface even when the workspace is very airy.
    colors.popover = material_color(colors.popover, 0.90 + opacity * 0.04);
    // Functional separators remain; outer chrome has no decorative rim.
    colors.border = material_color(colors.foreground, 0.10);
    colors.input = material_color(colors.foreground, 0.16);
    colors.sidebar_border = gpui::transparent_black();
    colors.title_bar_border = gpui::transparent_black();
    colors.window_border = gpui::transparent_black();
}

impl App {
    /// Suffix that marks a theme as the translucent "Glass" variant of a base
    /// theme. Paired by name: `"Atlas Midnight"` ↔ `"Atlas Midnight Glass"`.
    const GLASS_SUFFIX: &'static str = " Glass";

    /// Record `name` as the active profile's theme, persist, and request that the
    /// next render apply it. Cheap and cx-free so it can run inside `process_actions`.
    pub(crate) fn set_theme(&mut self, name: String) {
        self.theme_preview_before = None;
        self.state.profiles.active_mut().theme = name;
        self.theme_dirty = true;
        self.save();
    }

    pub(crate) fn preview_theme(&mut self, name: String) {
        if self.theme_preview_before.is_none() {
            self.theme_preview_before = Some(self.state.profiles.active().theme.clone());
        }
        if self.state.profiles.active().theme != name {
            self.state.profiles.active_mut().theme = name;
            self.theme_dirty = true;
        }
    }

    pub(crate) fn restore_theme_preview(&mut self) {
        let Some(name) = self.theme_preview_before.take() else {
            return;
        };
        if self.state.profiles.active().theme != name {
            self.state.profiles.active_mut().theme = name;
            self.theme_dirty = true;
        }
    }

    /// Apply the active profile's theme to the global gpui-component `Theme`.
    /// Falls back to [`DEFAULT_THEME`] when the stored name isn't registered (e.g.
    /// legacy enum values, or a custom theme file that was removed), rewriting the
    /// profile so the UI reflects what's actually shown.
    pub(crate) fn apply_active_theme(&mut self, window: &mut Window, cx: &mut GpuiApp) {
        let wanted = migrated_theme_name(&self.state.profiles.active().theme).to_string();
        let registry = ThemeRegistry::global(cx);
        let custom_themes_loaded = registry.custom_themes_loaded();

        // Try to look up the (possibly redirected) name, falling back to DEFAULT_THEME
        // when the stored name isn't registered (legacy enum values, removed theme, …).
        let (resolved_name, config) =
            if let Some(cfg) = registry.themes().get(&SharedString::from(wanted.as_str())) {
                (wanted.clone(), cfg.clone())
            } else if let Some(cfg) = registry.themes().get(&SharedString::from(DEFAULT_THEME)) {
                (DEFAULT_THEME.to_string(), cfg.clone())
            } else {
                // No themes registered at all — nothing to apply.
                return;
            };

        let gridline = crate::ui::gridline::is_theme(&resolved_name);
        crate::ui::gridline::set_active(gridline);
        let wants_backdrop = resolved_name.ends_with(Self::GLASS_SUFFIX);

        // Enable the OS backdrop for explicit glass themes. This is Atlas'
        // equivalent of Zed's `background.appearance=blurred`.
        window.set_background_appearance(if wants_backdrop {
            WindowBackgroundAppearance::Blurred
        } else {
            WindowBackgroundAppearance::Opaque
        });

        // Persist the normalized name so the migration is one-time and sticky.
        if let Some(name) = persisted_theme_name(&self.state.profiles.active().theme, &wanted, &resolved_name, custom_themes_loaded) {
            self.state.profiles.active_mut().theme = name.to_string();
            self.save();
        }

        Theme::global_mut(cx).apply_config(&config);
        if wants_backdrop {
            apply_frosted_material(
                &mut Theme::global_mut(cx).colors,
                config.mode,
                self.state.settings.glass_opacity,
            );
        }
        // Re-apply zoom so Root's per-frame set_rem_size(theme.font_size) picks up
        // the user's scale instead of the theme default (px(16.)).
        Theme::global_mut(cx).font_size = gpui::px(if gridline { 18.0 } else { 16.0 } * self.state.settings.global_zoom);
        // Always show scrollbar on hover (not only while scrolling) and boost thumb
        // opacity so it's actually visible against glass backgrounds.
        {
            let t = Theme::global_mut(cx);
            t.scrollbar_show = ScrollbarShow::Hover;
            // The text caret follows the foreground: it must read on every theme
            // (the default falls back to the accent, which can vanish on a tinted row).
            t.caret = t.foreground;
            t.scrollbar = gpui::transparent_black();
            let base = t.scrollbar_thumb;
            t.scrollbar_thumb = gpui::hsla(
                (base.hue.into_positive_degrees() / 360.0),
                base.saturation,
                base.lightness,
                0.45,
            );
            let hover_base = t.scrollbar_thumb_hover;
            t.scrollbar_thumb_hover = gpui::hsla(
                (hover_base.hue.into_positive_degrees() / 360.0),
                hover_base.saturation,
                hover_base.lightness,
                0.75,
            );
        }
        window.refresh();
    }
}

#[cfg(test)]
mod gpui_ce_tests {
    use super::{apply_frosted_material, migrated_theme_name, theme_is_available};
    use gpui_component::{Colorize, ThemeSet};

    #[test]
    fn delayed_custom_theme_loading_does_not_overwrite_the_saved_selection() {
        use super::persisted_theme_name;
        let custom = "Atlas Graphite Copy Glass";
        let fallback = atlas_model::profiles::DEFAULT_THEME;
        assert_eq!(persisted_theme_name(custom, custom, fallback, false), None);
        assert_eq!(persisted_theme_name(custom, custom, custom, true), None);
        assert_eq!(persisted_theme_name(custom, custom, fallback, true), Some(fallback));
        assert_eq!(persisted_theme_name("Default Dark", "Atlas Graphite Glass", "Atlas Graphite Glass", false), Some("Atlas Graphite Glass"));
    }

    #[test]
    fn retired_presets_are_hidden_and_saved_names_migrate() {
        let themes: ThemeSet =
            serde_json::from_str(include_str!("../../assets/themes/atlas-themes.json")).unwrap();
        assert!(themes
            .themes
            .iter()
            .all(|t| theme_is_available(t.name.as_ref())));
        assert_eq!(themes.themes.iter().filter(|t| t.is_default).count(), 1);
        for old_name in [
            "Default Light",
            "Default Dark",
            "Atlas Frost",
            "Atlas Frost Glass",
            "Atlas Figma Glass",
            "Atlas Glass",
        ] {
            assert!(!theme_is_available(old_name));
            let replacement = migrated_theme_name(old_name);
            assert!(themes.themes.iter().any(|t| t.name.as_ref() == replacement));
        }
        assert_eq!(migrated_theme_name("Default Dark"), "Atlas Graphite Glass");
        assert_eq!(migrated_theme_name("Custom Glass"), "Custom Glass");
    }

    #[test]
    fn every_glass_palette_updates_without_compounding_or_tinting_text() {
        let themes: ThemeSet =
            serde_json::from_str(include_str!("../../assets/themes/atlas-themes.json")).unwrap();
        for config in themes
            .themes
            .into_iter()
            .filter(|t| t.name.ends_with(" Glass"))
        {
            let config = std::rc::Rc::new(config);
            let mut theme = gpui_component::Theme::default();
            theme.apply_config(&config);
            let palette = theme.colors;
            apply_frosted_material(&mut theme.colors, config.mode, 0.8);
            let initial = serde_json::to_value(theme.colors).unwrap();
            apply_frosted_material(&mut theme.colors, config.mode, 0.4);
            if config.mode.is_dark() {
                assert!(theme.colors.list.alpha < 0.8, "{}", config.name);
                assert!(theme.colors.sidebar.alpha < theme.colors.list.alpha);
            } else {
                assert_eq!(theme.colors.list.alpha, 0.80, "{}", config.name);
                assert_eq!(theme.colors.sidebar.alpha, theme.colors.list.alpha);
            }
            assert!(theme.colors.tab_bar.alpha < theme.colors.sidebar.alpha);
            assert!(theme.colors.popover.alpha > 0.9);
            assert!(
                crate::ui::glass::layer_color(theme.colors.secondary, 0.04, 0.92).alpha
                    < theme.colors.list.alpha
            );
            assert_eq!(theme.colors.foreground, palette.foreground);
            assert_eq!(theme.colors.accent, palette.accent);
            assert_eq!(theme.colors.selection, palette.selection);
            assert_eq!(theme.colors.list.hue, palette.list.hue);
            assert_eq!(theme.colors.list.lightness, palette.list.lightness);
            assert_eq!(theme.colors.window_border.alpha, 0.0);
            assert_eq!(theme.colors.sidebar_border.alpha, 0.0);
            apply_frosted_material(&mut theme.colors, config.mode, 0.8);
            assert_eq!(serde_json::to_value(theme.colors).unwrap(), initial);
            // Reapplying a stored preset (close/reopen settings or profile switch)
            // must keep the chosen material; switching to solid restores its palette.
            theme.apply_config(&config);
            apply_frosted_material(&mut theme.colors, config.mode, 0.8);
            assert_eq!(serde_json::to_value(theme.colors).unwrap(), initial);
            theme.apply_config(&config);
            assert_eq!(
                serde_json::to_value(theme.colors).unwrap(),
                serde_json::to_value(palette).unwrap()
            );
        }
    }

    #[test]
    fn bundled_themes_keep_hex_colors_with_gpui_ce() {
        for content in [
            include_str!("../../assets/themes/atlas-themes.json"),
            include_str!(
                "../../../../vendor/gpui-component-fork/crates/ui/src/theme/default-theme.json"
            ),
        ] {
            let themes: ThemeSet =
                serde_json::from_str(content).expect("existing themes must load");
            assert!(!themes.themes.is_empty());
            let serialized = serde_json::to_string(&themes).expect("theme serialization");
            let restored: ThemeSet = serde_json::from_str(&serialized).expect("theme round trip");
            assert_eq!(restored.themes.len(), themes.themes.len());
            for (before, after) in themes.themes.iter().zip(restored.themes.iter()) {
                assert_eq!(before.name, after.name);
            }
        }
    }

    #[test]
    fn floating_controls_stay_readable_at_every_glass_density() {
        let themes: ThemeSet =
            serde_json::from_str(include_str!("../../assets/themes/atlas-themes.json")).unwrap();
        for config in themes
            .themes
            .into_iter()
            .filter(|t| t.name.ends_with(" Glass"))
        {
            let config = std::rc::Rc::new(config);
            for opacity in [0.2, 0.4, 0.8, 1.0, f32::NAN] {
                let mut theme = gpui_component::Theme::default();
                theme.apply_config(&config);
                let original = theme.colors;
                apply_frosted_material(&mut theme.colors, config.mode, opacity);
                let controls = crate::ui::glass::control_colors(&theme, true);
                let (min_alpha, max_alpha) = if config.mode.is_dark() {
                    (0.54, 0.72)
                } else {
                    (0.62, 0.78)
                };
                assert!(
                    (min_alpha..=max_alpha).contains(&controls.surface.alpha),
                    "{}",
                    config.name
                );
                assert_eq!(
                    crate::ui::glass::control_colors(&theme, false)
                        .surface
                        .alpha,
                    1.0
                );
                assert_eq!(controls.foreground, original.foreground);
                assert_eq!(controls.muted, original.muted_foreground);
                assert!(controls.hover.alpha > theme.colors.border.alpha * 0.42);
                assert!(controls.pressed.alpha > controls.hover.alpha);
                if !config.mode.is_dark() {
                    assert!(theme.colors.list.alpha >= 0.80);
                    assert!(theme.colors.sidebar.alpha >= 0.80);
                }
            }
        }
    }

    #[test]
    fn light_text_stays_readable_on_content_sidebar_and_hover() {
        fn composite(color: gpui::Rgba, base: gpui::Rgba) -> gpui::Rgba {
            gpui::Rgba::new(
                color.red * color.alpha + base.red * (1.0 - color.alpha),
                color.green * color.alpha + base.green * (1.0 - color.alpha),
                color.blue * color.alpha + base.blue * (1.0 - color.alpha),
                1.0,
            )
        }
        fn luminance(color: gpui::Rgba) -> f32 {
            let linear = |v: f32| {
                if v <= 0.04045 {
                    v / 12.92
                } else {
                    ((v + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * linear(color.red) + 0.7152 * linear(color.green) + 0.0722 * linear(color.blue)
        }
        let themes: ThemeSet =
            serde_json::from_str(include_str!("../../assets/themes/atlas-themes.json")).unwrap();
        for config in themes.themes.into_iter().filter(|t| !t.mode.is_dark()) {
            let glass = config.name.ends_with(" Glass");
            let config = std::rc::Rc::new(config);
            let mut theme = gpui_component::Theme::default();
            theme.apply_config(&config);
            if glass {
                apply_frosted_material(&mut theme.colors, config.mode, 0.2);
            }
            let base = if glass {
                gpui::black().to_rgb()
            } else {
                theme.background.to_rgb()
            };
            let controls = crate::ui::glass::control_colors(&theme, glass);
            for surface in [theme.colors.list, theme.sidebar] {
                let surface = composite(surface.to_rgb(), base);
                let hovered = composite(
                    crate::ui::glass::control_hover(theme.foreground).to_rgb(),
                    surface,
                );
                let control_surface = composite(controls.surface.to_rgb(), surface);
                let control_hovered = composite(controls.hover.to_rgb(), control_surface);
                for background in [surface, hovered, control_surface, control_hovered] {
                    for text in [theme.foreground, theme.muted_foreground] {
                        let text = composite(text.to_rgb(), background);
                        let ratio = (luminance(background) + 0.05) / (luminance(text) + 0.05);
                        assert!(ratio >= 4.5, "{}: text contrast {ratio}", config.name);
                    }
                }
            }
        }
    }

    #[test]
    fn palette_colors_preserve_hex_channels_and_alpha() {
        for hex in [
            "#F8FAFC",
            "#FEF2F2",
            "#0413FCAA",
            "#21C55E",
            "#FF0000",
            "#00000000",
        ] {
            let color = gpui::Hsla::parse_hex(hex).unwrap();
            assert_eq!(color.to_hex(), hex);
        }
        let config: gpui_component::highlighter::HighlightThemeStyle =
            serde_json::from_str(r##"{"syntax":{"comment":{"color":"#0413FCAA"}}}"##)
                .expect("hex syntax color");
        let value = serde_json::to_value(config).unwrap();
        assert_eq!(value["syntax"]["comment"]["color"], "#0413FCAA");
    }
}

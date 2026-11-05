//! Combined asset source: our embedded Phosphor icons (served under `icons/ph/…`)
//! plus everything `gpui-component` bundles (its own `IconName` SVGs, fonts, …).
//!
//! gpui renders `svg()` as an alpha mask tinted by the current `text_color`, and the
//! Phosphor SVGs use `fill="currentColor"`, so a single `svg().path("icons/ph/…")`
//! picks up whatever color we set on it.

use gpui::{AssetSource, Result, SharedString};
use std::borrow::Cow;

/// App asset source. Phosphor icons are embedded here; anything else delegates to the
/// `gpui-component` asset bundle so its built-in `IconName` set keeps working.
pub struct AtlasAssets;

pub(crate) const VAULT_FOLDER_ART: &str = "illustrations/vault-folder-lock.png";

pub fn bundled_fonts() -> Vec<Cow<'static, [u8]>> {
    vec![
        Cow::Borrowed(include_bytes!("../assets/fonts/plex/IBMPlexSans-Regular.ttf")),
        Cow::Borrowed(include_bytes!("../assets/fonts/plex/IBMPlexSans-Bold.ttf")),
        Cow::Borrowed(include_bytes!("../assets/fonts/plex/IBMPlexMono-Regular.ttf")),
        Cow::Borrowed(include_bytes!("../assets/fonts/plex/IBMPlexMono-SemiBold.ttf")),

        Cow::Borrowed(include_bytes!("../assets/fonts/geist/Geist-Variable.ttf")),
        Cow::Borrowed(include_bytes!(
            "../assets/fonts/geist/GeistMono-Variable.ttf"
        )),
    ]
}

macro_rules! phosphor_icons {
    ($($name:literal),* $(,)?) => {
        const PHOSPHOR_PATHS: &[&str] = &[$(concat!("icons/ph/", $name, ".svg")),*];
        /// Resolve an `icons/ph/<name>.svg` path to its embedded bytes.
        fn phosphor(path: &str) -> Option<&'static [u8]> {
            match path {
                $(
                    concat!("icons/ph/", $name, ".svg") => Some(include_bytes!(
                        concat!("../assets/icons/ph/", $name, ".svg")
                    )),
                )*
                _ => None,
            }
        }
    };
}

macro_rules! liquid_glass_filetype_icons {
    ($($name:literal),* $(,)?) => {
        const LIQUID_GLASS_PATHS: &[&str] = &[$(concat!("icons/filetypes/liquid-glass/", $name, ".png")),*];
        /// Resolve an `icons/filetypes/liquid-glass/<name>.png` path to embedded bytes.
        pub(crate) fn liquid_glass_filetype(path: &str) -> Option<&'static [u8]> {
            match path {
                $(
                    concat!("icons/filetypes/liquid-glass/", $name, ".png") => Some(include_bytes!(
                        concat!("../assets/icons/filetypes/liquid-glass/", $name, ".png")
                    )),
                )*
                _ => None,
            }
        }
    };
}

fn app_icon(path: &str) -> Option<&'static [u8]> {
    match path {
        VAULT_FOLDER_ART => Some(include_bytes!("../assets/illustrations/vault-folder-lock.png")),
        "icons/atlas-logo.png" => Some(include_bytes!("../assets/icons/atlas-logo.png")),
        "icons/atlas-app-icon.png" => Some(include_bytes!("../assets/icons/atlas-app-icon.png")),
        _ => None,
    }
}

phosphor_icons!(
    "funnel",
    "house",
    "monitor",
    "file",
    "file-text",
    "download-simple",
    "image",
    "music-notes",
    "film-strip",
    "folder",
    "lock-simple",
    "cloud",
    "tag",
    "clock-counter-clockwise",
    "hard-drives",
    "hard-drive",
    "hard-drive-system",
    "usb",
    "bookmark-simple",
    "caret-down",
    "caret-right",
    "magnifying-glass",
    "trash",
    "globe-simple",
    "discord-logo",
    "lifebuoy",
    "terminal-window",
    "grid-four",
    "list-bullets",
    "list-dashes",
    "columns",
    "copy-simple",
    "scissors",
    "clipboard",
    // Palette engine: section/command icons + per-item buttons.
    "star",
    "star-fill",
    "plus",
    "lightning",
    "gear",
    "palette",
    "user-switch",
    "puzzle-piece",
    "arrows-left-right",
    "split-horizontal",
    "split-vertical",
    "sidebar-simple",
    "eye",
    "pencil-simple",
    "arrow-counter-clockwise",
    "arrow-clockwise",
    "selection-all",
    "selection-inverse",
    "sort-ascending",
    "tree-structure",
    "git-branch",
    "x",
    "minus",
    "square",
    "arrow-left",
    "arrow-right",
    "arrow-up",
    "arrow-down",
    "tray-arrow-down",
);

liquid_glass_filetype_icons!(
    "archive",
    "audio",
    "code",
    "document",
    "file",
    "folder",
    "image",
    "pdf",
    "presentation",
    "python",
    "video",
);

impl AssetSource for AtlasAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if let Some(bytes) = phosphor(path) {
            return Ok(Some(Cow::Borrowed(bytes)));
        }
        if let Some(bytes) = app_icon(path) {
            return Ok(Some(Cow::Borrowed(bytes)));
        }
        if let Some(bytes) = liquid_glass_filetype(path) {
            return Ok(Some(Cow::Borrowed(bytes)));
        }
        gpui_component_assets::Assets.load(path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        // CE constructs its on-demand registry from list(""). Every Atlas
        // asset must be enumerated as well as resolvable through load().
        let mut paths = gpui_component_assets::Assets.list(path)?;
        paths.extend(
            PHOSPHOR_PATHS
                .iter()
                .chain(LIQUID_GLASS_PATHS.iter())
                .chain(["icons/atlas-logo.png", "icons/atlas-app-icon.png", VAULT_FOLDER_ART].iter())
                .filter(|name| name.starts_with(path))
                .map(|name| SharedString::from(*name)),
        );
        paths.sort_unstable();
        paths.dedup();
        Ok(paths)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ce_registry_includes_every_embedded_atlas_resource() {
        let registry = gpui::AssetRegistry::from(AtlasAssets);
        for path in PHOSPHOR_PATHS
            .iter()
            .chain(LIQUID_GLASS_PATHS.iter())
            .chain(["icons/atlas-logo.png", "icons/atlas-app-icon.png", VAULT_FOLDER_ART].iter())
        {
            let bytes = registry
                .load(path)
                .unwrap_or_else(|| panic!("missing CE asset {path}"));
            assert!(!bytes.is_empty(), "empty asset {path}");
        }
        let listed = AtlasAssets.list("icons/ph/").unwrap();
        assert_eq!(listed.len(), PHOSPHOR_PATHS.len());
        assert!(listed.iter().all(|name| name.starts_with("icons/ph/")));
    }
}

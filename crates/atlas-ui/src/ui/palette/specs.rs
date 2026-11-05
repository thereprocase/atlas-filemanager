//! Per-palette `PaletteSpec` builders.

use super::{
    FooterHint, FooterSpec, ItemButton, PaletteItem, PaletteKind, PaletteSection, PaletteSpec,
    ShortcutHint,
};
use crate::app::App;
use crate::input::action::Action;
use crate::ui::icons::AtlasIcon;
use atlas_core::search::navigation::{filename_match_quality, match_path_tokens, path_noise};
use atlas_model::pane::PaneId;
use atlas_model::settings::ViewMode;
use gpui::prelude::*;
use gpui::{div, px, AnyElement, Context, MouseButton};
use gpui_component::ActiveTheme;
use std::path::PathBuf;

/// How many local matches to include in each query's snapshot.
const GOTO_LIMIT: usize = 100;
/// Per-section cap for goto results (Carpetas / Archivos).
const GOTO_SECTION_LIMIT: usize = 25;
/// Per-category cap for the goto's empty-query home sections.
const GOTO_HOME_LIMIT: usize = 8;

/// Results are published together once matching sources have answered. Keeping
/// the ranked entries also freezes category order and keyboard row positions.
#[derive(Default)]
pub(crate) struct GotoSnapshot {
    key: Option<(String, PathBuf, bool, bool)>,
    results: Option<(Vec<atlas_core::search::nucleo_pool::IndexEntry>, bool)>,
}

impl GotoSnapshot {
    fn prepare(&mut self, query: &str, root: &std::path::Path, scoped: bool, show_hidden: bool) {
        let key = (query.to_owned(), root.to_path_buf(), scoped, show_hidden);
        if self.key.as_ref() != Some(&key) {
            self.key = Some(key);
            self.results = None;
        }
    }

    pub(crate) fn is_pending(&self) -> bool {
        self.results.is_none()
    }

    fn publish(
        &mut self,
        entries: Vec<atlas_core::search::nucleo_pool::IndexEntry>,
        files_first: bool,
    ) {
        if self.is_pending() {
            self.results = Some((entries, files_first));
        }
    }
}

/// One command-palette entry: a stable id (for pinning), display strings, and
/// both the runtime `Action` to run and the gpui action whose live keybinding
/// is shown as the shortcut badge.
struct CommandDef {
    id: String,
    title: String,
    description: std::borrow::Cow<'static, str>,
    icon: AtlasIcon,
    action: Action,
    aliases: Vec<String>,
}

impl CommandDef {
    fn new(
        id: &str,
        title: impl Into<String>,
        description: impl Into<std::borrow::Cow<'static, str>>,
        icon: AtlasIcon,
        action: Action,
    ) -> Self {
        let raw_title = title.into();
        let (title, keywords) = raw_title.split_once('^').unwrap_or((&raw_title, ""));
        Self {
            id: id.to_string(),
            title: title.trim().to_owned(),
            description: description.into(),
            icon,
            action,
            aliases: keywords.split(',').map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned).collect(),
        }
    }

    fn with_aliases(mut self, aliases: &[&'static str]) -> Self {
        self.aliases.extend(aliases.iter().map(|alias| (*alias).to_owned()));
        self
    }

    /// Convert into a palette row: star button pins/unpins, badge shows the
    /// live keybinding for the associated gpui action.
    fn item(
        &self,
        pinned: bool,
        shortcut: Option<gpui::Keystroke>,
        recording: bool,
    ) -> PaletteItem {
        let mut item = PaletteItem::new(self.title.clone(), self.action.clone());
        item.icon = Some(self.icon);
        if !self.description.is_empty() {
            item.description = Some(self.description.clone().into());
        }
        if crate::input::commands::command_remappable(&self.id) {
            item.shortcuts.push(ShortcutHint::CommandShortcut {
                command_id: self.id.clone(),
                key: shortcut,
                recording,
            });
        }
        item.buttons.push(ItemButton {
            id: "pin",
            icon: if pinned {
                AtlasIcon::StarFill
            } else {
                AtlasIcon::Star
            },
            tooltip: if pinned {
                rust_i18n::t!("palette_unpin_quick").into()
            } else {
                rust_i18n::t!("palette_pin_quick").into()
            },
            active: pinned,
            action: Action::ToggleCommandPin(self.id.clone()),
            closes_palette: false,
        });
        item
    }
}

impl App {
    pub(crate) fn pinned_header_commands(&self, cx: &gpui::App) -> Vec<(String, String, AtlasIcon)> {
        if self.state.profiles.active().pinned_commands.is_empty() { return Vec::new(); }
        let groups = self.command_groups(cx);
        self.state.profiles.active().pinned_commands.iter().filter_map(|id| {
            groups.iter().flat_map(|(_, _, _, defs)| defs).find(|def| &def.id == id)
                .map(|def| (def.id.clone(), def.title.clone(), def.icon))
        }).collect()
    }

    pub(crate) fn run_pinned_header_command(&mut self, pane: atlas_model::pane::TabId, id: &str, cx: &gpui::App) {
        self.activate_tab(pane);
        // Resolve after focus changes so pane IDs, selections and paths belong
        // to the clicked panel, including context-sensitive shell operations.
        let action = self.command_groups(cx).into_iter().flat_map(|(_, _, _, defs)| defs)
            .find(|def| def.id == id).map(|def| def.action);
        if let Some(action) = action { self.process_actions(vec![action]); }
    }
}

#[cfg(test)]
mod inline_keyword_tests {
    use super::*;

    #[test]
    fn inline_keywords_are_searchable_but_never_displayed() {
        let command = CommandDef::new("create", "Crear Carpeta^nueva, directorio,,mkdir", "", AtlasIcon::Plus, Action::CreateFolderFocused)
            .with_aliases(&["folder"]);
        assert_eq!(command.title, "Crear Carpeta");
        assert_eq!(command.aliases, ["nueva", "directorio", "mkdir", "folder"]);
        assert_eq!(command.item(false, None, false).title.as_ref(), "Crear Carpeta");
    }
}

/// A goto row for `path`: navigate on Enter, bookmark/new-tab side buttons,
/// description = parent location. Directories (`expandable`) drill in with Tab: the
/// query becomes "<path><sep>" and the fuzzy scope moves into that folder.
fn goto_entry(
    title: String,
    path: PathBuf,
    icon: AtlasIcon,
    expandable: bool,
    tab: atlas_model::pane::TabId,
    pane: PaneId,
) -> PaletteItem {
    let action = if expandable {
        Action::Navigate {
            tab,
            path: path.clone(),
        }
    } else {
        Action::Open(vec![path.clone()])
    };
    let mut item = PaletteItem::new(title.clone(), action);
    item.icon = Some(icon);
    item.description = Some(goto_location_label(&path, dirs::home_dir().as_deref()).into());
    item.inline_description = true;
    if expandable {
        item.expand = Some(format!("{}{}", path.display(), std::path::MAIN_SEPARATOR).into());
    }
    item.file_icon = Some(App::file_icon_ref_for_path(path.clone(), expandable));
    item.buttons.push(ItemButton {
        id: "bookmark",
        icon: AtlasIcon::Star,
        tooltip: rust_i18n::t!("palette_add_bookmark").into(),
        active: false,
        action: Action::AddBookmark {
            name: title,
            path: path.clone(),
        },
        closes_palette: false,
    });
    if expandable {
        item.buttons.push(ItemButton {
            id: "new-tab",
            icon: AtlasIcon::Plus,
            tooltip: rust_i18n::t!("palette_open_new_tab").into(),
            active: false,
            action: Action::NewTabWithPath {
                pane,
                path: path.clone(),
            },
            closes_palette: true,
        });
        item.buttons.push(ItemButton {
            id: "split-right",
            icon: AtlasIcon::SplitHorizontal,
            tooltip: rust_i18n::t!("palette_open_right_split").into(),
            active: false,
            action: Action::OpenDirInRightSplit {
                pane,
                path: path.clone(),
            },
            closes_palette: true,
        });
        item.buttons.push(ItemButton {
            id: "split-bottom",
            icon: AtlasIcon::SplitVertical,
            tooltip: rust_i18n::t!("palette_open_bottom_split").into(),
            active: false,
            action: Action::OpenDirInBottomSplit {
                pane,
                path: path.clone(),
            },
            closes_palette: true,
        });
    }
    item.buttons.push(ItemButton {
        id: "new-window",
        icon: AtlasIcon::Folder,
        tooltip: rust_i18n::t!("palette_open_new_window").into(),
        active: false,
        action: Action::OpenPathInNewWindow(path),
        closes_palette: true,
    });
    item
}

/// `goto_entry` with the file name as the title and a heuristic icon (an
/// extension usually means a file; no fs metadata on the render path).
fn goto_item(path: PathBuf, tab: atlas_model::pane::TabId, pane: PaneId) -> PaletteItem {
    let name = path.file_name().map_or_else(
        || path.to_string_lossy().into_owned(),
        |n| n.to_string_lossy().into_owned(),
    );
    let is_dir_guess = path.is_dir() || atlas_model::location::is_unc_host(&path.to_string_lossy());
    let icon = if is_dir_guess {
        AtlasIcon::Folder
    } else {
        AtlasIcon::File
    };
    goto_entry(name, path, icon, is_dir_guess, tab, pane)
}

/// `goto_entry` for an indexed result, where `is_dir` is known from the walk.
fn goto_index_item(
    entry: atlas_core::search::nucleo_pool::IndexEntry,
    tab: atlas_model::pane::TabId,
    pane: PaneId,
) -> PaletteItem {
    let name = entry.path.file_name().map_or_else(
        || entry.path.to_string_lossy().into_owned(),
        |n| n.to_string_lossy().into_owned(),
    );
    let icon = if entry.is_dir {
        AtlasIcon::Folder
    } else {
        AtlasIcon::File
    };
    goto_entry(name, entry.path, icon, entry.is_dir, tab, pane)
}

/// The folder pill shown left of the input while the search is scoped to a
/// drilled-into folder.
fn goto_scope_pill(base: &std::path::Path, cx: &Context<App>) -> gpui::AnyElement {
    let name = base.file_name().map_or_else(
        || base.to_string_lossy().into_owned(),
        |n| n.to_string_lossy().into_owned(),
    );
    let (muted, border) = {
        let t = cx.theme();
        (t.muted_foreground, t.border)
    };
    gpui::prelude::IntoElement::into_any_element(
        gpui_component::h_flex()
            .flex_shrink_0()
            .gap_1()
            .px_2()
            .py(px(2.))
            .rounded_md()
            .border_1()
            .border_color(border)
            .text_xs()
            .text_color(muted)
            .child(crate::ui::icons::icon(AtlasIcon::Folder, 12.0, muted))
            .child(name),
    )
}

/// If the query starts with an absolute path, split it at the last separator
/// into (base dir, needle): "C:\dev\atl" → ("C:\dev\", "atl"). The fuzzy
/// index is then scoped to the base dir.
fn split_query_base(query: &str) -> Option<(PathBuf, String)> {
    let q = query.trim_start();
    let expanded = dirs::home_dir().and_then(|home| expand_home_query(q, &home));
    let q = expanded.as_deref().unwrap_or(q);
    let bytes = q.as_bytes();
    let absolute = q.starts_with('/')
        || q.starts_with("\\\\")
        || (bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && (bytes[2] == b'\\' || bytes[2] == b'/'));
    if !absolute {
        return None;
    }
    let pos = q.rfind(['/', '\\'])?;
    let (base, rest) = q.split_at(pos + 1);
    let base_trimmed = if base.len() > 1 && (base.ends_with('/') || base.ends_with('\\')) && !base.ends_with("://") && base != "/" {
        base.trim_end_matches(['/', '\\'])
    } else {
        base
    };
    Some((PathBuf::from(base_trimmed), rest.to_string()))
}

pub(crate) fn expand_home_query(query: &str, home: &std::path::Path) -> Option<String> {
    let rest = if query == "~" {
        ""
    } else {
        query.strip_prefix("~/").or_else(|| query.strip_prefix("~\\"))?
    };
    let rest = if !cfg!(windows) && rest.contains('\\') {
        rest.replace('\\', "/")
    } else {
        rest.to_string()
    };
    Some(format!("{}{}{}", home.display(), std::path::MAIN_SEPARATOR, rest))
}

fn merge_goto_entries(
    mut local: Vec<atlas_core::search::nucleo_pool::IndexEntry>,
    service: Vec<atlas_core::search::nucleo_pool::IndexEntry>,
) -> Vec<atlas_core::search::nucleo_pool::IndexEntry> {
    let mut paths = std::collections::HashSet::new();
    local.extend(service);
    local.retain(|entry| paths.insert(entry.path.clone()));
    local
}

fn goto_location_label(path: &std::path::Path, home: Option<&std::path::Path>) -> String {
    let path_str = path.to_string_lossy();
    if !cfg!(windows) && path_str.contains('\\') && !path_str.contains('/') {
        if let Some(pos) = path_str.rfind('\\') {
            let parent_str = &path_str[..pos];
            if let Some(home_str) = home.map(|h| h.to_string_lossy()) {
                if parent_str.starts_with(home_str.as_ref()) {
                    let rel = &parent_str[home_str.len()..].trim_start_matches('\\');
                    if rel.is_empty() {
                        return "~".to_owned();
                    }
                    return format!("~\\{rel}");
                }
            }
            return parent_str.to_string();
        }
    }
    let parent = path.parent().filter(|parent| !parent.as_os_str().is_empty()).unwrap_or(path);
    if let Some(relative) = home.and_then(|home| parent.strip_prefix(home).ok()) {
        if relative.as_os_str().is_empty() {
            return "~".to_owned();
        }
        return format!("~{}{}", std::path::MAIN_SEPARATOR, relative.display());
    }
    parent.to_string_lossy().into_owned()
}

/// Familiar locations break ties between equally relevant user folders. Exact
/// names still win over a recently visited but only approximate match.
fn goto_familiarity(
    path: &std::path::Path,
    current: &std::path::Path,
    bookmarks: &[PathBuf],
    recent: &[PathBuf],
) -> usize {
    if path == current {
        0
    } else if bookmarks.iter().any(|bookmark| bookmark == path) {
        1
    } else if let Some(index) = recent.iter().rev().position(|folder| folder == path) {
        2 + index
    } else if path.starts_with(current) {
        2 + recent.len()
    } else {
        3 + recent.len()
    }
}

#[cfg(test)]
mod goto_query_tests {
    use super::{
        expand_home_query, filename_match_quality, goto_familiarity, goto_location_label,
        merge_goto_entries, split_query_base, GotoSnapshot,
    };
    use std::path::Path;

    #[test]
    fn location_labels_keep_real_separators_and_omit_the_repeated_filename() {
        let home = Path::new("C:\\Users\\user-test");
        let path = home.join("developer").join("git").join("My Project");
        assert_eq!(goto_location_label(&path, Some(home)), format!("~{0}developer{0}git", std::path::MAIN_SEPARATOR));
        assert_eq!(goto_location_label(&home.join("myproj"), Some(home)), "~");
        assert_eq!(goto_location_label(Path::new("D:\\Projects\\egui-app"), Some(home)), "D:\\Projects");
    }

    #[test]
    fn home_paths_scope_partial_folder_names() {
        let home = Path::new("C:\\Users\\user-test");
        for query in ["~\\developer\\git\\myproj", "~/developer/git/myproj"] {
            let expanded = expand_home_query(query, home).unwrap();
            let (base, needle) = split_query_base(&expanded).unwrap();
            assert_eq!(base, home.join("developer").join("git"));
            assert_eq!(needle, "myproj");
        }
    }

    #[test]
    fn home_root_and_trailing_separator_have_empty_needles() {
        let home = Path::new("C:\\Users\\user-test");
        for query in ["~", "~/", "~\\"] {
            let expanded = expand_home_query(query, home).unwrap();
            let (base, needle) = split_query_base(&expanded).unwrap();
            assert_eq!(base, home);
            assert!(needle.is_empty());
        }
    }

    #[test]
    fn filename_queries_and_named_users_are_not_home_paths() {
        let home = Path::new("C:\\Users\\user-test");
        for query in ["egui", "~someone/developer", "file~name"] {
            assert!(expand_home_query(query, home).is_none());
            assert!(split_query_base(query).is_none());
        }
    }

    #[test]
    fn published_results_and_category_order_stay_frozen_until_the_query_changes() {
        use atlas_core::search::nucleo_pool::IndexEntry;
        let entry = |path: &str| IndexEntry {
            path: path.into(),
            is_dir: true,
        };
        let root = Path::new("home");
        let mut snapshot = GotoSnapshot::default();
        snapshot.prepare("downloads", root, false, false);
        snapshot.publish(
            vec![entry("home/Downloads"), entry("other/Downloads")],
            false,
        );
        for _ in 0..10 {
            snapshot.prepare("downloads", root, false, false);
            // Simulate a late source with different counts and a better score.
            snapshot.publish(vec![entry("new/Downloads")], true);
            let (entries, files_first) = snapshot.results.as_ref().unwrap();
            assert_eq!(entries.len(), 2);
            assert_eq!(entries[0].path, Path::new("home/Downloads"));
            assert_eq!(entries[1].path, Path::new("other/Downloads"));
            assert!(!files_first);
            assert!(!snapshot.is_pending());
        }
        snapshot.prepare("download", root, false, false);
        assert!(snapshot.is_pending());
        snapshot.publish(vec![entry("new/Downloads")], true);
        assert_eq!(
            snapshot.results.as_ref().unwrap().0[0].path,
            Path::new("new/Downloads")
        );
        assert!(snapshot.results.as_ref().unwrap().1);
    }

    #[test]
    fn empty_results_are_final_and_scope_or_filter_changes_start_a_new_snapshot() {
        let root = Path::new("home");
        let mut snapshot = GotoSnapshot::default();
        snapshot.prepare("missing", root, false, false);
        snapshot.publish(Vec::new(), false);
        snapshot.prepare("missing", root, false, false);
        assert!(!snapshot.is_pending());
        snapshot.prepare("missing", Path::new("other"), false, false);
        assert!(snapshot.is_pending());
        snapshot.publish(Vec::new(), false);
        snapshot.prepare("missing", Path::new("other"), true, false);
        assert!(snapshot.is_pending());
        snapshot.publish(Vec::new(), false);
        snapshot.prepare("missing", Path::new("other"), true, true);
        assert!(snapshot.is_pending());
    }

    #[test]
    fn service_matches_never_hide_a_fresh_local_folder() {
        use atlas_core::search::nucleo_pool::IndexEntry;
        let entry = |path: &str| IndexEntry {
            path: path.into(),
            is_dir: true,
        };
        let entries = merge_goto_entries(
            vec![entry("developer/git/myproj"), entry("developer/git/myproj")],
            vec![entry("node_modules/duplexer"), entry("developer/git/myproj")],
        );
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].path, Path::new("developer/git/myproj"));
    }

    #[test]
    fn literal_names_lead_weak_fuzzy_matches() {
        assert!(filename_match_quality("myproj", "myproj") > filename_match_quality("duplexer", "myproj"));
        assert!(filename_match_quality("egui-app", "egui") > filename_match_quality("bevy_egui", "egui"));
        assert!(filename_match_quality("bevy_egui", "egui") > filename_match_quality("registry_utils", "egui"));
        assert_eq!(filename_match_quality("Egui", "egui"), 4);
        assert_eq!(filename_match_quality("egui", "Egui"), 1);
    }

    #[test]
    fn familiar_projects_precede_other_copies_of_the_same_folder_name() {
        let current = Path::new("home/developer");
        let bookmarks = vec!["home/projects/My Project".into()];
        let recent = vec!["home/old/My Project".into(), "home/work/My Project".into()];
        let mut paths = vec![
            Path::new("other/My Project"),
            Path::new("home/old/My Project"),
            Path::new("home/work/My Project"),
            Path::new("home/projects/My Project"),
            current,
        ];
        paths.sort_by_key(|path| goto_familiarity(path, current, &bookmarks, &recent));
        assert_eq!(
            paths,
            vec![
                current,
                Path::new("home/projects/My Project"),
                Path::new("home/work/My Project"),
                Path::new("home/old/My Project"),
                Path::new("other/My Project")
            ]
        );
    }
}

/// Wrap `items` in one titled section, or no sections when empty (so the panel
/// shows its empty placeholder instead of a lone header).
fn section_or_empty(
    id: &'static str,
    icon: AtlasIcon,
    title: impl Into<gpui::SharedString>,
    items: Vec<PaletteItem>,
) -> Vec<PaletteSection> {
    if items.is_empty() {
        Vec::new()
    } else {
        vec![PaletteSection {
            id,
            icon: Some(icon),
            title: Some(title.into()),
            collapsible: true,
            items,
        }]
    }
}

impl App {
    pub(crate) fn shortcut_conflict_message(&self, cx: &gpui::App) -> Option<String> {
        let id = self.command_shortcut_conflict.as_ref()?;
        let title = self.command_groups(cx).into_iter().flat_map(|(_, _, _, commands)| commands)
            .find(|command| command.id == *id).map_or_else(|| id.clone(), |command| command.title);
        Some(rust_i18n::t!("shortcut_conflict", command = title).to_string())
    }

    /// Goto bar: fuzzy folder/file navigation. Empty query → sidebar-like home
    /// sections (Recientes/Almacenamiento/Marcadores/Lugares); content mode →
    /// files whose contents matched the background grep; otherwise the
    /// streaming nucleo index, optionally scoped to a folder when the query
    /// starts with an absolute path ("C:\dev\" + needle).
    pub(crate) fn goto_spec(&mut self, cx: &mut Context<Self>) -> PaletteSpec {
        let focused_tile = self.state.focused_tile;
        let pane = self
            .state
            .find_tabs_container(focused_tile)
            .or_else(|| self.state.find_any_tabs_container())
            .unwrap_or(focused_tile);
        let content_mode = self.state.profiles.active().search_in_content;
        let query = self.state.overlay.goto_state.query.clone();
        let trimmed = query.trim();
        if self
            .goto_service_inflight
            .as_deref()
            .is_some_and(|inflight| inflight != trimmed || content_mode)
        {
            self.goto_service_generation
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.goto_service_inflight = None;
        }

        let mut search_prefix: Option<gpui::AnyElement> = None;
        let mut sections = if trimmed.is_empty() {
            self.goto_snapshot = Default::default();
            self.goto_home_sections(focused_tile, pane)
        } else if content_mode {
            self.goto_snapshot = Default::default();
            // Files whose *contents* matched the latest grep.
            let items: Vec<PaletteItem> = self
                .content_results
                .iter()
                .take(GOTO_LIMIT)
                .map(|p| {
                    let name = p.file_name().map_or_else(
                        || p.to_string_lossy().into_owned(),
                        |n| n.to_string_lossy().into_owned(),
                    );
                    goto_entry(
                        name,
                        p.clone(),
                        AtlasIcon::TextFile,
                        false,
                        focused_tile,
                        pane,
                    )
                })
                .collect();
            section_or_empty(
                "content",
                AtlasIcon::TextFile,
                rust_i18n::t!("palette_sec_content"),
                items,
            )
        } else {
            // Scoped fuzzy: an absolute-path prefix moves the index root into
            // that folder (relative-path haystacks, files included) and
            // matches the remainder inside it.
            let scoped = split_query_base(trimmed);
            let (root, needle) = scoped
                .clone()
                .unwrap_or_else(|| (self.goto_default_root(), trimmed.to_string()));
            self.goto_snapshot.prepare(
                trimmed,
                &root,
                scoped.is_some(),
                self.state.settings.show_hidden,
            );
            let current_root = self.goto_default_root();
            let recent = self.recent_folders();
            let bookmarks: Vec<_> = self
                .state
                .profiles
                .active()
                .bookmarks
                .iter()
                .map(|bookmark| bookmark.path.clone())
                .collect();
            if let Some((base, _)) = &scoped {
                search_prefix = Some(goto_scope_pill(base, cx));
            }

            let service_entries = if scoped.is_none() {
                if let Some((cached_query, items, complete)) = &self.goto_service_cache {
                    if cached_query == trimmed {
                        Some((items.as_ref().clone(), *complete))
                    } else {
                        None
                    }
                } else {
                    None
                }
            } else {
                None
            };

            if self.state.overlay.show_goto_bar
                && scoped.is_none()
                && self.goto_snapshot.is_pending()
                && service_entries.is_none()
                && self.goto_service_inflight.as_deref() != Some(trimmed)
                && self.goto_service_failed.as_deref() != Some(trimmed)
            {
                let gen = self
                    .goto_service_generation
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                    + 1;
                self.goto_service_inflight = Some(trimmed.to_string());
                let query_str = trimmed.to_string();
                let tx = self.tx.clone();
                let gen_arc = self.goto_service_generation.clone();
                self.rt.spawn(async move {
                    let mut req = atlas_core::search::SearchQuery::text(&query_str);
                    req.generation = gen;
                    req.limit = GOTO_SECTION_LIMIT;
                    req.interactive = true;
                    let request_generation = gen_arc.clone();
                    let response = tokio::task::spawn_blocking(move || {
                        // Check again when the blocking worker starts so
                        // superseded keystrokes never queue service I/O.
                        if request_generation.load(std::sync::atomic::Ordering::SeqCst) != gen {
                            return Err(std::io::Error::other("superseded goto query"));
                        }
                        let backend = atlas_core::search::WindowsServiceBackend::new();
                        let mut request = req;
                        request.filters.categories =
                            vec![atlas_core::search::SearchCategory::Folder];
                        let folders = backend.request(
                            &atlas_core::search::ServiceRequest::Search(request.clone()),
                        )?;
                        if request_generation.load(std::sync::atomic::Ordering::SeqCst) != gen {
                            return Err(std::io::Error::other("superseded goto query"));
                        }
                        request.filters.categories = vec![atlas_core::search::SearchCategory::File];
                        let files = backend
                            .request(&atlas_core::search::ServiceRequest::Search(request))?;
                        let response = match (folders, files) {
                            (
                                atlas_core::search::ServiceResponse::Search(mut folders),
                                atlas_core::search::ServiceResponse::Search(files),
                            ) => {
                                folders.complete &= files.complete;
                                folders.results.extend(files.results);
                                atlas_core::search::ServiceResponse::Search(folders)
                            }
                            (atlas_core::search::ServiceResponse::Error(error), _)
                            | (_, atlas_core::search::ServiceResponse::Error(error)) => {
                                atlas_core::search::ServiceResponse::Error(error)
                            }
                            _ => return Err(std::io::Error::other("unexpected search response")),
                        };
                        match response {
                            atlas_core::search::ServiceResponse::Search(batch) => {
                                // Validate the small persisted page on the
                                // blocking worker, alongside the pipe I/O.
                                let results = batch
                                    .results
                                    .into_iter()
                                    .filter_map(|r| {
                                        let metadata = std::fs::metadata(&r.path).ok()?;
                                        Some((r.path, metadata.is_dir()))
                                    })
                                    .collect();
                                Ok((results, batch.complete))
                            }
                            _ => Err(std::io::Error::other("goto search failed")),
                        }
                    })
                    .await;
                    if gen_arc.load(std::sync::atomic::Ordering::SeqCst) != gen {
                        return;
                    }
                    match response {
                        Ok(Ok((results, complete))) => {
                            let _ = tx.send(atlas_model::app_state::BgResult::GotoServiceResults {
                                gen,
                                query: query_str.clone(),
                                results,
                                complete,
                            });
                        }
                        _ => {
                            let _ = tx.send(atlas_model::app_state::BgResult::GotoServiceFailed {
                                gen,
                                query: query_str,
                            });
                        }
                    }
                });
            }

            // A persisted service index can miss newer folders that the live
            // local walk already found. Keep both sources, with one row per path.
            self.ensure_goto_index(root.clone(), scoped.is_some());
            if self.goto_snapshot.is_pending() {
                self.nucleo
                    .set_pattern(&atlas_core::search::navigation::normalized_name(
                        &needle, true,
                    ));
                self.nucleo.tick();
                // Publish both sources together. Showing a provisional local page
                // first would still reorder the rows when the service page arrives.
                // A new scoped index (or service fallback) has no other source
                // to answer the query: wait for its walk and matcher to finish,
                // including the empty-directory case.
                let local_ready = if scoped.is_some() || service_entries.is_none() {
                    self.nucleo.is_complete()
                } else {
                    !self.nucleo.is_pending()
                };
                if local_ready && self.goto_service_inflight.is_none() {
                    let mut entries = merge_goto_entries(
                        self.nucleo.results(GOTO_LIMIT),
                        service_entries.map_or_else(Vec::new, |(items, _)| items),
                    );
                    let preference_root = scoped.as_ref().map(|(base, _)| base.as_path());
                    let relative_path = |path: &PathBuf| {
                        preference_root
                            .and_then(|base| path.strip_prefix(base).ok())
                            .unwrap_or(path.as_path())
                            .to_path_buf()
                    };
                    entries.retain(|entry| {
                        let noise = path_noise(&relative_path(&entry.path), &needle);
                        (scoped.is_some() || noise < 3)
                            && (self.state.settings.show_hidden || noise < 2)
                    });
                    let noise: Vec<_> = entries
                        .iter()
                        .map(|entry| path_noise(&relative_path(&entry.path), &needle))
                        .collect();
                    let familiarity: Vec<_> = entries
                        .iter()
                        .map(|entry| {
                            goto_familiarity(&entry.path, &current_root, &bookmarks, &recent)
                        })
                        .collect();

                    // Name-first re-rank: nucleo scored the whole (relative) path, so
                    // a strong match buried in a parent segment can outrank a weak one
                    // in the file name. Entries whose *name* matches the needle come
                    // first (best name score wins); pure path matches keep their
                    // nucleo order after them.
                    let names: Vec<String> = entries
                        .iter()
                        .map(|e| {
                            e.path.file_name().map_or_else(
                                || e.path.to_string_lossy().into_owned(),
                                |n| n.to_string_lossy().into_owned(),
                            )
                        })
                        .collect();
                    let name_scores = self
                        .fuzzy_scorer
                        .scores(&needle, names.iter().map(String::as_str));
                    // Multi-word queries ("emi pers") match successive path
                    // segments; a complete in-order match outranks scattered ones.
                    let segment_scores: Vec<Option<u32>> = if needle.split_whitespace().nth(1).is_some() {
                        entries
                            .iter()
                            .map(|e| {
                                let m = match_path_tokens(&needle, &relative_path(&e.path).to_string_lossy());
                                m.complete.then_some(m.score)
                            })
                            .collect()
                    } else {
                        vec![None; entries.len()]
                    };
                    let mut order: Vec<usize> = (0..entries.len()).collect();
                    order.sort_by(|&a, &b| {
                        segment_scores[b]
                            .cmp(&segment_scores[a])
                            .then_with(|| {
                                filename_match_quality(&names[b], &needle)
                                    .min(2)
                                    .cmp(&filename_match_quality(&names[a], &needle).min(2))
                            })
                            .then_with(|| noise[a].cmp(&noise[b]))
                            .then_with(|| {
                                filename_match_quality(&names[b], &needle)
                                    .cmp(&filename_match_quality(&names[a], &needle))
                            })
                            .then_with(|| familiarity[a].cmp(&familiarity[b]))
                            .then_with(|| match (name_scores[a], name_scores[b]) {
                                (Some(x), Some(y)) => y.cmp(&x),
                                (Some(_), None) => std::cmp::Ordering::Less,
                                (None, Some(_)) => std::cmp::Ordering::Greater,
                                (None, None) => std::cmp::Ordering::Equal,
                            })
                            .then_with(|| {
                                entries[a]
                                    .path
                                    .components()
                                    .count()
                                    .cmp(&entries[b].path.components().count())
                            })
                            .then_with(|| entries[a].path.cmp(&entries[b].path))
                    });

                    // Only a literal exact file match earns priority over the
                    // folders section. This order is frozen with the entries.
                    let files_first = order.first().is_some_and(|&i| {
                        !entries[i].is_dir && filename_match_quality(&names[i], &needle) == 4
                    });
                    let ranked = order.into_iter().map(|idx| entries[idx].clone()).collect();
                    self.goto_snapshot.publish(ranked, files_first);
                }
            }
            let (entries, files_first) = self.goto_snapshot.results.clone().unwrap_or_default();

            // Each category capped — and the matched characters of the name
            // tinted for scanability.
            let mut folders: Vec<PaletteItem> = Vec::new();
            let mut files: Vec<PaletteItem> = Vec::new();
            for entry in &entries {
                let bucket = if entry.is_dir {
                    &mut folders
                } else {
                    &mut files
                };
                if bucket.len() >= GOTO_SECTION_LIMIT {
                    if folders.len() >= GOTO_SECTION_LIMIT && files.len() >= GOTO_SECTION_LIMIT {
                        break;
                    }
                    continue;
                }
                let mut item = goto_index_item(entry.clone(), focused_tile, pane);
                item.highlight = self.fuzzy_scorer.match_ranges(&needle, &item.title);
                if item.highlight.is_none() {
                    item.highlight = Some(match_path_tokens(&needle, &item.title).ranges);
                }
                if let Some(desc) = &item.description {
                    item.description_highlight = Some(match_path_tokens(&needle, desc).ranges);
                }
                bucket.push(item);
            }
            let direct_path = dirs::home_dir()
                .and_then(|h| expand_home_query(trimmed, &h))
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(trimmed));
            let direct_exists = atlas_model::location::is_unc_host(&direct_path.to_string_lossy()) || direct_path.exists();
            if direct_exists && !entries.iter().any(|e| e.path == direct_path) {
                let is_dir = atlas_model::location::is_unc_host(&direct_path.to_string_lossy()) || direct_path.is_dir();
                let direct_item = goto_item(direct_path.clone(), focused_tile, pane);
                if is_dir {
                    folders.insert(0, direct_item);
                } else {
                    files.insert(0, direct_item);
                }
            }

            let dirs_sec = section_or_empty(
                "dirs",
                AtlasIcon::Folder,
                rust_i18n::t!("palette_sec_folders"),
                folders,
            );
            let files_sec = section_or_empty(
                "files",
                AtlasIcon::File,
                rust_i18n::t!("palette_sec_files"),
                files,
            );
            let mut sections = Vec::new();
            if files_first {
                sections.extend(files_sec);
                sections.extend(dirs_sec);
            } else {
                sections.extend(dirs_sec);
                sections.extend(files_sec);
            }

            let clean_name = needle.trim();
            if !direct_exists && !clean_name.is_empty() {
                let mut create_folder_item = PaletteItem::new(
                    rust_i18n::t!("palette_create_folder", name = clean_name),
                    Action::CreateFolder {
                        parent: root.clone(),
                        name: clean_name.to_string(),
                    },
                );
                create_folder_item.icon = Some(AtlasIcon::Folder);
                create_folder_item.description =
                    Some(goto_location_label(&root, dirs::home_dir().as_deref()).into());

                let mut create_file_item = PaletteItem::new(
                    rust_i18n::t!("palette_create_file", name = clean_name),
                    Action::CreateFile {
                        parent: root.clone(),
                        name: clean_name.to_string(),
                    },
                );
                create_file_item.icon = Some(AtlasIcon::File);
                create_file_item.description =
                    Some(goto_location_label(&root, dirs::home_dir().as_deref()).into());

                sections.extend(section_or_empty(
                    "create",
                    AtlasIcon::Plus,
                    rust_i18n::t!("palette_sec_create"),
                    vec![create_folder_item, create_file_item],
                ));
            }

            sections
        };

        if !content_mode && !trimmed.is_empty() && split_query_base(trimmed).is_none() {
            let mut semantic = Vec::new();
            for (icon, label, path, keywords) in [
                (AtlasIcon::TextFile, rust_i18n::t!("palette_places_documents"), dirs::document_dir(), "pdf paper docs informe text"),
                (AtlasIcon::Image, rust_i18n::t!("palette_places_pictures"), dirs::picture_dir(), "photos pics capturas wallpaper img"),
                (AtlasIcon::Music, rust_i18n::t!("palette_places_music"), dirs::audio_dir(), "sound audio mp3 flac wav"),
                (AtlasIcon::Download, rust_i18n::t!("palette_places_downloads"), dirs::download_dir(), "installer setup torrent pkg"),
            ] {
                if let Some(path) = path.filter(|_| self.fuzzy_scorer.scores(trimmed, keywords.split_whitespace()).iter().any(Option::is_some)) {
                    semantic.push(goto_entry(label.to_string(), path, icon, true, focused_tile, pane));
                }
            }
            if self.fuzzy_scorer.scores(trimmed, ["root", "pc", "disks", "discos", "volumes", "equipo"]).iter().any(Option::is_some) {
                semantic.extend(atlas_core::fs::mounts::list_drives().into_iter().map(|drive| {
                    let icon = crate::ui::sidebar::drive_icon(&drive);
                    goto_entry(drive.label, drive.mount_point, icon, true, focused_tile, pane)
                }));
            }
            let mut places = section_or_empty("semantic-places", AtlasIcon::House, rust_i18n::t!("palette_sec_places"), semantic);
            places.append(&mut sections);
            sections = places;
        }

        PaletteSpec {
            id: "goto-bar",
            kind: PaletteKind::Goto,
            sections,
            search_prefix,
            search_buttons: vec![self.content_toggle_pill(content_mode, cx)],
            footer: FooterSpec::Hints(vec![
                FooterHint {
                    keys: vec![ShortcutHint::Label("Enter".into())],
                    label: rust_i18n::t!("palette_footer_use").into(),
                },
                FooterHint {
                    keys: vec![ShortcutHint::Label(if cfg!(target_os = "macos") {
                        "⌘↵".into()
                    } else {
                        "Ctrl+↵".into()
                    })],
                    label: rust_i18n::t!("palette_footer_split").into(),
                },
                FooterHint {
                    keys: vec![ShortcutHint::Label("Shift+↵".into())],
                    label: rust_i18n::t!("palette_footer_window").into(),
                },
                FooterHint {
                    keys: vec![ShortcutHint::Label("Tab".into())],
                    label: rust_i18n::t!("palette_footer_expand").into(),
                },
                FooterHint {
                    keys: vec![ShortcutHint::Label("Esc".into())],
                    label: rust_i18n::t!("palette_footer_close").into(),
                },
            ]),
            empty_text: if !content_mode && !trimmed.is_empty() && self.goto_snapshot.is_pending() {
                rust_i18n::t!("palette_searching").into()
            } else {
                rust_i18n::t!("palette_no_results").into()
            },
        }
    }

    /// The default fuzzy scope: the focused tab's directory (home as fallback).
    pub(crate) fn goto_default_root(&self) -> PathBuf {
        self.state
            .active_tab_data()
            .and_then(|td| td.location.fs_dir().map(std::path::Path::to_path_buf))
            .or_else(dirs::home_dir)
            .unwrap_or_else(|| PathBuf::from("/"))
    }

    /// Re-point the streaming nucleo index at `root` if it isn't there already
    /// (drill-in/out re-indexes; same-root keystrokes don't). A scoped index
    /// walks only that folder (files included, relative haystacks); the
    /// unscoped one is the global folder index.
    fn ensure_goto_index(&mut self, root: PathBuf, scoped: bool) {
        if self.goto_index_root.as_ref() == Some(&(root.clone(), scoped)) {
            return;
        }
        self.nucleo.reset();
        let injector = self.nucleo.injector();
        // These folders are already known to the profile. Make them searchable
        // immediately, even if a disk snapshot is stale or the walk is ongoing.
        let known_paths: Vec<_> = self
            .recent_folders()
            .into_iter()
            .chain(
                self.state
                    .profiles
                    .active()
                    .bookmarks
                    .iter()
                    .map(|bookmark| bookmark.path.clone()),
            )
            .collect();
        let known_root = root.clone();
        let known_injector = injector.clone();
        self.rt.spawn_blocking(move || {
            for path in known_paths {
                if (scoped && !path.starts_with(&known_root)) || !path.is_dir() {
                    continue;
                }
                let display = if scoped {
                    path.strip_prefix(&known_root).unwrap_or(&path)
                } else {
                    &path
                }
                .to_string_lossy()
                .into_owned();
                known_injector.push(
                    atlas_core::search::nucleo_pool::IndexEntry { path, is_dir: true },
                    move |_, cols| {
                        cols[0] = display.as_str().into();
                    },
                );
            }
        });
        if scoped {
            atlas_core::fs::index::build_index_scoped(root.clone(), injector);
        } else {
            let extended = self.goto_extended_index();
            atlas_core::fs::index::build_goto_index(root.clone(), injector, extended);
        }
        self.goto_index_root = Some((root, scoped));
    }

    /// Empty-query home view, mirroring the sidebar: Recientes, Almacenamiento
    /// (drives), Marcadores (profile bookmarks) and Lugares (user dirs).
    fn goto_home_sections(
        &self,
        tab: atlas_model::pane::TabId,
        pane: PaneId,
    ) -> Vec<PaletteSection> {
        let mut sections: Vec<PaletteSection> = Vec::new();

        // Recientes (most-recent first, excluding the current dir).
        let cur = self
            .state
            .active_tab_data()
            .and_then(|td| td.location.fs_dir().map(std::path::Path::to_path_buf));
        let recent: Vec<PaletteItem> = self
            .recent_folders()
            .iter()
            .rev()
            .filter(|p| cur.as_deref() != Some(p.as_path()))
            .take(GOTO_HOME_LIMIT)
            .map(|p| goto_item(p.clone(), tab, pane))
            .collect();
        sections.extend(section_or_empty(
            "recent",
            AtlasIcon::ClockCounterClockwise,
            rust_i18n::t!("palette_sec_recent"),
            recent,
        ));

        // Almacenamiento: mounted drives (cached; same source as the sidebar).
        let drives: Vec<PaletteItem> = atlas_core::fs::mounts::list_drives()
            .into_iter()
            .take(GOTO_HOME_LIMIT)
            .map(|d| {
                let icon = crate::ui::sidebar::drive_icon(&d);
                goto_entry(
                    d.label.clone(),
                    d.mount_point,
                    icon,
                    true,
                    tab,
                    pane,
                )
            })
            .collect();
        sections.extend(section_or_empty(
            "storage",
            AtlasIcon::HardDrive,
            rust_i18n::t!("palette_sec_storage"),
            drives,
        ));

        // Marcadores del perfil activo.
        let bookmarks: Vec<PaletteItem> = self
            .state
            .profiles
            .active()
            .bookmarks
            .iter()
            .take(GOTO_HOME_LIMIT)
            .map(|b| {
                goto_entry(
                    b.name.clone(),
                    b.path.clone(),
                    AtlasIcon::BookmarkSimple,
                    true,
                    tab,
                    pane,
                )
            })
            .collect();
        sections.extend(section_or_empty(
            "bookmarks",
            AtlasIcon::BookmarkSimple,
            rust_i18n::t!("palette_sec_bookmarks"),
            bookmarks,
        ));

        // Lugares: user dirs (same set/icons as the sidebar).
        let mut places: Vec<PaletteItem> = [
            (
                AtlasIcon::House,
                rust_i18n::t!("palette_places_home"),
                dirs::home_dir(),
            ),
            (
                AtlasIcon::Monitor,
                rust_i18n::t!("palette_places_desktop"),
                dirs::desktop_dir(),
            ),
            (
                AtlasIcon::TextFile,
                rust_i18n::t!("palette_places_documents"),
                dirs::document_dir(),
            ),
            (
                AtlasIcon::Download,
                rust_i18n::t!("palette_places_downloads"),
                dirs::download_dir(),
            ),
            (
                AtlasIcon::Image,
                rust_i18n::t!("palette_places_pictures"),
                dirs::picture_dir(),
            ),
            (
                AtlasIcon::Music,
                rust_i18n::t!("palette_places_music"),
                dirs::audio_dir(),
            ),
            (
                AtlasIcon::Video,
                rust_i18n::t!("palette_places_videos"),
                dirs::video_dir(),
            ),
        ]
        .into_iter()
        .filter_map(|(ic, label, p)| {
            p.map(|p| goto_entry(label.to_string(), p, ic, true, tab, pane))
        })
        .collect();
        #[cfg(target_os = "windows")]
        {
            let mut recycle_bin = PaletteItem::new(
                rust_i18n::t!("cmd_nav_recycle_bin_title").to_string(),
                Action::NavigateLocation {
                    tab,
                    location: atlas_model::location::Location::RecycleBin,
                },
            );
            recycle_bin.icon = Some(AtlasIcon::Trash);
            places.push(recycle_bin);
        }
        sections.extend(section_or_empty(
            "places",
            AtlasIcon::House,
            rust_i18n::t!("palette_sec_places"),
            places,
        ));

        sections
    }

    /// Opt-in (per profile): index every drive and WSL distro for goto.
    pub(crate) fn goto_extended_index(&self) -> bool {
        self.state
            .profiles
            .active()
            .prefs
            .get::<bool>("goto_extended_index")
            .unwrap_or(false)
    }

    /// Flip the per-profile extended goto index (Settings) and rebuild it.
    pub(crate) fn set_goto_extended_index(&mut self, on: bool) {
        self.state.profiles.active_mut().prefs.set("goto_extended_index", &on);
        atlas_core::util::config::save_profiles(&self.state.profiles);
        self.goto_index_root = None;
        self.goto_snapshot = Default::default();
    }

    /// The "search in content" pill at the right of the goto search row.
    /// Toggles the active profile's `search_in_content`, persists it, and
    /// restarts the search so the results switch modes immediately.
    fn content_toggle_pill(&mut self, active: bool, cx: &mut Context<Self>) -> AnyElement {
        let (accent, accent_fg, border, muted) = {
            let t = cx.theme();
            (t.accent, t.accent_foreground, t.border, t.muted_foreground)
        };
        let view = cx.entity();
        div()
            .id("goto-content-toggle")
            .px_2()
            .py(px(2.))
            .rounded_md()
            .border_1()
            .border_color(if active { accent } else { border })
            .when(active, |s| s.bg(accent).text_color(accent_fg))
            .when(!active, |s| s.text_color(muted))
            .cursor_pointer()
            .text_xs()
            .child(rust_i18n::t!("palette_btn_content"))
            .tooltip(|window, cx| {
                gpui_component::tooltip::Tooltip::new(
                    rust_i18n::t!("palette_btn_content_tooltip").to_string(),
                )
                .build(window, cx)
            })
            .on_mouse_down(MouseButton::Left, move |_ev, _w, cx| {
                view.update(cx, |this, cx| {
                    let now = !this.state.profiles.active().search_in_content;
                    this.state.profiles.active_mut().search_in_content = now;
                    atlas_core::util::config::save_profiles(&this.state.profiles);
                    // Reset results and relaunch in the new mode.
                    this.relaunch_content_search(cx);
                    this.state.overlay.goto_state.selected_idx = 0;
                    cx.notify();
                });
            })
            .into_any_element()
    }

    /// Extension palette: a flat pick-list of strings provided by a WASM
    /// extension; the chosen label is sent back over the reply channel.
    pub(crate) fn ext_palette_spec(&mut self) -> PaletteSpec {
        let query = self.state.overlay.ext_palette_state.query.clone();
        let labels = self.state.overlay.ext_palette_items.clone();
        let mut items: Vec<PaletteItem> = self
            .fuzzy_scorer
            .rank(&query, labels.iter().map(String::as_str))
            .into_iter()
            .map(|i| {
                let mut item = PaletteItem::new(
                    labels[i].clone(),
                    Action::SubmitExtPalette(labels[i].clone()),
                );
                item.icon = Some(AtlasIcon::Palette);
                item
            })
            .collect();
        for item in &mut items {
            item.highlight = self.fuzzy_scorer.match_ranges(&query, &item.title);
        }
        PaletteSpec {
            id: "ext-palette",
            kind: PaletteKind::Ext,
            sections: vec![PaletteSection::flat(items)],
            search_prefix: None,
            search_buttons: Vec::new(),
            footer: FooterSpec::Default,
            empty_text: rust_i18n::t!("palette_no_results").into(),
        }
    }

    /// Command palette: every app command grouped into sections, with icons,
    /// descriptions, live shortcut badges, and a pinned "Quick" section on top.
    /// A non-empty query fuzzy-ranks within each section, drops empty ones and
    /// re-orders the sections themselves by their best-matching item, so the
    /// most relevant category surfaces first.
    pub(crate) fn command_palette_spec(&mut self, cx: &gpui::App) -> PaletteSpec {
        let groups = self.command_groups(cx);
        let pinned = self.state.profiles.active().pinned_commands.clone();
        let query = self.state.overlay.palette_state.query.clone();
        let searching = !query.trim().is_empty();
        let pattern = if searching {
            atlas_core::search::fuzzy::FuzzyScorer::parse_pattern(&query)
        } else {
            None
        };

        // (best item score, declaration order, section) — declaration order is
        // the tie-breaker and the whole order when not searching.
        let mut scored: Vec<(u32, usize, PaletteSection)> = Vec::new();

        // "Quick": the pinned commands, in the order they were pinned.
        let quick_defs: Vec<&CommandDef> = pinned
            .iter()
            .filter_map(|id| {
                groups
                    .iter()
                    .flat_map(|(_, _, _, defs)| defs.iter())
                    .find(|def| def.id == *id)
            })
            .collect();
        if let Some((best, items)) = self.scored_command_items(&quick_defs, true, &pinned, pattern.as_ref()) {
            scored.push((
                best,
                0,
                PaletteSection {
                    id: "quick",
                    icon: Some(AtlasIcon::Lightning),
                    title: Some(rust_i18n::t!("palette_sec_quick").into()),
                    collapsible: true,
                    items,
                },
            ));
        }

        for (decl, (id, title, icon, defs)) in groups.into_iter().enumerate() {
            let refs: Vec<&CommandDef> = defs.iter().collect();
            let icon = icon.clone();
            let title = title.clone();
            let id = id;
            if let Some((best, items)) = self.scored_command_items(&refs, false, &pinned, pattern.as_ref()) {
                scored.push((
                    best,
                    decl + 1,
                    PaletteSection {
                        id,
                        icon: Some(icon),
                        title: Some(title.into()),
                        collapsible: true,
                        items,
                    },
                ));
            }
        }

        if searching {
            scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        }
        let sections: Vec<PaletteSection> = scored.into_iter().map(|(_, _, s)| s).collect();

        PaletteSpec {
            id: "command-palette",
            kind: PaletteKind::Commands,
            sections,
            search_prefix: None,
            search_buttons: Vec::new(),
            footer: FooterSpec::Default,
            empty_text: rust_i18n::t!("palette_no_results").into(),
        }
    }

    /// Score `defs` against `query`: matching items as palette rows (best
    /// first, highlights applied) plus the section's best score for category
    /// re-ordering. `None` when nothing matches. An empty query keeps the
    /// declared order with score 0.
    fn scored_command_items(
        &mut self,
        defs: &[&CommandDef],
        force_pinned: bool,
        pinned: &[String],
        pattern: Option<&atlas_core::search::fuzzy::Pattern>,
    ) -> Option<(u32, Vec<PaletteItem>)> {
        let scores: Vec<Option<u32>> = if let Some(pat) = pattern {
            defs.iter()
                .map(|d| {
                    if let Some(s) = self.fuzzy_scorer.score_pattern(pat, &d.title) {
                        return Some(s);
                    }
                    if let Some(s) = d.aliases.iter().find_map(|alias| {
                        self.fuzzy_scorer.score_pattern(pat, alias)
                    }) {
                        return Some(s.saturating_sub(5).max(1));
                    }
                    if let Some(s) = self.fuzzy_scorer.score_pattern(pat, &d.id) {
                        return Some(s.saturating_sub(10).max(1));
                    }
                    if !d.description.is_empty() {
                        if let Some(s) = self.fuzzy_scorer.score_pattern(pat, &d.description) {
                            return Some(s.saturating_sub(15).max(1));
                        }
                    }
                    None
                })
                .collect()
        } else {
            vec![None; defs.len()]
        };
        let order: Vec<usize> = if pattern.is_some() {
            let mut v: Vec<usize> = (0..defs.len()).filter(|&i| scores[i].is_some()).collect();
            v.sort_by(|&a, &b| scores[b].cmp(&scores[a]).then(a.cmp(&b)));
            v
        } else {
            (0..defs.len()).collect()
        };
        if order.is_empty() {
            return None;
        }
        let best = order.first().and_then(|&i| scores[i]).unwrap_or(0);
        let items: Vec<PaletteItem> = order
            .into_iter()
            .map(|i| {
                let def = defs[i];
                let shortcut = self.command_shortcut_keystroke(&def.id);
                let recording = self
                    .command_shortcut_capture
                    .as_ref()
                    .is_some_and(|id| id == &def.id);
                let mut item = def.item(
                    force_pinned || pinned.contains(&def.id),
                    shortcut,
                    recording,
                );
                if let Some(pat) = pattern {
                    item.highlight = self.fuzzy_scorer.match_ranges_pattern(pat, &item.title);
                }
                item
            })
            .collect();
        Some((best, items))
    }

    /// The command table: `(section id, section title, section icon, commands)`.
    #[allow(clippy::type_complexity)] // local table shape, read once below
    fn command_groups(
        &self,
        cx: &gpui::App,
    ) -> Vec<(&'static str, gpui::SharedString, AtlasIcon, Vec<CommandDef>)> {
        let focused = self.state.focused_tile;

        let tabs = vec![
            CommandDef::new(
                "tab.new",
                rust_i18n::t!("cmd_tab_new_title"),
                rust_i18n::t!("cmd_tab_new_desc"),
                AtlasIcon::Plus,
                Action::NewTabFocused,
            ),
            CommandDef::new(
                "tab.close",
                rust_i18n::t!("cmd_tab_close_title"),
                rust_i18n::t!("cmd_tab_close_desc"),
                AtlasIcon::X,
                Action::CloseFocusedTab,
            ),
            CommandDef::new(
                "tab.next",
                rust_i18n::t!("cmd_tab_next_title"),
                rust_i18n::t!("cmd_tab_next_desc"),
                AtlasIcon::ArrowRight,
                Action::FocusNextTab,
            ),
            CommandDef::new(
                "tab.reopen",
                if rust_i18n::locale().starts_with("es") { "Reabrir pestaña cerrada^restaurar,deshacer,cerrada" } else { "Reopen closed tab^restore,undo,closed" },
                "Ctrl+Shift+T",
                AtlasIcon::ArrowLeft,
                Action::ReopenClosedTab,
            ),
            CommandDef::new(
                "tab.detach",
                if rust_i18n::locale().starts_with("es") { "Mover pestaña a nueva ventana^desacoplar,separar" } else { "Move tab to new window^detach,separate" },
                "",
                AtlasIcon::ArrowRight,
                Action::DetachFocusedTab,
            ),
            CommandDef::new(
                "tab.previous",
                rust_i18n::t!("cmd_tab_previous_title"),
                rust_i18n::t!("cmd_tab_previous_desc"),
                AtlasIcon::ArrowLeft,
                Action::FocusPreviousTab,
            ),
            CommandDef::new(
                "tab.move-left",
                rust_i18n::t!("cmd_tab_move_left_title"),
                rust_i18n::t!("cmd_tab_move_left_desc"),
                AtlasIcon::ArrowLeft,
                Action::MoveFocusedTabLeft,
            ),
            CommandDef::new(
                "tab.move-right",
                rust_i18n::t!("cmd_tab_move_right_title"),
                rust_i18n::t!("cmd_tab_move_right_desc"),
                AtlasIcon::ArrowRight,
                Action::MoveFocusedTabRight,
            ),
            CommandDef::new(
                "tab.1",
                rust_i18n::t!("cmd_tab_1_title"),
                rust_i18n::t!("cmd_tab_index_desc"),
                AtlasIcon::List,
                Action::FocusTabIndex(0),
            ),
            CommandDef::new(
                "tab.2",
                rust_i18n::t!("cmd_tab_2_title"),
                rust_i18n::t!("cmd_tab_index_desc"),
                AtlasIcon::List,
                Action::FocusTabIndex(1),
            ),
            CommandDef::new(
                "tab.3",
                rust_i18n::t!("cmd_tab_3_title"),
                rust_i18n::t!("cmd_tab_index_desc"),
                AtlasIcon::List,
                Action::FocusTabIndex(2),
            ),
            CommandDef::new(
                "tab.4",
                rust_i18n::t!("cmd_tab_4_title"),
                rust_i18n::t!("cmd_tab_index_desc"),
                AtlasIcon::List,
                Action::FocusTabIndex(3),
            ),
            CommandDef::new(
                "tab.5",
                rust_i18n::t!("cmd_tab_5_title"),
                rust_i18n::t!("cmd_tab_index_desc"),
                AtlasIcon::List,
                Action::FocusTabIndex(4),
            ),
            CommandDef::new(
                "tab.6",
                rust_i18n::t!("cmd_tab_6_title"),
                rust_i18n::t!("cmd_tab_index_desc"),
                AtlasIcon::List,
                Action::FocusTabIndex(5),
            ),
            CommandDef::new(
                "tab.7",
                rust_i18n::t!("cmd_tab_7_title"),
                rust_i18n::t!("cmd_tab_index_desc"),
                AtlasIcon::List,
                Action::FocusTabIndex(6),
            ),
            CommandDef::new(
                "tab.8",
                rust_i18n::t!("cmd_tab_8_title"),
                rust_i18n::t!("cmd_tab_index_desc"),
                AtlasIcon::List,
                Action::FocusTabIndex(7),
            ),
            CommandDef::new(
                "tab.9",
                rust_i18n::t!("cmd_tab_9_title"),
                rust_i18n::t!("cmd_tab_index_desc"),
                AtlasIcon::List,
                Action::FocusTabIndex(8),
            ),
            CommandDef::new(
                "pane.split-h",
                rust_i18n::t!("cmd_pane_split_h_title"),
                rust_i18n::t!("cmd_pane_split_h_desc"),
                AtlasIcon::SplitHorizontal,
                Action::SplitHorizontalFocused,
            ),
            CommandDef::new(
                "pane.split-v",
                rust_i18n::t!("cmd_pane_split_v_title"),
                rust_i18n::t!("cmd_pane_split_v_desc"),
                AtlasIcon::SplitVertical,
                Action::SplitVerticalFocused,
            ),
            CommandDef::new(
                "pane.close",
                rust_i18n::t!("cmd_pane_close_title"),
                rust_i18n::t!("cmd_pane_close_desc"),
                AtlasIcon::X,
                Action::CloseFocusedPane,
            ),
            CommandDef::new(
                "pane.focus-next",
                rust_i18n::t!("cmd_pane_focus_next_title"),
                rust_i18n::t!("cmd_pane_focus_next_desc"),
                AtlasIcon::SplitVertical,
                Action::FocusNextPane,
            ),
            CommandDef::new("pane.focus-left", rust_i18n::t!("cmd_pane_left_title"), rust_i18n::t!("cmd_pane_spatial_desc"), AtlasIcon::SplitHorizontal, Action::SelectLeftPanel),
            CommandDef::new("pane.focus-right", rust_i18n::t!("cmd_pane_right_title"), rust_i18n::t!("cmd_pane_spatial_desc"), AtlasIcon::SplitHorizontal, Action::SelectRightPanel),
            CommandDef::new("pane.focus-above", rust_i18n::t!("cmd_pane_above_title"), rust_i18n::t!("cmd_pane_spatial_desc"), AtlasIcon::SplitVertical, Action::SelectPanelAbove),
            CommandDef::new("pane.focus-below", rust_i18n::t!("cmd_pane_below_title"), rust_i18n::t!("cmd_pane_spatial_desc"), AtlasIcon::SplitVertical, Action::SelectPanelBelow),
            CommandDef::new("pane.even-siblings", rust_i18n::t!("cmd_pane_even_siblings_title"), rust_i18n::t!("cmd_pane_even_siblings_desc"), AtlasIcon::SplitHorizontal, Action::EvenPanelSiblings),
            CommandDef::new("pane.even-all", rust_i18n::t!("cmd_pane_even_all_title"), rust_i18n::t!("cmd_pane_even_all_desc"), AtlasIcon::SplitVertical, Action::EvenAllPanes),
            CommandDef::new("folder.inherit-view", rust_i18n::t!("cmd_folder_inherit_title"), rust_i18n::t!("cmd_folder_inherit_desc"), AtlasIcon::Folder, Action::ToggleFolderViewRecursive),
        ];

        let nav = vec![
            CommandDef::new(
                "nav.back",
                rust_i18n::t!("cmd_nav_back_title"),
                rust_i18n::t!("cmd_nav_back_desc"),
                AtlasIcon::ArrowLeft,
                Action::NavigateBack(focused),
            ),
            CommandDef::new(
                "nav.forward",
                rust_i18n::t!("cmd_nav_forward_title"),
                rust_i18n::t!("cmd_nav_forward_desc"),
                AtlasIcon::ArrowRight,
                Action::NavigateForward(focused),
            ),
            CommandDef::new(
                "nav.parent",
                rust_i18n::t!("cmd_nav_parent_title"),
                rust_i18n::t!("cmd_nav_parent_desc"),
                AtlasIcon::ArrowCounterClockwise,
                Action::NavigateUp(focused),
            ),
            CommandDef::new(
                "nav.goto",
                rust_i18n::t!("cmd_nav_goto_title"),
                rust_i18n::t!("cmd_nav_goto_desc"),
                AtlasIcon::Search,
                Action::OpenGotoBar,
            ),
            CommandDef::new(
                "nav.refresh",
                rust_i18n::t!("cmd_nav_refresh_title"),
                rust_i18n::t!("cmd_nav_refresh_desc"),
                AtlasIcon::ArrowClockwise,
                Action::Refresh(focused),
            ),
            #[cfg(target_os = "windows")]
            CommandDef::new(
                "nav.recycle-bin",
                rust_i18n::t!("cmd_nav_recycle_bin_title"),
                rust_i18n::t!("cmd_nav_recycle_bin_desc"),
                AtlasIcon::Trash,
                Action::NavigateLocation {
                    tab: focused,
                    location: atlas_model::location::Location::RecycleBin,
                },
            ),
        ];

        let selection = vec![
            CommandDef::new(
                "selection.previous",
                rust_i18n::t!("cmd_selection_previous_title"),
                rust_i18n::t!("cmd_selection_previous_desc"),
                AtlasIcon::ArrowLeft,
                Action::MoveSelection {
                    target: crate::input::action::SelectionMove::Previous,
                    extend: false,
                },
            ),
            CommandDef::new(
                "selection.next",
                rust_i18n::t!("cmd_selection_next_title"),
                rust_i18n::t!("cmd_selection_next_desc"),
                AtlasIcon::ArrowRight,
                Action::MoveSelection {
                    target: crate::input::action::SelectionMove::Next,
                    extend: false,
                },
            ),
            CommandDef::new(
                "selection.left",
                rust_i18n::t!("cmd_selection_left_title"),
                rust_i18n::t!("cmd_selection_left_desc"),
                AtlasIcon::ArrowLeft,
                Action::MoveSelection {
                    target: crate::input::action::SelectionMove::Left,
                    extend: false,
                },
            ),
            CommandDef::new(
                "selection.right",
                rust_i18n::t!("cmd_selection_right_title"),
                rust_i18n::t!("cmd_selection_right_desc"),
                AtlasIcon::ArrowRight,
                Action::MoveSelection {
                    target: crate::input::action::SelectionMove::Right,
                    extend: false,
                },
            ),
            CommandDef::new(
                "selection.first",
                rust_i18n::t!("cmd_selection_first_title"),
                rust_i18n::t!("cmd_selection_first_desc"),
                AtlasIcon::ArrowLeft,
                Action::MoveSelection {
                    target: crate::input::action::SelectionMove::First,
                    extend: false,
                },
            ),
            CommandDef::new(
                "selection.last",
                rust_i18n::t!("cmd_selection_last_title"),
                rust_i18n::t!("cmd_selection_last_desc"),
                AtlasIcon::ArrowRight,
                Action::MoveSelection {
                    target: crate::input::action::SelectionMove::Last,
                    extend: false,
                },
            ),
            CommandDef::new(
                "selection.page-up",
                rust_i18n::t!("cmd_selection_page_up_title"),
                rust_i18n::t!("cmd_selection_page_up_desc"),
                AtlasIcon::ArrowLeft,
                Action::MoveSelection {
                    target: crate::input::action::SelectionMove::PageUp,
                    extend: false,
                },
            ),
            CommandDef::new(
                "selection.page-down",
                rust_i18n::t!("cmd_selection_page_down_title"),
                rust_i18n::t!("cmd_selection_page_down_desc"),
                AtlasIcon::ArrowRight,
                Action::MoveSelection {
                    target: crate::input::action::SelectionMove::PageDown,
                    extend: false,
                },
            ),
            CommandDef::new(
                "selection.extend-previous",
                rust_i18n::t!("cmd_selection_extend_previous_title"),
                rust_i18n::t!("cmd_selection_extend_previous_desc"),
                AtlasIcon::SelectionAll,
                Action::MoveSelection {
                    target: crate::input::action::SelectionMove::Previous,
                    extend: true,
                },
            ),
            CommandDef::new(
                "selection.extend-next",
                rust_i18n::t!("cmd_selection_extend_next_title"),
                rust_i18n::t!("cmd_selection_extend_next_desc"),
                AtlasIcon::SelectionAll,
                Action::MoveSelection {
                    target: crate::input::action::SelectionMove::Next,
                    extend: true,
                },
            ),
            CommandDef::new(
                "selection.extend-left",
                rust_i18n::t!("cmd_selection_extend_left_title"),
                rust_i18n::t!("cmd_selection_extend_left_desc"),
                AtlasIcon::SelectionAll,
                Action::MoveSelection {
                    target: crate::input::action::SelectionMove::Left,
                    extend: true,
                },
            ),
            CommandDef::new(
                "selection.extend-right",
                rust_i18n::t!("cmd_selection_extend_right_title"),
                rust_i18n::t!("cmd_selection_extend_right_desc"),
                AtlasIcon::SelectionAll,
                Action::MoveSelection {
                    target: crate::input::action::SelectionMove::Right,
                    extend: true,
                },
            ),
            CommandDef::new(
                "selection.extend-first",
                rust_i18n::t!("cmd_selection_extend_first_title"),
                rust_i18n::t!("cmd_selection_extend_first_desc"),
                AtlasIcon::SelectionAll,
                Action::MoveSelection {
                    target: crate::input::action::SelectionMove::First,
                    extend: true,
                },
            ),
            CommandDef::new(
                "selection.extend-last",
                rust_i18n::t!("cmd_selection_extend_last_title"),
                rust_i18n::t!("cmd_selection_extend_last_desc"),
                AtlasIcon::SelectionAll,
                Action::MoveSelection {
                    target: crate::input::action::SelectionMove::Last,
                    extend: true,
                },
            ),
            CommandDef::new(
                "selection.extend-page-up",
                rust_i18n::t!("cmd_selection_extend_page_up_title"),
                rust_i18n::t!("cmd_selection_extend_page_up_desc"),
                AtlasIcon::SelectionAll,
                Action::MoveSelection {
                    target: crate::input::action::SelectionMove::PageUp,
                    extend: true,
                },
            ),
            CommandDef::new(
                "selection.extend-page-down",
                rust_i18n::t!("cmd_selection_extend_page_down_title"),
                rust_i18n::t!("cmd_selection_extend_page_down_desc"),
                AtlasIcon::SelectionAll,
                Action::MoveSelection {
                    target: crate::input::action::SelectionMove::PageDown,
                    extend: true,
                },
            ),
            CommandDef::new(
                "selection.cursor-previous",
                rust_i18n::t!("cmd_selection_cursor_previous_title"),
                rust_i18n::t!("cmd_selection_cursor_previous_desc"),
                AtlasIcon::ArrowLeft,
                Action::MoveCursor(crate::input::action::SelectionMove::Previous),
            ),
            CommandDef::new(
                "selection.cursor-next",
                rust_i18n::t!("cmd_selection_cursor_next_title"),
                rust_i18n::t!("cmd_selection_cursor_next_desc"),
                AtlasIcon::ArrowRight,
                Action::MoveCursor(crate::input::action::SelectionMove::Next),
            ),
            CommandDef::new(
                "selection.cursor-left",
                rust_i18n::t!("cmd_selection_cursor_left_title"),
                rust_i18n::t!("cmd_selection_cursor_left_desc"),
                AtlasIcon::ArrowLeft,
                Action::MoveCursor(crate::input::action::SelectionMove::Left),
            ),
            CommandDef::new(
                "selection.cursor-right",
                rust_i18n::t!("cmd_selection_cursor_right_title"),
                rust_i18n::t!("cmd_selection_cursor_right_desc"),
                AtlasIcon::ArrowRight,
                Action::MoveCursor(crate::input::action::SelectionMove::Right),
            ),
            CommandDef::new(
                "selection.toggle",
                rust_i18n::t!("cmd_selection_toggle_title"),
                rust_i18n::t!("cmd_selection_toggle_desc"),
                AtlasIcon::Square,
                Action::ToggleSelectionCursor,
            ),
            CommandDef::new(
                "selection.open",
                rust_i18n::t!("cmd_selection_open_title"),
                rust_i18n::t!("cmd_selection_open_desc"),
                AtlasIcon::Folder,
                Action::OpenCursor,
            ),
        ];

        let view = vec![
            CommandDef::new(
                "view.hidden",
                rust_i18n::t!("cmd_view_hidden_title"),
                rust_i18n::t!("cmd_view_hidden_desc"),
                AtlasIcon::Eye,
                Action::ToggleHidden,
            ),
            CommandDef::new(
                "view.vim-mode",
                rust_i18n::t!("cmd_view_vim_mode_title"),
                rust_i18n::t!("cmd_view_vim_mode_desc"),
                AtlasIcon::Lightning,
                Action::ToggleVimMode,
            ),
            CommandDef::new(
                "view.inspector",
                rust_i18n::t!("cmd_view_inspector_title"),
                rust_i18n::t!("cmd_view_inspector_desc"),
                AtlasIcon::Columns,
                Action::ToggleInspector,
            ),
            CommandDef::new(
                "view.branch-view",
                rust_i18n::t!("cmd_view_branch_view_title"),
                rust_i18n::t!("cmd_view_branch_view_desc"),
                AtlasIcon::TreeStructure,
                Action::ToggleBranchViewFocused,
            ),
            CommandDef::new(
                "view.sidebar",
                rust_i18n::t!("cmd_view_sidebar_title"),
                rust_i18n::t!("cmd_view_sidebar_desc"),
                AtlasIcon::List,
                Action::ToggleSidebar,
            ),
            CommandDef::new(
                "view.sidebar-collapse",
                if self.state.settings.sidebar_collapsed {
                    rust_i18n::t!("sidebar_expand")
                } else {
                    rust_i18n::t!("sidebar_collapse")
                },
                "",
                AtlasIcon::SidebarSimple,
                Action::ToggleSidebarCollapsed,
            ),
            CommandDef::new(
                "view.details",
                rust_i18n::t!("cmd_view_details_title"),
                rust_i18n::t!("cmd_view_details_desc"),
                AtlasIcon::Details,
                Action::SetViewMode(ViewMode::Details),
            ),
            CommandDef::new(
                "view.grid",
                rust_i18n::t!("cmd_view_grid_title"),
                rust_i18n::t!("cmd_view_grid_desc"),
                AtlasIcon::Grid,
                Action::SetViewMode(ViewMode::Grid),
            ),
            CommandDef::new(
                "view.list",
                rust_i18n::t!("cmd_view_list_title"),
                rust_i18n::t!("cmd_view_list_desc"),
                AtlasIcon::List,
                Action::SetViewMode(ViewMode::List),
            ),
            CommandDef::new(
                "view.miller",
                rust_i18n::t!("cmd_view_miller_title"),
                rust_i18n::t!("cmd_view_miller_desc"),
                AtlasIcon::Columns,
                Action::SetViewMode(ViewMode::Miller),
            ),
            CommandDef::new(
                "view.tree",
                rust_i18n::t!("cmd_view_tree_title"),
                rust_i18n::t!("cmd_view_tree_desc"),
                AtlasIcon::TreeStructure,
                Action::SetViewMode(ViewMode::Tree),
            ),
            CommandDef::new(
                "view.group-none",
                rust_i18n::t!("cmd_group_none"), "", AtlasIcon::List,
                Action::SetGroupBy(atlas_model::tab::GroupBy::None),
            ),
            CommandDef::new(
                "view.group-date",
                rust_i18n::t!("cmd_group_date"), "", AtlasIcon::List,
                Action::SetGroupBy(atlas_model::tab::GroupBy::Date),
            ),
            CommandDef::new(
                "view.group-size",
                rust_i18n::t!("cmd_group_size"), "", AtlasIcon::List,
                Action::SetGroupBy(atlas_model::tab::GroupBy::Size),
            ),
            CommandDef::new(
                "view.group-type",
                rust_i18n::t!("cmd_group_type"), "", AtlasIcon::List,
                Action::SetGroupBy(atlas_model::tab::GroupBy::Type),
            ),
            CommandDef::new(
                "view.group-name",
                rust_i18n::t!("cmd_group_name"), "", AtlasIcon::List,
                Action::SetGroupBy(atlas_model::tab::GroupBy::Name),
            ),
            CommandDef::new(
                "view.group-created",
                rust_i18n::t!("cmd_group_created"), "", AtlasIcon::List,
                Action::SetGroupBy(atlas_model::tab::GroupBy::Created),
            ),
            CommandDef::new(
                "view.group-accessed",
                rust_i18n::t!("cmd_group_accessed"), "", AtlasIcon::List,
                Action::SetGroupBy(atlas_model::tab::GroupBy::Accessed),
            ),
            CommandDef::new(
                "view.group-deleted",
                rust_i18n::t!("cmd_group_deleted"), "", AtlasIcon::List,
                Action::SetGroupBy(atlas_model::tab::GroupBy::Deleted),
            ),
            CommandDef::new(
                "view.group-origin",
                rust_i18n::t!("cmd_group_origin"), "", AtlasIcon::List,
                Action::SetGroupBy(atlas_model::tab::GroupBy::Origin),
            ),
            CommandDef::new(
                "view.group-expand",
                rust_i18n::t!("cmd_group_expand"), "", AtlasIcon::List,
                Action::SetAllGroupsCollapsed(false),
            ),
            CommandDef::new(
                "view.group-collapse",
                rust_i18n::t!("cmd_group_collapse"), "", AtlasIcon::List,
                Action::SetAllGroupsCollapsed(true),
            ),
            CommandDef::new(
                "view.zoom-in",
                rust_i18n::t!("cmd_zoom_in_title"),
                rust_i18n::t!("cmd_zoom_in_desc"),
                AtlasIcon::Plus,
                Action::ZoomIn,
            ),
            CommandDef::new(
                "view.zoom-out",
                rust_i18n::t!("cmd_zoom_out_title"),
                rust_i18n::t!("cmd_zoom_out_desc"),
                AtlasIcon::ArrowCounterClockwise,
                Action::ZoomOut,
            ),
            CommandDef::new(
                "view.zoom-reset",
                rust_i18n::t!("cmd_zoom_reset_title"),
                rust_i18n::t!("cmd_zoom_reset_desc"),
                AtlasIcon::ArrowClockwise,
                Action::ZoomReset,
            ),
            CommandDef::new(
                "view.content-zoom-in",
                rust_i18n::t!("cmd_content_zoom_in_title"),
                rust_i18n::t!("cmd_content_zoom_in_desc"),
                AtlasIcon::Plus,
                Action::PaneContentZoomIn,
            ),
            CommandDef::new(
                "view.content-zoom-out",
                rust_i18n::t!("cmd_content_zoom_out_title"),
                rust_i18n::t!("cmd_content_zoom_out_desc"),
                AtlasIcon::ArrowCounterClockwise,
                Action::PaneContentZoomOut,
            ),
            CommandDef::new(
                "folder.toggle-folders-first",
                rust_i18n::t!("cmd_folder_toggle_folders_first_title"),
                rust_i18n::t!("cmd_folder_toggle_folders_first_desc"),
                AtlasIcon::Folder,
                Action::ToggleFoldersFirstFocused,
            ),
            CommandDef::new(
                "folder.toggle-files",
                rust_i18n::t!("cmd_folder_toggle_files_title"),
                rust_i18n::t!("cmd_folder_toggle_files_desc"),
                AtlasIcon::File,
                Action::ToggleShowFilesFocused,
            ),
            CommandDef::new(
                "folder.toggle-folders",
                rust_i18n::t!("cmd_folder_toggle_folders_title"),
                rust_i18n::t!("cmd_folder_toggle_folders_desc"),
                AtlasIcon::Folder,
                Action::ToggleShowFoldersFocused,
            ),
            CommandDef::new(
                "folder.search",
                rust_i18n::t!("cmd_folder_search_title"),
                rust_i18n::t!("cmd_folder_search_desc"),
                AtlasIcon::Search,
                Action::OpenSearchInPlace,
            ),
            CommandDef::new(
                "folder.search-subfolders",
                rust_i18n::t!("cmd_folder_search_subfolders_title"),
                rust_i18n::t!("cmd_folder_search_subfolders_desc"),
                AtlasIcon::Search,
                Action::ToggleSearchInSubfolders,
            ),
        ];

        let edit = vec![
            CommandDef::new(
                "edit.select-all",
                rust_i18n::t!("cmd_edit_select_all_title"),
                rust_i18n::t!("cmd_edit_select_all_desc"),
                AtlasIcon::SelectionAll,
                Action::SelectAll,
            ),
            CommandDef::new(
                "edit.invert",
                rust_i18n::t!("cmd_edit_invert_title"),
                rust_i18n::t!("cmd_edit_invert_desc"),
                AtlasIcon::SelectionInverse,
                Action::InvertSelection,
            ),
            CommandDef::new(
                "edit.undo",
                rust_i18n::t!("cmd_edit_undo_title"),
                rust_i18n::t!("cmd_edit_undo_desc"),
                AtlasIcon::ArrowCounterClockwise,
                Action::Undo,
            ),
            CommandDef::new(
                "edit.redo",
                rust_i18n::t!("cmd_edit_redo_title"),
                rust_i18n::t!("cmd_edit_redo_desc"),
                AtlasIcon::ArrowClockwise,
                Action::Redo,
            ),
            CommandDef::new(
                "edit.batch-rename",
                rust_i18n::t!("cmd_edit_batch_rename_title"),
                rust_i18n::t!("cmd_edit_batch_rename_desc"),
                AtlasIcon::PencilSimple,
                Action::OpenBatchRename,
            ),
        ];

        let selected_paths = self.state.selected_paths();
        let focused_dir = self
            .tab_data_for(focused)
            .and_then(|t| t.location.fs_dir().map(std::path::Path::to_path_buf));
        let archive_focused = self.tab_data_for(focused).is_some_and(|t| t.location.is_archive());
        let recycle_focused = self
            .tab_data_for(focused)
            .is_some_and(|t| t.location == atlas_model::location::Location::RecycleBin);
        // In the recycle bin, trash/delete don't apply to the $R payloads —
        // restore/purge below take their place.
        let mut files = Vec::new();

        if !archive_focused && !recycle_focused {
            if !selected_paths.is_empty() {
                files.push(CommandDef::new(
                    "files.copy",
                    rust_i18n::t!("root_copy"),
                    rust_i18n::t!("cmd_files_copy_desc"),
                    AtlasIcon::Copy,
                    Action::Copy(selected_paths.clone()),
                ));
                files.push(CommandDef::new(
                    "files.cut",
                    rust_i18n::t!("root_cut"),
                    rust_i18n::t!("cmd_files_cut_desc"),
                    AtlasIcon::Scissors,
                    Action::Cut(selected_paths.clone()),
                ));
            }
            if let Some(dir) = &focused_dir {
                files.push(CommandDef::new(
                    "files.paste",
                    rust_i18n::t!("root_paste"),
                    rust_i18n::t!("cmd_files_paste_desc"),
                    AtlasIcon::Clipboard,
                    Action::Paste { dest: dir.clone() },
                ));
            }
        }
        if let Some(dir) = &focused_dir {
            files.push(CommandDef::new(
                "files.open-terminal",
                rust_i18n::t!("cmd_files_open_terminal_title"),
                rust_i18n::t!("cmd_files_open_terminal_desc"),
                AtlasIcon::Lightning,
                Action::OpenTerminal { dir: dir.clone() },
            ));
        }

        if !recycle_focused {
            files.push(
                CommandDef::new(
                    "files.create-folder",
                    rust_i18n::t!("cmd_files_create_folder_title"),
                    rust_i18n::t!("cmd_files_create_folder_desc"),
                    AtlasIcon::Folder,
                    Action::CreateFolderFocused,
                )
                .with_aliases(&[
                    "new folder",
                    "create folder",
                    "nueva carpeta",
                    "crear carpeta",
                    "folder",
                    "carpeta",
                    "mkdir",
                    "new dir",
                ]),
            );
            files.push(
                CommandDef::new(
                    "files.create-file",
                    rust_i18n::t!("cmd_files_create_file_title"),
                    rust_i18n::t!("cmd_files_create_file_desc"),
                    AtlasIcon::File,
                    Action::CreateFileFocused,
                )
                .with_aliases(&[
                    "new file",
                    "create file",
                    "nuevo archivo",
                    "crear archivo",
                    "file",
                    "archivo",
                    "touch",
                ]),
            );
        }

        files.push(
            CommandDef::new(
                "files.open-in-window",
                rust_i18n::t!("cmd_files_open_window_title"),
                rust_i18n::t!("cmd_files_open_window_desc"),
                AtlasIcon::Folder,
                Action::OpenSelectedInNewWindow,
            )
            .with_aliases(&[
                "new window",
                "open in new window",
                "open window",
                "nueva ventana",
                "abrir en nueva ventana",
                "abrir ventana",
                "window",
                "ventana",
            ]),
        );

        files.push(
            CommandDef::new(
                "files.open-dir-right-split",
                rust_i18n::t!("cmd_files_open_right_split_title"),
                rust_i18n::t!("cmd_files_open_right_split_desc"),
                AtlasIcon::SplitHorizontal,
                Action::OpenSelectedDirInRightSplit,
            )
            .with_aliases(&[
                "split right",
                "open in right split",
                "right split",
                "split horizontal",
                "dividir derecha",
                "abrir en division derecha",
                "division derecha",
                "panel derecho",
                "split",
            ]),
        );

        files.push(
            CommandDef::new(
                "files.open-dir-bottom-split",
                rust_i18n::t!("cmd_files_open_bottom_split_title"),
                rust_i18n::t!("cmd_files_open_bottom_split_desc"),
                AtlasIcon::SplitVertical,
                Action::OpenSelectedDirInBottomSplit,
            )
            .with_aliases(&[
                "split bottom",
                "open in bottom split",
                "bottom split",
                "split vertical",
                "dividir abajo",
                "abrir en division inferior",
                "division inferior",
                "panel inferior",
                "split down",
            ]),
        );

        if !selected_paths.is_empty() && !recycle_focused {
            files.push(CommandDef::new(
                "files.convert",
                rust_i18n::t!("convert_title"),
                rust_i18n::t!("convert_description"),
                AtlasIcon::ArrowClockwise,
                Action::OpenConverter,
            ));
            files.push(CommandDef::new(
                "files.rename",
                rust_i18n::t!("cmd_files_rename_title"),
                rust_i18n::t!("cmd_files_rename_desc"),
                AtlasIcon::PencilSimple,
                Action::RenameFocused,
            ));
            files.push(CommandDef::new(
                "files.trash",
                rust_i18n::t!("cmd_files_trash_title"),
                rust_i18n::t!("cmd_files_trash_desc"),
                AtlasIcon::Trash,
                Action::Delete {
                    paths: selected_paths.clone(),
                    to_trash: true,
                },
            ));
            files.push(CommandDef::new(
                "files.delete",
                rust_i18n::t!("cmd_files_delete_title"),
                rust_i18n::t!("cmd_files_delete_desc"),
                AtlasIcon::Trash,
                Action::Delete {
                    paths: selected_paths.clone(),
                    to_trash: false,
                },
            ));
            files.push(CommandDef::new(
                "files.compress",
                rust_i18n::t!("cmd_files_compress_title"),
                rust_i18n::t!("cmd_files_compress_desc"),
                AtlasIcon::File,
                Action::CompressToZip(selected_paths.clone()),
            ));
            files.push(CommandDef::new(
                "files.open-dirs-in-tabs",
                rust_i18n::t!("cmd_files_open_dirs_tabs_title"),
                rust_i18n::t!("cmd_files_open_dirs_tabs_desc"),
                AtlasIcon::Folder,
                Action::OpenSelectedDirsInNewTabs,
            ));
            files.push(CommandDef::new(
                "files.copy-path",
                rust_i18n::t!("cmd_files_copy_path_title"),
                rust_i18n::t!("cmd_files_copy_path_desc"),
                AtlasIcon::Copy,
                Action::CopyPathsToClipboard(selected_paths.clone()),
            ));
            files.push(CommandDef::new(
                "files.properties",
                rust_i18n::t!("cmd_files_properties_title"),
                rust_i18n::t!("cmd_files_properties_desc"),
                AtlasIcon::Gear,
                Action::OpenPropertiesFocused,
            ));
        }
        #[cfg(windows)]
        if self.tab_data_for(focused).is_some_and(|tab| tab.location.fs_dir().is_some()) {
            files.push(CommandDef::new("files.paste-shortcut", rust_i18n::t!("cmd_paste_shortcut_title"),
                rust_i18n::t!("cmd_paste_shortcut_desc"), AtlasIcon::Copy, Action::PasteShortcutFocused));
        }
        if recycle_focused {
            files.push(CommandDef::new(
                "recycle.restore",
                rust_i18n::t!("cmd_recycle_restore_title"),
                rust_i18n::t!("cmd_recycle_restore_desc"),
                AtlasIcon::ArrowCounterClockwise,
                Action::RestoreSelectedFromRecycleBin,
            ));
            files.push(CommandDef::new(
                "recycle.purge",
                rust_i18n::t!("cmd_recycle_purge_title"),
                rust_i18n::t!("cmd_recycle_purge_desc"),
                AtlasIcon::Trash,
                Action::PurgeSelectedFromRecycleBin,
            ));
        }
        files.push(CommandDef::new(
            "recycle.empty",
            rust_i18n::t!("cmd_recycle_empty_title"),
            rust_i18n::t!("cmd_recycle_empty_desc"),
            AtlasIcon::Trash,
            Action::EmptyRecycleBin,
        ));

        let mut profiles: Vec<CommandDef> = self
            .state
            .profiles
            .profiles
            .iter()
            .enumerate()
            .map(|(idx, profile)| {
                CommandDef::new(
                    &format!("profile:{}", profile.name),
                    rust_i18n::t!("cmd_profile_switch_title", name = profile.name.clone()),
                    rust_i18n::t!("cmd_profile_switch_desc"),
                    AtlasIcon::UserSwitch,
                    Action::SwitchProfile(idx),
                )
            })
            .collect();
        // Management commands: create a fresh profile (auto-named, inherits the
        // current view) and delete the active one.
        profiles.push(CommandDef::new(
            "profile.new",
            rust_i18n::t!("cmd_profile_new_title"),
            rust_i18n::t!("cmd_profile_new_desc"),
            AtlasIcon::UserSwitch,
            Action::CreateProfile(String::new()),
        ));
        profiles.push(CommandDef::new(
            "profile.rename",
            rust_i18n::t!(
                "cmd_profile_rename_title",
                name = self.state.profiles.active().name.clone()
            ),
            rust_i18n::t!("cmd_profile_rename_desc"),
            AtlasIcon::PencilSimple,
            Action::OpenRenameProfile,
        ));
        if self.state.profiles.profiles.len() > 1 {
            profiles.push(CommandDef::new(
                "profile.delete",
                rust_i18n::t!(
                    "cmd_profile_delete_title",
                    name = self.state.profiles.active().name.clone()
                ),
                rust_i18n::t!("cmd_profile_delete_desc"),
                AtlasIcon::Trash,
                Action::DeleteActiveProfile,
            ));
        }

        let themes: Vec<CommandDef> = gpui_component::ThemeRegistry::global(cx)
            .sorted_themes()
            .iter()
            .filter(|cfg| crate::app::theme::theme_is_available(cfg.name.as_ref()))
            .map(|cfg| {
                let name = cfg.name.to_string();
                CommandDef::new(
                    &format!("theme:{name}"),
                    rust_i18n::t!("cmd_theme_switch_title", name = name.clone()),
                    rust_i18n::t!("cmd_theme_switch_desc"),
                    AtlasIcon::Palette,
                    Action::SetTheme(name),
                )
            })
            .collect();

        let mut ext = vec![CommandDef::new(
            "ext.manager",
            rust_i18n::t!("ext_manager_title"),
            rust_i18n::t!("ext_manager_desc"),
            AtlasIcon::PuzzlePiece,
            Action::OpenExtManager,
        )];
        // Dynamically register every enabled non-builtin, non-thumbnailer command
        // with prefix `ext.<ext_id>.<cmd_id>` so they appear in the palette.
        for extension in self.ext_registry.extensions.iter().filter(|e| e.enabled) {
            if extension.manifest.hidden
                || matches!(
                    extension.manifest.kind,
                    atlas_model::ext_model::ExtKind::Builtin
                        | atlas_model::ext_model::ExtKind::ToolProvider
                )
            {
                continue;
            }
            for cmd in extension
                .manifest
                .commands
                .iter()
                .filter(|c| !c.thumbnail && !c.preview)
            {
                ext.push(CommandDef::new(
                    &format!("ext.{}.{}", extension.manifest.id, cmd.id),
                    cmd.title.clone(),
                    extension.manifest.name.clone(),
                    AtlasIcon::PuzzlePiece,
                    Action::Ext(crate::app::ext::ExtAction::Run {
                        ext_id: extension.manifest.id.clone(),
                        cmd_id: cmd.id.clone(),
                    }),
                ));
            }
        }

        let system = vec![
            CommandDef::new(
                "sys.palette",
                rust_i18n::t!("atlas_menu_command_palette"),
                "",
                AtlasIcon::Lightning,
                Action::OpenPalette,
            ),
            CommandDef::new(
                "sys.getting-started",
                rust_i18n::t!("cmd_sys_getting_started_title"),
                rust_i18n::t!("cmd_sys_getting_started_desc"),
                AtlasIcon::Lightning,
                Action::OpenGettingStarted,
            ),
            CommandDef::new(
                "sys.guide",
                rust_i18n::t!("cmd_sys_guide_title"),
                rust_i18n::t!("cmd_sys_guide_desc"),
                AtlasIcon::Lifebuoy,
                Action::OpenGuide(0),
            ),
            CommandDef::new(
                "sys.settings",
                rust_i18n::t!("cmd_sys_settings_title"),
                rust_i18n::t!("cmd_sys_settings_desc"),
                AtlasIcon::Gear,
                Action::OpenSettings,
            ),
            CommandDef::new(
                "sys.get-premium",
                rust_i18n::t!("cmd_sys_get_premium_title"),
                rust_i18n::t!("cmd_sys_get_premium_desc"),
                AtlasIcon::Star,
                Action::GetPremium,
            ),
            CommandDef::new(
                "sys.activate-license",
                rust_i18n::t!("cmd_sys_activate_license_title"),
                rust_i18n::t!("cmd_sys_activate_license_desc"),
                AtlasIcon::Lock,
                Action::ActivateLicense,
            ),
            CommandDef::new(
                "sys.deactivate-license",
                rust_i18n::t!("cmd_sys_deactivate_license_title"),
                rust_i18n::t!("cmd_sys_deactivate_license_desc"),
                AtlasIcon::Lock,
                Action::DeactivateLicense,
            ),
            CommandDef::new(
                "sys.save-session",
                rust_i18n::t!("cmd_sys_save_session_title"),
                rust_i18n::t!("cmd_sys_save_session_desc"),
                AtlasIcon::Download,
                Action::SaveSession,
            ),
        ];

        vec![
            (
                "tabs",
                rust_i18n::t!("palette_cat_tabs").into(),
                AtlasIcon::SplitHorizontal,
                tabs,
            ),
            (
                "nav",
                rust_i18n::t!("palette_cat_nav").into(),
                AtlasIcon::ArrowsLeftRight,
                nav,
            ),
            (
                "selection",
                rust_i18n::t!("palette_cat_selection").into(),
                AtlasIcon::SelectionAll,
                selection,
            ),
            (
                "view",
                rust_i18n::t!("palette_cat_view").into(),
                AtlasIcon::Eye,
                view,
            ),
            (
                "edit",
                rust_i18n::t!("palette_cat_edit").into(),
                AtlasIcon::PencilSimple,
                edit,
            ),
            (
                "files",
                rust_i18n::t!("palette_cat_files").into(),
                AtlasIcon::Trash,
                files,
            ),
            (
                "profiles",
                rust_i18n::t!("palette_cat_profiles").into(),
                AtlasIcon::UserSwitch,
                profiles,
            ),
            (
                "themes",
                rust_i18n::t!("palette_cat_themes").into(),
                AtlasIcon::Palette,
                themes,
            ),
            (
                "ext",
                rust_i18n::t!("palette_cat_ext").into(),
                AtlasIcon::PuzzlePiece,
                ext,
            ),
            (
                "system",
                rust_i18n::t!("palette_cat_system").into(),
                AtlasIcon::Gear,
                system,
            ),
        ]
    }
}

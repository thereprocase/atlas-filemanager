//! Atlas–style right-click menu: a searchable overlay combining Atlas's own
//! commands (top) with the native Windows shell context menu (Open with, Send to,
//! 7-Zip, Properties, …) enumerated by the [`crate::shell_menu`] worker.
//!
//! The Atlas section is cross-platform; native items only appear on Windows.
//! Atlas rows dispatch the same boxed gpui actions the old `PopupMenu` used, so
//! all existing wiring is reused. Native rows are invoked through the worker.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use atlas_model::ext_model::CommandRef;

use crate::ui::imm::{motion::RetainedSpring, ImmTextInputState};
use gpui::{
    div, px, AnyElement, Context, FocusHandle, InteractiveElement, IntoElement, KeyDownEvent,
    MouseButton, ParentElement, Pixels, Point, RenderImage, Styled, Window,
};
use gpui_component::{kbd::Kbd, ActiveTheme};

use crate::app::App;
use crate::input::action::Action;
use crate::ui::icons::AtlasIcon;

#[cfg(feature = "ui-lab")]
pub mod diagnostic;

/// One native shell entry, ready for rendering (icon already a GPU texture).
#[derive(Clone)]
pub(crate) struct UiNativeItem {
    pub id: u32,
    pub title: String,
    pub verb: Option<String>,
    pub icon: Option<Arc<RenderImage>>,
    pub kind: UiItemKind,
    pub disabled: bool,
    pub is_default: bool,
}

// Shell command ids are bounded; reserve one local id for the combined group.
const OPEN_WITH_GROUP: u32 = u32::MAX;

fn native_open_with(item: &UiNativeItem) -> bool {
    if item.verb.as_deref().is_some_and(|verb| verb.eq_ignore_ascii_case("openwith")) {
        return true;
    }
    let label = item.title.replace('&', "").trim().trim_end_matches(['.', '…']).to_lowercase();
    ["open with", "abrir con"].iter().any(|prefix| label == *prefix || label.starts_with(&format!("{prefix} ")))
}

#[derive(Clone)]
pub(crate) enum UiItemKind {
    Normal,
    Separator,
    Submenu { children: Option<Vec<UiNativeItem>> },
}

/// Last-known native items per selection kind, so the menu opens instantly with
/// the previous result while the real (slow) shell build refreshes it.
#[cfg(windows)]
#[derive(Default)]
pub(crate) struct NativeMenuCache {
    items: HashMap<String, Vec<UiNativeItem>>,
    /// Leaf clicked while the list was still a cached guess: `(menu_id, title,
    /// screen_pt)`. Invoked by title once the real `Built` arrives.
    pending: Option<(u64, String, (i32, i32))>,
}

#[cfg(windows)]
const NATIVE_CACHE_MAX: usize = 64;

/// Cache key for a selection: one token per path (`dir` or `.ext`), sorted.
/// `None` for huge selections (avoids a stat per path).
#[cfg(windows)]
fn native_cache_key(paths: &[PathBuf], background: bool) -> Option<String> {
    if background {
        return Some("bg".into());
    }
    if paths.is_empty() || paths.len() > 16 {
        return None;
    }
    let mut kinds: Vec<String> = paths
        .iter()
        .map(|p| {
            if p.is_dir() {
                "dir".to_string()
            } else {
                p.extension()
                    .map(|e| format!(".{}", e.to_string_lossy().to_lowercase()))
                    .unwrap_or_default()
            }
        })
        .collect();
    kinds.sort();
    kinds.dedup();
    Some(format!("sel|{}", kinds.join(",")))
}

/// State for the open right-click menu. `None` on `App` when closed.
pub(crate) struct CtxMenuState {
    /// Monotonic id; stale `Built` events from a prior open are ignored.
    pub menu_id: u64,
    pub focus: FocusHandle,
    pub input: ImmTextInputState,
    pub scroll_top: f32,
    pub scroll_motion: RetainedSpring,
    pub input_bounds: Option<gpui::Bounds<Pixels>>,
    pub input_line: Option<gpui::ShapedLine>,
    pub input_scroll_x: f32,
    pub input_drag: bool,
    pub scroll_drag: Option<f32>,
    pub blink_visible: bool,
    /// One timeline per opening; search, hover and native results never replay it.
    pub opened_at: Instant,
    /// Top-left of the panel, in window-logical pixels.
    pub anchor: Point<Pixels>,
    /// Physical screen pixels for the native `InvokeCommand` (`ptInvoke`).
    pub screen_pt: (i32, i32),
    // reason: retained for a future "rebuild menu" path; not read yet.
    #[allow(dead_code)]
    pub paths: Vec<PathBuf>,
    pub native: Vec<UiNativeItem>,
    /// True until the worker's `Built` event arrives (shows a skeleton row).
    pub loading: bool,
    /// `native` is a cached guess from a previous open: command ids are not valid
    /// until the real `Built` replaces it.
    pub stale: bool,
    /// Key under which the real result is cached (Windows).
    pub cache_key: Option<String>,
    /// Keyboard cursor into the flattened list of selectable rows.
    pub selected: usize,
    /// Keyboard navigation requested that the selected row be scrolled into view.
    pub scroll_to_selected: bool,
    /// Inline-expanded submenu ids (no query / tree mode).
    pub expanded: HashSet<u32>,
    /// True when opened on empty space (background), not on a file selection.
    pub background: bool,
    /// Extension commands that apply to the current selection (prefix `ext.`).
    pub ext_cmds: Vec<CommandRef>,
    /// Associated system desktop applications for "Open with..."
    pub desktop_apps: Vec<atlas_core::fs::desktop_apps::DesktopApp>,
}

/// Atlas's own commands. Each maps to a label, icon and a boxed gpui action that
/// the root `Atlas` key context handles against the current selection.
#[derive(Clone, Copy, PartialEq)]
enum AtlasCmd {
    DiskCleanup,
    ProtectFolder,
    UnlockFolder,
    Convert,
    Open,
    Rename,
    Copy,
    Cut,
    Paste,
    #[cfg(windows)]
    PasteShortcut,
    Trash,
    DeletePerm,
    Restore,
    Properties,
    NewFolder,
    NewFile,
    OpenTerminal,
    Refresh,
    SelectAll,
}

impl AtlasCmd {
    fn label(self) -> String {
        match self {
            Self::DiskCleanup => rust_i18n::t!("cleanup_open").to_string(),
            Self::ProtectFolder => rust_i18n::t!("vault_context_protect").to_string(),
            Self::UnlockFolder => rust_i18n::t!("vault_context_unlock").to_string(),
            Self::Convert => rust_i18n::t!("convert_title").to_string(),
            Self::Open => rust_i18n::t!("root_open").to_string(),
            Self::Rename => rust_i18n::t!("root_rename").to_string(),
            Self::Copy => rust_i18n::t!("root_copy").to_string(),
            Self::NewFolder => rust_i18n::t!("root_new_folder").to_string(),
            Self::NewFile => rust_i18n::t!("cmd_files_create_file_title").to_string(),
            Self::OpenTerminal => rust_i18n::t!("cmd_files_open_terminal_title").to_string(),
            Self::Refresh => rust_i18n::t!("cmd_nav_refresh_title").to_string(),
            Self::SelectAll => rust_i18n::t!("root_select_all").to_string(),
            Self::Cut => rust_i18n::t!("root_cut").to_string(),
            Self::Paste => rust_i18n::t!("root_paste").to_string(),
            #[cfg(windows)]
            Self::PasteShortcut => rust_i18n::t!("cmd_paste_shortcut_title").to_string(),
            Self::Trash => rust_i18n::t!("root_move_to_trash").to_string(),
            Self::DeletePerm => rust_i18n::t!("root_delete_permanently").to_string(),
            Self::Restore => rust_i18n::t!("root_restore").to_string(),
            Self::Properties => rust_i18n::t!("root_properties").to_string(),
        }
    }
    fn icon(self) -> Option<AtlasIcon> {
        match self {
            Self::DiskCleanup => Some(AtlasIcon::HardDrive),
            Self::ProtectFolder | Self::UnlockFolder => Some(AtlasIcon::Lock),
            Self::Convert => Some(AtlasIcon::ArrowClockwise),
            Self::Open => Some(AtlasIcon::Eye),
            Self::Rename => Some(AtlasIcon::PencilSimple),
            Self::Copy => Some(AtlasIcon::Copy),
            Self::Cut => Some(AtlasIcon::Scissors),
            Self::Paste => Some(AtlasIcon::Clipboard),
            #[cfg(windows)]
            Self::PasteShortcut => Some(AtlasIcon::Copy),
            Self::Trash => Some(AtlasIcon::Trash),
            Self::DeletePerm => Some(AtlasIcon::X),
            Self::Restore => Some(AtlasIcon::ArrowCounterClockwise),
            Self::Properties => Some(AtlasIcon::Gear),
            Self::NewFolder => Some(AtlasIcon::Folder),
            Self::NewFile => Some(AtlasIcon::File),
            Self::OpenTerminal => Some(AtlasIcon::Lightning),
            Self::Refresh => Some(AtlasIcon::ArrowClockwise),
            Self::SelectAll => Some(AtlasIcon::SelectionAll),
        }
    }

    fn command_id(self) -> &'static str {
        match self {
            Self::DiskCleanup => "disk.cleanup",
            Self::ProtectFolder => "vault.protect",
            Self::UnlockFolder => "vault.unlock",
            Self::Convert => "files.convert",
            Self::Open => "selection.open",
            Self::Rename => "files.rename",
            Self::Copy => "files.copy",
            Self::Cut => "files.cut",
            Self::Paste => "files.paste",
            #[cfg(windows)]
            Self::PasteShortcut => "files.paste-shortcut",
            Self::Trash => "files.trash",
            Self::DeletePerm => "files.delete",
            Self::Refresh => "nav.refresh",
            Self::SelectAll => "edit.select-all",
            Self::Restore => "recycle.restore",
            Self::Properties => "files.properties",
            Self::NewFolder => "files.create-folder",
            Self::NewFile => "files.create-file",
            Self::OpenTerminal => "files.open-terminal",
        }
    }
}

/// Atlas items for normal directory views. `true` = separator follows the item.
const ATLAS_ITEMS: &[(AtlasCmd, bool)] = &[
    (AtlasCmd::Open, false),
    (AtlasCmd::Convert, false),
    (AtlasCmd::Rename, true),
    (AtlasCmd::Copy, false),
    (AtlasCmd::Cut, false),
    (AtlasCmd::Paste, true),
    #[cfg(windows)]
    (AtlasCmd::PasteShortcut, true),
    (AtlasCmd::Trash, false),
    (AtlasCmd::DeletePerm, true),
    (AtlasCmd::Properties, false),
];

/// Atlas items shown when the current tab is the Recycle Bin.
const ATLAS_ITEMS_RECYCLE: &[(AtlasCmd, bool)] = &[
    (AtlasCmd::Restore, false),
    (AtlasCmd::Cut, false),
    (AtlasCmd::DeletePerm, true),
    (AtlasCmd::Properties, false),
];

/// Atlas items shown on right-click on empty background (no file selection).
const ATLAS_ITEMS_BACKGROUND: &[(AtlasCmd, bool)] = &[
    (AtlasCmd::NewFile, false),
    (AtlasCmd::NewFolder, false),
    (AtlasCmd::OpenTerminal, true),
    (AtlasCmd::Paste, false),
    #[cfg(windows)]
    (AtlasCmd::PasteShortcut, false),
    (AtlasCmd::Refresh, false),
    (AtlasCmd::SelectAll, false),
];

/// A logical (non-element) row, recomputed each frame and shared by the renderer
/// and the keyboard handler so selection indices always agree.
#[derive(Clone)]
enum CtxRow {
    Separator,
    Loading,
    Atlas {
        cmd: AtlasCmd,
    },
    Ext {
        ext_id: String,
        cmd_id: String,
        title: String,
    },
    Native {
        id: u32,
        title: String,
        icon: Option<Arc<RenderImage>>,
        depth: u8,
        submenu: bool,
        expanded: bool,
        disabled: bool,
        is_default: bool,
    },
    DesktopApp {
        app: atlas_core::fs::desktop_apps::DesktopApp,
        depth: u8,
    },
}

impl CtxRow {
    fn selectable(&self) -> bool {
        matches!(
            self,
            CtxRow::Atlas { .. }
                | CtxRow::Ext { .. }
                | CtxRow::Native { .. }
                | CtxRow::DesktopApp { .. }
        )
    }
}

impl App {
    /// Open the right-click menu for `paths` anchored at `anchor` (window-logical
    /// px). Sends a `Build` request to the shell worker (Windows).
    pub(crate) fn open_ctx_menu(
        &mut self,
        paths: Vec<PathBuf>,
        anchor: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.suppress_next_shell_ctx_menu {
            self.suppress_next_shell_ctx_menu = false;
            self.close_ctx_menu(cx);
            return;
        }
        if paths.is_empty() {
            return;
        }
        self.close_type_filter_menu(cx);
        self.close_breadcrumb_menu(cx);
        self.ctx_menu_seq += 1;
        let menu_id = self.ctx_menu_seq;

        // Physical screen pixels for the eventual native invoke.
        let screen_pt = cursor_screen_pos();

        // Same-parent grouping: the native shell menu needs one parent folder.
        // If the selection spans parents we still show Atlas items (native empty).
        let same_parent = paths
            .first()
            .and_then(|p| p.parent())
            .map(|p0| paths.iter().all(|p| p.parent() == Some(p0)))
            .unwrap_or(false);

        let is_recycle_bin = self
            .state
            .active_tab_data()
            .is_some_and(|t| matches!(t.location, atlas_model::location::Location::RecycleBin));

        #[cfg(windows)]
        let loading = {
            // Skip the native shell Build for RecycleBin items: the $R paths are not
            // valid shell namespace objects and the native menu would duplicate Restore.
            if same_parent && !is_recycle_bin && !paths.iter().any(|p| atlas_core::vault::is_private_path(p)) {
                if let Some(handle) = &self.shell_menu {
                    handle.send(crate::shell_menu::ShellMenuRequest::Build {
                        id: menu_id,
                        paths: paths.clone(),
                    });
                    true
                } else {
                    false
                }
            } else {
                false
            }
        };
        #[cfg(not(windows))]
        let loading = {
            let _ = (same_parent, is_recycle_bin);
            false
        };

        #[cfg(windows)]
        let cache_key = if loading { native_cache_key(&paths, false) } else { None };
        #[cfg(not(windows))]
        let cache_key: Option<String> = None;
        #[cfg(windows)]
        let (native, loading, stale) = match cache_key
            .as_ref()
            .and_then(|k| self.ctx_native.items.get(k))
        {
            Some(cached) if loading => (cached.clone(), false, true),
            _ => (Vec::new(), loading, false),
        };
        #[cfg(not(windows))]
        let (native, stale): (Vec<UiNativeItem>, bool) = (Vec::new(), false);

        let selection: Vec<(Option<String>, bool)> = paths
            .iter()
            .map(|p| {
                (
                    p.extension().map(|e| e.to_string_lossy().to_lowercase()),
                    false,
                )
            })
            .collect();
        let ext_cmds = if paths.iter().any(|p| atlas_core::vault::is_private_path(p)) {
            Vec::new()
        } else {
            self.ext_registry.context_commands_for(&selection)
        };
        let desktop_apps = if paths.is_empty() || is_recycle_bin || paths.iter().any(|p| atlas_core::vault::is_private_path(p)) {
            Vec::new()
        } else {
            paths
                .first()
                .map(|p| atlas_core::fs::desktop_apps::apps_for_file(p))
                .unwrap_or_default()
        };

        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        self.ctx_menu = Some(CtxMenuState {
            menu_id,
            focus,
            input: ImmTextInputState::new(""),
            scroll_top: 0.0,
            scroll_motion: RetainedSpring::new(0.0, Instant::now()),
            input_bounds: None,
            input_line: None,
            input_scroll_x: 0.0,
            input_drag: false,
            scroll_drag: None,
            blink_visible: true,
            opened_at: Instant::now(),
            anchor,
            screen_pt,
            paths,
            native,
            loading,
            stale,
            cache_key,
            selected: 0,
            scroll_to_selected: true,
            expanded: HashSet::new(),
            background: false,
            ext_cmds,
            desktop_apps,
        });
        // Cached pane content must drop its tooltip builder even if the pointer
        // has not moved (including a menu opened from the keyboard).
        self.notify_all_panes(cx);
        cx.notify();
    }

    /// Close the menu, dismissing the worker's COM objects (Windows).
    pub(crate) fn close_ctx_menu(&mut self, cx: &mut Context<Self>) {
        if self.ctx_menu.is_some() {
            self.cancel_command_shortcut_capture();
        }
        if let Some(state) = self.ctx_menu.take() {
            #[cfg(windows)]
            if let Some(handle) = &self.shell_menu {
                handle.send(crate::shell_menu::ShellMenuRequest::Dismiss { id: state.menu_id });
            }
            let _ = state;
        }

        cx.notify();
    }

    /// The existing foreground drain drives only blink edges, not continuous frames.
    pub(crate) fn tick_ctx_input_blink(&mut self) -> bool {
        let Some(menu) = self.ctx_menu.as_mut() else {
            return false;
        };
        let visible = self.command_shortcut_capture.is_none()
            && (menu.input.blink_epoch.elapsed().as_millis() / 530) % 2 == 0;
        let changed = visible != menu.blink_visible;
        menu.blink_visible = visible;
        changed
    }

    /// Drain shell-worker events into the open menu. Returns whether anything
    /// changed (so the event-driven drain repaints). Windows-only.
    #[cfg(windows)]
    pub(crate) fn drain_shell_menu_events(&mut self, cx: &mut Context<Self>) -> bool {
        use crate::shell_menu::ShellMenuEvent;
        let Some(handle) = self.shell_menu.clone() else {
            return false;
        };
        let events = handle.drain_events();
        if events.is_empty() {
            return false;
        }
        let mut dirty = false;
        for ev in events {
            match ev {
                ShellMenuEvent::Built { id, mut items } => {
                    // A leaf clicked on the cached guess: run it now by title.
                    if self.ctx_native.pending.as_ref().is_some_and(|p| p.0 == id) {
                        if let Some((_, title, screen_pt)) = self.ctx_native.pending.take() {
                            let hit = items.iter().find(|i| {
                                matches!(i.kind, crate::shell_menu::NativeItemKind::Normal)
                                    && !i.disabled
                                    && i.title == title
                            });
                            match hit {
                                Some(item) => handle.send(
                                    crate::shell_menu::ShellMenuRequest::Invoke {
                                        id,
                                        item_id: item.id,
                                        screen_pt,
                                    },
                                ),
                                None => handle
                                    .send(crate::shell_menu::ShellMenuRequest::Dismiss { id }),
                            }
                        }
                        continue;
                    }
                    let stale = self.ctx_menu.as_ref().map(|m| m.menu_id) != Some(id);
                    if stale {
                        handle.send(crate::shell_menu::ShellMenuRequest::Dismiss { id });
                        continue;
                    }
                    if self.ctx_menu.as_ref().is_some_and(|menu| !menu.background) {
                        // Only the selection menu provides Atlas Properties.
                        // Keep the native action in background menus and submenus.
                        items.retain(|item| !native_properties_item(item));
                    }
                    let ui = native_items_to_ui(items, cx);
                    let key = self.ctx_menu.as_ref().and_then(|m| m.cache_key.clone());
                    if let (Some(key), false) = (key, ui.is_empty()) {
                        if self.ctx_native.items.len() >= NATIVE_CACHE_MAX {
                            self.ctx_native.items.clear();
                        }
                        self.ctx_native.items.insert(key, ui.clone());
                    }
                    if let Some(m) = self.ctx_menu.as_mut() {
                        // Command ids changed: carry expanded submenus over by title.
                        let mut reopen = Vec::new();
                        let open_with_expanded = m.expanded.contains(&OPEN_WITH_GROUP);
                        if m.stale {
                            expanded_titles(&m.native, &m.expanded, &mut reopen);
                            m.expanded.clear();
                        }
                        m.native = ui;
                        if open_with_expanded { m.expanded.insert(OPEN_WITH_GROUP); }
                        m.loading = false;
                        m.stale = false;
                        if open_with_expanded {
                            for item in m.native.iter().filter(|item| native_open_with(item) && submenu_children_missing(std::slice::from_ref(item), item.id)) {
                                handle.send(crate::shell_menu::ShellMenuRequest::ExpandSubmenu { id: m.menu_id, item_id: item.id });
                            }
                        }
                        for title in reopen {
                            if let Some(sub) = submenu_id_by_title(&m.native, &title) {
                                m.expanded.insert(sub);
                                if submenu_children_missing(&m.native, sub) {
                                    handle.send(crate::shell_menu::ShellMenuRequest::ExpandSubmenu {
                                        id: m.menu_id,
                                        item_id: sub,
                                    });
                                }
                            }
                        }
                    }
                    dirty = true;
                }
                ShellMenuEvent::Prewarmed { key, items } => {
                    let selection: Vec<_> = items
                        .iter()
                        .filter(|item| !native_properties_item(item))
                        .cloned()
                        .collect();
                    let sel_key = format!("sel|{key}");
                    if !self.ctx_native.items.contains_key(&sel_key) {
                        let ui = native_items_to_ui(selection, cx);
                        self.ctx_native.items.insert(sel_key, ui);
                    }
                    // A folder build doubles as the background (empty-space) menu.
                    if key == "dir" && !self.ctx_native.items.contains_key("bg") {
                        let ui = native_items_to_ui(items, cx);
                        self.ctx_native.items.insert("bg".into(), ui);
                    }
                }
                ShellMenuEvent::SubmenuExpanded { id, item_id, items } => {
                    if self.ctx_menu.as_ref().map(|m| m.menu_id) != Some(id) {
                        continue;
                    }
                    let ui = native_items_to_ui(items, cx);
                    if let Some(m) = self.ctx_menu.as_mut() {
                        let mut slot = Some(ui);
                        fill_submenu(&mut m.native, item_id, &mut slot);
                    }
                    dirty = true;
                }
                ShellMenuEvent::InvokeDone { id, result } => {
                    if let Err(e) = result {
                        tracing::warn!(menu_id = id, "shell invoke failed: {e}");
                    }
                    handle.send(crate::shell_menu::ShellMenuRequest::Dismiss { id });
                    if self.ctx_menu.as_ref().map(|m| m.menu_id) == Some(id) {
                        self.ctx_menu = None;
                    }
                    dirty = true;
                }
            }
        }
        if dirty {
            cx.notify();
        }
        dirty
    }

    /// The focus handle of the context menu's search input, for `sync_overlay_focus`.
    pub(crate) fn ctx_menu_focus(&self, cx: &Context<Self>) -> Option<FocusHandle> {
        let _ = cx;
        self.ctx_menu.as_ref().map(|menu| menu.focus.clone())
    }

    /// Build this frame's logical rows from the current query + native tree.
    fn ctx_menu_rows(&self, query: &str, scorer_query: &str) -> Vec<CtxRow> {
        let Some(state) = self.ctx_menu.as_ref() else {
            return Vec::new();
        };
        let is_recycle_bin = self
            .state
            .active_tab_data()
            .is_some_and(|t| matches!(t.location, atlas_model::location::Location::RecycleBin));
        let mut atlas_items = if state.background {
            ATLAS_ITEMS_BACKGROUND
        } else if is_recycle_bin {
            ATLAS_ITEMS_RECYCLE
        } else {
            ATLAS_ITEMS
        }.to_vec();
        let drive_root = cfg!(windows)
            && !state.background
            && state.paths.len() == 1
            && atlas_core::cleanup::is_drive_root(&state.paths[0]);
        if drive_root {
            atlas_items = vec![
                (AtlasCmd::Open, false),
                (AtlasCmd::DiskCleanup, true),
                (AtlasCmd::Properties, false),
            ];
        }
        if !state.background && !is_recycle_bin && !drive_root {
            if state.paths.iter().any(|p| atlas_core::vault::is_private_path(p)) {
                atlas_items = vec![(AtlasCmd::UnlockFolder, false)];
            } else if state.paths.len() == 1 && state.paths[0].is_dir()
                && self.state.active_tab_data().is_some_and(|t| !t.location.is_archive()) {
                atlas_items.insert(2, (AtlasCmd::ProtectFolder, false));
            }
        }

        let q = query.trim();
        let rows = if q.is_empty() {
            // Tree mode: Atlas section, separator, native tree (loading skeleton).
            let mut rows = Vec::new();
            for (cmd, sep_after) in &atlas_items {
                rows.push(CtxRow::Atlas { cmd: *cmd });
                if *cmd == AtlasCmd::Open && (!state.desktop_apps.is_empty() || state.native.iter().any(native_open_with)) {
                    let expanded = state.expanded.contains(&OPEN_WITH_GROUP);
                    rows.push(CtxRow::Native { id: OPEN_WITH_GROUP, title: rust_i18n::t!("open_with").to_string(), icon: None,
                        depth: 0, submenu: true, expanded, disabled: false, is_default: false });
                    if expanded {
                        for app in &state.desktop_apps {
                            rows.push(CtxRow::DesktopApp { app: app.clone(), depth: 1 });
                        }
                        for native in state.native.iter().filter(|item| native_open_with(item)) {
                            match &native.kind {
                                UiItemKind::Submenu { children: Some(children) } => push_native_tree(children, 1, &state.expanded, &mut rows),
                                UiItemKind::Submenu { children: None } => rows.push(CtxRow::Loading),
                                _ => push_native_tree(std::slice::from_ref(native), 1, &state.expanded, &mut rows),
                            }
                        }
                    }
                }
                if *sep_after {
                    rows.push(CtxRow::Separator);
                }
            }
            if !state.ext_cmds.is_empty() {
                rows.push(CtxRow::Separator);
                for cmd in &state.ext_cmds {
                    rows.push(CtxRow::Ext {
                        ext_id: cmd.ext_id.clone(),
                        cmd_id: cmd.command.id.clone(),
                        title: cmd.command.title.clone(),
                    });
                }
            }
            if !state.native.is_empty() || state.loading {
                rows.push(CtxRow::Separator);
            }
            let native: Vec<_> = state.native.iter().filter(|item| state.background || !native_open_with(item)).cloned().collect();
            push_native_tree(&native, 0, &state.expanded, &mut rows);
            if state.loading {
                rows.push(CtxRow::Loading);
            }
            rows
        } else {
            // Search mode: flatten every leaf (Atlas + native + ext + apps), rank, emit flat.
            let _ = scorer_query;
            let mut cand_titles: Vec<String> = Vec::new();
            let mut cands: Vec<CtxRow> = Vec::new();

            for (cmd, _) in &atlas_items {
                cand_titles.push(cmd.label().to_string());
                cands.push(CtxRow::Atlas { cmd: *cmd });
            }
            for app in &state.desktop_apps {
                cand_titles.push(format!("{} {}", rust_i18n::t!("open_with"), app.name));
                cands.push(CtxRow::DesktopApp { app: app.clone(), depth: 0 });
            }
            for cmd in &state.ext_cmds {
                cand_titles.push(cmd.command.title.clone());
                cands.push(CtxRow::Ext {
                    ext_id: cmd.ext_id.clone(),
                    cmd_id: cmd.command.id.clone(),
                    title: cmd.command.title.clone(),
                });
            }
            collect_native_leaves(&state.native, "", &mut cand_titles, &mut cands);

            // Rank via the shared fuzzy matcher (best first). The scorer needs
            // &mut self, which we don't have here; ranking is done in the caller.
            let order = rank_titles(q, &cand_titles);
            order
                .into_iter()
                .map(|i| std::mem::replace(&mut cands[i], CtxRow::Separator))
                .filter(|r| r.selectable())
                .collect()
        };
        sanitize_separators(rows)
    }

    /// Render the context-menu overlay layer (scrim + anchored panel).
    pub(crate) fn render_ctx_menu_layer(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (fg, muted, border, accent, accent_fg, bg, radius) = {
            let t = cx.theme();
            (
                t.popover_foreground,
                t.muted_foreground,
                t.border,
                t.accent,
                t.accent_foreground,
                t.popover,
                f32::from(t.radius),
            )
        };
        let query = self
            .ctx_menu
            .as_ref()
            .map(|menu| menu.input.text.clone())
            .unwrap_or_default();

        let rows = self.ctx_menu_rows(&query, &query);
        let selectable_count = rows.iter().filter(|row| row.selectable()).count();
        if let Some(state) = self.ctx_menu.as_mut() {
            if selectable_count == 0 {
                state.selected = 0;
            } else if state.selected >= selectable_count {
                state.selected = selectable_count - 1;
                state.scroll_to_selected = true;
            }
        }
        let state = self.ctx_menu.as_ref();
        let reveal_ms = if self.state.settings.disable_animations {
            f32::MAX
        } else {
            state.map_or(f32::MAX, |s| s.opened_at.elapsed().as_secs_f32() * 1000.0)
        };
        let reveal_duration = crate::ui::motion::MENU_REVEAL_MS
            + crate::ui::motion::MENU_STAGGER_MS * crate::ui::motion::MENU_STAGGER_CAP as u64;
        if reveal_ms < reveal_duration as f32 {
            window.request_animation_frame();
        }
        let (anchor, selected) = state
            .map(|s| (s.anchor, s.selected))
            .unwrap_or((Point::default(), 0));

        // Clamp the panel into the window so it never spills off an edge.
        let win = window.bounds().size;
        let zoom = f32::from(window.rem_size()) / crate::ui::BASE_PX;
        let width = (340.0 * zoom).min((f32::from(win.width) - 12.0).max(1.0));
        let (ax, ay) = (f32::from(anchor.x), f32::from(anchor.y));
        let (ww, wh) = (f32::from(win.width), f32::from(win.height));
        const MAX_H: f32 = 640.;
        const V_MARGIN: f32 = 72.;
        let input_h = 40.0 * zoom;
        let content_h = (rows.iter().map(ctx_row_height).sum::<f32>() + 8.0) * zoom;
        let max_h = MAX_H.min((wh - V_MARGIN * 2.0).max(180.0));
        // Include the bottom border so a fitting list does not acquire a
        // one-pixel overflow and paint an unnecessary full-height scrollbar.
        let est_h = (content_h + input_h + zoom).min(max_h);
        let list_h = est_h - input_h;
        let left = ax.min((ww - width - 6.0).max(6.0)).max(6.0);
        let top = ay.min((wh - est_h - 6.0).max(6.0)).max(6.0);

        let view = cx.entity();

        // Logical rows remain the command authority; IMM only receives ink and ids.
        let selected_child_idx = rows
            .iter()
            .enumerate()
            .filter(|(_, row)| row.selectable())
            .nth(selected)
            .map(|(index, _)| index);
        let mut ink = Vec::with_capacity(rows.len());
        for row in &rows {
            use crate::ui::imm::menu::{MenuIcon, MenuRow, MenuRowKind};
            let mut item = MenuRow {
                kind: MenuRowKind::Item,
                label: "".into(),
                icon: None,
                shortcut: None,
                shortcut_editable: false,
                shortcut_recording: false,
                depth: 0,
                disabled: false,
                emphasized: false,
                submenu: false,
                expanded: false,
            };
            match row {
                CtxRow::Separator => item.kind = MenuRowKind::Separator,
                CtxRow::Loading => {
                    item.kind = MenuRowKind::Loading;
                    item.label = rust_i18n::t!("palette_searching").to_string().into();
                }
                CtxRow::Atlas { cmd } => {
                    item.label = cmd.label().to_string().into();
                    item.icon = cmd.icon().map(MenuIcon::Theme);
                    let command_id = cmd.command_id();
                    item.shortcut_editable = crate::input::commands::command_remappable(command_id);
                    item.shortcut_recording =
                        self.command_shortcut_capture.as_deref() == Some(command_id);
                    item.shortcut = Some(if item.shortcut_recording {
                        rust_i18n::t!("palette_shortcut_recording").into()
                    } else if let Some(key) = self.command_shortcut_keystroke(command_id) {
                        Kbd::format(&key).into()
                    } else {
                        rust_i18n::t!("palette_shortcut_none").into()
                    });
                }
                CtxRow::Ext { title, .. } => {
                    item.label = title.clone().into();
                    item.icon = Some(MenuIcon::Theme(AtlasIcon::PuzzlePiece));
                }
                CtxRow::Native {
                    id,
                    title,
                    icon,
                    depth,
                    submenu,
                    expanded,
                    disabled,
                    is_default,
                    ..
                } => {
                    item.label = title.clone().into();
                    item.icon = if *id == OPEN_WITH_GROUP {
                        Some(MenuIcon::Theme(AtlasIcon::Grid))
                    } else {
                        icon.clone().map(MenuIcon::Image)
                    };
                    item.depth = *depth;
                    item.submenu = *submenu;
                    item.expanded = *expanded;
                    item.disabled = *disabled;
                    item.emphasized = *is_default;
                }
                CtxRow::DesktopApp { app, depth } => {
                    item.label = if *depth == 0 { format!("{} {}", rust_i18n::t!("open_with"), app.name) } else { app.name.clone() }.into();
                    item.depth = *depth;
                    item.icon = Some(MenuIcon::Theme(AtlasIcon::Eye));
                }
            }
            ink.push(item);
        }
        // The list now has a single layout child. Keyboard visibility uses pixel slots.
        if self
            .ctx_menu
            .as_ref()
            .is_some_and(|state| state.scroll_to_selected)
        {
            if let Some(index) = selected_child_idx {
                let row_top =
                    4.0 * zoom + ink.iter().take(index).map(|r| r.height(zoom)).sum::<f32>();
                let row_bottom = row_top + ink[index].height(zoom);
                let scroll_top = self.ctx_menu.as_ref().map_or(0.0, |menu| menu.scroll_top);
                let viewport = (list_h - zoom).max(1.0);
                let next = crate::ui::imm::menu::visible_scroll_top(
                    scroll_top, viewport, row_top, row_bottom,
                );
                if let Some(menu) = self.ctx_menu.as_mut() {
                    menu.scroll_top = next;
                }
            }
            if let Some(state) = self.ctx_menu.as_mut() {
                state.scroll_to_selected = false;
            }
        }
        let menu_id = self.ctx_menu.as_ref().map_or(0, |state| state.menu_id);
        let snapshot = Rc::new(rows);
        let hover_rows = snapshot.clone();
        let hover_view = view.clone();
        let on_hover = Rc::new(
            move |index: usize, _window: &mut Window, cx: &mut gpui::App| {
                let ordinal = hover_rows
                    .iter()
                    .take(index)
                    .filter(|row| row.selectable())
                    .count();
                hover_view.update(cx, |this, cx| {
                    if let Some(state) = this
                        .ctx_menu
                        .as_mut()
                        .filter(|state| state.menu_id == menu_id)
                    {
                        if state.selected != ordinal {
                            state.selected = ordinal;
                            cx.notify();
                        }
                    }
                });
            },
        );
        let activate_view = view.clone();
        let edit_rows = snapshot.clone();
        let edit_view = view.clone();
        let on_edit_shortcut = Rc::new(
            move |index: usize, _window: &mut Window, cx: &mut gpui::App| {
                edit_view.update(cx, |this, cx| {
                    if this
                        .ctx_menu
                        .as_ref()
                        .is_some_and(|state| state.menu_id == menu_id)
                    {
                        if let Some(CtxRow::Atlas { cmd }) = edit_rows.get(index) {
                            this.process_actions(vec![Action::StartCommandShortcutCapture(
                                cmd.command_id().to_string(),
                            )]);
                            cx.notify();
                        }
                    }
                });
            },
        );
        let on_activate = Rc::new(
            move |index: usize, window: &mut Window, cx: &mut gpui::App| {
                activate_view.update(cx, |this, cx| {
                    if this
                        .ctx_menu
                        .as_ref()
                        .is_some_and(|state| state.menu_id == menu_id)
                    {
                        if let Some(row) = snapshot.get(index) {
                            this.activate_ctx_row(row, window, cx);
                        }
                    }
                });
            },
        );
        let canvas = crate::ui::imm::menu::MenuCanvas::new(
            ink,
            selected_child_idx,
            crate::ui::imm::menu::MenuColors {
                foreground: fg,
                muted,
                border,
                accent,
                accent_foreground: accent_fg,
            },
            reveal_ms,
            on_hover,
            on_activate,
            on_edit_shortcut,
        );

        let Some(menu) = self.ctx_menu.as_mut() else {
            return div().into_any_element();
        };
        let viewport_h = (list_h - zoom).max(0.0);
        menu.scroll_top = menu
            .scroll_top
            .clamp(0.0, (content_h - viewport_h).max(0.0));
        let instant = self.state.settings.disable_animations
            || self.state.settings.scroll_smoothness <= 0.0
            || menu.scroll_drag.is_some();
        let scroll_config = crate::ui::motion::Spring {
            omega: 54.0 - 24.0 * self.state.settings.scroll_smoothness.clamp(0.0, 1.0),
            zeta: 1.0,
        }
        .config();
        let (scroll, scrolling) = menu.scroll_motion.sample_with_config(
            menu.scroll_top,
            instant,
            Instant::now(),
            scroll_config,
        );
        let scroll = scroll.clamp(0.0, (content_h - viewport_h).max(0.0));
        if scrolling {
            window.request_animation_frame();
        }
        let focus = menu.focus.clone();
        let panel = crate::ui::imm::menu_panel::MenuPanel {
            app: view.downgrade(),
            menu_id,
            focus: focus.clone(),
            input: menu.input.paint_snapshot(),
            recording: self.command_shortcut_capture.is_some(),
            scroll,
            content_h,
            height: est_h,
            colors: crate::ui::imm::menu_panel::PanelColors {
                background: bg,
                foreground: fg,
                muted,
                border,
                accent,
                radius: if bg.alpha < 1.0 { 10.0 * zoom } else { radius },
            },
            rows: crate::ui::imm::menu_panel::MenuPanel::rows(canvas),
        };
        // Only generic GPUI boxes remain: anchoring/focus and the fullscreen scrim.
        // All visible components, search editing and scrolling belong to IMM.
        let v = view.clone();
        gpui::deferred(
            div()
                .id("ctx-menu-scrim")
                .absolute()
                .inset_0()
                .size_full()
                .on_mouse_down(MouseButton::Left, move |_e, _w, cx| {
                    v.update(cx, |this, cx| this.close_ctx_menu(cx));
                })
                .on_mouse_down(MouseButton::Right, {
                    let v = view.clone();
                    move |_e, _w, cx| {
                        v.update(cx, |this, cx| this.close_ctx_menu(cx));
                    }
                })
                .child(
                    div()
                        .absolute()
                        .left(px(left))
                        .top(px(top))
                        .w(px(width))
                        .h(px(est_h))
                        .track_focus(&focus)
                        .opacity(0.92 + 0.08 * crate::ui::motion::menu_row_progress(reveal_ms, 0))
                        .paint_offset(gpui::point(
                            px(0.0),
                            px(4.0
                                * zoom
                                * (1.0 - crate::ui::motion::menu_row_progress(reveal_ms, 0))),
                        ))
                        .child(panel),
                ),
        )
        .priority(2)
        .into_any_element()
    }

    /// Keyboard model for the open menu. Returns `true` when the key was consumed
    /// (so the search input doesn't also see it).
    pub(crate) fn handle_ctx_menu_key(
        &mut self,
        ev: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.command_shortcut_capture.is_some() {
            return self.handle_command_shortcut_key(ev);
        }
        let ctrl = ev.keystroke.modifiers.control || ev.keystroke.modifiers.platform;
        let shift = ev.keystroke.modifiers.shift;
        let key = ev.keystroke.key.as_str();
        if !ctrl
            && self
                .ctx_menu
                .as_ref()
                .is_some_and(|menu| menu.input.marked.is_some())
        {
            // Candidate navigation/confirmation belongs to the platform IME.
            return false;
        }
        let editing_arrow = matches!(key, "left" | "right")
            && (ctrl
                || shift
                || self
                    .ctx_menu
                    .as_ref()
                    .is_some_and(|menu| !menu.input.text.is_empty()));
        if editing_arrow
            || matches!(key, "home" | "end" | "backspace" | "delete")
            || (ctrl && matches!(key, "a" | "c" | "v" | "x" | "z" | "y"))
        {
            let Some(menu) = self.ctx_menu.as_mut() else {
                return false;
            };
            let before = menu.input.text.clone();
            match key {
                "c" | "x" if ctrl => {
                    if menu.input.has_selection() {
                        cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                            menu.input.selected_text().to_string(),
                        ));
                        if key == "x" {
                            menu.input.delete_selection();
                        }
                    }
                }
                "v" if ctrl => {
                    if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                        menu.input.replace_native(None, &text, None, false);
                    }
                }
                _ => {
                    menu.input.handle_key(key, None, ctrl, shift);
                }
            }
            if before != menu.input.text {
                menu.selected = 0;
                menu.scroll_to_selected = true;
                menu.scroll_top = 0.0;
                menu.scroll_motion = RetainedSpring::new(0.0, Instant::now());
            }
            return true;
        }
        let query = self
            .ctx_menu
            .as_ref()
            .map(|menu| menu.input.text.clone())
            .unwrap_or_default();
        let rows = self.ctx_menu_rows(&query, &query);
        let selectable: Vec<usize> = rows
            .iter()
            .enumerate()
            .filter(|(_, r)| r.selectable())
            .map(|(i, _)| i)
            .collect();
        let count = selectable.len();

        match ev.keystroke.key.as_str() {
            "escape" => {
                self.close_ctx_menu(cx);
                true
            }
            "down" => {
                if let Some(m) = self.ctx_menu.as_mut() {
                    if count > 0 {
                        m.selected = (m.selected + 1) % count;
                        m.scroll_to_selected = true;
                    }
                }
                true
            }
            "up" => {
                if let Some(m) = self.ctx_menu.as_mut() {
                    if count > 0 {
                        m.selected = (m.selected + count - 1) % count;
                        m.scroll_to_selected = true;
                    }
                }
                true
            }
            "right" => {
                self.ctx_expand_selected(&rows, &selectable, true);
                true
            }
            "left" => {
                self.ctx_expand_selected(&rows, &selectable, false);
                true
            }
            "enter" => {
                self.ctx_activate_selected(&rows, &selectable, window, cx);
                true
            }
            _ => false,
        }
    }

    fn ctx_expand_selected(&mut self, rows: &[CtxRow], selectable: &[usize], expand: bool) {
        let local_group = self.ctx_menu.as_ref().and_then(|m| selectable.get(m.selected))
            .and_then(|&index| rows.get(index)).is_some_and(|row| matches!(row, CtxRow::Native { id: OPEN_WITH_GROUP, .. }));
        if local_group {
            self.set_open_with_expanded(expand);
            return;
        }
        let Some(m) = self.ctx_menu.as_mut() else {
            return;
        };
        let Some(&row_i) = selectable.get(m.selected) else {
            return;
        };
        if let CtxRow::Native { id, submenu, .. } = &rows[row_i] {
            if *submenu {
                if expand {
                    m.expanded.insert(*id);
                    // Lazy children: ask the worker if not loaded.
                    let need = !m.stale && submenu_children_missing(&m.native, *id);
                    #[cfg(windows)]
                    if need {
                        if let Some(handle) = &self.shell_menu {
                            handle.send(crate::shell_menu::ShellMenuRequest::ExpandSubmenu {
                                id: m.menu_id,
                                item_id: *id,
                            });
                        }
                    }
                    #[cfg(not(windows))]
                    let _ = need;
                } else {
                    m.expanded.remove(id);
                }
            }
        }
    }

    fn ctx_activate_selected(
        &mut self,
        rows: &[CtxRow],
        selectable: &[usize],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let selected = self.ctx_menu.as_ref().map(|m| m.selected).unwrap_or(0);
        let Some(&row_i) = selectable.get(selected) else {
            return;
        };
        self.activate_ctx_row(&rows[row_i], window, cx);
    }

    fn activate_ctx_row(&mut self, row: &CtxRow, window: &mut Window, cx: &mut Context<Self>) {
        self.cancel_command_shortcut_capture();
        match row {
            CtxRow::Atlas { cmd } => self.run_ctx_atlas(*cmd, window, cx),
            CtxRow::Ext { ext_id, cmd_id, .. } => {
                let ext_id = ext_id.clone();
                let cmd_id = cmd_id.clone();
                self.run_ctx_ext(&ext_id, &cmd_id, cx);
            }
            CtxRow::Native { id, submenu, .. } => {
                self.activate_ctx_native(*id, *submenu, window, cx)
            }
            CtxRow::DesktopApp { app, .. } => {
                let app = app.clone();
                self.run_ctx_desktop_app(&app, cx);
            }
            _ => {}
        }
    }

    pub(crate) fn run_ctx_desktop_app(
        &mut self,
        app: &atlas_core::fs::desktop_apps::DesktopApp,
        cx: &mut Context<Self>,
    ) {
        if let Some(state) = self.ctx_menu.take() {
            let _ = atlas_core::fs::desktop_apps::launch_desktop_app(app, &state.paths);
        }

        cx.notify();
    }

    fn run_ctx_ext(&mut self, ext_id: &str, cmd_id: &str, cx: &mut Context<Self>) {
        self.close_ctx_menu(cx);
        self.handle_ext_action(crate::app::ext::ExtAction::Run {
            ext_id: ext_id.to_string(),
            cmd_id: cmd_id.to_string(),
        });
        cx.notify();
    }

    #[cfg(feature = "ui-lab")]
    pub(crate) fn diagnostic_cleanup_menu(&mut self, path: PathBuf, activate: bool, window: &mut Window, cx: &mut Context<Self>) -> bool {
        self.open_ctx_menu(vec![path],gpui::point(gpui::px(400.),gpui::px(180.)),window,cx);
        let available=self.ctx_menu_rows("","").iter().any(|row|matches!(row,CtxRow::Atlas {cmd:AtlasCmd::DiskCleanup}));
        if available && activate {
            let paths=self.ctx_menu.as_ref().map(|m|m.paths.clone());
            self.run_ctx_atlas(AtlasCmd::DiskCleanup,window,cx);
            assert!(self.cleanup_panel.is_some(),"Cleanup menu command did not open the panel: {paths:?}");
        }else{self.close_ctx_menu(cx);}
        available
    }

    /// Run an Atlas command against the current selection, then close. We call the
    /// App methods directly (rather than dispatching the gpui action) so the menu's
    /// focused search input can't intercept `Copy`/`Cut`/`Paste` as text edits.
    fn run_ctx_atlas(&mut self, cmd: AtlasCmd, _window: &mut Window, cx: &mut Context<Self>) {
        let is_archive = self
            .state
            .active_tab_data()
            .is_some_and(|t| t.location.is_archive());
        let menu_paths = self
            .ctx_menu
            .as_ref()
            .filter(|m| !m.background)
            .map(|m| m.paths.clone())
            .unwrap_or_default();
        let menu_anchor = self.ctx_menu.as_ref().map(|m| m.anchor);
        self.close_ctx_menu(cx);
        match cmd {
            AtlasCmd::DiskCleanup => {
                if let [path] = menu_paths.as_slice() {
                    self.open_disk_cleanup(path.clone(), cx);
                }
            }
            AtlasCmd::ProtectFolder => {
                if let [path] = menu_paths.as_slice() {
                    self.request_vault(path.clone(), crate::app::vault::Phase::Create);
                }
            }
            AtlasCmd::UnlockFolder => {
                if let Some(path) = menu_paths.first() { self.try_open_vault(path); }
            }
            AtlasCmd::Convert => {
                if !is_archive {
                    self.open_converter_for(menu_paths);
                    // Open at the right-click point, beside the files, not where the menu item was.
                    if let Some(anchor) = menu_anchor {
                        self.anchor_converter(anchor);
                    }
                }
                else { self.toast_error(rust_i18n::t!("convert_local_only").into_owned()); }
            }
            AtlasCmd::Open => match menu_paths.as_slice() {
                [path] if cfg!(windows) && atlas_core::cleanup::is_drive_root(path) => {
                    self.navigate(self.state.focused_tile, path.clone());
                }
                _ => self.open_cursor(),
            },
            AtlasCmd::Rename => self.process_actions(vec![Action::RenameFocused]),
            AtlasCmd::Copy => {
                if is_archive {
                    self.toast_error(rust_i18n::t!("root_copy_archive_error").into());
                } else {
                    let paths = if menu_paths.is_empty() {
                        self.selected_paths()
                    } else {
                        menu_paths.clone()
                    };
                    if !paths.is_empty() {
                        self.process_actions(vec![Action::Copy(paths)]);
                    }
                }
            }
            AtlasCmd::Cut => {
                if is_archive {
                    self.toast_error(rust_i18n::t!("root_cut_archive_error").into());
                } else {
                    let paths = if menu_paths.is_empty() {
                        self.selected_paths()
                    } else {
                        menu_paths.clone()
                    };
                    if !paths.is_empty() {
                        self.process_actions(vec![Action::Cut(paths)]);
                    }
                }
            }
            AtlasCmd::Paste => {
                if let Some(dest) = self
                    .state
                    .active_tab_data()
                    .and_then(|t| t.location.fs_dir().map(|p| p.to_path_buf()))
                {
                    self.process_actions(vec![Action::Paste { dest }]);
                }
            }
            #[cfg(windows)]
            AtlasCmd::PasteShortcut => self.paste_shortcuts_focused(),
            AtlasCmd::Trash | AtlasCmd::DeletePerm => {
                if is_archive {
                    self.toast_error(rust_i18n::t!("root_delete_archive_error").into());
                } else {
                    let paths = if menu_paths.is_empty() {
                        self.selected_paths()
                    } else {
                        menu_paths.clone()
                    };
                    if !paths.is_empty() {
                        self.process_actions(vec![Action::Delete {
                            paths,
                            to_trash: cmd == AtlasCmd::Trash,
                        }]);
                    }
                }
            }
            AtlasCmd::Restore => {
                self.restore_selected_from_recycle_bin();
            }
            AtlasCmd::Properties => {
                let paths = if menu_paths.is_empty() {
                    self.selected_paths()
                } else {
                    menu_paths.clone()
                };
                #[cfg(windows)]
                match paths.as_slice() {
                    [] => {}
                    [single] => show_properties(single),
                    many => show_properties_multi(many),
                }
                // No native "properties" verb outside Windows — use our own
                // themed overlay instead (F3 #15).
                #[cfg(not(windows))]
                self.open_properties(paths);
            }
            AtlasCmd::SelectAll => {
                self.process_actions(vec![Action::SelectAll]);
            }
            AtlasCmd::NewFolder => {
                self.process_actions(vec![Action::CreateFolderFocused]);
            }
            AtlasCmd::NewFile => {
                self.process_actions(vec![Action::CreateFileFocused]);
            }
            AtlasCmd::OpenTerminal => {
                if let Some(dir) = self
                    .state
                    .active_tab_data()
                    .and_then(|t| t.location.fs_dir().map(|p| p.to_path_buf()))
                {
                    self.process_actions(vec![Action::OpenTerminal { dir }]);
                }
            }
            AtlasCmd::Refresh => {
                self.process_actions(vec![Action::Refresh(self.state.focused_tile)]);
            }
        }
        cx.notify();
    }

    /// Open the background right-click menu anchored at `anchor` (no file selection).
    #[allow(dead_code)]
    pub(crate) fn open_background_ctx_menu(
        &mut self,
        current_dir: Option<PathBuf>,
        anchor: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.suppress_next_shell_ctx_menu {
            self.suppress_next_shell_ctx_menu = false;
            self.close_ctx_menu(cx);
            return;
        }
        self.close_type_filter_menu(cx);
        self.close_breadcrumb_menu(cx);
        self.ctx_menu_seq += 1;
        let menu_id = self.ctx_menu_seq;
        let screen_pt = cursor_screen_pos();

        // Request native shell items for the current directory (background menu).

        #[cfg(windows)]
        let loading = {
            if let Some(ref dir) = current_dir {
                if let Some(handle) = &self.shell_menu {
                    handle.send(crate::shell_menu::ShellMenuRequest::Build {
                        id: menu_id,
                        paths: vec![dir.clone()],
                    });
                    true
                } else {
                    false
                }
            } else {
                false
            }
        };
        #[cfg(not(windows))]
        let loading = {
            let _ = &current_dir;
            false
        };

        #[cfg(windows)]
        let cache_key = if loading { native_cache_key(&[], true) } else { None };
        #[cfg(not(windows))]
        let cache_key: Option<String> = None;
        #[cfg(windows)]
        let (native, loading, stale) = match cache_key
            .as_ref()
            .and_then(|k| self.ctx_native.items.get(k))
        {
            Some(cached) if loading => (cached.clone(), false, true),
            _ => (Vec::new(), loading, false),
        };
        #[cfg(not(windows))]
        let (native, stale): (Vec<UiNativeItem>, bool) = (Vec::new(), false);

        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        self.ctx_menu = Some(CtxMenuState {
            menu_id,
            focus,
            input: ImmTextInputState::new(""),
            scroll_top: 0.0,
            scroll_motion: RetainedSpring::new(0.0, Instant::now()),
            input_bounds: None,
            input_line: None,
            input_scroll_x: 0.0,
            input_drag: false,
            scroll_drag: None,
            blink_visible: true,
            opened_at: Instant::now(),
            anchor,
            screen_pt,
            paths: current_dir.into_iter().collect(),
            native,
            loading,
            stale,
            cache_key,
            selected: 0,
            scroll_to_selected: true,
            expanded: HashSet::new(),
            background: true,
            ext_cmds: vec![],
            desktop_apps: Vec::new(),
        });
        self.notify_all_panes(cx);
        cx.notify();
    }

    /// Native row: a submenu toggles inline expansion; a leaf invokes the verb.
    fn activate_ctx_native(
        &mut self,
        id: u32,
        submenu: bool,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if id == OPEN_WITH_GROUP {
            let expanded = self.ctx_menu.as_ref().is_some_and(|menu| menu.expanded.contains(&OPEN_WITH_GROUP));
            self.set_open_with_expanded(!expanded);
            cx.notify();
            return;
        }
        if submenu {
            let expand = self
                .ctx_menu
                .as_ref()
                .map(|m| !m.expanded.contains(&id))
                .unwrap_or(false);
            if let Some(m) = self.ctx_menu.as_mut() {
                if expand {
                    m.expanded.insert(id);
                } else {
                    m.expanded.remove(&id);
                }
            }
            #[cfg(windows)]
            if expand {
                let (need, menu_id) = self
                    .ctx_menu
                    .as_ref()
                    .map(|m| (!m.stale && submenu_children_missing(&m.native, id), m.menu_id))
                    .unwrap_or((false, 0));
                if need {
                    if let Some(handle) = &self.shell_menu {
                        handle.send(crate::shell_menu::ShellMenuRequest::ExpandSubmenu {
                            id: menu_id,
                            item_id: id,
                        });
                    }
                }
            }
            cx.notify();
            return;
        }
        // Leaf: invoke through the worker, close the overlay immediately.
        #[cfg(windows)]
        if let Some(m) = self.ctx_menu.as_ref() {
            if m.stale {
                // Cached guess: ids are not live yet; run by title on `Built`.
                let hit = m
                    .native
                    .iter()
                    .find(|i| i.id == id && matches!(i.kind, UiItemKind::Normal));
                match hit {
                    Some(item) => {
                        self.ctx_native.pending =
                            Some((m.menu_id, item.title.clone(), m.screen_pt));
                    }
                    None => return, // nested leaf: wait for the real menu
                }
            } else if let Some(handle) = &self.shell_menu {
                handle.send(crate::shell_menu::ShellMenuRequest::Invoke {
                    id: m.menu_id,
                    item_id: id,
                    screen_pt: m.screen_pt,
                });
            }
        }
        #[cfg(not(windows))]
        let _ = id;
        // Visually close now; the worker frees its COM objects on InvokeDone.
        self.ctx_menu = None;

        cx.notify();
    }

    fn set_open_with_expanded(&mut self, expanded: bool) {
        let Some(menu) = &mut self.ctx_menu else { return; };
        if expanded { menu.expanded.insert(OPEN_WITH_GROUP); }
        else { menu.expanded.remove(&OPEN_WITH_GROUP); }
        menu.scroll_to_selected = true;
        #[cfg(windows)]
        if expanded && !menu.stale {
            if let Some(handle) = &self.shell_menu {
                for item in menu.native.iter().filter(|item| native_open_with(item) && submenu_children_missing(std::slice::from_ref(item), item.id)) {
                    handle.send(crate::shell_menu::ShellMenuRequest::ExpandSubmenu { id: menu.menu_id, item_id: item.id });
                }
            }
        }
    }
}

/// Remove leading, trailing, and consecutive `Separator` rows so no orphaned
/// dividers appear when a context-menu section is empty.
fn sanitize_separators(rows: Vec<CtxRow>) -> Vec<CtxRow> {
    let mut out: Vec<CtxRow> = Vec::with_capacity(rows.len());
    for row in rows {
        let is_sep = matches!(row, CtxRow::Separator);
        if is_sep && matches!(out.last(), None | Some(CtxRow::Separator)) {
            continue;
        }
        out.push(row);
    }
    // Strip trailing separator.
    if matches!(out.last(), Some(CtxRow::Separator)) {
        out.pop();
    }
    out
}

fn ctx_row_height(row: &CtxRow) -> f32 {
    match row {
        CtxRow::Separator => 9.0,
        CtxRow::Loading
        | CtxRow::Atlas { .. }
        | CtxRow::Ext { .. }
        | CtxRow::Native { .. }
        | CtxRow::DesktopApp { .. } => 30.0,
    }
}

/// Open the native shell Properties dialog for `path`.
/// On Windows uses `ShellExecuteExW` with the "properties" verb and
/// `SEE_MASK_INVOKEIDLIST` so the dialog works for any file system object.
/// No-op on non-Windows.
#[allow(unsafe_code)]
pub(crate) fn show_properties(path: &std::path::Path) {
    #[cfg(windows)]
    crate::shell_menu::properties(vec![path.to_path_buf()]);
    #[cfg(not(windows))]
    let _ = path;
}
#[allow(unsafe_code)]
pub(crate) fn show_properties_on_shell_thread(path: &std::path::Path) {
    #[cfg(windows)]
    {
        use windows::core::PCWSTR;
        use windows::Win32::Foundation::HWND;
        use windows::Win32::UI::Shell::{
            ShellExecuteExW, SEE_MASK_INVOKEIDLIST, SHELLEXECUTEINFOW,
        };
        use windows::Win32::UI::WindowsAndMessaging::SW_SHOW;

        let verb: Vec<u16> = "properties\0".encode_utf16().collect();
        let file: Vec<u16> = path
            .to_string_lossy()
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();

        let mut info = SHELLEXECUTEINFOW {
            cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
            fMask: SEE_MASK_INVOKEIDLIST,
            hwnd: HWND::default(),
            lpVerb: PCWSTR(verb.as_ptr()),
            lpFile: PCWSTR(file.as_ptr()),
            nShow: SW_SHOW.0,
            ..Default::default()
        };
        unsafe {
            let _ = ShellExecuteExW(&mut info);
        }
    }
    #[cfg(not(windows))]
    let _ = path;
}

/// Open the shell Properties dialog for "This PC".
#[allow(unsafe_code)]
pub(crate) fn show_this_pc_properties() {
    show_properties(std::path::Path::new(atlas_core::fs::shell_locations::COMPUTER));
}

/// Open the combined native Properties dialog for several paths at once
/// (`SHMultiFileProperties`, the same sheet Explorer shows for a multi-select).
/// Falls back to one dialog per path if the data object can't be built (e.g.
/// items don't share a common shell parent).
#[cfg(windows)]
#[allow(unsafe_code)]
fn show_properties_multi(paths: &[PathBuf]) {
    crate::shell_menu::properties(paths.to_vec());
}

/// Rank `titles` against `query` (best first). Standalone so it doesn't need the
/// `App` borrow held by `ctx_menu_rows`.
fn rank_titles(query: &str, titles: &[String]) -> Vec<usize> {
    let mut scorer = atlas_core::search::fuzzy::FuzzyScorer::new();
    scorer.rank(query, titles.iter().map(|s| s.as_str()))
}

/// Append the native tree (depth-aware, expansion-aware) as logical rows.
fn push_native_tree(
    items: &[UiNativeItem],
    depth: u8,
    expanded: &HashSet<u32>,
    out: &mut Vec<CtxRow>,
) {
    for it in items {
        match &it.kind {
            UiItemKind::Separator => out.push(CtxRow::Separator),
            UiItemKind::Normal => out.push(CtxRow::Native {
                id: it.id,
                title: it.title.clone(),
                icon: it.icon.clone(),
                depth,
                submenu: false,
                expanded: false,
                disabled: it.disabled,
                is_default: it.is_default,
            }),
            UiItemKind::Submenu { children } => {
                let is_exp = expanded.contains(&it.id);
                out.push(CtxRow::Native {
                    id: it.id,
                    title: it.title.clone(),
                    icon: it.icon.clone(),
                    depth,
                    submenu: true,
                    expanded: is_exp,
                    disabled: it.disabled,
                    is_default: it.is_default,
                });
                if is_exp {
                    if let Some(ch) = children {
                        push_native_tree(ch, depth + 1, expanded, out);
                    }
                }
            }
        }
    }
}

/// Collect leaf candidates for search mode, prefixing nested titles ("A › B").
fn collect_native_leaves(
    items: &[UiNativeItem],
    prefix: &str,
    titles: &mut Vec<String>,
    cands: &mut Vec<CtxRow>,
) {
    for it in items {
        match &it.kind {
            UiItemKind::Separator => {}
            UiItemKind::Normal => {
                let full = if prefix.is_empty() {
                    it.title.clone()
                } else {
                    format!("{prefix}{}", it.title)
                };
                titles.push(full.clone());
                cands.push(CtxRow::Native {
                    id: it.id,
                    title: full,
                    icon: it.icon.clone(),
                    depth: 0,
                    submenu: false,
                    expanded: false,
                    disabled: it.disabled,
                    is_default: it.is_default,
                });
            }
            UiItemKind::Submenu { children } => {
                if let Some(ch) = children {
                    let next = format!("{prefix}{} › ", it.title);
                    collect_native_leaves(ch, &next, titles, cands);
                }
            }
        }
    }
}

/// True when submenu `id` exists but its children are not yet loaded.
fn submenu_children_missing(items: &[UiNativeItem], id: u32) -> bool {
    for it in items {
        if let UiItemKind::Submenu { children } = &it.kind {
            if it.id == id {
                return children.is_none();
            }
            if let Some(ch) = children {
                if submenu_children_missing(ch, id) {
                    return true;
                }
            }
        }
    }
    false
}

/// Titles of the top-level submenus whose ids are in `expanded`.
#[cfg(windows)]
fn expanded_titles(items: &[UiNativeItem], expanded: &HashSet<u32>, out: &mut Vec<String>) {
    for it in items {
        if matches!(it.kind, UiItemKind::Submenu { .. }) && expanded.contains(&it.id) {
            out.push(it.title.clone());
        }
    }
}

/// Id of the top-level submenu called `title`.
#[cfg(windows)]
fn submenu_id_by_title(items: &[UiNativeItem], title: &str) -> Option<u32> {
    items
        .iter()
        .find(|it| matches!(it.kind, UiItemKind::Submenu { .. }) && it.title == title)
        .map(|it| it.id)
}

/// Place freshly-expanded children under submenu `id` (depth ≥ 2 lazy path).
/// `slot` is taken once the target is found, so recursion stops cleanly.
fn fill_submenu(items: &mut [UiNativeItem], id: u32, slot: &mut Option<Vec<UiNativeItem>>) {
    for it in items.iter_mut() {
        if let UiItemKind::Submenu { children } = &mut it.kind {
            if it.id == id {
                *children = slot.take();
                return;
            }
            if let Some(ch) = children {
                fill_submenu(ch, id, slot);
                if slot.is_none() {
                    return;
                }
            }
        }
    }
}

/// Capture the cursor position in physical screen pixels (for `ptInvoke`).
#[cfg(windows)]
#[allow(unsafe_code)]
fn cursor_screen_pos() -> (i32, i32) {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;
    let mut pt = POINT { x: 0, y: 0 };
    unsafe {
        let _ = GetCursorPos(&mut pt);
    }
    (pt.x, pt.y)
}

#[cfg(not(windows))]
fn cursor_screen_pos() -> (i32, i32) {
    (0, 0)
}

/// Identify the native action already supplied by Atlas's selection menu.
#[cfg(windows)]
fn native_properties_item(item: &crate::shell_menu::NativeMenuItem) -> bool {
    if !matches!(item.kind, crate::shell_menu::NativeItemKind::Normal) {
        return false;
    }
    if let Some(verb) = &item.verb {
        return verb.eq_ignore_ascii_case("properties");
    }
    // Some shell handlers do not expose a canonical verb. Only accept exact
    // system labels; application-specific commands must remain available.
    ["properties", "propiedades"]
        .iter()
        .any(|title| item.title.eq_ignore_ascii_case(title))
}

/// Convert worker `NativeMenuItem`s (with `RawIcon`) to UI items whose icons are
/// GPU textures. Recurses into eagerly-loaded submenu children.
#[cfg(windows)]
fn native_items_to_ui(
    items: Vec<crate::shell_menu::NativeMenuItem>,
    cx: &mut Context<App>,
) -> Vec<UiNativeItem> {
    use crate::shell_menu::NativeItemKind;
    let _ = cx;
    items
        .into_iter()
        .map(|it| {
            let icon = it.icon.and_then(raw_icon_to_texture);
            let kind = match it.kind {
                NativeItemKind::Normal => UiItemKind::Normal,
                NativeItemKind::Separator => UiItemKind::Separator,
                NativeItemKind::Submenu { children } => UiItemKind::Submenu {
                    children: children.map(|ch| native_items_to_ui(ch, cx)),
                },
            };
            UiNativeItem {
                id: it.id,
                title: it.title,
                verb: it.verb,
                icon,
                kind,
                disabled: it.disabled,
                is_default: it.is_default,
            }
        })
        .collect()
}

/// `RawIcon` (straight RGBA) → `RenderImage` (gpui expects BGRA, so swap R↔B).
#[cfg(windows)]
fn raw_icon_to_texture(raw: crate::shell_menu::RawIcon) -> Option<Arc<RenderImage>> {
    let mut bytes = raw.rgba;
    for px in bytes.chunks_exact_mut(4) {
        px.swap(0, 2); // RGBA → BGRA
    }
    let buf = image::RgbaImage::from_raw(raw.w, raw.h, bytes)?;
    Some(Arc::new(RenderImage::new(vec![image::Frame::new(buf)])))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn native_properties_identity_preserves_application_commands_and_submenus() {
        use crate::shell_menu::{NativeItemKind, NativeMenuItem};
        let item = |title: &str, verb: Option<&str>, kind| NativeMenuItem {
            id: 42,
            title: title.into(),
            verb: verb.map(str::to_owned),
            icon: None,
            kind,
            disabled: false,
            is_default: false,
        };
        for (title, verb) in [
            ("Properties", None),
            ("Propiedades", None),
            ("Localized", Some("PROPERTIES")),
        ] {
            assert!(native_properties_item(&item(
                title,
                verb,
                NativeItemKind::Normal
            )));
        }
        for (title, verb) in [
            ("Image properties", None),
            ("Properties", Some("app.properties")),
            ("WinRAR", None),
        ] {
            assert!(!native_properties_item(&item(
                title,
                verb,
                NativeItemKind::Normal
            )));
        }
        assert!(!native_properties_item(&item(
            "Properties",
            None,
            NativeItemKind::Submenu { children: None }
        )));
    }

    #[test]
    fn every_atlas_menu_command_uses_the_palette_binding_registry() {
        for items in [ATLAS_ITEMS, ATLAS_ITEMS_BACKGROUND, ATLAS_ITEMS_RECYCLE] {
            for (cmd, _) in items {
                assert!(
                    crate::input::commands::command_remappable(cmd.command_id()),
                    "{}",
                    cmd.command_id()
                );
            }
        }
    }
}

#[cfg(windows)]
#[allow(unsafe_code)]
pub(crate) fn show_properties_multi_on_shell_thread(paths: &[PathBuf]) {
    use crate::dnd::windows::data_object::AtlasDataObject;
    use std::sync::atomic::AtomicU32;
    use windows::Win32::System::Ole::DROPEFFECT_COPY;
    use windows::Win32::UI::Shell::SHMultiFileProperties;

    let data_obj = AtlasDataObject::new(paths, DROPEFFECT_COPY, Arc::new(AtomicU32::new(0)));
    let ok = data_obj.is_some_and(|obj| unsafe { SHMultiFileProperties(&obj, 0).is_ok() });
    if !ok {
        for path in paths {
            show_properties_on_shell_thread(path);
        }
    }
}

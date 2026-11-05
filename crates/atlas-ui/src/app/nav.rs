use crate::app::App;
use atlas_model::layout_tree::{Tile, TileId};
use atlas_model::location::Location;
use std::path::PathBuf;

/// Controls how `navigate_to` manipulates the history stacks.
pub(crate) enum NavKind {
    /// Push current location onto history, clear forward stack.
    Push,
    /// Caller already popped from history; push current to forward stack.
    Back,
    /// Caller already popped from forward stack; push current to history.
    Forward,
}

impl App {
    pub(crate) fn toggle_folder_view_recursive(&mut self) {
        use atlas_model::profiles::{FolderViewFeature, FolderViewPref};
        let Some(tab) = self.tab_data_for(self.state.focused_tile) else { return; };
        let Location::Directory(path) = &tab.location else { return; };
        let path = path.clone();
        let mut prefs = self.state.profiles.active().scoped::<FolderViewFeature>();
        let recursive = !prefs.get_exact(&path).is_some_and(|pref| pref.recursive);
        let pref = FolderViewPref { view_mode: tab.view_mode.clone(), sort_col: tab.sort_col.clone(),
            sort_dir: tab.sort_dir.clone(), columns: tab.details_columns.clone(), recursive };
        prefs.set(path, pref);
        self.state.profiles.active_mut().set_scoped::<FolderViewFeature>(&prefs);
        atlas_core::util::config::refresh_bookmark_volumes(&mut self.state.profiles);
        atlas_core::util::config::save_profiles(&self.state.profiles);
        self.toast_info(if recursive { rust_i18n::t!("folder_inherit_enabled").to_string() }
            else { rust_i18n::t!("folder_inherit_disabled").to_string() });
    }

    pub(crate) fn recent_folders(&self) -> Vec<PathBuf> {
        use atlas_model::profiles::RecentFoldersFeature;
        self.state
            .profiles
            .active()
            .scoped::<RecentFoldersFeature>()
    }

    pub(crate) fn push_recent_folder(&mut self, path: &std::path::Path) {
        if atlas_core::vault::live::is_private(path) { return; }
        if self.drag_hover.navigation_is_provisional() {
            return;
        }
        use atlas_model::profiles::RecentFoldersFeature;
        let mut recent = self
            .state
            .profiles
            .active()
            .scoped::<RecentFoldersFeature>();
        recent.retain(|p| p != path);
        recent.push(path.to_path_buf());
        if recent.len() > 50 {
            recent.remove(0);
        }
        self.state
            .profiles
            .active_mut()
            .set_scoped::<RecentFoldersFeature>(&recent);
        atlas_core::util::config::refresh_bookmark_volumes(&mut self.state.profiles);
    }

    /// Single entry point for all navigation. Handles history, watcher lifecycle,
    /// recent folders, begin_navigation, focused_tile, and spawn_list.
    pub(crate) fn navigate_to(&mut self, tab_id: TileId, mut loc: Location, kind: NavKind) {
        if atlas_core::vault::live::is_private(&loc.display_path()) && !self.owns_native_vault_path(&loc.display_path()) {
            self.toast_error(rust_i18n::t!("vault_access_denied").to_string()); return;
        }
        if let Location::Archive { archive, .. } = &loc {
            if atlas_core::vault::live::is_private(archive) {
                self.process_actions(vec![crate::input::action::Action::Open(vec![archive.clone()])]);
                return;
            }
        }
        if let Some(path) = loc.fs_dir() {
            if atlas_core::vault::is_private_path(path) { self.state.focused_tile = tab_id; }
            if self.try_open_vault(path) { return; }
        }
        let tab_id = match self.resolve_pane(tab_id) {
            Some(id) => id,
            None => return,
        };
        atlas_core::fs::paths::normalize_wsl_location_path(&mut loc);
        #[cfg(feature = "provider-host")]
        let _ = self.cloud_tx.send(atlas_core::cloud::CloudCmd::CancelList { tab: tab_id });
        if let Some(from) = self.tab_data_for(tab_id).map(|pane| pane.location.clone()) {
            self.pane_view_transitions.entry(tab_id).or_default()
                .navigate(&from, &loc, matches!(kind, NavKind::Back));
        }
        self.file_filter_projections.remove(&tab_id);
        self.rubber = None;
        self.pending_folder_click = None;
        // A glide belongs to its current listing, never to the next folder.
        if let Some(handle) = self.scroll_handles.get(&tab_id) {
            let scroll = &handle.0.borrow().base_handle;
            scroll.set_offset(scroll.offset());
        }
        for ((pane, _), handle) in &self.miller_scroll_handles {
            if *pane == tab_id {
                let scroll = &handle.0.borrow().base_handle;
                scroll.set_offset(scroll.offset());
            }
        }

        // Per-folder view/sort memory (F3 #14): looked up before the mutable
        // pane borrow below so it can be applied in the same pass.
        let restored_view_pref = if let Location::Directory(path) = &loc {
            use atlas_model::profiles::FolderViewFeature;
            self.state
                .profiles
                .active()
                .scoped::<FolderViewFeature>()
                .get(path)
        } else {
            None
        };
        let sticky_search = self.state.settings.filter_sticky
            && self.tab_data_for(tab_id).is_some_and(|tab| tab.location.fs_dir().is_some())
            && loc.fs_dir().is_some();

        // ── 1. History manipulation ──────────────────────────────────────────
        if let Some(Tile::Pane(pane)) = self.state.pane_tree.tiles.get_mut(tab_id) {
            #[cfg(feature = "provider-host")]
            if matches!(loc, Location::Remote { .. }) && matches!(pane.view_mode,
                atlas_model::settings::ViewMode::Tree | atlas_model::settings::ViewMode::Miller) {
                pane.view_mode = atlas_model::settings::ViewMode::Details;
            }
            if matches!(loc, Location::Shell(_)) && matches!(pane.view_mode, atlas_model::settings::ViewMode::Tree | atlas_model::settings::ViewMode::Miller) {
                pane.view_mode = atlas_model::settings::ViewMode::Details;
            }
            let old = pane.location.clone();
            pane.breadcrumb_trail = match (old.fs_dir(), loc.fs_dir()) {
                (Some(old), Some(new)) => {
                    pane.breadcrumb_trail.as_ref().filter(|trail| trail.starts_with(new)).cloned()
                        .or_else(|| (old != new && old.starts_with(new)).then(|| old.to_path_buf()))
                }
                _ => None,
            };
            match kind {
                NavKind::Push => {
                    if old != loc {
                        pane.history.push(old);
                        pane.history_forward.clear();
                    }
                }
                NavKind::Back => {
                    pane.history_forward.push(old);
                }
                NavKind::Forward => {
                    pane.history.push(old);
                }
            }
            pane.location = loc.clone();
            pane.volume_serial = None;
            pane.reconcile_volume(&self.mounted_volumes);
            pane.details_columns = restored_view_pref.as_ref().and_then(|pref| pref.columns.clone());
            if let Some(pref) = restored_view_pref {
                pane.view_mode = pref.view_mode;
                pane.sort_col = pref.sort_col;
                pane.sort_dir = pref.sort_dir;
            }
        }

        // ── 2. Watcher lifecycle ─────────────────────────────────────────────
        // Only Directory locations get a watcher; entering an archive/tag/remote
        // drops any lingering watcher for this pane (fixes stale-watcher bug on back).
        match &loc {
            Location::Directory(path) => {
                self.watchers.remove(&tab_id);
                if let Ok(handle) = atlas_core::fs::watcher::watch_path(path, self.tx.clone()) {
                    self.watchers.insert(tab_id, handle);
                }
            }
            _ => {
                self.watchers.remove(&tab_id);
            }
        }

        // ── 3. Recent folders (Directory only) ──────────────────────────────
        if let Location::Directory(path) = &loc {
            self.push_recent_folder(path);
        }

        // ── 4. Begin navigation ──────────────────────────────────────────────
        let had_entries = self
            .state
            .tab_states
            .get(&tab_id)
            .is_some_and(|ts| !ts.entries.is_empty());
        if let Some(ts) = self.state.tab_states.get_mut(&tab_id) {
            ts.begin_navigation_preserving_filter(sticky_search);
        }
        self.typed_selection = None;
        // Drop the pane's filter input so its stale text can't re-apply the old
        // filter to the new directory through the bottom-bar value sync.
        self.search_inputs.remove(&tab_id);
        self.state.clear_preview(tab_id);
        self.preview_textures.remove(&tab_id);
        self.multi_textures.remove(&tab_id);
        self.preview_editor = None;

        // ── 5. Stale drag geometry ───────────────────────────────────────────
        // Navigating mid-drag (spring-load) leaves this pane's row-level hover
        // targets and OLE zone dest pointing at the *previous* listing — a drop
        // would route to a folder that is no longer on screen. Prune the row
        // targets (the next prepaint re-registers fresh ones; meanwhile the
        // pane-bounds fallback reads the live location) and rewrite the OLE
        // zone dest in place, since the nested DoDragDrop loop may resolve a
        // drop before the next frame recommits zones.
        {
            self.pending_drag_hover_targets.retain(|target| {
                !matches!(&target.id,
                    crate::ui::drag_hover::DragHoverId::Folder(pane, _)
                    | crate::ui::drag_hover::DragHoverId::Column(pane, _)
                    | crate::ui::drag_hover::DragHoverId::Breadcrumb(pane, _)
                    | crate::ui::drag_hover::DragHoverId::Chrome(_, pane) if *pane == tab_id)
            });
            self.row_rects.borrow_mut().remove(&tab_id);
            #[cfg(windows)]
            if let Some(dnd) = &self.dnd {
                if let Ok(mut zones) = dnd.zones.lock() {
                    match crate::ui::drag_hover::location_drop_command(&loc) {
                        crate::ui::drag_hover::DragHoverDropCommand::TransferTo(dest) => {
                            zones.set_dest_for_pane(tab_id.0, dest);
                        }
                        _ => zones.remove_pane(tab_id.0),
                    }
                }
            }
        }

        // ── 6. Focused tile + listing ────────────────────────────────────────
        self.state.focused_tile = tab_id;
        self.spawn_list(tab_id, loc);
        #[cfg(windows)]
        self.publish_native_drop_snapshot();
        // Keep an already-painted pane mounted until BgResult atomically replaces
        // it. Repainting the empty begin_navigation state caused a visible flash.
        // New/empty panes still need their initial loading state painted.
        if !had_entries {
            self.dirty_panes.insert(tab_id);
        }
    }

    /// Persist a per-folder view/sort pref (F3 #14). No-op when `pref` is `None`
    /// (non-directory locations don't get remembered).
    pub(crate) fn save_folder_view_pref(
        &mut self,
        pref: Option<(PathBuf, atlas_model::profiles::FolderViewPref)>,
    ) {
        let Some((path, mut pref)) = pref else { return };
        if atlas_core::vault::live::is_private(&path) { return; }
        use atlas_model::profiles::FolderViewFeature;
        let mut prefs = self.state.profiles.active().scoped::<FolderViewFeature>();
        if let Some(previous) = prefs.get_exact(&path) { pref.recursive = previous.recursive; }
        if pref.columns.is_none() {
            pref.columns = self.tab_data_for(self.state.focused_tile).and_then(|tab| tab.details_columns.clone());
        }
        if prefs.get_exact(&path).as_ref() == Some(&pref) {
            return;
        }
        prefs.set(path, pref);
        self.state
            .profiles
            .active_mut()
            .set_scoped::<FolderViewFeature>(&prefs);
        atlas_core::util::config::refresh_bookmark_volumes(&mut self.state.profiles);
        atlas_core::util::config::save_profiles(&self.state.profiles);
    }

    // ── Thin public wrappers ──────────────────────────────────────────────────

    /// Navigate to an explicit local destination, regardless of the source account.
    pub(crate) fn navigate(&mut self, tab_id: TileId, path: PathBuf) {
        self.navigate_to(tab_id, entry_destination(None, path), NavKind::Push);
    }

    /// Entry activation carries its source location. Remote names that resemble
    /// Windows paths must still stay remote; sidebar/local destinations use navigate.
    pub(crate) fn navigate_entry(&mut self, tab_id: TileId, path: PathBuf) {
        let loc = entry_destination(self.tab_data_for(tab_id).map(|td| &td.location), path);
        self.navigate_to(tab_id, loc, NavKind::Push);
    }

    pub(crate) fn navigate_back(&mut self, tab_id: TileId) {
        let prev = match self.state.pane_tree.tiles.get_mut(tab_id) {
            Some(Tile::Pane(pane)) => pane.history.pop(),
            _ => None,
        };
        if let Some(loc) = prev {
            self.navigate_to(tab_id, loc, NavKind::Back);
        }
    }

    pub(crate) fn navigate_forward(&mut self, tab_id: TileId) {
        let next = match self.state.pane_tree.tiles.get_mut(tab_id) {
            Some(Tile::Pane(pane)) => pane.history_forward.pop(),
            _ => None,
        };
        if let Some(loc) = next {
            self.navigate_to(tab_id, loc, NavKind::Forward);
        }
    }

    pub(crate) fn navigate_up(&mut self, tab_id: TileId) {
        #[cfg(windows)]
        if let Some(mount) = &self.vault_mount {
            if self.tab_data_for(tab_id).and_then(|tab| tab.location.fs_dir()) == Some(mount.path()) {
                if let Some(parent) = mount.root().parent() {
                    self.navigate(tab_id, parent.to_path_buf()); return;
                }
            }
        }
        let parent = self
            .tab_data_for(tab_id)
            .and_then(|td| td.location.ascend());
        if let Some(loc) = parent {
            self.navigate_to(tab_id, loc, NavKind::Push);
        }
    }

    /// Enter an archive file or navigate to a subdirectory within one already open.
    pub(crate) fn navigate_into_archive(
        &mut self,
        tab_id: TileId,
        archive: PathBuf,
        inner: PathBuf,
    ) {
        let loc = Location::Archive { archive, inner };
        self.navigate_to(tab_id, loc, NavKind::Push);
    }

    /// Shared file activation for flat, Tree and Miller views. Virtual paths
    /// must be extracted before opening; local archives stay inside Atlas.
    pub(crate) fn activate_file_path(&mut self, tile: TileId, path: PathBuf, archive_root: Option<PathBuf>) {
        if let Some(archive) = archive_root {
            let open_id = self.state.next_op_id();
            self.rt.spawn(async move {
                let _ = tokio::task::spawn_blocking(move || {
                    #[allow(clippy::disallowed_methods)] // reason: background extraction
                    let result = (|| -> anyhow::Result<()> {
                        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_nanos();
                        let dir = std::env::temp_dir().join("atlas-open").join(format!("{}-{stamp}-{open_id}", std::process::id()));
                        std::fs::create_dir_all(&dir)?;
                        let dest = dir.join(path.file_name().unwrap_or_default());
                        let provider = atlas_core::fs::archive::open(&archive)
                            .ok_or_else(|| anyhow::anyhow!("Cannot open archive"))?;
                        provider.extract_to(&path, &dest)?;
                        open::that(dest)?;
                        Ok(())
                    })();
                    if let Err(error) = result { tracing::warn!(%error, "archive file activation failed"); }
                }).await;
            });
            return;
        }
        #[cfg(feature = "provider-host")]
        if self.open_remote_paths_for(tile, &[path.clone()]) { return; }
        if atlas_core::fs::archive::is_archive_path(&path) {
            self.navigate_into_archive(tile, path, PathBuf::new());
        } else {
            self.spawn_open_files(vec![path]);
        }
    }

    // ── Helpers ───────────────────────────────────────────────────────────────

    fn resolve_pane(&self, tab_id: TileId) -> Option<TileId> {
        if matches!(self.state.pane_tree.tiles.get(tab_id), Some(Tile::Pane(_))) {
            return Some(tab_id);
        }
        let focused = self.state.focused_tile;
        if matches!(self.state.pane_tree.tiles.get(focused), Some(Tile::Pane(_))) {
            return Some(focused);
        }
        self.state.all_pane_tile_ids().into_iter().next()
    }

    /// Dispatch a background listing based on location type (for refresh/toggle-hidden).
    /// Does NOT manipulate history or begin_navigation; use `navigate_to` for navigation.
    pub(crate) fn dispatch_list(
        &mut self,
        tab_id: TileId,
        loc: Location,
        col: atlas_model::file_entry::SortColumn,
        dir: atlas_model::file_entry::SortDir,
    ) {
        match loc {
            Location::Directory(path) => self.spawn_list_dir(tab_id, path, col, dir),
            Location::Shell(path) => {
                let op_id = self.state.next_op_id();
                let ts = self.state.tab_states.entry(tab_id).or_default();
                ts.load_state = atlas_model::tab::LoadState::Loading;
                ts.last_nav_op_id = op_id;
                let tx = self.tx.clone();
                self.rt.spawn(async move {
                    let result = tokio::task::spawn_blocking(move || atlas_core::fs::shell_locations::list(&path)).await;
                    let message = match result {
                        Ok(Ok(entries)) => atlas_model::app_state::BgResult::DirListed { tab: tab_id, op_id, entries },
                        Ok(Err(error)) => atlas_model::app_state::BgResult::DirListError { tab: tab_id, op_id, error: error.to_string() },
                        Err(error) => atlas_model::app_state::BgResult::DirListError { tab: tab_id, op_id, error: error.to_string() },
                    };
                    let _ = tx.send(message);
                });
            }
            Location::Archive { archive, inner } => {
                self.spawn_list_archive(tab_id, archive, inner, col, dir);
            }
            Location::Tag(tag) => {
                let op_id = self.state.next_op_id();
                let ts = self.state.tab_states.entry(tab_id).or_default();
                ts.load_state = atlas_model::tab::LoadState::Loading;
                ts.last_nav_op_id = op_id;
                let _ = self
                    .tag_db_tx
                    .send(atlas_core::fs::tag_db::TagDbCmd::QueryPathsForTag(tag));
            }
            Location::RecycleBin => {
                self.spawn_list_recycle_bin(tab_id, col, dir);
            }
            #[cfg(feature = "provider-host")]
            Location::Remote { id, path } => {
                self.spawn_list_remote(tab_id, id, path);
            }
        }
    }
}

fn entry_destination(current: Option<&Location>, path: PathBuf) -> Location {
    #[cfg(feature = "provider-host")]
    if let Some(Location::Remote { id, .. }) = current {
        return Location::Remote { id: id.clone(), path: path.to_string_lossy().into_owned() };
    }
    if let Some(Location::Archive { archive, .. }) = current {
        return Location::Archive { archive: archive.clone(), inner: path };
    }
    if path.to_string_lossy().starts_with("::") || atlas_model::location::is_unc_host(&path.to_string_lossy()) { return Location::Shell(path.to_string_lossy().into_owned()); }
    Location::Directory(path)
}

#[cfg(all(test, feature = "provider-host"))]
mod cloud_navigation_tests {
    use super::*;

    #[test]
    fn entry_destinations_keep_source_account_even_for_absolute_looking_names() {
        let remote = Location::Remote { id: "drive-2".into(), path: "docs".into() };
        let local = std::env::temp_dir().join("Downloads");
        assert_eq!(entry_destination(Some(&Location::Directory(local.clone())), local.clone()), Location::Directory(local));
        assert_eq!(entry_destination(Some(&remote), "docs/sub".into()),
            Location::Remote { id: "drive-2".into(), path: "docs/sub".into() });
        assert_eq!(entry_destination(Some(&Location::Remote { id: "drive-1".into(), path: "".into() }), "docs/sub".into()),
            Location::Remote { id: "drive-1".into(), path: "docs/sub".into() });
        assert_eq!(entry_destination(Some(&remote), r"C:\Users\example".into()),
            Location::Remote { id: "drive-2".into(), path: r"C:\Users\example".into() });
    }
}

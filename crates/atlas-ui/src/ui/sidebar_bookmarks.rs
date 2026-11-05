//! Insertion lanes move references; the center of an existing folder transfers files.
use crate::app::App;
use crate::ui::drag_hover::{
    drag_hover_recorder_with_drop, DragHoverCommand, DragHoverDropCommand, DragHoverId,
    DragHoverKind,
};
use crate::ui::{root::DraggedFiles, sidebar::SidebarStyle};
use gpui::ColorExt as _;
use gpui::{div, prelude::*, px, Context, Entity, IntoElement, RenderOnce, SharedString, Window};
use gpui_component::ElementExt;
use std::path::PathBuf;
use std::time::{Duration, Instant};

#[derive(Clone)]
struct BookmarkPresence {
    name: String,
    path: PathBuf,
    index: usize,
    entered: Option<Instant>,
    closing: Option<Instant>,
}

#[derive(Default)]
struct BookmarkPresenceState { initialized: bool, entries: Vec<BookmarkPresence> }

impl BookmarkPresenceState {
    fn sync(&mut self, items: &[(String, PathBuf)], now: Instant, instant: bool) -> Vec<BookmarkPresence> {
        for entry in &mut self.entries {
            if !items.iter().any(|(_, path)| *path == entry.path) { entry.closing.get_or_insert(now); }
        }
        self.entries.retain(|entry| entry.closing.is_none_or(|at| !instant && now.saturating_duration_since(at) < Duration::from_millis(240)));
        for (index, (name, path)) in items.iter().enumerate() {
            if let Some(entry) = self.entries.iter_mut().find(|entry| entry.path == *path) {
                entry.index = index;
                entry.name.clone_from(name);
                entry.closing = None;
            } else {
                self.entries.push(BookmarkPresence { name: name.clone(), path: path.clone(), index, entered: (self.initialized && !instant).then_some(now), closing: None });
            }
        }
        self.initialized = true;
        let mut entries = self.entries.clone();
        entries.sort_by_key(|entry| (entry.index, entry.closing.is_none()));
        entries
    }
}

#[derive(IntoElement)]
pub(crate) struct BookmarkCollection {
    pub items: Vec<(String, PathBuf)>,
    pub app: Entity<App>,
    pub style: SidebarStyle,
    pub current: Option<PathBuf>,
}

impl RenderOnce for BookmarkCollection {
    fn render(self, window: &mut Window, cx: &mut gpui::App) -> impl IntoElement {
        let profile = self.app.read(cx).state.profiles.active().name.clone();
        let state = window.use_keyed_state(SharedString::from(format!("bookmark-presence-{profile}")), cx, |_, _| BookmarkPresenceState::default());
        let now = Instant::now();
        let instant = cx.reduce_motion();
        let entries = state.update(cx, |state, _| state.sync(&self.items, now, instant));
        let insertion = match self.app.read(cx).active_drag_hover_drop_command() { Some(DragHoverDropCommand::InsertBookmarks(index)) => Some(index), _ => None };
        let mut list = div().w_full().flex().flex_col();
        let mut animating = false;
        for entry in entries {
            let live = entry.closing.is_none();
            if live { list = list.child(InsertionLane { index: entry.index, app: self.app.clone(), active: insertion == Some(entry.index), style: self.style }); }
            let id = SharedString::from(format!("bookmark-{profile}-{}", entry.path.display()));
            let row = if live {
                let shell = crate::ui::sidebar::sidebar_shell_tex(&self.app, &entry.path, self.style.windows_theme, cx);
                crate::ui::sidebar::bookmark_row(entry.index, id.clone(), entry.name, entry.path.clone(), shell, &self.app, self.style, self.current.as_ref() == Some(&entry.path), profile.clone()).into_any_element()
            } else {
                // Retained exits carry no click, drag or drop handlers.
                let t = now.saturating_duration_since(entry.closing.unwrap()).as_secs_f32() / 0.12;
                div().h(crate::ui::rem_px(if self.style.windows_theme { 30.0 } else { 24.0 }))
                    .px_2().flex().items_center().gap_2().text_color(self.style.fg)
                    .opacity(1.0 - crate::ui::motion::ease_out_quart(t))
                    .child(crate::ui::icons::theme_icon(crate::ui::icons::AtlasIcon::Folder, 16.0, self.style.fg, self.style.windows_theme))
                    .child(div().min_w_0().truncate().child(entry.name)).into_any_element()
            };
            let mut body = crate::ui::reveal::DisclosureBody::new(id, live, row);
            if entry.entered.is_some() { body = body.animate_entry(); }
            list = list.child(body);
            animating |= !live || entry.entered.is_some_and(|at| now.saturating_duration_since(at) < Duration::from_millis(240));
        }
        if !self.items.is_empty() { list = list.child(InsertionLane { index: self.items.len(), app: self.app, active: insertion == Some(self.items.len()), style: self.style }); }
        if animating && !instant { list = list.child(gpui::canvas(|_, _, _| {}, |_, _, window, _| window.request_animation_frame()).size(px(0.0))); }
        list
    }
}

#[derive(Clone)]
pub(crate) struct BookmarkDrag {
    pub removable: bool,
    pub path: PathBuf,
    pub profile: String,
    pub label: String,
}

impl gpui::Render for BookmarkDrag {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().px_2().py_1().child(self.label.clone())
    }
}

/// Boundary uses the original order, before removing dragged references.
pub(crate) fn insert_references(
    bookmarks: &mut Vec<atlas_model::profiles::Bookmark>,
    paths: &[PathBuf],
    boundary: usize,
) {
    let boundary = boundary.min(bookmarks.len());
    let mut seen = std::collections::HashSet::new();
    let paths: Vec<_> = paths
        .iter()
        .filter(|path| seen.insert((*path).clone()))
        .collect();
    let before = bookmarks
        .iter()
        .take(boundary)
        .filter(|b| paths.contains(&&b.path))
        .count();
    let mut insertion = boundary - before;
    let mut moved = Vec::new();
    for path in paths {
        if let Some(index) = bookmarks.iter().position(|b| &b.path == path) {
            moved.push(bookmarks.remove(index));
        } else {
            let name = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string());
            moved.push(atlas_model::profiles::Bookmark::new(name, path.clone()));
        }
    }
    insertion = insertion.min(bookmarks.len());
    bookmarks.splice(insertion..insertion, moved);
}

impl App {
    pub(crate) fn bookmark_removal_at(&self, position: gpui::Point<gpui::Pixels>) -> bool {
        self.sidebar_scroll_region.as_ref().is_some_and(|(bounds, _)| {
            position.x > bounds.right() + gpui::px(24.0)
        }) && !self.modal_overlay_active()
    }

    pub(crate) fn remove_bookmark_reference(&mut self, drag: &BookmarkDrag, position: gpui::Point<gpui::Pixels>) {
        if drag.removable && self.state.profiles.active().name == drag.profile && self.bookmark_removal_at(position) {
            let bookmarks = &mut self.state.profiles.active_mut().bookmarks;
            let before = bookmarks.len();
            bookmarks.retain(|bookmark| bookmark.path != drag.path);
            if bookmarks.len() != before {
                self.mark_drag_committed();
                atlas_core::util::config::save_profiles(&self.state.profiles);
                self.toast_info(rust_i18n::t!("sidebar_bookmark_removed").into_owned());
            }
        }
        self.drag_hover_external_left();
    }

    pub(crate) fn insert_bookmark_paths(&mut self, paths: Vec<PathBuf>, boundary: usize) {
        let directories: Vec<_> = paths.iter().filter(|path| path.is_dir()).cloned().collect();
        if directories.len() != paths.len() {
            self.toast_error(rust_i18n::t!("sidebar_bookmark_folders_only").into_owned());
        }
        if directories.is_empty() {
            return;
        }
        insert_references(
            &mut self.state.profiles.active_mut().bookmarks,
            &directories,
            boundary,
        );
        atlas_core::util::config::save_profiles(&self.state.profiles);
    }
}

#[derive(IntoElement)]
pub(crate) struct InsertionLane {
    pub index: usize,
    pub app: Entity<App>,
    pub active: bool,
    pub style: SidebarStyle,
}

impl RenderOnce for InsertionLane {
    fn render(self, _window: &mut Window, _cx: &mut gpui::App) -> impl IntoElement {
        let internal = self.app.clone();
        let external = self.app.clone();
        let reorder = self.app.clone();
        let index = self.index;
        let accent = self.style.accent;
        div()
            .id(SharedString::from(format!("bookmark-insertion-{index}")))
            .w_full()
            .flex()
            .flex_col()
            .relative()
            .min_h(crate::ui::rem_px(6.0))
            .child(crate::ui::reveal::DisclosureBody::new(
                SharedString::from(format!("bookmark-gap-{index}")),
                self.active,
                div().h(crate::ui::rem_px(12.0)).w_full(),
            ))
            .when(self.active, |lane| {
                lane.child(
                    div()
                        .absolute()
                        .left(px(6.0))
                        .right(px(6.0))
                        .top_1_2()
                        .h(px(2.0))
                        .bg(accent),
                )
            })
            .on_drag_over::<DraggedFiles>(move |style, _, _, _| style.bg(accent.opacity(0.2)))
            .on_drag_over::<gpui::ExternalPaths>(move |style, _, _, _| {
                style.bg(accent.opacity(0.2))
            })
            .on_drag_over::<BookmarkDrag>(move |style, _, _, _| style.bg(accent.opacity(0.2)))
            .on_prepaint_child(drag_hover_recorder_with_drop(
                DragHoverId::Sidebar(format!("bookmark-insertion-{index}")),
                DragHoverKind::Nav,
                DragHoverCommand::Process(crate::input::action::Action::CloseOverlays),
                DragHoverDropCommand::InsertBookmarks(index),
                &self.app,
            ))
            .on_drop::<DraggedFiles>(move |drag, window, cx| {
                if !crate::ui::sidebar::is_sidebar_drop_allowed(
                    &internal,
                    window.mouse_position(),
                    cx,
                ) {
                    return;
                }
                internal.update(cx, |app, cx| {
                    app.execute_drag_hover_drop_command(
                        DragHoverDropCommand::InsertBookmarks(index),
                        drag.paths.clone(),
                        true,
                    );
                    cx.notify();
                });
            })
            .on_drop::<gpui::ExternalPaths>(move |paths, window, cx| {
                if !crate::ui::sidebar::is_sidebar_drop_allowed(
                    &external,
                    window.mouse_position(),
                    cx,
                ) {
                    return;
                }
                external.update(cx, |app, cx| {
                    app.execute_drag_hover_drop_command(
                        DragHoverDropCommand::InsertBookmarks(index),
                        paths.paths().to_vec(),
                        false,
                    );
                    cx.notify();
                });
            })
            .on_drop::<BookmarkDrag>(move |drag, window, cx| {
                if !crate::ui::sidebar::is_sidebar_drop_allowed(
                    &reorder,
                    window.mouse_position(),
                    cx,
                ) {
                    return;
                }
                reorder.update(cx, |app, cx| {
                    if app.state.profiles.active().name != drag.profile {
                        return;
                    }
                    app.mark_drag_committed();
                    insert_references(
                        &mut app.state.profiles.active_mut().bookmarks,
                        &[drag.path.clone()],
                        index,
                    );
                    atlas_core::util::config::save_profiles(&app.state.profiles);
                    cx.notify();
                });
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use atlas_model::profiles::Bookmark;

    fn bookmarks() -> Vec<Bookmark> {
        ["a", "b", "c"]
            .into_iter()
            .map(|path| Bookmark::new(format!("Custom {path}"), path.into()))
            .collect()
    }

    #[test]
    fn reorder_uses_the_original_boundary_and_preserves_custom_names() {
        let mut items = bookmarks();
        insert_references(&mut items, &["a".into()], 3);
        assert_eq!(
            items
                .iter()
                .map(|b| b.path.to_str().unwrap())
                .collect::<Vec<_>>(),
            ["b", "c", "a"]
        );
        assert_eq!(items[2].name, "Custom a");
        insert_references(&mut items, &["a".into()], 0);
        assert_eq!(items[0].path, PathBuf::from("a"));
        assert_eq!(items.len(), 3);
    }

    #[test]
    fn batch_insertion_deduplicates_and_keeps_the_incoming_order() {
        let mut items = bookmarks();
        insert_references(&mut items, &["b".into(), "new".into(), "b".into()], 1);
        assert_eq!(
            items
                .iter()
                .map(|b| b.path.to_str().unwrap())
                .collect::<Vec<_>>(),
            ["a", "b", "new", "c"]
        );
        assert_eq!(items[1].name, "Custom b");
        insert_references(&mut items, &["last".into()], usize::MAX);
        assert_eq!(items.last().unwrap().path, PathBuf::from("last"));
    }

    #[test]
    fn dropping_at_own_boundary_does_not_change_order_or_metadata() {
        let mut items = bookmarks();
        let before: Vec<_> = items
            .iter()
            .map(|b| (b.name.clone(), b.path.clone()))
            .collect();
        insert_references(&mut items, &["b".into()], 2);
        assert_eq!(
            before,
            items
                .iter()
                .map(|b| (b.name.clone(), b.path.clone()))
                .collect::<Vec<_>>()
        );
    }
}

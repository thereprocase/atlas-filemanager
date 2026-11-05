//! Window chrome: tab pills, breadcrumb strip, floating search pill, rubber marquee.
#![allow(clippy::too_many_lines)] // reason: cohesive UI block kept whole

use crate::app::App;
use crate::input::action::Action;
use crate::ui::drag_hover::{
    drag_hover_recorder, drag_hover_recorder_with_drop, DragHoverCommand, DragHoverDropCommand,
    DragHoverId, DragHoverKind,
};
use crate::ui::glass::{control_colors, control_hover, glass_shadows_enabled, GlassExt};
use crate::ui::icons::{is_windows_theme, theme_icon, AtlasIcon};
use crate::ui::motion::MotionFeedbackExt as _;
use crate::ui::search_shell::search_icon;
use crate::ui::tooltips::shortcut_tooltip;
use atlas_model::location::Location;
use atlas_model::pane::TabId;
use atlas_model::settings::ViewMode;
use gpui::prelude::*;
use gpui::ColorExt as _;
use gpui::{
    div, px, AnimationExt, AnyElement, Context, Entity, Hsla, MouseButton, SharedString, Window,
};
use gpui_component::button::ButtonVariants as _;
use gpui_component::menu::{ContextMenuExt, DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_component::slider::{Slider, SliderEvent, SliderState, SliderValue};
use gpui_component::{h_flex, v_flex, ActiveTheme, ElementExt, Icon, Sizable as _};
use std::collections::HashSet;
use std::path::PathBuf;

/// Shared height of the three floating capsules (filter, branch, controls).
const PILL_HEIGHT: f32 = 30.;

impl App {
    #[allow(dead_code)]
    pub(crate) fn render_floating_search_pill(&self, cx: &mut Context<Self>) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let active_theme = self.state.profiles.active().theme.as_str();
        let is_glass = active_theme.ends_with(" Glass");
        let controls = control_colors(cx.theme(), is_glass);
        let windows_theme = is_windows_theme(active_theme);
        let bg = controls.surface;
        div()
            .absolute()
            .left_0()
            .right_0()
            .bottom(crate::ui::rem_px(18.))
            .flex()
            .justify_center()
            // The wrapper is non-interactive so only the pill captures clicks.
            .child(
                h_flex()
                    .id("search-pill")
                    .h(crate::ui::rem_px(36.))
                    .px_4()
                    .gap_2()
                    .items_center()
                    .rounded_full()
                    .bg(bg)
                    .when(!is_glass, |s| {
                        s.border_1().border_color(cx.theme().border.opacity(0.60))
                    })
                    .when(glass_shadows_enabled(), |s| s.shadow_lg())
                    .cursor_pointer()
                    .text_color(muted)
                    .motion_feedback()
                    .hover(move |s| s.bg(bg.blend(&controls.hover)))
                    .active(move |s| s.bg(bg.blend(&controls.pressed)))
                    .child(if windows_theme {
                        theme_icon(AtlasIcon::Search, 15.0, muted, true)
                    } else {
                        search_icon(muted)
                    })
                    .child(rust_i18n::t!("search"))
                    .on_click(cx.listener(|this, _ev, _window, cx| {
                        this.process_actions(vec![Action::OpenGotoBar]);
                        cx.notify();
                    })),
            )
            .into_any_element()
    }

    /// Phase C: the visible selection rectangle for the active drag, in window coords
    /// (same space as `row_rects`), painted as a deferred layer over the panes. Hidden
    /// until the drag actually moves, so a plain click doesn't flash a box.
    pub(crate) fn render_rubber_marquee(&self, _cx: &mut Context<Self>) -> Option<AnyElement> {
        // Drawing is now handled natively via canvas in `interactions.rs` (rubber_layer).
        // This avoids DOM layout issues with GPUI's deferred overlays.
        None
    }

    /// Toggle for immediate-mode experimental chrome (O(1) rect-slicing).
    /// Enabled for testing.
    pub const EXPERIMENTAL_IMM_CHROME: bool = true;

    /// Compact breadcrumb strip rendered at the top of each pane, showing the
    /// pane's current path. Clicking any segment navigates that specific pane.
    pub(crate) fn render_crumb_strip(
        &self,
        pane_id: TabId,
        view: &Entity<App>,
        fg: Hsla,
        muted: Hsla,
        border: Hsla,
        header_bg: Hsla,
        cx: &gpui::App,
    ) -> AnyElement {
        let body = self.render_crumb_strip_body(pane_id, view, fg, muted, border, header_bg);
        let commands = self.pinned_header_commands(cx);
        let loading = self.state.tab_states.get(&pane_id).is_some_and(|state|
            matches!(state.load_state, atlas_model::tab::LoadState::Loading) ||
            state.miller.columns.iter().any(|column| matches!(column.load_state, atlas_model::tab::LoadState::Loading))) ||
            self.tree_loading.iter().any(|(pane, _)| *pane == pane_id);
        let watcher_failed = self.tab_data_for(pane_id).is_some_and(|tab|
            matches!(tab.location, Location::Directory(_))) && !self.watchers.contains_key(&pane_id);
        let mut tools = h_flex().items_center().gap_1().flex_shrink_0();
        if loading {
            tools = tools.child(crate::ui::loading_spinner::loading_spinner(muted,
                !self.state.settings.disable_animations && !cx.reduce_motion()));
        }
        if watcher_failed {
            let app = view.clone();
            tools = tools.child(gpui_component::button::Button::new(
                SharedString::from(format!("watcher-refresh-{}", pane_id.0)))
                .ghost().small().icon(Icon::empty().path(AtlasIcon::ArrowClockwise.path()))
                .tooltip(rust_i18n::t!("watcher_auto_refresh_disabled_help").into_owned())
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(move |_, _, cx| {
                    app.update(cx, |this, cx| {
                        this.process_actions(vec![Action::Refresh(pane_id)]);
                        this.notify_all_panes(cx);
                        cx.notify();
                    });
                }));
        }
        for (id, title, icon) in commands {
            let app = view.clone();
            tools = tools.child(gpui_component::button::Button::new(
                SharedString::from(format!("pinned-{}-{id}", pane_id.0)))
                .ghost().small().icon(Icon::empty().path(icon.path()))
                .tooltip(title)
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(move |_, _, cx| {
                    app.update(cx, |this, cx| {
                        this.run_pinned_header_command(pane_id, &id, cx);
                        this.notify_all_panes(cx);
                        cx.notify();
                    });
                }));
        }
        h_flex().w_full().min_w(px(0.0)).items_center().bg(header_bg)
            .child(div().flex_1().min_w(px(0.0)).child(body))
            .child(div().id(SharedString::from(format!("pane-tools-{}", pane_id.0)))
                .max_w(gpui::relative(0.5)).min_w(px(0.0)).overflow_x_scroll().child(tools))
            .into_any_element()
    }

    fn render_crumb_strip_body(
        &self,
        pane_id: TabId,
        view: &Entity<App>,
        fg: Hsla,
        muted: Hsla,
        border: Hsla,
        header_bg: Hsla,
    ) -> AnyElement {
        let pane_archive_inner = self.tab_data_for(pane_id).and_then(|t| {
            t.location
                .as_archive()
                .map(|(archive, inner)| (archive.to_path_buf(), inner.to_path_buf()))
        });
        let pane_location = self.tab_data_for(pane_id).map(|t| t.location.clone());
        let (can_back, can_forward, up_location, recent_locations, bookmark_target) = self
            .tab_data_for(pane_id)
            .map(|t| {
                (
                    !t.history.is_empty(),
                    !t.history_forward.is_empty(),
                    t.location.ascend(),
                    self.recent_locations_for_pane(pane_id)
                        .into_iter()
                        .map(|loc| (self.resolved_location_label(&loc), loc))
                        .collect::<Vec<_>>(),
                    self.bookmark_target_for_pane(pane_id),
                )
            })
            .unwrap_or_default();
        let active_theme = self.state.profiles.active().theme.as_str();
        let windows_theme = is_windows_theme(active_theme);
        let is_glass = active_theme.ends_with(" Glass");
        let is_branch_view = self.tab_data_for(pane_id).is_some_and(|t| t.is_branch_view);
        let button_hover = control_hover(fg);

        if Self::EXPERIMENTAL_IMM_CHROME {
            if let Some(Location::Directory(dir)) = pane_location.as_ref() {
                let trail = self.tab_data_for(pane_id).and_then(|tab| tab.breadcrumb_trail.as_deref());
                let segments = std::sync::Arc::new(crate::ui::chrome_imm::path_with_ghost_segments(dir, trail));
                let is_bookmarked = if let Some(path) = self
                    .tab_data_for(pane_id)
                    .and_then(|t| t.location.fs_dir().map(|p| p.to_path_buf()))
                {
                    self.state
                        .profiles
                        .active()
                        .bookmarks
                        .iter()
                        .any(|b| b.path == path)
                } else {
                    false
                };
                let strip_bg = if is_glass {
                    header_bg
                } else {
                    border.opacity(0.03)
                };
                return crate::ui::chrome_imm::ImmChromeElement::new(
                    pane_id,
                    view.downgrade(),
                    segments,
                    can_back,
                    can_forward,
                    up_location.is_some(),
                    is_bookmarked,
                    windows_theme,
                    is_glass,
                    fg,
                    muted,
                    strip_bg,
                    border,
                    button_hover,
                    fg,
                )
                .into_any_element();
            }
        }

        let crumbs: Option<AnyElement> = if let Some((archive, inner)) = pane_archive_inner.as_ref()
        {
            Some(
                archive_breadcrumb(
                    view,
                    pane_id,
                    archive,
                    inner,
                    fg,
                    muted,
                    button_hover,
                    windows_theme,
                )
                .into_any_element(),
            )
        } else {
            pane_location.as_ref().map(|location| match location {
                Location::Directory(p) => {
                    breadcrumb(view, pane_id, p, fg, muted, button_hover, windows_theme, is_branch_view)
                        .into_any_element()
                }
                #[cfg(feature = "provider-host")]
                Location::Remote { id, path } => {
                    let remote_label = self
                        .state
                        .cloud
                        .remote(id)
                        .map(|r| r.name.clone())
                        .unwrap_or_else(|| id.clone());
                    remote_breadcrumb(
                        view,
                        pane_id,
                        id,
                        &remote_label,
                        path,
                        fg,
                        muted,
                        button_hover,
                        windows_theme,
                    )
                    .into_any_element()
                }
                // Virtual locations (tag results, recycle bin) have no
                // filesystem ancestors to climb — show a static labelled crumb
                // instead of a clickable path that would navigate nowhere.
                other => h_flex()
                    .min_w(px(0.))
                    .gap(crate::ui::rem_px(6.))
                    .items_center()
                    .text_color(fg)
                    .child(crate::ui::icons::icon(location_icon(other), 13.0, muted))
                    .child(div().truncate().child(self.resolved_location_label(other)))
                    .into_any_element(),
            })
        };
        let strip_bg = if is_glass {
            header_bg
        } else {
            border.opacity(0.03)
        };
        let finder_view = view.clone();
        h_flex()
            .w_full()
            .h(crate::ui::rem_px(if windows_theme { 44.0 } else { 36.0 }))
            .px_3()
            .min_w(px(0.))
            .gap(crate::ui::rem_px(if windows_theme { 9.0 } else { 6.0 }))
            .items_center()
            .bg(strip_bg)
            .when(!is_glass, |s| {
                s.border_b_1().border_color(border.opacity(0.45))
            })
            .when(!windows_theme, |s| s.text_sm())
            .when(windows_theme, |s| s.text_base())
            .text_color(muted)
            .cursor_pointer()
            .on_mouse_down(MouseButton::Left, move |_ev, _window, cx| {
                finder_view.update(cx, |this, cx| {
                    this.activate_tab(pane_id);
                    this.open_location_finder_or_editor();
                    cx.notify();
                });
            })
            .child(nav_button(NavButtonProps {
                id: "pane-back",
                pane_id,
                glyph: AtlasIcon::ArrowLeft,
                tooltip_title: rust_i18n::t!("tooltip_nav_back_title").to_string(),
                tooltip_desc: rust_i18n::t!("tooltip_nav_back_desc").to_string(),
                shortcuts: &["alt-left"],
                enabled: can_back,
                action: Action::NavigateBack(pane_id),
                view: view.clone(),
                recent: recent_locations.clone(),
                icon_color: fg,
                hover_bg: button_hover,
                windows_theme,
            }))
            .child(nav_button(NavButtonProps {
                id: "pane-forward",
                pane_id,
                glyph: AtlasIcon::ArrowRight,
                tooltip_title: rust_i18n::t!("tooltip_nav_forward_title").to_string(),
                tooltip_desc: rust_i18n::t!("tooltip_nav_forward_desc").to_string(),
                shortcuts: &["alt-right"],
                enabled: can_forward,
                action: Action::NavigateForward(pane_id),
                view: view.clone(),
                recent: recent_locations.clone(),
                icon_color: fg,
                hover_bg: button_hover,
                windows_theme,
            }))
            .child(nav_button(NavButtonProps {
                id: "pane-up",
                pane_id,
                glyph: AtlasIcon::ArrowUp,
                tooltip_title: rust_i18n::t!("tooltip_nav_up_title").to_string(),
                tooltip_desc: rust_i18n::t!("tooltip_nav_up_desc").to_string(),
                shortcuts: &["alt-up"],
                enabled: up_location.is_some(),
                action: Action::NavigateUp(pane_id),
                view: view.clone(),
                recent: recent_locations.clone(),
                icon_color: fg,
                hover_bg: button_hover,
                windows_theme,
            }))
            .child(bookmark_button(
                pane_id,
                view.clone(),
                bookmark_target.clone(),
                fg,
                button_hover,
                windows_theme,
            ))
            .child(
                div()
                    .flex_none()
                    .w(px(1.))
                    .h(crate::ui::rem_px(if windows_theme { 22.0 } else { 18.0 }))
                    .bg(border.opacity(0.35)),
            )
            .children(crumbs)
            .into_any_element()
    }

    /// User-facing label for a location: tag names resolved from the tag cache
    /// and the recycle bin localized (never the `tag:N` sentinel path).
    pub(crate) fn resolved_location_label(&self, location: &Location) -> SharedString {
        match location {
            Location::Tag(id) => self
                .state
                .tag_cache
                .def(*id)
                .map_or_else(|| location_label(location), |def| def.name.clone().into()),
            Location::RecycleBin => rust_i18n::t!("sidebar_recycle_bin").into_owned().into(),
            _ => location_label(location),
        }
    }

    fn recent_locations_for_pane(&self, pane_id: TabId) -> Vec<Location> {
        let Some(tab) = self.tab_data_for(pane_id) else {
            return Vec::new();
        };
        let mut seen = HashSet::new();
        let mut locations = Vec::new();
        seen.insert(tab.location.clone());
        for loc in tab
            .history
            .iter()
            .rev()
            .chain(tab.history_forward.iter().rev())
        {
            if seen.insert(loc.clone()) {
                locations.push(loc.clone());
                if locations.len() == 6 {
                    return locations;
                }
            }
        }
        for path in self.recent_folders().iter().rev() {
            let loc = Location::Directory(path.clone());
            if seen.insert(loc.clone()) {
                locations.push(loc);
                if locations.len() == 6 {
                    break;
                }
            }
        }
        locations
    }

    fn bookmark_target_for_pane(&self, pane_id: TabId) -> Option<BookmarkTarget> {
        let path = self
            .tab_data_for(pane_id)
            .and_then(|t| t.location.fs_dir().map(|p| p.to_path_buf()))?;
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| path.display().to_string());
        let index = self
            .state
            .profiles
            .active()
            .bookmarks
            .iter()
            .position(|bookmark| bookmark.path == path);
        Some(BookmarkTarget {
            name,
            path,
            exists: index.is_some(),
            index,
        })
    }

    /// Floating per-pane overlays, anchored to the bottom corners of the file
    /// list: a compact fuzzy-filter pill on the left (a lone magnifier until it
    /// activates) and a controls pill on the right (visibility toggles,
    /// folders-first, view selector). Both sit on translucent glass so the
    /// listing shows through.
    pub(crate) fn render_pane_pills(
        &mut self,
        pane_id: TabId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> (AnyElement, AnyElement) {
        // Mirror the per-tab toggles into the TabState so its cached filtering
        // (and the counts below) pick them up.
        let (folder_order, show_files, show_folders, is_branch_view) = self
            .tab_data_for(pane_id)
            .map(|td| (td.folder_order, td.show_files, td.show_folders, td.is_branch_view))
            .unwrap_or((atlas_model::pane::FolderOrder::default(), true, true, false));
        if let Some(ts) = self.state.tab_states.get_mut(&pane_id) {
            ts.set_visibility(show_files, show_folders);
            ts.is_branch_view = is_branch_view;
            ts.refresh_miller_visibility();
        }
        let flat_counts = self.flat_visible_counts(pane_id);

        let initial = self
            .state
            .tab_states
            .get(&pane_id)
            .map(|ts| ts.search.query.clone())
            .unwrap_or_default();
        let input = self
            .search_inputs
            .entry(pane_id)
            .or_insert_with(|| {
                crate::ui::text_input::single_line(
                    window,
                    cx,
                    rust_i18n::t!("filter_placeholder").into_owned(),
                    initial,
                )
            })
            .clone();

        let value = input.read(cx).value().to_string();
        let mut needs_refresh = false;
        if let Some(ts) = self.state.tab_states.get_mut(&pane_id) {
            // Focus is owned by `sync_overlay_focus`: while `search.active` the
            // field keeps the keyboard, so no per-render focus juggling here.
            ts.search.focus = false;
            if value != ts.search.query {
                ts.search.query.clone_from(&value);
                // Typing directly into the always-visible input activates the filter.
                if !value.is_empty() {
                    ts.search.active = true;
                }
                needs_refresh = true;
            }
        }
        if needs_refresh {
            self.refresh_search_in_place(pane_id);
        }

        let (active, _recursive, status, count, error) = self
            .state
            .tab_states
            .get_mut(&pane_id)
            .map(|ts| {
                (
                    ts.search.active,
                    ts.search.recursive,
                    ts.search.status,
                    ts.visible_indices().len(),
                    ts.search.error.clone(),
                )
            })
            .unwrap_or_default();
        let filtering = active && !value.trim().is_empty();
        let is_glass = self.state.profiles.active().theme.ends_with(" Glass");
        let controls = control_colors(
            cx.theme(),
            is_glass,
        );
        let pill_surface = if is_glass {
            controls.surface.opacity(0.55)
        } else {
            controls.surface
        };
        let (border, muted, _fg, accent, accent_fg) = {
            let t = cx.theme();
            (
                t.border,
                t.muted_foreground,
                t.foreground,
                t.accent,
                t.accent_foreground,
            )
        };
        let view = cx.entity();
        // Concise status inside the pill: result count, "…" while searching,
        // "!" (with the error in its own right) on failure.
        let status_text = if error.is_some() {
            Some("!".to_string())
        } else if status == atlas_model::tab::SearchStatus::Searching {
            Some("…".to_string())
        } else if filtering {
            Some(format!("{count}"))
        } else {
            None
        };
        let windows_theme = is_windows_theme(&self.state.profiles.active().theme);

        // Only the active pane shows the floating chrome. An inactive pane that
        // still has a filter applied keeps a tiny badge so it's obvious why
        // entries are missing; clicking it focuses the pane and the field.
        let focused = self.state.focused_tile == pane_id;
        if !focused {
            let badge: AnyElement = if active {
                let open_view = view.clone();
                h_flex()
                    .id(SharedString::from(format!("filter-badge-{}", pane_id.0)))
                    .absolute()
                    .bottom_2()
                    .left_2()
                    .px_2()
                    .py(crate::ui::rem_px(3.))
                    .gap_1()
                    .items_center()
                    .rounded_full()
                    .border_1()
                    .border_color(controls.border)
                    .bg(pill_surface)
                    .when(is_glass, |s| s.backdrop_blur(px(12.0)))
                    .raised_surface()
                    .text_xs()
                    .cursor_pointer()
                    .child(theme_icon(AtlasIcon::Search, 12.0, accent, windows_theme))
                    .children(
                        status_text.map(|t| div().text_color(muted).whitespace_nowrap().child(t)),
                    )
                    .on_mouse_down(MouseButton::Left, move |_ev, _window, cx| {
                        cx.stop_propagation();
                        open_view.update(cx, |this, cx| {
                            this.activate_tab(pane_id);
                            if let Some(ts) = this.state.tab_states.get_mut(&pane_id) {
                                ts.search.focus = true;
                            }
                            cx.notify();
                        });
                    })
                    .into_any_element()
            } else {
                div().into_any_element()
            };
            return (badge, div().into_any_element());
        }

        // ── Left pill: fuzzy filter (lone magnifier until active) ──
        let filter_pill: AnyElement = if active {
            let close_view = view.clone();
            h_flex()
                .id(SharedString::from(format!("filter-pill-{}", pane_id.0)))
                .absolute()
                .bottom_2()
                .left_2()
                .px_2()
                .h(crate::ui::rem_px(PILL_HEIGHT))
                .gap_1()
                .items_center()
                .rounded_xl()
                .border_1()
                .border_color(if filtering {
                    accent.opacity(0.8)
                } else {
                    controls.border
                })
                .bg(pill_surface)
                .when(is_glass, |s| s.backdrop_blur(px(12.0)))
                .raised_surface()
                .text_xs()
                // Keep pill clicks away from the pane underneath (rubber band,
                // pane-focus click handlers).
                .on_mouse_down(MouseButton::Left, |_ev, _window, cx| {
                    cx.stop_propagation();
                })
                .capture_key_down(
                    cx.listener(move |this, ev: &gpui::KeyDownEvent, _window, cx| {
                        let handled = match ev.keystroke.key.as_str() {
                            "escape" => {
                                this.activate_tab(pane_id);
                                this.process_actions(vec![Action::CloseSearchInPlace]);
                                true
                            }
                            "enter" => {
                                this.open_cursor();
                                true
                            }
                            "down" => {
                                this.nav_down(false, false);
                                true
                            }
                            "up" => {
                                this.nav_up(false, false);
                                true
                            }
                            _ => false,
                        };
                        if handled {
                            cx.stop_propagation();
                            cx.notify();
                        }
                    }),
                )
                .child(theme_icon(
                    AtlasIcon::Search,
                    14.0,
                    if filtering { accent } else { muted },
                    windows_theme,
                ))
                .child({
                    let field = div()
                        .overflow_hidden()
                        .child(div().w(crate::ui::rem_px(180.)).child(crate::ui::text_input::flat(&input).xsmall()));
                    // The capsule grows out of the lone magnifier when it opens.
                    if self.state.settings.disable_animations {
                        field.w(crate::ui::rem_px(180.)).into_any_element()
                    } else {
                        field
                            .with_animation(
                                SharedString::from(format!("filter-open-{}", pane_id.0)),
                                crate::ui::motion::animation(crate::ui::motion::MICRO_MS * 2),
                                |this, t| {
                                    let t = 1.0 - (1.0 - t).powi(3);
                                    this.w(crate::ui::rem_px(180. * t)).opacity(t)
                                },
                            )
                            .into_any_element()
                    }
                })
                .child({
                    let branch_view_action = view.clone();
                    div()
                        .id(SharedString::from(format!("filter-branch-{}", pane_id.0)))
                        .p(crate::ui::rem_px(3.))
                        .rounded_md()
                        .cursor_pointer()
                        .bg(controls.hover.opacity(0.0))
                        .when(is_branch_view, |s| s.bg(accent))
                        .motion_feedback()
                        .when(!is_branch_view, |s| s.hover(move |s| s.bg(controls.hover)))
                        .child(theme_icon(
                            AtlasIcon::GitBranch,
                            14.0,
                            if is_branch_view { accent_fg } else { muted },
                            windows_theme,
                        ))
                        .tooltip(|window, cx| {
                            gpui_component::tooltip::Tooltip::new(
                                rust_i18n::t!("cmd_view_branch_view_title").into_owned(),
                            )
                            .build(window, cx)
                        })
                        .on_mouse_down(MouseButton::Left, move |_ev, _window, cx| {
                            branch_view_action.update(cx, |this, cx| {
                                this.process_actions(vec![Action::ToggleBranchView(pane_id)]);
                                cx.notify();
                            });
                        })
                })
                .children(status_text.map(|t| {
                    let tip = error.clone();
                    div()
                        .id(SharedString::from(format!("filter-status-{}", pane_id.0)))
                        .text_color(if tip.is_some() { gpui::red() } else { muted })
                        .whitespace_nowrap()
                        .when_some(tip, |s, tip| {
                            s.tooltip(move |window, cx| {
                                gpui_component::tooltip::Tooltip::new(tip.clone()).build(window, cx)
                            })
                        })
                        .child(t)
                }))
                .child(
                    div()
                        .id(SharedString::from(format!("filter-close-{}", pane_id.0)))
                        .p(crate::ui::rem_px(3.))
                        .rounded_md()
                        .cursor_pointer()
                        .bg(controls.hover.opacity(0.0))
                        .motion_feedback()
                        .hover(move |s| s.bg(controls.hover))
                        .active(move |s| s.bg(controls.pressed))
                        .child(theme_icon(AtlasIcon::X, 12.0, muted, windows_theme))
                        .on_mouse_down(MouseButton::Left, move |_ev, _window, cx| {
                            close_view.update(cx, |this, cx| {
                                this.process_actions(vec![Action::CloseSearchInPlace]);
                                cx.notify();
                            });
                        }),
                )
                .into_any_element()
        } else {
            let open_view = view.clone();
            let branch_toggle_view = view.clone();
            h_flex()
                .id(SharedString::from(format!("filter-pill-{}", pane_id.0)))
                .absolute()
                .bottom_2()
                .left_2()
                .px_1()
                .h(crate::ui::rem_px(PILL_HEIGHT))
                .gap_1()
                .items_center()
                .rounded_xl()
                .border_1()
                .border_color(controls.border)
                .bg(pill_surface)
                .when(is_glass, |s| s.backdrop_blur(px(12.0)))
                .raised_surface()
                // 1. Search button
                .child(
                    div()
                        .id(SharedString::from(format!("filter-btn-search-{}", pane_id.0)))
                        .p(crate::ui::rem_px(4.))
                        .rounded_md()
                        .cursor_pointer()
                        .motion_feedback()
                        .hover(move |s| s.bg(controls.hover))
                        .active(move |s| s.bg(controls.pressed))
                        .child(theme_icon(
                            AtlasIcon::Search,
                            14.0,
                            controls.foreground,
                            windows_theme,
                        ))
                        .tooltip(|window, cx| {
                            gpui_component::tooltip::Tooltip::new(
                                rust_i18n::t!("tooltip_filter_files").into_owned(),
                            )
                            .build(window, cx)
                        })
                        .on_mouse_down(MouseButton::Left, move |_ev, _window, cx| {
                            cx.stop_propagation();
                            open_view.update(cx, |this, cx| {
                                this.activate_tab(pane_id);
                                this.process_actions(vec![Action::OpenSearchInPlace]);
                                cx.notify();
                            });
                        }),
                )
                // 2. Subtle vertical separator
                .child(
                    div()
                        .w(px(1.))
                        .h(crate::ui::rem_px(14.))
                        .bg(border.opacity(0.35)),
                )
                // 3. Branch View button
                .child(
                    div()
                        .id(SharedString::from(format!("filter-btn-branch-{}", pane_id.0)))
                        .p(crate::ui::rem_px(4.))
                        .rounded_md()
                        .cursor_pointer()
                        .motion_feedback()
                        .when(is_branch_view, |s| s.bg(accent))
                        .when(!is_branch_view, |s| s.hover(move |s| s.bg(controls.hover)))
                        .active(move |s| s.bg(controls.pressed))
                        .child(theme_icon(
                            AtlasIcon::GitBranch,
                            14.0,
                            if is_branch_view { accent_fg } else { controls.foreground },
                            windows_theme,
                        ))
                        .tooltip(|window, cx| {
                            gpui_component::tooltip::Tooltip::new("Branch View (Ctrl+B)".to_string())
                                .build(window, cx)
                        })
                        .on_mouse_down(MouseButton::Left, move |_ev, _window, cx| {
                            cx.stop_propagation();
                            branch_toggle_view.update(cx, |this, cx| {
                                this.activate_tab(pane_id);
                                this.process_actions(vec![Action::ToggleBranchView(pane_id)]);
                                cx.notify();
                            });
                        }),
                )
                .into_any_element()
        };

        // ── Right pill: visibility toggles + folders-first + view selector ──
        let pane_mode = self.tab_data_for(pane_id).map(|tab| tab.view_mode.clone()).unwrap_or_default();
        let (counts, listing_elapsed) = self.state.tab_states.get_mut(&pane_id).map(|tab| {
            tab.refresh_miller_visibility();
            let counts = if pane_mode == ViewMode::Miller {
                tab.miller.columns.get(tab.miller.focused).map_or(Default::default(), |column|
                    column.visible_counts(&tab.filter, tab.show_files, tab.show_folders))
            } else { flat_counts };
            (counts, tab.listing_elapsed)
        }).unwrap_or_default();
        let folder_label = counts.folders_shown.to_string();
        let file_label = counts.files_shown.to_string();
        let tbtn = |id: String,
                    glyph: AtlasIcon,
                    on: bool,
                    on_color: Hsla,
                    tip: String,
                    action: Action,
                    view: Entity<App>| {
            div()
                .id(SharedString::from(id))
                .flex().items_center().gap_1()
                .p(crate::ui::rem_px(4.))
                .rounded_md()
                .cursor_pointer()
                .bg(controls.hover.opacity(0.0))
                .when(on, |s| s.bg(accent.opacity(0.12)))
                .motion_feedback()
                .hover(move |s| s.bg(controls.hover))
                .active(move |s| s.bg(controls.pressed))
                .child(theme_icon(
                    glyph,
                    14.0,
                    if on { on_color } else { controls.foreground },
                    windows_theme,
                ))
                .tooltip(move |window, cx| {
                    gpui_component::tooltip::Tooltip::new(tip.clone()).build(window, cx)
                })
                .on_mouse_down(MouseButton::Left, move |_ev, _window, cx| {
                    view.update(cx, |this, cx| {
                        this.process_actions(vec![action.clone()]);
                        this.flush_dirty_panes(cx);
                    });
                })
        };

        let view_mode = self
            .tab_data_for(pane_id)
            .map(|t| t.view_mode.clone())
            .unwrap_or_default();
        let view_icon = match view_mode {
            ViewMode::Details => AtlasIcon::Details,
            ViewMode::Grid => AtlasIcon::Grid,
            ViewMode::List => AtlasIcon::List,
            ViewMode::Miller => AtlasIcon::Columns,
            ViewMode::Tree => AtlasIcon::TreeStructure,
        };
        let view_label = match view_mode {
            ViewMode::Details => rust_i18n::t!("view_details").into_owned(),
            ViewMode::Grid => rust_i18n::t!("view_grid").into_owned(),
            ViewMode::List => rust_i18n::t!("view_list").into_owned(),
            ViewMode::Miller => rust_i18n::t!("view_columns").into_owned(),
            ViewMode::Tree => rust_i18n::t!("view_tree").into_owned(),
        };
        let view_slider_value = self.pane_view_slider_value(pane_id);
        let zoom_slider = if let Some(slider) = self.pane_zoom_sliders.get(&pane_id) {
            slider.clone()
        } else {
            let slider = cx.new(|_| {
                SliderState::new()
                    .min(0.0)
                    .max(23.0)
                    .step(1.0)
                    .default_value(view_slider_value)
            });
            let sub = cx.subscribe_in(
                &slider,
                window,
                move |this, _, ev: &SliderEvent, _window, cx| {
                    let value = match ev {
                        SliderEvent::Change(value) | SliderEvent::Release(value) => value.end(),
                    }
                    .round()
                    .clamp(0.0, 23.0);
                    if this.set_pane_view_from_slider_value(pane_id, value) {
                        this.notify_pane(pane_id, cx);
                    }
                    cx.notify();
                },
            );
            self.pane_zoom_sliders.insert(pane_id, slider.clone());
            self.pane_zoom_slider_subs.insert(pane_id, sub);
            slider
        };
        let slider_value = zoom_slider.read(cx).value().end();
        if (slider_value - view_slider_value).abs() > f32::EPSILON {
            zoom_slider.update(cx, |slider, cx| {
                slider.set_value(SliderValue::Single(view_slider_value), window, cx);
            });
        }
        let zoom_slider_for_menu = zoom_slider.clone();
        let menu_view = view.clone();
        let view_btn = gpui_component::button::Button::new(SharedString::from(format!(
            "view-select-{}",
            pane_id.0
        )))
        .ghost()
        .xsmall()
        .child(theme_icon(view_icon, 14.0, controls.foreground, windows_theme))
        .tooltip(format!("{} - Ctrl+scroll", view_label))
        // Anchored to the bottom so the menu opens upward, above the icon.
        .dropdown_menu_with_anchor(
            gpui::Anchor::BottomRight,
            move |menu, _window, _cx| {
                let slider = zoom_slider_for_menu.clone();
                let view = menu_view.clone();
                menu.item(
                    PopupMenuItem::element(move |_: &mut Window, cx| {
                        let active_mode = view
                            .read(cx)
                            .tab_data_for(pane_id)
                            .map(|pane| pane.view_mode.clone())
                            .unwrap_or_default();
                        let row = |glyph: AtlasIcon,
                                   label: String,
                                   mode: ViewMode,
                                   active: bool,
                                   view: Entity<App>,
                                   slider: Entity<SliderState>| {
                            h_flex()
                                .id(SharedString::from(format!("view-mode-row-{}-{mode:?}", pane_id.0)))
                                .gap_1()
                                .items_center()
                                .cursor_pointer()
                                .rounded_md()
                                .px_2()
                                .h(px(24.0))
                                .motion_feedback()
                                .hover(move |this| this.bg(controls.hover))
                                .when(active, |this| this.bg(accent.opacity(0.14)))
                                .child(theme_icon(
                                    glyph,
                                    14.0,
                                    if active { accent } else { controls.foreground },
                                    windows_theme,
                                ))
                                .child(
                                    div()
                                        .w(px(82.0))
                                        .text_xs()
                                        .text_color(if active {
                                            cx.theme().foreground
                                        } else {
                                            cx.theme().muted_foreground
                                        })
                                        .child(label),
                                )
                                .on_mouse_down(MouseButton::Left, move |_ev, window, cx| {
                                    cx.stop_propagation();
                                    let mut slider_value = None;
                                    view.update(cx, |this, cx| {
                                        this.activate_tab(pane_id);
                                        if this.set_pane_view_mode(pane_id, mode.clone()) {
                                            this.notify_pane(pane_id, cx);
                                        }
                                        slider_value = Some(this.pane_view_slider_value(pane_id));
                                        cx.notify();
                                    });
                                    if let Some(slider_value) = slider_value {
                                        slider.update(cx, |state, cx| {
                                            state.set_value(
                                                SliderValue::Single(slider_value),
                                                window,
                                                cx,
                                            );
                                        });
                                    }
                                })
                        };
                        h_flex()
                            .gap_2()
                            .items_center()
                            .p_1()
                            .child(
                                v_flex()
                                    .gap_0p5()
                                    .child(row(
                                        AtlasIcon::Columns,
                                        rust_i18n::t!("view_columns").into_owned(),
                                        ViewMode::Miller,
                                        active_mode == ViewMode::Miller,
                                        view.clone(),
                                        slider.clone(),
                                    ))
                                    .child(div().h(px(1.0)).mx_1().bg(border.opacity(0.35)))
                                    .child(row(
                                        AtlasIcon::Grid,
                                        rust_i18n::t!("view_grid").into_owned(),
                                        ViewMode::Grid,
                                        active_mode == ViewMode::Grid,
                                        view.clone(),
                                        slider.clone(),
                                    ))
                                    .child(row(
                                        AtlasIcon::List,
                                        rust_i18n::t!("view_list").into_owned(),
                                        ViewMode::List,
                                        active_mode == ViewMode::List,
                                        view.clone(),
                                        slider.clone(),
                                    ))
                                    .child(row(
                                        AtlasIcon::TreeStructure,
                                        rust_i18n::t!("view_tree").into_owned(),
                                        ViewMode::Tree,
                                        active_mode == ViewMode::Tree,
                                        view.clone(),
                                        slider.clone(),
                                    ))
                                    .child(row(
                                        AtlasIcon::Details,
                                        rust_i18n::t!("view_details").into_owned(),
                                        ViewMode::Details,
                                        active_mode == ViewMode::Details,
                                        view.clone(),
                                        slider.clone(),
                                    )),
                            )
                            .child(
                                div().w(px(22.0)).h(px(120.0)).items_center().child(
                                    Slider::new(&slider)
                                        .vertical()
                                        .h(px(112.0))
                                        .bg(accent.opacity(0.55))
                                        .text_color(accent),
                                ),
                            )
                    })
                    .disabled(true),
                )
            },
        );

        let focus_view = view.clone();
        let order_click_view = view.clone();
        let animate_order = !self.state.settings.disable_animations;
        let order_on = folder_order != atlas_model::pane::FolderOrder::default();
        let order_id = format!("folder-order-{}", pane_id.0);
        let order_icon = div()
            .child(theme_icon(
                AtlasIcon::SortAscending,
                14.0,
                if order_on { accent } else { controls.foreground },
                windows_theme,
            ));
        // Restarts on every state change (the animation id carries the order), so the
        // glyph slides in from below each time the cycle advances.
        let order_icon = if animate_order {
            order_icon
                .with_animation(
                    SharedString::from(format!("{order_id}-{folder_order:?}")),
                    crate::ui::motion::animation(crate::ui::motion::MICRO_MS),
                    |this, t| this.opacity(0.35 + 0.65 * t).mt(px(5.0 * (1.0 - t))),
                )
                .into_any_element()
        } else {
            order_icon.into_any_element()
        };
        let order_tip = format!("{}
{}", rust_i18n::t!("folder_order_choose"), rust_i18n::t!(folder_order.label_key()));
        let order_button = div()
            .id(SharedString::from(order_id))
            .flex().items_center()
            .p(crate::ui::rem_px(4.))
            .rounded_md()
            .cursor_pointer()
            .overflow_hidden()
            .bg(controls.hover.opacity(0.0))
            .when(order_on, |s| s.bg(accent.opacity(0.12)))
            .motion_feedback()
            .hover(move |s| s.bg(controls.hover))
            .active(move |s| s.bg(controls.pressed))
            .child(order_icon)
            .tooltip(move |window, cx| {
                gpui_component::tooltip::Tooltip::new(order_tip.clone()).build(window, cx)
            })
            .on_mouse_down(MouseButton::Left, move |_ev, _window, cx| {
                order_click_view.update(cx, |this, cx| {
                    this.activate_tab(pane_id);
                    this.process_actions([Action::ToggleFoldersFirst(pane_id)]);
                    this.notify_pane(pane_id, cx);
                    cx.notify();
                });
            });
        let mut summary = rust_i18n::t!("footer_total_size",
            size = atlas_core::util::display::format_size(counts.bytes_shown.min(u128::from(u64::MAX)) as u64)).to_string();
        if let Some(elapsed) = listing_elapsed {
            let elapsed = if elapsed.as_secs_f64() < 1.0 { format!("{} ms", elapsed.as_millis()) }
                else { format!("{:.2} s", elapsed.as_secs_f64()) };
            summary.push('\n');
            summary.push_str(&rust_i18n::t!("footer_listing_time", elapsed = elapsed));
        }
        let controls_pill = h_flex()
            .id(SharedString::from(format!("controls-pill-{}", pane_id.0)))
            .absolute()
            .bottom_2()
            .right_2()
            .px_1()
            .h(crate::ui::rem_px(PILL_HEIGHT))
            .gap_1()
            .items_center()
            .rounded_xl()
            .border_1()
            .border_color(controls.border)
            .bg(pill_surface)
            .when(is_glass, |s| s.backdrop_blur(px(12.0)))
            .raised_surface()
            // The view-selector actions act on the focused pane; make sure that's
            // us — and keep the click away from the pane underneath.
            .on_mouse_down(MouseButton::Left, move |_ev, _window, cx| {
                cx.stop_propagation();
                focus_view.update(cx, |this, _| {
                    this.activate_tab(pane_id);
                });
            })
            .tooltip(move |window, cx| gpui_component::tooltip::Tooltip::new(summary.clone()).build(window, cx))
            .child(tbtn(
                format!("bb-folders-{}", pane_id.0),
                AtlasIcon::Folder,
                !show_folders,
                accent,
                format!("{}\n{}\n{}", rust_i18n::t!("footer_dirs", shown = counts.folders_shown, total = counts.folders_total),
                    rust_i18n::t!(if show_folders { "footer_dirs_visible" } else { "footer_dirs_hidden" }),
                    rust_i18n::t!(if self.state.settings.show_hidden { "footer_hidden_included" } else { "footer_hidden_excluded" })),
                Action::ToggleShowFolders(pane_id),
                view.clone(),
            ).child(div().text_xs().child(folder_label)))
            .child(tbtn(
                format!("bb-files-{}", pane_id.0),
                AtlasIcon::File,
                !show_files,
                accent,
                format!("{}\n{}\n{}", rust_i18n::t!("footer_files", shown = counts.files_shown, total = counts.files_total),
                    rust_i18n::t!(if show_files { "footer_files_visible" } else { "footer_files_hidden" }),
                    rust_i18n::t!(if self.state.settings.show_hidden { "footer_hidden_included" } else { "footer_hidden_excluded" })),
                Action::ToggleShowFiles(pane_id),
                view.clone(),
            ).child(div().text_xs().child(file_label)))
            .child(order_button)
            .child(
                div()
                    .w(px(1.))
                    .h(crate::ui::rem_px(14.))
                    .bg(border.opacity(0.35)),
            )
            .child(view_btn)
            .into_any_element();

        (filter_pill, controls_pill)
    }

    #[allow(dead_code)]
    pub(crate) fn render_pane_bottom_bar(
        &mut self,
        pane_id: TabId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (filter_pill, controls_pill) = self.render_pane_pills(pane_id, window, cx);
        div()
            .relative()
            .size_full()
            .child(filter_pill)
            .child(controls_pill)
            .into_any_element()
    }
}

#[derive(Clone)]
struct BookmarkTarget {
    name: String,
    path: PathBuf,
    exists: bool,
    index: Option<usize>,
}

struct NavButtonProps {
    id: &'static str,
    pane_id: TabId,
    glyph: AtlasIcon,
    tooltip_title: String,
    tooltip_desc: String,
    shortcuts: &'static [&'static str],
    enabled: bool,
    action: Action,
    view: Entity<App>,
    recent: Vec<(SharedString, Location)>,
    icon_color: Hsla,
    hover_bg: Hsla,
    windows_theme: bool,
}

fn nav_button(props: NavButtonProps) -> impl IntoElement {
    let menu_view = props.view.clone();
    let menu_recent = props.recent.clone();
    let hover_id = DragHoverId::Chrome(props.id, props.pane_id);
    let hover_command = match props.action.clone() {
        Action::NavigateBack(tile) => DragHoverCommand::NavigateBack(tile),
        Action::NavigateForward(tile) => DragHoverCommand::NavigateForward(tile),
        Action::NavigateUp(tile) => DragHoverCommand::NavigateUp(tile),
        other => DragHoverCommand::Process(other),
    };
    div()
        .id(SharedString::from(format!(
            "{}-{}",
            props.id, props.pane_id.0
        )))
        .flex_none()
        .size(crate::ui::rem_px(if props.windows_theme {
            34.0
        } else {
            26.0
        }))
        .rounded(px(if props.windows_theme { 8.0 } else { 7.0 }))
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .when(!props.enabled, |s| s.opacity(0.35))
        .bg(props.hover_bg.opacity(0.0))
        .motion_feedback()
        .when(props.enabled, |s| s.hover(move |s| s.bg(props.hover_bg)))
        .child(theme_icon(
            props.glyph,
            if props.windows_theme { 18.0 } else { 15.0 },
            props.icon_color.opacity(0.88),
            props.windows_theme,
        ))
        .when(props.enabled, |s| {
            s.on_prepaint_child(drag_hover_recorder(
                hover_id,
                DragHoverKind::Folder,
                hover_command,
                &props.view,
            ))
        })
        .tooltip(move |window, cx| {
            shortcut_tooltip(
                props.tooltip_title.clone(),
                props.tooltip_desc.clone(),
                props.shortcuts,
                window,
                cx,
            )
        })
        .on_mouse_down(MouseButton::Left, move |_ev, _window, cx| {
            cx.stop_propagation();
            if props.enabled {
                props.view.update(cx, |this, cx| {
                    this.process_actions(vec![props.action.clone()]);
                    this.flush_dirty_panes(cx);
                });
            }
        })
        .on_mouse_down(MouseButton::Right, {
            let view = menu_view.clone();
            move |_ev, _window, cx| {
                view.update(cx, |this, cx| {
                    this.suppress_next_shell_ctx_menu = true;
                    this.close_ctx_menu(cx);
                });
            }
        })
        .context_menu(move |menu, _window, _cx| {
            nav_context_menu(menu, props.pane_id, menu_view.clone(), menu_recent.clone())
        })
}

fn bookmark_button(
    pane_id: TabId,
    view: Entity<App>,
    bookmark: Option<BookmarkTarget>,
    icon_color: Hsla,
    hover_bg: Hsla,
    windows_theme: bool,
) -> impl IntoElement {
    let enabled = bookmark.as_ref().is_some_and(|b| !b.exists);
    let active = bookmark.as_ref().is_some_and(|b| b.exists);
    let (tooltip_title, tooltip_desc) = if active {
        (
            rust_i18n::t!("tooltip_bookmark_active_title").to_string(),
            rust_i18n::t!("tooltip_bookmark_active_desc").to_string(),
        )
    } else {
        (
            rust_i18n::t!("tooltip_bookmark_add_title").to_string(),
            rust_i18n::t!("tooltip_bookmark_add_desc").to_string(),
        )
    };

    div()
        .id(SharedString::from(format!("pane-bookmark-{}", pane_id.0)))
        .flex_none()
        .size(crate::ui::rem_px(if windows_theme { 34.0 } else { 26.0 }))
        .rounded(px(if windows_theme { 8.0 } else { 7.0 }))
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .when(!enabled && !active, |s| s.opacity(0.35))
        .bg(hover_bg.opacity(0.0))
        .motion_feedback()
        .hover(move |s| s.bg(hover_bg))
        .child(theme_icon(
            AtlasIcon::BookmarkSimple,
            if windows_theme { 18.0 } else { 15.0 },
            if active {
                gpui::yellow()
            } else {
                icon_color.opacity(0.88)
            },
            windows_theme,
        ))
        .tooltip(move |window, cx| {
            shortcut_tooltip(tooltip_title.clone(), tooltip_desc.clone(), &[], window, cx)
        })
        .on_mouse_down(MouseButton::Left, move |_ev, _window, cx| {
            cx.stop_propagation();
            let Some(bookmark) = bookmark.clone() else {
                return;
            };
            view.update(cx, |this, cx| {
                if let Some(index) = bookmark.index {
                    this.process_actions(vec![Action::RemoveBookmark(index)]);
                } else {
                    this.process_actions(vec![Action::AddBookmark {
                        name: bookmark.name,
                        path: bookmark.path,
                    }]);
                }
                cx.notify();
            });
        })
}

fn nav_context_menu(
    mut menu: PopupMenu,
    pane_id: TabId,
    view: Entity<App>,
    recent: Vec<(SharedString, Location)>,
) -> PopupMenu {
    if recent.is_empty() {
        menu = menu.label(rust_i18n::t!("no_recent_history"));
    } else {
        menu = menu.item(
            PopupMenuItem::new(rust_i18n::t!("recent_places"))
                .icon(Icon::empty().path(AtlasIcon::ClockCounterClockwise.path()))
                .disabled(true),
        );
        for (label, location) in recent.into_iter().take(6) {
            let icon_name = location_icon(&location);
            let item_view = view.clone();
            menu = menu.item(
                PopupMenuItem::new(label)
                    .icon(Icon::empty().path(icon_name.path()))
                    .on_click(move |_ev, _window, cx| {
                        let location = location.clone();
                        item_view.update(cx, |this, cx| {
                            this.process_actions(vec![Action::NavigateLocation {
                                tab: pane_id,
                                location,
                            }]);
                            cx.notify();
                        });
                    }),
            );
        }
    }
    menu
}

fn location_icon(location: &Location) -> AtlasIcon {
    match location {
        Location::Directory(_) => AtlasIcon::Folder,
        Location::Shell(_) => AtlasIcon::Monitor,
        Location::Archive { .. } => AtlasIcon::File,
        Location::Tag(_) => AtlasIcon::Tag,
        Location::RecycleBin => AtlasIcon::Trash,
        #[cfg(feature = "provider-host")]
        Location::Remote { .. } => AtlasIcon::Cloud,
    }
}

fn location_label(location: &Location) -> SharedString {
    let display = location.display_path();
    match location {
        Location::Directory(path) => path.display().to_string().into(),
        Location::Shell(path) => atlas_core::util::display::display_name(std::path::Path::new(path)).into(),
        Location::Archive { archive, inner } => {
            if inner.as_os_str().is_empty() {
                archive.display().to_string().into()
            } else {
                format!("{} › {}", archive.display(), inner.display()).into()
            }
        }
        Location::Tag(_) | Location::RecycleBin => display.display().to_string().into(),
        #[cfg(feature = "provider-host")]
        Location::Remote { .. } => display.display().to_string().into(),
    }
}

/// A clickable path breadcrumb: each segment navigates the focused pane to that
/// ancestor directory. Replaces the old plain truncated path label.
fn breadcrumb(
    view: &Entity<App>,
    tile: TabId,
    path: &std::path::Path,
    fg: Hsla,
    muted: Hsla,
    layer_base: Hsla,
    windows_theme: bool,
    is_branch_view: bool,
) -> impl IntoElement {
    let mut row = h_flex()
        .flex_1()
        .min_w(px(0.))
        .gap(crate::ui::rem_px(if windows_theme { 7.0 } else { 5.0 }))
        .items_center()
        // Plain clip (not overflow_x_scrollbar): the Scrollable wrapper adds its own
        // relative+size_full box that anchored each crumb's on_prepaint canvas to the
        // wrong rect, so drag-hover targets captured a region offset below the crumb.
        // Crumbs already truncate via max_w + text_ellipsis.
        .overflow_hidden();
    let mut cumulative = std::path::PathBuf::new();
    let private_root = atlas_core::vault::live::mount_root(path);
    let mut visible_segments = 0;
    for (i, comp) in path.components().enumerate() {
        let parent = cumulative.clone();
        cumulative.push(comp.as_os_str());
        if private_root.as_ref().is_some_and(|root| !cumulative.starts_with(root)) { continue; }
        let raw = comp.as_os_str().to_string_lossy().into_owned();
        let label = atlas_core::vault::live::display_name(&cumulative)
            .unwrap_or_else(|| if raw.is_empty() { "/".to_string() } else { raw });
        if visible_segments > 0 {
            row = row.child(breadcrumb_chevron(
                view,
                tile,
                parent,
                muted,
                layer_base,
                windows_theme,
            ));
        }
        let target = cumulative.clone();
        visible_segments += 1;
        row = row.child(breadcrumb_segment(BreadcrumbSegmentProps {
            view: view.clone(),
            tile,
            id: SharedString::from(format!("crumb-{}-{}", tile.0, i)),
            label,
            location: Location::Directory(target.clone()),
            drop_dest: Some(target),
            fg,
            layer_base,
            windows_theme,
        }));
    }
    if is_branch_view {
        row = row.child(breadcrumb_chevron(
            view,
            tile,
            cumulative,
            muted,
            layer_base,
            windows_theme,
        ));
        let badge_view = view.clone();
        row = row.child(
            h_flex()
                .id(SharedString::from(format!("crumb-branch-badge-{}", tile.0)))
                .items_center()
                .gap(crate::ui::rem_px(4.0))
                .px(crate::ui::rem_px(6.0))
                .py(crate::ui::rem_px(2.0))
                .rounded(crate::ui::rem_px(4.0))
                .bg(fg.opacity(0.12))
                .hover(|s| s.bg(fg.opacity(0.20)))
                .cursor_pointer()
                .text_color(fg)
                .text_xs()
                .tooltip(move |window, cx| {
                    shortcut_tooltip(
                        rust_i18n::t!("cmd_view_branch_view_title"),
                        rust_i18n::t!("cmd_view_branch_view_desc"),
                        &["ctrl-b"],
                        window,
                        cx,
                    )
                })
                .on_mouse_down(MouseButton::Left, move |_ev, _window, cx| {
                    badge_view.update(cx, |this, _| {
                        this.process_actions([Action::ToggleBranchView(tile)]);
                    });
                })
                .child(crate::ui::icons::icon(AtlasIcon::TreeStructure, 12.0, fg))
                .child(rust_i18n::t!("branch_view_badge").to_string()),
        );
    }
    row
}

struct BreadcrumbSegmentProps {
    view: Entity<App>,
    tile: TabId,
    id: SharedString,
    label: String,
    location: Location,
    drop_dest: Option<PathBuf>,
    fg: Hsla,
    layer_base: Hsla,
    windows_theme: bool,
}

fn breadcrumb_segment(props: BreadcrumbSegmentProps) -> gpui::Stateful<gpui::Div> {
    let down_v = props.view.clone();
    let down_location = props.location.clone();
    let hover_bg = props.layer_base;
    let hover_id = match &props.location {
        Location::Directory(path) => DragHoverId::Breadcrumb(props.tile, path.clone()),
        Location::Archive { archive, inner } => {
            DragHoverId::Breadcrumb(props.tile, archive.join(inner))
        }
        _ => DragHoverId::Breadcrumb(props.tile, props.location.display_path()),
    };
    let command = DragHoverCommand::Process(Action::NavigateLocation {
        tab: props.tile,
        location: props.location.clone(),
    });
    let mut el = div()
        .id(props.id.clone())
        .flex_none()
        .relative()
        .h(crate::ui::rem_px(if props.windows_theme {
            32.0
        } else {
            26.0
        }))
        .max_w(crate::ui::rem_px(if props.windows_theme {
            260.0
        } else {
            220.0
        }))
        .px(crate::ui::rem_px(if props.windows_theme {
            10.0
        } else {
            8.0
        }))
        .rounded(px(if props.windows_theme { 7.0 } else { 6.0 }))
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .text_color(props.fg)
        .overflow_hidden()
        .text_ellipsis()
        .whitespace_nowrap()
        .child(SharedString::from(props.label))
        .bg(hover_bg.opacity(0.0))
        .motion_feedback()
        .hover(move |s| s.bg(hover_bg))
        .on_mouse_down(MouseButton::Left, move |_ev, _window, cx| {
            let location = down_location.clone();
            down_v.update(cx, |this, cx| {
                this.state.focused_tile = props.tile;
                this.process_actions(vec![Action::NavigateLocation {
                    tab: props.tile,
                    location,
                }]);
                cx.notify();
            });
            cx.stop_propagation();
        });

    let drop_command = props.drop_dest.map(DragHoverDropCommand::TransferTo)
        .unwrap_or_else(|| crate::ui::drag_hover::location_drop_command(&props.location));
    {
        let drop_v = props.view.clone();
        let drop_v_external = props.view.clone();
        let drop_target = drop_command.clone();
        let drop_target_external = drop_command.clone();
        el = el
            .on_prepaint_child(drag_hover_recorder_with_drop(
                hover_id,
                DragHoverKind::Folder,
                command,
                drop_command,
                &props.view,
            ))
            .on_drop::<crate::ui::root::DraggedFiles>(move |d, _window, cx| {
                let paths = d.paths.clone();
                drop_v.update(cx, |this, cx| {
                    this.execute_drag_hover_drop_command(
                        drop_target.clone(),
                        paths,
                        true,
                    );
                    cx.notify();
                });
            })
            .on_drop::<gpui::ExternalPaths>(move |paths, _window, cx| {
                let paths = paths.paths().to_vec();
                drop_v_external.update(cx, |this, cx| {
                    this.execute_drag_hover_drop_command(
                        drop_target_external.clone(),
                        paths,
                        false,
                    );
                    cx.notify();
                });
            });
    }

    el
}

fn breadcrumb_chevron(
    view: &Entity<App>,
    tile: TabId,
    dir: PathBuf,
    muted: Hsla,
    layer_base: Hsla,
    windows_theme: bool,
) -> impl IntoElement {
    let v = view.clone();
    let hover_bg = layer_base;
    div()
        .id(SharedString::from(format!(
            "crumb-chevron-{}-{}",
            tile.0,
            dir.display()
        )))
        .flex_none()
        .size(crate::ui::rem_px(if windows_theme { 22.0 } else { 18.0 }))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(if windows_theme { 6.0 } else { 5.0 }))
        .cursor_pointer()
        .bg(hover_bg.opacity(0.0))
        .motion_feedback()
        .hover(move |s| s.bg(hover_bg))
        .child(theme_icon(
            AtlasIcon::CaretRight,
            11.0,
            muted,
            windows_theme,
        ))
        .on_mouse_down(MouseButton::Left, move |ev, window, cx| {
            let dir = dir.clone();
            let anchor = ev.position;
            v.update(cx, |this, cx| {
                this.state.focused_tile = tile;
                this.open_breadcrumb_menu(tile, dir, anchor, window, cx);
            });
            cx.stop_propagation();
        })
}

/// Breadcrumb for a tab currently browsing inside an archive.
/// Segments: real parent dirs → archive file → inner dirs.
fn archive_breadcrumb(
    view: &Entity<App>,
    tile: TabId,
    archive: &std::path::Path,
    inner: &std::path::Path,
    fg: Hsla,
    muted: Hsla,
    layer_base: Hsla,
    windows_theme: bool,
) -> impl IntoElement {
    let mut row = h_flex()
        .flex_1()
        .min_w(px(0.))
        .gap(crate::ui::rem_px(if windows_theme { 7.0 } else { 5.0 }))
        .items_center()
        .overflow_hidden();
    let mut sep = false;

    // Real filesystem ancestors of the archive file.
    let mut cumulative = std::path::PathBuf::new();
    for (i, comp) in archive
        .parent()
        .unwrap_or_else(|| std::path::Path::new(""))
        .components()
        .enumerate()
    {
        cumulative.push(comp.as_os_str());
        let raw = comp.as_os_str().to_string_lossy().into_owned();
        let label = if raw.is_empty() { "/".to_string() } else { raw };
        if sep {
            row = row.child(div().text_color(muted).child("›"));
        }
        sep = true;
        let target = cumulative.clone();
        row = row.child(breadcrumb_segment(BreadcrumbSegmentProps {
            view: view.clone(),
            tile,
            id: SharedString::from(format!("crumb-r{}-{}", tile.0, i)),
            label,
            location: Location::Directory(target.clone()),
            drop_dest: Some(target),
            fg,
            layer_base,
            windows_theme,
        }));
    }

    if sep {
        row = row.child(div().text_color(muted).child("›"));
    }
    let archive_target = archive.to_path_buf();
    row = row.child(breadcrumb_segment(BreadcrumbSegmentProps {
        view: view.clone(),
        tile,
        id: SharedString::from(format!("crumb-archive-{}", tile.0)),
        label: archive
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| archive.display().to_string()),
        location: Location::Archive {
            archive: archive_target,
            inner: PathBuf::new(),
        },
        drop_dest: None,
        fg,
        layer_base,
        windows_theme,
    }));

    // Inner path components within the archive.
    let mut inner_so_far = std::path::PathBuf::new();
    for (j, comp) in inner.components().enumerate() {
        inner_so_far.push(comp.as_os_str());
        let raw = comp.as_os_str().to_string_lossy().into_owned();
        row = row.child(div().text_color(muted).child("›"));
        let archive_path = archive.to_path_buf();
        let inner_target = inner_so_far.clone();
        row = row.child(breadcrumb_segment(BreadcrumbSegmentProps {
            view: view.clone(),
            tile,
            id: SharedString::from(format!("crumb-a{}-{}", tile.0, j)),
            label: raw,
            location: Location::Archive {
                archive: archive_path.clone(),
                inner: inner_target,
            },
            drop_dest: None,
            fg,
            layer_base,
            windows_theme,
        }));
    }

    row
}

#[cfg(feature = "provider-host")]
#[allow(clippy::too_many_arguments)]
fn remote_breadcrumb(
    view: &Entity<App>,
    tile: TabId,
    id: &str,
    remote_label: &str,
    path: &str,
    fg: Hsla,
    muted: Hsla,
    layer_base: Hsla,
    windows_theme: bool,
) -> impl IntoElement {
    let mut row = h_flex()
        .flex_1()
        .min_w(px(0.))
        .gap(crate::ui::rem_px(if windows_theme { 7.0 } else { 5.0 }))
        .items_center()
        .overflow_hidden();

    row = row.child(crate::ui::icons::icon(AtlasIcon::Cloud, 13.0, muted));

    // Root crumb for the remote itself
    row = row.child(breadcrumb_segment(BreadcrumbSegmentProps {
        view: view.clone(),
        tile,
        id: SharedString::from(format!("crumb-remote-root-{}", tile.0)),
        label: remote_label.to_string(),
        location: Location::Remote {
            id: id.to_string(),
            path: String::new(),
        },
        drop_dest: None,
        fg,
        layer_base,
        windows_theme,
    }));

    let trimmed = path.trim_matches('/');
    if !trimmed.is_empty() {
        let mut path_so_far = String::new();
        for (idx, comp) in trimmed.split('/').enumerate() {
            if !path_so_far.is_empty() {
                path_so_far.push('/');
            }
            path_so_far.push_str(comp);
            row = row.child(div().text_color(muted).child("›"));
            row = row.child(breadcrumb_segment(BreadcrumbSegmentProps {
                view: view.clone(),
                tile,
                id: SharedString::from(format!("crumb-remote-sub-{}-{}", tile.0, idx)),
                label: atlas_model::remote_path::file_name(comp),
                location: Location::Remote {
                    id: id.to_string(),
                    path: path_so_far.clone(),
                },
                drop_dest: None,
                fg,
                layer_base,
                windows_theme,
            }));
        }
    }

    row
}

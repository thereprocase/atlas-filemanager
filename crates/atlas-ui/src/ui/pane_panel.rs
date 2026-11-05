//! `FilePanel`: a gpui-component `Panel` wrapping one file-view pane.
//!
//! Phase A: the central tabs/splits are now a `gpui_component::dock::DockArea`. The
//! Dock owns the interactive layout (tab drag/reorder, drag-to-split, close), so
//! each pane must be its own `Entity` implementing the `Panel` trait. `FilePanel`
//! holds a weak handle back to `App` and renders the pane by delegating to the
//! existing `App::render_pane` (which dispatches Details/Grid/List/Miller).
//!
//! The Dock caches the active panel's content (`.cached()`), so the surrounding
//! DockArea drives invalidation. Avoid observing every `App` repaint here: doing so
//! invalidates every file panel on each app notify and makes window movement stutter.

use crate::app::{App as AtlasApp, PaneSignal};
use crate::input::action::Action;
use crate::ui::icons::{icon, AtlasIcon};
use crate::ui::motion::MotionFeedbackExt as _;
use crate::ui::tooltips::shortcut_tooltip;
use atlas_core::util::display::display_name;
use atlas_model::pane::TabId;
use gpui::prelude::*;
use gpui::ColorExt as _;
use gpui::{
    div, px, AnyView, App, Context, Entity, EventEmitter, FocusHandle, Focusable, IntoElement,
    MouseButton, Render, SharedString, WeakEntity, Window,
};
use gpui_component::dock::{Panel, PanelEvent, PanelInfo, PanelState};
use gpui_component::{h_flex, ActiveTheme as _, ElementExt as _};

pub struct FilePanel {
    /// Weak handle to the orchestrator that owns `AppState`.
    app: WeakEntity<AtlasApp>,
    /// Stable identity of the pane this panel renders (also its layout-tree tile id).
    pub(crate) pane_id: TabId,
    focus_handle: FocusHandle,
    content: Entity<PaneContentView>,
}

pub struct PaneContentView {
    app: WeakEntity<AtlasApp>,
    pane_id: TabId,
}

impl Render for PaneContentView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let app = self.app.clone();
        let pane_id = self.pane_id;
        crate::ui::current_bounds::current_bounds(move |bounds, window, cx| {
            crate::perf::count("render.pane_content");
            let _perf = crate::perf::span_detail("pane.content.render", || {
                format!("pane={}", pane_id.0)
            });
            let Some(app) = app.upgrade() else {
                return div().into_any_element();
            };
            app.update(cx, |app, cx| {
                // Column projection and virtualized contents must see the same
                // dimensions as their shell, including the first resize frame.
                if let Some(state) = app.state.tab_states.get_mut(&pane_id) {
                    state.last_pane_width = f32::from(bounds.size.width);
                    state.last_pane_height = f32::from(bounds.size.height);
                }
                app.render_pane(pane_id, window, cx)
            })
        })
        .into_any_element()
    }
}

impl FilePanel {
    /// `signal` must be pre-computed by the caller using the App context BEFORE
    /// `cx.new(|cx| FilePanel::new(...))` is called, because at that point the App
    /// entity is already leased and `app.update(cx, ...)` would double-borrow it.
    pub fn new(
        app: Entity<AtlasApp>,
        pane_id: TabId,
        signal: Entity<PaneSignal>,
        cx: &mut Context<Self>,
    ) -> Self {
        let content = cx.new(|cx| {
            cx.observe(&signal, |_, _, cx| cx.notify()).detach();
            PaneContentView {
                app: app.downgrade(),
                pane_id,
            }
        });
        Self {
            app: app.downgrade(),
            pane_id,
            focus_handle: cx.focus_handle(),
            content,
        }
    }
}

impl EventEmitter<PanelEvent> for FilePanel {}

impl Focusable for FilePanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Panel for FilePanel {
    fn panel_name(&self) -> &'static str {
        "FilePanel"
    }

    /// Hide the trailing "..." toolbar menu (and its grey suffix box) on file panes.
    fn show_toolbar(&self, _cx: &gpui::App) -> bool {
        false
    }

    fn title(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (label, tex) = self
            .app
            .upgrade()
            .map(|a| {
                let app = a.read(cx);
                let active_theme = app.state.profiles.active().theme.as_str();
                let windows_theme = crate::ui::icons::is_windows_theme(active_theme);

                let tab_data = app.tab_data_for(self.pane_id);
                let path = tab_data
                    .as_ref()
                    .and_then(|t| t.location.fs_dir().map(|p| p.to_path_buf()));
                let label = tab_data
                    .map(|t| display_name(&t.location.display_path()))
                    .unwrap_or_else(|| rust_i18n::t!("fallback_tab_title").to_string());

                let tex = if windows_theme {
                    path.as_ref().and_then(|p| app.grid_thumbnail(p))
                } else {
                    None
                };

                (label, tex)
            })
            .unwrap_or_else(|| (rust_i18n::t!("fallback_tab_title").to_string(), None));
        let pane_id = self.pane_id;
        let app = self.app.clone();
        let muted = cx.theme().muted_foreground;
        let hover_bg = cx.theme().border.opacity(0.45);
        let danger = cx.theme().danger;
        let title = h_flex()
            .id(SharedString::from(format!("file-tab-label-{}", pane_id.0)))
            .relative()
            .min_w(px(0.))
            .items_center()
            .gap(px(8.))
            .when_some(tex, |s, tex| s.child(gpui::img(tex).size(px(16.0))))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .child(SharedString::from(label)),
            )
            .child(
                div()
                    .id(SharedString::from(format!("file-tab-close-{}", pane_id.0)))
                    .flex_none()
                    .size(px(20.))
                    .rounded(crate::ui::gridline::radius(999.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .text_color(muted)
                    .motion_feedback()
                    .hover(|this| this.bg(hover_bg).text_color(danger))
                    .child(icon(AtlasIcon::X, 11.0, muted))
                    .tooltip(|window, cx| {
                        shortcut_tooltip(
                            rust_i18n::t!("tooltip_tab_close_title").to_string(),
                            rust_i18n::t!("tooltip_tab_close_desc").to_string(),
                            &["ctrl-w"],
                            window,
                            cx,
                        )
                    })
                    .on_mouse_down(MouseButton::Left, move |_ev, _window, cx| {
                        if let Some(app) = app.upgrade() {
                            app.update(cx, |app, cx| {
                                app.process_actions(vec![Action::CloseTab(pane_id)]);
                                cx.notify();
                            });
                        }
                        cx.stop_propagation();
                    }),
            );

        title.into_any_element()
    }

    fn title_suffix(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement> {
        let pane_id = self.pane_id;
        let app = self.app.clone();
        let muted = cx.theme().muted_foreground;
        let hover_bg = cx.theme().border.opacity(0.45);
        let accent = cx.theme().accent;

        Some(
            div()
                .id(SharedString::from(format!("file-tab-new-{}", pane_id.0)))
                .flex_none()
                .size(px(24.))
                .rounded(crate::ui::gridline::radius(7.))
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .text_color(muted)
                .motion_feedback()
                .hover(|this| this.bg(hover_bg).text_color(accent))
                .child(icon(AtlasIcon::Plus, 14.0, muted))
                .tooltip(|window, cx| {
                    shortcut_tooltip(
                        rust_i18n::t!("tooltip_tab_new_title").to_string(),
                        rust_i18n::t!("tooltip_tab_new_desc").to_string(),
                        &["ctrl-t"],
                        window,
                        cx,
                    )
                })
                .on_mouse_down(MouseButton::Left, move |_ev, _window, cx| {
                    if let Some(app) = app.upgrade() {
                        app.update(cx, |app, cx| {
                            app.process_actions(vec![Action::NewTab(pane_id)]);
                            cx.notify();
                        });
                    }
                    cx.stop_propagation();
                }),
        )
    }

    /// Serialize the pane identity into the dumped layout so the reconcile path can
    /// map a dumped `FilePanel` node back to its `TileId`.
    fn dump(&self, _cx: &App) -> PanelState {
        let mut state = PanelState::new(self);
        state.info = PanelInfo::Panel(serde_json::json!({ "pane_id": self.pane_id.0 }));
        state
    }
}

impl Render for FilePanel {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let pane_id = self.pane_id;
        crate::perf::count("render.file_panel");
        let _perf = crate::perf::span_detail("file_panel.render", || format!("pane={}", pane_id.0));
        match self.app.upgrade() {
            Some(_app) => {
                let content = AnyView::from(self.content.clone());
                let bounds_app = self.app.clone();
                let panel = div()
                    .size_full()
                    .relative()
                    .overflow_hidden()
                    .child(content)
                    .on_prepaint_child(move |bounds, _window, cx| {
                        if let Some(app) = bounds_app.upgrade() {
                            let should_update = {
                                let app_ref = app.read(cx);
                                app_ref.state.tab_states.contains_key(&pane_id)
                                    && app_ref.pane_layout_bounds.get(&pane_id) != Some(&bounds)
                            };
                            if should_update {
                                app.update(cx, |app, _cx| {
                                    app.pane_layout_bounds.insert(pane_id, bounds);
                                });
                            }
                        }
                    });

                panel.into_any_element()
            }
            None => div().into_any_element(),
        }
    }
}

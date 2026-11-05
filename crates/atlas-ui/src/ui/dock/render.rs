use gpui::prelude::*;
use gpui::ColorExt as _;
use gpui::{
    div, px, relative, AnyElement, Context, Entity, Focusable, MouseButton,
    SharedString, Window,
};
use gpui_component::{h_flex, v_flex, ActiveTheme as _, ElementExt as _};

use crate::app::{App, PaneAnimationKind};
use crate::ui::dock::drag::{tab_drag_entity, TabDrag};
use crate::ui::glass::{backdrop_tint, GlassExt};
use crate::ui::icons::{icon, is_windows_theme, AtlasIcon};
use crate::ui::layout_region::{layout_region, timed_layout_region, LayoutRegionTimings};
use crate::ui::pane_panel::FilePanel;
use atlas_core::util::display::display_name;
use atlas_model::layout_tree::{Container, LinearDir, Tile, TileId};
use atlas_model::location::Location;
use std::{sync::OnceLock, time::Instant};

const DOCK_FLAT_HOST_TIMINGS: LayoutRegionTimings = LayoutRegionTimings {
    request_layout: "dock.flat_host.request_layout",
    prepaint: "dock.flat_host.prepaint",
    paint: "dock.flat_host.paint",
};
const DOCK_SPLITTER_TIMINGS: LayoutRegionTimings = LayoutRegionTimings {
    request_layout: "dock.splitters.request_layout",
    prepaint: "dock.splitters.prepaint",
    paint: "dock.splitters.paint",
};
const PANE_CACHED_HOST_TIMINGS: LayoutRegionTimings = LayoutRegionTimings {
    request_layout: "pane.cached_host.request_layout",
    prepaint: "pane.cached_host.prepaint",
    paint: "pane.cached_host.paint",
};
const PANE_SHELL_TIMINGS: LayoutRegionTimings = LayoutRegionTimings {
    request_layout: "pane.shell.request_layout",
    prepaint: "pane.shell.prepaint",
    paint: "pane.shell.paint",
};
const PANE_TABS_BAR_TIMINGS: LayoutRegionTimings = LayoutRegionTimings {
    request_layout: "pane.tabs_bar.request_layout",
    prepaint: "pane.tabs_bar.prepaint",
    paint: "pane.tabs_bar.paint",
};
#[allow(dead_code)]
const PANE_CONTENT_SHELL_TIMINGS: LayoutRegionTimings = LayoutRegionTimings {
    request_layout: "pane.content_shell.request_layout",
    prepaint: "pane.content_shell.prepaint",
    paint: "pane.content_shell.paint",
};
#[allow(dead_code)]
const PANE_CONTENT_LAYERS_TIMINGS: LayoutRegionTimings = LayoutRegionTimings {
    request_layout: "pane.content_layers.request_layout",
    prepaint: "pane.content_layers.prepaint",
    paint: "pane.content_layers.paint",
};
#[allow(dead_code)]
const PANE_BODY_TIMINGS: LayoutRegionTimings = LayoutRegionTimings {
    request_layout: "pane.body.request_layout",
    prepaint: "pane.body.prepaint",
    paint: "pane.body.paint",
};
const PANE_DROP_ZONES_TIMINGS: LayoutRegionTimings = LayoutRegionTimings {
    request_layout: "pane.drop_zones.request_layout",
    prepaint: "pane.drop_zones.prepaint",
    paint: "pane.drop_zones.paint",
};

#[derive(Clone, Copy, Debug)]
enum SplitMotion {
    Opening,
    Closing,
}

#[derive(Clone, Copy)]
struct FlatRect {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
}

struct FlatPane {
    tile: TileId,
    /// Interpolated visual rect for this frame.
    rect: FlatRect,
    /// Stable final-layout rect. During FLIP, non-closing pane contents are laid out
    /// here once and only translated in paint on subsequent frames.
    layout_rect: FlatRect,
    /// Direction in which an exiting pane should slide beneath its survivor.
    close_axis: (f32, f32),
}

struct FlatSplitter {
    container: TileId,
    divider: usize,
    dir: LinearDir,
    rect: FlatRect,
}

/// Stable cached layout with a moving paint clip. Prepaint must see the same
/// bounds and mask throughout the reveal, otherwise GPUI rebuilds the pane.
struct PaneReveal {
    child: AnyElement,
    clip_offset: gpui::Point<gpui::Pixels>,
    clip_size: gpui::Size<gpui::Pixels>,
}

impl IntoElement for PaneReveal {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl gpui::Element for PaneReveal {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<gpui::ElementId> {
        None
    }
    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&gpui::GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut gpui::App,
    ) -> (gpui::LayoutId, ()) {
        (self.child.request_layout(window, cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&gpui::GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        _bounds: gpui::Bounds<gpui::Pixels>,
        _layout: &mut (),
        window: &mut Window,
        cx: &mut gpui::App,
    ) {
        self.child.prepaint(window, cx);
    }

    fn paint(
        &mut self,
        _id: Option<&gpui::GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: gpui::Bounds<gpui::Pixels>,
        _layout: &mut (),
        _prepaint: &mut (),
        window: &mut Window,
        cx: &mut gpui::App,
    ) {
        let clip = gpui::Bounds {
            origin: bounds.origin + self.clip_offset,
            size: self.clip_size,
        };
        window.with_paint_clip(clip, |window| self.child.paint(window, cx));
    }
}

fn split_child_span(share: f32, share_sum: f32, available: f32, child_count: usize) -> f32 {
    if share_sum > f32::EPSILON {
        share / share_sum * available
    } else {
        available / child_count.max(1) as f32
    }
}

fn closing_splitter_index(pane_ix: usize, num_splitters: usize) -> Option<usize> {
    (num_splitters > 0).then(|| pane_ix.min(num_splitters - 1))
}

fn stable_flip_layout_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var("ATLAS_STABLE_FLIP_LAYOUT")
            .ok()
            .map(|value| !matches!(value.as_str(), "" | "0" | "false" | "FALSE" | "off" | "OFF"))
            .unwrap_or(true)
    })
}

fn pane_visual_layout(
    current: FlatRect,
    final_rect: FlatRect,
    closing: bool,
    opening: bool,
    stable_flip: bool,
    previous_rect: Option<FlatRect>,
) -> (FlatRect, (f32, f32)) {
    if !stable_flip {
        return (current, (0.0, 0.0));
    }
    // Retain the larger content extent throughout a transition. The moving
    // paint clip reveals/covers it; shrinking content reflows once at settlement.
    // Opening panes have no previous layout to retain.
    let previous = previous_rect.unwrap_or(current);
    if closing {
        // A closing right/bottom pane's final origin is outside the dock. Keep
        // prepaint at its original visible origin or virtualized rows disappear
        // before paint can translate and clip the retained scene.
        return (previous, (current.x - previous.x, current.y - previous.y));
    }
    let (old_w, old_h) = if opening {
        (0.0, 0.0)
    } else {
        (previous.w, previous.h)
    };
    let rect = FlatRect {
        w: final_rect.w.max(old_w),
        h: final_rect.h.max(old_h),
        ..final_rect
    };
    (rect, (current.x - rect.x, current.y - rect.y))
}

fn pane_exit_paint_offset(
    current: FlatRect,
    retained: FlatRect,
    collapse_axis: (f32, f32),
) -> (f32, f32) {
    // A trailing pane's origin follows the seam outward. An earlier pane
    // leaves through its leading edge by right/bottom-aligning its retained
    // content to the shrinking slot. Both move the content, not just its clip.
    (
        current.x - retained.x
            + if collapse_axis.0 > 0.0 { current.w - retained.w } else { 0.0 },
        current.y - retained.y
            + if collapse_axis.1 > 0.0 { current.h - retained.h } else { 0.0 },
    )
}

/// Tabs-like tile (tab strip + body) hosted in its own entity so it can be embedded
/// with `AnyView::cached` (Zed's `dock.rs:1185` pattern: the whole pane, including
/// the tab bar, is cached). Rendering delegates to `App::render_tabs_like`;
/// invalidation is decided by the skeleton fingerprint in `render_cached_tabs`.
pub(crate) struct PaneTabsView {
    app: gpui::WeakEntity<App>,
    tile: TileId,
}

impl gpui::Render for PaneTabsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::perf::count("render.pane_tabs_view");
        let tile = self.tile;
        let Some(app) = self.app.upgrade() else {
            return div().into_any_element();
        };
        app.update(cx, |app, cx| {
            let (children, active) = match app.state.pane_tree.tiles.get(tile) {
                Some(Tile::Pane(_)) => (vec![tile], Some(tile)),
                Some(Tile::Container(Container::Tabs(tabs))) => {
                    let children = tabs.children.clone();
                    let active = tabs
                        .active
                        .filter(|a| children.contains(a))
                        .or_else(|| children.first().copied());
                    (children, active)
                }
                _ => return div().size_full().into_any_element(),
            };
            if children.is_empty() {
                return div().size_full().into_any_element();
            }
            app.render_tabs_like(children, active, window, cx)
        })
    }
}

fn split_drop_zone(kind: AtlasIcon) -> gpui::Div {
    div()
        .absolute()
        .opacity(0.0)
        .bg(gpui::transparent_black())
        .drag_over::<TabDrag>(|s, _, _, _| s.opacity(1.0).bg(gpui::hsla(0.58, 0.70, 0.42, 0.24)))
        .child(
            div()
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .size(crate::ui::rem_px(58.))
                        .rounded(px(10.))
                        .border_1()
                        .border_color(gpui::hsla(0.58, 0.84, 0.58, 0.78))
                        .bg(gpui::hsla(0.58, 0.78, 0.30, 0.58))
                        .shadow_lg()
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(icon(kind, 26.0, gpui::hsla(0.58, 0.90, 0.84, 1.0))),
                ),
        )
}

impl App {
    pub(crate) fn render_custom_dock(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if self.tick_pane_close() {
            // Finish on the dock's frame clock instead of waiting up to 100 ms
            // for the background drain. Notify chrome once, after teardown.
            cx.notify();
            window.request_animation_frame();
        }
        if self.tick_tab_close() {
            // Self-drive the next frame on the host view (DockHost), not App.
            window.request_animation_frame();
        }
        crate::perf::count("render.dock");
        let _perf = crate::perf::span_detail("dock.render", || {
            format!(
                "tiles={} cached_panels={} focused={}",
                self.state.pane_tree.tiles.iter().count(),
                self.file_panel_cache.len(),
                self.state.focused_tile.0
            )
        });
        // Focus is synced from `App::render` (root.rs), not here: hosting the dock in
        // its own `DockHost` view means this render no longer runs every App frame, and
        // pane focus must be re-asserted every frame (see `sync_overlay_focus`).
        let is_dragging = cx.has_active_drag();
        if is_dragging {
            if let Some(pane) = self.state.dragging_pane {
                let parent = self.state.find_tabs_container(pane).and_then(|id| self.state.find_parent_of(id));
                // A Ctrl toggle retargets only a drag collapse. A simultaneous
                // real close in the same split must retain its own timeline.
                let real_close = parent.is_some_and(|parent| self.pane_closing.keys()
                    .any(|closing| self.state.container_contains_pane(parent, *closing)));
                let collapse = !window.modifiers().control;
                if !real_close && parent.and_then(|id| self.dock_split_animations.get(&id))
                    .is_some_and(|animation| animation.closing != collapse) {
                    self.animate_pane_drag(pane, collapse);
                }
            }
        }
        if !is_dragging { self.tab_reorder_preview = None; }
        if !is_dragging && self.state.dragging_pane.is_some() {
            if !self.state.drag_pending {
                let pane_id = self.state.dragging_pane.unwrap();
                self.state.dragging_pane = None;
                self.animate_pane_drag(pane_id, false);
                // Settle frame on the host view (DockHost), not App.
                window.request_animation_frame();
            }
        } else if is_dragging {
            self.state.drag_pending = false;
        }

        // Poda de views/fingerprints de tiles que ya no existen (split/close/dnd).
        let tiles = &self.state.pane_tree.tiles;
        self.dock_split_animations.retain(|id, animation| {
            matches!(tiles.get(*id), Some(Tile::Container(Container::Linear(linear)))
                if linear.children.len() == animation.to.len())
        });
        self.pane_tabs_cache
            .retain(|id, _| tiles.get(*id).is_some());
        self.pane_tabs_fp.retain(|id, _| tiles.get(*id).is_some());
        self.pane_tabs_bounds
            .retain(|id, _| tiles.get(*id).is_some());
        self.details_row_rings
            .retain(|id, _| tiles.get(*id).is_some());

        let view = cx.entity();
        crate::ui::current_bounds::current_bounds(move |bounds, window, cx| {
            view.update(cx, |this, cx| {
                // Use this layout pass's constraints. Measuring after building
                // the absolute slots made them trail window resize by a frame.
                this.dock_root_bounds = Some(bounds);
                let content = this.state.pane_tree.root
                    .and_then(|root| {
                        this.render_flat_tree(root, window, cx)
                            .or_else(|| this.render_tree(root, window, cx))
                    })
                    .unwrap_or_else(|| div().size_full().into_any_element());
                this.with_root_drop_zones(content, window, cx)
            })
        })
        .into_any_element()
    }

    fn with_root_drop_zones(&self, content: AnyElement, window: &Window, cx: &Context<Self>) -> AnyElement {
        use crate::ui::tab_reorder::RootDropEdge;
        let mut host = div().relative().size_full().child(content);
        if !cx.active_drag().is_some_and(|drag| drag.value.is::<TabDrag>()) {
            return host.into_any_element();
        }
        if !window.modifiers().control && self.state.pane_tree.tiles.iter()
            .filter(|(_, tile)| matches!(tile, Tile::Pane(_))).take(2).count() < 2 {
            return host.into_any_element();
        }
        let band = 20.0 * f32::from(window.rem_size()) / crate::ui::BASE_PX;
        let accent = cx.theme().accent;
        let animate = !self.state.settings.disable_animations && !cx.reduce_motion();
        let preview_app = cx.entity();
        // Canvas paints without a hitbox. Its translucent prediction therefore
        // cannot intercept the pointer or turn the whole half-window into a zone.
        host = host.child(gpui::canvas(|bounds, _, _| bounds, move |_, bounds, window, cx| {
            let mouse = window.mouse_position();
            if preview_app.read(cx).tab_drop_target_at(f32::from(mouse.x), f32::from(mouse.y)).is_some() {
                return;
            }
            let pos = mouse - bounds.origin;
            let (w, h) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
            let Some(edge) = RootDropEdge::at(f32::from(pos.x), f32::from(pos.y), w, h, band) else { return; };
            let [x, y, w, h] = edge.preview(w, h);
            let prediction = gpui::Bounds::new(bounds.origin + gpui::point(px(x), px(y)), gpui::size(px(w), px(h)));
            let pulse = if animate {
                static CLOCK: OnceLock<Instant> = OnceLock::new();
                let t = CLOCK.get_or_init(Instant::now).elapsed().as_secs_f32();
                window.request_animation_frame();
                0.68 + 0.24 * (t * 5.0).sin().abs()
            } else { 0.85 };
            window.paint_quad(gpui::quad(prediction, px(3.0), accent.opacity(0.16), px(2.0), accent.opacity(pulse), gpui::BorderStyle::Solid));
        }).absolute().inset_0());
        for edge in [RootDropEdge::RootTop, RootDropEdge::RootBottom, RootDropEdge::RootLeft, RootDropEdge::RootRight] {
            let app = cx.entity();
            let target_app = app.clone();
            // Root edges overlap the top/side tab controls. Let the tab under
            // the cursor receive its reorder drop instead of splitting the root.
            let zone = div().id(SharedString::from(format!("root-drop-{edge:?}"))).absolute()
                .can_drop(move |_, window, cx| {
                    let pos = window.mouse_position();
                    target_app.read(cx).tab_drop_target_at(f32::from(pos.x), f32::from(pos.y)).is_none()
                });
            let zone = match edge {
                RootDropEdge::RootTop => zone.top_0().left_0().right_0().h(px(band)),
                RootDropEdge::RootBottom => zone.bottom_0().left_0().right_0().h(px(band)),
                RootDropEdge::RootLeft => zone.left_0().top(px(band)).bottom(px(band)).w(px(band)),
                RootDropEdge::RootRight => zone.right_0().top(px(band)).bottom(px(band)).w(px(band)),
            };
            host = host.child(zone.on_drop::<TabDrag>(move |drag, window, cx| {
                app.update(cx, |this, cx| {
                    tracing::debug!(pane = drag.pane_id.0, ?edge, "dnd tab root drop");
                    if let Some(id) = this.tab_for_drop(drag.pane_id, window.modifiers().control) {
                        this.split_tab_at_root(id, edge.vertical(), edge.before());
                    }
                    cx.notify();
                });
            }));
        }
        host.into_any_element()
    }

    fn render_flat_tree(
        &mut self,
        root: TileId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let bounds = self.dock_root_bounds?;
        let w = f32::from(bounds.size.width);
        let h = f32::from(bounds.size.height);
        if w <= 1.0 || h <= 1.0 {
            return None;
        }

        let mut panes = Vec::new();
        let mut splitters = Vec::new();
        let rect = FlatRect {
            x: 0.0,
            y: 0.0,
            w,
            h,
        };
        // The newest transition owns the seam accent. Every live container still
        // interpolates its geometry, so a rapid split on another axis cannot snap
        // an earlier transition to its destination.
        let active_animation = self
            .dock_split_animations
            .values()
            .filter(|animation| !self.state.settings.disable_animations && !animation.done())
            .max_by_key(|animation| animation.started)
            .map(|animation| animation.started);
        if !self.collect_flat_layout(
            root,
            rect,
            rect,
            window,
            cx,
            &mut panes,
            &mut splitters,
            (0.0, 0.0),
            active_animation,
        ) {
            return None;
        }

        let view = cx.entity();
        let flip_active = active_animation.is_some();
        let stable_flip = flip_active && stable_flip_layout_enabled();
        let mut host = div().absolute().inset_0().overflow_hidden();
        let mut closing_panes = Vec::new();
        let mut opening_panes = Vec::new();
        let mut live_panes = Vec::new();

        for pane in panes {
            let motion = self.split_motion_for_tile(pane.tile);
            let is_closing = matches!(motion, Some(SplitMotion::Closing));
            let is_opening = matches!(motion, Some(SplitMotion::Opening));
            let current_rect = pane.rect;
            if is_closing && (current_rect.w < 12.0 || current_rect.h < 12.0) {
                // A departing pane collapsed below visible threshold needs no scene replay while its logical
                // teardown is pending during another pane's transition.
                continue;
            }
            let (rect, paint_offset) = pane_visual_layout(
                current_rect,
                pane.layout_rect,
                is_closing,
                is_opening,
                stable_flip,
                self.pane_tabs_bounds
                    .get(&pane.tile)
                    .map(|previous| FlatRect {
                        x: f32::from(previous.origin.x - bounds.origin.x),
                        y: f32::from(previous.origin.y - bounds.origin.y),
                        w: f32::from(previous.size.width),
                        h: f32::from(previous.size.height),
                    }),
            );
            if stable_flip {
                crate::perf::count("dock.retained_reveal.pane");
                let offset = if is_closing {
                    pane_exit_paint_offset(current_rect, rect, pane.close_axis)
                } else {
                    paint_offset
                };
                let content = self.render_cached_tabs(pane.tile, is_closing, window, cx);
                host = host.child(layout_region(
                    "dock.pane_slot",
                    PaneReveal {
                        child: div()
                            .absolute()
                            .left(px(rect.x))
                            .top(px(rect.y))
                            .w(px(rect.w.max(0.0)))
                            .h(px(rect.h.max(0.0)))
                            .paint_offset(gpui::point(px(offset.0), px(offset.1)))
                            .child(content)
                            .into_any_element(),
                        // Clip in window space; cached prepaint stays at the stable
                        // layout origin while the content moves only during paint.
                        clip_offset: gpui::point(
                            px(current_rect.x - rect.x),
                            px(current_rect.y - rect.y),
                        ),
                        clip_size: gpui::size(
                            px(current_rect.w.max(0.0)),
                            px(current_rect.h.max(0.0)),
                        ),
                    },
                ));
                continue;
            }
            let content = if rect.w <= 4.0 || rect.h <= 4.0 {
                div().size_full().into_any_element()
            } else if is_closing {
                let bounds = self
                    .pane_tabs_bounds
                    .get(&pane.tile)
                    .cloned()
                    .unwrap_or_default();
                let left_shift = if pane.close_axis.0 > 0.0 {
                    current_rect.w - f32::from(bounds.size.width)
                } else {
                    0.0
                };
                let top_shift = if pane.close_axis.1 > 0.0 {
                    current_rect.h - f32::from(bounds.size.height)
                } else {
                    0.0
                };
                div()
                    .absolute()
                    .left(px(left_shift))
                    .top(px(top_shift))
                    .w(bounds.size.width)
                    .h(bounds.size.height)
                    .child(self.render_cached_tabs(pane.tile, true, window, cx))
                    .into_any_element()
            } else if is_opening {
                // Target-First: inner content is laid out at its final destination dimensions,
                // while the outer container (rect) smoothly expands from 0 to final width,
                // cleanly revealing the content as the splitter seam slides.
                div()
                    .w(px(pane.layout_rect.w))
                    .h(px(pane.layout_rect.h))
                    .overflow_hidden()
                    .child(self.render_cached_tabs(pane.tile, false, window, cx))
                    .into_any_element()
            } else {
                self.render_cached_tabs(pane.tile, false, window, cx)
            };
            let animated = div().size_full().child(content);
            let pane = div()
                .absolute()
                .left(px(rect.x))
                .top(px(rect.y))
                .w(px(rect.w.max(0.0)))
                .h(px(rect.h.max(0.0)))
                .min_w(px(0.))
                .min_h(px(0.))
                .overflow_hidden()
                .when(paint_offset.0 != 0.0 || paint_offset.1 != 0.0, |this| {
                    this.paint_offset(gpui::point(px(paint_offset.0), px(paint_offset.1)))
                })
                .child(animated);
            if is_closing {
                closing_panes.push(pane);
            } else if is_opening {
                opening_panes.push(pane);
            } else {
                live_panes.push(pane);
            }
        }

        // Transition panes stay underneath. Survivors slide over them from their
        // previous origins, which reveals an opening pane and covers a closing one
        // without a second pop/scale/exit animation on the pane itself.
        for pane in closing_panes
            .into_iter()
            .chain(opening_panes)
            .chain(live_panes)
        {
            host = host.child(layout_region("dock.pane_slot", pane));
        }

        for splitter in splitters {
            let thickness = match splitter.dir {
                LinearDir::Horizontal => splitter.rect.w,
                LinearDir::Vertical => splitter.rect.h,
            };
            let splitter_el = crate::ui::dock::resize::splitter(
                splitter.container,
                splitter.divider,
                splitter.dir,
                &view,
                cx,
                thickness,
            );
            // One quiet seam highlight acknowledges the changed boundary. A
            // single quad fades with the layout clock; no glow/filter/extra timer.
            let seam = self
                .dock_split_animations
                .get(&splitter.container)
                .filter(|animation| !animation.closing && Some(animation.started) == active_animation)
                .map(|animation| {
                    let p = animation.progress();
                    let color = cx.theme().primary.opacity(0.32 * 4.0 * p * (1.0 - p));
                    let seam = div().absolute().bg(color);
                    match splitter.dir {
                        LinearDir::Horizontal => seam
                            .left(px((thickness - 1.0).max(0.0) * 0.5))
                            .top_0()
                            .bottom_0()
                            .w(px(thickness.min(1.0))),
                        LinearDir::Vertical => seam
                            .top(px((thickness - 1.0).max(0.0) * 0.5))
                            .left_0()
                            .right_0()
                            .h(px(thickness.min(1.0))),
                    }
                });
            host = host.child(timed_layout_region(
                "dock.splitters",
                DOCK_SPLITTER_TIMINGS,
                div()
                    .absolute()
                    .left(px(splitter.rect.x))
                    .top(px(splitter.rect.y))
                    .w(px(splitter.rect.w.max(0.0)))
                    .h(px(splitter.rect.h.max(0.0)))
                    .child(splitter_el)
                    .children(seam),
            ));
        }

        if stable_flip {
            // Paint-only FLIP deliberately keeps cached hitboxes at their final
            // geometry while panes are visually translated. One dock-wide occluder
            // prevents a short-lived click/drop mismatch without rebuilding every
            // pane's interaction tree on each animation frame.
            crate::perf::count("dock.stable_flip.occluder");
            host = host.child(div().absolute().inset_0().occlude());
        }

        Some(timed_layout_region("dock.flat_host", DOCK_FLAT_HOST_TIMINGS, host).into_any_element())
    }

    fn collect_flat_layout(
        &mut self,
        id: TileId,
        rect: FlatRect,
        layout_rect: FlatRect,
        window: &mut Window,
        cx: &mut Context<Self>,
        panes: &mut Vec<FlatPane>,
        splitters: &mut Vec<FlatSplitter>,
        close_axis: (f32, f32),
        active_animation: Option<Instant>,
    ) -> bool {
        match self.state.pane_tree.tiles.get(id).cloned() {
            Some(Tile::Pane(_)) => {
                panes.push(FlatPane {
                    tile: id,
                    rect,
                    layout_rect,
                    close_axis,
                });
                true
            }
            Some(Tile::Container(Container::Tabs(tabs))) => {
                if tabs.children.is_empty() {
                    false
                } else {
                    panes.push(FlatPane {
                        tile: id,
                        rect,
                        layout_rect,
                        close_axis,
                    });
                    true
                }
            }
            Some(Tile::Container(Container::Linear(linear))) => {
                if linear.children.is_empty() {
                    return false;
                }
                self.dock_linear_bounds.insert(
                    id,
                    gpui::Bounds {
                        origin: gpui::point(gpui::px(rect.x), gpui::px(rect.y)),
                        size: gpui::size(gpui::px(rect.w), gpui::px(rect.h)),
                    },
                );

                let mut final_shares = linear.shares.clone();
                if cx.has_active_drag() && !window.modifiers().control {
                    if let Some(dragged) = self.state.dragging_pane {
                        for (ix, child_id) in linear.children.iter().enumerate() {
                            if ix < final_shares.len()
                                && self.drag_vacates_container(*child_id, dragged)
                            {
                                final_shares[ix] = 0.0;
                            }
                        }
                    }
                }
                let drag_collapsing = cx.has_active_drag() && !window.modifiers().control
                    && self.state.dragging_pane.is_some_and(|pane| linear.children.iter()
                        .any(|child| self.drag_vacates_container(*child, pane)));
                let closing_layout = drag_collapsing || self
                    .dock_split_animations
                    .get(&id)
                    .is_some_and(|animation| animation.closing)
                    || linear.children.iter().any(|child| {
                        matches!(
                            self.split_motion_for_tile(*child),
                            Some(SplitMotion::Closing)
                        )
                    });
                if closing_layout {
                    crate::app::normalize_split_shares_allow_zero(&mut final_shares);
                } else {
                    crate::app::normalize_split_shares(&mut final_shares);
                }
                let flip_animation = self
                    .dock_split_animations
                    .get(&id)
                    .filter(|animation| {
                        !self.state.settings.disable_animations && !animation.done()
                            && animation.from.len() == linear.children.len()
                    })
                    .cloned();
                let sum: f32 = final_shares.iter().copied().filter(|v| *v > 0.0).sum();
                let shares = if final_shares.len() == linear.children.len() && sum > f32::EPSILON {
                    self.dock_split_shares(id, final_shares, window, cx)
                } else {
                    vec![1.0; linear.children.len()]
                };
                let share_sum = shares.iter().copied().filter(|v| *v > 0.0).sum::<f32>();

                let splitter_size = 5.0;
                let span = match linear.dir {
                    LinearDir::Horizontal => rect.w,
                    LinearDir::Vertical => rect.h,
                };
                let layout_span = match linear.dir {
                    LinearDir::Horizontal => layout_rect.w,
                    LinearDir::Vertical => layout_rect.h,
                };
                let num_splitters = linear.children.len().saturating_sub(1);
                let closing_ix = self
                    .dock_split_animations
                    .get(&id)
                    .filter(|animation| animation.closing)
                    .and_then(|animation| animation.closing_ix)
                    .or_else(|| drag_collapsing.then(|| linear.children.iter().position(|child|
                        self.state.dragging_pane.is_some_and(|pane| self.drag_vacates_container(*child, pane)))).flatten())
                    .or_else(|| {
                        linear.children.iter().position(|child| {
                            matches!(
                                self.split_motion_for_tile(*child),
                                Some(SplitMotion::Closing)
                            )
                        })
                    });
                let mut splitter_gaps = crate::app::DockSplitAnimation::gap_targets(&shares)
                    .iter().map(|factor| splitter_size * factor).collect::<Vec<_>>();
                if let Some(splitter_ix) = closing_ix.and_then(|ix| closing_splitter_index(ix, num_splitters)) {
                    splitter_gaps[splitter_ix] = 0.0;
                }
                let start_splitter_gaps = flip_animation.as_ref()
                    .filter(|animation| animation.from_gap_factors.len() == num_splitters)
                    .map(|animation| animation.from_gap_factors.iter()
                        .map(|factor| splitter_size * factor).collect::<Vec<_>>())
                    .unwrap_or_else(|| splitter_gaps.clone());
                let available = (span - splitter_gaps.iter().sum::<f32>()).max(0.0);
                let layout_available = (layout_span - splitter_gaps.iter().sum::<f32>()).max(0.0);
                let start_available = (span - start_splitter_gaps.iter().sum::<f32>()).max(0.0);
                let (start_shares, progress) = if let Some(animation) = flip_animation {
                    let progress = animation.progress();
                    (animation.from, progress)
                } else {
                    (shares.clone(), 1.0)
                };
                let start_share_sum = start_shares
                    .iter()
                    .copied()
                    .filter(|share| *share > 0.0)
                    .sum::<f32>();
                let mut offset = match linear.dir {
                    LinearDir::Horizontal => rect.x,
                    LinearDir::Vertical => rect.y,
                };
                let mut start_offset = offset;
                let mut layout_offset = match linear.dir {
                    LinearDir::Horizontal => layout_rect.x,
                    LinearDir::Vertical => layout_rect.y,
                };
                let mut any = false;

                for (ix, child_id) in linear.children.iter().copied().enumerate() {
                    if ix > 0 {
                        let gap = splitter_gaps[ix - 1];
                        let start_gap = start_splitter_gaps[ix - 1];

                        let current_offset = start_offset + (offset - start_offset) * progress;
                        let current_gap = start_gap + (gap - start_gap) * progress;

                        let srect = match linear.dir {
                            LinearDir::Horizontal => FlatRect {
                                x: current_offset,
                                y: rect.y,
                                w: current_gap,
                                h: rect.h,
                            },
                            LinearDir::Vertical => FlatRect {
                                x: rect.x,
                                y: current_offset,
                                w: rect.w,
                                h: current_gap,
                            },
                        };
                        splitters.push(FlatSplitter {
                            container: id,
                            divider: ix - 1,
                            dir: linear.dir,
                            rect: srect,
                        });
                        offset += gap;
                        start_offset += start_gap;
                        layout_offset += gap;
                    }

                    let share = if closing_layout {
                        shares.get(ix).copied().unwrap_or(1.0).max(0.0)
                    } else {
                        shares.get(ix).copied().unwrap_or(1.0).max(0.01)
                    };
                    let child_span =
                        split_child_span(share, share_sum, available, linear.children.len());
                    let layout_child_span =
                        split_child_span(share, share_sum, layout_available, linear.children.len());
                    let start_share = start_shares.get(ix).copied().unwrap_or(1.0).max(0.0);
                    let start_child_span = split_child_span(
                        start_share,
                        start_share_sum,
                        start_available,
                        linear.children.len(),
                    );

                    let current_offset = start_offset + (offset - start_offset) * progress;
                    let current_child_span =
                        start_child_span + (child_span - start_child_span) * progress;

                    let child_rect = match linear.dir {
                        LinearDir::Horizontal => FlatRect {
                            x: current_offset,
                            y: rect.y,
                            w: current_child_span,
                            h: rect.h,
                        },
                        LinearDir::Vertical => FlatRect {
                            x: rect.x,
                            y: current_offset,
                            w: rect.w,
                            h: current_child_span,
                        },
                    };
                    let layout_child_rect = match linear.dir {
                        LinearDir::Horizontal => FlatRect {
                            x: layout_offset,
                            y: layout_rect.y,
                            w: layout_child_span,
                            h: layout_rect.h,
                        },
                        LinearDir::Vertical => FlatRect {
                            x: layout_rect.x,
                            y: layout_offset,
                            w: layout_rect.w,
                            h: layout_child_span,
                        },
                    };
                    offset += child_span;
                    start_offset += start_child_span;
                    layout_offset += layout_child_span;
                    let child_close_axis = if matches!(
                        self.split_motion_for_tile(child_id),
                        Some(SplitMotion::Closing)
                    ) {
                        match linear.dir {
                            LinearDir::Horizontal => {
                                if ix + 1 < linear.children.len() {
                                    (1.0, 0.0)
                                } else {
                                    (-1.0, 0.0)
                                }
                            }
                            LinearDir::Vertical => {
                                if ix + 1 < linear.children.len() {
                                    (0.0, 1.0)
                                } else {
                                    (0.0, -1.0)
                                }
                            }
                        }
                    } else {
                        close_axis
                    };
                    any |= self.collect_flat_layout(
                        child_id,
                        child_rect,
                        layout_child_rect,
                        window,
                        cx,
                        panes,
                        splitters,
                        child_close_axis,
                        active_animation,
                    );
                }
                any
            }
            None => false,
        }
    }

    fn render_tree(
        &mut self,
        id: TileId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        crate::perf::count("dock.render_tree");
        match self.state.pane_tree.tiles.get(id)?.clone() {
            Tile::Pane(_) => Some(self.render_cached_tabs(id, false, window, cx)),
            Tile::Container(Container::Tabs(tabs)) => {
                if tabs.children.is_empty() {
                    return None;
                }
                Some(self.render_cached_tabs(id, false, window, cx))
            }
            Tile::Container(Container::Linear(linear)) => {
                let mut children = Vec::new();
                for child in &linear.children {
                    if let Some(el) = self.render_tree(*child, window, cx) {
                        children.push((*child, el, self.split_motion_for_tile(*child)));
                    }
                }
                if children.is_empty() {
                    return None;
                }
                let container_bounds = self.dock_linear_bounds.get(&id).copied();
                let exact_span = if linear.dir == LinearDir::Horizontal {
                    container_bounds.map(|b| f32::from(b.size.width))
                } else {
                    container_bounds.map(|b| f32::from(b.size.height))
                };

                let mut row = if exact_span.is_some() {
                    div().relative()
                } else {
                    if linear.dir == LinearDir::Horizontal {
                        h_flex()
                    } else {
                        v_flex()
                    }
                }
                .size_full()
                .min_w(px(0.))
                .min_h(px(0.));

                if exact_span.is_none() {
                    row = row.items_stretch();
                }

                let mut final_shares = linear.shares.clone();
                if cx.has_active_drag() && !window.modifiers().control {
                    if let Some(dragged) = self.state.dragging_pane {
                        for (ix, child_id) in linear.children.iter().enumerate() {
                            if self.drag_vacates_container(*child_id, dragged) {
                                final_shares[ix] = 0.0;
                            }
                        }
                    }
                }
                let drag_collapsing = cx.has_active_drag() && !window.modifiers().control
                    && self.state.dragging_pane.is_some_and(|pane| linear.children.iter()
                        .any(|child| self.drag_vacates_container(*child, pane)));
                let closing_layout = drag_collapsing || self
                    .dock_split_animations
                    .get(&id)
                    .is_some_and(|animation| animation.closing)
                    || linear.children.iter().any(|child| {
                        matches!(
                            self.split_motion_for_tile(*child),
                            Some(SplitMotion::Closing)
                        )
                    });
                if closing_layout {
                    crate::app::normalize_split_shares_allow_zero(&mut final_shares);
                } else {
                    crate::app::normalize_split_shares(&mut final_shares);
                }
                let sum: f32 = final_shares.iter().copied().filter(|v| *v > 0.0).sum();
                let shares = if final_shares.len() == children.len() && sum > f32::EPSILON {
                    self.dock_split_shares(id, final_shares, window, cx)
                } else {
                    vec![1.0; children.len()]
                };
                let share_sum = shares.iter().copied().filter(|v| *v > 0.0).sum::<f32>();

                let view = cx.entity();
                let child_count = children.len();
                let num_splitters = child_count.saturating_sub(1);
                let splitter_size = 5.0; // from resize.rs splitter thickness
                let closing_ix = self
                    .dock_split_animations
                    .get(&id)
                    .filter(|animation| animation.closing)
                    .and_then(|animation| animation.closing_ix)
                    .or_else(|| drag_collapsing.then(|| linear.children.iter().position(|child|
                        self.state.dragging_pane.is_some_and(|pane| self.drag_vacates_container(*child, pane)))).flatten())
                    .or_else(|| {
                        linear.children.iter().position(|child| {
                            matches!(
                                self.split_motion_for_tile(*child),
                                Some(SplitMotion::Closing)
                            )
                        })
                    });
                let closing_splitter_ix =
                    closing_ix.and_then(|pane_ix| closing_splitter_index(pane_ix, num_splitters));
                let mut splitter_gaps = vec![splitter_size; num_splitters];
                if let Some(splitter_ix) = closing_splitter_ix {
                    splitter_gaps[splitter_ix] = 0.0;
                }
                let total_splitter_size = splitter_gaps.iter().sum::<f32>();
                let mut current_offset = 0.0;

                for (ix, (_child_id, child, intro)) in children.into_iter().enumerate() {
                    let share = if closing_layout {
                        shares[ix].max(0.0)
                    } else {
                        shares[ix].max(0.01)
                    };

                    if ix > 0 {
                        let gap = splitter_gaps[ix - 1];
                        let splitter_el = crate::ui::dock::resize::splitter(
                            id,
                            ix - 1,
                            linear.dir,
                            &view,
                            cx,
                            gap,
                        );
                        if exact_span.is_some() {
                            let mut s_div = div().absolute();
                            if linear.dir == LinearDir::Horizontal {
                                s_div =
                                    s_div.left(px(current_offset)).top_0().bottom_0().w(px(gap));
                            } else {
                                s_div = s_div.top(px(current_offset)).left_0().right_0().h(px(gap));
                            }
                            row = row.child(s_div.child(splitter_el));
                            current_offset += gap;
                        } else {
                            row = row.child(splitter_el);
                        }
                    }

                    let child_span = if let Some(span) = exact_span {
                        let available = (span - total_splitter_size).max(0.0);
                        Some(split_child_span(share, share_sum, available, child_count))
                    } else {
                        None
                    };

                    let mut pane_div = div().size_full().min_w(px(0.)).min_h(px(0.));

                    if let Some(span) = child_span {
                        pane_div = pane_div.absolute();
                        if linear.dir == LinearDir::Horizontal {
                            pane_div = pane_div
                                .left(px(current_offset))
                                .top_0()
                                .bottom_0()
                                .w(px(span));
                        } else {
                            pane_div = pane_div
                                .top(px(current_offset))
                                .left_0()
                                .right_0()
                                .h(px(span));
                        }
                        current_offset += span;
                    } else {
                        pane_div = pane_div.flex_grow(share).flex_basis(px(0.));
                    }

                    // A closing pane whose span has already collapsed paints no content:
                    // Avoid drawing a nearly collapsed slot in the fallback tree.
                    let collapsed_closing = matches!(intro, Some(SplitMotion::Closing))
                        && child_span.is_some_and(|s| s <= 6.0);
                    let content = if collapsed_closing {
                        div().size_full().into_any_element()
                    } else {
                        child
                    };
                    row = row.child(pane_div.child(content));
                }

                let view = cx.entity();
                Some(
                    row.on_prepaint_child(move |bounds, _window, cx| {
                        crate::perf::count("prepaint.dock_linear");
                        view.update(cx, |this, _cx| {
                            this.dock_linear_bounds.insert(id, bounds);
                        });
                    })
                    .into_any_element(),
                )
            }
        }
    }

    fn split_motion_for_tile(&self, id: TileId) -> Option<SplitMotion> {
        if self.state.settings.disable_animations {
            return None;
        }
        match self.state.pane_tree.tiles.get(id)? {
            Tile::Pane(_) => self
                .pane_closing
                .get(&id)
                .copied()
                .map(|_| SplitMotion::Closing)
                .or_else(|| {
                    self.pane_animation_for(id)
                        .filter(|animation| {
                            matches!(
                                animation.kind,
                                PaneAnimationKind::SplitHorizontal
                                    | PaneAnimationKind::SplitVertical
                            )
                        })
                        .map(|_| SplitMotion::Opening)
                }),
            Tile::Container(Container::Tabs(tabs)) => tabs
                .children
                .iter()
                .find_map(|child| self.split_motion_for_tile(*child)),
            Tile::Container(Container::Linear(_)) => None,
        }
    }

    fn dock_split_shares(
        &mut self,
        id: TileId,
        final_shares: Vec<f32>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<f32> {
        let Some(anim) = self.dock_split_animations.get(&id) else {
            return final_shares;
        };
        if anim.to.len() != final_shares.len() {
            self.dock_split_animations.remove(&id);
            if !self.is_pane_or_dock_animating() {
                self.flush_pending_column_reflows(cx);
            }
            return final_shares;
        }
        if anim.done() || self.state.settings.disable_animations {
            self.dock_split_animations.remove(&id);
            if !self.is_pane_or_dock_animating() {
                self.flush_pending_column_reflows(cx);
            }
            return final_shares;
        }
        // FLIP: layout jumps to final shares once. Cached paint moves from the
        // previous origins, so Taffy/prepaint do not repeat on every animation frame.
        window.request_animation_frame();
        let progress = anim.progress();
        let elapsed_ms = anim.started.elapsed().as_millis();
        if anim.closing && (elapsed_ms < 20 || elapsed_ms % 80 < 18) {
            crate::perf::event(|| {
                format!(
                    "pane_close.flip_frame parent={} elapsed_ms={} progress={:.3}",
                    id.0, elapsed_ms, progress
                )
            });
        }
        final_shares
    }

    /// Embed the tabs-like tile as a cached `PaneTabsView`. The fingerprint captures
    /// every bit of state read by `render_tabs_like`/`render_tab_strip`; if it did not
    /// change, the view is not notified and GPUI reuses the previous layout, hitboxes,
    /// and paint (pane chrome costs one Taffy node).
    fn render_cached_tabs(
        &mut self,
        id: TileId,
        is_closing: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let fp = self.tabs_fingerprint(id, window, cx);
        let view = if let Some(view) = self.pane_tabs_cache.get(&id) {
            view.clone()
        } else {
            let app = cx.entity().downgrade();
            let view = cx.new(|_| PaneTabsView { app, tile: id });
            self.pane_tabs_cache.insert(id, view.clone());
            view
        };
        if self.pane_tabs_fp.insert(id, fp) != Some(fp) && !is_closing {
            view.update(cx, |_, cx| cx.notify());
        }
        let style = gpui::StyleRefinement::default().size_full();

        let app = cx.entity();
        let host = div()
            .size_full()
            .overflow_hidden()
            .child(layout_region(
                "pane.cached_view",
                gpui::AnyView::from(view).cached(style),
            ))
            .on_prepaint_child(move |bounds, _window, cx| {
                let should_update = {
                    let app_ref = app.read(cx);
                    app_ref.state.pane_tree.tiles.get(id).is_some()
                        && app_ref.pane_tabs_bounds.get(&id) != Some(&bounds)
                };
                if should_update {
                    app.update(cx, |this, _cx| {
                        this.pane_tabs_bounds.insert(id, bounds);
                    });
                }
            });
        timed_layout_region("pane.cached_host", PANE_CACHED_HOST_TIMINGS, host).into_any_element()
    }

    fn tabs_fingerprint(&self, id: TileId, window: &Window, cx: &Context<Self>) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        let epoch = self.startup_started;
        let hash_instant = |h: &mut std::collections::hash_map::DefaultHasher, i: Instant| {
            i.saturating_duration_since(epoch).as_nanos().hash(h);
        };

        let children: Vec<TileId> = match self.state.pane_tree.tiles.get(id) {
            Some(Tile::Pane(_)) => vec![id],
            Some(Tile::Container(Container::Tabs(tabs))) => {
                tabs.active.map(|a| a.0).hash(&mut h);
                tabs.children.clone()
            }
            _ => Vec::new(),
        };
        for child in &children {
            child.0.hash(&mut h);
            if let Some(t) = self.tab_data_for(*child) {
                t.location.display_path().hash(&mut h);
                if let Some(p) = t.location.fs_dir() {
                    self.grid_thumbnail(&p.to_path_buf()).is_some().hash(&mut h);
                }
            }
            if let Some(started) = self.tab_closing.get(child) {
                hash_instant(&mut h, *started);
            }
            self.pane_closing.contains_key(child).hash(&mut h);
            if let Some(anim) = (!self.state.settings.disable_animations)
                .then(|| self.pane_animation_for(*child))
                .flatten()
            {
                std::mem::discriminant(&anim.kind).hash(&mut h);
                hash_instant(&mut h, anim.started);
            }
        }
        // Global state that changes chrome, Alt overlay for alt-drag
        // drag, and active drag state for body drop zones.
        self.state.profiles.active().theme.hash(&mut h);
        self.state.settings.glass_opacity.to_bits().hash(&mut h);
        self.state.settings.show_titlebar.hash(&mut h);
        if self.state.settings.show_titlebar {
            f32::from(window.viewport_size().width)
                .to_bits()
                .hash(&mut h);
        }
        self.state.settings.disable_animations.hash(&mut h);
        self.state.settings.dim_inactive_panes.hash(&mut h);
        if self.state.settings.dim_inactive_panes {
            self.state.focused_tile.0.hash(&mut h);
        }
        window.modifiers().alt.hash(&mut h);
        window.modifiers().control.hash(&mut h);
        cx.has_active_drag().hash(&mut h);
        self.drag_hover_maybe_active().hash(&mut h);
        self.state.dragging_pane.map(|p| p.0).hash(&mut h);
        self.tab_reorder_preview.hash(&mut h);
        self.state.drag_pending.hash(&mut h);
        self.rename_edit
            .as_ref()
            .map(|edit| edit.pane.0)
            .hash(&mut h);
        h.finish()
    }

    fn render_tabs_like(
        &mut self,
        children: Vec<TileId>,
        active: Option<TileId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        crate::perf::count("render.tabs_like");
        // root.rs owns modifier changes through the element's paint listener.
        // A cached pane may render during prepaint; registering a second window
        // listener here violates GPUI's phase rules.
        let _perf = crate::perf::span_detail("dock.tabs", || {
            format!(
                "children={} active={}",
                children.len(),
                active.map_or(0, |id| id.0)
            )
        });
        let active = active
            .filter(|id| children.contains(id))
            .or_else(|| children.first().copied());
        let active_theme = self.state.profiles.active().theme.as_str();
        let is_glass = active_theme.ends_with(" Glass");
        let windows_theme = is_windows_theme(active_theme);
        let panel_bg = if is_glass || windows_theme {
            cx.theme().colors.list
        } else {
            cx.theme().background
        };
        let active_id = active.unwrap_or_else(|| *children.first().unwrap());
        let content = active
            .map(|id| {
                if self.should_render_pane_body(id) {
                    // Zed pattern (pane_group.rs:554): pane cached as AnyView. If the
                    // pane was not notified and its bounds did not change, GPUI reuses
                    // the previous layout, hitboxes, and primitives: one Taffy node
                    // instead of roughly 250.
                    gpui::AnyView::from(self.get_or_make_file_panel(id, cx))
                        .cached(gpui::StyleRefinement::default().size_full())
                        .into_any_element()
                } else {
                    crate::perf::count("render.pane_body_skipped");
                    div().size_full().into_any_element()
                }
            })
            .unwrap_or_else(|| {
                div()
                    .w_full()
                    .flex_1()
                    .min_h(px(0.))
                    .min_w(px(0.))
                    .into_any_element()
            });
        let content = if let Some(active) = active {
            let is_focused = self.state.focused_tile == active;
            let has_filter = self
                .state
                .tab_states
                .get(&active)
                .is_some_and(|ts| ts.search.active && !ts.search.query.is_empty());
            let mut pane_box = div().size_full().relative().child(content);

            // Only the active pane renders the full floating menu (view switcher + file/folder toggles).
            // Inactive splits do not render the floating controls, avoiding lag across multiple splits.
            if is_focused || has_filter {
                let (filter_pill, controls_pill) = self.render_pane_pills(active, window, cx);
                pane_box = pane_box.child(filter_pill).child(controls_pill);
            }
            self.with_split_drop_zones(active, pane_box.into_any_element(), cx)
        } else {
            content
        };

        let panel = FastPaneShell::new(
            active_id,
            self.render_tab_strip(children, active, window, cx),
            content,
        )
        .size_full()
        .min_w(px(0.))
        .min_h(px(0.))
        .overflow_hidden()
        .when(
            self.state.settings.dim_inactive_panes && self.state.focused_tile != active_id,
            |s| s.opacity(0.65),
        )
        .when(is_glass, |s| s.glass_surface(panel_bg))
        .when(!is_glass, |s| s.rounded(cx.theme().radius_lg).bg(panel_bg))
        .when(!is_glass && !windows_theme, |s| {
            s.border_1().border_color(cx.theme().border.opacity(0.55))
        });

        let is_mod = window.modifiers().alt;
        let activate_drag_app = cx.entity().clone();
        let activate_drag_app_down = cx.entity().clone();
        let activate_drag_app_up = cx.entity().clone();
        let app_for_resize = cx.entity().clone();

        let panel = if is_mod {
            div()
                .size_full()
                .relative()
                .child(panel)
                .child(layout_region(
                    "pane.drag_overlay",
                    div()
                        .id(gpui::SharedString::from(format!(
                            "alt-drag-{}",
                            active_id.0
                        )))
                        .absolute()
                        .inset_0()
                        .cursor_grab()
                        .on_drag_alt(tab_drag_source(
                            TabDrag {
                                pane_id: active_id,
                                content: self.get_or_make_file_panel(active_id, cx).clone().into(),
                                size: self.pane_view_bounds.get(&active_id).map(|b| b.size),
                            },
                            move |drag, _pos, _window, cx| {
                                activate_drag_app.update(cx, |this, cx| {
                                    this.animate_pane_drag(drag.pane_id, true);
                                    this.state.dragging_pane = Some(drag.pane_id);
                                    this.state.drag_pending = false;
                                    cx.notify();
                                });
                                tab_drag_entity(drag.pane_id, drag.content.clone(), drag.size, cx)
                            },
                        ))
                        .on_mouse_down(MouseButton::Left, move |_, _window, cx| {
                            activate_drag_app_down.update(cx, |this, _cx| {
                                if this.state.dragging_pane != Some(active_id) {
                                    this.animate_pane_drag(active_id, true);
                                    this.state.dragging_pane = Some(active_id);
                                    this.state.drag_pending = true;
                                }
                            });
                        })
                        .on_mouse_up(MouseButton::Left, move |_, _window, cx| {
                            activate_drag_app_up.update(cx, |this, cx| {
                                if this.state.drag_pending {
                                    this.state.drag_pending = false;
                                    if this.state.dragging_pane == Some(active_id) {
                                        this.state.dragging_pane = None;
                                        this.animate_pane_drag(active_id, false);
                                        cx.notify();
                                    }
                                }
                            });
                        })
                        .on_mouse_down(MouseButton::Right, move |ev, _window, cx| {
                            app_for_resize.update(cx, |this, _cx| {
                                this.start_alt_resize(active_id, ev.position);
                            });
                            cx.stop_propagation();
                        }),
                ))
                .into_any_element()
        } else {
            panel.into_any_element()
        };

        timed_layout_region("pane.shell", PANE_SHELL_TIMINGS, panel).into_any_element()
    }

    fn should_render_pane_body(&self, _pane_id: TileId) -> bool {
        true
    }

    fn render_tab_strip(
        &mut self,
        children: Vec<TileId>,
        active: Option<TileId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let border = cx.theme().border;
        let muted = cx.theme().muted_foreground;
        let active_theme = self.state.profiles.active().theme.as_str();
        let is_glass = active_theme.ends_with(" Glass");
        let tab_bar_bg = if is_glass || crate::ui::gridline::active() {
            cx.theme().tab_bar
        } else {
            backdrop_tint(cx.theme().tab_bar, 0.16)
        };
        let active_bg = if is_glass || crate::ui::gridline::active() {
            cx.theme().tab_active
        } else {
            backdrop_tint(cx.theme().tab_active, 0.22)
        };
        let active_fg = cx.theme().tab_active_foreground;
        let tab_fg = cx.theme().tab_foreground;
        let windows_theme = is_windows_theme(active_theme);
        let hover_bg = if is_glass {
            cx.theme().list_hover
        } else {
            border.opacity(0.22)
        };

        let app = cx.entity();
        let strip_height = crate::ui::rem_px(if windows_theme { 46.0 } else { 34.0 });
        let font = crate::ui::surface::chrome_ui_font(window);
        let font_size = crate::ui::surface::chrome_font_size_tabs(window, windows_theme);

        let mut tab_items = Vec::with_capacity(children.len());

        for pane_id in &children {
            let (label, tag_color) = self
                .tab_data_for(*pane_id)
                .map(|t| {
                    let tag_color = match &t.location {
                        Location::Tag(id) => self
                            .state
                            .tag_cache
                            .def(*id)
                            .map(|def| def.color),
                        _ => None,
                    };
                    let label = match &t.location {
                        Location::Tag(id) => self.state.tag_cache.def(*id).map_or_else(
                            || display_name(&t.location.display_path()),
                            |def| def.name.to_string(),
                        ),
                        _ => display_name(&t.location.display_path()),
                    };
                    (label, tag_color)
                })
                .unwrap_or_else(|| (rust_i18n::t!("fallback_tab_title").to_string(), None));

            tab_items.push(crate::ui::views::tabs_bar_element::TabBarItem {
                pane_id: *pane_id,
                label,
                is_active: Some(*pane_id) == active,
                tag_rgb: tag_color,
                closing: self.tab_closing.get(pane_id).copied(),
            });
        }

        let geometry = std::rc::Rc::new(std::cell::RefCell::new(
            crate::ui::views::tabs_bar_element::TabDropGeometry::default()));
        let reorder = self.tab_reorder_preview.as_ref().filter(|preview|
            preview.children == children && cx.has_active_drag()).cloned();
        let instant = self.state.settings.disable_animations || cx.reduce_motion();
        let tabs_bar = crate::ui::views::tabs_bar_element::TabsBarElement::new(
            app.downgrade(),
            tab_items,
            active,
            font,
            font_size,
            windows_theme,
            tab_bar_bg,
            active_bg,
            hover_bg,
            active_fg,
            tab_fg,
            muted,
            border,
            cx.theme().accent,
        ).interaction(geometry.clone(), reorder, instant);

        let mut strip_container = div()
            .relative()
            .w_full()
            .h(strip_height)
            .child(tabs_bar);
        let preview_app = app.clone();
        let preview_children = children.clone();
        let drop_app = app.clone();
        let drop_children = children.clone();
        strip_container = strip_container
            .on_drag_move::<TabDrag>(move |event, _window, cx| {
                let dragged = event.drag(cx).pane_id;
                let inside = event.bounds.contains(&event.event.position);
                let boundary = geometry.borrow().boundary_at(f32::from(event.event.position.x - event.bounds.left()));
                preview_app.update(cx, |this, cx| {
                    let next = inside.then(|| crate::ui::tab_reorder::TabReorderPreview { dragged, children: preview_children.clone(), boundary, copying: event.event.modifiers.control });
                    if !inside && this.tab_reorder_preview.as_ref().is_none_or(|preview| preview.children != preview_children) { return; }
                    if this.tab_reorder_preview != next {
                        this.tab_reorder_preview = next;
                        for view in this.pane_tabs_cache.values() { view.update(cx, |_, cx| cx.notify()); }
                        cx.notify();
                    }
                });
            })
            .on_drop::<TabDrag>(move |drag, window, cx| {
                drop_app.update(cx, |this, cx| {
                    let boundary = this.tab_reorder_preview.as_ref().filter(|preview| preview.dragged == drag.pane_id && preview.children == drop_children).map_or(drop_children.len(), |preview| preview.boundary);
                    tracing::debug!(pane = drag.pane_id.0, boundary, targets = drop_children.len(), "dnd tab strip drop");
                    if let Some(id) = this.tab_for_drop(drag.pane_id, window.modifiers().control) {
                        this.commit_tab_reorder(id, &drop_children, boundary);
                    }
                    cx.notify();
                });
            });

        let target_anchor = active.or_else(|| children.first().copied());
        if let Some(target) = target_anchor {
            let drop_app_ext = app.clone();
            let drop_app_ext2 = app;
            strip_container = strip_container
                .on_drop::<crate::ui::root::DraggedFiles>(move |d, window, cx| {
                    let paths = d.paths.clone();
                    drop_app_ext.update(cx, |this, cx| {
                        let pos = window.mouse_position();
                        let target = this.tab_drop_target_at(f32::from(pos.x), f32::from(pos.y)).unwrap_or(target);
                        if let Some(command) = this.tab_drop_command(target) {
                            this.execute_drag_hover_drop_command(command, paths, true);
                        }
                        cx.notify();
                    });
                })
                .on_drop::<gpui::ExternalPaths>(move |paths, window, cx| {
                    let paths = paths.paths().to_vec();
                    drop_app_ext2.update(cx, |this, cx| {
                        let pos = window.mouse_position();
                        let target = this.tab_drop_target_at(f32::from(pos.x), f32::from(pos.y)).unwrap_or(target);
                        if let Some(command) = this.tab_drop_command(target) {
                            this.execute_drag_hover_drop_command(command, paths, false);
                        }
                        cx.notify();
                    });
                });
        }

        let strip = if self.state.settings.show_titlebar {
            let height = strip_height.to_pixels(window.rem_size());
            let controls_top = crate::ui::window_chrome::controls_top(
                height,
                px(if is_glass { 6.0 } else { 3.0 }),
                f32::from(window.rem_size()) / crate::ui::BASE_PX,
            );
            crate::ui::window_chrome::CornerTabStrip::new(
                strip_container.into_any_element(),
                height,
                controls_top,
            )
            .into_any_element()
        } else {
            strip_container.into_any_element()
        };
        timed_layout_region("pane.tabs_bar", PANE_TABS_BAR_TIMINGS, strip).into_any_element()
    }

    fn with_split_drop_zones(
        &self,
        target: TileId,
        content: AnyElement,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut el = div()
            .w_full()
            .flex_1()
            .min_h(px(0.))
            .min_w(px(0.))
            .relative()
            .group("tab-split-zone")
            .child(content);

        if !cx.has_active_drag() {
            return el.into_any_element();
        }

        let app = cx.entity();
        let overlay_top_app = app.clone();
        let overlay_left_app = app.clone();
        let overlay_right_app = app.clone();
        let overlay_bottom_app = app.clone();
        let overlay_center_app = app;

        let drop_zones = div()
            .absolute()
            .inset_0()
            .child(
                split_drop_zone(AtlasIcon::SplitHorizontal)
                    .left_0()
                    .top(relative(0.25))
                    .bottom(relative(0.25))
                    .w(relative(0.35))
                    .on_drop::<TabDrag>(move |drag, window, cx| {
                        overlay_left_app.update(cx, |this, cx| {
                            if let Some(id) = this.tab_for_drop(drag.pane_id, window.modifiers().control) { this.split_existing_tab(target, id, false, true); }
                            cx.notify();
                        });
                    })
                    .child(
                        div()
                            .size_full()
                            .bg(cx.theme().background.opacity(0.85))
                            .opacity(0.0)
                            .group_hover("tab-split-zone", |s| s.opacity(1.0)),
                    ),
            )
            .child(
                split_drop_zone(AtlasIcon::SplitHorizontal)
                    .right_0()
                    .top(relative(0.25))
                    .bottom(relative(0.25))
                    .w(relative(0.35))
                    .on_drop::<TabDrag>(move |drag, window, cx| {
                        overlay_right_app.update(cx, |this, cx| {
                            if let Some(id) = this.tab_for_drop(drag.pane_id, window.modifiers().control) { this.split_existing_tab(target, id, false, false); }
                            cx.notify();
                        });
                    })
                    .child(
                        div()
                            .size_full()
                            .bg(cx.theme().background.opacity(0.85))
                            .opacity(0.0)
                            .group_hover("tab-split-zone", |s| s.opacity(1.0)),
                    ),
            )
            .child(
                split_drop_zone(AtlasIcon::SplitVertical)
                    .top_0()
                    .left_0()
                    .right_0()
                    .h(relative(0.25))
                    .on_drop::<TabDrag>(move |drag, window, cx| {
                        overlay_top_app.update(cx, |this, cx| {
                            if let Some(id) = this.tab_for_drop(drag.pane_id, window.modifiers().control) { this.split_existing_tab(target, id, true, true); }
                            cx.notify();
                        });
                    })
                    .child(
                        div()
                            .size_full()
                            .bg(cx.theme().background.opacity(0.85))
                            .opacity(0.0)
                            .group_hover("tab-split-zone", |s| s.opacity(1.0)),
                    ),
            )
            .child(
                split_drop_zone(AtlasIcon::SplitVertical)
                    .bottom_0()
                    .left_0()
                    .right_0()
                    .h(relative(0.25))
                    .on_drop::<TabDrag>(move |drag, window, cx| {
                        overlay_bottom_app.update(cx, |this, cx| {
                            if let Some(id) = this.tab_for_drop(drag.pane_id, window.modifiers().control) { this.split_existing_tab(target, id, true, false); }
                            cx.notify();
                        });
                    })
                    .child(
                        div()
                            .size_full()
                            .bg(cx.theme().background.opacity(0.85))
                            .opacity(0.0)
                            .group_hover("tab-split-zone", |s| s.opacity(1.0)),
                    ),
            )
            .child(
                split_drop_zone(AtlasIcon::Plus)
                    .left(relative(0.35))
                    .right(relative(0.35))
                    .top(relative(0.25))
                    .bottom(relative(0.25))
                    .on_drop::<TabDrag>(move |drag, window, cx| {
                        overlay_center_app.update(cx, |this, cx| {
                            if let Some(id) = this.tab_for_drop(drag.pane_id, window.modifiers().control) { this.move_tab_before(id, target); }
                            cx.notify();
                        });
                    })
                    .child(
                        div()
                            .size_full()
                            .bg(cx.theme().background.opacity(0.85))
                            .opacity(0.0)
                            .group_hover("tab-split-zone", |s| s.opacity(1.0)),
                    ),
            );

        el = el.child(timed_layout_region(
            "pane.drop_zones",
            PANE_DROP_ZONES_TIMINGS,
            drop_zones,
        ));

        el.into_any_element()
    }

    pub(crate) fn get_or_make_file_panel(
        &mut self,
        id: TileId,
        cx: &mut Context<Self>,
    ) -> Entity<FilePanel> {
        if let Some(panel) = self.file_panel_cache.get(&id) {
            return panel.clone();
        }
        let app = cx.entity();
        let signal = self.pane_signal_for(id, cx);
        let panel = cx.new(|cx| FilePanel::new(app, id, signal, cx));
        crate::perf::event(|| format!("new FilePanel pane={}", id.0));
        self.file_panel_cache.insert(id, panel.clone());
        panel
    }

    pub(crate) fn sync_pane_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.modal_overlay_active() || self.pane_closing.contains_key(&self.state.focused_tile) {
            return;
        }
        if let Some(panel) = self.file_panel_cache.get(&self.state.focused_tile) {
            window.focus(&panel.read(cx).focus_handle(cx), cx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_child_span_uses_share_ratio() {
        assert_eq!(split_child_span(1.0, 2.0, 100.0, 2), 50.0);
        assert_eq!(split_child_span(3.0, 4.0, 100.0, 2), 75.0);
    }

    #[test]
    fn split_child_span_falls_back_to_even_width() {
        assert_eq!(split_child_span(1.0, 0.0, 90.0, 3), 30.0);
    }

    #[test]
    fn collapses_the_divider_adjacent_to_the_closing_pane() {
        assert_eq!(closing_splitter_index(0, 2), Some(0));
        assert_eq!(closing_splitter_index(1, 2), Some(1));
        assert_eq!(closing_splitter_index(2, 2), Some(1));
        assert_eq!(closing_splitter_index(0, 0), None);
    }

    #[test]
    fn shrinking_pane_retains_content_extent_until_settlement() {
        let current = FlatRect {
            x: 120.0,
            y: 20.0,
            w: 300.0,
            h: 400.0,
        };
        let final_rect = FlatRect {
            x: 80.0,
            y: 20.0,
            w: 220.0,
            h: 400.0,
        };

        let (layout, offset) = pane_visual_layout(
            current,
            final_rect,
            false,
            false,
            true,
            Some(FlatRect {
                w: 440.0,
                ..current
            }),
        );
        assert_eq!(
            (layout.x, layout.y, layout.w, layout.h),
            (80.0, 20.0, 440.0, 400.0)
        );
        assert_eq!(offset, (40.0, 0.0));
    }

    #[test]
    fn closing_pane_retains_original_content_extent() {
        let current = FlatRect {
            x: 10.0,
            y: 20.0,
            w: 90.0,
            h: 400.0,
        };
        let final_rect = FlatRect {
            x: 10.0,
            y: 20.0,
            w: 0.0,
            h: 400.0,
        };

        let (layout, offset) = pane_visual_layout(
            current,
            final_rect,
            true,
            false,
            true,
            Some(FlatRect {
                w: 220.0,
                ..current
            }),
        );
        assert_eq!(
            (layout.x, layout.y, layout.w, layout.h),
            (10.0, 20.0, 220.0, 400.0)
        );
        assert_eq!(offset, (0.0, 0.0));
    }

    #[test]
    fn opening_pane_retains_target_layout_behind_the_moving_clip() {
        let current = FlatRect {
            x: 10.0,
            y: 20.0,
            w: 10.0,
            h: 400.0,
        };
        let final_rect = FlatRect {
            x: 80.0,
            y: 20.0,
            w: 220.0,
            h: 400.0,
        };

        let (layout, offset) = pane_visual_layout(current, final_rect, false, true, true, None);
        assert_eq!(
            (layout.x, layout.y, layout.w, layout.h),
            (80.0, 20.0, 220.0, 400.0)
        );
        assert_eq!(offset, (-70.0, 0.0));
    }

    #[test]
    fn trailing_close_keeps_prepaint_inside_the_dock() {
        let previous = FlatRect { x: 400.0, y: 0.0, w: 400.0, h: 500.0 };
        let current = FlatRect { x: 700.0, w: 100.0, ..previous };
        let final_rect = FlatRect { x: 800.0, w: 0.0, ..previous };
        let (layout, _) = pane_visual_layout(current, final_rect, true, false, true, Some(previous));
        assert_eq!(layout.x, previous.x);
        assert_eq!(layout.w, previous.w);
        let offset = pane_exit_paint_offset(current, layout, (-1.0, 0.0));
        assert_eq!(offset.0, 300.0, "content must leave with the seam, not stay pinned");
    }

    #[test]
    fn closing_content_moves_outward_on_both_axes_with_stable_layout() {
        let retained = FlatRect { x: 100.0, y: 200.0, w: 400.0, h: 300.0 };
        for fraction in [0.0, 0.25, 0.5, 0.75, 1.0] {
            let horizontal_loss = retained.w * fraction;
            let vertical_loss = retained.h * fraction;
            let leading_x = FlatRect { w: retained.w - horizontal_loss, ..retained };
            let trailing_x = FlatRect { x: retained.x + horizontal_loss, ..leading_x };
            let leading_y = FlatRect { h: retained.h - vertical_loss, ..retained };
            let trailing_y = FlatRect { y: retained.y + vertical_loss, ..leading_y };
            assert_eq!(pane_exit_paint_offset(leading_x, retained, (1.0, 0.0)), (-horizontal_loss, 0.0));
            assert_eq!(pane_exit_paint_offset(trailing_x, retained, (-1.0, 0.0)), (horizontal_loss, 0.0));
            assert_eq!(pane_exit_paint_offset(leading_y, retained, (0.0, 1.0)), (0.0, -vertical_loss));
            assert_eq!(pane_exit_paint_offset(trailing_y, retained, (0.0, -1.0)), (0.0, vertical_loss));
        }
    }

    #[test]
    fn expanding_survivor_uses_a_stable_target_extent() {
        let previous = FlatRect { x: 0.0, y: 0.0, w: 200.0, h: 300.0 };
        let final_rect = FlatRect { w: 400.0, h: 600.0, ..previous };
        for fraction in [0.0, 0.2, 0.5, 0.9] {
            let current = FlatRect { w: 200.0 + 200.0 * fraction, h: 300.0 + 300.0 * fraction, ..previous };
            let (layout, _) = pane_visual_layout(current, final_rect, false, false, true, Some(previous));
            assert_eq!((layout.w, layout.h), (400.0, 600.0));
        }
    }

    #[test]
    fn without_retention_uses_the_current_geometry() {
        let current = FlatRect { x: 10.0, y: 20.0, w: 90.0, h: 400.0 };
        let final_rect = FlatRect { w: 220.0, ..current };
        let (layout, offset) = pane_visual_layout(current, final_rect, false, true, false, None);
        assert_eq!((layout.x, layout.y, layout.w, layout.h), (10.0, 20.0, 90.0, 400.0));
        assert_eq!(offset, (0.0, 0.0));
    }
}

pub(crate) struct FastPaneShell {
    id: gpui::ElementId,
    tab_strip: gpui::AnyElement,
    content: gpui::AnyElement,
    style: gpui::StyleRefinement,
}

pub(crate) struct FastPaneShellPrepaint {
    tab_layout: gpui::LayoutId,
    content_layout: gpui::LayoutId,
}

impl FastPaneShell {
    pub(crate) fn new(
        pane_id: TileId,
        tab_strip: gpui::AnyElement,
        content: gpui::AnyElement,
    ) -> Self {
        Self {
            id: gpui::ElementId::Name(gpui::SharedString::from(format!(
                "pane-shell-{}",
                pane_id.0
            ))),
            tab_strip,
            content,
            style: gpui::StyleRefinement::default(),
        }
    }
}

impl gpui::IntoElement for FastPaneShell {
    type Element = Self;
    fn into_element(self) -> Self::Element {
        self
    }
}

impl gpui::Styled for FastPaneShell {
    fn style(&mut self) -> &mut gpui::StyleRefinement {
        &mut self.style
    }
}

impl gpui::Element for FastPaneShell {
    type RequestLayoutState = FastPaneShellPrepaint;
    type PrepaintState = ();

    fn id(&self) -> Option<gpui::ElementId> {
        Some(self.id.clone())
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&gpui::GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        window: &mut gpui::Window,
        cx: &mut gpui::App,
    ) -> (gpui::LayoutId, Self::RequestLayoutState) {
        crate::perf::count("pane.shell.layout");
        let tab_layout = self.tab_strip.request_layout(window, cx);
        let content_layout = self.content.request_layout(window, cx);

        let mut style = gpui::Style::default();
        style.refine(&self.style);
        style.display = gpui::Display::Flex;
        style.flex_direction = gpui::FlexDirection::Column;

        let layout_id = window.request_layout(style, vec![tab_layout, content_layout], cx);

        (
            layout_id,
            FastPaneShellPrepaint {
                tab_layout,
                content_layout,
            },
        )
    }

    fn prepaint(
        &mut self,
        _id: Option<&gpui::GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: gpui::Bounds<gpui::Pixels>,
        request_layout: &mut Self::RequestLayoutState,
        window: &mut gpui::Window,
        cx: &mut gpui::App,
    ) -> Self::PrepaintState {
        crate::perf::count("pane.shell.prepaint");

        let tab_bounds = window.layout_bounds(request_layout.tab_layout);
        let content_bounds = window.layout_bounds(request_layout.content_layout);

        let tab_offset = bounds.origin - tab_bounds.origin;
        let content_offset = bounds.origin
            + gpui::Point {
                x: gpui::px(0.0),
                y: tab_bounds.size.height,
            }
            - content_bounds.origin;

        window.with_content_mask(
            Some(gpui::ContentMask {
                bounds,
                ..Default::default()
            }),
            |window| {
                window.with_element_offset(tab_offset, |window| {
                    self.tab_strip.prepaint(window, cx);
                });
                window.with_element_offset(content_offset, |window| {
                    self.content.prepaint(window, cx);
                });
            },
        );
    }

    fn paint(
        &mut self,
        _id: Option<&gpui::GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: gpui::Bounds<gpui::Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        _prepaint: &mut Self::PrepaintState,
        window: &mut gpui::Window,
        cx: &mut gpui::App,
    ) {
        crate::perf::count("pane.shell.paint");
        let mut style = gpui::Style::default();
        style.refine(&self.style);
        // Each child's Drawable retains the bounds set during prepaint.
        // CE restricts layout queries and element offsets to the layout phases.
        style.paint(bounds, window, cx, |window, cx| {
            self.tab_strip.paint(window, cx);
            self.content.paint(window, cx);
        });
    }
}

fn tab_drag_source<V: gpui::Render + 'static>(
    payload: TabDrag,
    build: impl Fn(&TabDrag, gpui::Point<gpui::Pixels>, &mut Window, &mut gpui::App) -> gpui::Entity<V>
        + 'static,
) -> impl Fn(
    gpui::Point<gpui::Pixels>,
    Option<gpui::CursorStyle>,
    &mut Window,
    &mut gpui::App,
) -> gpui::AnyDrag {
    move |offset, cursor, window, cx| {
        let view = build(&payload, offset, window, cx);
        gpui::AnyDrag::new(payload.clone(), view)
            .offset(offset)
            .cursor_style(cursor)
    }
}
